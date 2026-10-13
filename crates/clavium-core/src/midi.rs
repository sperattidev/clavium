//! Importación de archivos MIDI estándar (SMF) al modelo de pieza.
//!
//! `midly` lee los bytes (running status, longitudes variables); acá se arma
//! el mapa de tempo, se emparejan Note On / Note Off, se reconstruye el pedal
//! de sustain y se asigna la mano de cada nota.

use std::collections::HashMap;

use midly::{Format, MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};

use crate::model::{
    Confidence, Hand, Metadata, Note, PedalSpan, Piece, Provenance, TimeSignature, sha256_hex,
};
use crate::tempo::{TempoChange, TempoError, TempoMap};

/// Controlador MIDI del pedal de sustain.
const CC_SUSTAIN: u8 = 64;
/// Altura desde la que una nota va a la mano derecha cuando no hay otra pista
/// para decidirlo (Do central).
pub const DEFAULT_HAND_SPLIT: u8 = 60;

#[derive(Debug, thiserror::Error)]
pub enum MidiImportError {
    #[error("archivo MIDI inválido: {0}")]
    Parse(#[from] midly::Error),
    #[error("el formato MIDI 2 (pistas secuenciales) no está soportado")]
    UnsupportedFormat,
    #[error("la división de tiempo SMPTE no está soportada")]
    UnsupportedTiming,
    #[error(transparent)]
    Tempo(#[from] TempoError),
}

/// Una parte de un archivo MIDI: las notas de un canal dentro de una pista.
/// Un MIDI de una canción suele tener una parte por instrumento.
pub type PartId = (usize, u8);

/// Canal MIDI de la batería en General MIDI (el 10, contando desde 1).
const DRUM_CHANNEL: u8 = 9;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiPart {
    pub track: usize,
    pub channel: u8,
    /// Nombre de la pista, si tiene.
    pub name: String,
    /// Programa General MIDI (0 = piano de cola), si el archivo lo indica.
    pub program: Option<u8>,
    pub drums: bool,
    pub notes: usize,
    pub lowest: u8,
    pub highest: u8,
}

impl MidiPart {
    pub fn id(&self) -> PartId {
        (self.track, self.channel)
    }

    /// Piano o teclado, según el programa o el nombre de la pista.
    pub fn is_piano(&self) -> bool {
        let name = self.name.to_lowercase();
        !self.drums
            && (self.program.is_some_and(|p| p < 8)
                || name.contains("piano")
                || name.contains("keys")
                || name.contains("teclado"))
    }

    /// Familia de instrumento de General MIDI, para mostrar en castellano.
    pub fn instrument(&self) -> &'static str {
        if self.drums {
            return "Batería";
        }
        const FAMILIES: [&str; 16] = [
            "Piano",
            "Percusión afinada",
            "Órgano",
            "Guitarra",
            "Bajo",
            "Cuerdas",
            "Ensamble",
            "Metales",
            "Lengüetas",
            "Vientos",
            "Sintetizador",
            "Sintetizador (colchón)",
            "Efectos de sintetizador",
            "Instrumentos étnicos",
            "Percusión",
            "Efectos de sonido",
        ];
        self.program.map_or("Instrumento sin indicar", |p| {
            FAMILIES[usize::from(p / 8).min(15)]
        })
    }
}

/// Mano para las notas de una parte al importar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PartHand {
    /// Se decide entre las partes en automático: las agudas a la derecha.
    #[default]
    Auto,
    Right,
    Left,
    /// Cada nota según su altura, dividiendo en el Do central.
    Split,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartChoice {
    pub part: PartId,
    pub hand: PartHand,
}

struct Parsed {
    ppq: u16,
    tempo_changes: Vec<TempoChange>,
    time_signatures: Vec<TimeSignature>,
    title: Option<String>,
    notes: Vec<(PartId, Note)>,
    sustain: Vec<PedalSpan>,
    track_names: HashMap<usize, String>,
    programs: HashMap<PartId, u8>,
}

fn parse(bytes: &[u8]) -> Result<Parsed, MidiImportError> {
    let smf = Smf::parse(bytes)?;
    if smf.header.format == Format::Sequential {
        return Err(MidiImportError::UnsupportedFormat);
    }
    let ppq = match smf.header.timing {
        Timing::Metrical(ppq) => ppq.as_int(),
        Timing::Timecode(..) => return Err(MidiImportError::UnsupportedTiming),
    };

    let mut parsed = Parsed {
        ppq,
        tempo_changes: Vec::new(),
        time_signatures: Vec::new(),
        title: None,
        notes: Vec::new(),
        sustain: Vec::new(),
        track_names: HashMap::new(),
        programs: HashMap::new(),
    };
    let mut pedal_down: Option<u64> = None;
    let mut last_tick = 0;

    for (track_index, track) in smf.tracks.iter().enumerate() {
        let mut tick: u64 = 0;
        // (canal, altura) -> (inicio, velocity)
        let mut open: HashMap<(u8, u8), (u64, u8)> = HashMap::new();

        for event in track {
            tick += u64::from(event.delta.as_int());
            match event.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => {
                    parsed.tempo_changes.push(TempoChange {
                        tick,
                        us_per_quarter: us.as_int(),
                    })
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(num, den_pow, _, _)) => {
                    if num > 0 && den_pow <= 6 {
                        parsed.time_signatures.push(TimeSignature {
                            tick,
                            numerator: num,
                            denominator: 1 << den_pow,
                        });
                    }
                }
                TrackEventKind::Meta(MetaMessage::TrackName(name)) => {
                    let name = String::from_utf8_lossy(name).trim().to_string();
                    if !name.is_empty() {
                        if parsed.title.is_none() {
                            parsed.title = Some(name.clone());
                        }
                        parsed.track_names.entry(track_index).or_insert(name);
                    }
                }
                TrackEventKind::Midi { channel, message } => {
                    let channel = channel.as_int();
                    let part = (track_index, channel);
                    match message {
                        MidiMessage::ProgramChange { program } => {
                            parsed.programs.entry(part).or_insert(program.as_int());
                        }
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            let key = key.as_int();
                            // Una tecla que se vuelve a tocar sin soltarse cierra la anterior.
                            if let Some((start, velocity)) = open.remove(&(channel, key))
                                && start < tick
                            {
                                parsed.notes.push((part, note(key, velocity, start, tick)));
                            }
                            open.insert((channel, key), (tick, vel.as_int()));
                        }
                        // Note On con velocity 0 equivale a Note Off.
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                            let key = key.as_int();
                            if let Some((start, velocity)) = open.remove(&(channel, key)) {
                                parsed.notes.push((part, note(key, velocity, start, tick)));
                            }
                        }
                        MidiMessage::Controller { controller, value }
                            if controller.as_int() == CC_SUSTAIN =>
                        {
                            let down = value.as_int() >= 64;
                            match (down, pedal_down) {
                                (true, None) => pedal_down = Some(tick),
                                (false, Some(start)) => {
                                    if start < tick {
                                        parsed.sustain.push(PedalSpan {
                                            start_tick: start,
                                            end_tick: tick,
                                        });
                                    }
                                    pedal_down = None;
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        // Notas que nunca se soltaron terminan al final de su pista.
        for ((channel, key), (start, velocity)) in open {
            parsed
                .notes
                .push(((track_index, channel), note(key, velocity, start, tick)));
        }
        last_tick = last_tick.max(tick);
    }

    if let Some(start) = pedal_down
        && start < last_tick
    {
        parsed.sustain.push(PedalSpan {
            start_tick: start,
            end_tick: last_tick,
        });
    }
    parsed.sustain.sort_by_key(|p| p.start_tick);
    parsed.time_signatures.sort_by_key(|s| s.tick);
    Ok(parsed)
}

/// Las partes del archivo que tienen notas, en el orden de sus pistas.
pub fn inspect_parts(bytes: &[u8]) -> Result<Vec<MidiPart>, MidiImportError> {
    let parsed = parse(bytes)?;
    let mut parts: Vec<MidiPart> = Vec::new();
    for (id, n) in &parsed.notes {
        match parts.iter_mut().find(|p| p.id() == *id) {
            Some(p) => {
                p.notes += 1;
                p.lowest = p.lowest.min(n.pitch);
                p.highest = p.highest.max(n.pitch);
            }
            None => parts.push(MidiPart {
                track: id.0,
                channel: id.1,
                name: parsed.track_names.get(&id.0).cloned().unwrap_or_default(),
                program: parsed.programs.get(id).copied(),
                drums: id.1 == DRUM_CHANNEL,
                notes: 1,
                lowest: n.pitch,
                highest: n.pitch,
            }),
        }
    }
    parts.sort_by_key(|p| (p.track, p.channel));
    Ok(parts)
}

/// Las partes que conviene marcar al abrir el archivo: las de piano si hay;
/// si no, todas menos la batería.
pub fn default_choices(parts: &[MidiPart]) -> Vec<PartChoice> {
    let piano: Vec<&MidiPart> = parts.iter().filter(|p| p.is_piano()).collect();
    let chosen: Vec<&MidiPart> = if piano.is_empty() {
        parts.iter().filter(|p| !p.drums).collect()
    } else {
        piano
    };
    chosen
        .into_iter()
        .map(|p| PartChoice {
            part: p.id(),
            hand: PartHand::Auto,
        })
        .collect()
}

/// Importa un archivo MIDI completo, sin la batería. La pieza resultante queda
/// con procedencia de nivel D (sin verificar) y el hash del archivo; el
/// pipeline de catálogo la reemplaza cuando la pieza se revisa.
pub fn import_smf(bytes: &[u8]) -> Result<Piece, MidiImportError> {
    let choices: Vec<PartChoice> = inspect_parts(bytes)?
        .iter()
        .filter(|p| !p.drums)
        .map(|p| PartChoice {
            part: p.id(),
            hand: PartHand::Auto,
        })
        .collect();
    import_parts(bytes, &choices)
}

/// Importa solo las partes elegidas, con la mano indicada para cada una.
pub fn import_parts(bytes: &[u8], choices: &[PartChoice]) -> Result<Piece, MidiImportError> {
    let parsed = parse(bytes)?;
    let hand_of = |id: &PartId| choices.iter().find(|c| c.part == *id).map(|c| c.hand);
    let mut notes: Vec<(PartId, Note)> = parsed
        .notes
        .into_iter()
        .filter(|(id, _)| hand_of(id).is_some())
        .collect();

    // Las partes en automático se reparten entre las dos manos.
    let mut auto: Vec<(PartId, Note)> = Vec::new();
    let mut fixed: Vec<Note> = Vec::new();
    for (id, mut n) in notes.drain(..) {
        match hand_of(&id).unwrap_or_default() {
            PartHand::Auto => auto.push((id, n)),
            PartHand::Right => {
                n.hand = Hand::Right;
                fixed.push(n);
            }
            PartHand::Left => {
                n.hand = Hand::Left;
                fixed.push(n);
            }
            PartHand::Split => {
                n.hand = hand_by_split(n.pitch, DEFAULT_HAND_SPLIT);
                fixed.push(n);
            }
        }
    }
    assign_hands_by_part(&mut auto);

    let mut piece = Piece::new(
        Metadata::titled(parsed.title.unwrap_or_else(|| "Sin título".to_string())),
        TempoMap::new(parsed.ppq, parsed.tempo_changes)?,
    );
    piece.provenance = Some(Provenance {
        source: "importado por el usuario".to_string(),
        edition: None,
        license: "desconocida".to_string(),
        file_sha256: Some(sha256_hex(bytes)),
        confidence: Confidence::D,
        reviewers: Vec::new(),
    });
    piece.time_signatures = parsed.time_signatures;
    piece.notes = auto.into_iter().map(|(_, n)| n).chain(fixed).collect();
    resolve_overlaps(&mut piece.notes);
    piece.sustain = parsed.sustain;
    piece.sort_notes();
    Ok(piece)
}

fn note(pitch: u8, velocity: u8, start_tick: u64, end_tick: u64) -> Note {
    Note {
        pitch,
        velocity,
        start_tick,
        end_tick,
        hand: Hand::Unknown,
        finger: None,
    }
}

/// Con dos o más pistas, se ordenan por altura media y se parten en dos
/// grupos por el salto más grande entre medias consecutivas: el grupo agudo
/// es la mano derecha. Así funciona tanto con una pista por mano como con
/// una pista por voz (lo que generan muchos editores de partituras). Con una
/// sola pista se divide por altura en el Do central.
pub(crate) fn assign_hands_by_part(notes: &mut [(PartId, Note)]) {
    let mut by_track: HashMap<PartId, (u64, u64)> = HashMap::new(); // parte -> (suma de alturas, cantidad)
    for (track, n) in notes.iter() {
        let entry = by_track.entry(*track).or_default();
        entry.0 += u64::from(n.pitch);
        entry.1 += 1;
    }

    if by_track.len() >= 2 {
        let mut tracks: Vec<(PartId, f64)> = by_track
            .into_iter()
            .map(|(t, (sum, count))| (t, sum as f64 / count as f64))
            .collect();
        tracks.sort_by(|a, b| a.1.total_cmp(&b.1));
        let split = (1..tracks.len())
            .max_by(|&a, &b| {
                let gap_a = tracks[a].1 - tracks[a - 1].1;
                let gap_b = tracks[b].1 - tracks[b - 1].1;
                gap_a.total_cmp(&gap_b)
            })
            .expect("hay al menos dos pistas");
        let left: Vec<PartId> = tracks[..split].iter().map(|(t, _)| *t).collect();
        for (track, n) in notes.iter_mut() {
            n.hand = if left.contains(track) {
                Hand::Left
            } else {
                Hand::Right
            };
        }
    } else {
        for (_, n) in notes.iter_mut() {
            n.hand = hand_by_split(n.pitch, DEFAULT_HAND_SPLIT);
        }
    }
}

/// Dos notas de la misma altura no pueden sonar a la vez en un piano. Pasa
/// en MIDI de partituras cuando dos voces comparten una nota: la primera se
/// corta donde empieza la segunda, y si empiezan juntas queda la más larga.
pub fn resolve_overlaps(notes: &mut Vec<Note>) {
    notes.sort_by_key(|n| (n.pitch, n.start_tick, std::cmp::Reverse(n.end_tick)));
    let mut kept: Vec<Note> = Vec::with_capacity(notes.len());
    for n in notes.drain(..) {
        if let Some(prev) = kept.last_mut()
            && prev.pitch == n.pitch
            && n.start_tick < prev.end_tick
        {
            if n.start_tick == prev.start_tick {
                continue; // la anterior es igual o más larga
            }
            prev.end_tick = n.start_tick;
        }
        kept.push(n);
    }
    *notes = kept;
}

/// Mano que corresponde a una altura si se divide el teclado en `split`.
pub fn hand_by_split(pitch: u8, split: u8) -> Hand {
    if pitch >= split {
        Hand::Right
    } else {
        Hand::Left
    }
}

/// Reasigna por altura las notas sin mano, por ejemplo después de importar un
/// archivo de una sola pista con una división elegida por el usuario.
pub fn assign_unknown_hands_by_split(piece: &mut Piece, split: u8) {
    for n in piece.notes.iter_mut().filter(|n| n.hand == Hand::Unknown) {
        n.hand = hand_by_split(n.pitch, split);
    }
}
