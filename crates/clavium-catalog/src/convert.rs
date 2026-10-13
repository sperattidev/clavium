//! Conversión de un MIDI de partitura en una pieza de Clavium lista para el
//! catálogo: metadatos, procedencia, dificultad y secciones para practicar.

use clavium_core::analysis::{auto_sections, estimate_level};
use clavium_core::midi::import_smf;
use clavium_core::validate::{IssueKind, Severity, validate};
use clavium_core::{Arrangement, Confidence, Metadata, Piece, Provenance};

/// Menos notas que esto no es una obra para practicar (o el MIDI está vacío).
const MIN_NOTES: usize = 16;

pub struct Source<'a> {
    pub title: &'a str,
    pub composer: Option<&'a str>,
    pub style: Option<&'a str>,
    pub edition: Option<&'a str>,
    pub license: &'a str,
    pub url: &'a str,
}

pub fn convert(midi: &[u8], source: &Source<'_>) -> Result<Piece, String> {
    let mut piece = import_smf(midi).map_err(|e| e.to_string())?;
    // Notas de duración cero (adornos que el MIDI no dibuja) no se pueden tocar a la vista.
    piece.notes.retain(|n| n.end_tick > n.start_tick);
    if piece.notes.len() < MIN_NOTES {
        return Err(format!("solo tiene {} notas", piece.notes.len()));
    }

    let file_sha256 = piece
        .provenance
        .as_ref()
        .and_then(|p| p.file_sha256.clone());
    piece.metadata = Metadata {
        title: source.title.to_string(),
        composer: source.composer.map(str::to_string),
        arranger: None,
        style: source.style.map(str::to_string),
        difficulty: Some(estimate_level(&piece)),
        arrangement: Arrangement::Original,
    };
    piece.provenance = Some(Provenance {
        source: format!("Mutopia Project, {}", source.url),
        edition: source.edition.map(str::to_string),
        license: source.license.to_string(),
        file_sha256,
        confidence: Confidence::A,
        reviewers: Vec::new(),
    });
    piece.sections = auto_sections(&piece);

    let errors: Vec<String> = validate(&piece)
        .into_iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| match i.kind {
            IssueKind::OutOfRange { pitch } => format!("nota {pitch} fuera del piano"),
            _ => i.to_string(),
        })
        .collect();
    if let Some(first) = errors.first() {
        return Err(format!(
            "{} errores de validación, el primero: {first}",
            errors.len()
        ));
    }
    Ok(piece)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clavium_core::notation::parse_piece;

    #[test]
    fn sections_cover_the_whole_piece_in_groups_of_eight_bars() {
        let bars = "C4:4 | ".repeat(20);
        let piece = parse_piece(&format!(
            "title: x\ntempo: 60\nright: {bars}\nleft: {}",
            "C3:4 | ".repeat(20)
        ))
        .unwrap();
        let sections = auto_sections(&piece);
        let names: Vec<_> = sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Compases 1–8", "Compases 9–16", "Compases 17–20"]
        );
        assert_eq!(sections[0].start_tick, 0);
        assert_eq!(sections[1].start_tick, sections[0].end_tick);
        assert_eq!(sections.last().unwrap().end_tick, piece.end_tick());
    }
}
