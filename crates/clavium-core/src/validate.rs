//! Chequeos automáticos de una pieza, previos a la revisión humana.

use std::fmt;

use crate::model::{Hand, PIANO_HIGHEST, PIANO_LOWEST, Piece};

/// Tempos fuera de este rango (en BPM) casi siempre son un error del archivo.
pub const MIN_REASONABLE_BPM: f64 = 20.0;
pub const MAX_REASONABLE_BPM: f64 = 400.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// La pieza no se puede publicar así.
    Error,
    /// Hay que revisarlo, pero puede ser intencional.
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IssueKind {
    NoNotes,
    OutOfRange {
        pitch: u8,
    },
    ZeroDuration {
        pitch: u8,
    },
    /// Dos notas de la misma altura se superponen.
    Overlap {
        pitch: u8,
    },
    ExtremeTempo {
        bpm: f64,
    },
    UnknownHand {
        pitch: u8,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub severity: Severity,
    pub kind: IssueKind,
    pub tick: u64,
    /// Índice en `Piece::notes`, si el problema es de una nota.
    pub note_index: Option<usize>,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "aviso",
        };
        write!(f, "{level} en el tick {}: ", self.tick)?;
        match &self.kind {
            IssueKind::NoNotes => write!(f, "la pieza no tiene notas"),
            IssueKind::OutOfRange { pitch } => {
                write!(f, "la nota {pitch} está fuera del rango del piano")
            }
            IssueKind::ZeroDuration { pitch } => write!(f, "la nota {pitch} dura 0 ticks"),
            IssueKind::Overlap { pitch } => {
                write!(f, "dos notas {pitch} se superponen")
            }
            IssueKind::ExtremeTempo { bpm } => write!(f, "tempo extremo de {bpm:.1} BPM"),
            IssueKind::UnknownHand { pitch } => write!(f, "la nota {pitch} no tiene mano asignada"),
        }
    }
}

/// Devuelve los problemas encontrados, ordenados por tick.
pub fn validate(piece: &Piece) -> Vec<Issue> {
    let mut issues = Vec::new();

    if piece.notes.is_empty() {
        issues.push(Issue {
            severity: Severity::Error,
            kind: IssueKind::NoNotes,
            tick: 0,
            note_index: None,
        });
    }

    for change in piece.tempo.changes() {
        let bpm = piece.tempo.bpm_at(change.tick);
        if !(MIN_REASONABLE_BPM..=MAX_REASONABLE_BPM).contains(&bpm) {
            issues.push(Issue {
                severity: Severity::Warning,
                kind: IssueKind::ExtremeTempo { bpm },
                tick: change.tick,
                note_index: None,
            });
        }
    }

    // Índices de notas por altura, en orden de inicio, para buscar superposiciones.
    let mut by_pitch: Vec<Vec<usize>> = vec![Vec::new(); 128];

    for (i, n) in piece.notes.iter().enumerate() {
        let note_issue = |severity, kind| Issue {
            severity,
            kind,
            tick: n.start_tick,
            note_index: Some(i),
        };
        if !(PIANO_LOWEST..=PIANO_HIGHEST).contains(&n.pitch) {
            issues.push(note_issue(
                Severity::Error,
                IssueKind::OutOfRange { pitch: n.pitch },
            ));
        }
        if n.end_tick <= n.start_tick {
            issues.push(note_issue(
                Severity::Error,
                IssueKind::ZeroDuration { pitch: n.pitch },
            ));
        }
        if n.hand == Hand::Unknown {
            issues.push(note_issue(
                Severity::Warning,
                IssueKind::UnknownHand { pitch: n.pitch },
            ));
        }
        if let Some(list) = by_pitch.get_mut(usize::from(n.pitch)) {
            list.push(i);
        }
    }

    for list in &mut by_pitch {
        list.sort_by_key(|&i| piece.notes[i].start_tick);
        for pair in list.windows(2) {
            let (a, b) = (&piece.notes[pair[0]], &piece.notes[pair[1]]);
            if b.start_tick < a.end_tick {
                issues.push(Issue {
                    severity: Severity::Error,
                    kind: IssueKind::Overlap { pitch: b.pitch },
                    tick: b.start_tick,
                    note_index: Some(pair[1]),
                });
            }
        }
    }

    issues.sort_by_key(|i| i.tick);
    issues
}

/// `true` si no hay ningún problema de severidad [`Severity::Error`].
pub fn is_publishable(issues: &[Issue]) -> bool {
    issues.iter().all(|i| i.severity != Severity::Error)
}
