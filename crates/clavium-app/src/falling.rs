//! Notas que caen hacia el teclado.

use clavium_core::Hand;
use clavium_eval::Outcome;
use eframe::egui::{Color32, CornerRadius, Painter, Pos2, Rect, Stroke, Vec2};

use crate::keyboard::KeyboardLayout;
use crate::session::Session;

/// Segundos reales que tarda una nota en caer desde arriba hasta el teclado.
pub const LOOKAHEAD_SECONDS: f64 = 3.0;

pub const RIGHT_HAND: Color32 = Color32::from_rgb(86, 170, 255);
pub const LEFT_HAND: Color32 = Color32::from_rgb(255, 160, 70);
pub const HIT: Color32 = Color32::from_rgb(90, 210, 120);
pub const MISSED: Color32 = Color32::from_rgb(220, 80, 80);
const AUTO: Color32 = Color32::from_rgb(120, 120, 130);
const BAR_LINE: Color32 = Color32::from_rgb(52, 52, 58);
const IMPACT_LINE: Color32 = Color32::from_rgb(110, 196, 255);

pub fn hand_color(hand: Hand) -> Color32 {
    match hand {
        Hand::Left => LEFT_HAND,
        Hand::Right | Hand::Unknown => RIGHT_HAND,
    }
}

/// Dibuja las notas visibles. `key_width` es el ancho de una tecla blanca
/// en píxeles, el mismo que usa el teclado de abajo.
pub fn draw(
    painter: &Painter,
    rect: Rect,
    layout: &KeyboardLayout,
    key_width: f32,
    session: &Session,
) {
    let t = session.clock.position();
    let window = LOOKAHEAD_SECONDS * session.speed;
    let y_of = |time: f64| rect.bottom() - ((time - t) / window) as f32 * rect.height();

    // Líneas de compás.
    for &bar in session.eval.bar_starts() {
        if bar >= t && bar <= t + window {
            let y = y_of(bar);
            painter.hline(rect.x_range(), y, Stroke::new(1.0, BAR_LINE));
        }
    }

    let expected = session.eval.expected();
    let outcomes = session.eval.outcomes();
    // Primero las notas de teclas blancas, para que las negras queden encima.
    for black_pass in [false, true] {
        for (e, outcome) in expected.iter().zip(outcomes) {
            if e.end < t || e.start > t + window {
                continue;
            }
            let Some(key) = layout.key(e.pitch) else {
                continue;
            };
            if key.black != black_pass {
                continue;
            }
            let color = match outcome {
                Outcome::Pending => hand_color(e.hand),
                Outcome::Hit { .. } => HIT,
                Outcome::Missed => MISSED.gamma_multiply(0.6),
                Outcome::Ignored => AUTO.gamma_multiply(0.7),
            };
            let color = if key.black { darken(color) } else { color };
            let x = rect.left() + key.x * key_width;
            let w = key.width * key_width;
            let top = y_of(e.end).max(rect.top());
            let bottom = y_of(e.start).min(rect.bottom());
            if bottom <= top {
                continue;
            }
            let note_rect =
                Rect::from_min_max(Pos2::new(x + 1.5, top), Pos2::new(x + w - 1.5, bottom));
            painter.rect_filled(note_rect, CornerRadius::same(4), color);
            painter.rect_stroke(
                note_rect,
                CornerRadius::same(4),
                Stroke::new(1.0, Color32::from_black_alpha(90)),
                eframe::egui::StrokeKind::Inside,
            );
        }
    }

    // Línea de impacto.
    painter.rect_filled(
        Rect::from_min_size(
            Pos2::new(rect.left(), rect.bottom() - 2.0),
            Vec2::new(rect.width(), 3.0),
        ),
        0.0,
        IMPACT_LINE,
    );
}

fn darken(c: Color32) -> Color32 {
    Color32::from_rgb(
        (f32::from(c.r()) * 0.72) as u8,
        (f32::from(c.g()) * 0.72) as u8,
        (f32::from(c.b()) * 0.72) as u8,
    )
}
