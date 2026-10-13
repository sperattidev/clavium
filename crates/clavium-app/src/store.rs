//! Progreso y preferencias guardados en una base SQLite local.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use clavium_eval::{HandFilter, PracticeMode, Summary};
use rusqlite::{Connection, OptionalExtension, params};

/// Versión del esquema de la base. Cada cambio de tablas suma una migración.
const SCHEMA_VERSION: i32 = 2;

/// Precisión con la que una velocidad cuenta como dominada.
pub const MASTERED_ACCURACY: f64 = 0.9;

/// Una sesión terminada, para guardar en el historial.
#[derive(Debug, Clone)]
pub struct SessionRecord<'a> {
    pub piece: &'a str,
    pub mode: PracticeMode,
    pub hands: HandFilter,
    pub speed: f64,
    /// Nombre de la sección practicada, o `None` si fue la pieza completa.
    pub section: Option<&'a str>,
    pub summary: &'a Summary,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PieceStats {
    pub sessions: u32,
    /// Mejor precisión en modo ritmo, con ambas manos y la pieza completa.
    pub best_accuracy: Option<f64>,
    /// Mayor velocidad tocada con al menos 90 % de precisión en esas condiciones.
    pub best_speed: Option<f64>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Abre la base en la carpeta de datos del usuario.
    pub fn open_default() -> rusqlite::Result<Self> {
        let path = default_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::open(&path)
    }

    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS settings (
                     key   TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS sessions (
                     id        INTEGER PRIMARY KEY,
                     piece     TEXT NOT NULL,
                     played_at INTEGER NOT NULL,
                     mode      TEXT NOT NULL,
                     hands     TEXT NOT NULL,
                     speed     REAL NOT NULL,
                     section   TEXT,
                     accuracy  REAL,
                     perfect   INTEGER NOT NULL,
                     good      INTEGER NOT NULL,
                     off_time  INTEGER NOT NULL,
                     missed    INTEGER NOT NULL,
                     wrong     INTEGER NOT NULL,
                     stars     INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS sessions_piece ON sessions (piece);",
            )?;
        }
        if version < 2 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS reviews (
                     piece       TEXT PRIMARY KEY,
                     reviewer    TEXT NOT NULL,
                     reviewed_at INTEGER NOT NULL
                 );",
            )?;
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Store { conn })
    }

    pub fn setting<T: FromStr>(&self, key: &str) -> Option<T> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
    }

    pub fn set_setting(&self, key: &str, value: impl ToString) {
        let _ = self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value.to_string()],
        );
    }

    pub fn record_session(&self, record: &SessionRecord<'_>) -> rusqlite::Result<()> {
        let s = record.summary;
        let now = unix_now();
        self.conn.execute(
            "INSERT INTO sessions (piece, played_at, mode, hands, speed, section, accuracy,
                                   perfect, good, off_time, missed, wrong, stars)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                record.piece,
                now,
                mode_name(record.mode),
                hands_name(record.hands),
                record.speed,
                record.section,
                s.accuracy,
                s.perfect as i64,
                s.good as i64,
                s.off_time as i64,
                s.missed as i64,
                s.wrong as i64,
                i64::from(s.stars),
            ],
        )?;
        Ok(())
    }

    /// Marca una pieza como revisada: alguien la tocó completa y las notas
    /// coinciden con la partitura.
    pub fn mark_reviewed(&self, piece: &str, reviewer: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO reviews (piece, reviewer, reviewed_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (piece) DO UPDATE SET reviewer = excluded.reviewer,
                                               reviewed_at = excluded.reviewed_at",
            params![piece, reviewer, unix_now()],
        )?;
        Ok(())
    }

    pub fn unmark_reviewed(&self, piece: &str) {
        let _ = self
            .conn
            .execute("DELETE FROM reviews WHERE piece = ?1", [piece]);
    }

    /// Revisiones guardadas: (pieza, revisor, fecha AAAA-MM-DD).
    pub fn reviews(&self) -> Vec<(String, String, String)> {
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT piece, reviewer, reviewed_at FROM reviews ORDER BY piece")
        else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                date(r.get::<_, i64>(2)?),
            ))
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    pub fn piece_stats(&self, piece: &str) -> PieceStats {
        let sessions: u32 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE piece = ?1",
                [piece],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let full_rhythm = "piece = ?1 AND mode = 'rhythm' AND hands = 'both' AND section IS NULL";
        let best_accuracy: Option<f64> = self
            .conn
            .query_row(
                &format!("SELECT MAX(accuracy) FROM sessions WHERE {full_rhythm}"),
                [piece],
                |r| r.get(0),
            )
            .unwrap_or(None);
        let best_speed: Option<f64> = self
            .conn
            .query_row(
                &format!("SELECT MAX(speed) FROM sessions WHERE {full_rhythm} AND accuracy >= ?2"),
                params![piece, MASTERED_ACCURACY],
                |r| r.get(0),
            )
            .unwrap_or(None);
        PieceStats {
            sessions,
            best_accuracy,
            best_speed,
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Fecha AAAA-MM-DD (UTC) de un instante Unix.
fn date(unix: i64) -> String {
    // Algoritmo de días civiles de Howard Hinnant.
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn default_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Clavium")
        .join("clavium.db")
}

fn mode_name(mode: PracticeMode) -> &'static str {
    match mode {
        PracticeMode::Wait => "wait",
        PracticeMode::Rhythm => "rhythm",
        PracticeMode::Listen => "listen",
    }
}

fn hands_name(hands: HandFilter) -> &'static str {
    match hands {
        HandFilter::Both => "both",
        HandFilter::Right => "right",
        HandFilter::Left => "left",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(accuracy: f64) -> Summary {
        Summary {
            total: 10,
            perfect: 8,
            good: 1,
            off_time: 0,
            missed: 1,
            wrong: 0,
            accuracy: Some(accuracy),
            mean_offset_ms: None,
            stars: 2,
            weakest_bars: Vec::new(),
        }
    }

    fn record<'a>(s: &'a Summary, speed: f64, section: Option<&'a str>) -> SessionRecord<'a> {
        SessionRecord {
            piece: "estrellita",
            mode: PracticeMode::Rhythm,
            hands: HandFilter::Both,
            speed,
            section,
            summary: s,
        }
    }

    #[test]
    fn settings_round_trip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.setting::<f32>("volume"), None);
        store.set_setting("volume", 0.6f32);
        store.set_setting("volume", 0.4f32);
        assert_eq!(store.setting::<f32>("volume"), Some(0.4));
        store.set_setting("port", "SAMSON Carbon49");
        assert_eq!(
            store.setting::<String>("port").as_deref(),
            Some("SAMSON Carbon49")
        );
    }

    #[test]
    fn stats_count_only_full_rhythm_runs_for_records() {
        let store = Store::open_in_memory().unwrap();
        let (a, b, c, d) = (summary(0.95), summary(0.8), summary(0.99), summary(0.92));
        store.record_session(&record(&a, 0.75, None)).unwrap();
        store.record_session(&record(&b, 1.0, None)).unwrap();
        // Una sección no cuenta para el récord de la pieza.
        store
            .record_session(&record(&c, 1.0, Some("Frase A")))
            .unwrap();
        store.record_session(&record(&d, 0.5, None)).unwrap();

        let stats = store.piece_stats("estrellita");
        assert_eq!(stats.sessions, 4);
        assert_eq!(stats.best_accuracy, Some(0.95));
        assert_eq!(stats.best_speed, Some(0.75));
        assert_eq!(store.piece_stats("otra"), PieceStats::default());
    }

    #[test]
    fn reviews_are_saved_and_listed() {
        let store = Store::open_in_memory().unwrap();
        store.mark_reviewed("mutopia-x", "Matías").unwrap();
        store.mark_reviewed("mutopia-x", "Otra persona").unwrap();
        let reviews = store.reviews();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].1, "Otra persona");
        assert_eq!(reviews[0].2.len(), 10);
        store.unmark_reviewed("mutopia-x");
        assert!(store.reviews().is_empty());
    }

    #[test]
    fn dates_are_formatted_in_utc() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_791_849_600), "2026-10-13");
        assert_eq!(date(951_782_400), "2000-02-29");
    }

    #[test]
    fn reopening_keeps_the_data() {
        let dir = std::env::temp_dir().join(format!("clavium-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db.sqlite");
        {
            let store = Store::open(&path).unwrap();
            store.set_setting("curve", "light");
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.setting::<String>("curve").as_deref(), Some("light"));
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
