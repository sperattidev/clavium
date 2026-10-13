//! Arma el paquete de catálogo de Clavium a partir del Mutopia Project.
//!
//! ```text
//! git clone --depth 1 https://github.com/MutopiaProject/MutopiaProject.git
//! cargo run -p clavium-catalog --release -- mutopia MutopiaProject salida --zip clavium-catalog.zip
//! ```
//!
//! Lee del repositorio qué obras son para piano solo y con licencia admitida
//! (dominio público o CC BY), descarga su ficha y su MIDI del sitio (con
//! caché, de a una por vez), las convierte y valida, y escribe `index.json`,
//! un `.clav` por obra y `REPORT.md` con lo que quedó afuera y por qué.

mod convert;
mod mutopia;

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clavium_core::catalog::{CatalogEntry, CatalogIndex, INDEX_FILE, SCHEMA_VERSION};
use clavium_core::{clav, sha256_hex};

const USER_AGENT: &str = "clavium-catalog/0.1 (+https://github.com/sperattidev/clavium)";
/// Pausa entre pedidos al sitio, para no cargarlo.
const REQUEST_SPACING: Duration = Duration::from_millis(400);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [command, repo, out, rest @ ..] = args.as_slice() else {
        eprintln!(
            "uso: clavium-catalog mutopia <repo de Mutopia> <carpeta de salida> [--cache <carpeta>] [--zip <archivo>] [--limit <n>]"
        );
        return ExitCode::FAILURE;
    };
    if command != "mutopia" {
        eprintln!("comando desconocido: {command}");
        return ExitCode::FAILURE;
    }
    let option = |name: &str| {
        rest.iter()
            .position(|a| a == name)
            .and_then(|i| rest.get(i + 1))
            .cloned()
    };
    let out = PathBuf::from(out);
    let cache = option("--cache").map_or_else(|| out.join("cache"), PathBuf::from);
    let limit: Option<usize> = option("--limit").and_then(|v| v.parse().ok());

    match run(
        Path::new(repo),
        &out,
        &cache,
        limit,
        option("--zip").map(PathBuf::from),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

struct Fetcher {
    cache: PathBuf,
    last: Option<Instant>,
}

impl Fetcher {
    fn get(&mut self, url: &str) -> Result<Vec<u8>, String> {
        let key: String = url
            .trim_start_matches("https://")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = self.cache.join(key);
        if let Ok(bytes) = std::fs::read(&path) {
            return Ok(bytes);
        }
        if let Some(last) = self.last {
            let wait = REQUEST_SPACING.saturating_sub(last.elapsed());
            std::thread::sleep(wait);
        }
        self.last = Some(Instant::now());
        let bytes = ureq::get(url)
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| format!("{url}: {e}"))?
            .into_body()
            .read_to_vec()
            .map_err(|e| format!("{url}: {e}"))?;
        std::fs::create_dir_all(&self.cache).map_err(|e| e.to_string())?;
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
        Ok(bytes)
    }
}

fn slug(text: &str) -> String {
    let mut s: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s.trim_matches('-').to_string()
}

fn run(
    repo: &Path,
    out: &Path,
    cache: &Path,
    limit: Option<usize>,
    zip: Option<PathBuf>,
) -> Result<(), String> {
    let mut candidates =
        mutopia::scan(repo).map_err(|e| format!("no se pudo leer {}: {e}", repo.display()))?;
    println!(
        "{} obras para piano solo con licencia admitida",
        candidates.len()
    );
    if let Some(limit) = limit {
        candidates.truncate(limit);
    }

    let pieces_dir = out.join("pieces");
    std::fs::create_dir_all(&pieces_dir).map_err(|e| e.to_string())?;
    let mut fetcher = Fetcher {
        cache: cache.to_path_buf(),
        last: None,
    };
    let mut entries = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();

    for (n, c) in candidates.iter().enumerate() {
        let title = c.field("title").unwrap_or(&c.name).to_string();
        print!("[{}/{}] {title} … ", n + 1, candidates.len());
        let _ = std::io::stdout().flush();

        let rdf = match fetcher.get(&c.rdf_url()) {
            Ok(bytes) => mutopia::parse_rdf(&String::from_utf8_lossy(&bytes)),
            Err(e) => {
                println!("sin ficha");
                skipped.push((title, format!("no se pudo descargar la ficha: {e}")));
                continue;
            }
        };
        // La licencia del sitio manda sobre la del archivo, por si cambió.
        let license = rdf
            .licence
            .clone()
            .or_else(|| c.field("license").map(str::to_string))
            .unwrap_or_default();
        if !mutopia::license_allowed(&license) {
            println!("licencia {license}");
            skipped.push((title, format!("licencia {license}")));
            continue;
        }
        let mid_file = rdf
            .mid_file
            .clone()
            .unwrap_or_else(|| format!("{}.mid", c.name));
        let bytes = match fetcher.get(&c.file_url(&mid_file)) {
            Ok(b) => b,
            Err(e) => {
                println!("sin MIDI");
                skipped.push((title, format!("no se pudo descargar el MIDI: {e}")));
                continue;
            }
        };

        // Las obras con varios movimientos traen un .zip con un MIDI por movimiento.
        let midis: Vec<(Option<usize>, Vec<u8>)> = if mid_file.ends_with(".zip") {
            match unzip_midis(&bytes) {
                Ok(list) if list.len() == 1 => vec![(None, list.into_iter().next().unwrap())],
                Ok(list) => list
                    .into_iter()
                    .enumerate()
                    .map(|(i, b)| (Some(i + 1), b))
                    .collect(),
                Err(e) => {
                    println!("zip inválido");
                    skipped.push((title, e));
                    continue;
                }
            }
        } else {
            vec![(None, bytes)]
        };

        let composer = c.header.get("composer").cloned();
        let maintainer = rdf
            .maintainer
            .clone()
            .or_else(|| c.header.get("maintainer").cloned());
        let mut converted = 0;
        for (movement, midi) in midis {
            let (id, piece_title) = match movement {
                Some(m) => (
                    format!("mutopia-{}-{m}", slug(&c.dir)),
                    format!("{title} ({m})"),
                ),
                None => (format!("mutopia-{}", slug(&c.dir)), title.clone()),
            };
            let source = convert::Source {
                title: &piece_title,
                composer: composer.as_deref(),
                style: c.field("style"),
                edition: c.field("source"),
                license: &license,
                url: &c.page_url(),
            };
            match convert::convert(&midi, &source) {
                Ok(piece) => {
                    let mut file = Vec::new();
                    clav::write(&piece, &mut file).map_err(|e| e.to_string())?;
                    let rel = format!("pieces/{id}.clav");
                    std::fs::write(out.join(&rel), &file).map_err(|e| e.to_string())?;
                    entries.push(CatalogEntry {
                        id,
                        title: piece_title,
                        composer: composer.clone(),
                        opus: c.header.get("opus").cloned().filter(|o| !o.is_empty()),
                        style: c.field("style").map(str::to_string),
                        level: piece.metadata.difficulty.unwrap_or(1),
                        duration_seconds: piece.duration_seconds(),
                        notes: piece.notes.len(),
                        license: license.clone(),
                        attribution: maintainer.clone(),
                        source_url: c.page_url(),
                        file: rel,
                        sha256: sha256_hex(&file),
                    });
                    converted += 1;
                }
                Err(e) => skipped.push((piece_title, e)),
            }
        }
        println!("{converted} pieza(s)");
    }

    entries.sort_by(|a, b| {
        (
            a.level,
            a.composer.as_deref().unwrap_or(""),
            a.title.as_str(),
        )
            .cmp(&(
                b.level,
                b.composer.as_deref().unwrap_or(""),
                b.title.as_str(),
            ))
    });
    let index = CatalogIndex {
        schema_version: SCHEMA_VERSION,
        name: "Mutopia Project".to_string(),
        pieces: entries,
    };
    let json = serde_json::to_string_pretty(&index).map_err(|e| e.to_string())?;
    std::fs::write(out.join(INDEX_FILE), json).map_err(|e| e.to_string())?;
    write_report(out, &index, &skipped)?;
    println!(
        "\n{} piezas en el catálogo, {} quedaron afuera (ver REPORT.md)",
        index.pieces.len(),
        skipped.len()
    );

    if let Some(zip) = zip {
        write_zip(out, &index, &zip)?;
        println!("Paquete: {}", zip.display());
    }
    Ok(())
}

fn unzip_midis(bytes: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name().map_err(|e| e.to_string())?.into_owned();
        if !name.to_ascii_lowercase().ends_with(".mid")
            && !name.to_ascii_lowercase().ends_with(".midi")
        {
            continue;
        }
        let mut data = Vec::new();
        file.read_to_end(&mut data).map_err(|e| e.to_string())?;
        files.push((name, data));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    if files.is_empty() {
        return Err("el zip no tiene MIDI".into());
    }
    Ok(files.into_iter().map(|(_, d)| d).collect())
}

fn write_report(
    out: &Path,
    index: &CatalogIndex,
    skipped: &[(String, String)],
) -> Result<(), String> {
    let mut report = String::from("# Catálogo Mutopia para Clavium\n\n");
    report.push_str(&format!("- Piezas incluidas: {}\n", index.pieces.len()));
    for level in 1..=8 {
        let count = index.pieces.iter().filter(|p| p.level == level).count();
        report.push_str(&format!("  - Nivel {level}: {count}\n"));
    }
    report.push_str(&format!(
        "- Quedaron afuera: {}\n\n## Afuera\n\n",
        skipped.len()
    ));
    for (title, reason) in skipped {
        report.push_str(&format!("- {title}: {reason}\n"));
    }
    std::fs::write(out.join("REPORT.md"), report).map_err(|e| e.to_string())
}

fn write_zip(out: &Path, index: &CatalogIndex, zip_path: &Path) -> Result<(), String> {
    let file = std::fs::File::create(zip_path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    let mut add = |name: &str, path: PathBuf| -> Result<(), String> {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        zip.start_file(name, options).map_err(|e| e.to_string())?;
        zip.write_all(&data).map_err(|e| e.to_string())
    };
    add(INDEX_FILE, out.join(INDEX_FILE))?;
    for piece in &index.pieces {
        add(&piece.file, out.join(&piece.file))?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}
