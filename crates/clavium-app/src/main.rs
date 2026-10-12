//! Clavium: conectá tu teclado MIDI y tocá.
//!
//! Fase 1: detección del teclado, reconexión en caliente y teclado en
//! pantalla que se ilumina con lo que tocás.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod keyboard;
mod live;

use std::collections::VecDeque;
use std::time::Instant;

use clavium_midi::{KeyEvent, MidiEvent, MidiService, PortKind, ServiceConfig, classify_port};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    Vec2,
};

use keyboard::{BLACK_LENGTH, KeyboardLayout, KeyboardSize, note_name};
use live::{KeyState, LatencyStats, LiveKeys};

const ACCENT_DIM: Color32 = Color32::from_rgb(28, 86, 150);
const ACCENT_BRIGHT: Color32 = Color32::from_rgb(110, 196, 255);
const WHITE_KEY: Color32 = Color32::from_rgb(244, 243, 238);
const BLACK_KEY: Color32 = Color32::from_rgb(28, 28, 32);
const KEY_EDGE: Color32 = Color32::from_rgb(90, 90, 96);
const OK_GREEN: Color32 = Color32::from_rgb(90, 200, 120);
const WARN_AMBER: Color32 = Color32::from_rgb(235, 180, 70);

/// Velocity de las notas tocadas con el teclado de la computadora o el mouse.
const FALLBACK_VELOCITY: u8 = 90;

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

struct ClaviumApp {
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
}

impl ClaviumApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let ctx = cc.egui_ctx.clone();
        let midi = MidiService::start(ServiceConfig::default(), move || ctx.request_repaint());
        let size = KeyboardSize::Keys88;
        ClaviumApp {
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

    fn press(&mut self, pitch: u8, velocity: u8) {
        self.keys.note_on(pitch, velocity);
        self.out_of_range = (!self.layout.contains(pitch)).then_some(pitch);
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
                    self.connection = Connection::Searching { lost: Some(port) };
                }
                MidiEvent::ConnectFailed { port, reason } => {
                    self.push_log(format!("no se pudo abrir {port}: {reason}"));
                    self.connection = Connection::Failed { port, reason };
                }
                MidiEvent::Key { event, at } => match event {
                    KeyEvent::NoteOn { pitch, velocity } => {
                        self.press(pitch, velocity);
                        self.pending.push(at);
                        self.push_log(format!("{} vel {velocity}", note_name(pitch)));
                    }
                    KeyEvent::NoteOff { pitch } => self.keys.note_off(pitch),
                    KeyEvent::Sustain { down } => {
                        self.keys.set_sustain(down);
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
                    self.press(pitch, FALLBACK_VELOCITY);
                } else {
                    self.keys.note_off(pitch);
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
            });
        });
    }

    fn now_playing(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.25);
            let down: Vec<String> = self.keys.down().map(note_name).collect();
            if down.is_empty() {
                ui.label(
                    RichText::new("Tocá una tecla")
                        .size(40.0)
                        .color(ui.visuals().weak_text_color()),
                );
                ui.add_space(8.0);
                if !matches!(self.connection, Connection::Connected(_)) {
                    ui.label(
                        "Sin teclado MIDI podés tocar con la computadora: A W S E D F T G Y H U J K \
                         (Z y X cambian de octava) o con el mouse.",
                    );
                }
            } else {
                ui.label(RichText::new(down.join("  ")).size(48.0).strong());
            }
            ui.add_space(12.0);
            if self.keys.sustain() {
                ui.label(RichText::new("Pedal presionado").color(ACCENT_BRIGHT));
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
                self.keys.note_off(old);
            }
            if let Some(new) = target {
                self.press(new, FALLBACK_VELOCITY);
            }
            self.mouse_note = target;
        }

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
            let fill = match self.keys.get(key.pitch) {
                KeyState::Down { velocity } => lit(velocity, 1.0),
                KeyState::Sustained { velocity } => lit(velocity, 0.45),
                KeyState::Up if key.black => BLACK_KEY,
                KeyState::Up => WHITE_KEY,
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
                ui.heading("Latencia");
                match (self.latency.mean(), self.latency.max()) {
                    (Some(mean), Some(max)) => ui.label(format!(
                        "De la llegada de la nota al cuadro que la muestra: media {mean:.1} ms, máxima {max:.1} ms ({} notas). \
                         A esto se suma un cuadro de pantalla.",
                        self.latency.count()
                    )),
                    _ => ui.label("Tocá algunas notas en el teclado MIDI para medir."),
                };
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
        egui::CentralPanel::default().show(ui, |ui| self.now_playing(ui));

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
