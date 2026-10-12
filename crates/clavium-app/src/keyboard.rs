//! Geometría del teclado en pantalla, en unidades de "ancho de tecla blanca".

/// Ancho de una tecla negra respecto de una blanca.
pub const BLACK_WIDTH: f32 = 0.58;
/// Largo de una tecla negra respecto de una blanca.
pub const BLACK_LENGTH: f32 = 0.63;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardSize {
    Keys88,
    Keys76,
    Keys61,
    Keys49,
}

impl KeyboardSize {
    pub const ALL: [KeyboardSize; 4] = [
        KeyboardSize::Keys88,
        KeyboardSize::Keys76,
        KeyboardSize::Keys61,
        KeyboardSize::Keys49,
    ];

    /// Primera y última nota MIDI de los tamaños de teclado más comunes.
    pub fn range(self) -> (u8, u8) {
        match self {
            KeyboardSize::Keys88 => (21, 108), // La0 a Do8
            KeyboardSize::Keys76 => (28, 103), // Mi1 a Sol7
            KeyboardSize::Keys61 => (36, 96),  // Do2 a Do7
            KeyboardSize::Keys49 => (36, 84),  // Do2 a Do6
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            KeyboardSize::Keys88 => "88 teclas",
            KeyboardSize::Keys76 => "76 teclas",
            KeyboardSize::Keys61 => "61 teclas",
            KeyboardSize::Keys49 => "49 teclas",
        }
    }
}

pub fn is_black(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

/// Corrimiento de cada tecla negra respecto del borde entre sus dos blancas.
/// En un piano real Do# y Re# se abren hacia afuera, igual que Fa# y La#.
fn black_offset(pitch: u8) -> f32 {
    match pitch % 12 {
        1 => -0.08,
        3 => 0.08,
        6 => -0.1,
        10 => 0.1,
        _ => 0.0,
    }
}

const NAMES: [&str; 12] = [
    "Do", "Do#", "Re", "Re#", "Mi", "Fa", "Fa#", "Sol", "Sol#", "La", "La#", "Si",
];

/// Nombre de la nota en notación latina, con Do4 = 60 (Do central).
pub fn note_name(pitch: u8) -> String {
    let octave = i32::from(pitch / 12) - 1;
    format!("{}{}", NAMES[usize::from(pitch % 12)], octave)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyShape {
    pub pitch: u8,
    pub black: bool,
    /// Borde izquierdo, en anchos de tecla blanca.
    pub x: f32,
    pub width: f32,
}

#[derive(Debug, Clone)]
pub struct KeyboardLayout {
    pub lowest: u8,
    pub highest: u8,
    /// Primero todas las blancas y después las negras, para dibujarlas en orden.
    pub keys: Vec<KeyShape>,
    pub white_count: usize,
}

impl KeyboardLayout {
    pub fn new(size: KeyboardSize) -> Self {
        let (lowest, highest) = size.range();
        let mut whites = Vec::new();
        let mut blacks = Vec::new();
        let mut white_index = 0.0;
        for pitch in lowest..=highest {
            if is_black(pitch) {
                let center = white_index + black_offset(pitch);
                blacks.push(KeyShape {
                    pitch,
                    black: true,
                    x: center - BLACK_WIDTH / 2.0,
                    width: BLACK_WIDTH,
                });
            } else {
                whites.push(KeyShape {
                    pitch,
                    black: false,
                    x: white_index,
                    width: 1.0,
                });
                white_index += 1.0;
            }
        }
        let white_count = whites.len();
        whites.extend(blacks);
        KeyboardLayout {
            lowest,
            highest,
            keys: whites,
            white_count,
        }
    }

    pub fn contains(&self, pitch: u8) -> bool {
        (self.lowest..=self.highest).contains(&pitch)
    }

    /// Tecla bajo un punto: `x` en anchos de tecla blanca desde el borde
    /// izquierdo, `depth` de 0 (fondo del teclado) a 1 (borde delantero).
    pub fn key_at(&self, x: f32, depth: f32) -> Option<u8> {
        let hit = |k: &&KeyShape| x >= k.x && x < k.x + k.width;
        if depth < BLACK_LENGTH
            && let Some(k) = self.keys.iter().filter(|k| k.black).find(hit)
        {
            return Some(k.pitch);
        }
        self.keys
            .iter()
            .filter(|k| !k.black)
            .find(hit)
            .map(|k| k.pitch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(size: KeyboardSize) -> (usize, usize) {
        let layout = KeyboardLayout::new(size);
        (layout.white_count, layout.keys.len() - layout.white_count)
    }

    #[test]
    fn standard_sizes_have_the_right_keys() {
        assert_eq!(counts(KeyboardSize::Keys88), (52, 36));
        assert_eq!(counts(KeyboardSize::Keys76), (45, 31));
        assert_eq!(counts(KeyboardSize::Keys61), (36, 25));
        assert_eq!(counts(KeyboardSize::Keys49), (29, 20));
    }

    #[test]
    fn hit_test_prefers_black_keys_near_the_back() {
        let layout = KeyboardLayout::new(KeyboardSize::Keys61);
        let c4 = layout.keys.iter().find(|k| k.pitch == 60).unwrap();
        let c_sharp = layout.keys.iter().find(|k| k.pitch == 61).unwrap();
        let x = c_sharp.x + c_sharp.width / 2.0;
        assert_eq!(layout.key_at(x, 0.3), Some(61));
        // Do# está corrido a la izquierda, así que por delante cae sobre Do.
        assert_eq!(layout.key_at(x, 0.9), Some(60));
        assert_eq!(layout.key_at(c4.x + 1.2, 0.9), Some(62));
        assert_eq!(layout.key_at(-1.0, 0.5), None);
    }

    #[test]
    fn names_use_middle_c_as_do4() {
        assert_eq!(note_name(60), "Do4");
        assert_eq!(note_name(21), "La0");
        assert_eq!(note_name(108), "Do8");
        assert_eq!(note_name(66), "Fa#4");
    }
}
