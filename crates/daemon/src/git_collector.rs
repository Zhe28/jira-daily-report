//! Fetch a repo's commits whose committer time falls on `date` within the
//! [work_start, work_end) window, with full patch text and added/removed lines.

use std::path::Path;
use std::process::Command;

use chrono::{Local, NaiveDate, NaiveTime};

/// One commit.
#[derive(Debug, Clone)]
pub struct Commit {
    pub sha: String,
    pub committer_date: String, // RFC3339, local
    pub subject: String,
    pub patch: String,
    pub added: u64,
    pub removed: u64,
}

/// All commits for one repo in the target window.
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
    let out = Command::new("git").args(args).current_dir(cwd).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args[0],
            String::from_utf8_lossy(&out.stderr).trim().to_string()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The committer email to filter by when collecting commits: the repo-local
/// `user.email` if set, otherwise the global `user.email`.
///
/// The global lookup is run with `--global` (not from inside the repo) so it
/// cannot pick up the repo-local value and so it works even when `cwd` is not
/// inside a work tree (config validation checks repos that may not be git
/// yet).
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

/// Inclusive start / exclusive end window test. Handles a window that wraps
/// past midnight (e.g. 19:00–08:00) correctly.
pub fn in_window(t: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    if start <= end {
        t >= start && t < end
    } else {
        // wraps: [start, 24:00) U [00:00, end)
        t >= start || t < end
    }
}

/// Count content +/- lines from a unified diff, skipping +++/--- file headers.
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

/// Parse `sha<TAB>date<TAB>committer-email<TAB>subject` lines from
/// `git log --format=%H%x09%cI%x09%ce%x09%s`.
fn parse_log(lines: &[&str]) -> Vec<(String, String, String, String)> {
    lines
        .iter()
        .filter_map(|l| {
            let mut it = l.splitn(4, '\t');
            match (it.next(), it.next(), it.next(), it.next()) {
                (Some(s), Some(d), Some(ce), Some(subj)) => {
                    Some((s.to_string(), d.to_string(), ce.to_string(), subj.to_string()))
                }
                _ => None,
            }
        })
        .collect()
}

/// Collect commits authored/committed by `email` in `repo_path` on `date`
/// within `[start, end)`.
///
/// Uses `git log --all` (every ref: all local branches, remote-tracking
/// branches, tags) instead of `HEAD`, so commits made on a branch that is not
/// currently checked out are still found. The committer email then excludes
/// commits made by other people on shared/remote branches.
pub fn collect_for(repo_path: &Path, email: &str, date: NaiveDate, start: NaiveTime, end: NaiveTime) -> RepoCommits {
    let label = repo_path.display().to_string();

    // Confirm it's a git work tree.
    if run_git(repo_path, &["rev-parse", "--is-inside-work-tree"]).is_err() {
        return RepoCommits { repo: label, commits: vec![], error: Some("not a git repo".into()) };
    }

    // Fetch a superset: everything committed since (date - 1 day) across all
    // refs. Using a slightly wide --since (no --until) avoids a git gotcha
    // where a date-based --until prunes in-range commits when the newest
    // commit in graph order has an older date than its ancestors. The exact
    // day + window + author selection is done in code below, so the superset
    // is safe.
    let offset = Local::now().offset().to_string(); // e.g. "+08:00"
    let since_date = (date - chrono::Duration::days(1)).format("%Y-%m-%d");
    let since = format!("{since_date}T00:00:00{offset}");
    let format = "%H%x09%cI%x09%ce%x09%s";
    let raw = match run_git(
        repo_path,
        &["log", "--all", "--no-color", &format!("--format={format}"), &format!("--since={since}")],
    ) {
        Ok(r) => r,
        Err(e) => return RepoCommits { repo: label, commits: vec![], error: Some(e) },
    };

    let mut commits = Vec::new();
    for (sha, cdate, committer_email, subject) in parse_log(&raw.lines().collect::<Vec<_>>()) {
        // Keep only this user's commits...
        if committer_email != email {
            continue;
        }
        // ...on the target local calendar day...
        let dt = match chrono::DateTime::parse_from_rfc3339(&cdate) {
            Ok(dt) => dt,
            Err(_) => continue,
        };
        let local = dt.with_timezone(&Local);
        // ...inside the work window.
        if local.date_naive() != date || !in_window(local.time(), start, end) {
            continue;
        }

        // Fetch the full patch for this commit.
        let patch = run_git(repo_path, &["show", "--no-color", "--format=", "--patch", &sha]).unwrap_or_default();
        let (added, removed) = count_diff(&patch);
        commits.push(Commit { sha, committer_date: cdate, subject, patch, added, removed });
    }

    RepoCommits { repo: label, commits, error: None }
}

/// Collect commits by whoever this machine identifies as (repo-local, then
/// global `git config user.email`) for `date` within `[start, end)`.
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
        assert!(in_window(NaiveTime::from_hms_opt(17, 59, 59).unwrap(), s, e));
        assert!(!in_window(NaiveTime::from_hms_opt(18, 0, 0).unwrap(), s, e));
        assert!(!in_window(NaiveTime::from_hms_opt(8, 59, 59).unwrap(), s, e));
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
