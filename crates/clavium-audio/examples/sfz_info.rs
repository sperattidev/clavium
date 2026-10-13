//! Resume un instrumento SFZ: regiones, capas de dinámica y samples.
//!
//! cargo run -p clavium-audio --example sfz_info -- ruta/al/instrumento.sfz [capas]

use clavium_audio::sfz::{self, Trigger};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("uso: sfz_info <archivo.sfz> [capas]");
    let layers: Option<usize> = args.next().and_then(|v| v.parse().ok());
    let mut instrument = sfz::load(std::path::Path::new(&path)).expect("SFZ válido");
    if let Some(n) = layers {
        instrument = instrument.with_velocity_layers(n);
    }
    let attack: Vec<_> = instrument
        .regions
        .iter()
        .filter(|r| r.trigger == Trigger::Attack)
        .collect();
    let release = instrument.regions.len() - attack.len();
    let mut layers: Vec<(u8, u8)> = attack.iter().map(|r| (r.lovel, r.hivel)).collect();
    layers.sort();
    layers.dedup();
    let low = attack.iter().map(|r| r.lokey).min().unwrap_or(0);
    let high = attack.iter().map(|r| r.hikey).max().unwrap_or(0);
    println!("Regiones de ataque: {}", attack.len());
    println!("Regiones de liberación: {release}");
    println!("Capas de dinámica: {} {layers:?}", layers.len());
    println!("Rango: {low}–{high}");
    println!("Samples distintos: {}", instrument.sample_paths().len());
    for r in attack.iter().take(3) {
        println!("  {r:?}");
    }

    // Con una tercera opción `cargar`, carga los samples presentes y mide la memoria.
    if args.next().as_deref() == Some("cargar") {
        for max_seconds in [8.0f32, 12.0, 60.0] {
            let start = std::time::Instant::now();
            let bank =
                clavium_audio::SampleBank::load("prueba", &instrument, max_seconds, &|_, _| {})
                    .expect("samples");
            println!(
                "Cola de {max_seconds} s: {:.0} MB en memoria, cargado en {:.1} s",
                bank.memory_bytes() as f64 / 1e6,
                start.elapsed().as_secs_f64()
            );
        }
    }
}
