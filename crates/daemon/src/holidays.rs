//! Load + validate the external holidays JSON, and answer "is this date a
//! statutory holiday?".

use std::collections::HashSet;
use std::path::Path;

use chrono::NaiveDate;
use serde::Deserialize;

/// Outcome of a holiday lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolidayCheck {
    /// The date is a known holiday (and its name, if any).
    Holiday(String),
    /// Holidays were loaded and the date is not one of them.
    NotHoliday,
    /// No holiday data could be loaded (missing/corrupt) — caller should
    /// degrade to "skip only if no commits" and warn the user.
    Unavailable(String),
}

#[derive(Debug, Deserialize)]
struct YearFile {
    year: i32,
    holidays: Vec<HolidayEntry>,
}

#[derive(Debug, Deserialize)]
struct HolidayEntry {
    #[serde(rename = "date")]
    date: String,
    #[serde(default)]
    name: Option<String>,
}

/// Loaded holiday data keyed by date.
pub struct HolidaySet {
    dates: HashSet<NaiveDate>,
    names: std::collections::HashMap<NaiveDate, String>,
    sources: Vec<String>,
}

impl HolidaySet {
    pub fn empty() -> Self {
        Self { dates: HashSet::new(), names: std::collections::HashMap::new(), sources: vec![] }
    }

    pub fn is_holiday(&self, d: NaiveDate) -> Option<&str> {
        if let Some(n) = self.names.get(&d) {
            Some(n.as_str())
        } else if self.dates.contains(&d) {
            Some("(holiday)")
        } else {
            None
        }
    }
}

fn parse_date(s: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| format!("bad date '{}': {}", s, e))
}

/// Validate and ingest one year file's JSON.
fn ingest(text: &str, source: &str, expected_year: Option<i32>) -> Result<Vec<(NaiveDate, String)>, String> {
    let yf: YearFile = serde_json::from_str(text).map_err(|e| format!("{}: JSON parse: {}", source, e))?;
    if let Some(exp) = expected_year {
        if yf.year != exp {
            return Err(format!("{}: year field {} != file year {}", source, yf.year, exp));
        }
    }
    let mut out = Vec::new();
    for h in &yf.holidays {
        let d = parse_date(&h.date)?;
        let name = h.name.clone().unwrap_or_default();
        out.push((d, name));
    }
    Ok(out)
}

/// Load holiday data from a directory (`holidays-<year>.json` files, merged) or a
/// single file path. Missing → empty (NotHoliday). Corrupt → error.
pub fn load_for_date(path: &Path) -> Result<HolidaySet, String> {
    let mut set = HolidaySet::empty();

    let mut sources = Vec::new();
    if path.is_dir() {
        // Merge all holidays-*.json in the dir; membership is checked per-date.
        for entry in std::fs::read_dir(path).map_err(|e| format!("reading dir {}: {}", path.display(), e))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(y) = extract_year(&name) else { continue };
            let entries = ingest(&std::fs::read_to_string(entry.path()).map_err(|e| e.to_string())?, &name, Some(y))?;
            for (dd, nm) in entries {
                if set.dates.insert(dd) {
                    set.names.insert(dd, nm);
                }
            }
            sources.push(name);
        }
        set.sources = sources;
        return Ok(set);
    }

    // Not a directory: treat `path` as a single holidays file.
    if path.is_file() {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let expected = extract_year(&name);
        let entries = ingest(&text, &name, expected)?;
        for (dd, nm) in entries {
            if set.dates.insert(dd) {
                set.names.insert(dd, nm);
            }
        }
        return Ok(set);
    }

    // Neither dir nor file — no holiday data available (degrades gracefully).
    Ok(set)
}

fn extract_year(name: &str) -> Option<i32> {
    let stem = name.strip_suffix(".json")?;
    let y = stem.strip_prefix("holidays-")?;
    y.parse().ok()
}

/// High-level: answer is_holiday for a date, mapping load failures to Unavailable.
pub fn is_holiday(path: &Path, d: NaiveDate) -> HolidayCheck {
    match load_for_date(path) {
        Ok(set) => match set.is_holiday(d) {
            Some(name) => HolidayCheck::Holiday(name.to_string()),
            None => HolidayCheck::NotHoliday,
        },
        Err(e) => HolidayCheck::Unavailable(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        static C: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = C.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("hol-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn valid_file_hits_holiday() {
        let d = dir();
        std::fs::write(
            d.join("holidays-2026.json"),
            r#"{"year":2026,"holidays":[{"date":"2026-10-01","name":"国庆节"},{"date":"2026-10-02"}]}"#,
        )
        .unwrap();
        let check = is_holiday(&d, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
        assert_eq!(check, HolidayCheck::Holiday("国庆节".into()));
        assert_eq!(
            is_holiday(&d, NaiveDate::from_ymd_opt(2026, 9, 10).unwrap()),
            HolidayCheck::NotHoliday
        );
    }

    #[test]
    fn missing_file_is_not_holiday_not_error() {
        let d = dir();
        let check = is_holiday(&d, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
        assert_eq!(check, HolidayCheck::NotHoliday);
    }

    #[test]
    fn corrupt_file_is_unavailable() {
        let d = dir();
        std::fs::write(d.join("holidays-2026.json"), "{not json").unwrap();
        let check = is_holiday(&d, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
        assert!(matches!(check, HolidayCheck::Unavailable(_)));
    }

    #[test]
    fn bad_date_format_is_unavailable() {
        let d = dir();
        std::fs::write(
            d.join("holidays-2026.json"),
            r#"{"year":2026,"holidays":[{"date":"2026/10/01"}]}"#,
        )
        .unwrap();
        let check = is_holiday(&d, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
        assert!(matches!(check, HolidayCheck::Unavailable(_)));
    }

    #[test]
    fn year_mismatch_is_unavailable() {
        let d = dir();
        std::fs::write(
            d.join("holidays-2026.json"),
            r#"{"year":2025,"holidays":[{"date":"2025-10-01"}]}"#,
        )
        .unwrap();
        let check = is_holiday(&d, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
        assert!(matches!(check, HolidayCheck::Unavailable(_)));
    }
}
