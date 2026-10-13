//! Formato de texto para escribir piezas a mano.
//!
//! ```text
//! title: Oda a la alegría
//! composer: Ludwig van Beethoven
//! difficulty: 1
//! arrangement: simplified
//! tempo: 100
//! time: 4/4
//! section: Frase A 1-4
//! right: E4 E4 F4 G4 | G4 F4 E4 D4
//! left:  C3:4        | G2:4
//! ```
//!
//! Cada nota es una altura en notación inglesa (`C4` es el Do central,
//! `F#3`, `Bb2`) seguida opcionalmente de `:duración` en negras (`:2`, `:0.5`,
//! `:1/3`). Sin duración se repite la anterior. `R` es un silencio y `+` une
//! notas en un acorde (`C3+E3+G3:2`). Las barras `|` y los fines de línea
//! cierran compases, y se verifica que cada uno sume lo que indica la cifra
//! de compás. Las líneas `right:` y `left:` pueden repetirse y se concatenan.

use crate::model::{
    Arrangement, Confidence, Hand, Metadata, Note, Piece, Provenance, Section, TimeSignature,
};
use crate::tempo::{TempoChange, TempoMap};

/// Resolución de las piezas escritas con este formato.
pub const PPQ: u16 = 480;
const DEFAULT_VELOCITY: u8 = 80;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotationError {
    #[error("línea {line}: {message}")]
    Syntax { line: usize, message: String },
    #[error("falta el campo `{0}`")]
    Missing(&'static str),
    #[error("mano {hand}, compás {bar}: dura {found} negras y debería durar {expected}")]
    BarLength {
        hand: &'static str,
        bar: usize,
        found: String,
        expected: String,
    },
}

/// Lee una pieza completa escrita en el formato de texto.
pub fn parse_piece(text: &str) -> Result<Piece, NotationError> {
    let mut metadata = Metadata::default();
    let mut bpm: Option<f64> = None;
    let mut time = (4u8, 4u8);
    let mut source = None;
    let mut license = None;
    let mut sections = Vec::new();
    let mut right = Vec::new();
    let mut left = Vec::new();

    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        // Solo las líneas que empiezan con `#` son comentarios: `#` también es
        // el sostenido de `F#4`.
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let err = |message: String| NotationError::Syntax {
            line: line_no,
            message,
        };
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| err("se esperaba `campo: valor`".into()))?;
        let value = value.trim();
        match key.trim() {
            "title" => metadata.title = value.to_string(),
            "composer" => metadata.composer = Some(value.to_string()),
            "arranger" => metadata.arranger = Some(value.to_string()),
            "style" => metadata.style = Some(value.to_string()),
            "difficulty" => {
                let d: u8 = value
                    .parse()
                    .map_err(|_| err("dificultad inválida".into()))?;
                metadata.difficulty = Some(d.clamp(1, 8));
            }
            "arrangement" => {
                metadata.arrangement = match value {
                    "original" => Arrangement::Original,
                    "intermediate" => Arrangement::Intermediate,
                    "simplified" => Arrangement::Simplified,
                    other => return Err(err(format!("arreglo desconocido `{other}`"))),
                }
            }
            "tempo" => bpm = Some(value.parse().map_err(|_| err("tempo inválido".into()))?),
            "time" => {
                let (n, d) = value
                    .split_once('/')
                    .ok_or_else(|| err("compás inválido, se esperaba por ejemplo 3/4".into()))?;
                let n: u8 = n
                    .trim()
                    .parse()
                    .map_err(|_| err("compás inválido".into()))?;
                let d: u8 = d
                    .trim()
                    .parse()
                    .map_err(|_| err("compás inválido".into()))?;
                if n == 0 || !d.is_power_of_two() {
                    return Err(err("compás inválido".into()));
                }
                time = (n, d);
            }
            "source" => source = Some(value.to_string()),
            "license" => license = Some(value.to_string()),
            "section" => sections.push((value.to_string(), line_no)),
            "right" => right.push((value.to_string(), line_no)),
            "left" => left.push((value.to_string(), line_no)),
            other => return Err(err(format!("campo desconocido `{other}`"))),
        }
    }

    if metadata.title.is_empty() {
        return Err(NotationError::Missing("title"));
    }
    let bpm = bpm.ok_or(NotationError::Missing("tempo"))?;
    if !(10.0..=400.0).contains(&bpm) {
        return Err(NotationError::Missing("tempo"));
    }
    let bar_beats = Beats::new(4 * i64::from(time.0), i64::from(time.1));

    let mut notes = parse_voice(&right, Hand::Right, "derecha", bar_beats)?;
    notes.extend(parse_voice(&left, Hand::Left, "izquierda", bar_beats)?);

    let us_per_quarter = (60_000_000.0 / bpm).round() as u32;
    let mut piece = Piece::new(
        metadata,
        TempoMap::new(
            PPQ,
            vec![TempoChange {
                tick: 0,
                us_per_quarter,
            }],
        )
        .expect("PPQ y tempo válidos"),
    );
    piece.time_signatures = vec![TimeSignature {
        tick: 0,
        numerator: time.0,
        denominator: time.1,
    }];
    let bar_ticks = bar_beats.to_ticks();
    for (value, line) in sections {
        piece.sections.push(
            parse_section(&value, bar_ticks)
                .map_err(|message| NotationError::Syntax { line, message })?,
        );
    }
    if source.is_some() || license.is_some() {
        piece.provenance = Some(Provenance {
            source: source.unwrap_or_else(|| "sin indicar".into()),
            edition: None,
            license: license.unwrap_or_else(|| "sin indicar".into()),
            file_sha256: None,
            confidence: Confidence::A,
            reviewers: Vec::new(),
        });
    }
    piece.notes = notes;
    piece.sort_notes();
    Ok(piece)
}

/// Duración exacta en negras, como fracción.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Beats {
    num: i64,
    den: i64,
}

impl Beats {
    fn new(num: i64, den: i64) -> Self {
        let g = gcd(num.abs(), den.abs()).max(1);
        Beats {
            num: num / g,
            den: den / g,
        }
    }

    fn add(self, other: Beats) -> Beats {
        Beats::new(
            self.num * other.den + other.num * self.den,
            self.den * other.den,
        )
    }

    fn is_zero(self) -> bool {
        self.num == 0
    }

    fn to_ticks(self) -> u64 {
        (self.num * i64::from(PPQ) / self.den) as u64
    }

    fn parse(text: &str) -> Option<Beats> {
        if let Some((n, d)) = text.split_once('/') {
            let (n, d): (i64, i64) = (n.parse().ok()?, d.parse().ok()?);
            return (n > 0 && d > 0).then(|| Beats::new(n, d));
        }
        // Decimal: "1.5" = 3/2.
        let (int, frac) = text.split_once('.').unwrap_or((text, ""));
        let den = 10i64.checked_pow(frac.len() as u32)?;
        let num = format!("{int}{frac}").parse::<i64>().ok()?;
        (num > 0).then(|| Beats::new(num, den))
    }
}

impl std::fmt::Display for Beats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn parse_voice(
    lines: &[(String, usize)],
    hand: Hand,
    hand_name: &'static str,
    bar_beats: Beats,
) -> Result<Vec<Note>, NotationError> {
    let mut notes = Vec::new();
    let mut cursor = Beats::new(0, 1);
    let mut bar_len = Beats::new(0, 1);
    let mut bar = 1;
    let mut duration = Beats::new(1, 1);

    for (text, line) in lines {
        let err = |message: String| NotationError::Syntax {
            line: *line,
            message,
        };
        for token in text.split_whitespace() {
            if token == "|" {
                if bar_len != bar_beats {
                    return Err(NotationError::BarLength {
                        hand: hand_name,
                        bar,
                        found: bar_len.to_string(),
                        expected: bar_beats.to_string(),
                    });
                }
                bar += 1;
                bar_len = Beats::new(0, 1);
                continue;
            }
            let (pitches, dur) = match token.split_once(':') {
                Some((p, d)) => (
                    p,
                    Some(Beats::parse(d).ok_or_else(|| err(format!("duración inválida `{d}`")))?),
                ),
                None => (token, None),
            };
            if let Some(d) = dur {
                duration = d;
            }
            if pitches != "R" {
                for name in pitches.split('+') {
                    let pitch =
                        parse_pitch(name).ok_or_else(|| err(format!("nota inválida `{name}`")))?;
                    let start = cursor.to_ticks();
                    notes.push(Note {
                        pitch,
                        velocity: DEFAULT_VELOCITY,
                        start_tick: start,
                        end_tick: cursor.add(duration).to_ticks(),
                        hand,
                        finger: None,
                    });
                }
            }
            cursor = cursor.add(duration);
            bar_len = bar_len.add(duration);
        }
        // El fin de línea también cierra el compás.
        if !bar_len.is_zero() {
            if bar_len != bar_beats {
                return Err(NotationError::BarLength {
                    hand: hand_name,
                    bar,
                    found: bar_len.to_string(),
                    expected: bar_beats.to_string(),
                });
            }
            bar += 1;
            bar_len = Beats::new(0, 1);
        }
    }
    Ok(notes)
}

/// `C4` = 60. Acepta `#` y `b` como alteraciones.
pub fn parse_pitch(name: &str) -> Option<u8> {
    let mut chars = name.chars();
    let base = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let (accidental, octave) = if let Some(r) = rest.strip_prefix('#') {
        (1, r)
    } else if let Some(r) = rest.strip_prefix('b') {
        (-1, r)
    } else {
        (0, rest.as_str())
    };
    let octave: i32 = octave.parse().ok()?;
    let midi = (octave + 1) * 12 + base + accidental;
    u8::try_from(midi).ok().filter(|&p| p <= 127)
}

/// `Frase A 1-4`: nombre y rango de compases, contando desde 1.
fn parse_section(value: &str, bar_ticks: u64) -> Result<Section, String> {
    let (name, range) = value
        .rsplit_once(' ')
        .ok_or("sección inválida, se esperaba por ejemplo `Tema A 1-8`")?;
    let (from, to) = range.split_once('-').ok_or("rango de compases inválido")?;
    let from: u64 = from.parse().map_err(|_| "rango de compases inválido")?;
    let to: u64 = to.parse().map_err(|_| "rango de compases inválido")?;
    if from == 0 || to < from {
        return Err("rango de compases inválido".into());
    }
    Ok(Section {
        name: name.trim().to_string(),
        start_tick: (from - 1) * bar_ticks,
        end_tick: to * bar_ticks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "
        title: Prueba
        tempo: 120
        time: 3/4
        section: Todo 1-2
        right: C4 D4:0.5 E4 F4:1 | G4+B4:3
        left:  C3:3 | G2:1/3 A2 B2 R:2
    ";

    #[test]
    fn parses_pitches() {
        assert_eq!(parse_pitch("C4"), Some(60));
        assert_eq!(parse_pitch("A0"), Some(21));
        assert_eq!(parse_pitch("F#3"), Some(54));
        assert_eq!(parse_pitch("Bb2"), Some(46));
        assert_eq!(parse_pitch("H2"), None);
    }

    #[test]
    fn parses_durations_chords_and_rests() {
        let piece = parse_piece(SAMPLE).unwrap();
        assert_eq!(piece.metadata.title, "Prueba");
        assert_eq!(piece.tempo.bpm_at(0), 120.0);

        let right: Vec<_> = piece
            .notes
            .iter()
            .filter(|n| n.hand == Hand::Right)
            .collect();
        let spans: Vec<_> = right
            .iter()
            .map(|n| (n.pitch, n.start_tick, n.end_tick))
            .collect();
        assert_eq!(
            spans,
            vec![
                (60, 0, 480),
                (62, 480, 720),
                (64, 720, 960),
                (65, 960, 1440),
                (67, 1440, 2880),
                (71, 1440, 2880),
            ]
        );

        let left: Vec<_> = piece
            .notes
            .iter()
            .filter(|n| n.hand == Hand::Left)
            .collect();
        assert_eq!(left.len(), 4);
        assert_eq!(
            (left[2].start_tick, left[2].end_tick),
            (1440 + 160, 1440 + 320)
        );
        assert_eq!(
            (left[3].start_tick, left[3].end_tick),
            (1440 + 320, 1440 + 480)
        );

        assert_eq!(piece.sections[0].end_tick, 2880);
        assert_eq!(piece.bar_ticks(), vec![0, 1440]);
    }

    #[test]
    fn rejects_incomplete_bars() {
        let text = "title: x\ntempo: 100\ntime: 4/4\nright: C4 D4 E4 | C4:4";
        assert!(matches!(
            parse_piece(text),
            Err(NotationError::BarLength { bar: 1, .. })
        ));
    }

    #[test]
    fn reports_the_line_of_a_bad_note() {
        let text = "title: x\ntempo: 100\nright: C4 Q4:4";
        assert!(matches!(
            parse_piece(text),
            Err(NotationError::Syntax { line: 3, .. })
        ));
    }
}
