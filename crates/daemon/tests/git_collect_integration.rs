//! 针对带受控日期/时间提交的真实临时 git 仓库的 git 收集器端到端测试,验证
//! 09:00–18:00 窗口过滤与日期选择在真实的 `git` 下是否有效。

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
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args[0],
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 按*本地*时区偏移构造提交日期字符串,使测试在任何机器的 TZ 下都确定
/// (收集器会将提交时间转换为 Local)。
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
    assert!(
        out.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 显式指定提交者身份+日期的提交(用于模拟他人或另一分支上的提交)。
fn commit_as(
    cwd: &Path,
    date: &str,
    msg: &str,
    file: &str,
    content: &str,
    name: &str,
    email: &str,
) {
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
    assert!(
        out.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

    // A:目标日、窗口内(10:00) → 纳入
    commit(
        &dir,
        &local_date("2026-09-09", "10:00"),
        "commit A in window",
        "a.txt",
        "a",
    );
    // B:目标日、窗口外(19:00) → 排除
    commit(
        &dir,
        &local_date("2026-09-09", "19:00"),
        "commit B out of window",
        "b.txt",
        "b",
    );
    // C:前一日、窗口内 → 排除(不同日期)
    commit(
        &dir,
        &local_date("2026-09-08", "10:00"),
        "commit C previous day",
        "c.txt",
        "c",
    );

    let (date, start, end) = window();
    let rc = git_collector::collect(&dir, date, start, end);
    assert!(rc.error.is_none(), "unexpected error: {:?}", rc.error);
    let subjects: Vec<&str> = rc.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(
        subjects,
        vec!["commit A in window"],
        "expected only commit A, got {subjects:?}"
    );

    // 保留下来的提交应有非空 patch 且行数被计数。
    let a = &rc.commits[0];
    assert!(!a.patch.is_empty());
    assert!(a.added >= 1, "added should be >=1, got {}", a.added);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn collector_overtime_collects_until_end_of_day() {
    let dir = tmp_repo("overtime");
    let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
    let end_of_day = NaiveTime::from_hms_opt(18, 0, 0).unwrap();

    // P:前一日晚间 → 排除(不同日期)
    commit(
        &dir,
        &local_date("2026-09-08", "20:00"),
        "P previous day evening",
        "p.txt",
        "p",
    );
    // M:紧邻 work_end 之前(17:59) → 不计入加班
    commit(
        &dir,
        &local_date("2026-09-09", "17:59"),
        "M before 18:00",
        "m.txt",
        "m",
    );
    // X:恰好在 work_end(18:00:00) → 纳入
    commit(
        &dir,
        &local_date("2026-09-09", "18:00"),
        "X at 18:00",
        "x.txt",
        "x",
    );
    // B:19:00 → 纳入
    commit(
        &dir,
        &local_date("2026-09-09", "19:00"),
        "B at 19:00",
        "b.txt",
        "b",
    );
    // N:次日清晨(2026-09-10 00:30) → 排除(不同日期)
    commit(
        &dir,
        &local_date("2026-09-10", "00:30"),
        "N next morning",
        "n.txt",
        "n",
    );
    // L:深夜 23:59 → 纳入
    commit(
        &dir,
        &local_date("2026-09-09", "23:59"),
        "L at 23:59",
        "l.txt",
        "l",
    );

    let rc = git_collector::collect_for_overtime(&dir, "me@corp.com", date, end_of_day);
    assert!(rc.error.is_none(), "unexpected error: {:?}", rc.error);
    let mut subjects: Vec<&str> = rc.commits.iter().map(|c| c.subject.as_str()).collect();
    subjects.sort_unstable();
    assert_eq!(
        subjects,
        vec!["B at 19:00", "L at 23:59", "X at 18:00"],
        "expected only the target-day >=18:00 commits, got {subjects:?}"
    );

    // 保留的提交携带 patch 且行数被计数。
    for c in &rc.commits {
        assert!(!c.patch.is_empty(), "empty patch for {}", c.subject);
        assert!(c.added >= 1, "added should be >=1, got {}", c.added);
    }

    // 常规窗口段只看到 17:59 的提交;>=18:00 的不在其中(两个窗口互斥)。
    let (d, start, end) = window();
    let rc_normal = git_collector::collect_for(&dir, "me@corp.com", d, start, end);
    let normal_subjects: Vec<&str> = rc_normal
        .commits
        .iter()
        .map(|c| c.subject.as_str())
        .collect();
    assert_eq!(
        normal_subjects,
        vec!["M before 18:00"],
        "normal pass should contain only the 17:59 commit, got {normal_subjects:?}"
    );

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

    // 默认分支上的基线提交,位于目标日之外。
    commit(
        &dir,
        &local_date("2026-09-07", "10:00"),
        "baseline",
        "base.txt",
        "base",
    );

    // 工作在 feature 分支上进行,位于窗口内。
    git(&dir, &["checkout", "-qb", "feature"]);
    commit(
        &dir,
        &local_date("2026-09-09", "10:00"),
        "feature work in window",
        "feat.txt",
        "feat",
    );
    // …同一天同事在同一分支上有提交(必须被排除)。
    commit_as(
        &dir,
        &local_date("2026-09-09", "11:00"),
        "colleague work on same day",
        "other.txt",
        "other",
        "colleague",
        "colleague@corp.com",
    );

    // 切回默认分支:HEAD 不再能看到 feature 提交。
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
    // git config 中声明了一个身份的仓库;用不同的显式邮箱调用 collect_for
    // 应只拾取该邮箱的提交。
    let dir = tmp_repo("explicit");
    commit(
        &dir,
        &local_date("2026-09-07", "10:00"),
        "baseline",
        "base.txt",
        "base",
    );
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

    // 而 git-config 身份(me@corp.com)当天找不到任何内容。
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
    git(
        &dir,
        &["config", "--global", "user.email", "global@corp.com"],
    );
    // 仓库级值必须优先于全局值。
    assert_eq!(
        git_collector::git_identity(&dir).as_deref(),
        Some("local@corp.com")
    );

    // 在没有仓库级值的目录中,使用全局值。
    let other = std::env::temp_dir().join(format!("gitcol-identity-other-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&other);
    std::fs::create_dir_all(&other).unwrap();
    assert_eq!(
        git_collector::git_identity(&other).as_deref(),
        Some("global@corp.com")
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other);
    let _ = std::fs::remove_dir_all(&local);
    std::env::remove_var("GIT_CONFIG_GLOBAL");
    std::env::remove_var("GIT_CONFIG_SYSTEM");
}
