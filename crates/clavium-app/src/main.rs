//! Clavium: conectá tu teclado MIDI y tocá.
//!
//! Conectás el teclado, elegís una pieza de la biblioteca y tocás mientras
//! las notas caen; la app evalúa cada nota y muestra el resultado.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod falling;
mod keyboard;
mod library;
mod live;
mod session;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use clavium_audio::{AudioEngine, Command};
use clavium_midi::{
    KeyEvent, MidiEvent, MidiService, PortKind, ServiceConfig, VelocityCurve, classify_port,
};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    Vec2,
};

use clavium_eval::{HandFilter, PracticeMode, Summary};
use keyboard::{BLACK_LENGTH, KeyboardLayout, KeyboardSize, note_name};
use library::Library;
use live::{KeyState, LatencyStats, LiveKeys};
use session::{Feedback, Session};

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

fn main() -> eframe::Result {
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
}

impl Default for PlayOptions {
    fn default() -> Self {
        PlayOptions {
            hands: HandFilter::Both,
            mode: PracticeMode::Wait,
            speed: 1.0,
        }
    }
}

enum Screen {
    Library,
    Playing(Box<Session>),
    Results {
        session: Box<Session>,
        summary: Summary,
    },
}

struct ClaviumApp {
    library: Library,
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
        let ctx = cc.egui_ctx.clone();
        let midi = MidiService::start(ServiceConfig::default(), move || ctx.request_repaint());
        let size = KeyboardSize::Keys88;
        let (audio, audio_error) = match AudioEngine::start() {
            Ok(engine) => (Some(engine), None),
            Err(e) => (None, Some(e.to_string())),
        };
        ClaviumApp {
            library: Library::bundled(),
            screen: Screen::Library,
            selected: 0,
            options: PlayOptions::default(),
            audio_retry_at: audio_error.as_ref().map(|_| Instant::now() + AUDIO_RETRY),
            audio,
            audio_error,
            sound_on: true,
            volume: 0.8,
            curve: VelocityCurve::Normal,
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
        }
    }

    fn press(&mut self, pitch: u8, velocity: u8, at: Instant) {
        self.keys.note_on(pitch, velocity);
        self.send_audio(Command::NoteOn { pitch, velocity });
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
        let Some(entry) = self.library.entries.get(self.selected) else {
            return;
        };
        // Que entre toda la pieza en el teclado de pantalla.
        if let Some(shift) = self.layout.shift_to_fit(entry.lowest, entry.highest)
            && let Some(layout) = KeyboardLayout::with_shift(self.size, shift)
        {
            self.layout = layout;
        }
        let options = self.options;
        self.screen = Screen::Playing(Box::new(Session::new(
            entry.piece.clone(),
            options.hands,
            options.mode,
            options.speed,
        )));
    }

    fn leave_session(&mut self) {
        let screen = std::mem::replace(&mut self.screen, Screen::Library);
        if let Screen::Playing(mut session) | Screen::Results { mut session, .. } = screen {
            let commands = session.stop_auto();
            self.send_auto(commands);
        }
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
            if let (Some(accuracy), Some(entry)) =
                (summary.accuracy, self.library.entries.get(self.selected))
                && session.mode == PracticeMode::Rhythm
            {
                self.library.record(entry.id, accuracy);
            }
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
                Ok(engine) => {
                    engine.set_volume(self.volume);
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
                } else {
                    ui.label(RichText::new("Sin salida de audio").color(WARN_AMBER))
                        .on_hover_text(self.audio_error.clone().unwrap_or_default());
                }
            });
        });
    }

    fn library_screen(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        ui.label(RichText::new("Biblioteca").size(26.0).strong());
        ui.label(
            RichText::new("Elegí una pieza y cómo practicarla.")
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(12.0);

        let cards: Vec<(String, String, Option<f64>)> = self
            .library
            .entries
            .iter()
            .map(|e| {
                (
                    e.piece.metadata.title.clone(),
                    e.subtitle(),
                    self.library.best.get(e.id).copied(),
                )
            })
            .collect();
        let mut start = false;

        ui.columns(2, |cols| {
            egui::ScrollArea::vertical()
                .id_salt("pieces")
                .show(&mut cols[0], |ui| {
                    for (i, (title, subtitle, best)) in cards.iter().enumerate() {
                        let selected = i == self.selected;
                        let fill = if selected {
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
                                ui.label(RichText::new(title).size(19.0).strong());
                                ui.label(
                                    RichText::new(subtitle).color(ui.visuals().weak_text_color()),
                                );
                                if let Some(best) = best {
                                    ui.label(format!(
                                        "Mejor puntaje en ritmo: {:.0} %",
                                        best * 100.0
                                    ));
                                }
                            })
                            .response
                            .interact(Sense::click());
                        if response.clicked() {
                            self.selected = i;
                        }
                        if response.double_clicked() {
                            self.selected = i;
                            start = true;
                        }
                        ui.add_space(8.0);
                    }
                });

            let ui = &mut cols[1];
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
                    PracticeMode::Wait => {
                        "La música se detiene hasta que tocás las notas correctas."
                    }
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
            ui.add_space(16.0);
            let tocar = ui.add_sized(
                [180.0, 44.0],
                egui::Button::new(RichText::new("Tocar").size(20.0)),
            );
            if tocar.clicked() {
                start = true;
            }
            if let Some(entry) = self.library.entries.get(self.selected)
                && self
                    .layout
                    .shift_to_fit(entry.lowest, entry.highest)
                    .is_none()
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
        });

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
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

        if start {
            self.start_session();
        }
    }

    fn playing_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::Playing(session) = &self.screen else {
            return;
        };
        let mut leave = false;
        let mut restart = false;
        let mut toggle = false;

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
            ui.label(
                RichText::new(format!("{mode} · {:.0} %", session.speed * 100.0))
                    .color(ui.visuals().weak_text_color()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
        let overlay = if !session.clock.is_running() {
            Some("En pausa. Espacio para seguir.".to_string())
        } else if position < 0.0 {
            Some(format!(
                "Empieza en {:.0}…",
                (-position / session.speed).ceil()
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
        if leave {
            self.leave_session();
        }
    }

    fn results_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::Results { session, summary } = &self.screen else {
            return;
        };
        let mut again = false;
        let mut leave = false;
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
                }
            }
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 300.0).max(0.0) / 2.0);
                if ui
                    .add_sized([140.0, 40.0], egui::Button::new("Repetir"))
                    .clicked()
                {
                    again = true;
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
        } else if leave {
            self.leave_session();
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
                ui.heading("Últimos eventos");
                for line in &self.log {
                    ui.monospace(line);
                }
            });
        self.show_diagnostics = open;
    }
}

impl eframe::App for ClaviumApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_midi();
        self.handle_computer_keyboard(ctx);
        self.update_audio();
        self.update_session();
        if matches!(self.screen, Screen::Playing(_)) {
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
            });

        if self.show_diagnostics {
            let ctx = ui.ctx().clone();
            self.diagnostics(&ctx);
        }

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
