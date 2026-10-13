//! Pianos de samples que se descargan aparte: por ahora, el Yamaha C5 de
//! Salamander Grand Piano. El piano sintetizado sigue siendo el sonido por
//! defecto y no necesita descargas.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use clavium_audio::SampleBank;
use clavium_audio::sfz::{self, SfzError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// 8 capas de dinámica y colas de 8 segundos: unos 400 MB de descarga.
    Standard,
    /// Las 16 capas y colas de 20 segundos: unos 750 MB y más de 1 GB de memoria.
    Full,
}

impl Quality {
    pub fn key(self) -> &'static str {
        match self {
            Quality::Standard => "standard",
            Quality::Full => "full",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        [Quality::Standard, Quality::Full]
            .into_iter()
            .find(|q| q.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Quality::Standard => "Estándar (unos 400 MB)",
            Quality::Full => "Completa (unos 750 MB, más de 1 GB de memoria)",
        }
    }

    fn layers(self) -> usize {
        match self {
            Quality::Standard => 8,
            Quality::Full => 0, // todas
        }
    }

    fn max_seconds(self) -> f32 {
        match self {
            Quality::Standard => 8.0,
            Quality::Full => 20.0,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct SampledPiano {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Crédito obligatorio por la licencia.
    pub credit: &'static str,
    raw_base: &'static str,
    sfz_file: &'static str,
}

pub const SALAMANDER: SampledPiano = SampledPiano {
    id: "salamander",
    name: "Yamaha C5 (Salamander Grand Piano)",
    description: "Un piano de cola Yamaha C5 grabado nota por nota, con hasta 16 capas de dinámica.",
    credit: "Salamander Grand Piano V3 por Alexander Holm, afinación \"Retuned\" de Markus Fiedler, \
             versión SFZ de sfzinstruments (kinwie). Licencia Creative Commons Attribution 3.0. \
             https://github.com/sfzinstruments/SalamanderGrandPiano",
    raw_base: "https://raw.githubusercontent.com/sfzinstruments/SalamanderGrandPiano/master",
    sfz_file: "Salamander Grand Piano V3.sfz",
};

/// Marca que se escribe al terminar la descarga, con la calidad bajada.
const INSTALLED_MARK: &str = "instalado.txt";

pub fn dir(piano: &SampledPiano) -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Clavium")
        .join("instrumentos")
        .join(piano.id)
}

/// Calidad instalada, si el piano ya se descargó completo.
pub fn installed(piano: &SampledPiano) -> Option<Quality> {
    std::fs::read_to_string(dir(piano).join(INSTALLED_MARK))
        .ok()
        .and_then(|q| Quality::from_key(q.trim()))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    /// Archivos descargados de un total.
    Downloading { done: usize, total: usize },
    /// Samples cargados en memoria de un total.
    Loading { done: usize, total: usize },
}

pub enum Event {
    Progress(Progress),
    Ready(Arc<SampleBank>),
    Failed(String),
}

/// De dónde sale el piano de samples.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// Uno de los que Clavium sabe descargar.
    Download(&'static SampledPiano),
    /// Un `.sfz` que el usuario ya tiene en su computadora.
    Custom(PathBuf),
}

impl Source {
    pub fn name(&self) -> String {
        match self {
            Source::Download(piano) => piano.name.to_string(),
            Source::Custom(path) => path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Librería propia".to_string()),
        }
    }
}

/// Descarga (si hace falta) y carga el piano en un hilo aparte. Los avances
/// y el resultado llegan por el canal; `wake` pide redibujar la ventana.
pub fn prepare_in_background(
    source: Source,
    quality: Quality,
    download: bool,
    wake: impl Fn() + Send + 'static,
) -> Receiver<Event> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let send = |event: Event| {
            let _ = tx.send(event);
            wake();
        };
        let result = (|| {
            let instrument = match &source {
                Source::Download(piano) => {
                    let dir = dir(piano);
                    if download {
                        install(piano, quality, &dir, &tx, &wake)?;
                    }
                    sfz::load(&dir.join(piano.sfz_file)).map_err(|e| e.to_string())?
                }
                Source::Custom(path) => sfz::load(path).map_err(|e| e.to_string())?,
            }
            .with_velocity_layers(quality.layers());
            let bank = SampleBank::load(
                &source.name(),
                &instrument,
                quality.max_seconds(),
                &|done, total| {
                    let _ = tx.send(Event::Progress(Progress::Loading { done, total }));
                    if done % 16 == 0 {
                        wake();
                    }
                },
            )?;
            Ok::<_, String>(Arc::new(bank))
        })();
        send(match result {
            Ok(bank) => Event::Ready(bank),
            Err(e) => Event::Failed(e),
        });
    });
    rx
}

/// Baja el `.sfz`, los archivos que incluye y los samples de la calidad
/// elegida. Lo que ya está bajado no se vuelve a bajar.
fn install(
    piano: &SampledPiano,
    quality: Quality,
    dir: &Path,
    tx: &Sender<Event>,
    wake: &dyn Fn(),
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(dir.join(INSTALLED_MARK));

    // El .sfz y sus #include se bajan a medida que el lector los pide.
    let fetch_text = |rel: &str| -> Result<String, String> {
        let target = dir.join(rel);
        if let Ok(text) = std::fs::read_to_string(&target) {
            return Ok(text);
        }
        let bytes = download(&format!("{}/{}", piano.raw_base, encode(rel)))?;
        save(&target, &bytes)?;
        String::from_utf8(bytes).map_err(|e| e.to_string())
    };
    let main = fetch_text(piano.sfz_file)?;
    let read = |p: &Path| -> Result<String, SfzError> {
        let rel = p
            .strip_prefix(dir)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/");
        fetch_text(&rel).map_err(|e| SfzError::Io {
            path: rel,
            source: std::io::Error::other(e),
        })
    };
    let instrument = sfz::parse(&main, dir, &read)
        .map_err(|e| e.to_string())?
        .with_velocity_layers(quality.layers());

    let samples = instrument.sample_paths();
    for (i, rel) in samples.iter().enumerate() {
        let _ = tx.send(Event::Progress(Progress::Downloading {
            done: i,
            total: samples.len(),
        }));
        wake();
        let target = dir.join(rel);
        if target.metadata().is_ok_and(|m| m.len() > 0) {
            continue;
        }
        let bytes = download(&format!("{}/{}", piano.raw_base, encode(rel)))?;
        save(&target, &bytes)?;
    }
    std::fs::write(dir.join(INSTALLED_MARK), quality.key()).map_err(|e| e.to_string())?;
    Ok(())
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    ureq::get(url)
        .call()
        .map_err(|e| format!("no se pudo descargar {url}: {e}"))?
        .into_body()
        .into_with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("la descarga de {url} se cortó: {e}"))
}

/// Guarda en un archivo temporal y lo renombra al final, para que una
/// descarga cortada no deje un archivo a medias.
fn save(target: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let partial = target.with_extension("parcial");
    std::fs::write(&partial, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, target).map_err(|e| e.to_string())
}

/// Codifica una ruta para URL: espacios y `#` (que aparece en los nombres
/// de notas como `D#4v8.flac`).
fn encode(path: &str) -> String {
    path.chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '#' => "%23".to_string(),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_encoded_for_urls() {
        assert_eq!(encode("Samples/D#4v8.flac"), "Samples/D%234v8.flac");
        assert_eq!(
            encode("Salamander Grand Piano V3.sfz"),
            "Salamander%20Grand%20Piano%20V3.sfz"
        );
    }

    #[test]
    fn a_custom_sfz_library_loads_from_disk() {
        let dir = std::env::temp_dir().join(format!("clavium-sfz-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("samples")).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut wav = hound::WavWriter::create(dir.join("samples/c4.wav"), spec).unwrap();
        for i in 0..44_100 {
            let s = (i as f32 * 261.6 * std::f32::consts::TAU / 44_100.0).sin();
            wav.write_sample((s * 12_000.0) as i16).unwrap();
        }
        wav.finalize().unwrap();
        std::fs::write(
            dir.join("mi piano.sfz"),
            "<control> default_path=samples/
<region> sample=c4.wav lokey=21 hikey=108 pitch_keycenter=60
",
        )
        .unwrap();

        let rx = prepare_in_background(
            Source::Custom(dir.join("mi piano.sfz")),
            Quality::Standard,
            false,
            || {},
        );
        let bank = rx
            .into_iter()
            .find_map(|e| match e {
                Event::Ready(bank) => Some(Ok(bank)),
                Event::Failed(e) => Some(Err(e)),
                Event::Progress(_) => None,
            })
            .unwrap()
            .unwrap();
        assert_eq!(bank.name, "mi piano");
        assert_eq!(bank.memory_bytes(), 44_100 * 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quality_keys_round_trip() {
        for q in [Quality::Standard, Quality::Full] {
            assert_eq!(Quality::from_key(q.key()), Some(q));
        }
    }
}
