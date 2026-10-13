//! Biblioteca ampliada: el paquete de catálogo que se descarga aparte y se
//! instala en la carpeta de datos del usuario.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use clavium_core::catalog::{CatalogIndex, INDEX_FILE, SCHEMA_VERSION};
use clavium_core::sha256_hex;

/// Dónde se publica el paquete. Se descarga solo si el usuario lo pide.
pub const DOWNLOAD_URL: &str =
    "https://github.com/sperattidev/clavium/releases/download/catalog-v1/clavium-catalog.zip";
/// Tope de tamaño de la descarga, por seguridad.
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

pub fn default_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Clavium")
        .join("catalog")
}

/// Lee el índice del catálogo instalado, si hay uno.
pub fn load_index(dir: &Path) -> Option<CatalogIndex> {
    let text = std::fs::read_to_string(dir.join(INDEX_FILE)).ok()?;
    let index: CatalogIndex = serde_json::from_str(&text).ok()?;
    (index.schema_version <= SCHEMA_VERSION).then_some(index)
}

/// Verifica un paquete `.zip` y lo instala en `dir`, reemplazando el anterior.
/// Devuelve cuántas obras tiene.
pub fn install_zip(bytes: &[u8], dir: &Path) -> Result<usize, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("no es un paquete válido: {e}"))?;
    let read =
        |archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str| -> Result<Vec<u8>, String> {
            let mut file = archive
                .by_name(name)
                .map_err(|_| format!("al paquete le falta {name}"))?;
            let mut data = Vec::new();
            file.read_to_end(&mut data).map_err(|e| e.to_string())?;
            Ok(data)
        };

    let index_bytes = read(&mut archive, INDEX_FILE)?;
    let index: CatalogIndex =
        serde_json::from_slice(&index_bytes).map_err(|e| format!("índice inválido: {e}"))?;
    if index.schema_version > SCHEMA_VERSION {
        return Err("el paquete es de una versión más nueva de Clavium".into());
    }

    // Se arma en una carpeta temporal y recién al final reemplaza a la anterior.
    let staging = dir.with_extension("nuevo");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(staging.join("pieces")).map_err(|e| e.to_string())?;
    for entry in &index.pieces {
        if !is_safe_path(&entry.file) {
            return Err(format!("ruta inválida en el paquete: {}", entry.file));
        }
        let data = read(&mut archive, &entry.file)?;
        if sha256_hex(&data) != entry.sha256 {
            return Err(format!("{} está dañado (no coincide su hash)", entry.title));
        }
        std::fs::write(staging.join(&entry.file), data).map_err(|e| e.to_string())?;
    }
    std::fs::write(staging.join(INDEX_FILE), &index_bytes).map_err(|e| e.to_string())?;

    let _ = std::fs::remove_dir_all(dir);
    std::fs::rename(&staging, dir).map_err(|e| e.to_string())?;
    Ok(index.pieces.len())
}

/// Solo rutas relativas simples, para que un paquete no escriba fuera de su carpeta.
fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Descarga e instala el paquete en un hilo aparte. El resultado llega por
/// el canal cuando termina.
pub fn download_in_background(
    dir: PathBuf,
    wake: impl Fn() + Send + 'static,
) -> Receiver<Result<usize, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = ureq::get(DOWNLOAD_URL)
            .call()
            .map_err(|e| format!("no se pudo descargar: {e}"))
            .and_then(|response| {
                response
                    .into_body()
                    .into_with_config()
                    .limit(MAX_DOWNLOAD_BYTES)
                    .read_to_vec()
                    .map_err(|e| format!("la descarga se cortó: {e}"))
            })
            .and_then(|bytes| install_zip(&bytes, &dir));
        let _ = tx.send(result);
        wake();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use clavium_core::catalog::CatalogEntry;
    use std::io::Write;

    fn pack(tamper: bool) -> Vec<u8> {
        let clav = b"no importa el contenido".to_vec();
        let index = CatalogIndex {
            schema_version: SCHEMA_VERSION,
            name: "Prueba".into(),
            pieces: vec![CatalogEntry {
                id: "x".into(),
                title: "X".into(),
                composer: None,
                opus: None,
                style: None,
                level: 1,
                duration_seconds: 10.0,
                notes: 20,
                license: "Public Domain".into(),
                attribution: None,
                source_url: "https://example.com".into(),
                file: "pieces/x.clav".into(),
                sha256: sha256_hex(&clav),
            }],
        };
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file(INDEX_FILE, options).unwrap();
            zip.write_all(serde_json::to_string(&index).unwrap().as_bytes())
                .unwrap();
            zip.start_file("pieces/x.clav", options).unwrap();
            let data = if tamper {
                b"otro contenido".to_vec()
            } else {
                clav
            };
            zip.write_all(&data).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("clavium-catalog-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn installs_a_valid_pack_and_reads_its_index() {
        let dir = temp_dir("ok");
        assert_eq!(install_zip(&pack(false), &dir), Ok(1));
        let index = load_index(&dir).unwrap();
        assert_eq!(index.pieces[0].title, "X");
        assert!(dir.join("pieces/x.clav").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_a_damaged_pack_and_keeps_the_previous_one() {
        let dir = temp_dir("bad");
        install_zip(&pack(false), &dir).unwrap();
        assert!(
            install_zip(&pack(true), &dir)
                .unwrap_err()
                .contains("dañado")
        );
        assert!(
            load_index(&dir).is_some(),
            "el catálogo anterior sigue instalado"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(dir.with_extension("nuevo"));
    }

    #[test]
    fn rejects_paths_outside_the_pack() {
        assert!(is_safe_path("pieces/x.clav"));
        assert!(!is_safe_path("../x.clav"));
        assert!(!is_safe_path("/etc/x"));
        assert!(!is_safe_path("C:/x"));
        assert!(!is_safe_path("pieces\\x"));
    }
}
