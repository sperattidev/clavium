//! Estimación de la dificultad de una pieza a partir de sus notas.
//!
//! Es una aproximación para ordenar una biblioteca grande sin escuchar cada
//! obra: mide qué tan rápido hay que tocar, qué tan grandes son los acordes y
//! los saltos. Se puede corregir a mano en el catálogo.

use crate::model::{Hand, Piece};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Features {
    /// Notas por segundo, contando las dos manos, a tempo original.
    pub notes_per_second: f64,
    /// Tamaño de acorde que se supera solo en el 10 % de los ataques.
    pub chord_p90: usize,
    /// Salto medio entre ataques consecutivos de una mano, en semitonos.
    pub mean_jump: f64,
}

pub fn features(piece: &Piece) -> Features {
    let notes = &piece.notes;
    let start = notes.iter().map(|n| n.start_tick).min().unwrap_or(0);
    let end = notes.iter().map(|n| n.end_tick).max().unwrap_or(0);
    let seconds = (piece.tempo.tick_to_seconds(end) - piece.tempo.tick_to_seconds(start)).max(1.0);

    let mut chords = Vec::new();
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
    chords.sort_unstable();
    let chord_p90 = chords
        .get((chords.len() as f64 * 0.9) as usize)
        .or(chords.last())
        .copied()
        .unwrap_or(0);
    let mean_jump = if jumps.is_empty() {
        0.0
    } else {
        jumps.iter().sum::<f64>() / jumps.len() as f64
    };

    Features {
        notes_per_second: notes.len() as f64 / seconds,
        chord_p90,
        mean_jump,
    }
}

/// Nivel de dificultad estimado, del 1 al 8.
pub fn estimate_level(piece: &Piece) -> u8 {
    let f = features(piece);
    let base: u8 = match f.notes_per_second {
        x if x < 1.5 => 1,
        x if x < 2.5 => 2,
        x if x < 3.5 => 3,
        x if x < 5.0 => 4,
        x if x < 7.0 => 5,
        x if x < 9.5 => 6,
        x if x < 13.0 => 7,
        _ => 8,
    };
    let mut level = base;
    if f.chord_p90 >= 4 {
        level += 1;
    }
    if f.mean_jump > 7.0 {
        level += 1;
    }
    level.clamp(1, 8)
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
}
