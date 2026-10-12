//! Entrada MIDI en vivo: detecta el teclado del usuario, se reconecta solo y
//! entrega cada nota con el instante en que llegó.

mod message;
mod ports;
mod service;

pub use message::{KeyEvent, parse_message};
pub use ports::{PortKind, choose_port, classify_port};
pub use service::{MidiEvent, MidiService, ServiceConfig};
