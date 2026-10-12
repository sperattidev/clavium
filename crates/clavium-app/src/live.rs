//! Estado de lo que el usuario está tocando en este momento.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Up,
    Down {
        velocity: u8,
    },
    /// La tecla se soltó pero sigue sonando por el pedal de sustain.
    Sustained {
        velocity: u8,
    },
}

#[derive(Debug, Clone)]
pub struct LiveKeys {
    keys: [KeyState; 128],
    sustain: bool,
}

impl Default for LiveKeys {
    fn default() -> Self {
        LiveKeys {
            keys: [KeyState::Up; 128],
            sustain: false,
        }
    }
}

impl LiveKeys {
    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        if let Some(key) = self.keys.get_mut(usize::from(pitch)) {
            *key = KeyState::Down { velocity };
        }
    }

    pub fn note_off(&mut self, pitch: u8) {
        if let Some(key) = self.keys.get_mut(usize::from(pitch))
            && let KeyState::Down { velocity } = *key
        {
            *key = if self.sustain {
                KeyState::Sustained { velocity }
            } else {
                KeyState::Up
            };
        }
    }

    pub fn set_sustain(&mut self, down: bool) {
        self.sustain = down;
        if !down {
            for key in &mut self.keys {
                if matches!(key, KeyState::Sustained { .. }) {
                    *key = KeyState::Up;
                }
            }
        }
    }

    pub fn sustain(&self) -> bool {
        self.sustain
    }

    pub fn get(&self, pitch: u8) -> KeyState {
        self.keys
            .get(usize::from(pitch))
            .copied()
            .unwrap_or(KeyState::Up)
    }

    /// Teclas apretadas ahora, de grave a agudo.
    pub fn down(&self) -> impl Iterator<Item = u8> + '_ {
        (0..=127u8).filter(|&p| matches!(self.get(p), KeyState::Down { .. }))
    }

    /// Suelta todo, por ejemplo cuando se desconecta el teclado.
    pub fn release_all(&mut self) {
        *self = LiveKeys::default();
    }
}

/// Últimas mediciones del tiempo entre que llega una nota y se arma el
/// cuadro que la muestra, en milisegundos.
#[derive(Debug, Clone, Default)]
pub struct LatencyStats {
    samples: VecDeque<f32>,
}

impl LatencyStats {
    const CAPACITY: usize = 200;

    pub fn record(&mut self, ms: f32) {
        if self.samples.len() == Self::CAPACITY {
            self.samples.pop_front();
        }
        self.samples.push_back(ms);
    }

    pub fn count(&self) -> usize {
        self.samples.len()
    }

    pub fn mean(&self) -> Option<f32> {
        (!self.samples.is_empty())
            .then(|| self.samples.iter().sum::<f32>() / self.samples.len() as f32)
    }

    pub fn max(&self) -> Option<f32> {
        self.samples.iter().copied().reduce(f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pedal_keeps_released_keys_sounding() {
        let mut keys = LiveKeys::default();
        keys.set_sustain(true);
        keys.note_on(60, 90);
        keys.note_off(60);
        assert_eq!(keys.get(60), KeyState::Sustained { velocity: 90 });
        assert_eq!(keys.down().count(), 0);

        keys.note_on(64, 70);
        keys.set_sustain(false);
        assert_eq!(keys.get(60), KeyState::Up);
        assert_eq!(keys.get(64), KeyState::Down { velocity: 70 });
    }

    #[test]
    fn note_off_without_pedal_releases_the_key() {
        let mut keys = LiveKeys::default();
        keys.note_on(60, 90);
        keys.note_on(67, 90);
        assert_eq!(keys.down().collect::<Vec<_>>(), vec![60, 67]);
        keys.note_off(60);
        assert_eq!(keys.get(60), KeyState::Up);
        keys.release_all();
        assert_eq!(keys.down().count(), 0);
    }

    #[test]
    fn latency_stats_keep_a_rolling_window() {
        let mut stats = LatencyStats::default();
        assert_eq!(stats.mean(), None);
        for i in 0..250 {
            stats.record(i as f32);
        }
        assert_eq!(stats.count(), 200);
        assert_eq!(stats.max(), Some(249.0));
        assert_eq!(stats.mean(), Some(149.5));
    }
}
