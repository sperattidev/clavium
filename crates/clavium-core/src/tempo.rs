//! Mapa de tempo: conversión exacta entre ticks MIDI y segundos.
//!
//! El tiempo de un tick es la suma de lo que dura cada tramo de tempo hasta él:
//! `t = Σ Δticks_i · μ_i / (PPQ · 10⁶)`, con `μ` en microsegundos por negra.
//! El numerador se acumula en enteros, así que no hay error de redondeo
//! acumulado aunque la pieza tenga miles de cambios de tempo.

use serde::{Deserialize, Serialize};

/// Tempo por defecto de un MIDI sin meta-evento de tempo: 120 BPM.
pub const DEFAULT_US_PER_QUARTER: u32 = 500_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TempoChange {
    pub tick: u64,
    pub us_per_quarter: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TempoError {
    #[error("la resolución (PPQ) no puede ser 0")]
    ZeroPpq,
    #[error("tempo inválido de 0 µs por negra en el tick {0}")]
    ZeroTempo(u64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "TempoMapRaw", into = "TempoMapRaw")]
pub struct TempoMap {
    ppq: u16,
    changes: Vec<TempoChange>,
    /// `acc[i]` = Σ Δticks·μ desde el tick 0 hasta `changes[i].tick`.
    acc: Vec<u128>,
}

#[derive(Serialize, Deserialize)]
struct TempoMapRaw {
    ppq: u16,
    changes: Vec<TempoChange>,
}

impl TryFrom<TempoMapRaw> for TempoMap {
    type Error = TempoError;

    fn try_from(raw: TempoMapRaw) -> Result<Self, Self::Error> {
        TempoMap::new(raw.ppq, raw.changes)
    }
}

impl From<TempoMap> for TempoMapRaw {
    fn from(map: TempoMap) -> Self {
        TempoMapRaw {
            ppq: map.ppq,
            changes: map.changes,
        }
    }
}

impl TempoMap {
    /// Crea un mapa a partir de cambios en cualquier orden. Si hay dos cambios
    /// en el mismo tick gana el último; si no hay ninguno en el tick 0 se usa
    /// el tempo por defecto hasta el primero.
    pub fn new(ppq: u16, mut changes: Vec<TempoChange>) -> Result<Self, TempoError> {
        if ppq == 0 {
            return Err(TempoError::ZeroPpq);
        }
        if let Some(c) = changes.iter().find(|c| c.us_per_quarter == 0) {
            return Err(TempoError::ZeroTempo(c.tick));
        }

        changes.sort_by_key(|c| c.tick); // estable: conserva el orden original en empates
        let mut dedup: Vec<TempoChange> = Vec::with_capacity(changes.len() + 1);
        for c in changes {
            match dedup.last_mut() {
                Some(last) if last.tick == c.tick => *last = c,
                _ => dedup.push(c),
            }
        }
        if dedup.first().is_none_or(|c| c.tick > 0) {
            dedup.insert(
                0,
                TempoChange {
                    tick: 0,
                    us_per_quarter: DEFAULT_US_PER_QUARTER,
                },
            );
        }

        let mut acc = Vec::with_capacity(dedup.len());
        let mut sum: u128 = 0;
        for (i, c) in dedup.iter().enumerate() {
            if i > 0 {
                let prev = dedup[i - 1];
                sum += u128::from(c.tick - prev.tick) * u128::from(prev.us_per_quarter);
            }
            acc.push(sum);
        }

        Ok(TempoMap {
            ppq,
            changes: dedup,
            acc,
        })
    }

    /// Mapa con un tempo constante.
    pub fn constant(ppq: u16, us_per_quarter: u32) -> Result<Self, TempoError> {
        Self::new(
            ppq,
            vec![TempoChange {
                tick: 0,
                us_per_quarter,
            }],
        )
    }

    pub fn ppq(&self) -> u16 {
        self.ppq
    }

    pub fn changes(&self) -> &[TempoChange] {
        &self.changes
    }

    fn segment_at_tick(&self, tick: u64) -> usize {
        self.changes.partition_point(|c| c.tick <= tick) - 1
    }

    /// Segundos desde el inicio de la pieza hasta `tick`.
    pub fn tick_to_seconds(&self, tick: u64) -> f64 {
        let i = self.segment_at_tick(tick);
        let c = self.changes[i];
        let numer = self.acc[i] + u128::from(tick - c.tick) * u128::from(c.us_per_quarter);
        numer as f64 / (f64::from(self.ppq) * 1e6)
    }

    /// Tick (fraccionario) que corresponde a `seconds`. Inversa de
    /// [`TempoMap::tick_to_seconds`]; los tiempos negativos dan 0.
    pub fn seconds_to_tick(&self, seconds: f64) -> f64 {
        let target = seconds.max(0.0) * f64::from(self.ppq) * 1e6;
        let i = self.acc.partition_point(|&a| a as f64 <= target) - 1;
        let c = self.changes[i];
        c.tick as f64 + (target - self.acc[i] as f64) / f64::from(c.us_per_quarter)
    }

    /// Microsegundos por negra vigentes en `tick`.
    pub fn us_per_quarter_at(&self, tick: u64) -> u32 {
        self.changes[self.segment_at_tick(tick)].us_per_quarter
    }

    /// Tempo en BPM (negras por minuto) vigente en `tick`.
    pub fn bpm_at(&self, tick: u64) -> f64 {
        60_000_000.0 / f64::from(self.us_per_quarter_at(tick))
    }
}
