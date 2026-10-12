//! Modelo interno de una pieza.
//!
//! Los tiempos se guardan en ticks, que son exactos; los segundos se derivan
//! del [`TempoMap`]. Así una pieza se puede reproducir a cualquier tempo sin
//! perder precisión.

use serde::{Deserialize, Serialize};

use crate::tempo::TempoMap;

/// Nota MIDI más grave de un piano de 88 teclas (La0).
pub const PIANO_LOWEST: u8 = 21;
/// Nota MIDI más aguda de un piano de 88 teclas (Do8).
pub const PIANO_HIGHEST: u8 = 108;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hand {
    Left,
    Right,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub pitch: u8,
    pub velocity: u8,
    pub start_tick: u64,
    /// Momento en que se suelta la tecla (sin contar el pedal).
    pub end_tick: u64,
    #[serde(default)]
    pub hand: Hand,
    /// Digitación sugerida, de 1 (pulgar) a 5 (meñique).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finger: Option<u8>,
}

/// Tramo con el pedal de sustain presionado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PedalSpan {
    pub start_tick: u64,
    pub end_tick: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSignature {
    pub tick: u64,
    pub numerator: u8,
    pub denominator: u8,
}

/// Rango de compases con nombre (intro, tema A, puente) para elegir bucles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub start_tick: u64,
    pub end_tick: u64,
}

/// Nivel de confianza de las notas, según su origen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    /// Partitura digital revisada.
    A,
    /// Interpretación capturada por MIDI y alineada con la partitura.
    B,
    /// Transcripción automática desde audio.
    C,
    /// MIDI sin procedencia.
    D,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arrangement {
    #[default]
    Original,
    Intermediate,
    Simplified,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Metadata {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arranger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// Nivel de dificultad del 1 al 8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<u8>,
    #[serde(default)]
    pub arrangement: Arrangement,
}

impl Metadata {
    pub fn titled(title: impl Into<String>) -> Self {
        Metadata {
            title: title.into(),
            ..Default::default()
        }
    }
}

/// De dónde sale una pieza y con qué derechos se puede usar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edition: Option<String>,
    pub license: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_sha256: Option<String>,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Piece {
    pub metadata: Metadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    pub tempo: TempoMap,
    #[serde(default)]
    pub time_signatures: Vec<TimeSignature>,
    /// Ordenadas por inicio y luego por altura.
    pub notes: Vec<Note>,
    #[serde(default)]
    pub sustain: Vec<PedalSpan>,
    #[serde(default)]
    pub sections: Vec<Section>,
}

impl Piece {
    pub fn new(metadata: Metadata, tempo: TempoMap) -> Self {
        Piece {
            metadata,
            provenance: None,
            tempo,
            time_signatures: Vec::new(),
            notes: Vec::new(),
            sustain: Vec::new(),
            sections: Vec::new(),
        }
    }

    pub fn sort_notes(&mut self) {
        self.notes
            .sort_by_key(|n| (n.start_tick, n.pitch, n.end_tick));
    }

    /// Último tick con contenido (fin de nota o de pedal).
    pub fn end_tick(&self) -> u64 {
        let notes = self.notes.iter().map(|n| n.end_tick);
        let pedal = self.sustain.iter().map(|p| p.end_tick);
        notes.chain(pedal).max().unwrap_or(0)
    }

    pub fn duration_seconds(&self) -> f64 {
        self.tempo.tick_to_seconds(self.end_tick())
    }

    /// Inicio y fin de la nota en segundos.
    pub fn note_seconds(&self, note: &Note) -> (f64, f64) {
        (
            self.tempo.tick_to_seconds(note.start_tick),
            self.tempo.tick_to_seconds(note.end_tick),
        )
    }

    /// Fin sonoro efectivo: si la tecla se suelta con el pedal de sustain
    /// presionado, la nota suena hasta que se levanta el pedal.
    pub fn sounding_end_tick(&self, note: &Note) -> u64 {
        self.sustain
            .iter()
            .find(|p| p.start_tick <= note.end_tick && note.end_tick < p.end_tick)
            .map_or(note.end_tick, |p| p.end_tick)
    }

    /// Ticks donde empieza cada compás, hasta cubrir el final de la pieza.
    /// Sin cifra de compás se asume 4/4; un cambio de compás entra en vigor
    /// en el primer compás que empieza en o después de él.
    pub fn bar_ticks(&self) -> Vec<u64> {
        let mut sigs = self.time_signatures.clone();
        sigs.sort_by_key(|s| s.tick);
        if sigs.first().is_none_or(|s| s.tick > 0) {
            sigs.insert(
                0,
                TimeSignature {
                    tick: 0,
                    numerator: 4,
                    denominator: 4,
                },
            );
        }

        let end = self.end_tick().max(1);
        let ppq = u64::from(self.tempo.ppq());
        let mut bars = Vec::new();
        let mut tick = 0;
        let mut i = 0;
        while tick < end {
            while i + 1 < sigs.len() && sigs[i + 1].tick <= tick {
                i += 1;
            }
            bars.push(tick);
            let sig = sigs[i];
            let len = ppq * 4 * u64::from(sig.numerator) / u64::from(sig.denominator.max(1));
            if len == 0 {
                break;
            }
            tick += len;
        }
        bars
    }
}

/// Hash SHA-256 en hexadecimal, para registrar la procedencia de un archivo.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
