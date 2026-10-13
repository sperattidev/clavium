//! Estimación de la dificultad de una pieza a partir de sus notas.
//!
//! Es una aproximación para ordenar una biblioteca grande sin escuchar cada
//! obra: mide qué tan rápido hay que tocar, qué tan grandes y abiertos son
//! los acordes, los saltos, cuántas notas suenan a la vez y cuánto dura.
//! Se puede corregir a mano en el catálogo.

use crate::model::{Hand, Piece};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Features {
    /// Notas por segundo, contando las dos manos, a tempo original.
    pub notes_per_second: f64,
    /// Tamaño de acorde que se supera solo en el 10 % de los ataques.
    pub chord_p90: usize,
    /// Apertura de la mano (semitonos entre la nota más grave y la más aguda
    /// de un acorde) que se supera solo en el 10 % de los acordes.
    pub span_p90: u8,
    /// Salto medio entre ataques consecutivos de una mano, en semitonos.
    pub mean_jump: f64,
    /// Notas que suenan a la vez, en promedio, en cada ataque.
    pub polyphony: f64,
    pub minutes: f64,
    /// Notas por segundo en los pasajes más rápidos: la densidad que se supera
    /// solo en el 10 % de las ventanas de 4 segundos.
    pub peak_notes_per_second: f64,
}

/// Duración de las ventanas para medir los pasajes más rápidos, en segundos.
const PEAK_WINDOW: f64 = 4.0;

fn percentile<T: Copy + Ord + Default>(mut values: Vec<T>, p: f64) -> T {
    values.sort_unstable();
    values
        .get((values.len() as f64 * p) as usize)
        .or(values.last())
        .copied()
        .unwrap_or_default()
}

pub fn features(piece: &Piece) -> Features {
    let notes = &piece.notes;
    let start = notes.iter().map(|n| n.start_tick).min().unwrap_or(0);
    let end = notes.iter().map(|n| n.end_tick).max().unwrap_or(0);
    let seconds = (piece.tempo.tick_to_seconds(end) - piece.tempo.tick_to_seconds(start)).max(1.0);

    let mut chords = Vec::new();
    let mut spans = Vec::new();
    let mut jumps = Vec::new();
    for hand in [Hand::Right, Hand::Left, Hand::Unknown] {
        let mut onsets: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut hand_notes: Vec<_> = notes.iter().filter(|n| n.hand == hand).collect();
        hand_notes.sort_by_key(|n| (n.start_tick, n.pitch));
        for n in hand_notes {
            match onsets.last_mut() {
                Some((tick, pitches)) if *tick == n.start_tick => pitches.push(n.pitch),
                _ => onsets.push((n.start_tick, vec![n.pitch])),
            }
        }
        chords.extend(onsets.iter().map(|(_, p)| p.len()));
        spans.extend(
            onsets
                .iter()
                .filter(|(_, p)| p.len() > 1)
                .map(|(_, p)| p.iter().max().unwrap() - p.iter().min().unwrap()),
        );
        // La derecha se guía por la nota más aguda; la izquierda, por la más grave.
        let guide = |p: &Vec<u8>| -> u8 {
            if hand == Hand::Left {
                *p.iter().min().unwrap()
            } else {
                *p.iter().max().unwrap()
            }
        };
        jumps.extend(onsets.windows(2).map(|w| {
            (i16::from(guide(&w[1].1)) - i16::from(guide(&w[0].1))).unsigned_abs() as f64
        }));
    }

    // Notas sonando en cada ataque: las que empezaron antes y todavía no terminaron.
    let mut by_start: Vec<_> = notes.iter().collect();
    by_start.sort_by_key(|n| n.start_tick);
    let mut onset_ticks: Vec<u64> = by_start.iter().map(|n| n.start_tick).collect();
    onset_ticks.dedup();
    let mut sounding_total = 0usize;
    for &tick in &onset_ticks {
        let first_after = by_start.partition_point(|n| n.start_tick <= tick);
        sounding_total += by_start[..first_after]
            .iter()
            .filter(|n| n.end_tick > tick)
            .count();
    }

    // Densidad de los pasajes más rápidos, en ventanas de 4 s que avanzan de a 1 s.
    let mut onset_seconds: Vec<f64> = notes
        .iter()
        .map(|n| piece.tempo.tick_to_seconds(n.start_tick))
        .collect();
    onset_seconds.sort_by(f64::total_cmp);
    let first = onset_seconds.first().copied().unwrap_or(0.0);
    let last = onset_seconds.last().copied().unwrap_or(0.0);
    let mut window_counts = Vec::new();
    let mut t = first;
    while t <= last {
        let from = onset_seconds.partition_point(|&s| s < t);
        let to = onset_seconds.partition_point(|&s| s < t + PEAK_WINDOW);
        window_counts.push(to - from);
        t += 1.0;
    }
    let peak_notes_per_second = percentile(window_counts, 0.9) as f64 / PEAK_WINDOW;

    Features {
        notes_per_second: notes.len() as f64 / seconds,
        chord_p90: percentile(chords, 0.9),
        span_p90: percentile(spans, 0.9),
        mean_jump: if jumps.is_empty() {
            0.0
        } else {
            jumps.iter().sum::<f64>() / jumps.len() as f64
        },
        polyphony: if onset_ticks.is_empty() {
            0.0
        } else {
            sounding_total as f64 / onset_ticks.len() as f64
        },
        minutes: seconds / 60.0,
        peak_notes_per_second,
    }
}

/// Puntaje de dificultad: más alto, más difícil. Sirve para ordenar.
pub fn difficulty_score(f: &Features) -> f64 {
    1.2 * (1.0 + f.notes_per_second).ln()
        + 1.4 * (1.0 + f.peak_notes_per_second).ln()
        + 0.35 * (f.chord_p90.max(1) - 1) as f64
        + 0.08 * f.mean_jump
        + 0.06 * f64::from(f.span_p90)
        + 0.15 * f.polyphony
        + 0.5 * (1.0 + f.minutes).ln()
}

/// Puntajes que separan los niveles 1 a 8 para una pieza suelta.
const LEVEL_THRESHOLDS: [f64; 7] = [6.2, 7.0, 7.5, 7.9, 8.3, 8.9, 9.6];

/// Nivel de dificultad estimado, del 1 al 8, para una pieza suelta.
pub fn estimate_level(piece: &Piece) -> u8 {
    let score = difficulty_score(&features(piece));
    1 + LEVEL_THRESHOLDS.iter().filter(|&&t| score >= t).count() as u8
}

/// Proporción de obras de cada nivel al repartir un catálogo por posición:
/// pocas en los extremos y la mayoría en el medio.
pub const CATALOG_LEVEL_SHARE: [f64; 8] = [0.08, 0.12, 0.15, 0.16, 0.16, 0.14, 0.11, 0.08];

/// Niveles para un catálogo completo: se ordenan las obras por puntaje y se
/// reparten según `CATALOG_LEVEL_SHARE`, así hay obras en todos los niveles
/// aunque la escala absoluta del puntaje no sea exacta.
pub fn levels_by_rank(scores: &[f64]) -> Vec<u8> {
    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|&a, &b| scores[a].total_cmp(&scores[b]));
    let mut levels = vec![1u8; scores.len()];
    let n = scores.len() as f64;
    for (rank, &i) in order.iter().enumerate() {
        let position = (rank as f64 + 0.5) / n;
        let mut acc = 0.0;
        let mut level = 8;
        for (k, share) in CATALOG_LEVEL_SHARE.iter().enumerate() {
            acc += share;
            if position < acc {
                level = k as u8 + 1;
                break;
            }
        }
        levels[i] = level;
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notation::parse_piece;

    #[test]
    fn a_slow_melody_is_level_one() {
        let piece =
            parse_piece("title: x\ntempo: 60\nright: C4 D4 E4 F4 | G4:4\nleft: C3:4 | G2:4")
                .unwrap();
        let f = features(&piece);
        assert!((f.notes_per_second - 7.0 / 8.0).abs() < 1e-9);
        assert_eq!(f.chord_p90, 1);
        assert_eq!(estimate_level(&piece), 1);
    }

    #[test]
    fn fast_chords_and_leaps_are_hard() {
        let right = "C4+E4+G4+C5:0.25 C6+E6+G6+C7 C4+E4+G4+C5 C6+E6+G6+C7 ".repeat(4);
        let left = "C2:0.25 C4 C2 C4 ".repeat(4);
        let piece = parse_piece(&format!(
            "title: x\ntempo: 120\nright: {right}\nleft: {left}"
        ))
        .unwrap();
        let f = features(&piece);
        assert!(f.notes_per_second > 30.0);
        assert_eq!(f.chord_p90, 4);
        assert!(f.mean_jump > 7.0);
        assert_eq!(estimate_level(&piece), 8);
    }

    #[test]
    fn a_catalog_is_spread_over_all_levels() {
        let scores: Vec<f64> = (0..100).map(|i| f64::from(i) * 0.01 + 5.0).collect();
        let levels = levels_by_rank(&scores);
        let count = |l: u8| levels.iter().filter(|&&x| x == l).count();
        assert_eq!(
            (1..=8).map(count).collect::<Vec<_>>(),
            vec![8, 12, 15, 16, 16, 14, 11, 8]
        );
        assert_eq!(levels[0], 1);
        assert_eq!(levels[99], 8);
    }
}
