//! 获取仓库中提交者时间落在 `date` 且处于 [work_start, work_end) 窗口内的提交,
//! 附完整 patch 文本及增删行数。

use std::path::Path;
use std::process::Command;

use chrono::{Local, NaiveDate, NaiveTime};

/// 一条提交。
#[derive(Debug, Clone)]
pub struct Commit {
    pub sha: String,
    pub committer_date: String, // RFC3339,本地时区
    pub subject: String,
    pub patch: String,
    pub added: u64,
    pub removed: u64,
}

/// 目标窗口内某仓库的全部提交。
#[derive(Debug, Clone)]
pub struct RepoCommits {
    pub repo: String,
    pub commits: Vec<Commit>,
    pub error: Option<String>,
}

impl RepoCommits {
    pub fn has_commits(&self) -> bool {
        !self.commits.is_empty()
    }
    pub fn total_added(&self) -> u64 {
        self.commits.iter().map(|c| c.added).sum()
    }
    pub fn total_removed(&self) -> u64 {
        self.commits.iter().map(|c| c.removed).sum()
    }
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args[0],
            String::from_utf8_lossy(&out.stderr).trim().to_string()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 收集提交时用于过滤的提交者邮箱:若设置了仓库级 `user.email` 则用它,否则用全局 `user.email`。
///
/// 全局查询使用 `--global`(不在仓库内执行),以避免取到仓库级值,并且在 `cwd`
/// 不在工作树内时也能正常工作(配置校验会检查可能还不是 git 仓库的目录)。
pub fn git_identity(cwd: &Path) -> Option<String> {
    for args in [
        &["config", "user.email"] as &[&str],
        &["config", "--global", "user.email"] as &[&str],
    ] {
        if let Ok(v) = run_git(cwd, args) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// 窗口判定:起点含、终点不含。正确处理跨午夜窗口(如 19:00–08:00)。
pub fn in_window(t: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    if start <= end {
        t >= start && t < end
    } else {
        // 跨午夜:[start, 24:00) ∪ [00:00, end)
        t >= start || t < end
    }
}

/// 从 unified diff 中统计内容行的 +/- 行,跳过 +++/--- 文件头。
pub fn count_diff(patch: &str) -> (u64, u64) {
    let mut added = 0u64;
    let mut removed = 0u64;
    for line in patch.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

/// 解析 `git log --format=%H%x09%cI%x09%ce%x09%s` 输出的
/// `sha<TAB>日期<TAB>提交者邮箱<TAB>标题` 行。
fn parse_log(lines: &[&str]) -> Vec<(String, String, String, String)> {
    lines
        .iter()
        .filter_map(|l| {
            let mut it = l.splitn(4, '\t');
            match (it.next(), it.next(), it.next(), it.next()) {
                (Some(s), Some(d), Some(ce), Some(subj)) => Some((
                    s.to_string(),
                    d.to_string(),
                    ce.to_string(),
                    subj.to_string(),
                )),
                _ => None,
            }
        })
        .collect()
}

/// 收集 `repo_path` 仓库中由 `email` 提交、日期为 `date` 且位于 `[start, end)` 内的提交。
///
/// 使用 `git log --all`(所有引用:全部本地分支、远程跟踪分支、标签)而非 `HEAD`,
/// 以便尚未检出的分支上的提交也能被找到。随后按提交者邮箱过滤,排除共享/远程
/// 分支上他人所作的提交。
pub fn collect_for(
    repo_path: &Path,
    email: &str,
    date: NaiveDate,
    start: NaiveTime,
    end: NaiveTime,
) -> RepoCommits {
    collect_for_range(repo_path, email, date, start, Some(end))
}

/// 收集本用户在 `date` 当天从 `start` 至日终的提交(加班段;`NaiveTime` 无法表示
/// 24:00,故设独立入口)。
pub fn collect_for_overtime(
    repo_path: &Path,
    email: &str,
    date: NaiveDate,
    start: NaiveTime,
) -> RepoCommits {
    collect_for_range(repo_path, email, date, start, None)
}

/// 核心收集逻辑。`end = None` 时保留 `date` 当天所有 `time >= start` 的提交
/// (即到日终为止)。
fn collect_for_range(
    repo_path: &Path,
    email: &str,
    date: NaiveDate,
    start: NaiveTime,
    end: Option<NaiveTime>,
) -> RepoCommits {
    let label = repo_path.display().to_string();

    // 确认是 git 工作树。
    if run_git(repo_path, &["rev-parse", "--is-inside-work-tree"]).is_err() {
        return RepoCommits {
            repo: label,
            commits: vec![],
            error: Some("not a git repo".into()),
        };
    }

    // 抓取超集:自(date - 1 天)起、所有引用上的全部提交。采用略宽的 --since
    // (不加 --until)可规避一个 git 坑:当图序中最新提交的日期比其祖先更旧时,
    // 基于日期的 --until 会剪掉窗口内提交。精确的日期 + 窗口 + 作者筛选在下面
    // 代码中完成,因此取超集是安全的。
    let offset = Local::now().offset().to_string(); // 例如 "+08:00"
    let since_date = (date - chrono::Duration::days(1)).format("%Y-%m-%d");
    let since = format!("{since_date}T00:00:00{offset}");
    let format = "%H%x09%cI%x09%ce%x09%s";
    let raw = match run_git(
        repo_path,
        &[
            "log",
            "--all",
            "--no-color",
            &format!("--format={format}"),
            &format!("--since={since}"),
        ],
    ) {
        Ok(r) => r,
        Err(e) => {
            return RepoCommits {
                repo: label,
                commits: vec![],
                error: Some(e),
            }
        }
    };

    let mut commits = Vec::new();
    for (sha, cdate, committer_email, subject) in parse_log(&raw.lines().collect::<Vec<_>>()) {
        // 只保留本用户的提交…
        if committer_email != email {
            continue;
        }
        // …且属于目标本地日历日…
        let dt = match chrono::DateTime::parse_from_rfc3339(&cdate) {
            Ok(dt) => dt,
            Err(_) => continue,
        };
        let local = dt.with_timezone(&Local);
        // …且在窗口内(end 为 None 时,即从 start 到日终)。
        let in_range = match end {
            Some(e) => in_window(local.time(), start, e),
            None => local.time() >= start,
        };
        if local.date_naive() != date || !in_range {
            continue;
        }

        // 获取该提交的完整 patch。
        let patch = run_git(
            repo_path,
            &["show", "--no-color", "--format=", "--patch", &sha],
        )
        .unwrap_or_default();
        let (added, removed) = count_diff(&patch);
        commits.push(Commit {
            sha,
            committer_date: cdate,
            subject,
            patch,
            added,
            removed,
        });
    }

    RepoCommits {
        repo: label,
        commits,
        error: None,
    }
}

/// 收集本机所识别身份的人(先仓库级,后全局 `git config user.email`)在 `date`
/// 当天 `[start, end)` 内的提交。
pub fn collect(repo_path: &Path, date: NaiveDate, start: NaiveTime, end: NaiveTime) -> RepoCommits {
    match git_identity(repo_path) {
        Some(email) => collect_for(repo_path, &email, date, start, end),
        None => RepoCommits {
            repo: repo_path.display().to_string(),
            commits: vec![],
            error: Some(
                "无法确定 git 提交者邮箱：请设置仓库级或全局 git config user.email，或在 config.toml 中配置 git_email"
                    .into(),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_simple() {
        let s = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let e = NaiveTime::from_hms_opt(18, 0, 0).unwrap();
        assert!(in_window(NaiveTime::from_hms_opt(9, 0, 0).unwrap(), s, e));
        assert!(in_window(
            NaiveTime::from_hms_opt(17, 59, 59).unwrap(),
            s,
            e
        ));
        assert!(!in_window(NaiveTime::from_hms_opt(18, 0, 0).unwrap(), s, e));
        assert!(!in_window(
            NaiveTime::from_hms_opt(8, 59, 59).unwrap(),
            s,
            e
        ));
    }

    #[test]
    fn window_wraps() {
        let s = NaiveTime::from_hms_opt(19, 0, 0).unwrap();
        let e = NaiveTime::from_hms_opt(8, 0, 0).unwrap();
        assert!(in_window(NaiveTime::from_hms_opt(20, 0, 0).unwrap(), s, e));
        assert!(in_window(NaiveTime::from_hms_opt(0, 0, 0).unwrap(), s, e));
        assert!(in_window(NaiveTime::from_hms_opt(7, 59, 0).unwrap(), s, e));
        assert!(!in_window(NaiveTime::from_hms_opt(9, 0, 0).unwrap(), s, e));
        assert!(!in_window(NaiveTime::from_hms_opt(18, 0, 0).unwrap(), s, e));
    }

    #[test]
    fn count_diff_counts_lines() {
        let patch = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1,2 @@\n-old\n+new1\n+new2\n";
        let (a, r) = count_diff(patch);
        assert_eq!(a, 2);
        assert_eq!(r, 1);
    }

    #[test]
    fn parse_log_splits_fields() {
        let lines: &[&str] = &[
            "abc123\t2026-09-10T10:00:00+08:00\tme@corp.com\tfix login",
            "def456\t2026-09-10T11:00:00+08:00\tother@corp.com\tadd feature",
        ];
        let parsed = parse_log(lines);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "abc123");
        assert_eq!(parsed[0].2, "me@corp.com");
        assert_eq!(parsed[0].3, "fix login");
        assert_eq!(parsed[1].2, "other@corp.com");
    }
}
