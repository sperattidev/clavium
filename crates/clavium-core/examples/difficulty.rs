//! Muestra el puntaje de dificultad y el nivel de cada `.clav` de una carpeta,
//! para revisar la calibración. Con un filtro, solo las obras cuyo título lo contiene.
//!
//! cargo run -p clavium-core --example difficulty -- <carpeta> [filtro]

use clavium_core::analysis::{difficulty_score, features, levels_by_rank};
use clavium_core::clav;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("uso: difficulty <carpeta> [filtro]");
    let filter = args.next().map(|f| f.to_lowercase());

    let mut rows = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("carpeta") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|x| x != "clav") {
            continue;
        }
        let piece = clav::load(&path).expect("clav válido");
        let f = features(&piece);
        let title = format!(
            "{} | {}",
            piece.metadata.composer.as_deref().unwrap_or("?"),
            piece.metadata.title
        );
        rows.push((difficulty_score(&f), title, f));
    }
    let scores: Vec<f64> = rows.iter().map(|r| r.0).collect();
    let levels = levels_by_rank(&scores);
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| rows[a].0.total_cmp(&rows[b].0));
    for i in order {
        let (score, title, f) = &rows[i];
        if filter
            .as_ref()
            .is_some_and(|f| !title.to_lowercase().contains(f))
        {
            continue;
        }
        println!(
            "nivel {} · {score:5.2} · {:5.1} nps (pico {:4.1}) · acorde {} · apertura {:2} · salto {:4.1} · simult. {:3.1} · {:4.1} min · {title}",
            levels[i],
            f.notes_per_second,
            f.peak_notes_per_second,
            f.chord_p90,
            f.span_p90,
            f.mean_jump,
            f.polyphony,
            f.minutes
        );
    }
}
