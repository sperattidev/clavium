//! Lector de instrumentos SFZ (el formato de texto de las librerías de
//! samples libres como Salamander Grand Piano o VCSL).
//!
//! Soporta lo que usan las librerías de piano: `#define`, `#include`,
//! encabezados `<control>`, `<global>`, `<master>`, `<group>` y `<region>`
//! con herencia, rangos de tecla y dinámica, afinación, volumen, envolvente,
//! samples de liberación con atenuación, alternancia (`seq_*`), teclas de
//! cambio (`sw_*`) y valores por defecto de controladores (`set_cc*`). Las
//! regiones que dependen de un controlador (ruido de pedal, por ejemplo) se
//! omiten.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Attack,
    Release,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    /// Ruta del sample relativa a la carpeta del `.sfz`, con `/`.
    pub sample: String,
    pub lokey: u8,
    pub hikey: u8,
    pub pitch_keycenter: u8,
    pub lovel: u8,
    pub hivel: u8,
    pub trigger: Trigger,
    /// Afinación en cents, sumando `tune` y `transpose`.
    pub tune_cents: f32,
    /// Cents por semitono entre la tecla y la nota del sample (100 = normal).
    pub pitch_keytrack: f32,
    pub volume_db: f32,
    /// Cuánto depende el volumen de la dinámica, de 0 a 1.
    pub amp_veltrack: f32,
    pub attack: f32,
    pub release: f32,
    /// Atenuación de los samples de liberación por segundo de nota sostenida.
    pub rt_decay_db: f32,
    /// Muestras a saltear al principio del sample.
    pub offset: u32,
    pub seq_length: u8,
    pub seq_position: u8,
}

impl Region {
    pub fn matches(&self, key: u8, vel: u8) -> bool {
        (self.lokey..=self.hikey).contains(&key) && (self.lovel..=self.hivel).contains(&vel)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SfzInstrument {
    pub regions: Vec<Region>,
    /// Carpeta del `.sfz`, base de las rutas de los samples.
    pub base_dir: PathBuf,
}

impl SfzInstrument {
    /// Samples distintos que usa el instrumento, en orden.
    pub fn sample_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.regions.iter().map(|r| r.sample.clone()).collect();
        paths.sort();
        paths.dedup();
        paths
    }

    /// Se queda con `layers` capas de dinámica repartidas de la más suave a la
    /// más fuerte (las regiones de liberación se mantienen). Sirve para una
    /// versión liviana que ocupa menos disco y memoria.
    pub fn with_velocity_layers(&self, layers: usize) -> SfzInstrument {
        let mut ranges: Vec<(u8, u8)> = self
            .regions
            .iter()
            .filter(|r| r.trigger == Trigger::Attack)
            .map(|r| (r.lovel, r.hivel))
            .collect();
        ranges.sort();
        ranges.dedup();
        if layers == 0 || ranges.len() <= layers {
            return self.clone();
        }
        let keep: Vec<(u8, u8)> = (0..layers)
            .map(|i| ranges[(i * (ranges.len() - 1) + (layers - 1) / 2) / (layers - 1).max(1)])
            .collect();
        SfzInstrument {
            regions: self
                .regions
                .iter()
                .filter(|r| r.trigger == Trigger::Release || keep.contains(&(r.lovel, r.hivel)))
                .cloned()
                .collect(),
            base_dir: self.base_dir.clone(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SfzError {
    #[error("no se pudo leer {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("demasiados #include anidados")]
    IncludeDepth,
    #[error("el instrumento no tiene ninguna región con sample")]
    Empty,
}

/// Lee un `.sfz` del disco, resolviendo sus `#include`.
pub fn load(path: &Path) -> Result<SfzInstrument, SfzError> {
    let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let read = |p: &Path| {
        std::fs::read_to_string(p).map_err(|source| SfzError::Io {
            path: p.display().to_string(),
            source,
        })
    };
    let text = read(path)?;
    parse(&text, &base_dir, &read)
}

/// Lee un `.sfz` a partir de su texto. `read` resuelve los `#include`
/// (relativos a `base_dir`).
pub fn parse(
    text: &str,
    base_dir: &Path,
    read: &dyn Fn(&Path) -> Result<String, SfzError>,
) -> Result<SfzInstrument, SfzError> {
    let mut expanded = String::new();
    let mut defines: Vec<(String, String)> = Vec::new();
    expand(text, base_dir, read, &mut defines, &mut expanded, 0)?;
    let regions = build_regions(&expanded);
    if regions.is_empty() {
        return Err(SfzError::Empty);
    }
    Ok(SfzInstrument {
        regions,
        base_dir: base_dir.to_path_buf(),
    })
}

/// Expande `#include` y `#define` en orden: una variable redefinida vale lo
/// nuevo desde ese punto en adelante, como en los reproductores SFZ.
fn expand(
    text: &str,
    base_dir: &Path,
    read: &dyn Fn(&Path) -> Result<String, SfzError>,
    defines: &mut Vec<(String, String)>,
    out: &mut String,
    depth: usize,
) -> Result<(), SfzError> {
    if depth > 16 {
        return Err(SfzError::IncludeDepth);
    }
    let text = strip_block_comments(text);
    for raw_line in text.lines() {
        let line = raw_line.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#define") {
            let rest = rest.trim();
            let (name, value) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let value = substitute(value.trim(), defines);
            defines.retain(|(n, _)| n != name);
            defines.push((name.to_string(), value));
            // Las más largas primero, para que `$OFF1` no tape a `$OFF10`.
            defines.sort_by_key(|d| std::cmp::Reverse(d.0.len()));
            continue;
        }
        let mut rest = line;
        while let Some(at) = rest.find("#include") {
            out.push_str(&substitute(&rest[..at], defines));
            out.push(' ');
            let after = rest[at + "#include".len()..].trim_start();
            let Some(quoted) = after.strip_prefix('"') else {
                rest = after;
                continue;
            };
            let end = quoted.find('"').unwrap_or(quoted.len());
            let file = substitute(&quoted[..end], defines).replace('\\', "/");
            let included = read(&base_dir.join(&file))?;
            expand(&included, base_dir, read, defines, out, depth + 1)?;
            rest = quoted.get(end + 1..).unwrap_or("");
        }
        out.push_str(&substitute(rest, defines));
        out.push('\n');
    }
    Ok(())
}

fn strip_block_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = rest[start + 2..]
            .split_once("*/")
            .map_or("", |(_, after)| after);
    }
    out.push_str(rest);
    out
}

fn substitute(text: &str, defines: &[(String, String)]) -> String {
    if !text.contains('$') {
        return text.to_string();
    }
    let mut out = text.to_string();
    for (name, value) in defines {
        out = out.replace(name.as_str(), value);
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Control,
    Global,
    Master,
    Group,
    Region,
}

/// Separa una línea en encabezados y pares `opcode=valor`. Un valor llega
/// hasta el siguiente opcode, así se admiten rutas con espacios.
fn tokens(line: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut rest = line.trim();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('<') {
            let end = after.find('>').unwrap_or(after.len());
            out.push(Token::Header(after[..end].trim()));
            rest = after.get(end + 1..).unwrap_or("").trim_start();
            continue;
        }
        let Some(eq) = rest.find('=') else {
            break;
        };
        let name = rest[..eq].trim();
        let value_start = eq + 1;
        // El valor termina donde empieza el próximo `opcode=` o `<encabezado>`.
        let tail = &rest[value_start..];
        let mut end = tail.len();
        for (i, c) in tail.char_indices() {
            if c == '<' {
                end = i;
                break;
            }
            if c.is_whitespace() {
                let next = tail[i..].trim_start();
                let word_len = next
                    .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                    .unwrap_or(next.len());
                if word_len > 0 && next[word_len..].starts_with('=') {
                    end = i;
                    break;
                }
            }
        }
        out.push(Token::Opcode(name, tail[..end].trim()));
        rest = tail[end..].trim_start();
    }
    out
}

enum Token<'a> {
    Header(&'a str),
    Opcode(&'a str, &'a str),
}

fn build_regions(text: &str) -> Vec<Region> {
    let mut maps: [HashMap<String, String>; 5] = Default::default();
    let mut level: Option<Level> = None;
    let mut regions = Vec::new();

    let finish = |maps: &[HashMap<String, String>; 5], regions: &mut Vec<Region>| {
        if let Some(r) = make_region(maps) {
            regions.push(r);
        }
    };

    for line in text.lines() {
        for token in tokens(line) {
            match token {
                Token::Header(name) => {
                    if level == Some(Level::Region) {
                        finish(&maps, &mut regions);
                    }
                    let new_level = match name {
                        "control" => Level::Control,
                        "global" => Level::Global,
                        "master" => Level::Master,
                        "group" => Level::Group,
                        "region" => Level::Region,
                        _ => {
                            // Encabezados que no producen sonido (<curve>, <effect>…).
                            level = None;
                            continue;
                        }
                    };
                    // Un encabezado nuevo borra lo de su nivel y los de abajo.
                    for map in maps.iter_mut().skip(new_level as usize) {
                        map.clear();
                    }
                    level = Some(new_level);
                }
                Token::Opcode(name, value) => {
                    if let Some(l) = level {
                        maps[l as usize].insert(name.to_string(), value.to_string());
                    }
                }
            }
        }
    }
    if level == Some(Level::Region) {
        finish(&maps, &mut regions);
    }
    regions
}

/// Nota MIDI a partir de un número o un nombre (`c4` = 60, `f#3`, `db2`).
pub fn parse_note(value: &str) -> Option<i32> {
    let v = value.trim();
    if let Ok(n) = v.parse::<i32>() {
        return Some(n);
    }
    let mut chars = v.chars();
    let base = match chars.next()?.to_ascii_lowercase() {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let (shift, octave) = if let Some(r) = rest.strip_prefix('#') {
        (1, r)
    } else if let Some(r) = rest.strip_prefix('b') {
        (-1, r)
    } else {
        (0, rest.as_str())
    };
    let octave: i32 = octave.parse().ok()?;
    Some((octave + 1) * 12 + base + shift)
}

fn make_region(maps: &[HashMap<String, String>; 5]) -> Option<Region> {
    // Valores efectivos: los niveles de abajo pisan a los de arriba.
    let get = |name: &str| -> Option<&str> {
        maps.iter()
            .rev()
            .find_map(|m| m.get(name))
            .map(String::as_str)
    };
    let num = |name: &str, default: f32| -> f32 {
        get(name).and_then(|v| v.parse().ok()).unwrap_or(default)
    };
    // Valor por defecto de un controlador, de 0 a 1.
    let cc = |n: &str| -> f32 {
        let control = &maps[Level::Control as usize];
        if let Some(v) = control
            .get(&format!("set_hdcc{n}"))
            .and_then(|v| v.parse().ok())
        {
            return v;
        }
        control
            .get(&format!("set_cc{n}"))
            .and_then(|v| v.parse::<f32>().ok())
            .map_or(0.0, |v| v / 127.0)
    };
    // Suma de las modulaciones `<opcode>_onccN` con los controladores en su valor inicial.
    let modulated = |name: &str, base: f32| -> f32 {
        let prefix = format!("{name}_oncc");
        let mut total = base;
        let mut seen = Vec::new();
        for m in maps.iter() {
            for k in m.keys() {
                if let Some(n) = k.strip_prefix(&prefix)
                    && !seen.contains(&n)
                    && let Some(n_value) = get(k).and_then(|v| v.parse::<f32>().ok())
                {
                    seen.push(n);
                    total += n_value * cc(n);
                }
            }
        }
        total
    };

    let sample = get("sample")?.trim().replace('\\', "/");
    if sample.is_empty() || sample.starts_with('*') {
        return None; // Generadores internos (*sine, *noise…): no son samples.
    }
    // Regiones disparadas por controladores (ruido de pedal y similares).
    if maps
        .iter()
        .flat_map(|m| m.keys())
        .any(|k| k.starts_with("on_locc") || k.starts_with("on_hicc"))
    {
        return None;
    }
    // Condiciones sobre controladores: se evalúan con su valor inicial.
    for m in maps.iter() {
        for k in m.keys() {
            if let Some(n) = k.strip_prefix("locc") {
                let lo = num(k, 0.0) / 127.0;
                let hi = num(&format!("hicc{n}"), 127.0) / 127.0;
                if !(lo..=hi).contains(&cc(n)) {
                    return None;
                }
            }
        }
    }
    // Teclas de cambio: se usa la variante por defecto.
    if let Some(last) = get("sw_last").and_then(parse_note) {
        let default = get("sw_default").and_then(parse_note);
        if default.is_some_and(|d| d != last) {
            return None;
        }
    }

    let key = get("key").and_then(parse_note);
    let lokey = get("lokey").and_then(parse_note).or(key).unwrap_or(0);
    let hikey = get("hikey").and_then(parse_note).or(key).unwrap_or(127);
    if lokey < 0 || hikey < 0 || lokey > hikey {
        return None;
    }
    let keycenter = get("pitch_keycenter")
        .and_then(parse_note)
        .or(key)
        .unwrap_or(60);
    let trigger = match get("trigger").unwrap_or("attack") {
        "release" | "release_key" => Trigger::Release,
        _ => Trigger::Attack,
    };
    let path = get("default_path").unwrap_or("").replace('\\', "/");

    Some(Region {
        sample: format!("{path}{sample}"),
        lokey: lokey.clamp(0, 127) as u8,
        hikey: hikey.clamp(0, 127) as u8,
        pitch_keycenter: keycenter.clamp(0, 127) as u8,
        lovel: num("lovel", 1.0).clamp(1.0, 127.0) as u8,
        hivel: num("hivel", 127.0).clamp(1.0, 127.0) as u8,
        trigger,
        tune_cents: num("tune", 0.0) + 100.0 * num("transpose", 0.0),
        pitch_keytrack: num("pitch_keytrack", 100.0),
        volume_db: num("volume", 0.0),
        amp_veltrack: (modulated("amp_veltrack", num("amp_veltrack", 100.0)) / 100.0)
            .clamp(0.0, 1.0),
        attack: modulated("ampeg_attack", num("ampeg_attack", 0.0)).max(0.0),
        release: modulated("ampeg_release", num("ampeg_release", 0.0)).max(0.0),
        rt_decay_db: num("rt_decay", 0.0),
        offset: modulated("offset", num("offset", 0.0)).max(0.0) as u32,
        seq_length: num("seq_length", 1.0).max(1.0) as u8,
        seq_position: num("seq_position", 1.0).max(1.0) as u8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_files(files: &[(&str, &str)]) -> SfzInstrument {
        let map: HashMap<String, String> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let read = move |p: &Path| {
            let key = p.to_string_lossy().replace('\\', "/");
            let key = key.trim_start_matches("base/").to_string();
            map.get(&key).cloned().ok_or_else(|| SfzError::Io {
                path: key,
                source: std::io::Error::other("no está"),
            })
        };
        let main = files[0].1;
        parse(main, Path::new("base"), &read).unwrap()
    }

    #[test]
    fn reads_a_salamander_style_instrument() {
        // Estructura de Salamander Grand Piano V3, recortada.
        let sfz = parse_files(&[
            (
                "main.sfz",
                r#"
// comentario
#define $RELEASE 72
#define $VELTRACK 99
#define $NATURAL C0
#define $RETUNED C#0
#define $EXT flac
<control>
default_path=Samples/
set_hdcc$RELEASE=0.5
set_hdcc$VELTRACK=0.73
<global>
amp_veltrack_oncc$VELTRACK=-100
ampeg_release_oncc$RELEASE=2
sw_lokey=$NATURAL sw_hikey=$RETUNED sw_default=$NATURAL
<master> sw_last=$NATURAL
#include "Data/notes.txt"
<master> sw_last=$RETUNED
#include "Data/notes.txt"
<global> trigger=release
<group> group_label=rel volume=-37 rt_decay=2
<region> key=21 sample=rel1.$EXT
<group> on_locc64=127 on_hicc64=127
<region> sample=pedalD1.$EXT
"#,
            ),
            (
                "Data/notes.txt",
                r#"<group> #include "Data/vel_01.txt" lovel=1 hivel=64 #include "Data/region.txt"
<group> #include "Data/vel_02.txt" lovel=65 hivel=127 #include "Data/region.txt""#,
            ),
            ("Data/vel_01.txt", "#define $VEL v1"),
            ("Data/vel_02.txt", "#define $VEL v2"),
            (
                "Data/region.txt",
                "<region> lokey=21 hikey=22 pitch_keycenter=21 tune=3 sample=A0$VEL.$EXT\n<region> lokey=23 hikey=25 pitch_keycenter=24 sample=C1$VEL.$EXT",
            ),
        ]);

        let attack: Vec<&Region> = sfz
            .regions
            .iter()
            .filter(|r| r.trigger == Trigger::Attack)
            .collect();
        // Solo la variante "Natural" (la tecla de cambio por defecto): 2 capas × 2 regiones.
        assert_eq!(attack.len(), 4);
        let a0_soft = attack
            .iter()
            .find(|r| r.sample == "Samples/A0v1.flac")
            .unwrap();
        assert_eq!(
            (a0_soft.lokey, a0_soft.hikey, a0_soft.pitch_keycenter),
            (21, 22, 21)
        );
        assert_eq!((a0_soft.lovel, a0_soft.hivel), (1, 64));
        assert_eq!(a0_soft.tune_cents, 3.0);
        assert!((a0_soft.amp_veltrack - 0.27).abs() < 1e-4);
        assert!((a0_soft.release - 1.0).abs() < 1e-6);
        assert!(attack.iter().any(|r| r.sample == "Samples/C1v2.flac"));

        let release: Vec<&Region> = sfz
            .regions
            .iter()
            .filter(|r| r.trigger == Trigger::Release)
            .collect();
        assert_eq!(
            release.len(),
            1,
            "el ruido de pedal depende de un controlador y se omite"
        );
        assert_eq!(release[0].sample, "Samples/rel1.flac");
        assert_eq!((release[0].volume_db, release[0].rt_decay_db), (-37.0, 2.0));
    }

    #[test]
    fn values_can_contain_spaces_and_notes_can_be_names() {
        let sfz = parse_files(&[(
            "x.sfz",
            "<region> sample=Grand Piano\\C4 soft.wav lokey=c4 hikey=e4 lovel=1 hivel=60 seq_length=2 seq_position=2",
        )]);
        let r = &sfz.regions[0];
        assert_eq!(r.sample, "Grand Piano/C4 soft.wav");
        assert_eq!((r.lokey, r.hikey, r.pitch_keycenter), (60, 64, 60));
        assert_eq!((r.seq_length, r.seq_position), (2, 2));
    }

    #[test]
    fn note_names_follow_the_sfz_convention() {
        assert_eq!(parse_note("c4"), Some(60));
        assert_eq!(parse_note("C#0"), Some(13));
        assert_eq!(parse_note("db2"), Some(37));
        assert_eq!(parse_note("21"), Some(21));
        assert_eq!(parse_note("h2"), None);
    }

    #[test]
    fn a_lighter_version_keeps_evenly_spread_layers() {
        let mut text = String::new();
        for layer in 0..16u8 {
            let (lo, hi) = (layer * 8 + 1, layer * 8 + 8);
            text.push_str(&format!(
                "<region> key=60 lovel={lo} hivel={hi} sample=v{layer}.wav\n"
            ));
        }
        text.push_str("<region> key=60 trigger=release sample=rel.wav\n");
        let sfz = parse_files(&[("x.sfz", &text)]);
        let light = sfz.with_velocity_layers(8);
        let samples = light.sample_paths();
        assert_eq!(samples.len(), 9, "8 capas más la liberación");
        assert!(samples.contains(&"v0.wav".to_string()));
        assert!(samples.contains(&"v15.wav".to_string()));
        assert!(samples.contains(&"rel.wav".to_string()));
    }
}
