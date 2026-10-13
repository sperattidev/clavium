//! Importación de MusicXML (`.musicxml`, `.xml` y el comprimido `.mxl`), el
//! formato que exportan MuseScore, Finale, Sibelius y los programas de
//! reconocimiento óptico de partituras.
//!
//! A diferencia de un MIDI, la partitura dice en qué pentagrama va cada nota:
//! en una parte de piano, el pentagrama de arriba es la mano derecha y el de
//! abajo la izquierda. Se lee la partitura tal como está escrita: las
//! repeticiones se tocan una sola vez.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use crate::midi::{
    DEFAULT_HAND_SPLIT, MidiPart, PartChoice, PartHand, PartId, assign_hands_by_part,
    hand_by_split, resolve_overlaps,
};
use crate::model::{
    Confidence, Hand, Metadata, Note, Piece, Provenance, TimeSignature, sha256_hex,
};
use crate::tempo::{TempoChange, TempoMap};

/// Resolución de las piezas importadas desde MusicXML.
const PPQ: u64 = 480;
const DEFAULT_VELOCITY: u8 = 80;

#[derive(Debug, thiserror::Error)]
pub enum MusicXmlError {
    #[error("el archivo no es un MusicXML válido: {0}")]
    Xml(String),
    #[error("el archivo .mxl está dañado: {0}")]
    Zip(String),
    #[error("{0}")]
    Unsupported(&'static str),
}

struct ScorePart {
    name: String,
    program: Option<u8>,
    /// (pentagrama, nota).
    notes: Vec<(u8, Note)>,
}

struct Score {
    title: Option<String>,
    tempo_changes: Vec<TempoChange>,
    time_signatures: Vec<TimeSignature>,
    parts: Vec<ScorePart>,
}

/// Texto XML del archivo: directo, o el de adentro de un `.mxl`.
fn xml_text(bytes: &[u8]) -> Result<String, MusicXmlError> {
    if bytes.starts_with(b"PK") {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| MusicXmlError::Zip(e.to_string()))?;
        let read = |zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
                    name: &str|
         -> Result<String, MusicXmlError> {
            let mut file = zip
                .by_name(name)
                .map_err(|e| MusicXmlError::Zip(e.to_string()))?;
            let mut text = String::new();
            file.read_to_string(&mut text)
                .map_err(|e| MusicXmlError::Zip(e.to_string()))?;
            Ok(text)
        };
        // container.xml dice cuál es la partitura principal.
        let container = read(&mut zip, "META-INF/container.xml")?;
        let doc = roxmltree::Document::parse(&container)
            .map_err(|e| MusicXmlError::Xml(e.to_string()))?;
        let root = doc
            .descendants()
            .find(|n| n.has_tag_name("rootfile"))
            .and_then(|n| n.attribute("full-path"))
            .ok_or(MusicXmlError::Zip("falta la partitura principal".into()))?
            .to_string();
        return read(&mut zip, &root);
    }
    let text = String::from_utf8_lossy(bytes);
    Ok(text.trim_start_matches('\u{feff}').to_string())
}

fn child_text<'a>(node: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|c| c.has_tag_name(name))
        .and_then(|c| c.text())
        .map(str::trim)
}

fn parse(bytes: &[u8]) -> Result<Score, MusicXmlError> {
    let text = xml_text(bytes)?;
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(&text, options)
        .map_err(|e| MusicXmlError::Xml(e.to_string()))?;
    let root = doc.root_element();
    if root.has_tag_name("score-timewise") {
        return Err(MusicXmlError::Unsupported(
            "las partituras MusicXML por tiempo (timewise) no están soportadas; exportala por partes (partwise)",
        ));
    }
    if !root.has_tag_name("score-partwise") {
        return Err(MusicXmlError::Xml("no es una partitura MusicXML".into()));
    }

    let title = root
        .descendants()
        .find(|n| n.has_tag_name("work-title"))
        .or_else(|| root.children().find(|n| n.has_tag_name("movement-title")))
        .and_then(|n| n.text())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    // Nombres e instrumentos de la lista de partes.
    let mut part_info: HashMap<&str, (String, Option<u8>)> = HashMap::new();
    for sp in root.descendants().filter(|n| n.has_tag_name("score-part")) {
        let Some(id) = sp.attribute("id") else {
            continue;
        };
        let name = child_text(sp, "part-name").unwrap_or("").to_string();
        let program = sp
            .descendants()
            .find(|n| n.has_tag_name("midi-program"))
            .and_then(|n| n.text())
            .and_then(|t| t.trim().parse::<u8>().ok())
            .map(|p| p.saturating_sub(1));
        part_info.insert(id, (name, program));
    }

    let mut score = Score {
        title,
        tempo_changes: Vec::new(),
        time_signatures: Vec::new(),
        parts: Vec::new(),
    };
    for (index, part) in root
        .children()
        .filter(|n| n.has_tag_name("part"))
        .enumerate()
    {
        let (name, program) = part
            .attribute("id")
            .and_then(|id| part_info.get(id).cloned())
            .unwrap_or_default();
        // El tempo y los compases se toman de la primera parte.
        let first = index == 0;
        let mut notes: Vec<(u8, Note)> = Vec::new();
        let mut divisions: u64 = 1;
        let mut cursor: u64 = 0;
        let mut last_start: u64 = 0;
        // Notas ligadas que siguen: (pentagrama, altura) -> índice en `notes`.
        let mut tied: HashMap<(u8, u8), usize> = HashMap::new();
        let ticks = |d: u64, divisions: u64| d * PPQ / divisions.max(1);

        for measure in part.children().filter(|n| n.has_tag_name("measure")) {
            for el in measure.children().filter(|n| n.is_element()) {
                match el.tag_name().name() {
                    "attributes" => {
                        if let Some(d) =
                            child_text(el, "divisions").and_then(|t| t.parse::<u64>().ok())
                        {
                            divisions = d.max(1);
                        }
                        if first
                            && let Some(time) = el.children().find(|c| c.has_tag_name("time"))
                            && let (Some(beats), Some(beat_type)) = (
                                child_text(time, "beats").and_then(|t| t.parse::<u8>().ok()),
                                child_text(time, "beat-type").and_then(|t| t.parse::<u8>().ok()),
                            )
                            && beats > 0
                            && beat_type.is_power_of_two()
                        {
                            score.time_signatures.push(TimeSignature {
                                tick: cursor,
                                numerator: beats,
                                denominator: beat_type,
                            });
                        }
                    }
                    "direction" | "sound" if first => {
                        let tempo = if el.has_tag_name("sound") {
                            el.attribute("tempo")
                        } else {
                            el.descendants()
                                .find(|n| n.has_tag_name("sound"))
                                .and_then(|s| s.attribute("tempo"))
                        };
                        if let Some(bpm) = tempo
                            .and_then(|t| t.parse::<f64>().ok())
                            .filter(|b| *b > 0.0)
                        {
                            score.tempo_changes.push(TempoChange {
                                tick: cursor,
                                us_per_quarter: (60_000_000.0 / bpm).round() as u32,
                            });
                        }
                    }
                    "backup" => {
                        let d = child_text(el, "duration")
                            .and_then(|t| t.parse::<u64>().ok())
                            .unwrap_or(0);
                        cursor = cursor.saturating_sub(ticks(d, divisions));
                    }
                    "forward" => {
                        let d = child_text(el, "duration")
                            .and_then(|t| t.parse::<u64>().ok())
                            .unwrap_or(0);
                        cursor += ticks(d, divisions);
                    }
                    "note" => {
                        let has = |name: &str| el.children().any(|c| c.has_tag_name(name));
                        // Adornos y notas de referencia no se tocan a tiempo.
                        if has("grace") || has("cue") {
                            continue;
                        }
                        let d = child_text(el, "duration")
                            .and_then(|t| t.parse::<u64>().ok())
                            .unwrap_or(0);
                        let length = ticks(d, divisions);
                        let start = if has("chord") { last_start } else { cursor };
                        if !has("chord") {
                            last_start = cursor;
                            cursor += length;
                        }
                        let Some(pitch) = el.children().find(|c| c.has_tag_name("pitch")) else {
                            continue; // silencio o percusión sin altura
                        };
                        let step = match child_text(pitch, "step") {
                            Some("C") => 0,
                            Some("D") => 2,
                            Some("E") => 4,
                            Some("F") => 5,
                            Some("G") => 7,
                            Some("A") => 9,
                            Some("B") => 11,
                            _ => continue,
                        };
                        let alter = child_text(pitch, "alter")
                            .and_then(|t| t.parse::<f32>().ok())
                            .unwrap_or(0.0)
                            .round() as i32;
                        let Some(octave) =
                            child_text(pitch, "octave").and_then(|t| t.parse::<i32>().ok())
                        else {
                            continue;
                        };
                        let Ok(midi) = u8::try_from((octave + 1) * 12 + step + alter) else {
                            continue;
                        };
                        if midi > 127 {
                            continue;
                        }
                        let staff = child_text(el, "staff")
                            .and_then(|t| t.parse::<u8>().ok())
                            .unwrap_or(1);
                        let tie = |kind: &str| {
                            el.children()
                                .any(|c| c.has_tag_name("tie") && c.attribute("type") == Some(kind))
                        };
                        let end = start + length;
                        if tie("stop")
                            && let Some(&i) = tied.get(&(staff, midi))
                        {
                            // Continúa la nota ligada anterior.
                            notes[i].1.end_tick = notes[i].1.end_tick.max(end);
                            if !tie("start") {
                                tied.remove(&(staff, midi));
                            }
                            continue;
                        }
                        notes.push((
                            staff,
                            Note {
                                pitch: midi,
                                velocity: DEFAULT_VELOCITY,
                                start_tick: start,
                                end_tick: end,
                                hand: Hand::Unknown,
                                finger: None,
                            },
                        ));
                        if tie("start") {
                            tied.insert((staff, midi), notes.len() - 1);
                        }
                    }
                    _ => {}
                }
            }
        }
        score.parts.push(ScorePart {
            name,
            program,
            notes,
        });
    }
    Ok(score)
}

/// Las partes de la partitura que tienen notas.
pub fn inspect_parts(bytes: &[u8]) -> Result<Vec<MidiPart>, MusicXmlError> {
    let score = parse(bytes)?;
    Ok(score
        .parts
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.notes.is_empty())
        .map(|(i, p)| MidiPart {
            track: i,
            channel: 0,
            name: p.name.clone(),
            program: p.program,
            drums: false,
            notes: p.notes.len(),
            lowest: p.notes.iter().map(|(_, n)| n.pitch).min().unwrap_or(60),
            highest: p.notes.iter().map(|(_, n)| n.pitch).max().unwrap_or(60),
        })
        .collect())
}

/// Importa las partes elegidas. En automático, una parte con dos pentagramas
/// usa el de arriba para la mano derecha y los demás para la izquierda; las
/// de un solo pentagrama se reparten entre las manos como en un MIDI.
pub fn import_parts(bytes: &[u8], choices: &[PartChoice]) -> Result<Piece, MusicXmlError> {
    let score = parse(bytes)?;
    let mut auto: Vec<(PartId, Note)> = Vec::new();
    let mut fixed: Vec<Note> = Vec::new();
    for (i, part) in score.parts.into_iter().enumerate() {
        let id: PartId = (i, 0);
        let Some(choice) = choices.iter().find(|c| c.part == id) else {
            continue;
        };
        let staves = part.notes.iter().map(|(s, _)| *s).max().unwrap_or(1);
        for (staff, mut n) in part.notes {
            n.hand = match choice.hand {
                PartHand::Right => Hand::Right,
                PartHand::Left => Hand::Left,
                PartHand::Split => hand_by_split(n.pitch, DEFAULT_HAND_SPLIT),
                PartHand::Auto if staves > 1 => {
                    if staff <= 1 {
                        Hand::Right
                    } else {
                        Hand::Left
                    }
                }
                PartHand::Auto => {
                    auto.push((id, n));
                    continue;
                }
            };
            fixed.push(n);
        }
    }
    assign_hands_by_part(&mut auto);

    let mut piece = Piece::new(
        Metadata::titled(score.title.unwrap_or_else(|| "Sin título".to_string())),
        TempoMap::new(PPQ as u16, score.tempo_changes)
            .map_err(|e| MusicXmlError::Xml(e.to_string()))?,
    );
    piece.provenance = Some(Provenance {
        source: "importado por el usuario (MusicXML)".to_string(),
        edition: None,
        license: "desconocida".to_string(),
        file_sha256: Some(sha256_hex(bytes)),
        confidence: Confidence::D,
        reviewers: Vec::new(),
    });
    let mut signatures = score.time_signatures;
    signatures.dedup_by_key(|s| (s.tick, s.numerator, s.denominator));
    piece.time_signatures = signatures;
    piece.notes = auto.into_iter().map(|(_, n)| n).chain(fixed).collect();
    resolve_overlaps(&mut piece.notes);
    piece.sort_notes();
    Ok(piece)
}

/// Si los bytes parecen MusicXML (texto XML o `.mxl` comprimido).
pub fn looks_like_musicxml(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    bytes.starts_with(b"PK") || head.contains("<score-partwise") || head.contains("<!DOCTYPE score")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Dos compases de 3/4 para piano en dos pentagramas, con acorde, ligadura,
    /// silencio y `backup` para pasar al pentagrama de abajo.
    const PIANO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE score-partwise PUBLIC "-//Recordare//DTD MusicXML 4.0 Partwise//EN" "http://www.musicxml.org/dtds/partwise.dtd">
<score-partwise version="4.0">
  <work><work-title>Valsecito</work-title></work>
  <part-list>
    <score-part id="P1"><part-name>Piano</part-name>
      <midi-instrument id="P1-I1"><midi-program>1</midi-program></midi-instrument>
    </score-part>
  </part-list>
  <part id="P1">
    <measure number="1">
      <attributes><divisions>2</divisions><time><beats>3</beats><beat-type>4</beat-type></time><staves>2</staves></attributes>
      <direction><sound tempo="90"/></direction>
      <note><pitch><step>E</step><octave>5</octave></pitch><duration>2</duration><staff>1</staff></note>
      <note><chord/><pitch><step>C</step><octave>5</octave></pitch><duration>2</duration><staff>1</staff></note>
      <note><pitch><step>F</step><alter>1</alter><octave>4</octave></pitch><duration>4</duration><tie type="start"/><staff>1</staff></note>
      <backup><duration>6</duration></backup>
      <note><pitch><step>C</step><octave>3</octave></pitch><duration>6</duration><staff>2</staff></note>
    </measure>
    <measure number="2">
      <note><pitch><step>F</step><alter>1</alter><octave>4</octave></pitch><duration>2</duration><tie type="stop"/><staff>1</staff></note>
      <note><rest/><duration>4</duration><staff>1</staff></note>
      <backup><duration>6</duration></backup>
      <note><grace/><pitch><step>D</step><octave>3</octave></pitch><staff>2</staff></note>
      <note><pitch><step>G</step><octave>2</octave></pitch><duration>6</duration><staff>2</staff></note>
    </measure>
  </part>
</score-partwise>"#;

    fn import(bytes: &[u8]) -> Piece {
        let parts = inspect_parts(bytes).unwrap();
        let choices: Vec<PartChoice> = parts
            .iter()
            .map(|p| PartChoice {
                part: p.id(),
                hand: PartHand::Auto,
            })
            .collect();
        import_parts(bytes, &choices).unwrap()
    }

    #[test]
    fn reads_notes_chords_ties_and_staves() {
        let piece = import(PIANO.as_bytes());
        assert_eq!(piece.metadata.title, "Valsecito");
        assert!((piece.tempo.bpm_at(0) - 90.0).abs() < 0.01);
        assert_eq!(piece.time_signatures[0].numerator, 3);

        let notes: Vec<(u8, u64, u64, Hand)> = piece
            .notes
            .iter()
            .map(|n| (n.pitch, n.start_tick, n.end_tick, n.hand))
            .collect();
        assert_eq!(
            notes,
            vec![
                (48, 0, 1440, Hand::Left),    // Do3, blanca con puntillo abajo
                (72, 0, 480, Hand::Right),    // Do5 del acorde
                (76, 0, 480, Hand::Right),    // Mi5 del acorde
                (66, 480, 1920, Hand::Right), // Fa#4 ligada al compás 2
                (43, 1440, 2880, Hand::Left), // Sol2; el adorno no se cuenta
            ]
        );
        assert_eq!(piece.bar_ticks(), vec![0, 1440]);
    }

    #[test]
    fn parts_are_listed_with_their_instrument() {
        let parts = inspect_parts(PIANO.as_bytes()).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].name, "Piano");
        assert!(parts[0].is_piano());
        assert_eq!(parts[0].notes, 5);
    }

    #[test]
    fn reads_compressed_mxl_files() {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("META-INF/container.xml", options).unwrap();
            zip.write_all(
                br#"<container><rootfiles><rootfile full-path="score/valsecito.musicxml"/></rootfiles></container>"#,
            )
            .unwrap();
            zip.start_file("score/valsecito.musicxml", options).unwrap();
            zip.write_all(PIANO.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        assert!(looks_like_musicxml(&buf));
        assert_eq!(import(&buf).notes.len(), 5);
    }

    #[test]
    fn timewise_scores_are_rejected_with_a_clear_message() {
        let err = import_parts(b"<score-timewise></score-timewise>", &[]).unwrap_err();
        assert!(err.to_string().contains("partwise"));
    }
}
