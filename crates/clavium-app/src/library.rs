//! Biblioteca: las piezas que vienen con la app (agrupadas en rutas de
//! aprendizaje) y, si está instalado, el catálogo ampliado por nivel.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clavium_core::notation::parse_piece;
use clavium_core::{Arrangement, Hand, Piece, clav};

use crate::{catalog, mine};

mod bundled {
    include!(concat!(env!("OUT_DIR"), "/bundled.rs"));
}

/// Nombre del grupo de piezas importadas por el usuario.
pub const MY_PIECES: &str = "Mis piezas";

pub struct LibraryEntry {
    pub id: String,
    pub title: String,
    pub composer: Option<String>,
    pub level: u8,
    pub duration: f64,
    pub arrangement: Arrangement,
    /// Un arreglo o una transcripción es borrador hasta que alguien lo revisa tocándolo.
    pub draft: bool,
    /// Revisada por el usuario de esta app (se guarda en la base local).
    pub reviewed_here: bool,
    /// Importada por el usuario ("Mis piezas").
    pub mine: bool,
    pub style: Option<String>,
    /// Clave estable del compositor, para agrupar.
    pub composer_key: Option<String>,
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
            reviewed_here: false,
            mine: false,
            style: meta.style.clone(),
            composer_key: None,
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

    /// Sin revisar: ni la fuente ni este usuario confirmaron las notas tocándola.
    pub fn needs_review(&self) -> bool {
        self.draft && !self.reviewed_here
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
    /// Rutas de aprendizaje de las piezas incluidas.
    pub groups: Vec<Group>,
    /// Obras del catálogo ampliado instalado, si hay.
    pub catalog_size: usize,
    /// Índice de la primera obra del catálogo en `entries`.
    catalog_start: usize,
}

impl Library {
    pub fn load(catalog_dir: &Path, mine_dir: &Path) -> Self {
        let mut entries: Vec<LibraryEntry> = bundled::BUNDLED
            .iter()
            .map(|(id, text)| {
                let piece = parse_piece(text)
                    .unwrap_or_else(|e| panic!("la pieza incluida `{id}` es inválida: {e}"));
                LibraryEntry::from_piece(id.to_string(), piece)
            })
            .collect();
        let mut groups = group_by_routes(&entries, bundled::ROUTES);

        // Mis piezas: las que importó el usuario, primero.
        let mine_start = entries.len();
        for (id, path) in mine::list(mine_dir) {
            if let Ok(piece) = clav::load(&path) {
                let mut entry = LibraryEntry::from_piece(id, piece);
                entry.mine = true;
                // Son del usuario: no tiene sentido marcarlas como sin revisar.
                entry.draft = false;
                entry.credit =
                    Some("Importada por vos. Solo está guardada en esta computadora.".into());
                entry.path = Some(path);
                entries.push(entry);
            }
        }
        if entries.len() > mine_start {
            groups.insert(
                0,
                Group {
                    name: MY_PIECES.to_string(),
                    entries: (mine_start..entries.len()).collect(),
                    is_route: false,
                },
            );
        }

        let catalog_start = entries.len();
        if let Some(index) = catalog::load_index(catalog_dir) {
            for item in index.pieces {
                entries.push(LibraryEntry {
                    credit: Some(item.credit()),
                    id: item.id,
                    title: item.title,
                    composer: item.composer,
                    level: item.level,
                    duration: item.duration_seconds,
                    arrangement: Arrangement::Original,
                    draft: !item.reviewed,
                    reviewed_here: false,
                    mine: false,
                    style: item.style,
                    composer_key: item.composer_key,
                    piece: None,
                    path: Some(catalog_dir.join(item.file)),
                });
            }
        }

        Library {
            catalog_size: entries.len() - catalog_start,
            catalog_start,
            entries,
            groups,
        }
    }

    /// Grupos para mostrar: primero las rutas de las piezas incluidas, después
    /// el catálogo agrupado como se pidió. Todo pasa por el filtro; los grupos
    /// vacíos no se muestran.
    pub fn browse(&self, by: GroupBy, filter: &Filter) -> Vec<Group> {
        let keep = |i: &usize| filter.accepts(&self.entries[*i]);
        let mut groups: Vec<Group> = self
            .groups
            .iter()
            .map(|g| Group {
                name: g.name.clone(),
                entries: g.entries.iter().copied().filter(keep).collect(),
                is_route: g.is_route,
            })
            .collect();

        let catalog: Vec<usize> = (self.catalog_start..self.entries.len())
            .filter(keep)
            .collect();
        let mut buckets: BTreeMap<(u8, String), Vec<usize>> = BTreeMap::new();
        for i in catalog {
            let e = &self.entries[i];
            let key = match by {
                GroupBy::Level => (e.level, format!("Nivel {}", e.level)),
                GroupBy::Composer => (0, self.composer_name(e)),
                GroupBy::Style => style_group(e.style.as_deref()),
            };
            buckets.entry(key).or_default().push(i);
        }
        for ((_, name), mut members) in buckets {
            members.sort_by(|&a, &b| {
                let (ea, eb) = (&self.entries[a], &self.entries[b]);
                (ea.level, ea.composer.as_deref(), &ea.title).cmp(&(
                    eb.level,
                    eb.composer.as_deref(),
                    &eb.title,
                ))
            });
            groups.push(Group {
                name,
                entries: members,
                is_route: false,
            });
        }
        groups.retain(|g| !g.entries.is_empty());
        groups
    }

    /// Nombre del compositor para agrupar: el más usado entre las obras con la
    /// misma clave, porque la fuente a veces lo escribe de formas distintas.
    fn composer_name(&self, entry: &LibraryEntry) -> String {
        let Some(key) = &entry.composer_key else {
            return entry
                .composer
                .clone()
                .unwrap_or_else(|| "Compositor sin indicar".to_string());
        };
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for e in &self.entries[self.catalog_start..] {
            if e.composer_key.as_ref() == Some(key)
                && let Some(name) = &e.composer
            {
                *counts.entry(name).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .max_by_key(|(name, count)| (*count, std::cmp::Reverse(*name)))
            .map(|(name, _)| name.to_string())
            .unwrap_or_else(|| key.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupBy {
    #[default]
    Level,
    Composer,
    Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Length {
    #[default]
    Any,
    /// Menos de 3 minutos.
    Short,
    /// De 3 a 8 minutos.
    Medium,
    /// Más de 8 minutos.
    Long,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Any,
    NotReviewed,
    Reviewed,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Filter {
    pub query: String,
    /// Nivel mínimo y máximo, inclusive.
    pub min_level: u8,
    pub max_level: u8,
    pub length: Length,
    pub status: Status,
}

impl Filter {
    pub fn accepts(&self, e: &LibraryEntry) -> bool {
        let level_ok = e.level >= self.min_level.max(1) && e.level <= self.max_level.clamp(1, 8);
        let minutes = e.duration / 60.0;
        let length_ok = match self.length {
            Length::Any => true,
            Length::Short => minutes < 3.0,
            Length::Medium => (3.0..=8.0).contains(&minutes),
            Length::Long => minutes > 8.0,
        };
        let status_ok = match self.status {
            Status::Any => true,
            Status::NotReviewed => e.needs_review(),
            Status::Reviewed => !e.needs_review(),
        };
        level_ok && length_ok && status_ok && e.matches(&self.query)
    }
}

/// Estilo de la fuente en castellano, con un orden cronológico para los grupos.
fn style_group(style: Option<&str>) -> (u8, String) {
    let s = style.unwrap_or("").to_ascii_lowercase();
    let (order, name) = match s.as_str() {
        "renaissance" => (1, "Renacimiento"),
        "baroque" => (2, "Barroco"),
        "classical" => (3, "Clasicismo"),
        "romantic" => (4, "Romanticismo"),
        "modern" | "contemporary" => (5, "Siglo XX"),
        "popular" | "jazz" | "ragtime" | "blues" => (6, "Ragtime y popular"),
        "folk" | "traditional" => (7, "Tradicional"),
        "hymn" | "religious" | "sacred" => (8, "Música sacra"),
        "" => (10, "Sin estilo indicado"),
        _ => (9, "Otros estilos"),
    };
    (order, name.to_string())
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
        Library::load(Path::new("no-existe"), Path::new("no-existe"))
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
    fn filters_by_level_and_review_status() {
        let mut library = no_catalog();
        let all = Filter {
            min_level: 1,
            max_level: 8,
            ..Default::default()
        };
        let count = |l: &Library, f: &Filter| -> usize {
            l.browse(GroupBy::Level, f)
                .iter()
                .map(|g| g.entries.len())
                .sum()
        };
        assert_eq!(count(&library, &all), library.entries.len());

        let level_two = Filter {
            min_level: 2,
            max_level: 2,
            ..all.clone()
        };
        let shown: Vec<String> = library
            .browse(GroupBy::Level, &level_two)
            .iter()
            .flat_map(|g| g.entries.iter().map(|&i| library.entries[i].id.clone()))
            .collect();
        assert_eq!(shown, vec!["minueto-en-sol"]);

        let reviewed = Filter {
            status: Status::Reviewed,
            ..all.clone()
        };
        assert_eq!(count(&library, &reviewed), 0);
        library.entries[0].reviewed_here = true;
        assert_eq!(count(&library, &reviewed), 1);
    }

    #[test]
    fn styles_are_grouped_in_spanish_and_in_order() {
        assert_eq!(style_group(Some("Baroque")), (2, "Barroco".to_string()));
        assert!(style_group(Some("Romantic")).0 > style_group(Some("Classical")).0);
        assert_eq!(style_group(None).1, "Sin estilo indicado");
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
