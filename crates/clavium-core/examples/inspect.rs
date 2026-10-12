//! Muestra cómo queda un archivo MIDI dentro de Clavium.
//!
//! cargo run -p clavium-core --example inspect -- ruta/al/archivo.mid [salida.clav]

use std::process::ExitCode;

use clavium_core::{clav, midi, validate};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("uso: inspect <archivo.mid> [salida.clav]");
        return ExitCode::FAILURE;
    };

    let bytes = match std::fs::read(&input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("no se pudo leer {input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let piece = match midi::import_smf(&bytes) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    println!("Título:     {}", piece.metadata.title);
    println!("PPQ:        {}", piece.tempo.ppq());
    println!("Tempos:     {}", piece.tempo.changes().len());
    println!("Duración:   {:.2} s", piece.duration_seconds());
    println!("Compases:   {}", piece.bar_ticks().len());
    println!("Notas:      {}", piece.notes.len());
    println!("Pedal:      {} tramos", piece.sustain.len());
    println!();
    println!(
        "{:>9} {:>9} {:>5} {:>4}  mano",
        "inicio", "fin", "nota", "vel"
    );
    for n in piece.notes.iter().take(20) {
        let (start, end) = piece.note_seconds(n);
        println!(
            "{start:>8.3}s {end:>8.3}s {:>5} {:>4}  {:?}",
            n.pitch, n.velocity, n.hand
        );
    }
    if piece.notes.len() > 20 {
        println!("… y {} notas más", piece.notes.len() - 20);
    }

    let issues = validate::validate(&piece);
    println!();
    println!("Validación: {} problemas", issues.len());
    for issue in issues.iter().take(10) {
        println!("  {issue}");
    }

    if let Some(output) = args.next() {
        if let Err(e) = clav::save(&piece, &output) {
            eprintln!("no se pudo guardar {output}: {e}");
            return ExitCode::FAILURE;
        }
        println!();
        println!("Guardado en {output}");
    }
    ExitCode::SUCCESS
}
