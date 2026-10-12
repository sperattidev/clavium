/// Lo que Clavium lee de un mensaje MIDI. Todo lo demás se ignora.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    NoteOn {
        pitch: u8,
        velocity: u8,
    },
    NoteOff {
        pitch: u8,
    },
    /// Pedal de sustain (CC 64): presionado desde el valor 64.
    Sustain {
        down: bool,
    },
}

const CC_SUSTAIN: u8 = 64;

/// Interpreta un mensaje MIDI completo. Acepta los 16 canales.
pub fn parse_message(bytes: &[u8]) -> Option<KeyEvent> {
    let (&status, data) = bytes.split_first()?;
    let [a, b, ..] = *data else {
        return None;
    };
    if a > 127 || b > 127 {
        return None;
    }
    match status & 0xF0 {
        // Note On con velocity 0 equivale a Note Off.
        0x90 if b > 0 => Some(KeyEvent::NoteOn {
            pitch: a,
            velocity: b,
        }),
        0x90 | 0x80 => Some(KeyEvent::NoteOff { pitch: a }),
        0xB0 if a == CC_SUSTAIN => Some(KeyEvent::Sustain { down: b >= 64 }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_notes_on_any_channel() {
        assert_eq!(
            parse_message(&[0x93, 60, 100]),
            Some(KeyEvent::NoteOn {
                pitch: 60,
                velocity: 100
            })
        );
        assert_eq!(
            parse_message(&[0x8F, 60, 40]),
            Some(KeyEvent::NoteOff { pitch: 60 })
        );
    }

    #[test]
    fn velocity_zero_is_note_off() {
        assert_eq!(
            parse_message(&[0x90, 64, 0]),
            Some(KeyEvent::NoteOff { pitch: 64 })
        );
    }

    #[test]
    fn reads_sustain_pedal() {
        assert_eq!(
            parse_message(&[0xB0, 64, 127]),
            Some(KeyEvent::Sustain { down: true })
        );
        assert_eq!(
            parse_message(&[0xB0, 64, 63]),
            Some(KeyEvent::Sustain { down: false })
        );
    }

    #[test]
    fn ignores_everything_else() {
        assert_eq!(parse_message(&[0xB0, 7, 100]), None); // volumen
        assert_eq!(parse_message(&[0xFE]), None); // active sensing
        assert_eq!(parse_message(&[0xF8]), None); // clock
        assert_eq!(parse_message(&[0xE0, 0, 64]), None); // pitch bend
        assert_eq!(parse_message(&[0x90, 60]), None); // incompleto
        assert_eq!(parse_message(&[]), None);
    }
}
