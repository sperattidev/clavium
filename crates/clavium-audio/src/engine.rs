//! Salida de audio: abre la placa de sonido y alimenta el piano desde el
//! hilo de audio. La comunicación con ese hilo usa colas sin bloqueo
//! (`rtrb`), para no trabar nunca el callback.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, Stream, StreamConfig, SupportedBufferSize};

use crate::synth::PianoSynth;

/// Buffer pedido a la placa: 256 muestras a 48 kHz son unos 5,3 ms.
const PREFERRED_BUFFER: u32 = 256;
const COMMAND_QUEUE: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    NoteOn { pitch: u8, velocity: u8 },
    NoteOff { pitch: u8 },
    Sustain(bool),
    AllNotesOff,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no hay ninguna salida de audio")]
    NoDevice,
    #[error("no se pudo configurar la salida de audio: {0}")]
    Config(String),
    #[error("no se pudo abrir la salida de audio: {0}")]
    Stream(String),
}

/// Datos de la salida abierta, para mostrar en el diagnóstico.
#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// `None` si el sistema eligió el tamaño de buffer.
    pub buffer_frames: Option<u32>,
}

pub struct AudioEngine {
    _stream: Stream,
    commands: rtrb::Producer<(Command, Instant)>,
    latencies: rtrb::Consumer<f32>,
    volume: Arc<AtomicU32>,
    failed: Arc<AtomicBool>,
    info: OutputInfo,
}

impl AudioEngine {
    /// Abre la salida de audio por defecto.
    pub fn start() -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "Salida por defecto".to_string());
        let supported = device
            .default_output_config()
            .map_err(|e| AudioError::Config(e.to_string()))?;
        if supported.sample_format() != SampleFormat::F32 {
            return Err(AudioError::Config(format!(
                "formato de muestra {:?} no soportado",
                supported.sample_format()
            )));
        }

        let buffer_frames = match supported.buffer_size() {
            SupportedBufferSize::Range { min, max } => Some(PREFERRED_BUFFER.clamp(*min, *max)),
            SupportedBufferSize::Unknown => None,
        };
        let config = StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: buffer_frames.map_or(BufferSize::Default, BufferSize::Fixed),
        };
        let info = OutputInfo {
            device: name,
            sample_rate: config.sample_rate,
            channels: config.channels,
            buffer_frames,
        };

        let (commands, mut command_rx) = rtrb::RingBuffer::<(Command, Instant)>::new(COMMAND_QUEUE);
        let (mut latency_tx, latencies) = rtrb::RingBuffer::<f32>::new(COMMAND_QUEUE);
        let volume = Arc::new(AtomicU32::new(0.8f32.to_bits()));
        let failed = Arc::new(AtomicBool::new(false));

        let mut synth = PianoSynth::new(config.sample_rate);
        let channels = usize::from(config.channels.max(1));
        let callback_volume = volume.clone();
        let error_flag = failed.clone();

        let stream = device
            .build_output_stream(
                config,
                move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                    let timestamp = info.timestamp();
                    let output_delay = timestamp.playback.duration_since(timestamp.callback);
                    let now = Instant::now();
                    while let Ok((command, at)) = command_rx.pop() {
                        match command {
                            Command::NoteOn { pitch, velocity } => {
                                synth.note_on(pitch, velocity);
                                let total = now.saturating_duration_since(at) + output_delay;
                                let _ = latency_tx.push(total.as_secs_f32() * 1000.0);
                            }
                            Command::NoteOff { pitch } => synth.note_off(pitch),
                            Command::Sustain(down) => synth.set_sustain(down),
                            Command::AllNotesOff => synth.all_notes_off(),
                        }
                    }
                    let volume = f32::from_bits(callback_volume.load(Ordering::Relaxed));
                    for frame in data.chunks_mut(channels) {
                        let s = soft_clip(synth.next_sample() * volume);
                        frame.fill(s);
                    }
                },
                move |_err| error_flag.store(true, Ordering::Relaxed),
                None,
            )
            .map_err(|e| AudioError::Stream(e.to_string()))?;
        stream
            .play()
            .map_err(|e| AudioError::Stream(e.to_string()))?;

        Ok(AudioEngine {
            _stream: stream,
            commands,
            latencies,
            volume,
            failed,
            info,
        })
    }

    pub fn send(&mut self, command: Command) {
        // Con la cola llena (más de 1024 eventos sin procesar) se descarta.
        let _ = self.commands.push((command, Instant::now()));
    }

    /// Volumen general, de 0 a 1.
    pub fn set_volume(&self, volume: f32) {
        self.volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn info(&self) -> &OutputInfo {
        &self.info
    }

    /// `true` si la placa informó un error (por ejemplo, se desconectó).
    pub fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Mediciones nuevas de tecla a sonido, en milisegundos: desde que la app
    /// manda la nota hasta que la placa la reproduce.
    pub fn drain_latencies(&mut self) -> impl Iterator<Item = f32> + '_ {
        std::iter::from_fn(|| self.latencies.pop().ok())
    }
}

/// Saturación suave para que muchas notas juntas no distorsionen de golpe.
fn soft_clip(x: f32) -> f32 {
    x / (1.0 + x.abs())
}
