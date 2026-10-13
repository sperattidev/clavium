//! Biblioteca que viene con la app: todas las piezas de `library/`,
//! agrupadas en rutas de aprendizaje.

use clavium_core::notation::parse_piece;
use clavium_core::{Arrangement, Hand, Piece};

mod bundled {
    include!(concat!(env!("OUT_DIR"), "/bundled.rs"));
}

pub struct LibraryEntry {
    pub id: &'static str,
    pub piece: Piece,
    pub duration: f64,
    pub lowest: u8,
    pub highest: u8,
    pub two_hands: bool,
}

impl LibraryEntry {
    /// Un arreglo es borrador hasta que alguien lo revisa tocándolo.
    pub fn is_draft(&self) -> bool {
        self.piece
            .provenance
            .as_ref()
            .is_none_or(|p| p.reviewers.is_empty())
    }

    pub fn level(&self) -> u8 {
        self.piece.metadata.difficulty.unwrap_or(1)
    }

    pub fn subtitle(&self) -> String {
        let meta = &self.piece.metadata;
        let mut parts = Vec::new();
        if let Some(composer) = &meta.composer {
            parts.push(composer.clone());
        }
        parts.push(format!("Nivel {}", self.level()));
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

/// Una ruta de aprendizaje, o el grupo de piezas que no están en ninguna.
pub struct Group {
    pub name: String,
    /// Índices en `Library::entries`, en el orden recomendado.
    pub entries: Vec<usize>,
}

pub struct Library {
    pub entries: Vec<LibraryEntry>,
    pub groups: Vec<Group>,
}

impl Library {
    pub fn bundled() -> Self {
        let entries: Vec<LibraryEntry> = bundled::BUNDLED
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
        let groups = group_by_routes(&entries, bundled::ROUTES);
        Library { entries, groups }
    }
}

/// Lee `rutas.txt` y arma los grupos. Las piezas que no están en ninguna
/// ruta van al final, por nivel y título.
fn group_by_routes(entries: &[LibraryEntry], routes: &str) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut placed = vec![false; entries.len()];
    for line in routes.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix("ruta:") {
            groups.push(Group {
                name: name.trim().to_string(),
                entries: Vec::new(),
            });
        } else if let Some(id) = line.strip_prefix('-')
            && let Some(group) = groups.last_mut()
            && let Some(i) = entries.iter().position(|e| e.id == id.trim())
            && !placed[i]
        {
            placed[i] = true;
            group.entries.push(i);
        }
    }
    let mut rest: Vec<usize> = (0..entries.len()).filter(|&i| !placed[i]).collect();
    rest.sort_by_key(|&i| (entries[i].level(), entries[i].piece.metadata.title.clone()));
    if !rest.is_empty() {
        groups.push(Group {
            name: "Otras piezas".to_string(),
            entries: rest,
        });
    }
    groups.retain(|g| !g.entries.is_empty());
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_piece_is_shown_once() {
        let library = Library::bundled();
        assert!(library.entries.len() >= 3);
        assert!(library.entries.iter().all(|e| e.duration > 5.0));
        let mut shown: Vec<usize> = library
            .groups
            .iter()
            .flat_map(|g| g.entries.clone())
            .collect();
        shown.sort();
        assert_eq!(shown, (0..library.entries.len()).collect::<Vec<_>>());
    }

    #[test]
    fn every_route_entry_names_an_existing_piece() {
        let library = Library::bundled();
        for line in bundled::ROUTES.lines().map(str::trim) {
            if let Some(id) = line.strip_prefix('-') {
                assert!(
                    library.entries.iter().any(|e| e.id == id.trim()),
                    "rutas.txt nombra `{}`, que no existe en library/",
                    id.trim()
                );
            }
        }
    }
}
