//! Piano sintetizado: síntesis aditiva con parciales levemente inarmónicos,
//! un golpe de martillo y decaimiento que depende de la altura.
//!
//! No pretende sonar como un piano grabado; sirve para escuchar lo que se
//! toca sin descargar samples, y para ajustar latencia y pedal.

/// Voces simultáneas. Con el pedal presionado se acumulan rápido.
pub const MAX_VOICES: usize = 64;
const MAX_PARTIALS: usize = 12;
/// Inarmonicidad de las cuerdas: los parciales agudos quedan un poco altos.
const INHARMONICITY: f32 = 0.0004;
/// Duración del ataque, en segundos, para evitar un clic.
const ATTACK_SECONDS: f32 = 0.002;
/// Constante de tiempo del apagador al soltar la tecla.
const RELEASE_SECONDS: f32 = 0.09;
/// Debajo de este nivel la voz se da por terminada (−80 dB).
const SILENCE: f32 = 1e-4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// La tecla está apretada.
    Held,
    /// La tecla se soltó pero el pedal la sostiene.
    Sustained,
    /// El apagador cayó sobre la cuerda.
    Released,
}

#[derive(Debug, Clone, Copy)]
struct Partial {
    /// Oscilador por rotación: (cos, sin) del ángulo actual y del paso.
    re: f32,
    im: f32,
    step_re: f32,
    step_im: f32,
    amp: f32,
    decay: f32,
}

#[derive(Debug, Clone)]
struct Voice {
    pitch: u8,
    stage: Stage,
    partials: [Partial; MAX_PARTIALS],
    partial_count: usize,
    gain: f32,
    attack: f32,
    attack_step: f32,
    release: f32,
    release_decay: f32,
    noise: f32,
    noise_decay: f32,
    noise_state: u32,
    age: u64,
}

impl Voice {
    fn new(pitch: u8, velocity: u8, sample_rate: f32, age: u64) -> Self {
        let freq = 440.0 * 2f32.powf((f32::from(pitch) - 69.0) / 12.0);
        let v = f32::from(velocity.clamp(1, 127)) / 127.0;
        // Más fuerte = más brillante: los parciales altos caen menos.
        let rolloff = 0.55 - 0.4 * v;
        // Las cuerdas graves suenan mucho más que las agudas.
        let tau = (7.0 * (110.0 / freq).sqrt()).clamp(0.4, 12.0);

        let mut partials = [Partial {
            re: 1.0,
            im: 0.0,
            step_re: 1.0,
            step_im: 0.0,
            amp: 0.0,
            decay: 1.0,
        }; MAX_PARTIALS];
        let mut count = 0;
        let mut norm = 0.0;
        for (i, p) in partials.iter_mut().enumerate() {
            let n = (i + 1) as f32;
            let f = n * freq * (1.0 + INHARMONICITY * n * n).sqrt();
            if f > sample_rate * 0.45 {
                break;
            }
            let w = std::f32::consts::TAU * f / sample_rate;
            p.step_re = w.cos();
            p.step_im = w.sin();
            p.amp = (-(n - 1.0) * rolloff).exp() / n.powf(0.9);
            let partial_tau = tau / (1.0 + 0.5 * (n - 1.0));
            p.decay = (-1.0 / (partial_tau * sample_rate)).exp();
            norm += p.amp;
            count = i + 1;
        }
        for p in &mut partials[..count] {
            p.amp /= norm.max(1e-6);
        }

        Voice {
            pitch,
            stage: Stage::Held,
            partials,
            partial_count: count,
            gain: 0.35 * v.powf(1.6),
            attack: 0.0,
            attack_step: 1.0 / (ATTACK_SECONDS * sample_rate),
            release: 1.0,
            release_decay: (-1.0 / (RELEASE_SECONDS * sample_rate)).exp(),
            noise: 0.04 * v,
            noise_decay: (-1.0 / (0.008 * sample_rate)).exp(),
            noise_state: 0x9E37_79B9 ^ u32::from(pitch) << 8 ^ u32::from(velocity),
            age,
        }
    }

    fn next(&mut self) -> f32 {
        let mut s = 0.0;
        for p in &mut self.partials[..self.partial_count] {
            let re = p.re * p.step_re - p.im * p.step_im;
            p.im = p.re * p.step_im + p.im * p.step_re;
            p.re = re;
            s += p.im * p.amp;
            p.amp *= p.decay;
        }

        // Golpe del martillo: ruido corto que se apaga en milisegundos.
        self.noise_state = self
            .noise_state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        let white = (self.noise_state >> 8) as f32 / (1 << 23) as f32 - 1.0;
        s += white * self.noise;
        self.noise *= self.noise_decay;

        if self.attack < 1.0 {
            self.attack = (self.attack + self.attack_step).min(1.0);
        }
        if self.stage == Stage::Released {
            self.release *= self.release_decay;
        }
        s * self.gain * self.attack * self.release
    }

    /// Corrige el desvío numérico de los osciladores por rotación.
    fn renormalize(&mut self) {
        for p in &mut self.partials[..self.partial_count] {
            let mag = (p.re * p.re + p.im * p.im).sqrt();
            if mag > 0.0 {
                p.re /= mag;
                p.im /= mag;
            }
        }
    }

    fn level(&self) -> f32 {
        let partials: f32 = self.partials[..self.partial_count]
            .iter()
            .map(|p| p.amp)
            .sum();
        (partials + self.noise) * self.gain * self.release
    }

    fn finished(&self) -> bool {
        self.attack >= 1.0 && self.level() < SILENCE
    }
}

/// Banco de voces del piano. Vive en el hilo de audio: no reserva memoria
/// después de crearse.
pub struct PianoSynth {
    sample_rate: f32,
    voices: Vec<Voice>,
    sustain: bool,
    counter: u64,
    since_renormalize: u32,
}

impl PianoSynth {
    pub fn new(sample_rate: u32) -> Self {
        PianoSynth {
            sample_rate: sample_rate as f32,
            voices: Vec::with_capacity(MAX_VOICES),
            sustain: false,
            counter: 0,
            since_renormalize: 0,
        }
    }

    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        if velocity == 0 {
            self.note_off(pitch);
            return;
        }
        // Volver a tocar una tecla apaga la cuerda que ya sonaba.
        for v in self.voices.iter_mut().filter(|v| v.pitch == pitch) {
            v.stage = Stage::Released;
        }
        if self.voices.len() == MAX_VOICES {
            // Se roba la voz más silenciosa, o la más vieja si empatan.
            let victim = self
                .voices
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| a.level().total_cmp(&b.level()).then(a.age.cmp(&b.age)))
                .map(|(i, _)| i)
                .expect("hay voces");
            self.voices.swap_remove(victim);
        }
        self.counter += 1;
        self.voices
            .push(Voice::new(pitch, velocity, self.sample_rate, self.counter));
    }

    pub fn note_off(&mut self, pitch: u8) {
        let stage = if self.sustain {
            Stage::Sustained
        } else {
            Stage::Released
        };
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.pitch == pitch && v.stage == Stage::Held)
        {
            v.stage = stage;
        }
    }

    pub fn set_sustain(&mut self, down: bool) {
        self.sustain = down;
        if !down {
            for v in self
                .voices
                .iter_mut()
                .filter(|v| v.stage == Stage::Sustained)
            {
                v.stage = Stage::Released;
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        self.sustain = false;
        for v in &mut self.voices {
            v.stage = Stage::Released;
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.len()
    }

    /// Suma una muestra mono de todas las voces.
    pub fn next_sample(&mut self) -> f32 {
        let mut out = 0.0;
        for v in &mut self.voices {
            out += v.next();
        }
        self.since_renormalize += 1;
        if self.since_renormalize >= 1024 {
            self.since_renormalize = 0;
            for v in &mut self.voices {
                v.renormalize();
            }
            self.voices.retain(|v| !v.finished());
        }
        out
    }

    /// Llena un buffer mono.
    pub fn render(&mut self, out: &mut [f32]) {
        for s in out {
            *s = self.next_sample();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn render(synth: &mut PianoSynth, seconds: f32) -> Vec<f32> {
        let mut buf = vec![0.0; (seconds * RATE as f32) as usize];
        synth.render(&mut buf);
        buf
    }

    #[test]
    fn a_note_sounds_and_stays_in_range() {
        let mut synth = PianoSynth::new(RATE);
        synth.note_on(60, 100);
        let out = render(&mut synth, 0.5);
        assert!(rms(&out) > 0.01);
        assert!(out.iter().all(|s| s.abs() < 1.0));
    }

    #[test]
    fn louder_velocity_is_louder() {
        let mut soft = PianoSynth::new(RATE);
        soft.note_on(60, 30);
        let mut hard = PianoSynth::new(RATE);
        hard.note_on(60, 120);
        assert!(rms(&render(&mut hard, 0.2)) > 3.0 * rms(&render(&mut soft, 0.2)));
    }

    #[test]
    fn releasing_the_key_silences_the_note() {
        let mut synth = PianoSynth::new(RATE);
        synth.note_on(60, 100);
        render(&mut synth, 0.2);
        synth.note_off(60);
        render(&mut synth, 1.5);
        assert_eq!(synth.active_voices(), 0);
    }

    #[test]
    fn sustain_pedal_keeps_the_note_until_it_is_lifted() {
        let mut synth = PianoSynth::new(RATE);
        synth.set_sustain(true);
        synth.note_on(48, 100);
        render(&mut synth, 0.1);
        synth.note_off(48);
        let held = render(&mut synth, 1.0);
        assert!(rms(&held[held.len() - 4800..]) > 0.005);
        assert_eq!(synth.active_voices(), 1);

        synth.set_sustain(false);
        render(&mut synth, 1.5);
        assert_eq!(synth.active_voices(), 0);
    }

    #[test]
    fn voice_count_is_capped() {
        let mut synth = PianoSynth::new(RATE);
        synth.set_sustain(true);
        for i in 0..200u32 {
            synth.note_on(21 + (i % 88) as u8, 90);
        }
        assert_eq!(synth.active_voices(), MAX_VOICES);
        let out = render(&mut synth, 0.05);
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn retriggering_a_key_releases_the_previous_strike() {
        let mut synth = PianoSynth::new(RATE);
        synth.note_on(60, 100);
        synth.note_on(60, 100);
        assert_eq!(synth.active_voices(), 2);
        render(&mut synth, 1.0);
        assert_eq!(synth.active_voices(), 1);
    }
}
