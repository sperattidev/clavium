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

/// Importa un archivo MIDI. La pieza resultante queda con procedencia de
/// nivel D (sin verificar) y el hash del archivo; el pipeline de catálogo la
/// reemplaza cuando la pieza se revisa.
pub fn import_smf(bytes: &[u8]) -> Result<Piece, MidiImportError> {
    let smf = Smf::parse(bytes)?;
    if smf.header.format == Format::Sequential {
        return Err(MidiImportError::UnsupportedFormat);
    }
    let ppq = match smf.header.timing {
        Timing::Metrical(ppq) => ppq.as_int(),
        Timing::Timecode(..) => return Err(MidiImportError::UnsupportedTiming),
    };

    let mut tempo_changes = Vec::new();
    let mut time_signatures = Vec::new();
    let mut title: Option<String> = None;
    // (pista, nota)
    let mut notes: Vec<(usize, Note)> = Vec::new();
    let mut sustain = Vec::new();
    let mut pedal_down: Option<u64> = None;
    let mut last_tick = 0;

    for (track_index, track) in smf.tracks.iter().enumerate() {
        let mut tick: u64 = 0;
        // (canal, altura) -> (inicio, velocity)
        let mut open: HashMap<(u8, u8), (u64, u8)> = HashMap::new();

        for event in track {
            tick += u64::from(event.delta.as_int());
            match event.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => tempo_changes.push(TempoChange {
                    tick,
                    us_per_quarter: us.as_int(),
                }),
                TrackEventKind::Meta(MetaMessage::TimeSignature(num, den_pow, _, _)) => {
                    if num > 0 && den_pow <= 6 {
                        time_signatures.push(TimeSignature {
                            tick,
                            numerator: num,
                            denominator: 1 << den_pow,
                        });
                    }
                }
                TrackEventKind::Meta(MetaMessage::TrackName(name)) if title.is_none() => {
                    let name = String::from_utf8_lossy(name).trim().to_string();
                    if !name.is_empty() {
                        title = Some(name);
                    }
                }
                TrackEventKind::Midi { channel, message } => {
                    let channel = channel.as_int();
                    match message {
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            let key = key.as_int();
                            // Una tecla que se vuelve a tocar sin soltarse cierra la anterior.
                            if let Some((start, velocity)) = open.remove(&(channel, key))
                                && start < tick
                            {
                                notes.push((track_index, note(key, velocity, start, tick)));
                            }
                            open.insert((channel, key), (tick, vel.as_int()));
                        }
                        // Note On con velocity 0 equivale a Note Off.
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                            let key = key.as_int();
                            if let Some((start, velocity)) = open.remove(&(channel, key)) {
                                notes.push((track_index, note(key, velocity, start, tick)));
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
                                        sustain.push(PedalSpan {
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
        for ((_, key), (start, velocity)) in open {
            notes.push((track_index, note(key, velocity, start, tick)));
        }
        last_tick = last_tick.max(tick);
    }

    if let Some(start) = pedal_down
        && start < last_tick
    {
        sustain.push(PedalSpan {
            start_tick: start,
            end_tick: last_tick,
        });
    }
    sustain.sort_by_key(|p| p.start_tick);
    time_signatures.sort_by_key(|s| s.tick);

    assign_hands_by_track(&mut notes);

    let mut piece = Piece::new(
        Metadata::titled(title.unwrap_or_else(|| "Sin título".to_string())),
        TempoMap::new(ppq, tempo_changes)?,
    );
    piece.provenance = Some(Provenance {
        source: "importado por el usuario".to_string(),
        edition: None,
        license: "desconocida".to_string(),
        file_sha256: Some(sha256_hex(bytes)),
        confidence: Confidence::D,
        reviewers: Vec::new(),
    });
    piece.time_signatures = time_signatures;
    piece.notes = notes.into_iter().map(|(_, n)| n).collect();
    piece.sustain = sustain;
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

/// Si exactamente dos pistas tienen notas, la de altura media más aguda es la
/// mano derecha (la convención habitual de los MIDI de piano). En otro caso
/// se divide por altura en el Do central.
fn assign_hands_by_track(notes: &mut [(usize, Note)]) {
    let mut by_track: HashMap<usize, (u64, u64)> = HashMap::new(); // pista -> (suma de alturas, cantidad)
    for (track, n) in notes.iter() {
        let entry = by_track.entry(*track).or_default();
        entry.0 += u64::from(n.pitch);
        entry.1 += 1;
    }

    if by_track.len() == 2 {
        let mut tracks: Vec<(usize, f64)> = by_track
            .into_iter()
            .map(|(t, (sum, count))| (t, sum as f64 / count as f64))
            .collect();
        tracks.sort_by(|a, b| a.1.total_cmp(&b.1));
        let left_track = tracks[0].0;
        for (track, n) in notes.iter_mut() {
            n.hand = if *track == left_track {
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
