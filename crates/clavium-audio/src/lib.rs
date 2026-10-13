//! Sonido de Clavium: el piano interno y la salida de audio.

mod engine;
pub mod synth;

pub use engine::{AudioEngine, AudioError, Command, OutputInfo};
pub use synth::PianoSynth;
