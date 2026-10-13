//! Lectura del repositorio del Mutopia Project
//! (https://github.com/MutopiaProject/MutopiaProject): cada obra es una
//! carpeta dentro de `ftp/` con su partitura en LilyPond, cuyo encabezado
//! dice el instrumento y la licencia.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SITE: &str = "https://www.mutopiaproject.org/ftp";

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Carpeta de la obra relativa a `ftp/`, con `/`: `BachJS/BWV846/bwv-846`.
    pub dir: String,
    /// Nombre de la carpeta, que también es el nombre de sus archivos.
    pub name: String,
    pub header: BTreeMap<String, String>,
}

impl Candidate {
    pub fn field(&self, key: &str) -> Option<&str> {
        // Mutopia usa `mutopiatitle` para el título "limpio"; si no está, `title`.
        self.header
            .get(&format!("mutopia{key}"))
            .or_else(|| self.header.get(key))
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    pub fn rdf_url(&self) -> String {
        format!("{SITE}/{}/{}.rdf", self.dir, self.name)
    }

    pub fn file_url(&self, file: &str) -> String {
        format!("{SITE}/{}/{file}", self.dir)
    }

    pub fn page_url(&self) -> String {
        format!("{SITE}/{}/", self.dir)
    }
}

/// Lee los campos `clave = "valor"` del encabezado de un archivo LilyPond.
pub fn parse_header(ly: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for line in ly.lines() {
        let line = line.trim();
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        let rest = rest.trim();
        let Some(value) = rest.strip_prefix('"').and_then(|r| r.split('"').next()) else {
            continue;
        };
        fields
            .entry(key.to_ascii_lowercase())
            .or_insert_with(|| value.trim().to_string());
    }
    fields
}

/// Si la licencia permite incluir la obra en Clavium: dominio público o
/// Creative Commons Attribution. Las ShareAlike quedan afuera porque
/// obligarían a liberar con la misma licencia el contenido pago.
pub fn license_allowed(license: &str) -> bool {
    let l = license.to_ascii_lowercase();
    l == "public domain"
        || (l.starts_with("creative commons attribution") && !l.contains("sharealike"))
}

/// Obras para un solo teclado que se tocan en piano: "Piano", "Harpsichord,
/// Piano", "Clavichord"… Quedan afuera las de voz, a cuatro manos, para
/// órgano (usa pedalera) o con otros instrumentos.
pub fn is_solo_piano(instrument: &str) -> bool {
    const KEYBOARDS: &[&str] = &[
        "piano",
        "harpsichord",
        "clavichord",
        "fortepiano",
        "pianoforte",
        "keyboard",
        "clavier",
        "spinet",
        "virginal",
    ];
    let lower = instrument.to_ascii_lowercase();
    let parts: Vec<&str> = lower
        .split([',', '/', '&'])
        .flat_map(|p| p.split(" or "))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    parts.contains(&"piano") && parts.iter().all(|p| KEYBOARDS.contains(p))
}

/// Recorre `ftp/` y devuelve las obras para piano solo con licencia admitida.
pub fn scan(repo: &Path) -> std::io::Result<Vec<Candidate>> {
    let ftp = repo.join("ftp");
    let mut by_dir: BTreeMap<PathBuf, Candidate> = BTreeMap::new();
    let mut stack = vec![ftp.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|x| x != "ly") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let header = parse_header(&text);
            if !header.contains_key("mutopiainstrument") {
                continue;
            }
            // Las obras de varios archivos los guardan en `nombre-lys/`.
            let parent = path.parent().unwrap();
            let piece_dir = if parent
                .file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with("-lys"))
            {
                parent.parent().unwrap().to_path_buf()
            } else {
                parent.to_path_buf()
            };
            if by_dir.contains_key(&piece_dir) {
                continue;
            }
            let rel = piece_dir
                .strip_prefix(&ftp)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let name = piece_dir
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            by_dir.insert(
                piece_dir,
                Candidate {
                    dir: rel,
                    name,
                    header,
                },
            );
        }
    }
    Ok(by_dir
        .into_values()
        .filter(|c| c.field("instrument").is_some_and(is_solo_piano))
        // Muchas partituras no declaran la licencia: esas se deciden con la
        // ficha del sitio. Las que la declaran y no se admite, ya se descartan.
        .filter(|c| c.field("license").is_none_or(license_allowed))
        .collect())
}

/// Campos que interesan de la ficha RDF de una obra en el sitio.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Rdf {
    pub licence: Option<String>,
    pub mid_file: Option<String>,
    pub maintainer: Option<String>,
}

pub fn parse_rdf(xml: &str) -> Rdf {
    let tag = |name: &str| -> Option<String> {
        let open = format!("<mp:{name}>");
        let close = format!("</mp:{name}>");
        let start = xml.find(&open)? + open.len();
        let end = xml[start..].find(&close)? + start;
        let value = xml[start..end].trim();
        (!value.is_empty()).then(|| value.to_string())
    };
    Rdf {
        licence: tag("licence"),
        mid_file: tag("midFile"),
        maintainer: tag("maintainer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LY: &str = r#"
\version "2.18.2"
\header {
  title = "Prelude"
  mutopiatitle = "Prelude in C major"
  composer = "Johann Sebastian Bach"
  mutopiacomposer = "BachJS"
  opus = "BWV 846"
  mutopiainstrument = "Piano"
  license = "Creative Commons Attribution 4.0"
  maintainer = "Alguien"
  footer = "Mutopia-2016/01/01-1"
}
"#;

    #[test]
    fn reads_the_lilypond_header() {
        let h = parse_header(LY);
        assert_eq!(h["title"], "Prelude");
        assert_eq!(h["mutopiainstrument"], "Piano");
        let c = Candidate {
            dir: "BachJS/BWV846/bwv-846".into(),
            name: "bwv-846".into(),
            header: h,
        };
        assert_eq!(c.field("title"), Some("Prelude in C major"));
        assert_eq!(c.field("composer"), Some("BachJS"));
        assert_eq!(c.header["composer"], "Johann Sebastian Bach");
        assert_eq!(
            c.rdf_url(),
            "https://www.mutopiaproject.org/ftp/BachJS/BWV846/bwv-846/bwv-846.rdf"
        );
    }

    #[test]
    fn filters_licenses_and_instruments() {
        assert!(license_allowed("Public Domain"));
        assert!(license_allowed("Creative Commons Attribution 4.0"));
        assert!(!license_allowed(
            "Creative Commons Attribution-ShareAlike 4.0"
        ));
        assert!(is_solo_piano("Piano"));
        assert!(is_solo_piano("piano"));
        assert!(is_solo_piano("Harpsichord, Piano"));
        assert!(is_solo_piano("Harpsichord, Piano, Clavichord"));
        assert!(!is_solo_piano("Harpsichord"));
        assert!(!is_solo_piano("Piano (4 hands)"));
        assert!(!is_solo_piano("Piano Duet"));
        assert!(!is_solo_piano("Voice, Piano"));
        assert!(!is_solo_piano("Voice and Piano"));
        assert!(!is_solo_piano("Organ, Piano"));
    }

    #[test]
    fn reads_the_rdf() {
        let rdf = parse_rdf(
            "<mp:licence>Public Domain</mp:licence><mp:midFile>giselle.mid</mp:midFile><mp:maintainer/>",
        );
        assert_eq!(rdf.licence.as_deref(), Some("Public Domain"));
        assert_eq!(rdf.mid_file.as_deref(), Some("giselle.mid"));
        assert_eq!(rdf.maintainer, None);
    }
}
