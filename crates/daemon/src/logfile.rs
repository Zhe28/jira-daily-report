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
    /// True for the overtime part of the day (rendered as「加班」).
    pub overtime: bool,
}

/// Build the merged log body.
pub fn render(date: NaiveDate, total_seconds: u64, sections: &[LogSection]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# 日报 {}\n", date.format("%Y-%m-%d")));
    out.push_str(&format!("# 总时长 {} 小时（{} 秒）\n", hours_label(total_seconds), total_seconds));
    out.push_str(&format!("# 生成时间 {}\n\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S")));
    for s in sections {
        let header = if s.overtime {
            format!("## {}（{}）加班 — {}小时", s.issue_key, s.repo, hours_label(s.seconds))
        } else {
            format!("## {}（{}）— {}小时", s.issue_key, s.repo, hours_label(s.seconds))
        };
        out.push_str(&header);
        out.push('\n');
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
            overtime: false,
        })
        .collect()
}

/// 删除 `dir` 下 `daemon-YYYY-MM-DD.log` 形式、日期早于 (今天 - keep_days) 的旧日志；
/// 前缀不符或日期解析失败的文件一律保留。
pub fn prune_old_logs(dir: &Path, keep_days: i64) {
    let today = chrono::Local::now().date_naive();
    let cutoff = today - chrono::Duration::days(keep_days);
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_prefix("daemon-").and_then(|s| s.strip_suffix(".log")) else { continue };
        let Ok(d) = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d") else { continue };
        if d < cutoff {
            let _ = std::fs::remove_file(e.path());
        }
    }
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
            overtime: false,
        }];
        let body = render(NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(), 28800, &secs);
        assert!(body.contains("# 日报 2026-09-09"));
        assert!(body.contains("## A-1"));
        assert!(!body.contains("加班"));
        assert!(body.contains("增加功能A"));
    }

    #[test]
    fn render_marks_overtime_section() {
        let secs = vec![
            LogSection { issue_key: "A-1".into(), repo: "r".into(), seconds: 28800, report: "白天".into(), overtime: false },
            LogSection { issue_key: "A-1".into(), repo: "r".into(), seconds: 7200, report: "晚上".into(), overtime: true },
        ];
        let body = render(NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(), 36000, &secs);
        assert!(body.contains("## A-1（r）— 8小时"));
        assert!(body.contains("## A-1（r）加班 — 2小时"));
    }

    #[test]
    fn filename_with_overtime_total() {
        let p = log_path(Path::new("/tmp/x"), NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(), 36000);
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "2026-09-09-10小时.log");
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
            overtime: false,
        }];
        let p = write_log(&dir, NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(), 28800, &secs).unwrap();
        assert!(p.exists());
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains("内容"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_old_logs_removes_only_old_daemon_files() {
        let d = std::env::temp_dir().join(format!("prune-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let today = chrono::Local::now().date_naive();
        let old = (today - chrono::Duration::days(30)).format("%Y-%m-%d").to_string();
        let recent = (today - chrono::Duration::days(2)).format("%Y-%m-%d").to_string();
        std::fs::write(d.join(format!("daemon-{old}.log")), "x").unwrap();
        std::fs::write(d.join(format!("daemon-{recent}.log")), "x").unwrap();
        std::fs::write(d.join("daemon-bogus.log"), "x").unwrap();
        std::fs::write(d.join("2026-09-09-8小时.log"), "x").unwrap();
        prune_old_logs(&d, 14);
        assert!(!d.join(format!("daemon-{old}.log")).exists());
        assert!(d.join(format!("daemon-{recent}.log")).exists());
        assert!(d.join("daemon-bogus.log").exists());
        assert!(d.join("2026-09-09-8小时.log").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
