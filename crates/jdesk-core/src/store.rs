//! The local cache: `cache.sqlite` in the data folder. It only ever holds what Jira said, so
//! deleting it loses nothing but the time to download again. (Your notes and queued changes
//! live in the sidecar files, never here.)

use crate::model::Issue;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::path::Path;

/// Bump when [`Issue`]'s shape changes: an older cache is then downloaded again.
pub const SCHEMA: u32 = 2;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("cannot open the cache {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS issues (
                 key TEXT PRIMARY KEY, updated TEXT NOT NULL, json TEXT NOT NULL);",
        )?;
        let store = Self { conn };
        if store.meta::<u32>("schema")? != Some(SCHEMA) {
            store.clear()?;
            store.set_meta("schema", &SCHEMA)?;
        }
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE issues (key TEXT PRIMARY KEY, updated TEXT NOT NULL, json TEXT NOT NULL);",
        )?;
        Ok(Self { conn })
    }

    /// Forgets every issue and sync marker (the next sync downloads everything).
    pub fn clear(&self) -> Result<()> {
        self.conn
            .execute_batch("DELETE FROM issues; DELETE FROM meta;")?;
        Ok(())
    }

    pub fn meta<T: DeserializeOwned>(&self, k: &str) -> Result<Option<T>> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT v FROM meta WHERE k = ?1", [k], |r| r.get(0))
            .optional()?;
        Ok(v.and_then(|v| serde_json::from_str(&v).ok()))
    }

    pub fn set_meta<T: Serialize>(&self, k: &str, v: &T) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = ?2",
            params![k, serde_json::to_string(v)?],
        )?;
        Ok(())
    }

    /// Every cached issue. Rows that no longer parse are skipped (and fetched again by the
    /// next full sync, since they are missing from [`Store::versions`]).
    pub fn load_all(&self) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare("SELECT json FROM issues")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Ok(i) = serde_json::from_str::<Issue>(&row?) {
                out.push(i);
            }
        }
        Ok(out)
    }

    pub fn get(&self, key: &str) -> Result<Option<Issue>> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT json FROM issues WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(v.and_then(|v| serde_json::from_str(&v).ok()))
    }

    /// `updated` of every cached issue, by key.
    pub fn versions(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self.conn.prepare("SELECT key, updated, json FROM issues")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (k, u, json) = row?;
            if serde_json::from_str::<Issue>(&json).is_ok() {
                out.insert(k, u);
            }
        }
        Ok(out)
    }

    pub fn upsert(&mut self, issues: &[Issue]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO issues (key, updated, json) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET updated = ?2, json = ?3",
            )?;
            for i in issues {
                stmt.execute(params![i.key, i.updated, serde_json::to_string(i)?])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove(&mut self, keys: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for k in keys {
            tx.execute("DELETE FROM issues WHERE key = ?1", [k])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn count(&self) -> Result<usize> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM issues", [], |r| r.get::<_, i64>(0))?
            as usize)
    }
}
