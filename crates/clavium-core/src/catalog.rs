//! Índice de un paquete de catálogo: la lista de piezas que se descargan
//! aparte de la app, con su procedencia y licencia.
//!
//! Un paquete es una carpeta (o un `.zip` con esa carpeta) con `index.json`
//! y un archivo `.clav` por pieza.

use serde::{Deserialize, Serialize};

pub const INDEX_FILE: &str = "index.json";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogIndex {
    pub schema_version: u32,
    /// Nombre del paquete, por ejemplo "Mutopia Project".
    pub name: String,
    pub pieces: Vec<CatalogEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer: Option<String>,
    /// Clave estable del compositor en la fuente (por ejemplo "BachJS"), para
    /// agrupar aunque el nombre esté escrito de formas distintas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// Nivel de dificultad estimado, del 1 al 8.
    pub level: u8,
    pub duration_seconds: f64,
    pub notes: usize,
    pub license: String,
    /// A quién hay que dar crédito (por ejemplo, quien transcribió la obra).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    pub source_url: String,
    /// Ruta del `.clav` dentro del paquete.
    pub file: String,
    /// SHA-256 del `.clav`, para verificar la descarga.
    pub sha256: String,
    /// Si alguien la revisó tocándola completa.
    #[serde(default)]
    pub reviewed: bool,
}

impl CatalogEntry {
    /// Texto de crédito para mostrar en la app.
    pub fn credit(&self) -> String {
        match &self.attribution {
            Some(who) => format!(
                "Transcripción: {who}. Licencia: {}. Fuente: {}",
                self.license, self.source_url
            ),
            None => format!("Licencia: {}. Fuente: {}", self.license, self.source_url),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_round_trips_through_json() {
        let index = CatalogIndex {
            schema_version: SCHEMA_VERSION,
            name: "Prueba".into(),
            pieces: vec![CatalogEntry {
                id: "mutopia-bachjs-bwv846".into(),
                title: "Preludio en Do".into(),
                composer: Some("J. S. Bach".into()),
                composer_key: Some("BachJS".into()),
                opus: Some("BWV 846".into()),
                style: None,
                level: 4,
                duration_seconds: 120.0,
                notes: 560,
                license: "Public Domain".into(),
                attribution: None,
                source_url: "https://www.mutopiaproject.org/".into(),
                file: "pieces/mutopia-bachjs-bwv846.clav".into(),
                sha256: "00".into(),
                reviewed: false,
            }],
        };
        let json = serde_json::to_string(&index).unwrap();
        assert_eq!(serde_json::from_str::<CatalogIndex>(&json).unwrap(), index);
        assert!(
            index.pieces[0]
                .credit()
                .starts_with("Licencia: Public Domain")
        );
    }
}
