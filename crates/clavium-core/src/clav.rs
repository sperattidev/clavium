//! Formato de archivo `.clav`: JSON comprimido con gzip, con número de versión
//! de esquema para poder migrar archivos sin romper la biblioteca.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};

use crate::model::Piece;

/// Versión del esquema que escribe y lee esta versión de Clavium.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ClavError {
    #[error("error de lectura o escritura: {0}")]
    Io(#[from] std::io::Error),
    #[error("archivo .clav inválido: {0}")]
    Json(#[from] serde_json::Error),
    #[error("versión de esquema {found} no soportada (esta versión lee hasta la {supported})")]
    UnsupportedVersion { found: u32, supported: u32 },
}

#[derive(Serialize)]
struct ClavFileOut<'a> {
    schema_version: u32,
    piece: &'a Piece,
}

#[derive(Deserialize)]
struct ClavFileIn {
    schema_version: u32,
    piece: serde_json::Value,
}

pub fn write<W: Write>(piece: &Piece, writer: W) -> Result<(), ClavError> {
    let mut encoder = GzEncoder::new(writer, Compression::default());
    serde_json::to_writer(
        &mut encoder,
        &ClavFileOut {
            schema_version: SCHEMA_VERSION,
            piece,
        },
    )?;
    encoder.finish()?;
    Ok(())
}

pub fn read<R: Read>(reader: R) -> Result<Piece, ClavError> {
    let file: ClavFileIn = serde_json::from_reader(GzDecoder::new(reader))?;
    if file.schema_version == 0 || file.schema_version > SCHEMA_VERSION {
        return Err(ClavError::UnsupportedVersion {
            found: file.schema_version,
            supported: SCHEMA_VERSION,
        });
    }
    // Cuando exista la versión 2, acá se migra `file.piece` antes de leerlo.
    Ok(serde_json::from_value(file.piece)?)
}

pub fn save(piece: &Piece, path: impl AsRef<Path>) -> Result<(), ClavError> {
    write(piece, BufWriter::new(File::create(path)?))
}

pub fn load(path: impl AsRef<Path>) -> Result<Piece, ClavError> {
    read(BufReader::new(File::open(path)?))
}
