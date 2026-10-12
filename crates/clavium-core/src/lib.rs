//! Núcleo de Clavium: modelo de pieza, mapa de tempo, importación MIDI,
//! validación y formato de archivo `.clav`.
//!
//! Este crate no depende de GPU, audio ni dispositivos MIDI, así que todo se
//! puede probar con tests automáticos.

pub mod clav;
pub mod midi;
pub mod model;
pub mod tempo;
pub mod validate;

pub use model::{
    Arrangement, Confidence, Hand, Metadata, Note, PIANO_HIGHEST, PIANO_LOWEST, PedalSpan, Piece,
    Provenance, Section, TimeSignature, sha256_hex,
};
pub use tempo::{TempoChange, TempoError, TempoMap};
