//! Una sesión de práctica: el reloj musical, la evaluación de lo que toca
//! el usuario y las notas que toca la app (la otra mano o el modo escucha).

use std::time::Instant;

use clavium_audio::Command;
use clavium_core::{Hand, Piece};
use clavium_eval::{Evaluator, HandFilter, Judgement, Outcome, PracticeMode, Summary, Windows};

/// Segundos reales de cuenta previa: las primeras notas caen desde arriba.
pub const LEAD_IN_SECONDS: f64 = 3.0;
/// Espera al final antes de mostrar el resultado.
const TAIL_SECONDS: f64 = 1.0;

/// Posición en la pieza, en segundos a tempo original. Avanza según la
/// velocidad elegida, se congela en pausa y, en modo espera, no pasa del
/// acorde que falta tocar.
#[derive(Debug, Clone)]
pub struct MusicClock {
    position: f64,
    speed: f64,
    running: bool,
    last: Option<Instant>,
    /// Si en el último avance el reloj quedó frenado por el modo espera.
    held: bool,
}

impl MusicClock {
    pub fn new(start: f64, speed: f64) -> Self {
        MusicClock {
            position: start,
            speed,
            running: false,
            last: None,
            held: false,
        }
    }

    pub fn position(&self) -> f64 {
        self.position
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn play(&mut self) {
        self.running = true;
    }

    pub fn pause(&mut self) {
        self.running = false;
    }

    /// Avanza hasta `now`, sin pasar de `gate`.
    pub fn tick(&mut self, now: Instant, gate: Option<f64>) -> f64 {
        if self.running
            && let Some(last) = self.last
        {
            self.position += now.saturating_duration_since(last).as_secs_f64() * self.speed;
        }
        self.last = Some(now);
        self.held = false;
        if let Some(gate) = gate
            && self.position > gate
        {
            self.position = gate;
            self.held = true;
        }
        self.position
    }

    /// Posición musical que correspondía al instante `at`, anterior al
    /// último avance. Así una nota se evalúa cuando llegó, no cuando se dibujó.
    pub fn time_at(&self, at: Instant) -> f64 {
        match self.last {
            Some(last) if self.running && !self.held => {
                self.position - last.saturating_duration_since(at).as_secs_f64() * self.speed
            }
            _ => self.position,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feedback {
    Hit,
    Wrong,
}

pub struct Session {
    pub title: String,
    pub piece: Piece,
    pub eval: Evaluator,
    pub clock: MusicClock,
    pub hands: HandFilter,
    pub mode: PracticeMode,
    pub speed: f64,
    /// Índices de las notas que toca la app, en orden de inicio.
    auto: Vec<usize>,
    auto_next: usize,
    /// (fin, altura) de las notas de la app que están sonando.
    auto_sounding: Vec<(f64, u8)>,
    feedback: [Option<Feedback>; 128],
    finished: bool,
}

impl Session {
    pub fn new(piece: Piece, hands: HandFilter, mode: PracticeMode, speed: f64) -> Self {
        let eval = Evaluator::new(&piece, hands, mode, Windows::default(), speed);
        let auto = eval
            .outcomes()
            .iter()
            .enumerate()
            .filter(|(_, o)| **o == Outcome::Ignored)
            .map(|(i, _)| i)
            .collect();
        let mut clock = MusicClock::new(-LEAD_IN_SECONDS * speed, speed);
        clock.play();
        Session {
            title: piece.metadata.title.clone(),
            piece,
            eval,
            clock,
            hands,
            mode,
            speed,
            auto,
            auto_next: 0,
            auto_sounding: Vec::new(),
            feedback: [None; 128],
            finished: false,
        }
    }

    /// Vuelve a empezar con las mismas opciones.
    pub fn restarted(&self) -> Self {
        Session::new(self.piece.clone(), self.hands, self.mode, self.speed)
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn summary(&self) -> Summary {
        self.eval.summary()
    }

    /// Progreso de 0 a 1.
    pub fn progress(&self) -> f32 {
        let end = self.eval.end_time().max(0.001);
        (self.clock.position() / end).clamp(0.0, 1.0) as f32
    }

    pub fn feedback(&self, pitch: u8) -> Option<Feedback> {
        self.feedback.get(usize::from(pitch)).copied().flatten()
    }

    /// Avanza la sesión hasta `now` y devuelve lo que tiene que sonar.
    pub fn update(&mut self, now: Instant) -> Vec<Command> {
        let mut commands = Vec::new();
        if self.finished {
            return commands;
        }
        let gate = self.eval.wait_gate();
        let t = self.clock.tick(now, gate);
        self.eval.advance(t);

        if self.clock.is_running() {
            // En modo espera la otra mano no se adelanta al acorde que falta.
            while let Some(&i) = self.auto.get(self.auto_next) {
                let e = self.eval.expected()[i];
                let blocked = gate.is_some_and(|g| e.start >= g - 1e-6);
                if e.start > t || blocked {
                    break;
                }
                commands.push(Command::NoteOn {
                    pitch: e.pitch,
                    velocity: e.velocity,
                });
                self.auto_sounding.push((e.end, e.pitch));
                self.auto_next += 1;
            }
            self.auto_sounding.retain(|&(end, pitch)| {
                let done = end <= t;
                if done {
                    commands.push(Command::NoteOff { pitch });
                }
                !done
            });
        }

        let tail = TAIL_SECONDS * self.speed;
        if self.eval.is_complete() && t >= self.eval.end_time() + tail {
            self.finished = true;
            commands.extend(self.stop_auto());
        }
        commands
    }

    /// Registra una nota del usuario que llegó en `at`.
    pub fn on_user_note(&mut self, pitch: u8, at: Instant) -> Option<Judgement> {
        if self.finished || self.mode == PracticeMode::Listen {
            return None;
        }
        let t = self.clock.time_at(at);
        let judgement = self.eval.on_note(pitch, t);
        if let Some(slot) = self.feedback.get_mut(usize::from(pitch)) {
            *slot = Some(match judgement {
                Judgement::Hit { .. } => Feedback::Hit,
                Judgement::Wrong { .. } => Feedback::Wrong,
            });
        }
        Some(judgement)
    }

    pub fn on_user_release(&mut self, pitch: u8) {
        if let Some(slot) = self.feedback.get_mut(usize::from(pitch)) {
            *slot = None;
        }
    }

    pub fn toggle_pause(&mut self) -> Vec<Command> {
        if self.clock.is_running() {
            self.clock.pause();
            self.stop_auto()
        } else {
            self.clock.play();
            Vec::new()
        }
    }

    /// Corta las notas que está tocando la app.
    pub fn stop_auto(&mut self) -> Vec<Command> {
        self.auto_sounding
            .drain(..)
            .map(|(_, pitch)| Command::NoteOff { pitch })
            .collect()
    }

    /// Notas a resaltar en el teclado, con su mano: el acorde que hay que tocar.
    pub fn guide(&self) -> Vec<(u8, Hand)> {
        if self.mode == PracticeMode::Listen {
            return Vec::new();
        }
        let hand_of = |pitch: u8| {
            self.eval
                .expected()
                .iter()
                .zip(self.eval.outcomes())
                .find(|(e, o)| e.pitch == pitch && **o == Outcome::Pending)
                .map_or(Hand::Unknown, |(e, _)| e.hand)
        };
        let group: Vec<(u8, Hand)> = self
            .eval
            .next_group()
            .into_iter()
            .map(|p| (p, hand_of(p)))
            .collect();
        match self.eval.wait_gate() {
            Some(_) => group,
            None => {
                // En modo ritmo, solo cuando la nota está por llegar.
                let soon = self
                    .eval
                    .expected()
                    .iter()
                    .zip(self.eval.outcomes())
                    .find(|(_, o)| **o == Outcome::Pending)
                    .is_some_and(|(e, _)| e.start - self.clock.position() < 0.4 * self.speed);
                if soon { group } else { Vec::new() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clavium_core::notation::parse_piece;
    use std::time::Duration;

    fn piece() -> Piece {
        parse_piece("title: x\ntempo: 60\ntime: 4/4\nright: C4 D4 E4 F4\nleft: C3:4").unwrap()
    }

    #[test]
    fn clock_follows_speed_and_stops_at_the_gate() {
        let t0 = Instant::now();
        let mut clock = MusicClock::new(0.0, 0.5);
        clock.tick(t0, None);
        clock.play();
        assert!((clock.tick(t0 + Duration::from_secs(2), None) - 1.0).abs() < 1e-9);
        assert_eq!(clock.tick(t0 + Duration::from_secs(4), Some(1.5)), 1.5);
        clock.pause();
        assert_eq!(clock.tick(t0 + Duration::from_secs(6), None), 1.5);
    }

    #[test]
    fn notes_are_judged_at_arrival_time() {
        let t0 = Instant::now();
        let mut clock = MusicClock::new(0.0, 1.0);
        clock.tick(t0, None);
        clock.play();
        clock.tick(t0 + Duration::from_millis(100), None);
        let t = clock.time_at(t0 + Duration::from_millis(40));
        assert!((t - 0.04).abs() < 1e-9);
    }

    #[test]
    fn the_app_plays_the_other_hand() {
        let t0 = Instant::now();
        let mut s = Session::new(piece(), HandFilter::Right, PracticeMode::Rhythm, 1.0);
        s.update(t0);
        let early = s.update(t0 + Duration::from_secs_f64(LEAD_IN_SECONDS - 0.1));
        assert!(early.is_empty());
        let on = s.update(t0 + Duration::from_secs_f64(LEAD_IN_SECONDS + 0.01));
        assert_eq!(
            on,
            vec![Command::NoteOn {
                pitch: 48,
                velocity: 80
            }]
        );
        let off = s.update(t0 + Duration::from_secs_f64(LEAD_IN_SECONDS + 4.01));
        assert!(off.contains(&Command::NoteOff { pitch: 48 }));
    }

    #[test]
    fn wait_mode_holds_the_other_hand_until_the_chord_is_played() {
        let t0 = Instant::now();
        let mut s = Session::new(piece(), HandFilter::Right, PracticeMode::Wait, 1.0);
        s.update(t0);
        let waiting = s.update(t0 + Duration::from_secs_f64(LEAD_IN_SECONDS + 1.0));
        assert!(waiting.is_empty(), "la izquierda espera al Do4");
        assert_eq!(s.guide(), vec![(60, Hand::Right)]);

        let at = t0 + Duration::from_secs_f64(LEAD_IN_SECONDS + 1.1);
        assert!(matches!(
            s.on_user_note(60, at),
            Some(Judgement::Hit { .. })
        ));
        assert_eq!(s.feedback(60), Some(Feedback::Hit));
        let after = s.update(at);
        assert!(after.contains(&Command::NoteOn {
            pitch: 48,
            velocity: 80
        }));
    }

    #[test]
    fn a_session_finishes_after_the_last_note() {
        let t0 = Instant::now();
        let mut s = Session::new(piece(), HandFilter::Right, PracticeMode::Rhythm, 1.0);
        s.update(t0);
        s.update(t0 + Duration::from_secs_f64(LEAD_IN_SECONDS + 4.5));
        assert!(!s.is_finished());
        s.update(t0 + Duration::from_secs_f64(LEAD_IN_SECONDS + 5.1));
        assert!(s.is_finished());
        assert_eq!(s.summary().missed, 4);
    }
}
