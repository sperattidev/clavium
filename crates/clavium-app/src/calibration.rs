//! Calibración: barras que caen al ritmo de un metrónomo. El usuario toca
//! cualquier tecla cuando cada barra llega a la línea, y el desvío medio
//! (de su equipo y suyo) se descuenta después al evaluar.

use std::time::Instant;

use clavium_audio::Command;

use crate::session::MusicClock;

/// Pulsos del metrónomo: 100 BPM, 20 pulsos.
pub const INTERVAL: f64 = 0.6;
pub const BEATS: usize = 20;
/// Los primeros pulsos sirven para tomar el ritmo y no se miden.
const WARMUP: usize = 4;
/// Pulsos medidos necesarios para dar un resultado.
pub const MIN_TAPS: usize = 8;
const LEAD_IN: f64 = 3.0;
/// Más lejos que esto, el toque no corresponde a ningún pulso.
const MAX_DISTANCE: f64 = 0.25;
const CLICK_PITCH: u8 = 96;
const CLICK_LENGTH: f64 = 0.08;

pub struct Calibration {
    pub clock: MusicClock,
    offsets: Vec<f64>,
    next_click: usize,
    click_off: Option<f64>,
    tapped: Vec<bool>,
}

impl Default for Calibration {
    fn default() -> Self {
        let mut clock = MusicClock::new(-LEAD_IN, 1.0);
        clock.play();
        Calibration {
            clock,
            offsets: Vec::new(),
            next_click: 0,
            click_off: None,
            tapped: vec![false; BEATS],
        }
    }
}

impl Calibration {
    pub fn beat_time(i: usize) -> f64 {
        i as f64 * INTERVAL
    }

    pub fn is_finished(&self) -> bool {
        self.clock.position() > Self::beat_time(BEATS - 1) + 1.0
    }

    /// Pulsos medidos hasta ahora.
    pub fn measured(&self) -> usize {
        self.offsets.len()
    }

    /// Desvío mediano en segundos: positivo si el usuario toca tarde.
    pub fn result(&self) -> Option<f64> {
        if self.offsets.len() < MIN_TAPS {
            return None;
        }
        let mut sorted = self.offsets.clone();
        sorted.sort_by(f64::total_cmp);
        let mid = sorted.len() / 2;
        Some(if sorted.len().is_multiple_of(2) {
            (sorted[mid - 1] + sorted[mid]) / 2.0
        } else {
            sorted[mid]
        })
    }

    /// Avanza y devuelve el sonido del metrónomo.
    pub fn update(&mut self, now: Instant) -> Vec<Command> {
        let t = self.clock.tick(now, None);
        let mut commands = Vec::new();
        if let Some(off) = self.click_off
            && t >= off
        {
            commands.push(Command::NoteOff { pitch: CLICK_PITCH });
            self.click_off = None;
        }
        if self.next_click < BEATS && t >= Self::beat_time(self.next_click) {
            // Los primeros pulsos más fuertes, para marcar el compás.
            let velocity = if self.next_click < WARMUP { 110 } else { 80 };
            commands.push(Command::NoteOn {
                pitch: CLICK_PITCH,
                velocity,
            });
            self.click_off = Some(t + CLICK_LENGTH);
            self.next_click += 1;
        }
        commands
    }

    /// Registra un toque de cualquier tecla.
    pub fn on_tap(&mut self, at: Instant) {
        let t = self.clock.time_at(at);
        let nearest = ((t / INTERVAL).round().max(0.0) as usize).min(BEATS - 1);
        let offset = t - Self::beat_time(nearest);
        if nearest >= WARMUP && offset.abs() <= MAX_DISTANCE && !self.tapped[nearest] {
            self.tapped[nearest] = true;
            self.offsets.push(offset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn measures_the_median_delay_and_ignores_the_warmup() {
        let t0 = Instant::now();
        let mut cal = Calibration::default();
        cal.update(t0);
        for i in 0..BEATS {
            // 40 ms tarde, con un toque muy desviado que la mediana descarta.
            let delay = if i == 10 { 0.2 } else { 0.04 };
            let at = t0 + Duration::from_secs_f64(LEAD_IN + Calibration::beat_time(i) + delay);
            cal.update(at);
            cal.on_tap(at);
            // Un segundo toque en el mismo pulso no cuenta.
            cal.on_tap(at);
        }
        assert_eq!(cal.measured(), BEATS - WARMUP);
        assert!((cal.result().unwrap() - 0.04).abs() < 1e-6);
    }

    #[test]
    fn needs_enough_taps() {
        let t0 = Instant::now();
        let mut cal = Calibration::default();
        cal.update(t0);
        let at = t0 + Duration::from_secs_f64(LEAD_IN + Calibration::beat_time(5));
        cal.update(at);
        cal.on_tap(at);
        assert_eq!(cal.result(), None);
    }

    #[test]
    fn plays_one_click_per_beat() {
        let t0 = Instant::now();
        let mut cal = Calibration::default();
        cal.update(t0);
        let mut clicks = 0;
        for ms in (0..((LEAD_IN + 13.0) * 1000.0) as u64).step_by(10) {
            let commands = cal.update(t0 + Duration::from_millis(ms));
            clicks += commands
                .iter()
                .filter(|c| matches!(c, Command::NoteOn { .. }))
                .count();
        }
        assert_eq!(clicks, BEATS);
        assert!(cal.is_finished());
    }
}
