//! Persistent state (JSON) so the tool can:
//! - remember which days it already processed (avoid re-filling on restart),
//! - keep a record of the report drafts it generated.
//!
//! Stored at `<log_dir>/state.json`.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DayRecord {
    processed_at: String,
    /// issue_key -> worklog seconds that were (or were planned to be) written.
    worklogs: BTreeMap<String, u64>,
    /// issue_key -> report text draft.
    reports: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    #[serde(default)]
    pub days: BTreeMap<String, DayRecord>,
}

impl State {
    pub fn path(log_dir: &Path) -> std::path::PathBuf {
        log_dir.join("state.json")
    }

    pub fn load(log_dir: &Path) -> State {
        let p = Self::path(log_dir);
        std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, log_dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(log_dir)?;
        let p = Self::path(log_dir);
        let s = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, s)?;
        Ok(())
    }

    /// Has this date already been processed (and written)?
    pub fn is_processed(&self, date: NaiveDate) -> bool {
        let k = date.format("%Y-%m-%d").to_string();
        self.days.contains_key(&k)
    }

    /// Mark a date processed, recording the worklogs + report drafts.
    pub fn mark_processed(&mut self, date: NaiveDate, worklogs: BTreeMap<String, u64>, reports: BTreeMap<String, String>) {
        let k = date.format("%Y-%m-%d").to_string();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        self.days.insert(k, DayRecord { processed_at: now, worklogs, reports });
    }

    pub fn record(&self, date: NaiveDate) -> Option<&DayRecord> {
        let k = date.format("%Y-%m-%d").to_string();
        self.days.get(&k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        static C: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = C.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("state-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_and_reload() {
        let d = dir();
        let mut st = State::default();
        let mut wl = BTreeMap::new();
        wl.insert("A-1".to_string(), 28800u64);
        let mut rp = BTreeMap::new();
        rp.insert("A-1".to_string(), "日报".to_string());
        let date = NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
        st.mark_processed(date, wl, rp);
        st.save(&d).unwrap();

        let loaded = State::load(&d);
        assert!(loaded.is_processed(date));
        assert_eq!(loaded.record(date).unwrap().worklogs.get("A-1"), Some(&28800));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_file_is_empty() {
        let d = dir();
        let st = State::load(&d);
        assert!(!st.is_processed(NaiveDate::from_ymd_opt(2026, 9, 9).unwrap()));
        let _ = std::fs::remove_dir_all(&d);
    }
}
