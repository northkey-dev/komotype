//! SQLite storage. Only counts are stored: never text, never key sequences.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA_VERSION: i32 = 1;

pub struct Db {
    conn: Connection,
}

/// Key counts per device and minute (`minute` = Unix time / 60).
#[derive(Debug, Default, Clone, Copy)]
pub struct MinuteCounts {
    pub chars: u32,
    pub backspaces: u32,
    pub keys: u32,
}

#[derive(Debug, Default)]
pub struct DayTotals {
    pub main: MinuteCounts,
    pub all_keys: u32,
    pub active_ms: i64,
    pub best_wpm: Option<f32>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let version: i32 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version >= SCHEMA_VERSION {
            return Ok(());
        }
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS devices(
                id INTEGER PRIMARY KEY,
                key TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                vid INTEGER,
                pid INTEGER
            );
            CREATE TABLE IF NOT EXISTS minutes(
                device_id INTEGER NOT NULL REFERENCES devices(id),
                minute INTEGER NOT NULL,
                chars INTEGER NOT NULL,
                backspaces INTEGER NOT NULL,
                keys INTEGER NOT NULL,
                PRIMARY KEY(device_id, minute)
            );
            CREATE TABLE IF NOT EXISTS bursts(
                id INTEGER PRIMARY KEY,
                device_id INTEGER NOT NULL REFERENCES devices(id),
                start_ms INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL,
                chars INTEGER NOT NULL,
                backspaces INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS bursts_start ON bursts(start_ms);
            CREATE TABLE IF NOT EXISTS key_counts(
                device_id INTEGER NOT NULL REFERENCES devices(id),
                day TEXT NOT NULL,
                vkey INTEGER NOT NULL,
                count INTEGER NOT NULL,
                PRIMARY KEY(device_id, day, vkey)
            );
            CREATE TABLE IF NOT EXISTS battery(
                ts_ms INTEGER NOT NULL,
                central INTEGER,
                peripheral INTEGER
            );
            PRAGMA user_version = 1;",
        )?;
        Ok(())
    }

    pub fn device_id(
        &self,
        key: &str,
        name: &str,
        vid: Option<u16>,
        pid: Option<u16>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO devices(key, name, vid, pid) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET name = excluded.name",
            params![key, name, vid, pid],
        )?;
        Ok(self
            .conn
            .query_row("SELECT id FROM devices WHERE key = ?1", [key], |r| r.get(0))?)
    }

    /// Finds a stored device by VID/PID and optional product-name substring.
    pub fn find_device(&self, vid: u16, pid: u16, product: Option<&str>) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM devices WHERE vid = ?1 AND pid = ?2 ORDER BY id")?;
        let rows = stmt.query_map(params![vid, pid], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        let want = product.map(str::to_lowercase);
        for row in rows {
            let (id, name) = row?;
            if want
                .as_ref()
                .is_none_or(|w| name.to_lowercase().contains(w))
            {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Writes buffered counts in one transaction.
    pub fn flush(
        &mut self,
        minutes: &[((i64, i64), MinuteCounts)],
        keys: &[((i64, String, u16), u32)],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO minutes(device_id, minute, chars, backspaces, keys) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(device_id, minute) DO UPDATE SET
                    chars = chars + excluded.chars,
                    backspaces = backspaces + excluded.backspaces,
                    keys = keys + excluded.keys",
            )?;
            for ((device, minute), c) in minutes {
                stmt.execute(params![device, minute, c.chars, c.backspaces, c.keys])?;
            }
            let mut stmt = tx.prepare_cached(
                "INSERT INTO key_counts(device_id, day, vkey, count) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(device_id, day, vkey) DO UPDATE SET count = count + excluded.count",
            )?;
            for ((device, day, vkey), count) in keys {
                stmt.execute(params![device, day, vkey, count])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn add_burst(
        &self,
        device: i64,
        start_ms: i64,
        duration_ms: i64,
        chars: u32,
        backspaces: u32,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bursts(device_id, start_ms, duration_ms, chars, backspaces) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![device, start_ms, duration_ms, chars, backspaces],
        )?;
        Ok(())
    }

    pub fn add_battery(
        &self,
        ts_ms: i64,
        central: Option<u8>,
        peripheral: Option<u8>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO battery(ts_ms, central, peripheral) VALUES (?1, ?2, ?3)",
            params![ts_ms, central, peripheral],
        )?;
        Ok(())
    }

    /// Totals since `since_ms` for the main device (`None` = no main device configured).
    pub fn day_totals(
        &self,
        main: Option<i64>,
        since_ms: i64,
        min_record_ms: i64,
    ) -> Result<DayTotals> {
        let since_minute = since_ms / 60_000;
        let mut totals = DayTotals {
            all_keys: self.conn.query_row(
                "SELECT COALESCE(SUM(keys), 0) FROM minutes WHERE minute >= ?1",
                [since_minute],
                |r| r.get(0),
            )?,
            ..Default::default()
        };
        let Some(main) = main else { return Ok(totals) };
        totals.main = self.conn.query_row(
            "SELECT COALESCE(SUM(chars), 0), COALESCE(SUM(backspaces), 0), COALESCE(SUM(keys), 0)
             FROM minutes WHERE device_id = ?1 AND minute >= ?2",
            params![main, since_minute],
            |r| {
                Ok(MinuteCounts {
                    chars: r.get(0)?,
                    backspaces: r.get(1)?,
                    keys: r.get(2)?,
                })
            },
        )?;
        totals.active_ms = self.conn.query_row(
            "SELECT COALESCE(SUM(duration_ms), 0) FROM bursts WHERE device_id = ?1 AND start_ms >= ?2",
            params![main, since_ms],
            |r| r.get(0),
        )?;
        totals.best_wpm = self.best_wpm(main, Some(since_ms), min_record_ms)?;
        Ok(totals)
    }

    pub fn best_wpm(
        &self,
        device: i64,
        since_ms: Option<i64>,
        min_duration_ms: i64,
    ) -> Result<Option<f32>> {
        Ok(self
            .conn
            .query_row(
                "SELECT MAX(chars / 5.0 / (duration_ms / 60000.0)) FROM bursts
                 WHERE device_id = ?1 AND start_ms >= ?2 AND duration_ms >= ?3",
                params![device, since_ms.unwrap_or(0), min_duration_ms],
                |r| r.get::<_, Option<f64>>(0),
            )
            .optional()?
            .flatten()
            .map(|v| v as f32))
    }
}
