//! End-to-end test of the git collector against a real temporary git repo with
//! commits placed at controlled dates/times, to verify the 09:00–18:00 window
//! filter and day selection work against actual `git`.

use std::path::Path;
use std::process::Command;

use chrono::NaiveTime;

use daily_report::git_collector;

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {} failed: {}", args[0], String::from_utf8_lossy(&out.stderr));
}

/// Build a commit date string at the *local* timezone offset, so the test is
/// deterministic regardless of the machine's TZ (the collector converts commit
/// times to Local).
fn local_date(day: &str, hhmm: &str) -> String {
    let offset = chrono::Local::now().offset().to_string();
    format!("{day}T{hhmm}:00{offset}")
}

fn commit(cwd: &Path, date: &str, msg: &str, file: &str, content: &str) {
    std::fs::write(cwd.join(file), content).unwrap();
    git(cwd, &["add", file]);
    let out = Command::new("git")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .args(["commit", "-m", msg, "--no-verify"])
        .current_dir(cwd)
        .output()
        .expect("run git commit");
    assert!(out.status.success(), "git commit failed: {}", String::from_utf8_lossy(&out.stderr));
}

/// Commit with an explicit committer identity + date (used to simulate
/// commits made by another person or on another branch).
fn commit_as(cwd: &Path, date: &str, msg: &str, file: &str, content: &str, name: &str, email: &str) {
    std::fs::write(cwd.join(file), content).unwrap();
    git(cwd, &["add", file]);
    let out = Command::new("git")
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_COMMITTER_NAME", name)
        .env("GIT_COMMITTER_EMAIL", email)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .args(["commit", "-m", msg, "--no-verify"])
        .current_dir(cwd)
        .output()
        .expect("run git commit");
    assert!(out.status.success(), "git commit failed: {}", String::from_utf8_lossy(&out.stderr));
}

fn tmp_repo(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("gitcol-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.email", "me@corp.com"]);
    git(&dir, &["config", "user.name", "me"]);
    dir
}

fn window() -> (chrono::NaiveDate, NaiveTime, NaiveTime) {
    (
        chrono::NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(),
        NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
    )
}

#[test]
fn collector_filters_by_day_and_window() {
    let dir = tmp_repo("day-window");

    // A: target day, in window (10:00)  -> included
    commit(&dir, &local_date("2026-09-09", "10:00"), "commit A in window", "a.txt", "a");
    // B: target day, out of window (19:00) -> excluded
    commit(&dir, &local_date("2026-09-09", "19:00"), "commit B out of window", "b.txt", "b");
    // C: previous day, in window -> excluded (different day)
    commit(&dir, &local_date("2026-09-08", "10:00"), "commit C previous day", "c.txt", "c");

    let (date, start, end) = window();
    let rc = git_collector::collect(&dir, date, start, end);
    assert!(rc.error.is_none(), "unexpected error: {:?}", rc.error);
    let subjects: Vec<&str> = rc.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, vec!["commit A in window"], "expected only commit A, got {subjects:?}");

    // The kept commit should have a non-empty patch and counted lines.
    let a = &rc.commits[0];
    assert!(!a.patch.is_empty());
    assert!(a.added >= 1, "added should be >=1, got {}", a.added);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_git_dir_reports_error() {
    let dir = std::env::temp_dir().join(format!("gitcol-notgit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (date, start, end) = window();
    let rc = git_collector::collect(&dir, date, start, end);
    assert!(rc.error.is_some(), "expected an error for a non-git dir");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn finds_commits_on_branches_not_checked_out() {
    let dir = tmp_repo("nonhead");

    // Baseline commit on the default branch, outside the target day.
    commit(&dir, &local_date("2026-09-07", "10:00"), "baseline", "base.txt", "base");

    // Work happens on a feature branch, inside the window.
    git(&dir, &["checkout", "-qb", "feature"]);
    commit(&dir, &local_date("2026-09-09", "10:00"), "feature work in window", "feat.txt", "feat");
    // ...and a colleague committed on the same branch that day (must be excluded).
    commit_as(
        &dir,
        &local_date("2026-09-09", "11:00"),
        "colleague work on same day",
        "other.txt",
        "other",
        "colleague",
        "colleague@corp.com",
    );

    // Back to the default branch: HEAD no longer sees the feature commits.
    git(&dir, &["checkout", "-q", "-"]);

    let (date, start, end) = window();
    let rc = git_collector::collect_for(&dir, "me@corp.com", date, start, end);
    assert!(rc.error.is_none(), "unexpected error: {:?}", rc.error);
    let subjects: Vec<&str> = rc.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(
        subjects,
        vec!["feature work in window"],
        "expected only my feature commit (not the colleague's), got {subjects:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn explicit_email_overrides_git_config() {
    // A repo whose git config says one identity; collect_for with a different
    // explicit email must pick up only commits made by that email.
    let dir = tmp_repo("explicit");
    commit(&dir, &local_date("2026-09-07", "10:00"), "baseline", "base.txt", "base");
    commit_as(
        &dir,
        &local_date("2026-09-09", "10:00"),
        "work under explicit email",
        "w.txt",
        "w",
        "alt",
        "alt@corp.com",
    );

    let (date, start, end) = window();
    let rc = git_collector::collect_for(&dir, "alt@corp.com", date, start, end);
    let subjects: Vec<&str> = rc.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, vec!["work under explicit email"]);

    // And the git-config identity (me@corp.com) finds nothing that day.
    let rc2 = git_collector::collect_for(&dir, "me@corp.com", date, start, end);
    assert!(rc2.commits.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn git_identity_prefers_repo_local_then_global() {
    let dir = tmp_repo("identity");
    git(&dir, &["config", "user.email", "local@corp.com"]);

    let local = std::env::temp_dir().join(format!("gitcol-identity-global-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local);
    std::fs::create_dir_all(&local).unwrap();
    std::env::set_var("GIT_CONFIG_GLOBAL", local.join("config"));
    std::env::set_var("GIT_CONFIG_SYSTEM", local.join("no-such-system"));
    git(&dir, &["config", "--global", "user.email", "global@corp.com"]);
    // The repo-local value must win over the global one.
    assert_eq!(git_collector::git_identity(&dir).as_deref(), Some("local@corp.com"));

    // In a dir without a repo-local value, the global one is used.
    let other = std::env::temp_dir().join(format!("gitcol-identity-other-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&other);
    std::fs::create_dir_all(&other).unwrap();
    assert_eq!(git_collector::git_identity(&other).as_deref(), Some("global@corp.com"));

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other);
    let _ = std::fs::remove_dir_all(&local);
    std::env::remove_var("GIT_CONFIG_GLOBAL");
    std::env::remove_var("GIT_CONFIG_SYSTEM");
}
