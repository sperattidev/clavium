//! "Mis piezas": archivos MIDI o MusicXML que el usuario importa. Se guardan
//! solo en su computadora, convertidos a `.clav`; Clavium no los distribuye.

use std::path::{Path, PathBuf};

use clavium_core::analysis::{auto_sections, estimate_level};
use clavium_core::midi::{self, MidiPart, PartChoice, PartHand, default_choices};
use clavium_core::musicxml;
use clavium_core::validate::{IssueKind, validate};
use clavium_core::{PIANO_HIGHEST, PIANO_LOWEST, Piece, clav, sha256_hex};

pub fn dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Clavium")
        .join("mis-piezas")
}

/// Un MIDI abierto, esperando que el usuario elija qué partes tocar.
pub struct PendingImport {
    pub file_name: String,
    pub title: String,
    pub parts: Vec<MidiPart>,
    /// Si cada parte se toca, y con qué mano.
    pub choices: Vec<(bool, PartHand)>,
    bytes: Vec<u8>,
    /// MusicXML (o .mxl) en lugar de MIDI.
    musicxml: bool,
}

impl PendingImport {
    pub fn open(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("no se pudo leer el archivo: {e}"))?;
        let file_name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Pieza importada".to_string());
        Self::from_bytes(file_name, bytes)
    }

    pub fn from_bytes(file_name: String, bytes: Vec<u8>) -> Result<Self, String> {
        let is_xml = musicxml::looks_like_musicxml(&bytes);
        let parts = if is_xml {
            musicxml::inspect_parts(&bytes).map_err(|e| e.to_string())?
        } else {
            midi::inspect_parts(&bytes).map_err(|e| e.to_string())?
        };
        if parts.iter().all(|p| p.drums) {
            return Err("el archivo no tiene notas para tocar en piano".into());
        }
        let defaults = default_choices(&parts);
        let choices = parts
            .iter()
            .map(|p| (defaults.iter().any(|c| c.part == p.id()), PartHand::Auto))
            .collect();
        Ok(PendingImport {
            title: file_name.replace(['_', '-'], " ").trim().to_string(),
            file_name,
            parts,
            choices,
            bytes,
            musicxml: is_xml,
        })
    }

    pub fn selected_notes(&self) -> usize {
        self.parts
            .iter()
            .zip(&self.choices)
            .filter(|(_, (on, _))| *on)
            .map(|(p, _)| p.notes)
            .sum()
    }

    /// Convierte las partes elegidas en una pieza lista para practicar.
    pub fn build(&self) -> Result<Piece, String> {
        let choices: Vec<PartChoice> = self
            .parts
            .iter()
            .zip(&self.choices)
            .filter(|(_, (on, _))| *on)
            .map(|(p, (_, hand))| PartChoice {
                part: p.id(),
                hand: *hand,
            })
            .collect();
        if choices.is_empty() {
            return Err("elegí al menos una parte para tocar".into());
        }
        let mut piece = if self.musicxml {
            musicxml::import_parts(&self.bytes, &choices).map_err(|e| e.to_string())?
        } else {
            midi::import_parts(&self.bytes, &choices).map_err(|e| e.to_string())?
        };
        // Notas que el piano no puede tocar o que no duran nada.
        piece.notes.retain(|n| {
            n.end_tick > n.start_tick && (PIANO_LOWEST..=PIANO_HIGHEST).contains(&n.pitch)
        });
        if piece.notes.is_empty() {
            return Err("las partes elegidas no tienen notas tocables".into());
        }
        let title = self.title.trim();
        piece.metadata.title = if title.is_empty() {
            self.file_name.clone()
        } else {
            title.to_string()
        };
        piece.metadata.difficulty = Some(estimate_level(&piece));
        piece.sections = auto_sections(&piece);
        let errors = validate(&piece)
            .into_iter()
            .filter(|i| {
                !matches!(
                    i.kind,
                    IssueKind::UnknownHand { .. } | IssueKind::ExtremeTempo { .. }
                )
            })
            .count();
        if errors > 0 {
            return Err(format!(
                "la pieza tiene {errors} problemas que no se pudieron corregir"
            ));
        }
        Ok(piece)
    }

    /// Guarda la pieza en "Mis piezas" y devuelve su identificador.
    pub fn save(&self, piece: &Piece) -> Result<String, String> {
        save_in(&dir(), &self.bytes, piece)
    }
}

fn save_in(dir: &Path, source: &[u8], piece: &Piece) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // El mismo archivo con las mismas partes da el mismo id: no se duplica.
    let mut key = source.to_vec();
    key.extend(piece.metadata.title.as_bytes());
    key.extend(piece.notes.len().to_le_bytes());
    let id = format!("mia-{}", &sha256_hex(&key)[..12]);
    clav::save(piece, dir.join(format!("{id}.clav"))).map_err(|e| e.to_string())?;
    Ok(id)
}

/// Las piezas guardadas, con su id (el nombre del archivo).
pub fn list(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut pieces: Vec<(String, PathBuf)> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "clav"))
        .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p)))
        .collect();
    pieces.sort();
    pieces
}

pub fn remove(id: &str) -> Result<(), String> {
    if !id.starts_with("mia-") || id.contains(['/', '\\', '.']) {
        return Err("solo se pueden quitar piezas importadas".into());
    }
    std::fs::remove_file(dir().join(format!("{id}.clav"))).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use midly::num::{u4, u7, u15, u28};
    use midly::{
        Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
    };

    fn midi(channel: u8, keys: &[u8]) -> Vec<u8> {
        let mut events = Vec::new();
        for &k in keys {
            for (delta, vel) in [(0u32, 90u8), (480, 0)] {
                events.push(TrackEvent {
                    delta: u28::new(delta),
                    kind: TrackEventKind::Midi {
                        channel: u4::new(channel),
                        message: MidiMessage::NoteOn {
                            key: u7::new(k),
                            vel: u7::new(vel),
                        },
                    },
                });
            }
        }
        events.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        let smf = Smf {
            header: Header::new(Format::SingleTrack, Timing::Metrical(u15::new(480))),
            tracks: vec![events],
        };
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn a_melody_is_imported_with_sections_and_level() {
        let pending =
            PendingImport::from_bytes("mi_tema".into(), midi(0, &[60, 62, 64, 65, 67, 69, 71, 72]))
                .unwrap();
        assert_eq!(pending.title, "mi tema");
        assert_eq!(pending.selected_notes(), 8);
        let piece = pending.build().unwrap();
        assert_eq!(piece.metadata.title, "mi tema");
        assert!(piece.metadata.difficulty.is_some());
        assert_eq!(piece.sections.len(), 1);
    }

    #[test]
    fn a_drums_only_file_is_rejected() {
        assert!(PendingImport::from_bytes("bateria".into(), midi(9, &[36, 38])).is_err());
    }

    #[test]
    fn saving_twice_keeps_one_file_and_it_can_be_listed() {
        let dir = std::env::temp_dir().join(format!("clavium-mine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bytes = midi(0, &[60, 64, 67]);
        let pending = PendingImport::from_bytes("acorde".into(), bytes.clone()).unwrap();
        let piece = pending.build().unwrap();
        let a = save_in(&dir, &bytes, &piece).unwrap();
        let b = save_in(&dir, &bytes, &piece).unwrap();
        assert_eq!(a, b);
        let listed = list(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, a);
        assert!(clav::load(&listed[0].1).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_imported_pieces_can_be_removed() {
        assert!(remove("../otra").is_err());
        assert!(remove("estrellita").is_err());
    }
}
