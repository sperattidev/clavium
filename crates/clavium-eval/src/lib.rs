//! Motor de evaluación: empareja cada nota que toca el usuario con una nota
//! esperada de la partitura, una a una, y califica la diferencia de tiempo.
//!
//! Los tiempos que recibe son del reloj musical (segundos de la pieza a
//! tempo original). Las ventanas de tolerancia están en tiempo real, así que
//! se escalan con la velocidad: bajar el tempo no vuelve la evaluación más
//! estricta ni más laxa.

use clavium_core::{Hand, Piece};

/// Notas que empiezan a menos de esta distancia forman un mismo acorde.
const CHORD_TOLERANCE: f64 = 0.03;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PracticeMode {
    /// La música se detiene en cada grupo de notas hasta que se tocan todas.
    #[default]
    Wait,
    /// La música avanza sola y se evalúa el tiempo de cada nota.
    Rhythm,
    /// La app toca la pieza y el usuario solo mira.
    Listen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HandFilter {
    #[default]
    Both,
    Right,
    Left,
}

impl HandFilter {
    /// Si el usuario toca las notas de esta mano. Las notas sin mano asignada
    /// siempre las toca el usuario.
    pub fn plays(self, hand: Hand) -> bool {
        match (self, hand) {
            (HandFilter::Both, _) | (_, Hand::Unknown) => true,
            (HandFilter::Right, h) => h == Hand::Right,
            (HandFilter::Left, h) => h == Hand::Left,
        }
    }
}

/// Ventanas de tiempo en segundos reales, medidas desde el inicio de la nota.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Windows {
    pub perfect: f64,
    pub good: f64,
    /// Más allá de esta distancia la nota tocada no cuenta como esa nota.
    pub late: f64,
}

impl Default for Windows {
    fn default() -> Self {
        Windows {
            perfect: 0.05,
            good: 0.12,
            late: 0.25,
        }
    }
}

impl Windows {
    /// Ventanas más anchas o más angostas (por ejemplo 1,5 para principiantes).
    pub fn scaled(self, factor: f64) -> Self {
        Windows {
            perfect: self.perfect * factor,
            good: self.good * factor,
            late: self.late * factor,
        }
    }

    fn grade(&self, offset: f64) -> Grade {
        let d = offset.abs();
        if d <= self.perfect {
            Grade::Perfect
        } else if d <= self.good {
            Grade::Good
        } else {
            Grade::OffTime
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    Perfect,
    Good,
    /// Temprano o tarde, pero dentro de la ventana máxima.
    OffTime,
}

impl Grade {
    pub fn points(self) -> f64 {
        match self {
            Grade::Perfect => 1.0,
            Grade::Good => 0.7,
            Grade::OffTime => 0.4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    Pending,
    /// `offset` en segundos reales: positivo si se tocó tarde.
    Hit {
        grade: Grade,
        offset: f64,
    },
    Missed,
    /// La toca la app: es de la otra mano o el modo es escucha.
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Expected {
    pub pitch: u8,
    pub velocity: u8,
    /// Segundos de la pieza a tempo original.
    pub start: f64,
    pub end: f64,
    pub hand: Hand,
    /// Compás, contando desde 1.
    pub bar: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Judgement {
    Hit {
        index: usize,
        grade: Grade,
        offset: f64,
    },
    /// Tecla equivocada, nota de más o repetida.
    Wrong { pitch: u8 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// Notas que tocaba el usuario.
    pub total: usize,
    pub perfect: usize,
    pub good: usize,
    pub off_time: usize,
    pub missed: usize,
    pub wrong: usize,
    /// Precisión de 0 a 1. `None` si no había notas para evaluar.
    pub accuracy: Option<f64>,
    /// Desvío medio en milisegundos: positivo si el usuario tiende a atrasarse.
    pub mean_offset_ms: Option<f64>,
    /// De 0 a 3. Solo se ganan al tempo original y en modo ritmo.
    pub stars: u8,
    /// Hasta tres compases con más fallas, de más a menos.
    pub weakest_bars: Vec<usize>,
}

/// Cuánto resta de la precisión cada nota equivocada.
pub const WRONG_NOTE_PENALTY: f64 = 0.02;
/// Precisión necesaria para 1, 2 y 3 estrellas.
pub const STAR_THRESHOLDS: [f64; 3] = [0.70, 0.85, 0.95];

#[derive(Debug, Clone)]
pub struct Evaluator {
    expected: Vec<Expected>,
    outcomes: Vec<Outcome>,
    bar_starts: Vec<f64>,
    windows: Windows,
    mode: PracticeMode,
    speed: f64,
    /// (altura, compás) de cada nota equivocada.
    wrong: Vec<(u8, usize)>,
    /// Primera nota que puede seguir pendiente.
    cursor: usize,
}

impl Evaluator {
    /// `speed` es la velocidad de reproducción: 0,5 es la mitad del tempo.
    pub fn new(
        piece: &Piece,
        hands: HandFilter,
        mode: PracticeMode,
        windows: Windows,
        speed: f64,
    ) -> Self {
        let bar_starts: Vec<f64> = piece
            .bar_ticks()
            .into_iter()
            .map(|t| piece.tempo.tick_to_seconds(t))
            .collect();
        let bar_of = |t: f64| bar_starts.partition_point(|&b| b <= t + 1e-9).max(1);

        let mut expected: Vec<Expected> = piece
            .notes
            .iter()
            .map(|n| {
                let (start, end) = piece.note_seconds(n);
                Expected {
                    pitch: n.pitch,
                    velocity: n.velocity,
                    start,
                    end,
                    hand: n.hand,
                    bar: bar_of(start),
                }
            })
            .collect();
        expected.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));

        let outcomes = expected
            .iter()
            .map(|e| {
                if mode == PracticeMode::Listen || !hands.plays(e.hand) {
                    Outcome::Ignored
                } else {
                    Outcome::Pending
                }
            })
            .collect();

        Evaluator {
            expected,
            outcomes,
            bar_starts,
            windows,
            mode,
            speed: speed.max(0.05),
            wrong: Vec::new(),
            cursor: 0,
        }
    }

    pub fn expected(&self) -> &[Expected] {
        &self.expected
    }

    pub fn outcomes(&self) -> &[Outcome] {
        &self.outcomes
    }

    pub fn mode(&self) -> PracticeMode {
        self.mode
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Fin de la última nota, incluidas las que toca la app.
    pub fn end_time(&self) -> f64 {
        self.expected.iter().map(|e| e.end).fold(0.0, f64::max)
    }

    /// Inicio de los compases, en segundos.
    pub fn bar_starts(&self) -> &[f64] {
        &self.bar_starts
    }

    /// Tolerancia máxima en segundos de la pieza.
    fn tolerance(&self) -> f64 {
        self.windows.late * self.speed
    }

    fn first_pending(&self) -> Option<usize> {
        (self.cursor..self.outcomes.len()).find(|&i| self.outcomes[i] == Outcome::Pending)
    }

    /// En modo espera, el reloj no puede pasar de este instante: el inicio
    /// de la primera nota que falta tocar.
    pub fn wait_gate(&self) -> Option<f64> {
        if self.mode != PracticeMode::Wait {
            return None;
        }
        self.first_pending().map(|i| self.expected[i].start)
    }

    /// Notas que el usuario tiene que tocar ahora en modo espera (el acorde
    /// en el que se detuvo la música), o las próximas en modo ritmo.
    pub fn next_group(&self) -> Vec<u8> {
        let Some(first) = self.first_pending() else {
            return Vec::new();
        };
        let start = self.expected[first].start;
        (first..self.expected.len())
            .take_while(|&i| self.expected[i].start <= start + CHORD_TOLERANCE)
            .filter(|&i| self.outcomes[i] == Outcome::Pending)
            .map(|i| self.expected[i].pitch)
            .collect()
    }

    /// Registra una nota tocada en el instante `t` del reloj musical.
    pub fn on_note(&mut self, pitch: u8, t: f64) -> Judgement {
        let tol = self.tolerance();
        let gate = self.wait_gate();
        let candidate = (self.cursor..self.expected.len())
            .take_while(|&i| self.expected[i].start <= t.max(gate.unwrap_or(t)) + tol)
            .filter(|&i| self.outcomes[i] == Outcome::Pending && self.expected[i].pitch == pitch)
            .filter(|&i| {
                let start = self.expected[i].start;
                let in_window = (start - t).abs() <= tol;
                // En modo espera vale tocar el acorde en el que se detuvo la música.
                let at_gate = gate.is_some_and(|g| start <= g + CHORD_TOLERANCE);
                in_window || at_gate
            })
            .min_by(|&a, &b| {
                let da = (self.expected[a].start - t).abs();
                let db = (self.expected[b].start - t).abs();
                da.total_cmp(&db)
            });

        match candidate {
            Some(index) => {
                let (grade, offset) = if self.mode == PracticeMode::Wait {
                    // En modo espera no se evalúa el tiempo.
                    (Grade::Perfect, 0.0)
                } else {
                    let offset = (t - self.expected[index].start) / self.speed;
                    (self.windows.grade(offset), offset)
                };
                self.outcomes[index] = Outcome::Hit { grade, offset };
                self.advance_cursor();
                Judgement::Hit {
                    index,
                    grade,
                    offset,
                }
            }
            None => {
                let bar = self.bar_starts.partition_point(|&b| b <= t).max(1);
                self.wrong.push((pitch, bar));
                Judgement::Wrong { pitch }
            }
        }
    }

    /// Avanza el reloj musical hasta `t`: en modo ritmo, las notas que ya
    /// no se pueden tocar pasan a perdidas. Devuelve sus índices.
    pub fn advance(&mut self, t: f64) -> Vec<usize> {
        let mut missed = Vec::new();
        if self.mode == PracticeMode::Rhythm {
            let tol = self.tolerance();
            for i in self.cursor..self.expected.len() {
                if self.expected[i].start + tol >= t {
                    break;
                }
                if self.outcomes[i] == Outcome::Pending {
                    self.outcomes[i] = Outcome::Missed;
                    missed.push(i);
                }
            }
        }
        self.advance_cursor();
        missed
    }

    fn advance_cursor(&mut self) {
        while self.cursor < self.outcomes.len() && self.outcomes[self.cursor] != Outcome::Pending {
            self.cursor += 1;
        }
    }

    /// `true` cuando ya no queda ninguna nota por tocar.
    pub fn is_complete(&self) -> bool {
        self.first_pending().is_none()
    }

    pub fn summary(&self) -> Summary {
        let mut s = Summary {
            total: 0,
            perfect: 0,
            good: 0,
            off_time: 0,
            missed: 0,
            wrong: self.wrong.len(),
            accuracy: None,
            mean_offset_ms: None,
            stars: 0,
            weakest_bars: Vec::new(),
        };
        let mut points = 0.0;
        let mut offsets = Vec::new();
        let mut faults_by_bar: std::collections::BTreeMap<usize, usize> = Default::default();

        for (e, outcome) in self.expected.iter().zip(&self.outcomes) {
            match *outcome {
                Outcome::Ignored => continue,
                Outcome::Pending | Outcome::Missed => {
                    s.missed += 1;
                    *faults_by_bar.entry(e.bar).or_default() += 1;
                }
                Outcome::Hit { grade, offset } => {
                    points += grade.points();
                    match grade {
                        Grade::Perfect => s.perfect += 1,
                        Grade::Good => s.good += 1,
                        Grade::OffTime => {
                            s.off_time += 1;
                            *faults_by_bar.entry(e.bar).or_default() += 1;
                        }
                    }
                    if self.mode == PracticeMode::Rhythm {
                        offsets.push(offset);
                    }
                }
            }
            s.total += 1;
        }
        for &(_, bar) in &self.wrong {
            *faults_by_bar.entry(bar).or_default() += 1;
        }

        if s.total > 0 {
            let accuracy =
                (points / s.total as f64 - WRONG_NOTE_PENALTY * s.wrong as f64).clamp(0.0, 1.0);
            s.accuracy = Some(accuracy);
            if self.mode == PracticeMode::Rhythm && self.speed >= 1.0 {
                s.stars = STAR_THRESHOLDS.iter().filter(|&&t| accuracy >= t).count() as u8;
            }
        }
        if !offsets.is_empty() {
            s.mean_offset_ms = Some(offsets.iter().sum::<f64>() / offsets.len() as f64 * 1000.0);
        }

        let mut bars: Vec<(usize, usize)> = faults_by_bar.into_iter().collect();
        bars.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        s.weakest_bars = bars.into_iter().take(3).map(|(bar, _)| bar).collect();
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clavium_core::notation::parse_piece;

    /// A 60 BPM una negra dura exactamente un segundo.
    fn piece(right: &str, left: &str) -> Piece {
        parse_piece(&format!(
            "title: Prueba\ntempo: 60\ntime: 4/4\nright: {right}\nleft: {left}"
        ))
        .unwrap()
    }

    fn scale() -> Piece {
        piece("C4 D4 E4 F4 | G4:4", "C3:4 | G2:4")
    }

    fn rhythm(p: &Piece, hands: HandFilter) -> Evaluator {
        Evaluator::new(p, hands, PracticeMode::Rhythm, Windows::default(), 1.0)
    }

    #[test]
    fn a_perfect_run_gets_three_stars() {
        let p = scale();
        let mut ev = rhythm(&p, HandFilter::Both);
        for (pitch, t) in [
            (60, 0.0),
            (48, 0.0),
            (62, 1.0),
            (64, 2.0),
            (65, 3.0),
            (67, 4.0),
            (43, 4.0),
        ] {
            assert!(matches!(
                ev.on_note(pitch, t),
                Judgement::Hit {
                    grade: Grade::Perfect,
                    ..
                }
            ));
        }
        ev.advance(10.0);
        let s = ev.summary();
        assert_eq!((s.total, s.perfect, s.missed, s.wrong), (7, 7, 0, 0));
        assert_eq!(s.accuracy, Some(1.0));
        assert_eq!(s.stars, 3);
        assert!(ev.is_complete());
    }

    #[test]
    fn timing_is_graded_by_window() {
        let p = scale();
        let mut ev = rhythm(&p, HandFilter::Right);
        let grade = |j| match j {
            Judgement::Hit { grade, .. } => Some(grade),
            Judgement::Wrong { .. } => None,
        };
        assert_eq!(grade(ev.on_note(60, 0.03)), Some(Grade::Perfect));
        assert_eq!(grade(ev.on_note(62, 0.9)), Some(Grade::Good));
        assert_eq!(grade(ev.on_note(64, 2.2)), Some(Grade::OffTime));
        // Fuera de la ventana máxima ya no es esa nota.
        assert_eq!(grade(ev.on_note(65, 3.3)), None);
        ev.advance(10.0);
        let s = ev.summary();
        assert_eq!(
            (s.perfect, s.good, s.off_time, s.missed, s.wrong),
            (1, 1, 1, 2, 1)
        );
        assert_eq!(s.weakest_bars.first(), Some(&1));
    }

    #[test]
    fn notes_pass_to_missed_after_the_window() {
        let p = scale();
        let mut ev = rhythm(&p, HandFilter::Right);
        assert!(ev.advance(0.2).is_empty());
        assert_eq!(ev.advance(0.3), vec![1]); // índice de Do4 en orden de inicio
        assert_eq!(ev.outcomes()[1], Outcome::Missed);
    }

    #[test]
    fn each_note_of_a_chord_counts_once() {
        let p = piece("C4+E4+G4:4", "R:4");
        let mut ev = rhythm(&p, HandFilter::Both);
        assert!(matches!(ev.on_note(64, 0.01), Judgement::Hit { .. }));
        assert!(matches!(ev.on_note(64, 0.02), Judgement::Wrong { .. }));
        assert!(matches!(ev.on_note(60, 0.0), Judgement::Hit { .. }));
        assert!(matches!(ev.on_note(67, 0.04), Judgement::Hit { .. }));
        assert!(ev.is_complete());
    }

    #[test]
    fn repeated_notes_are_matched_one_to_one() {
        let p = piece("C4:0.25 C4 C4 C4 R:3", "R:4");
        let mut ev = rhythm(&p, HandFilter::Right);
        let hits: Vec<usize> = [0.0, 0.26, 0.5, 0.74]
            .into_iter()
            .filter_map(|t| match ev.on_note(60, t) {
                Judgement::Hit { index, .. } => Some(index),
                Judgement::Wrong { .. } => None,
            })
            .collect();
        assert_eq!(hits.len(), 4);
        let mut sorted = hits.clone();
        sorted.dedup();
        assert_eq!(sorted.len(), 4);
    }

    #[test]
    fn the_other_hand_is_ignored() {
        let p = scale();
        let ev = rhythm(&p, HandFilter::Left);
        let ignored = ev
            .outcomes()
            .iter()
            .filter(|o| **o == Outcome::Ignored)
            .count();
        assert_eq!(ignored, 5);
        assert_eq!(ev.summary().total, 2);
    }

    #[test]
    fn windows_are_measured_in_real_time_at_slower_speeds() {
        let p = scale();
        let mut ev = Evaluator::new(
            &p,
            HandFilter::Right,
            PracticeMode::Rhythm,
            Windows::default(),
            0.5,
        );
        // 0,04 s de la pieza a la mitad de velocidad son 0,08 s reales.
        match ev.on_note(60, 0.04) {
            Judgement::Hit { grade, offset, .. } => {
                assert_eq!(grade, Grade::Good);
                assert!((offset - 0.08).abs() < 1e-9);
            }
            j => panic!("{j:?}"),
        }
        ev.advance(10.0);
        assert_eq!(
            ev.summary().stars,
            0,
            "sin estrellas por debajo del tempo original"
        );
    }

    #[test]
    fn wait_mode_stops_at_each_group_until_it_is_played() {
        let p = piece("C4 D4 E4+G4:2", "R:4");
        let mut ev = Evaluator::new(
            &p,
            HandFilter::Right,
            PracticeMode::Wait,
            Windows::default(),
            1.0,
        );
        assert_eq!(ev.wait_gate(), Some(0.0));
        assert!(
            ev.advance(5.0).is_empty(),
            "en modo espera no se pierden notas"
        );

        assert!(matches!(ev.on_note(61, 0.0), Judgement::Wrong { .. }));
        assert_eq!(ev.wait_gate(), Some(0.0));
        assert!(matches!(ev.on_note(60, 0.0), Judgement::Hit { .. }));
        assert_eq!(ev.wait_gate(), Some(1.0));
        assert!(matches!(ev.on_note(62, 1.0), Judgement::Hit { .. }));

        assert_eq!(ev.next_group(), vec![64, 67]);
        ev.on_note(67, 2.0);
        assert_eq!(ev.wait_gate(), Some(2.0));
        ev.on_note(64, 2.0);
        assert_eq!(ev.wait_gate(), None);
        let s = ev.summary();
        assert_eq!((s.perfect, s.wrong, s.stars), (4, 1, 0));
        assert!((s.accuracy.unwrap() - 0.98).abs() < 1e-9);
    }

    #[test]
    fn listen_mode_evaluates_nothing() {
        let p = scale();
        let ev = Evaluator::new(
            &p,
            HandFilter::Both,
            PracticeMode::Listen,
            Windows::default(),
            1.0,
        );
        assert!(ev.is_complete());
        assert_eq!(ev.summary().accuracy, None);
        assert!((ev.end_time() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn mean_offset_shows_a_tendency_to_rush() {
        let p = scale();
        let mut ev = rhythm(&p, HandFilter::Right);
        for (pitch, t) in [(60, -0.04), (62, 0.96), (64, 1.96), (65, 2.96), (67, 3.96)] {
            ev.on_note(pitch, t);
        }
        let s = ev.summary();
        assert!((s.mean_offset_ms.unwrap() + 40.0).abs() < 1e-6);
    }
}
