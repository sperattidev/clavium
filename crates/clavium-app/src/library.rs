//! Biblioteca inicial que viene con la app.

use clavium_core::notation::parse_piece;
use clavium_core::{Arrangement, Hand, Piece};

/// Piezas incluidas en el binario, en el orden en que se muestran.
const BUNDLED: &[(&str, &str)] = &[
    (
        "estrellita",
        include_str!("../../../library/estrellita.txt"),
    ),
    (
        "oda-a-la-alegria",
        include_str!("../../../library/oda-a-la-alegria.txt"),
    ),
    (
        "minueto-en-sol",
        include_str!("../../../library/minueto-en-sol.txt"),
    ),
];

pub struct LibraryEntry {
    pub id: &'static str,
    pub piece: Piece,
    pub duration: f64,
    pub lowest: u8,
    pub highest: u8,
    pub two_hands: bool,
}

impl LibraryEntry {
    pub fn subtitle(&self) -> String {
        let meta = &self.piece.metadata;
        let mut parts = Vec::new();
        if let Some(composer) = &meta.composer {
            parts.push(composer.clone());
        }
        if let Some(level) = meta.difficulty {
            parts.push(format!("Nivel {level}"));
        }
        let secs = self.duration.round() as u32;
        parts.push(format!("{}:{:02}", secs / 60, secs % 60));
        parts.push(
            match meta.arrangement {
                Arrangement::Original => "Original",
                Arrangement::Intermediate => "Versión intermedia",
                Arrangement::Simplified => "Versión simplificada",
            }
            .to_string(),
        );
        if !self.two_hands {
            parts.push("Una mano".to_string());
        }
        parts.join(" · ")
    }
}

pub struct Library {
    pub entries: Vec<LibraryEntry>,
}

impl Library {
    pub fn bundled() -> Self {
        let entries = BUNDLED
            .iter()
            .map(|(id, text)| {
                let piece = parse_piece(text)
                    .unwrap_or_else(|e| panic!("la pieza incluida `{id}` es inválida: {e}"));
                let lowest = piece.notes.iter().map(|n| n.pitch).min().unwrap_or(60);
                let highest = piece.notes.iter().map(|n| n.pitch).max().unwrap_or(60);
                let two_hands = piece.notes.iter().any(|n| n.hand == Hand::Left)
                    && piece.notes.iter().any(|n| n.hand == Hand::Right);
                LibraryEntry {
                    id,
                    duration: piece.duration_seconds(),
                    lowest,
                    highest,
                    two_hands,
                    piece,
                }
            })
            .collect();
        Library { entries }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_library_loads() {
        let library = Library::bundled();
        assert_eq!(library.entries.len(), 3);
        assert!(library.entries.iter().all(|e| e.duration > 5.0));
    }
}
