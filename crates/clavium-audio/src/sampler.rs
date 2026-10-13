//! Piano de samples: reproduce un instrumento SFZ con las grabaciones de un
//! piano real.
//!
//! Los samples se cargan una sola vez en memoria (como enteros de 16 bits)
//! y el hilo de audio solo los lee: no reserva memoria ni toca el disco.

use std::path::Path;
use std::sync::Arc;

use crate::sfz::{Region, SfzInstrument, Trigger};

/// Voces simultáneas del sampler.
pub const MAX_VOICES: usize = 96;
/// Fundido al cortar una cola larga para ahorrar memoria, en segundos.
const TAIL_FADE: f32 = 1.0;
/// Fundido mínimo al soltar, para que no haga clic.
const MIN_RELEASE: f32 = 0.02;
/// Debajo de este nivel la voz se da por terminada (−80 dB).
const SILENCE: f32 = 1e-4;

#[derive(Debug, Clone)]
pub struct SampleData {
    pub rate: u32,
    /// 1 o 2.
    pub channels: usize,
    /// Muestras intercaladas por canal, en 16 bits.
    pub data: Vec<i16>,
}

impl SampleData {
    pub fn frames(&self) -> usize {
        self.data.len() / self.channels
    }

    fn frame(&self, i: usize) -> (f32, f32) {
        const SCALE: f32 = 1.0 / 32768.0;
        if self.channels == 1 {
            let s = f32::from(self.data[i]) * SCALE;
            (s, s)
        } else {
            (
                f32::from(self.data[2 * i]) * SCALE,
                f32::from(self.data[2 * i + 1]) * SCALE,
            )
        }
    }

    /// Recorta la cola a `max_seconds` con un fundido, para ahorrar memoria.
    fn trim(&mut self, max_seconds: f32) {
        let max_frames = (max_seconds * self.rate as f32) as usize;
        if self.frames() <= max_frames {
            return;
        }
        let fade = ((TAIL_FADE * self.rate as f32) as usize).min(max_frames);
        self.data.truncate(max_frames * self.channels);
        for f in 0..fade {
            let gain = 1.0 - f as f32 / fade as f32;
            let frame = max_frames - fade + f;
            for c in 0..self.channels {
                let s = &mut self.data[frame * self.channels + c];
                *s = (f32::from(*s) * gain) as i16;
            }
        }
    }
}

/// Lee un sample FLAC o WAV y lo convierte a 16 bits (como mucho estéreo).
pub fn load_sample(path: &Path) -> Result<SampleData, String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let to_i16 = |v: i32, bits: u32| -> i16 {
        if bits >= 16 {
            (v >> (bits - 16)) as i16
        } else {
            (v << (16 - bits)) as i16
        }
    };
    let (rate, channels, interleaved): (u32, usize, Vec<i16>) = match ext.as_str() {
        "flac" => {
            let mut reader = claxon::FlacReader::open(path).map_err(|e| e.to_string())?;
            let info = reader.streaminfo();
            let bits = info.bits_per_sample;
            let samples: Result<Vec<i16>, _> = reader
                .samples()
                .map(|s| s.map(|v| to_i16(v, bits)))
                .collect();
            (
                info.sample_rate,
                info.channels as usize,
                samples.map_err(|e| e.to_string())?,
            )
        }
        "wav" => {
            let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
            let spec = reader.spec();
            let samples: Vec<i16> = match spec.sample_format {
                hound::SampleFormat::Int => reader
                    .samples::<i32>()
                    .map(|s| s.map(|v| to_i16(v, u32::from(spec.bits_per_sample))))
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())?,
                hound::SampleFormat::Float => reader
                    .samples::<f32>()
                    .map(|s| s.map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i16))
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())?,
            };
            (spec.sample_rate, usize::from(spec.channels), samples)
        }
        other => return Err(format!("formato de sample no soportado: .{other}")),
    };
    if channels == 0 || rate == 0 {
        return Err("sample vacío".into());
    }
    // Más de dos canales: se quedan los dos primeros.
    let data = if channels > 2 {
        interleaved
            .chunks(channels)
            .flat_map(|f| [f[0], f[1]])
            .collect()
    } else {
        interleaved
    };
    Ok(SampleData {
        rate,
        channels: channels.min(2),
        data,
    })
}

/// Un instrumento listo para tocar: regiones con sus samples ya en memoria.
pub struct SampleBank {
    pub name: String,
    regions: Vec<(Region, Arc<SampleData>)>,
}

impl SampleBank {
    /// Carga los samples del instrumento. Los que faltan se omiten (por
    /// ejemplo, las capas que no se descargaron). `progress` recibe cuántos
    /// samples van cargados y el total.
    pub fn load(
        name: &str,
        sfz: &SfzInstrument,
        max_seconds: f32,
        progress: &dyn Fn(usize, usize),
    ) -> Result<SampleBank, String> {
        let paths = sfz.sample_paths();
        let mut loaded: Vec<(String, Arc<SampleData>)> = Vec::with_capacity(paths.len());
        for (i, rel) in paths.iter().enumerate() {
            progress(i, paths.len());
            let full = sfz.base_dir.join(rel);
            if !full.exists() {
                continue;
            }
            let mut sample = load_sample(&full).map_err(|e| format!("{rel}: {e}"))?;
            sample.trim(max_seconds);
            loaded.push((rel.clone(), Arc::new(sample)));
        }
        progress(paths.len(), paths.len());
        let regions: Vec<(Region, Arc<SampleData>)> = sfz
            .regions
            .iter()
            .filter_map(|r| {
                loaded
                    .iter()
                    .find(|(p, _)| *p == r.sample)
                    .map(|(_, s)| (r.clone(), s.clone()))
            })
            .collect();
        if !regions.iter().any(|(r, _)| r.trigger == Trigger::Attack) {
            return Err("no se encontró ningún sample del instrumento".into());
        }
        Ok(SampleBank {
            name: name.to_string(),
            regions,
        })
    }

    /// Memoria que ocupan los samples, en bytes.
    pub fn memory_bytes(&self) -> usize {
        let mut seen: Vec<*const SampleData> = Vec::new();
        let mut total = 0;
        for (_, s) in &self.regions {
            let ptr = Arc::as_ptr(s);
            if !seen.contains(&ptr) {
                seen.push(ptr);
                total += s.data.len() * 2;
            }
        }
        total
    }

    #[cfg(test)]
    pub(crate) fn from_parts(regions: Vec<(Region, Arc<SampleData>)>) -> Self {
        SampleBank {
            name: "prueba".into(),
            regions,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Held,
    Sustained,
    Released,
}

struct Voice {
    pitch: u8,
    sample: Arc<SampleData>,
    pos: f64,
    step: f64,
    gain: f32,
    attack_step: f32,
    attack: f32,
    release_mul: f32,
    env: f32,
    stage: Stage,
    is_release_sample: bool,
    age: u64,
}

impl Voice {
    fn next(&mut self) -> Option<(f32, f32)> {
        let i = self.pos as usize;
        let frames = self.sample.frames();
        if i + 1 >= frames || self.env < SILENCE {
            return None;
        }
        let t = (self.pos - i as f64) as f32;
        let (l0, r0) = self.sample.frame(i);
        let (l1, r1) = self.sample.frame(i + 1);
        self.pos += self.step;
        if self.attack < 1.0 {
            self.attack = (self.attack + self.attack_step).min(1.0);
        }
        if self.stage == Stage::Released {
            self.env *= self.release_mul;
        }
        let g = self.gain * self.attack * self.env;
        Some(((l0 + (l1 - l0) * t) * g, (r0 + (r1 - r0) * t) * g))
    }
}

/// Estado de cada tecla para disparar los samples de liberación.
#[derive(Clone, Copy, Default)]
struct KeyHold {
    velocity: u8,
    /// Muestra (de salida) en la que se tocó la nota.
    since: u64,
    down: bool,
}

pub struct Sampler {
    bank: Arc<SampleBank>,
    out_rate: f32,
    voices: Vec<Voice>,
    sustain: bool,
    keys: [KeyHold; 128],
    /// Contador de alternancia (`seq_position`) por tecla.
    seq: [u8; 128],
    clock: u64,
    counter: u64,
}

impl Sampler {
    pub fn new(bank: Arc<SampleBank>, out_rate: u32) -> Self {
        Sampler {
            bank,
            out_rate: out_rate as f32,
            voices: Vec::with_capacity(MAX_VOICES),
            sustain: false,
            keys: [KeyHold::default(); 128],
            seq: [0; 128],
            clock: 0,
            counter: 0,
        }
    }

    /// Devuelve el banco, para soltarlo fuera del hilo de audio.
    pub fn into_bank(self) -> Arc<SampleBank> {
        self.bank
    }

    pub fn active_voices(&self) -> usize {
        self.voices.len()
    }

    fn start(
        &mut self,
        region: &Region,
        sample: &Arc<SampleData>,
        pitch: u8,
        velocity: u8,
        extra_db: f32,
        release_sample: bool,
    ) {
        let v = f32::from(velocity) / 127.0;
        // amp_veltrack: 0 = siempre igual, 1 = curva cuadrática de la dinámica.
        let vel_gain = (1.0 - region.amp_veltrack) + region.amp_veltrack * v * v;
        let gain = db(region.volume_db + extra_db) * vel_gain;
        let semis = (f32::from(pitch) - f32::from(region.pitch_keycenter)) * region.pitch_keytrack
            / 100.0
            + region.tune_cents / 100.0;
        let step = f64::from(2f32.powf(semis / 12.0) * sample.rate as f32 / self.out_rate);
        let release_time = region.release.max(MIN_RELEASE);

        if self.voices.len() >= MAX_VOICES {
            // Se roba la voz más silenciosa (o la más vieja si empatan).
            if let Some(victim) = self
                .voices
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (a.env * a.gain)
                        .total_cmp(&(b.env * b.gain))
                        .then(a.age.cmp(&b.age))
                })
                .map(|(i, _)| i)
            {
                self.voices.swap_remove(victim);
            }
        }
        self.counter += 1;
        self.voices.push(Voice {
            pitch,
            sample: sample.clone(),
            pos: f64::from(region.offset).min(sample.frames().saturating_sub(2) as f64),
            step,
            gain,
            attack_step: 1.0 / (region.attack.max(0.001) * self.out_rate),
            attack: 0.0,
            release_mul: (-1.0 / (release_time * self.out_rate)).exp(),
            env: 1.0,
            // Los samples de liberación suenan enteros: su envolvente no corre.
            stage: Stage::Held,
            is_release_sample: release_sample,
            age: self.counter,
        });
    }

    /// Regiones que corresponden a la tecla y la dinámica. Si ninguna
    /// coincide (por ejemplo, en una versión con menos capas), se usa la
    /// capa de dinámica más cercana.
    fn regions_for(&self, trigger: Trigger, key: u8, velocity: u8, seq: u8) -> Vec<usize> {
        let regions = &self.bank.regions;
        let of_key: Vec<usize> = (0..regions.len())
            .filter(|&i| {
                let r = &regions[i].0;
                r.trigger == trigger && (r.lokey..=r.hikey).contains(&key)
            })
            .collect();
        let in_seq = |i: &usize| {
            let r = &regions[*i].0;
            r.seq_length <= 1 || (seq % r.seq_length) + 1 == r.seq_position
        };
        let exact: Vec<usize> = of_key
            .iter()
            .copied()
            .filter(|&i| (regions[i].0.lovel..=regions[i].0.hivel).contains(&velocity))
            .filter(in_seq)
            .collect();
        if !exact.is_empty() || trigger == Trigger::Release {
            return exact;
        }
        let distance = |i: usize| {
            let r = &regions[i].0;
            if velocity < r.lovel {
                r.lovel - velocity
            } else {
                velocity.saturating_sub(r.hivel)
            }
        };
        let best = of_key.iter().map(|&i| distance(i)).min();
        of_key
            .into_iter()
            .filter(|&i| Some(distance(i)) == best)
            .filter(in_seq)
            .collect()
    }

    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        if velocity == 0 {
            self.note_off(pitch);
            return;
        }
        let key = usize::from(pitch.min(127));
        // Volver a tocar una tecla apaga la cuerda que ya sonaba.
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.pitch == pitch && !v.is_release_sample)
        {
            v.stage = Stage::Released;
        }
        let seq = self.seq[key];
        self.seq[key] = self.seq[key].wrapping_add(1);
        let bank = self.bank.clone();
        for i in self.regions_for(Trigger::Attack, pitch, velocity, seq) {
            let (region, sample) = &bank.regions[i];
            self.start(region, sample, pitch, velocity, 0.0, false);
        }
        self.keys[key] = KeyHold {
            velocity,
            since: self.clock,
            down: true,
        };
    }

    pub fn note_off(&mut self, pitch: u8) {
        let key = usize::from(pitch.min(127));
        if !self.keys[key].down {
            return;
        }
        self.keys[key].down = false;
        if self.sustain {
            for v in self
                .voices
                .iter_mut()
                .filter(|v| v.pitch == pitch && v.stage == Stage::Held && !v.is_release_sample)
            {
                v.stage = Stage::Sustained;
            }
        } else {
            self.release_key(pitch);
        }
    }

    /// El apagador cae sobre la cuerda: termina la nota y suena la liberación.
    fn release_key(&mut self, pitch: u8) {
        let mut released = false;
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.pitch == pitch && !v.is_release_sample && v.stage != Stage::Released)
        {
            v.stage = Stage::Released;
            released = true;
        }
        if !released {
            return;
        }
        let hold = self.keys[usize::from(pitch.min(127))];
        let held_seconds = (self.clock - hold.since) as f32 / self.out_rate;
        let bank = self.bank.clone();
        for i in self.regions_for(Trigger::Release, pitch, hold.velocity.max(1), 0) {
            let (region, sample) = &bank.regions[i];
            let fade = -region.rt_decay_db * held_seconds;
            self.start(region, sample, pitch, hold.velocity.max(1), fade, true);
        }
    }

    pub fn set_sustain(&mut self, down: bool) {
        self.sustain = down;
        if !down {
            let sustained: Vec<u8> = self
                .voices
                .iter()
                .filter(|v| v.stage == Stage::Sustained)
                .map(|v| v.pitch)
                .collect();
            for pitch in sustained {
                self.release_key(pitch);
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        self.sustain = false;
        for v in &mut self.voices {
            v.stage = Stage::Released;
        }
        for k in &mut self.keys {
            k.down = false;
        }
    }

    /// Una muestra estéreo de todas las voces.
    pub fn next_frame(&mut self) -> (f32, f32) {
        self.clock += 1;
        let (mut l, mut r) = (0.0, 0.0);
        let mut i = 0;
        while i < self.voices.len() {
            match self.voices[i].next() {
                Some((a, b)) => {
                    l += a;
                    r += b;
                    i += 1;
                }
                None => {
                    self.voices.swap_remove(i);
                }
            }
        }
        (l, r)
    }
}

fn db(value: f32) -> f32 {
    10f32.powf(value / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Una senoidal de `seconds` segundos a 440 Hz, mono.
    fn tone(seconds: f32) -> Arc<SampleData> {
        let n = (seconds * RATE as f32) as usize;
        Arc::new(SampleData {
            rate: RATE,
            channels: 1,
            data: (0..n)
                .map(|i| {
                    ((i as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 16000.0)
                        as i16
                })
                .collect(),
        })
    }

    fn region(lovel: u8, hivel: u8, trigger: Trigger) -> Region {
        Region {
            sample: String::new(),
            lokey: 60,
            hikey: 62,
            pitch_keycenter: 60,
            lovel,
            hivel,
            trigger,
            tune_cents: 0.0,
            pitch_keytrack: 100.0,
            volume_db: 0.0,
            amp_veltrack: 1.0,
            attack: 0.0,
            release: 0.1,
            rt_decay_db: 0.0,
            offset: 0,
            seq_length: 1,
            seq_position: 1,
        }
    }

    fn render(s: &mut Sampler, seconds: f32) -> Vec<f32> {
        (0..(seconds * RATE as f32) as usize)
            .map(|_| s.next_frame().0)
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn plays_the_layer_of_the_velocity_and_falls_back_to_the_nearest() {
        let bank = Arc::new(SampleBank::from_parts(vec![
            (region(1, 40, Trigger::Attack), tone(2.0)),
            (region(100, 127, Trigger::Attack), tone(2.0)),
        ]));
        let s = Sampler::new(bank, RATE);
        assert_eq!(s.regions_for(Trigger::Attack, 60, 20, 0), vec![0]);
        assert_eq!(s.regions_for(Trigger::Attack, 60, 110, 0), vec![1]);
        // 60 está más cerca de 40 que de 100: capa suave.
        assert_eq!(s.regions_for(Trigger::Attack, 60, 60, 0), vec![0]);
        assert!(s.regions_for(Trigger::Attack, 70, 60, 0).is_empty());
    }

    #[test]
    fn transposes_between_recorded_notes() {
        let bank = Arc::new(SampleBank::from_parts(vec![(
            region(1, 127, Trigger::Attack),
            tone(2.0),
        )]));
        let mut s = Sampler::new(bank, RATE);
        s.note_on(62, 100);
        let expected = 2f64.powf(2.0 / 12.0);
        assert!((s.voices[0].step - expected).abs() < 1e-4);
    }

    #[test]
    fn releasing_the_key_fades_the_note_and_plays_the_release_sample() {
        let mut rel = region(1, 127, Trigger::Release);
        rel.rt_decay_db = 10.0;
        let bank = Arc::new(SampleBank::from_parts(vec![
            (region(1, 127, Trigger::Attack), tone(3.0)),
            (rel, tone(0.2)),
        ]));
        let mut s = Sampler::new(bank, RATE);
        s.note_on(60, 100);
        let held = render(&mut s, 0.5);
        assert!(rms(&held) > 0.05);
        s.note_off(60);
        assert_eq!(s.active_voices(), 2, "la nota y su liberación");
        render(&mut s, 1.5);
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn the_sustain_pedal_holds_notes_until_it_is_lifted() {
        let bank = Arc::new(SampleBank::from_parts(vec![(
            region(1, 127, Trigger::Attack),
            tone(3.0),
        )]));
        let mut s = Sampler::new(bank, RATE);
        s.set_sustain(true);
        s.note_on(60, 100);
        render(&mut s, 0.1);
        s.note_off(60);
        let held = render(&mut s, 1.0);
        assert!(rms(&held[held.len() - 4800..]) > 0.05);
        s.set_sustain(false);
        render(&mut s, 1.0);
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn soft_notes_are_quieter() {
        let bank = Arc::new(SampleBank::from_parts(vec![(
            region(1, 127, Trigger::Attack),
            tone(1.0),
        )]));
        let mut soft = Sampler::new(bank.clone(), RATE);
        soft.note_on(60, 30);
        let mut loud = Sampler::new(bank, RATE);
        loud.note_on(60, 120);
        assert!(rms(&render(&mut loud, 0.2)) > 4.0 * rms(&render(&mut soft, 0.2)));
    }

    #[test]
    fn long_tails_are_trimmed_with_a_fade() {
        let mut s = (*tone(5.0)).clone();
        s.trim(2.0);
        assert_eq!(s.frames(), 2 * RATE as usize);
        assert_eq!(*s.data.last().unwrap(), 0);
    }
}
