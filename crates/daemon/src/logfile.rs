//! Write the merged daily `.log` file: `[date]-[total_hours]小时.log` in `log_dir`,
//! with one section per repo (issue).

use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use crate::worklog_plan::{hours_label, Allocated};

/// One repo's contribution to the daily log.
#[derive(Debug, Clone)]
pub struct LogSection {
    pub issue_key: String,
    pub repo: String,
    pub seconds: u64,
    pub report: String,
}

/// Build the merged log body.
pub fn render(date: NaiveDate, total_seconds: u64, sections: &[LogSection]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# 日报 {}\n", date.format("%Y-%m-%d")));
    out.push_str(&format!("# 总时长 {} 小时（{} 秒）\n", hours_label(total_seconds), total_seconds));
    out.push_str(&format!("# 生成时间 {}\n\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S")));
    for s in sections {
        out.push_str(&format!("## {}（{}）— {}小时\n", s.issue_key, s.repo, hours_label(s.seconds)));
        out.push_str(&s.report.trim());
        out.push_str("\n\n");
    }
    out
}

/// Build the output file path for the given date.
pub fn log_path(log_dir: &Path, date: NaiveDate, total_seconds: u64) -> PathBuf {
    log_dir.join(format!("{}-{}小时.log", date.format("%Y-%m-%d"), hours_label(total_seconds)))
}

/// Write the merged log. Creates `log_dir` if needed.
pub fn write_log(log_dir: &Path, date: NaiveDate, total_seconds: u64, sections: &[LogSection]) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(log_dir)?;
    let path = log_path(log_dir, date, total_seconds);
    let body = render(date, total_seconds, sections);
    std::fs::write(&path, body)?;
    Ok(path)
}

/// Merge allocations with reports into sections (order-preserving).
pub fn build_sections(allocations: &[Allocated], reports: &std::collections::HashMap<String, String>) -> Vec<LogSection> {
    allocations
        .iter()
        .map(|a| LogSection {
            issue_key: a.issue_key.clone(),
            repo: a.repo.clone(),
            seconds: a.seconds,
            report: reports.get(&a.issue_key).cloned().unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_format() {
        let p = log_path(Path::new("/tmp/x"), NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(), 28800);
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "2026-09-09-8小时.log");
    }

    #[test]
    fn filename_fractional_hours() {
        let p = log_path(Path::new("/tmp/x"), NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(), 9000);
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "2026-09-09-2.5小时.log");
    }

    #[test]
    fn render_has_sections() {
        let secs = vec![LogSection {
            issue_key: "A-1".into(),
            repo: "D:\\r".into(),
            seconds: 28800,
            report: "增加功能A\n修复bugB".into(),
        }];
        let body = render(NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(), 28800, &secs);
        assert!(body.contains("# 日报 2026-09-09"));
        assert!(body.contains("## A-1"));
        assert!(body.contains("增加功能A"));
    }

    #[test]
    fn write_and_read_back() {
        let dir = std::env::temp_dir().join(format!("log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let secs = vec![LogSection {
            issue_key: "A-1".into(),
            repo: "r".into(),
            seconds: 28800,
            report: "内容".into(),
        }];
        let p = write_log(&dir, NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(), 28800, &secs).unwrap();
        assert!(p.exists());
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains("内容"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
