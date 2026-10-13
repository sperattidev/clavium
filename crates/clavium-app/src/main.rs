//! Clavium: conectá tu teclado MIDI y tocá.
//!
//! Conectás el teclado, elegís una pieza de la biblioteca y tocás mientras
//! las notas caen; la app evalúa cada nota y muestra el resultado.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod calibration;
mod catalog;
mod falling;
mod instruments;
mod keyboard;
mod library;
mod live;
mod mine;
mod session;
mod store;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use clavium_audio::{AudioEngine, Command, SampleBank};
use clavium_midi::{
    KeyEvent, MidiEvent, MidiService, PortKind, ServiceConfig, VelocityCurve, classify_port,
};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    Vec2,
};

use calibration::Calibration;
use clavium_core::midi::PartHand;
use clavium_eval::{HandFilter, PracticeMode, Summary};
use instruments::{Quality, SALAMANDER};
use keyboard::{BLACK_LENGTH, KeyboardLayout, KeyboardSize, note_name};
use library::{Filter, GroupBy, Length, Library, Status};
use live::{KeyState, LatencyStats, LiveKeys};
use session::{Feedback, PracticeOptions, PracticeRange, Session};
use store::{PieceStats, SessionRecord, Store};

const ACCENT_DIM: Color32 = Color32::from_rgb(28, 86, 150);
const ACCENT_BRIGHT: Color32 = Color32::from_rgb(110, 196, 255);
const WHITE_KEY: Color32 = Color32::from_rgb(244, 243, 238);
const BLACK_KEY: Color32 = Color32::from_rgb(28, 28, 32);
const KEY_EDGE: Color32 = Color32::from_rgb(90, 90, 96);
const OK_GREEN: Color32 = Color32::from_rgb(90, 200, 120);
const WARN_AMBER: Color32 = Color32::from_rgb(235, 180, 70);

/// Velocity de las notas tocadas con el teclado de la computadora o el mouse.
const FALLBACK_VELOCITY: u8 = 90;
/// Espera antes de volver a abrir la salida de audio si falló.
const AUDIO_RETRY: Duration = Duration::from_secs(2);
/// Alto reservado al pie de la biblioteca.
const FOOTER_HEIGHT: f32 = 34.0;

fn main() -> eframe::Result {
    // `clavium-app --instalar-catalogo paquete.zip` instala el catálogo sin abrir la ventana.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, path] = args.as_slice()
        && flag == "--instalar-catalogo"
    {
        let result = std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| catalog::install_zip(&bytes, &catalog::default_dir()));
        match result {
            Ok(count) => println!(
                "Se instalaron {count} obras en {}",
                catalog::default_dir().display()
            ),
            Err(e) => {
                eprintln!("No se pudo instalar el catálogo: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // `clavium-app --importar archivo.mid` lo suma a Mis piezas con las partes sugeridas.
    if let [flag, path] = args.as_slice()
        && flag == "--importar"
    {
        let result = mine::PendingImport::open(std::path::Path::new(path)).and_then(|p| {
            p.build()
                .and_then(|piece| p.save(&piece).map(|id| (id, piece)))
        });
        match result {
            Ok((id, piece)) => println!(
                "Importada como {id}: {} ({} notas, nivel {}, {} secciones)",
                piece.metadata.title,
                piece.notes.len(),
                piece.metadata.difficulty.unwrap_or(1),
                piece.sections.len()
            ),
            Err(e) => {
                eprintln!("No se pudo importar: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // `clavium-app --instalar-piano [estandar|completa]` descarga el Yamaha C5 sin abrir la ventana.
    if let Some(flag) = args.first()
        && flag == "--instalar-piano"
    {
        let quality = match args.get(1).map(String::as_str) {
            Some("completa") => Quality::Full,
            _ => Quality::Standard,
        };
        let rx = instruments::prepare_in_background(&SALAMANDER, quality, true, || {});
        let mut last = String::new();
        for event in rx {
            match event {
                instruments::Event::Progress(p) => {
                    let line = format!("{p:?}");
                    if line != last {
                        println!("{line}");
                        last = line;
                    }
                }
                instruments::Event::Ready(bank) => {
                    println!(
                        "Listo: {} ({:.0} MB en memoria)",
                        bank.name,
                        bank.memory_bytes() as f64 / 1e6
                    );
                    return Ok(());
                }
                instruments::Event::Failed(e) => {
                    eprintln!("No se pudo instalar el piano: {e}");
                    std::process::exit(1);
                }
            }
        }
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Clavium")
            .with_inner_size([1200.0, 640.0])
            .with_min_inner_size([640.0, 360.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "Clavium",
        options,
        Box::new(|cc| Ok(Box::new(ClaviumApp::new(cc)))),
    )
}

#[derive(Debug, Clone, PartialEq)]
enum Connection {
    Searching { lost: Option<String> },
    Connected(String),
    Failed { port: String, reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PlayOptions {
    hands: HandFilter,
    mode: PracticeMode,
    speed: f64,
    /// Índice en `Piece::sections`, o `None` para la pieza completa.
    section: Option<usize>,
    looping: bool,
    speed_up: bool,
}

impl Default for PlayOptions {
    fn default() -> Self {
        PlayOptions {
            hands: HandFilter::Both,
            mode: PracticeMode::Wait,
            speed: 1.0,
            section: None,
            looping: false,
            speed_up: true,
        }
    }
}

/// Preferencias que se guardan entre sesiones de la app.
#[derive(Debug, Clone, PartialEq)]
struct Settings {
    size: KeyboardSize,
    curve: VelocityCurve,
    volume: f32,
    sound_on: bool,
    input_offset_ms: f64,
    /// Piano de samples elegido, o `None` para el sintetizado.
    sampled: bool,
    quality: Quality,
    /// Si el usuario descartó la recomendación de bajar el Yamaha C5.
    prompt_dismissed: bool,
}

enum Screen {
    Library,
    Playing(Box<Session>),
    Results {
        session: Box<Session>,
        summary: Summary,
    },
    Calibration(Box<Calibration>),
}

struct ClaviumApp {
    /// Piano de samples elegido (si es `false`, suena el sintetizado).
    sampled: bool,
    quality: Quality,
    prompt_dismissed: bool,
    /// Descarga o carga del piano de samples en curso.
    piano_job: Option<Receiver<instruments::Event>>,
    piano_progress: Option<instruments::Progress>,
    /// Piano de samples cargado, para volver a dárselo al audio si se reabre.
    piano_bank: Option<Arc<SampleBank>>,
    piano_error: Option<String>,
    store: Option<Store>,
    /// Estadísticas por pieza, leídas de la base.
    stats: HashMap<String, PieceStats>,
    saved: Settings,
    /// Desvío medido en la calibración, en milisegundos.
    input_offset_ms: f64,
    library: Library,
    filter: Filter,
    browse_by: GroupBy,
    /// Nombre con el que se firman las revisiones.
    reviewer_name: String,
    /// Descarga del catálogo en curso.
    download: Option<Receiver<Result<usize, String>>>,
    /// Aviso para mostrar en la biblioteca (descargas, errores al abrir).
    library_message: Option<String>,
    /// MIDI abierto esperando que se elijan sus partes.
    pending_import: Option<mine::PendingImport>,
    /// Pieza propia a punto de quitarse (pide confirmación).
    confirm_remove: Option<String>,
    screen: Screen,
    selected: usize,
    options: PlayOptions,
    midi: MidiService,
    connection: Connection,
    ports: Vec<String>,
    keys: LiveKeys,
    size: KeyboardSize,
    layout: KeyboardLayout,
    /// Base del teclado de la computadora (la tecla A).
    computer_base: u8,
    mouse_note: Option<u8>,
    /// Instantes de llegada de notas que todavía no se dibujaron.
    pending: Vec<Instant>,
    latency: LatencyStats,
    out_of_range: Option<u8>,
    show_diagnostics: bool,
    log: VecDeque<String>,
    started: Instant,
    audio: Option<AudioEngine>,
    audio_error: Option<String>,
    audio_retry_at: Option<Instant>,
    /// Si el piano interno suena. Se apaga si el teclado tiene sonido propio.
    sound_on: bool,
    volume: f32,
    curve: VelocityCurve,
    sound_latency: LatencyStats,
}

impl ClaviumApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        // Sin base de datos la app funciona igual, pero no recuerda nada.
        let store = Store::open_default().ok();
        let setting = |key: &str| store.as_ref().and_then(|s| s.setting::<String>(key));
        let settings = Settings {
            size: setting("keyboard_size")
                .and_then(|v| KeyboardSize::from_key(&v))
                .unwrap_or(KeyboardSize::Keys88),
            curve: setting("velocity_curve")
                .and_then(|v| curve_from_key(&v))
                .unwrap_or_default(),
            volume: setting("volume")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.8),
            sound_on: setting("sound_on").is_none_or(|v| v == "true"),
            input_offset_ms: setting("input_offset_ms")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            sampled: setting("instrument").is_some_and(|v| v == SALAMANDER.id),
            quality: setting("instrument_quality")
                .and_then(|v| Quality::from_key(&v))
                .unwrap_or(Quality::Standard),
            prompt_dismissed: setting("salamander_prompt").is_some_and(|v| v == "dismissed"),
        };
        let reviewer_name = setting("reviewer_name").unwrap_or_default();
        let mut library = Library::load(&catalog::default_dir(), &mine::dir());
        apply_local_reviews(&mut library, store.as_ref());
        let stats = load_stats(&library, store.as_ref());

        let ctx = cc.egui_ctx.clone();
        let midi_config = ServiceConfig {
            preferred_port: setting("midi_port"),
            ..Default::default()
        };
        let midi = MidiService::start(midi_config, move || ctx.request_repaint());
        let size = settings.size;
        let (audio, audio_error) = match AudioEngine::start() {
            Ok(engine) => {
                engine.set_volume(settings.volume);
                (Some(engine), None)
            }
            Err(e) => (None, Some(e.to_string())),
        };
        let mut app = ClaviumApp {
            store,
            stats,
            input_offset_ms: settings.input_offset_ms,
            // Solo se usa el piano de samples si ya está descargado.
            sampled: settings.sampled && instruments::installed(&SALAMANDER).is_some(),
            quality: settings.quality,
            prompt_dismissed: settings.prompt_dismissed,
            piano_job: None,
            piano_progress: None,
            piano_bank: None,
            piano_error: None,
            sound_on: settings.sound_on,
            volume: settings.volume,
            curve: settings.curve,
            saved: settings,
            library,
            filter: Filter {
                min_level: 1,
                max_level: 8,
                ..Default::default()
            },
            browse_by: GroupBy::Level,
            reviewer_name,
            download: None,
            library_message: None,
            pending_import: None,
            confirm_remove: None,
            screen: Screen::Library,
            selected: 0,
            options: PlayOptions::default(),
            audio_retry_at: audio_error.as_ref().map(|_| Instant::now() + AUDIO_RETRY),
            audio,
            audio_error,
            sound_latency: LatencyStats::default(),
            midi,
            connection: Connection::Searching { lost: None },
            ports: Vec::new(),
            keys: LiveKeys::default(),
            size,
            layout: KeyboardLayout::new(size),
            computer_base: 60,
            mouse_note: None,
            pending: Vec::new(),
            latency: LatencyStats::default(),
            out_of_range: None,
            show_diagnostics: false,
            log: VecDeque::new(),
            started: Instant::now(),
        };
        if app.sampled {
            app.start_piano(&cc.egui_ctx, false);
        }
        app
    }

    /// Descarga (si `download`) y carga el Yamaha C5 en segundo plano.
    fn start_piano(&mut self, ctx: &egui::Context, download: bool) {
        if self.piano_job.is_some() {
            return;
        }
        self.piano_error = None;
        self.piano_progress = None;
        let ctx = ctx.clone();
        self.piano_job = Some(instruments::prepare_in_background(
            &SALAMANDER,
            self.quality,
            download,
            move || ctx.request_repaint(),
        ));
    }

    /// Recibe los avances de la descarga o la carga del piano de samples.
    fn update_piano(&mut self) {
        let Some(rx) = &self.piano_job else {
            return;
        };
        let mut finished = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                instruments::Event::Progress(p) => self.piano_progress = Some(p),
                instruments::Event::Ready(bank) => {
                    if let Some(audio) = &mut self.audio {
                        audio.set_instrument(Some(bank.clone()));
                    }
                    self.piano_bank = Some(bank);
                    self.sampled = true;
                    finished = true;
                }
                instruments::Event::Failed(e) => {
                    self.piano_error = Some(e);
                    self.sampled = false;
                    finished = true;
                }
            }
        }
        if finished {
            self.piano_job = None;
            self.piano_progress = None;
        }
    }

    /// Vuelve al piano sintetizado y libera la memoria del de samples.
    fn use_synth(&mut self) {
        self.sampled = false;
        self.piano_bank = None;
        if let Some(audio) = &mut self.audio {
            audio.set_instrument(None);
        }
    }

    fn press(&mut self, pitch: u8, velocity: u8, at: Instant) {
        self.keys.note_on(pitch, velocity);
        self.send_audio(Command::NoteOn { pitch, velocity });
        if let Screen::Calibration(calibration) = &mut self.screen {
            calibration.on_tap(at);
            return;
        }
        if let Screen::Playing(session) = &mut self.screen {
            session.on_user_note(pitch, at);
            // Durante una pieza el teclado de pantalla queda fijo en su rango.
            self.out_of_range = (!self.layout.contains(pitch)).then_some(pitch);
            return;
        }
        // Un teclado chico con los botones de octava corridos: se corre la
        // pantalla igual que el teclado.
        self.out_of_range = match self.layout.shift_to_include(pitch) {
            Some(shift) if shift == self.layout.octave_shift => None,
            Some(shift) => {
                if let Some(layout) = KeyboardLayout::with_shift(self.size, shift) {
                    self.layout = layout;
                }
                None
            }
            None => Some(pitch),
        };
    }

    fn release(&mut self, pitch: u8) {
        self.keys.note_off(pitch);
        self.send_audio(Command::NoteOff { pitch });
        if let Screen::Playing(session) = &mut self.screen {
            session.on_user_release(pitch);
        }
    }

    /// Notas que toca la app (la otra mano o el modo escucha). Suenan aunque
    /// el sonido del usuario esté apagado, porque su teclado no las toca.
    fn send_auto(&mut self, commands: Vec<Command>) {
        if let Some(audio) = &mut self.audio {
            for command in commands {
                audio.send(command);
            }
        }
    }

    fn start_session(&mut self) {
        let Some(entry) = self.library.entries.get_mut(self.selected) else {
            return;
        };
        let piece = match entry.piece() {
            Ok(piece) => piece,
            Err(e) => {
                self.library_message = Some(format!("No se pudo abrir la pieza: {e}"));
                return;
            }
        };
        let range = self
            .options
            .section
            .and_then(|i| piece.sections.get(i))
            .map(|s| {
                let bars = piece.bar_ticks();
                let first = bars.partition_point(|&b| b <= s.start_tick);
                let last = bars.partition_point(|&b| b < s.end_tick);
                PracticeRange {
                    label: format!("{} (compases {first}–{last})", s.name),
                    from: piece.tempo.tick_to_seconds(s.start_tick),
                    to: piece.tempo.tick_to_seconds(s.end_tick),
                }
            });
        let options = PracticeOptions {
            hands: self.options.hands,
            mode: self.options.mode,
            speed: self.options.speed,
            looping: self.options.looping && range.is_some(),
            speed_up: self.options.speed_up,
            range,
            input_offset: self.input_offset_ms / 1000.0,
        };
        let piece = piece.clone();
        self.begin(piece, options);
    }

    /// Repasar en bucle los compases con más fallas de la última vuelta.
    fn practice_weak_bars(&mut self, session: &Session, bar: usize) {
        let starts = session.eval.bar_starts();
        let first = bar.saturating_sub(1).max(1);
        let last = (bar + 1).min(starts.len());
        let from = starts.get(first - 1).copied().unwrap_or(0.0);
        let to = starts
            .get(last)
            .copied()
            .unwrap_or_else(|| session.piece.duration_seconds());
        let options = PracticeOptions {
            range: Some(PracticeRange {
                label: format!("Compases {first}–{last}"),
                from,
                to,
            }),
            looping: true,
            speed_up: true,
            speed: (session.options.speed - 0.25).max(0.5),
            ..session.options.clone()
        };
        self.begin(session.piece.clone(), options);
    }

    fn begin(&mut self, piece: clavium_core::Piece, options: PracticeOptions) {
        // Que entre toda la pieza en el teclado de pantalla.
        let lowest = piece.notes.iter().map(|n| n.pitch).min().unwrap_or(60);
        let highest = piece.notes.iter().map(|n| n.pitch).max().unwrap_or(60);
        if let Some(shift) = self.layout.shift_to_fit(lowest, highest)
            && let Some(layout) = KeyboardLayout::with_shift(self.size, shift)
        {
            self.layout = layout;
        }
        self.screen = Screen::Playing(Box::new(Session::new(piece, options)));
    }

    /// Vuelve a leer la biblioteca (por ejemplo, después de instalar el
    /// catálogo) manteniendo la pieza elegida.
    fn reload_library(&mut self) {
        let selected_id = self
            .library
            .entries
            .get(self.selected)
            .map(|e| e.id.clone());
        self.library = Library::load(&catalog::default_dir(), &mine::dir());
        apply_local_reviews(&mut self.library, self.store.as_ref());
        self.stats = load_stats(&self.library, self.store.as_ref());
        self.selected = selected_id
            .and_then(|id| self.library.entries.iter().position(|e| e.id == id))
            .unwrap_or(0);
    }

    /// Termina una descarga del catálogo o instala un paquete arrastrado.
    fn update_catalog(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.download
            && let Ok(result) = rx.try_recv()
        {
            self.download = None;
            self.finish_install(result);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        let extension = |p: &PathBuf| {
            p.extension()
                .map(|x| x.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default()
        };
        if let Some(path) = dropped.first() {
            match extension(path).as_str() {
                "zip" => {
                    let result = std::fs::read(path)
                        .map_err(|e| e.to_string())
                        .and_then(|bytes| catalog::install_zip(&bytes, &catalog::default_dir()));
                    self.finish_install(result);
                }
                "mid" | "midi" => self.open_midi(path),
                _ => {
                    self.library_message = Some(
                        "Se pueden arrastrar archivos MIDI (.mid) o paquetes de catálogo (.zip)."
                            .into(),
                    );
                }
            }
        }
    }

    /// Abre un MIDI para elegir qué partes importar.
    fn open_midi(&mut self, path: &std::path::Path) {
        match mine::PendingImport::open(path) {
            Ok(pending) => {
                self.pending_import = Some(pending);
                self.screen_to_library();
            }
            Err(e) => self.library_message = Some(format!("No se pudo abrir el MIDI: {e}")),
        }
    }

    fn pick_midi(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Importar un archivo MIDI")
            .add_filter("Archivos MIDI", &["mid", "midi"])
            .pick_file()
        {
            self.open_midi(&path);
        }
    }

    /// Si se está tocando, vuelve a la biblioteca para mostrar el diálogo.
    fn screen_to_library(&mut self) {
        if !matches!(self.screen, Screen::Library) {
            self.leave_session();
            self.screen = Screen::Library;
        }
    }

    /// Diálogo para elegir las partes del MIDI y con qué mano se tocan.
    fn import_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = &mut self.pending_import else {
            return;
        };
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Importar MIDI")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Título:");
                    ui.add(egui::TextEdit::singleline(&mut pending.title).desired_width(320.0));
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Elegí qué partes querés tocar. En los MIDI de canciones suele haber batería, \
                         bajo y voz además del piano.",
                    )
                    .color(ui.visuals().weak_text_color()),
                );
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    egui::Grid::new("parts")
                        .num_columns(4)
                        .spacing([12.0, 6.0])
                        .striped(true)
                        .show(ui, |ui| {
                            ui.label(RichText::new("Tocar").strong());
                            ui.label(RichText::new("Parte").strong());
                            ui.label(RichText::new("Notas").strong());
                            ui.label(RichText::new("Mano").strong());
                            ui.end_row();
                            for (i, part) in pending.parts.iter().enumerate() {
                                let (on, hand) = &mut pending.choices[i];
                                ui.checkbox(on, "");
                                let name = if part.name.is_empty() {
                                    format!("Pista {}", part.track + 1)
                                } else {
                                    part.name.clone()
                                };
                                ui.label(format!(
                                    "{name} · {} ({}–{})",
                                    part.instrument(),
                                    note_name(part.lowest),
                                    note_name(part.highest)
                                ));
                                ui.label(part.notes.to_string());
                                ui.add_enabled_ui(*on, |ui| {
                                    egui::ComboBox::from_id_salt(("hand", i))
                                        .selected_text(hand_label(*hand))
                                        .show_ui(ui, |ui| {
                                            for h in [PartHand::Auto, PartHand::Right, PartHand::Left, PartHand::Split] {
                                                ui.selectable_value(hand, h, hand_label(h));
                                            }
                                        });
                                });
                                ui.end_row();
                            }
                        });
                });
                ui.add_space(8.0);
                ui.label(format!("{} notas elegidas.", pending.selected_notes()));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Importar").clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancelar").clicked() {
                        cancel = true;
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "La pieza se guarda solo en esta computadora. Usá archivos que tengas \
                         derecho a usar.",
                    )
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
            });

        if confirm {
            let result = pending.build().and_then(|piece| pending.save(&piece));
            match result {
                Ok(id) => {
                    self.pending_import = None;
                    self.reload_library();
                    if let Some(i) = self.library.entries.iter().position(|e| e.id == id) {
                        self.selected = i;
                    }
                    self.options.section = None;
                    self.library_message = Some("Pieza importada en Mis piezas.".into());
                }
                Err(e) => self.library_message = Some(format!("No se pudo importar: {e}")),
            }
        } else if cancel || !open {
            self.pending_import = None;
        }
    }

    fn finish_install(&mut self, result: Result<usize, String>) {
        match result {
            Ok(count) => {
                self.reload_library();
                self.library_message = Some(format!("Se instalaron {count} obras completas."));
            }
            Err(e) => {
                self.library_message = Some(format!("No se pudo instalar el catálogo: {e}"));
            }
        }
    }

    fn update_calibration(&mut self) {
        let Screen::Calibration(calibration) = &mut self.screen else {
            return;
        };
        let commands = calibration.update(Instant::now());
        self.send_auto(commands);
    }

    fn record(&mut self, session: &Session, summary: &Summary) {
        let Some(id) = self
            .library
            .entries
            .get(self.selected)
            .map(|e| e.id.clone())
        else {
            return;
        };
        if session.mode == PracticeMode::Listen {
            return;
        }
        let record = SessionRecord {
            piece: &id,
            mode: session.mode,
            hands: session.hands,
            speed: session.speed,
            section: session.options.range.as_ref().map(|r| r.label.as_str()),
            summary,
        };
        match &self.store {
            Some(store) => {
                let _ = store.record_session(&record);
                self.stats.insert(id.clone(), store.piece_stats(&id));
            }
            None => {
                // Sin base, se lleva la cuenta solo mientras la app está abierta.
                let stats = self.stats.entry(id.clone()).or_default();
                stats.sessions += 1;
                let counts = session.mode == PracticeMode::Rhythm
                    && session.hands == HandFilter::Both
                    && session.options.range.is_none();
                if let (true, Some(accuracy)) = (counts, summary.accuracy) {
                    stats.best_accuracy = Some(stats.best_accuracy.unwrap_or(0.0).max(accuracy));
                    if accuracy >= store::MASTERED_ACCURACY {
                        stats.best_speed = Some(stats.best_speed.unwrap_or(0.0).max(session.speed));
                    }
                }
            }
        }
    }

    fn save_settings(&mut self) {
        let current = Settings {
            size: self.size,
            curve: self.curve,
            volume: self.volume,
            sound_on: self.sound_on,
            input_offset_ms: self.input_offset_ms,
            sampled: self.sampled,
            quality: self.quality,
            prompt_dismissed: self.prompt_dismissed,
        };
        if current == self.saved {
            return;
        }
        if let Some(store) = &self.store {
            store.set_setting("keyboard_size", current.size.key());
            store.set_setting("velocity_curve", curve_key(current.curve));
            store.set_setting("volume", current.volume);
            store.set_setting("sound_on", current.sound_on);
            store.set_setting("input_offset_ms", current.input_offset_ms);
            store.set_setting(
                "instrument",
                if current.sampled {
                    SALAMANDER.id
                } else {
                    "synth"
                },
            );
            store.set_setting("instrument_quality", current.quality.key());
            if current.prompt_dismissed {
                store.set_setting("salamander_prompt", "dismissed");
            }
        }
        self.saved = current;
    }

    fn leave_session(&mut self) {
        let screen = std::mem::replace(&mut self.screen, Screen::Library);
        if let Screen::Playing(mut session) | Screen::Results { mut session, .. } = screen {
            let commands = session.stop_auto();
            self.send_auto(commands);
        }
    }

    /// Termina un bucle y muestra el resultado de la última vuelta.
    fn finish_loop(&mut self) {
        let Screen::Playing(session) = &mut self.screen else {
            return;
        };
        let commands = session.finish();
        self.send_auto(commands);
        self.update_session();
    }

    fn update_session(&mut self) {
        let Screen::Playing(session) = &mut self.screen else {
            return;
        };
        let commands = session.update(Instant::now());
        let finished = session.is_finished();
        self.send_auto(commands);
        if finished {
            let Screen::Playing(session) = std::mem::replace(&mut self.screen, Screen::Library)
            else {
                unreachable!()
            };
            let summary = session.summary();
            self.record(&session, &summary);
            self.screen = Screen::Results { session, summary };
        }
    }

    fn set_sustain(&mut self, down: bool) {
        self.keys.set_sustain(down);
        self.send_audio(Command::Sustain(down));
    }

    fn send_audio(&mut self, command: Command) {
        let silenceable = matches!(command, Command::NoteOn { .. });
        if let Some(audio) = &mut self.audio
            && (self.sound_on || !silenceable)
        {
            audio.send(command);
        }
    }

    fn update_audio(&mut self) {
        if let Some(audio) = &mut self.audio {
            audio.release_retired();
            for ms in audio.drain_latencies() {
                self.sound_latency.record(ms);
            }
            if audio.has_failed() {
                self.audio = None;
                self.audio_error = Some("Se perdió la salida de audio".to_string());
                self.audio_retry_at = Some(Instant::now() + AUDIO_RETRY);
            }
        } else if self.audio_retry_at.is_some_and(|t| Instant::now() >= t) {
            match AudioEngine::start() {
                Ok(mut engine) => {
                    engine.set_volume(self.volume);
                    if let Some(bank) = &self.piano_bank {
                        engine.set_instrument(Some(bank.clone()));
                    }
                    self.audio = Some(engine);
                    self.audio_error = None;
                    self.audio_retry_at = None;
                }
                Err(e) => {
                    self.audio_error = Some(e.to_string());
                    self.audio_retry_at = Some(Instant::now() + AUDIO_RETRY);
                }
            }
        }
    }

    fn push_log(&mut self, line: String) {
        let t = self.started.elapsed().as_secs_f32();
        self.log.push_front(format!("{t:8.3} s  {line}"));
        self.log.truncate(14);
    }

    fn handle_midi(&mut self) {
        let events: Vec<MidiEvent> = self.midi.drain().collect();
        for event in events {
            match event {
                MidiEvent::PortsChanged(ports) => self.ports = ports,
                MidiEvent::Connected { port } => {
                    self.push_log(format!("conectado: {port}"));
                    if let Some(store) = &self.store {
                        store.set_setting("midi_port", &port);
                    }
                    self.connection = Connection::Connected(port);
                }
                MidiEvent::Disconnected { port } => {
                    self.push_log(format!("desconectado: {port}"));
                    self.keys.release_all();
                    self.send_audio(Command::AllNotesOff);
                    self.connection = Connection::Searching { lost: Some(port) };
                }
                MidiEvent::ConnectFailed { port, reason } => {
                    self.push_log(format!("no se pudo abrir {port}: {reason}"));
                    self.connection = Connection::Failed { port, reason };
                }
                MidiEvent::Key { event, at } => match event {
                    KeyEvent::NoteOn { pitch, velocity } => {
                        let curved = self.curve.apply(velocity);
                        self.press(pitch, curved, at);
                        self.pending.push(at);
                        self.push_log(format!("{} vel {velocity} → {curved}", note_name(pitch)));
                    }
                    KeyEvent::NoteOff { pitch } => self.release(pitch),
                    KeyEvent::Sustain { down } => {
                        self.set_sustain(down);
                        self.push_log(format!("pedal {}", if down { "abajo" } else { "arriba" }));
                    }
                },
            }
        }
    }

    fn handle_computer_keyboard(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|i| i.events.clone());
        for event in events {
            let egui::Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                ..
            } = event
            else {
                continue;
            };
            if repeat {
                continue;
            }
            let key = physical_key.unwrap_or(key);
            if pressed {
                match (&mut self.screen, key) {
                    (Screen::Playing(session), egui::Key::Space) => {
                        let commands = session.toggle_pause();
                        self.send_auto(commands);
                        continue;
                    }
                    (Screen::Playing(_) | Screen::Results { .. }, egui::Key::Escape) => {
                        self.leave_session();
                        continue;
                    }
                    (Screen::Calibration(_), egui::Key::Escape) => {
                        self.screen = Screen::Library;
                        continue;
                    }
                    _ => {}
                }
                match key {
                    egui::Key::Z => {
                        self.computer_base = self.computer_base.saturating_sub(12).max(24)
                    }
                    egui::Key::X => self.computer_base = (self.computer_base + 12).min(96),
                    _ => {}
                }
            }
            if let Some(offset) = computer_key_offset(key) {
                let pitch = self.computer_base + offset;
                if pressed {
                    self.press(pitch, FALLBACK_VELOCITY, Instant::now());
                } else {
                    self.release(pitch);
                }
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match &self.connection {
                Connection::Connected(port) => {
                    status_dot(ui, OK_GREEN);
                    ui.label(RichText::new(format!("{port} conectado")).strong());
                }
                Connection::Searching { lost } => {
                    status_dot(ui, WARN_AMBER);
                    let text = match lost {
                        Some(port) => format!("Se desconectó {port}. Buscando teclado…"),
                        None => "Buscando teclado… conectalo por USB".to_string(),
                    };
                    ui.label(text);
                }
                Connection::Failed { port, .. } => {
                    status_dot(ui, WARN_AMBER);
                    ui.label(format!(
                        "No se pudo abrir {port}. Puede estar en uso por otra app: cerrala y Clavium lo vuelve a intentar solo."
                    ));
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.toggle_value(&mut self.show_diagnostics, "Diagnóstico");
                let calibrate = ui
                    .add_enabled(
                        matches!(self.screen, Screen::Library),
                        egui::Button::new("Calibrar"),
                    )
                    .on_hover_text(format!(
                        "Mide tu desvío al tocar para evaluarte con justicia. Ahora: {:+.0} ms",
                        self.input_offset_ms
                    ));
                if calibrate.clicked() {
                    self.screen = Screen::Calibration(Box::default());
                }
                ui.separator();

                let before = self.size;
                egui::ComboBox::from_id_salt("keyboard-size")
                    .selected_text(self.size.label())
                    .show_ui(ui, |ui| {
                        for size in KeyboardSize::ALL {
                            ui.selectable_value(&mut self.size, size, size.label());
                        }
                    });
                if self.size != before {
                    self.layout = KeyboardLayout::new(self.size);
                    self.out_of_range = None;
                }
                ui.label("Tu teclado:");
                ui.separator();

                egui::ComboBox::from_id_salt("velocity-curve")
                    .selected_text(self.curve.label())
                    .show_ui(ui, |ui| {
                        for curve in VelocityCurve::ALL {
                            ui.selectable_value(&mut self.curve, curve, curve.label());
                        }
                    })
                    .response
                    .on_hover_text(
                        "Liviana: con poca fuerza ya suena fuerte. Probala si tu teclado suena muy bajo.",
                    );
                ui.label("Sensibilidad:");
                ui.separator();

                if self.audio.is_some() {
                    let slider = ui.add_enabled(
                        self.sound_on,
                        egui::Slider::new(&mut self.volume, 0.0..=1.0).show_value(false),
                    );
                    if slider.changed()
                        && let Some(audio) = &self.audio
                    {
                        audio.set_volume(self.volume);
                    }
                    let toggle = ui
                        .checkbox(&mut self.sound_on, "Sonido")
                        .on_hover_text("Apagalo si tu teclado tiene sonido propio.");
                    if toggle.changed() && !self.sound_on {
                        self.send_audio(Command::AllNotesOff);
                    }
                    ui.separator();
                    self.piano_selector(ui);
                } else {
                    ui.label(RichText::new("Sin salida de audio").color(WARN_AMBER))
                        .on_hover_text(self.audio_error.clone().unwrap_or_default());
                }
            });
        });
    }

    /// Qué piano suena: el sintetizado o el Yamaha C5 de samples.
    fn piano_selector(&mut self, ui: &mut egui::Ui) {
        if let Some(progress) = &self.piano_progress {
            let text = match progress {
                instruments::Progress::Downloading { done, total } => {
                    format!("Descargando Yamaha C5: {done} de {total}")
                }
                instruments::Progress::Loading { done, total } => {
                    format!("Cargando Yamaha C5: {} %", done * 100 / (*total).max(1))
                }
            };
            ui.label(text);
            ui.spinner();
            return;
        }
        let installed = instruments::installed(&SALAMANDER).is_some();
        let current = if self.sampled {
            "Yamaha C5"
        } else {
            "Sintetizado"
        };
        let mut choice = self.sampled;
        egui::ComboBox::from_id_salt("piano")
            .selected_text(current)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut choice, false, "Sintetizado");
                let label = if installed {
                    "Yamaha C5 (Salamander)"
                } else {
                    "Yamaha C5 (Salamander): descargar"
                };
                ui.selectable_value(&mut choice, true, label)
                    .on_hover_text(SALAMANDER.credit);
            })
            .response
            .on_hover_text(if self.sampled {
                SALAMANDER.credit
            } else {
                "Piano generado por Clavium, sin descargas."
            });
        ui.label("Piano:");
        if choice != self.sampled {
            if choice {
                let ctx = ui.ctx().clone();
                self.start_piano(&ctx, !installed);
            } else {
                self.use_synth();
            }
        }
    }

    /// Recomendación de bajar el Yamaha C5, hasta que se baje o se descarte.
    fn piano_banner(&mut self, ui: &mut egui::Ui) {
        if self.prompt_dismissed
            || self.sampled
            || self.piano_job.is_some()
            || instruments::installed(&SALAMANDER).is_some()
        {
            if let Some(error) = &self.piano_error {
                ui.label(
                    RichText::new(format!("No se pudo preparar el Yamaha C5: {error}"))
                        .color(WARN_AMBER),
                );
            }
            return;
        }
        egui::Frame::new()
            .inner_margin(12)
            .corner_radius(8)
            .fill(ACCENT_DIM.gamma_multiply(0.35))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Recomendado: sonido de piano real").strong());
                    ui.label(format!(
                        "{} Reemplaza al piano sintetizado y se descarga una sola vez (unos 400 MB).",
                        SALAMANDER.description
                    ));
                });
                ui.label(
                    RichText::new(SALAMANDER.credit)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
                if let Some(error) = &self.piano_error {
                    ui.label(RichText::new(error).color(WARN_AMBER));
                }
                ui.horizontal(|ui| {
                    if ui.button("Descargar el Yamaha C5").clicked() {
                        let ctx = ui.ctx().clone();
                        self.start_piano(&ctx, true);
                    }
                    if ui.button("Ahora no").clicked() {
                        self.prompt_dismissed = true;
                    }
                });
            });
        ui.add_space(6.0);
    }

    fn library_screen(&mut self, ui: &mut egui::Ui) {
        // La pieza elegida se lee al elegirla, para mostrar sus secciones.
        if let Some(entry) = self.library.entries.get_mut(self.selected) {
            let _ = entry.piece();
        }

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Biblioteca").size(26.0).strong());
            ui.add_space(16.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.filter.query)
                    .hint_text("Buscar por título o compositor")
                    .desired_width(260.0),
            );
            if !self.filter.query.is_empty() && ui.button("Borrar").clicked() {
                self.filter.query.clear();
            }
            ui.add_space(16.0);
            if ui
                .button("Importar MIDI…")
                .on_hover_text(
                    "Abrí un archivo MIDI para tocarlo. También podés arrastrarlo a la ventana.",
                )
                .clicked()
            {
                self.pick_midi();
            }
        });
        ui.add_space(6.0);
        self.piano_banner(ui);
        self.filter_bar(ui);
        ui.add_space(8.0);

        let cards: Vec<(String, String, Option<String>, bool)> = self
            .library
            .entries
            .iter()
            .map(|e| {
                let progress = self.stats.get(&e.id).and_then(progress_text);
                (e.title.clone(), e.subtitle(), progress, e.needs_review())
            })
            .collect();
        // Una pieza está dominada si se tocó al tempo original con 90 % o más.
        let mastered = |id: &str| {
            self.stats
                .get(id)
                .and_then(|s| s.best_speed)
                .is_some_and(|s| s >= 1.0)
        };
        let browsed = self.library.browse(self.browse_by, &self.filter);
        let total: usize = browsed.iter().map(|g| g.entries.len()).sum();
        let filtering = self.filter != self.default_filter();
        // Con pocos grupos o con un filtro activo se abren todos; si no, solo las rutas.
        let open_all = filtering || browsed.len() <= 4;
        let groups: Vec<(String, Vec<usize>, bool)> = browsed
            .into_iter()
            .map(|g| {
                let header = if g.is_route {
                    let done = g
                        .entries
                        .iter()
                        .filter(|&&i| mastered(&self.library.entries[i].id))
                        .count();
                    format!("{}  ·  {done} de {} dominadas", g.name, g.entries.len())
                } else {
                    let n = g.entries.len();
                    format!(
                        "{}  ·  {n} {}",
                        g.name,
                        if n == 1 { "obra" } else { "obras" }
                    )
                };
                (
                    header,
                    g.entries,
                    g.is_route || open_all || g.name == library::MY_PIECES,
                )
            })
            .collect();
        let mut start = false;
        // Las listas dejan lugar al pie, que se dibuja debajo.
        let list_height = (ui.available_height() - FOOTER_HEIGHT).max(120.0);
        ui.columns(2, |cols| {
            egui::ScrollArea::vertical()
                .id_salt("pieces")
                .max_height(list_height)
                .show(&mut cols[0], |ui| {
                    if filtering {
                        ui.label(
                            RichText::new(format!("{total} piezas con estos filtros"))
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                    for (header, members, open) in &groups {
                        egui::CollapsingHeader::new(
                            RichText::new(header).strong().color(ACCENT_BRIGHT),
                        )
                        .id_salt((header.as_str(), self.browse_by as u8))
                        .default_open(*open)
                        .open(filtering.then_some(true))
                        .show(ui, |ui| {
                            for &i in members {
                                let (title, subtitle, progress, draft) = &cards[i];
                                let fill = if i == self.selected {
                                    ACCENT_DIM.gamma_multiply(0.55)
                                } else {
                                    ui.visuals().faint_bg_color
                                };
                                let response = egui::Frame::new()
                                    .inner_margin(12)
                                    .corner_radius(8)
                                    .fill(fill)
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(RichText::new(title).size(18.0).strong());
                                            if *draft {
                                                ui.label(
                                                    RichText::new("Sin revisar")
                                                        .color(WARN_AMBER),
                                                )
                                                .on_hover_text(
                                                    "Nadie la revisó tocándola todavía: puede tener errores de notas.",
                                                );
                                            }
                                        });
                                        ui.label(
                                            RichText::new(subtitle)
                                                .color(ui.visuals().weak_text_color()),
                                        );
                                        if let Some(progress) = progress {
                                            ui.label(RichText::new(progress).color(OK_GREEN));
                                        }
                                    })
                                    .response
                                    .interact(Sense::click());
                                if response.clicked() && self.selected != i {
                                    self.selected = i;
                                    self.options.section = None;
                                    self.library_message = None;
                                }
                                if response.double_clicked() {
                                    self.selected = i;
                                    start = true;
                                }
                                ui.add_space(6.0);
                            }
                        });
                    }
                    if groups.is_empty() {
                        ui.label("Ninguna pieza cumple estos filtros.");
                    }

                    ui.add_space(12.0);
                    self.catalog_panel(ui);
                });

            egui::ScrollArea::vertical()
                .id_salt("options")
                .max_height(list_height)
                .show(&mut cols[1], |ui| {
                    if self.practice_options(ui) {
                        start = true;
                    }
                });
        });
        self.library_footer(ui);

        if start {
            self.start_session();
        }
    }

    fn default_filter(&self) -> Filter {
        Filter {
            min_level: 1,
            max_level: 8,
            ..Default::default()
        }
    }

    /// Agrupación y filtros de la biblioteca.
    fn filter_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Agrupar:");
            ui.selectable_value(&mut self.browse_by, GroupBy::Level, "Nivel");
            ui.selectable_value(&mut self.browse_by, GroupBy::Composer, "Compositor");
            ui.selectable_value(&mut self.browse_by, GroupBy::Style, "Época");
            ui.separator();

            ui.label("Nivel:");
            let f = &mut self.filter;
            egui::ComboBox::from_id_salt("min-level")
                .width(40.0)
                .selected_text(f.min_level.to_string())
                .show_ui(ui, |ui| {
                    for l in 1..=8u8 {
                        ui.selectable_value(&mut f.min_level, l, l.to_string());
                    }
                });
            ui.label("a");
            egui::ComboBox::from_id_salt("max-level")
                .width(40.0)
                .selected_text(f.max_level.to_string())
                .show_ui(ui, |ui| {
                    for l in 1..=8u8 {
                        ui.selectable_value(&mut f.max_level, l, l.to_string());
                    }
                });
            if f.min_level > f.max_level {
                std::mem::swap(&mut f.min_level, &mut f.max_level);
            }
            ui.separator();

            ui.label("Duración:");
            egui::ComboBox::from_id_salt("length")
                .selected_text(match f.length {
                    Length::Any => "Cualquiera",
                    Length::Short => "Menos de 3 min",
                    Length::Medium => "De 3 a 8 min",
                    Length::Long => "Más de 8 min",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut f.length, Length::Any, "Cualquiera");
                    ui.selectable_value(&mut f.length, Length::Short, "Menos de 3 min");
                    ui.selectable_value(&mut f.length, Length::Medium, "De 3 a 8 min");
                    ui.selectable_value(&mut f.length, Length::Long, "Más de 8 min");
                });
            ui.separator();

            ui.label("Estado:");
            egui::ComboBox::from_id_salt("status")
                .selected_text(match f.status {
                    Status::Any => "Todas",
                    Status::NotReviewed => "Sin revisar",
                    Status::Reviewed => "Revisadas",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut f.status, Status::Any, "Todas");
                    ui.selectable_value(&mut f.status, Status::NotReviewed, "Sin revisar");
                    ui.selectable_value(&mut f.status, Status::Reviewed, "Revisadas");
                });

            let default = self.default_filter();
            if self.filter != default && ui.button("Quitar filtros").clicked() {
                self.filter = default;
            }
        });
    }

    /// Quitar una pieza importada de "Mis piezas", con confirmación.
    fn remove_box(&mut self, ui: &mut egui::Ui) {
        let Some(entry) = self.library.entries.get(self.selected) else {
            return;
        };
        if !entry.mine {
            return;
        }
        let id = entry.id.clone();
        ui.add_space(12.0);
        if self.confirm_remove.as_deref() == Some(id.as_str()) {
            ui.label("¿Quitar esta pieza de Mis piezas? El archivo MIDI original no se borra.");
            ui.horizontal(|ui| {
                if ui.button("Sí, quitarla").clicked() {
                    match mine::remove(&id) {
                        Ok(()) => {
                            self.confirm_remove = None;
                            self.reload_library();
                            self.selected = 0;
                            self.library_message = Some("Pieza quitada de Mis piezas.".into());
                        }
                        Err(e) => self.library_message = Some(format!("No se pudo quitar: {e}")),
                    }
                }
                if ui.button("Cancelar").clicked() {
                    self.confirm_remove = None;
                }
            });
        } else if ui.small_button("Quitar de Mis piezas").clicked() {
            self.confirm_remove = Some(id);
        }
    }

    /// Revisión de la pieza elegida: quien la toca completa y comprueba que
    /// las notas coinciden con la partitura, la marca como revisada.
    fn review_box(&mut self, ui: &mut egui::Ui) {
        let Some(entry) = self.library.entries.get(self.selected) else {
            return;
        };
        if !entry.draft {
            return; // Ya viene revisada de la fuente.
        }
        let id = entry.id.clone();
        let reviewed = entry.reviewed_here;
        ui.add_space(12.0);
        egui::Frame::new()
            .inner_margin(10)
            .corner_radius(8)
            .stroke(Stroke::new(1.0, ui.visuals().weak_text_color()))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if reviewed {
                    ui.label(RichText::new("Revisaste esta pieza.").color(OK_GREEN));
                    if ui.small_button("Quitar la revisión").clicked() {
                        if let Some(store) = &self.store {
                            store.unmark_reviewed(&id);
                        }
                        self.library.entries[self.selected].reviewed_here = false;
                    }
                    return;
                }
                ui.label(RichText::new("Revisión").strong());
                ui.label(
                    RichText::new(
                        "Si la tocaste completa y las notas coinciden con la partitura, \
                         marcala como revisada para que deje de figurar como sin revisar.",
                    )
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
                ui.horizontal(|ui| {
                    ui.label("Tu nombre:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.reviewer_name).desired_width(140.0),
                    );
                });
                let name = self.reviewer_name.trim().to_string();
                let button =
                    ui.add_enabled(!name.is_empty(), egui::Button::new("Marcar como revisada"));
                if button.clicked() {
                    match &self.store {
                        Some(store) => {
                            let _ = store.mark_reviewed(&id, &name);
                            store.set_setting("reviewer_name", &name);
                            self.library.entries[self.selected].reviewed_here = true;
                        }
                        None => {
                            self.library_message = Some(
                                "No se puede guardar la revisión: no hay base de datos.".into(),
                            );
                        }
                    }
                }
            });
    }

    /// Escribe las revisiones de este usuario en el formato de
    /// `catalog/revisiones.txt`, para sumarlas al catálogo.
    fn export_reviews(&self) -> Result<PathBuf, String> {
        let store = self.store.as_ref().ok_or("no hay base de datos")?;
        let mut text = String::from("# id | revisor | fecha\n");
        for (id, reviewer, date) in store.reviews() {
            text.push_str(&format!("{id} | {reviewer} | {date}\n"));
        }
        let path = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Clavium")
            .join("revisiones.txt");
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Pie de la biblioteca: lo que se está tocando y avisos del teclado.
    fn library_footer(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            if self.keys.sustain() {
                ui.label(RichText::new("Pedal presionado").color(ACCENT_BRIGHT));
            }
            let down: Vec<String> = self.keys.down().map(note_name).collect();
            if !down.is_empty() {
                ui.label(RichText::new(down.join("  ")).size(22.0).strong());
            } else if !matches!(self.connection, Connection::Connected(_)) {
                ui.label(
                    RichText::new(
                        "Sin teclado MIDI podés tocar con la computadora: A W S E D F T G Y H U J K \
                         (Z y X cambian de octava) o con el mouse.",
                    )
                    .color(ui.visuals().weak_text_color()),
                );
            }
            if let Some(pitch) = self.out_of_range {
                ui.label(
                    RichText::new(format!(
                        "Tocaste {}, que no entra en un teclado de {}. ¿Tu teclado es más grande?",
                        note_name(pitch),
                        self.size.label()
                    ))
                    .color(WARN_AMBER),
                );
            }
        });
    }

    /// Opciones de práctica de la pieza elegida. Devuelve `true` si se apretó Tocar.
    fn practice_options(&mut self, ui: &mut egui::Ui) -> bool {
        let mut start = false;
        if let Some(entry) = self.library.entries.get(self.selected) {
            ui.label(RichText::new(&entry.title).size(20.0).strong());
            if let Some(credit) = &entry.credit {
                ui.label(
                    RichText::new(credit)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            }
            ui.add_space(8.0);
        }
        ui.label(RichText::new("Mano").strong());
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.options.hands, HandFilter::Both, "Ambas");
            ui.selectable_value(&mut self.options.hands, HandFilter::Right, "Derecha");
            ui.selectable_value(&mut self.options.hands, HandFilter::Left, "Izquierda");
        });
        ui.add_space(10.0);
        ui.label(RichText::new("Modo").strong());
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.options.mode, PracticeMode::Wait, "Espera");
            ui.selectable_value(&mut self.options.mode, PracticeMode::Rhythm, "Ritmo");
            ui.selectable_value(&mut self.options.mode, PracticeMode::Listen, "Escucha");
        });
        ui.label(
            RichText::new(match self.options.mode {
                PracticeMode::Wait => "La música se detiene hasta que tocás las notas correctas.",
                PracticeMode::Rhythm => "La música avanza sola y se evalúa si llegás a tiempo.",
                PracticeMode::Listen => "Clavium toca la pieza y vos mirás.",
            })
            .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(10.0);
        ui.label(RichText::new("Velocidad").strong());
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.options.speed, 0.5, "50 %");
            ui.selectable_value(&mut self.options.speed, 0.75, "75 %");
            ui.selectable_value(&mut self.options.speed, 1.0, "100 %");
        });
        ui.add_space(10.0);

        let sections: Vec<String> = self
            .library
            .entries
            .get(self.selected)
            .and_then(|e| e.loaded())
            .map(|p| p.sections.iter().map(|s| s.name.clone()).collect())
            .unwrap_or_default();
        if !sections.is_empty() {
            ui.label(RichText::new("Qué practicar").strong());
            let current = self
                .options
                .section
                .and_then(|i| sections.get(i).cloned())
                .unwrap_or_else(|| "La pieza completa".to_string());
            egui::ComboBox::from_id_salt("section")
                .selected_text(current)
                .height(320.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.options.section, None, "La pieza completa");
                    for (i, name) in sections.iter().enumerate() {
                        ui.selectable_value(&mut self.options.section, Some(i), name);
                    }
                });
            if self.options.section.is_some() {
                ui.checkbox(&mut self.options.looping, "Repetir en bucle");
                ui.add_enabled(
                    self.options.looping,
                    egui::Checkbox::new(
                        &mut self.options.speed_up,
                        "Subir el tempo 5 % por vuelta si pasás el 90 %",
                    ),
                );
            }
        }
        ui.add_space(16.0);
        let tocar = ui.add_sized(
            [180.0, 44.0],
            egui::Button::new(RichText::new("Tocar").size(20.0)),
        );
        if tocar.clicked() {
            start = true;
        }
        if let Some((low, high)) = self
            .library
            .entries
            .get(self.selected)
            .and_then(|e| e.range())
            && self.layout.shift_to_fit(low, high).is_none()
        {
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "Esta pieza usa notas que no entran en un teclado de {}.",
                    self.size.label()
                ))
                .color(WARN_AMBER),
            );
        }
        self.review_box(ui);
        self.remove_box(ui);
        if let Some(message) = &self.library_message {
            ui.add_space(8.0);
            ui.label(RichText::new(message).color(WARN_AMBER));
        }
        start
    }

    /// Estado de la biblioteca ampliada: descargarla o ver cuántas obras tiene.
    fn catalog_panel(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .inner_margin(12)
            .corner_radius(8)
            .stroke(Stroke::new(1.0, ui.visuals().weak_text_color()))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Biblioteca ampliada").strong());
                if self.download.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Descargando obras completas…");
                    });
                    return;
                }
                let text = if self.library.catalog_size > 0 {
                    format!(
                        "{} obras completas de dominio público o CC BY, del Mutopia Project.",
                        self.library.catalog_size
                    )
                } else {
                    "Cientos de obras completas de dominio público o CC BY, del Mutopia Project. \
                     Se descargan aparte."
                        .to_string()
                };
                ui.label(RichText::new(text).color(ui.visuals().weak_text_color()));
                let label = if self.library.catalog_size > 0 {
                    "Actualizar"
                } else {
                    "Descargar"
                };
                if ui.button(label).clicked() {
                    let ctx = ui.ctx().clone();
                    self.download = Some(catalog::download_in_background(
                        catalog::default_dir(),
                        move || ctx.request_repaint(),
                    ));
                }
                ui.label(
                    RichText::new("También podés arrastrar un paquete .zip a la ventana.")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            });
    }

    fn playing_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::Playing(session) = &self.screen else {
            return;
        };
        let mut leave = false;
        let mut restart = false;
        let mut toggle = false;
        let mut finish = false;

        ui.horizontal(|ui| {
            if ui.button("← Biblioteca").on_hover_text("Esc").clicked() {
                leave = true;
            }
            ui.label(RichText::new(&session.title).size(18.0).strong());
            let mode = match session.mode {
                PracticeMode::Wait => "Modo espera",
                PracticeMode::Rhythm => "Modo ritmo",
                PracticeMode::Listen => "Escucha",
            };
            let mut detail = format!("{mode} · {:.0} %", session.speed * 100.0);
            if let Some(range) = &session.options.range {
                detail.push_str(&format!(" · {}", range.label));
            }
            if session.options.looping {
                detail.push_str(&format!(" · Vuelta {}", session.passes().len() + 1));
                if let Some(accuracy) = session.passes().last().and_then(|p| p.accuracy) {
                    detail.push_str(&format!(", la anterior {:.0} %", accuracy * 100.0));
                }
            }
            ui.label(RichText::new(detail).color(ui.visuals().weak_text_color()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if session.options.looping && ui.button("Terminar").clicked() {
                    finish = true;
                }
                if ui.button("Reiniciar").clicked() {
                    restart = true;
                }
                let label = if session.clock.is_running() {
                    "Pausa"
                } else {
                    "Seguir"
                };
                if ui.button(label).on_hover_text("Espacio").clicked() {
                    toggle = true;
                }
                ui.add(
                    egui::ProgressBar::new(session.progress())
                        .desired_width(ui.available_width().min(320.0)),
                );
            });
        });
        ui.add_space(6.0);

        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        let key_w = rect.width() / self.layout.white_count as f32;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_rgb(22, 22, 26));
        falling::draw(&painter, rect, &self.layout, key_w, session);

        let position = session.clock.position();
        let from = session.options.range.as_ref().map_or(0.0, |r| r.from);
        let overlay = if !session.clock.is_running() {
            Some("En pausa. Espacio para seguir.".to_string())
        } else if position < from {
            Some(format!(
                "Empieza en {:.0}…",
                ((from - position) / session.speed).ceil()
            ))
        } else if session.mode == PracticeMode::Wait
            && session.eval.wait_gate().is_some_and(|g| position >= g)
        {
            Some("Tocá las teclas resaltadas".to_string())
        } else {
            None
        };
        if let Some(text) = overlay {
            painter.text(
                rect.center_top() + Vec2::new(0.0, 40.0),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(22.0),
                Color32::from_gray(200),
            );
        }

        if toggle && let Screen::Playing(session) = &mut self.screen {
            let commands = session.toggle_pause();
            self.send_auto(commands);
        }
        if restart && let Screen::Playing(session) = &mut self.screen {
            let commands = session.stop_auto();
            let fresh = session.restarted();
            **session = fresh;
            self.send_auto(commands);
        }
        if finish {
            self.finish_loop();
        } else if leave {
            self.leave_session();
        }
    }

    fn results_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::Results { session, summary } = &self.screen else {
            return;
        };
        let mut again = false;
        let mut leave = false;
        let mut practice = None;
        let weak = ui.visuals().weak_text_color();

        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.label(RichText::new(&session.title).size(22.0).strong());
            ui.add_space(8.0);
            match summary.accuracy {
                None => {
                    ui.label(RichText::new("Fin de la pieza").size(40.0));
                }
                Some(accuracy) => {
                    ui.label(
                        RichText::new(format!("{:.0} %", accuracy * 100.0))
                            .size(56.0)
                            .strong(),
                    );
                    if session.mode == PracticeMode::Rhythm && session.speed >= 1.0 {
                        stars(ui, summary.stars);
                    } else {
                        let why = if session.mode == PracticeMode::Wait {
                            "Las estrellas se ganan en modo ritmo."
                        } else {
                            "Las estrellas se ganan al tempo original."
                        };
                        ui.label(RichText::new(why).color(weak));
                    }
                    ui.add_space(12.0);
                    egui::Grid::new("result-counts")
                        .num_columns(2)
                        .spacing([24.0, 6.0])
                        .show(ui, |ui| {
                            let mut row = |label: &str, value: usize, color: Color32| {
                                ui.label(label);
                                ui.label(RichText::new(value.to_string()).color(color).strong());
                                ui.end_row();
                            };
                            row("Perfectas", summary.perfect, falling::HIT);
                            row("Bien", summary.good, falling::RIGHT_HAND);
                            row("Fuera de tiempo", summary.off_time, WARN_AMBER);
                            row("Perdidas", summary.missed, falling::MISSED);
                            row("Equivocadas", summary.wrong, falling::MISSED);
                        });
                    ui.add_space(12.0);
                    if let Some(ms) = summary.mean_offset_ms {
                        let text = if ms.abs() < 15.0 {
                            "Vas a tiempo.".to_string()
                        } else if ms < 0.0 {
                            format!("Tendés a adelantarte unos {:.0} ms.", -ms)
                        } else {
                            format!("Tendés a atrasarte unos {:.0} ms.", ms)
                        };
                        ui.label(text);
                    }
                    if !summary.weakest_bars.is_empty() {
                        let bars: Vec<String> =
                            summary.weakest_bars.iter().map(|b| b.to_string()).collect();
                        ui.label(format!("Compases para repasar: {}", bars.join(", ")));
                    }
                    if session.passes().len() > 1 {
                        ui.label(
                            RichText::new(format!(
                                "{} vueltas; el resultado es el de la última, a {:.0} %.",
                                session.passes().len(),
                                session.speed * 100.0
                            ))
                            .color(weak),
                        );
                    }
                }
            }
            ui.add_space(20.0);
            let weakest = summary.weakest_bars.first().copied();
            let buttons = if weakest.is_some() { 450.0 } else { 300.0 };
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - buttons).max(0.0) / 2.0);
                if ui
                    .add_sized([140.0, 40.0], egui::Button::new("Repetir"))
                    .clicked()
                {
                    again = true;
                }
                if let Some(bar) = weakest
                    && ui
                        .add_sized([140.0, 40.0], egui::Button::new(format!("Repasar compás {bar}")))
                        .on_hover_text("En bucle, desde un compás antes hasta uno después, más lento y subiendo el tempo si te sale.")
                        .clicked()
                {
                    practice = Some(bar);
                }
                if ui
                    .add_sized([140.0, 40.0], egui::Button::new("Biblioteca"))
                    .clicked()
                {
                    leave = true;
                }
            });
        });

        if again && let Screen::Results { session, .. } = &self.screen {
            let fresh = session.restarted();
            self.screen = Screen::Playing(Box::new(fresh));
        } else if let Some(bar) = practice
            && let Screen::Results { session, .. } =
                std::mem::replace(&mut self.screen, Screen::Library)
        {
            self.practice_weak_bars(&session, bar);
        } else if leave {
            self.leave_session();
        }
    }

    fn calibration_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::Calibration(calibration) = &self.screen else {
            return;
        };
        let mut close = false;
        let mut save = None;
        let mut again = false;

        ui.horizontal(|ui| {
            if ui.button("← Biblioteca").on_hover_text("Esc").clicked() {
                close = true;
            }
            ui.label(RichText::new("Calibración").size(18.0).strong());
        });
        ui.add_space(6.0);

        if calibration.is_finished() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                match calibration.result() {
                    Some(offset) => {
                        let ms = offset * 1000.0;
                        let text = if ms.abs() < 10.0 {
                            "Tocás prácticamente a tiempo: casi no hace falta corregir.".to_string()
                        } else if ms > 0.0 {
                            format!(
                                "Tocás unos {ms:.0} ms después de que la nota llega a la línea."
                            )
                        } else {
                            format!(
                                "Tocás unos {:.0} ms antes de que la nota llegue a la línea.",
                                -ms
                            )
                        };
                        ui.label(RichText::new(text).size(20.0));
                        ui.label(
                            RichText::new(
                                "Clavium va a descontar ese desvío al evaluar el modo ritmo.",
                            )
                            .color(ui.visuals().weak_text_color()),
                        );
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 300.0).max(0.0) / 2.0);
                            if ui
                                .add_sized([140.0, 40.0], egui::Button::new("Guardar"))
                                .clicked()
                            {
                                save = Some(ms);
                            }
                            if ui
                                .add_sized([140.0, 40.0], egui::Button::new("Repetir"))
                                .clicked()
                            {
                                again = true;
                            }
                        });
                    }
                    None => {
                        ui.label(RichText::new("No llegué a medir suficientes notas.").size(20.0));
                        ui.label(format!(
                            "Hacen falta al menos {} toques cerca de la línea.",
                            calibration::MIN_TAPS
                        ));
                        ui.add_space(16.0);
                        if ui
                            .add_sized([140.0, 40.0], egui::Button::new("Repetir"))
                            .clicked()
                        {
                            again = true;
                        }
                    }
                }
            });
        } else {
            let rect = ui.available_rect_before_wrap();
            ui.allocate_rect(rect, Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, Color32::from_rgb(22, 22, 26));
            let t = calibration.clock.position();
            let window = falling::LOOKAHEAD_SECONDS;
            let lane = Rect::from_center_size(
                Pos2::new(rect.center().x, rect.center().y),
                Vec2::new(160.0, rect.height()),
            );
            for i in 0..calibration::BEATS {
                let beat = Calibration::beat_time(i);
                if beat < t - 0.2 || beat > t + window {
                    continue;
                }
                let y = rect.bottom() - ((beat - t) / window) as f32 * rect.height();
                let bar =
                    Rect::from_center_size(Pos2::new(lane.center().x, y), Vec2::new(160.0, 14.0));
                painter.rect_filled(bar, 6.0, falling::RIGHT_HAND);
            }
            painter.rect_filled(
                Rect::from_min_size(
                    Pos2::new(rect.left(), rect.bottom() - 2.0),
                    Vec2::new(rect.width(), 3.0),
                ),
                0.0,
                ACCENT_BRIGHT,
            );
            painter.text(
                rect.center_top() + Vec2::new(0.0, 40.0),
                Align2::CENTER_CENTER,
                "Tocá cualquier tecla justo cuando cada barra llega a la línea",
                FontId::proportional(20.0),
                Color32::from_gray(210),
            );
            painter.text(
                rect.center_top() + Vec2::new(0.0, 72.0),
                Align2::CENTER_CENTER,
                format!("{} notas medidas", calibration.measured()),
                FontId::proportional(15.0),
                Color32::from_gray(150),
            );
        }

        if let Some(ms) = save {
            self.input_offset_ms = ms;
            self.screen = Screen::Library;
        } else if again {
            self.screen = Screen::Calibration(Box::default());
        } else if close {
            self.screen = Screen::Library;
        }
    }

    fn piano(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let key_w = width / self.layout.white_count as f32;
        let height = (key_w * 5.2).clamp(90.0, 240.0);
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());

        // Tocar con el mouse.
        let hovered_key = response.interact_pointer_pos().and_then(|pos| {
            let x = (pos.x - rect.left()) / key_w;
            let depth = (pos.y - rect.top()) / rect.height();
            self.layout.key_at(x, depth)
        });
        let target = if response.is_pointer_button_down_on() {
            hovered_key
        } else {
            None
        };
        if target != self.mouse_note {
            if let Some(old) = self.mouse_note {
                self.release(old);
            }
            if let Some(new) = target {
                self.press(new, FALLBACK_VELOCITY, Instant::now());
            }
            self.mouse_note = target;
        }

        let session = match &self.screen {
            Screen::Playing(s) => Some(s),
            _ => None,
        };
        let guide = session.map(|s| s.guide()).unwrap_or_default();

        let painter = ui.painter_at(rect);
        painter.rect_filled(
            Rect::from_min_size(rect.min - Vec2::new(0.0, 4.0), Vec2::new(width, 4.0)),
            0.0,
            ACCENT_DIM,
        );
        for key in &self.layout.keys {
            let x = rect.left() + key.x * key_w;
            let w = key.width * key_w;
            let h = if key.black {
                rect.height() * BLACK_LENGTH
            } else {
                rect.height()
            };
            let key_rect = Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(w, h));
            let feedback = session.and_then(|s| s.feedback(key.pitch));
            let guided = guide.iter().find(|(p, _)| *p == key.pitch).map(|(_, h)| *h);
            let base = if key.black { BLACK_KEY } else { WHITE_KEY };
            let fill = match (self.keys.get(key.pitch), feedback) {
                (KeyState::Down { .. }, Some(Feedback::Hit)) => falling::HIT,
                (KeyState::Down { .. }, Some(Feedback::Wrong)) => falling::MISSED,
                (KeyState::Down { velocity }, None) => lit(velocity, 1.0),
                (KeyState::Sustained { velocity }, _) => lit(velocity, 0.45),
                (KeyState::Up, _) => match guided {
                    Some(hand) => mix(base, falling::hand_color(hand), 0.55),
                    None => base,
                },
            };
            let radius = CornerRadius {
                nw: 0,
                ne: 0,
                sw: 4,
                se: 4,
            };
            painter.rect_filled(key_rect.shrink2(Vec2::new(0.5, 0.0)), radius, fill);
            painter.rect_stroke(
                key_rect,
                radius,
                Stroke::new(1.0, KEY_EDGE),
                StrokeKind::Inside,
            );

            if !key.black && key.pitch % 12 == 0 && key_w > 13.0 {
                painter.text(
                    Pos2::new(x + w / 2.0, rect.bottom() - 6.0),
                    Align2::CENTER_BOTTOM,
                    note_name(key.pitch),
                    FontId::proportional((key_w * 0.42).min(12.0)),
                    Color32::from_gray(110),
                );
            }
        }
    }

    fn diagnostics(&mut self, ctx: &egui::Context) {
        let mut open = self.show_diagnostics;
        let mut exported = None;
        let mut quality_change = None;
        egui::Window::new("Diagnóstico")
            .open(&mut open)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.heading("Puertos MIDI de entrada");
                if self.ports.is_empty() {
                    ui.label("No se detecta ningún puerto.");
                }
                for port in &self.ports {
                    let connected = self.connection == Connection::Connected(port.clone());
                    let kind = match classify_port(port) {
                        PortKind::Device => "dispositivo",
                        PortKind::Virtual => "virtual",
                    };
                    let state = if connected { ", conectado" } else { "" };
                    ui.label(format!("{port}  ({kind}{state})"));
                }
                ui.separator();
                ui.heading("Latencia de pantalla");
                match (self.latency.mean(), self.latency.max()) {
                    (Some(mean), Some(max)) => ui.label(format!(
                        "De la llegada de la nota al cuadro que la muestra: media {mean:.1} ms, máxima {max:.1} ms ({} notas). \
                         A esto se suma un cuadro de pantalla.",
                        self.latency.count()
                    )),
                    _ => ui.label("Tocá algunas notas en el teclado MIDI para medir."),
                };
                ui.separator();
                ui.heading("Audio");
                match &self.audio {
                    Some(audio) => {
                        let info = audio.info();
                        let buffer = match info.buffer_frames {
                            Some(frames) => format!(
                                "buffer de {frames} muestras ({:.1} ms)",
                                frames as f32 * 1000.0 / info.sample_rate as f32
                            ),
                            None => "buffer elegido por el sistema".to_string(),
                        };
                        ui.label(format!(
                            "{}: {} Hz, {} canales, {buffer}",
                            info.device, info.sample_rate, info.channels
                        ));
                        match (self.sound_latency.mean(), self.sound_latency.max()) {
                            (Some(mean), Some(max)) => ui.label(format!(
                                "De la tecla al sonido: media {mean:.1} ms, máxima {max:.1} ms ({} notas). \
                                 Incluye la espera del buffer y la salida de la placa.",
                                self.sound_latency.count()
                            )),
                            _ => ui.label("Tocá algunas notas con el sonido activado para medir."),
                        };
                    }
                    None => {
                        ui.label(format!(
                            "Sin salida de audio: {}. Clavium lo vuelve a intentar solo.",
                            self.audio_error.as_deref().unwrap_or("error desconocido")
                        ));
                    }
                }
                ui.separator();
                ui.heading("Créditos");
                ui.label(SALAMANDER.credit);
                if let Some(bank) = &self.piano_bank {
                    ui.label(format!(
                        "Piano cargado: {} ({:.0} MB en memoria, calidad {}).",
                        bank.name,
                        bank.memory_bytes() as f64 / 1e6,
                        self.quality.label()
                    ));
                }
                ui.horizontal(|ui| {
                    ui.label("Calidad del Yamaha C5:");
                    for q in [Quality::Standard, Quality::Full] {
                        if ui.selectable_label(self.quality == q, q.label()).clicked() && self.quality != q {
                            quality_change = Some(q);
                        }
                    }
                });
                ui.separator();
                ui.heading("Revisiones");
                ui.label("Exporta las piezas que revisaste, para sumarlas a catalog/revisiones.txt.");
                if ui.button("Exportar revisiones").clicked() {
                    exported = Some(self.export_reviews());
                }
                ui.separator();
                ui.heading("Últimos eventos");
                for line in &self.log {
                    ui.monospace(line);
                }
            });
        if let Some(q) = quality_change {
            self.quality = q;
            if self.sampled {
                // Con la calidad nueva hay que bajar (o dejar de usar) capas.
                self.use_synth();
                self.start_piano(ctx, true);
            }
        }
        if let Some(result) = exported {
            self.library_message = Some(match result {
                Ok(path) => format!("Revisiones exportadas en {}", path.display()),
                Err(e) => format!("No se pudieron exportar las revisiones: {e}"),
            });
        }
        self.show_diagnostics = open;
    }
}

impl eframe::App for ClaviumApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_midi();
        self.handle_computer_keyboard(ctx);
        self.update_audio();
        self.update_piano();
        self.update_session();
        self.update_calibration();
        self.update_catalog(ctx);
        if matches!(self.screen, Screen::Playing(_) | Screen::Calibration(_)) {
            // Las notas caen: hay que dibujar todos los cuadros.
            ctx.request_repaint();
        } else if self.audio.is_none() {
            ctx.request_repaint_after(AUDIO_RETRY);
        } else if self.show_diagnostics {
            // Las mediciones de audio llegan después de la nota.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("status").show(ui, |ui| {
            ui.add_space(6.0);
            self.status_bar(ui);
            ui.add_space(6.0);
        });
        egui::Panel::bottom("piano")
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 8)))
            .show(ui, |ui| self.piano(ui));
        // El mismo margen lateral que el teclado, para que las notas caigan
        // exactamente sobre sus teclas.
        let central =
            egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::symmetric(8, 8));
        egui::CentralPanel::default()
            .frame(central)
            .show(ui, |ui| match self.screen {
                Screen::Library => self.library_screen(ui),
                Screen::Playing(_) => self.playing_screen(ui),
                Screen::Results { .. } => self.results_screen(ui),
                Screen::Calibration(_) => self.calibration_screen(ui),
            });
        self.save_settings();

        if self.show_diagnostics {
            let ctx = ui.ctx().clone();
            self.diagnostics(&ctx);
        }
        let ctx = ui.ctx().clone();
        self.import_dialog(&ctx);

        let now = Instant::now();
        for at in self.pending.drain(..) {
            self.latency
                .record(now.duration_since(at).as_secs_f32() * 1000.0);
        }
    }
}

/// Punto de color para el estado de la conexión. Se dibuja en vez de usar
/// "●" porque la fuente por defecto no trae ese carácter.
fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

/// Marca como revisadas las piezas que este usuario ya revisó.
fn apply_local_reviews(library: &mut Library, store: Option<&Store>) {
    let Some(store) = store else {
        return;
    };
    let reviewed: Vec<String> = store.reviews().into_iter().map(|(id, _, _)| id).collect();
    for entry in &mut library.entries {
        entry.reviewed_here = reviewed.contains(&entry.id);
    }
}

fn load_stats(library: &Library, store: Option<&Store>) -> HashMap<String, PieceStats> {
    library
        .entries
        .iter()
        .map(|e| {
            let stats = store.map(|s| s.piece_stats(&e.id)).unwrap_or_default();
            (e.id.clone(), stats)
        })
        .collect()
}

/// Resumen del progreso de una pieza para su tarjeta en la biblioteca.
fn progress_text(stats: &PieceStats) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(best) = stats.best_accuracy {
        parts.push(format!("Mejor: {:.0} %", best * 100.0));
    }
    if let Some(speed) = stats.best_speed {
        parts.push(format!("Dominada al {:.0} %", speed * 100.0));
    }
    if stats.sessions > 0 {
        let plural = if stats.sessions == 1 {
            "sesión"
        } else {
            "sesiones"
        };
        parts.push(format!("{} {plural}", stats.sessions));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn hand_label(hand: PartHand) -> &'static str {
    match hand {
        PartHand::Auto => "Automática",
        PartHand::Right => "Derecha",
        PartHand::Left => "Izquierda",
        PartHand::Split => "Según la altura",
    }
}

fn curve_key(curve: VelocityCurve) -> &'static str {
    match curve {
        VelocityCurve::Light => "light",
        VelocityCurve::Normal => "normal",
        VelocityCurve::Heavy => "heavy",
        VelocityCurve::Fixed => "fixed",
    }
}

fn curve_from_key(key: &str) -> Option<VelocityCurve> {
    VelocityCurve::ALL
        .into_iter()
        .find(|c| curve_key(*c) == key)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let ch = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
    Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

/// Tres estrellas, llenas las ganadas. Se dibujan porque la fuente por
/// defecto no trae el carácter de estrella.
fn stars(ui: &mut egui::Ui, earned: u8) {
    let size = 34.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(size * 3.6, size * 1.2), Sense::hover());
    let painter = ui.painter_at(rect);
    for i in 0..3u8 {
        let center = Pos2::new(
            rect.left() + size * (0.6 + 1.2 * f32::from(i)),
            rect.center().y,
        );
        let color = if i < earned {
            Color32::from_rgb(250, 200, 70)
        } else {
            Color32::from_gray(70)
        };
        let point = |k: usize, r: f32| {
            let angle = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 5.0;
            center + Vec2::new(angle.cos(), angle.sin()) * r
        };
        // Una estrella no es convexa: pentágono central más cinco puntas.
        let inner: Vec<Pos2> = (0..5).map(|k| point(2 * k + 1, size * 0.22)).collect();
        painter.add(egui::Shape::convex_polygon(
            inner.clone(),
            color,
            Stroke::NONE,
        ));
        for k in 0..5 {
            let tip = point(2 * k, size * 0.5);
            let left = inner[(k + 4) % 5];
            let right = inner[k];
            painter.add(egui::Shape::convex_polygon(
                vec![left, tip, right],
                color,
                Stroke::NONE,
            ));
        }
    }
}

/// Color de una tecla encendida: más brillante cuanto más fuerte se tocó.
fn lit(velocity: u8, strength: f32) -> Color32 {
    let t = (0.35 + 0.65 * f32::from(velocity) / 127.0) * strength;
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t) as u8;
    let base = Color32::from_rgb(40, 44, 52);
    Color32::from_rgb(
        mix(base.r(), ACCENT_BRIGHT.r()),
        mix(base.g(), ACCENT_BRIGHT.g()),
        mix(base.b(), ACCENT_BRIGHT.b()),
    )
}

/// Semitonos desde Do para la fila central del teclado de la computadora,
/// con las negras en la fila de arriba, como en los secuenciadores.
fn computer_key_offset(key: egui::Key) -> Option<u8> {
    use egui::Key::*;
    Some(match key {
        A => 0,
        W => 1,
        S => 2,
        E => 3,
        D => 4,
        F => 5,
        T => 6,
        G => 7,
        Y => 8,
        H => 9,
        U => 10,
        J => 11,
        K => 12,
        O => 13,
        L => 14,
        _ => return None,
    })
}
