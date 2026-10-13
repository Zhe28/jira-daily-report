//! 加载并校验外部节假日 JSON,并回答"该日期是否为法定节假日?"。

use std::collections::HashSet;
use std::path::Path;

use chrono::NaiveDate;
use serde::Deserialize;

/// 节假日查询的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolidayCheck {
    /// 该日期是已知节假日(附名称,如有)。
    Holiday(String),
    /// 节假日已加载,但该日期不在其中。
    NotHoliday,
    /// 无法加载节假日数据(缺失/损坏)—调用方应降级为"仅在无提交时跳过"并向用户发出警告。
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

/// 按日期索引的已加载节假日数据。
pub struct HolidaySet {
    dates: HashSet<NaiveDate>,
    names: std::collections::HashMap<NaiveDate, String>,
    sources: Vec<String>,
}

impl HolidaySet {
    pub fn empty() -> Self {
        Self {
            dates: HashSet::new(),
            names: std::collections::HashMap::new(),
            sources: vec![],
        }
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

/// 校验并导入某一年份文件的 JSON。
fn ingest(
    text: &str,
    source: &str,
    expected_year: Option<i32>,
) -> Result<Vec<(NaiveDate, String)>, String> {
    let yf: YearFile =
        serde_json::from_str(text).map_err(|e| format!("{}: JSON parse: {}", source, e))?;
    if let Some(exp) = expected_year {
        if yf.year != exp {
            return Err(format!(
                "{}: year field {} != file year {}",
                source, yf.year, exp
            ));
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

/// 从目录(`holidays-<年份>.json` 文件合并)或单个文件路径加载节假日数据。
/// 缺失 → 空集(NotHoliday);损坏 → 错误。
pub fn load_for_date(path: &Path) -> Result<HolidaySet, String> {
    let mut set = HolidaySet::empty();

    let mut sources = Vec::new();
    if path.is_dir() {
        // 合并目录中所有 holidays-*.json;按日期逐个判断是否命中。
        for entry in
            std::fs::read_dir(path).map_err(|e| format!("reading dir {}: {}", path.display(), e))?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(y) = extract_year(&name) else {
                continue;
            };
            let entries = ingest(
                &std::fs::read_to_string(entry.path()).map_err(|e| e.to_string())?,
                &name,
                Some(y),
            )?;
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

    // 不是目录:将 path 视为单个节假日文件处理。
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

    // 既非目录也非文件——没有可用的节假日数据(优雅降级)。
    Ok(set)
}

fn extract_year(name: &str) -> Option<i32> {
    let stem = name.strip_suffix(".json")?;
    let y = stem.strip_prefix("holidays-")?;
    y.parse().ok()
}

/// 高层接口:回答某日期是否节假日,将加载失败映射为 Unavailable。
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
