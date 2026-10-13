//! Biblioteca: las piezas que vienen con la app (agrupadas en rutas de
//! aprendizaje) y, si está instalado, el catálogo ampliado por nivel.

use std::path::{Path, PathBuf};

use clavium_core::notation::parse_piece;
use clavium_core::{Arrangement, Hand, Piece, clav};

use crate::catalog;

mod bundled {
    include!(concat!(env!("OUT_DIR"), "/bundled.rs"));
}

pub struct LibraryEntry {
    pub id: String,
    pub title: String,
    pub composer: Option<String>,
    pub level: u8,
    pub duration: f64,
    pub arrangement: Arrangement,
    /// Un arreglo o una transcripción es borrador hasta que alguien lo revisa tocándolo.
    pub draft: bool,
    /// Crédito y licencia, para las obras del catálogo.
    pub credit: Option<String>,
    /// La pieza se lee recién cuando hace falta: el catálogo puede tener cientos.
    piece: Option<Piece>,
    path: Option<PathBuf>,
}

impl LibraryEntry {
    fn from_piece(id: String, piece: Piece) -> Self {
        let meta = &piece.metadata;
        LibraryEntry {
            id,
            title: meta.title.clone(),
            composer: meta.composer.clone(),
            level: meta.difficulty.unwrap_or(1),
            duration: piece.duration_seconds(),
            arrangement: meta.arrangement,
            draft: piece
                .provenance
                .as_ref()
                .is_none_or(|p| p.reviewers.is_empty()),
            credit: None,
            piece: Some(piece),
            path: None,
        }
    }

    /// La pieza, leyéndola del disco la primera vez.
    pub fn piece(&mut self) -> Result<&Piece, String> {
        if self.piece.is_none() {
            let path = self.path.as_ref().ok_or("la pieza no tiene archivo")?;
            let piece = clav::load(path).map_err(|e| e.to_string())?;
            self.draft = piece
                .provenance
                .as_ref()
                .is_none_or(|p| p.reviewers.is_empty());
            self.piece = Some(piece);
        }
        Ok(self.piece.as_ref().expect("recién cargada"))
    }

    /// La pieza si ya se leyó.
    pub fn loaded(&self) -> Option<&Piece> {
        self.piece.as_ref()
    }

    /// Nota más grave y más aguda, si la pieza ya se leyó.
    pub fn range(&self) -> Option<(u8, u8)> {
        let notes = &self.piece.as_ref()?.notes;
        Some((
            notes.iter().map(|n| n.pitch).min()?,
            notes.iter().map(|n| n.pitch).max()?,
        ))
    }

    pub fn subtitle(&self) -> String {
        let mut parts = Vec::new();
        if let Some(composer) = &self.composer {
            parts.push(composer.clone());
        }
        parts.push(format!("Nivel {}", self.level));
        let secs = self.duration.round() as u32;
        parts.push(format!("{}:{:02}", secs / 60, secs % 60));
        parts.push(
            match self.arrangement {
                Arrangement::Original => "Obra completa",
                Arrangement::Intermediate => "Versión intermedia",
                Arrangement::Simplified => "Versión simplificada",
            }
            .to_string(),
        );
        let one_hand = self.piece.as_ref().is_some_and(|p| {
            !(p.notes.iter().any(|n| n.hand == Hand::Left)
                && p.notes.iter().any(|n| n.hand == Hand::Right))
        });
        if one_hand {
            parts.push("Una mano".to_string());
        }
        parts.join(" · ")
    }

    /// Si la pieza coincide con una búsqueda por título o compositor.
    pub fn matches(&self, query: &str) -> bool {
        let query = fold(query);
        if query.is_empty() {
            return true;
        }
        let haystack = fold(&format!(
            "{} {}",
            self.title,
            self.composer.as_deref().unwrap_or("")
        ));
        query.split_whitespace().all(|word| haystack.contains(word))
    }
}

/// Minúsculas y sin tildes, para buscar "chopin" o "fur elise" sin cuidar la escritura.
fn fold(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

/// Una ruta de aprendizaje, o un grupo del catálogo.
pub struct Group {
    pub name: String,
    /// Índices en `Library::entries`, en el orden en que se muestran.
    pub entries: Vec<usize>,
    /// Las rutas cuentan cuántas piezas se dominaron; el catálogo, no.
    pub is_route: bool,
}

pub struct Library {
    pub entries: Vec<LibraryEntry>,
    pub groups: Vec<Group>,
    /// Obras del catálogo ampliado instalado, si hay.
    pub catalog_size: usize,
}

impl Library {
    pub fn load(catalog_dir: &Path) -> Self {
        let mut entries: Vec<LibraryEntry> = bundled::BUNDLED
            .iter()
            .map(|(id, text)| {
                let piece = parse_piece(text)
                    .unwrap_or_else(|e| panic!("la pieza incluida `{id}` es inválida: {e}"));
                LibraryEntry::from_piece(id.to_string(), piece)
            })
            .collect();
        let mut groups = group_by_routes(&entries, bundled::ROUTES);

        let mut catalog_size = 0;
        if let Some(index) = catalog::load_index(catalog_dir) {
            let first = entries.len();
            for item in index.pieces {
                entries.push(LibraryEntry {
                    credit: Some(item.credit()),
                    id: item.id,
                    title: item.title,
                    composer: item.composer,
                    level: item.level,
                    duration: item.duration_seconds,
                    arrangement: Arrangement::Original,
                    draft: true,
                    piece: None,
                    path: Some(catalog_dir.join(item.file)),
                });
            }
            catalog_size = entries.len() - first;
            for level in 1..=8u8 {
                let mut members: Vec<usize> = (first..entries.len())
                    .filter(|&i| entries[i].level == level)
                    .collect();
                members.sort_by(|&a, &b| {
                    (entries[a].composer.as_deref(), &entries[a].title)
                        .cmp(&(entries[b].composer.as_deref(), &entries[b].title))
                });
                if !members.is_empty() {
                    groups.push(Group {
                        name: format!("Obras completas · Nivel {level}"),
                        entries: members,
                        is_route: false,
                    });
                }
            }
        }

        Library {
            entries,
            groups,
            catalog_size,
        }
    }
}

/// Lee `rutas.txt` y arma los grupos. Las piezas incluidas que no están en
/// ninguna ruta van al final, por nivel y título.
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
                is_route: true,
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
    rest.sort_by_key(|&i| (entries[i].level, entries[i].title.clone()));
    if !rest.is_empty() {
        groups.push(Group {
            name: "Otras piezas".to_string(),
            entries: rest,
            is_route: false,
        });
    }
    groups.retain(|g| !g.entries.is_empty());
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_catalog() -> Library {
        Library::load(Path::new("no-existe"))
    }

    #[test]
    fn every_bundled_piece_is_shown_once() {
        let library = no_catalog();
        assert!(library.entries.len() >= 3);
        assert_eq!(library.catalog_size, 0);
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
        let library = no_catalog();
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

    #[test]
    fn search_ignores_case_and_accents() {
        let library = no_catalog();
        let oda = library
            .entries
            .iter()
            .find(|e| e.id == "oda-a-la-alegria")
            .unwrap();
        assert!(oda.matches("ODA alegria"));
        assert!(oda.matches("beethoven"));
        assert!(!oda.matches("chopin"));
        assert!(oda.matches(""));
    }
}
