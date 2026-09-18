//! The daily pipeline: holiday check -> git collect -> (skip?) -> AI report ->
//! hour allocation -> merged .log -> Tempo worklog write (per issue) -> state.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use crate::config::Config;
use crate::git_collector::{self, RepoCommits};
use crate::holidays::{self, HolidayCheck};
use crate::logfile::{self, LogSection};
use crate::notify;
use crate::reporter::{self, AIClient};
use crate::state::State;
use crate::worklog_plan::{self, RepoWeight};

/// Abstraction over Tempo so the pipeline is testable offline.
pub trait WorklogStore: Send + Sync {
    fn reachable(&self) -> anyhow::Result<()>;
    fn issue_id(&self, key: &str) -> anyhow::Result<u64>;
    fn has_worklog_for(&self, id: u64, date: NaiveDate) -> anyhow::Result<bool>;
    fn create_worklog(&self, id: u64, seconds: u64, comment: &str, started: &str, billable: bool) -> anyhow::Result<u64>;
}

/// One repo that had commits on the target day.
#[derive(Debug, Clone)]
pub struct RepoDay {
    pub repo: String,
    pub issue_key: String,
    pub commits: RepoCommits,
    pub prompt_file: Option<String>,
}

/// Result of the collect phase.
#[derive(Debug)]
pub struct CollectResult {
    /// Set when the whole day is skipped (holiday / no commits).
    pub skipped: Option<String>,
    pub repo_days: Vec<RepoDay>,
}

/// Result of the full day processing.
#[derive(Debug, Default)]
pub struct DayOutcome {
    pub skipped_reason: Option<String>,
    pub log_file: Option<std::path::PathBuf>,
    pub report_by_issue: BTreeMap<String, String>,
    pub created: Vec<(String, u64, u64)>, // (issue_key, seconds, worklog_id)
    pub planned: Vec<(String, u64)>, // dry-run: (issue_key, seconds)
    pub skipped_existing: Vec<String>, // issue_key already had a worklog
    pub failed: Vec<(String, String)>, // (issue_key, error)
}

/// Collect phase: holiday check + git collect. Returns a skip reason if the
/// whole day should be skipped.
pub fn collect_day(cfg: &Config, date: NaiveDate) -> anyhow::Result<CollectResult> {
    // 1) Holiday gate.
    match holidays::is_holiday(&cfg.holidays_dir, date) {
        HolidayCheck::Holiday(name) => {
            tracing::info!("{} 是法定节假日（{name}），整日跳过", date);
            return Ok(CollectResult { skipped: Some(format!("法定节假日: {name}")), repo_days: vec![] });
        }
        HolidayCheck::Unavailable(e) => {
            notify::notify("节假日文件异常", &format!("本次仅按无提交跳过。详情: {e}"));
        }
        HolidayCheck::NotHoliday => {}
    }

    // 2) Git collect per repo (09:00-18:00 window, filtered to this user's
    //    commits across all branches).
    let mut repo_days = Vec::new();
    for r in &cfg.repos {
        let email = cfg.resolve_git_email(r).ok_or_else(|| {
            anyhow::anyhow!(
                "repo {} 无可用 git 提交者身份（应在启动检查时已拦截）",
                r.local_path.display()
            )
        })?;
        let rc = git_collector::collect_for(&r.local_path, &email, date, cfg.work_start_time(), cfg.work_end_time());
        if let Some(e) = &rc.error {
            tracing::warn!("repo {}: {}", r.local_path.display(), e);
        }
        if rc.has_commits() {
            repo_days.push(RepoDay {
                repo: r.local_path.display().to_string(),
                issue_key: r.issue_key.clone(),
                commits: rc,
                prompt_file: r.prompt_file.clone(),
            });
        }
    }

    if repo_days.is_empty() {
        tracing::info!("{} 所有仓库在窗口内均无提交，整日跳过", date);
        return Ok(CollectResult { skipped: Some("无提交记录".into()), repo_days: vec![] });
    }
    Ok(CollectResult { skipped: None, repo_days })
}

/// Format a datetime as `YYYY-MM-DD HH:MM:SS` (no millisecond suffix; the Tempo
/// client appends `.000`).
fn started_str(dt: chrono::NaiveDateTime) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Resolve the worklog `started` string for `date`: the configured exact
/// `worklog_start` time (or `work_start` when omitted), at 0 seconds.
fn resolve_started(cfg: &Config, date: NaiveDate) -> String {
    let dt = date.and_time(cfg.worklog_start_time());
    started_str(dt)
}

/// Process phase: AI report + hours + .log + Tempo writes + state.
pub fn process_day(
    cfg: &Config,
    date: NaiveDate,
    repo_days: &[RepoDay],
    ai: &dyn AIClient,
    store: &dyn WorklogStore,
    dry_run: bool,
) -> anyhow::Result<DayOutcome> {
    let mut outcome = DayOutcome::default();
    if repo_days.is_empty() {
        return Ok(outcome);
    }

    // 1) AI report per repo.
    let mut reports: BTreeMap<String, Result<String, String>> = BTreeMap::new();
    let mut weights: Vec<RepoWeight> = Vec::new();
    for rd in repo_days {
        weights.push(RepoWeight {
            repo: rd.repo.clone(),
            issue_key: rd.issue_key.clone(),
            commit_count: rd.commits.commits.len() as u64,
            added: rd.commits.total_added(),
            removed: rd.commits.total_removed(),
        });
        // Read per-repo prompt file (if configured and the file exists).
        let repo_path = std::path::Path::new(&rd.repo);
        let custom_prompt = rd.prompt_file.as_deref()
            .and_then(|pf| reporter::read_repo_prompt(repo_path, pf));

        match reporter::report_repo_commits(ai, &rd.commits, custom_prompt.as_deref()) {
            Ok(text) => {
                reports.insert(rd.issue_key.clone(), Ok(text));
            }
            Err(e) => {
                let msg = e.to_string();
                tracing::error!("issue {} AI 生成失败: {}", rd.issue_key, msg);
                notify::notify("AI 日报生成失败", &format!("issue {} ({}): {}", rd.issue_key, rd.repo, msg));
                reports.insert(rd.issue_key.clone(), Err(msg));
            }
        }
    }

    // 2) Hour allocation (single repo -> full 8h; multiple -> weighted split).
    let allocations = worklog_plan::allocate(cfg.total_daily_seconds, &weights);

    // 3) Readiness check (<=5s) — notify on failure, continue (best effort).
    if let Err(e) = store.reachable() {
        notify::notify("Jira 检查超时/失败", &format!("{e} — 请手动处理，工具仍会尝试补跑"));
    }

    // 4) Merged .log (local artifact; written even in dry-run for review).
    let sections: Vec<LogSection> = allocations
        .iter()
        .map(|a| {
            let report = reports
                .get(&a.issue_key)
                .map(|r| match r {
                    Ok(t) => t.clone(),
                    Err(e) => format!("（AI 生成失败: {e}）"),
                })
                .unwrap_or_default();
            LogSection { issue_key: a.issue_key.clone(), repo: a.repo.clone(), seconds: a.seconds, report }
        })
        .collect();
    let log_file = logfile::write_log(&cfg.log_dir, date, cfg.total_daily_seconds, &sections)?;
    outcome.log_file = Some(log_file.clone());

    // 5) Tempo writes (gated by dry_run; per-issue dedup via live check).
    let started = resolve_started(cfg, date);
    for a in &allocations {
        let Some(report_res) = reports.get(&a.issue_key) else {
            continue;
        };
        let Ok(report) = report_res else {
            outcome.failed.push((a.issue_key.clone(), "AI 生成失败，未写入".into()));
            continue;
        };
        let id = match store.issue_id(&a.issue_key) {
            Ok(id) => id,
            Err(e) => {
                let msg = e.to_string();
                notify::notify("issue 解析失败", &format!("{}: {}", a.issue_key, msg));
                outcome.failed.push((a.issue_key.clone(), msg));
                continue;
            }
        };
        // On a check error, assume "may exist" and skip, to avoid duplicates.
        let exists = match store.has_worklog_for(id, date) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("worklog 检查失败 ({}): {}，为安全起见跳过写入", a.issue_key, e);
                outcome.skipped_existing.push(a.issue_key.clone());
                continue;
            }
        };
        if exists {
            outcome.skipped_existing.push(a.issue_key.clone());
            continue;
        }
        if dry_run {
            outcome.planned.push((a.issue_key.clone(), a.seconds));
            continue;
        }
        match store.create_worklog(id, a.seconds, report, &started, true) {
            Ok(wid) => {
                outcome.created.push((a.issue_key.clone(), a.seconds, wid));
            }
            Err(e) => {
                let msg = e.to_string();
                notify::notify("写入 Tempo 失败", &format!("issue {}: {}", a.issue_key, msg));
                outcome.failed.push((a.issue_key.clone(), msg));
            }
        }
    }

    // 6) Persist the report drafts (for the .log we already wrote them).
    for (k, r) in &reports {
        if let Ok(t) = r {
            outcome.report_by_issue.insert(k.clone(), t.clone());
        }
    }

    Ok(outcome)
}

/// Record the outcome in state (only for real, non-dry-run runs).
pub fn save_state(cfg: &Config, date: NaiveDate, outcome: &DayOutcome, dry_run: bool) {
    if dry_run {
        return;
    }
    let mut st = State::load(&cfg.log_dir);
    let mut wl = BTreeMap::new();
    for (k, s, _) in &outcome.created {
        wl.insert(k.clone(), *s);
    }
    for (k, s) in &outcome.planned {
        wl.insert(k.clone(), *s);
    }
    st.mark_processed(date, wl, outcome.report_by_issue.clone());
    if let Err(e) = st.save(&cfg.log_dir) {
        tracing::warn!("写入 state 失败: {e}");
    }
}

/// Persist the skip reason (only for real, non-dry-run runs).
pub fn save_skip(cfg: &Config, date: NaiveDate, reason: &str, dry_run: bool) {
    if dry_run {
        return;
    }
    let mut st = State::load(&cfg.log_dir);
    st.mark_skipped(date, reason.to_string());
    if let Err(e) = st.save(&cfg.log_dir) {
        tracing::warn!("写入 state（skip）失败: {e}");
    }
}

/// Top-level: collect + process + state + print summary.
pub fn run_day(cfg: &Config, date: NaiveDate, ai: &dyn AIClient, store: &dyn WorklogStore, dry_run: bool) -> anyhow::Result<DayOutcome> {
    tracing::info!("==== 处理 {} (dry_run={}) ====", date, dry_run);
    let collected = collect_day(cfg, date)?;
    if let Some(reason) = &collected.skipped {
        tracing::info!("整日跳过: {reason}");
        let outcome = DayOutcome { skipped_reason: Some(reason.clone()), ..Default::default() };
        save_skip(cfg, date, reason, dry_run);
        return Ok(outcome);
    }
    let outcome = process_day(cfg, date, &collected.repo_days, ai, store, dry_run)?;
    save_state(cfg, date, &outcome, dry_run);
    print_summary(date, &outcome, dry_run);
    Ok(outcome)
}

fn print_summary(date: NaiveDate, o: &DayOutcome, dry_run: bool) {
    if let Some(f) = &o.log_file {
        tracing::info!("日报文件: {}", f.display());
    }
    for (k, s, wid) in &o.created {
        tracing::info!("已写入 Tempo: issue {} {} 秒 (worklog id {wid})", k, s);
    }
    for (k, s) in &o.planned {
        tracing::info!("[dry-run] 将写入 Tempo: issue {} {} 秒", k, s);
    }
    for k in &o.skipped_existing {
        tracing::info!("跳过（已有工时）: issue {}", k);
    }
    for (k, e) in &o.failed {
        tracing::warn!("失败: issue {}: {}", k, e);
    }
    if dry_run {
        tracing::info!("[dry-run] 未对 Tempo 做任何写入");
    }
    let _ = date;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_collector::Commit;

    // --- Mocks ---
    struct MockAi {
        text: String,
        fail: bool,
    }
    impl AIClient for MockAi {
        fn generate(&self, _s: &str, _u: &str) -> anyhow::Result<String> {
            if self.fail {
                anyhow::bail!("boom");
            }
            Ok(self.text.clone())
        }
    }

    struct MockStore {
        ids: BTreeMap<String, u64>,
        existing: Vec<u64>, // issue ids that already have a worklog
        reachable_ok: bool,
        created: std::sync::Mutex<Vec<String>>,
        started: std::sync::Mutex<Vec<String>>, // captured `started` per write
    }
    impl WorklogStore for MockStore {
        fn reachable(&self) -> anyhow::Result<()> {
            if self.reachable_ok { Ok(()) } else { anyhow::bail!("down") }
        }
        fn issue_id(&self, key: &str) -> anyhow::Result<u64> {
            self.ids.get(key).copied().ok_or_else(|| anyhow::anyhow!("no issue {key}"))
        }
        fn has_worklog_for(&self, id: u64, _date: NaiveDate) -> anyhow::Result<bool> {
            Ok(self.existing.contains(&id))
        }
        fn create_worklog(&self, _id: u64, seconds: u64, comment: &str, started: &str, _billable: bool) -> anyhow::Result<u64> {
            self.started.lock().unwrap().push(started.to_string());
            self.created.lock().unwrap().push(format!("{comment}:{seconds}"));
            Ok(1)
        }
    }

    fn rc_one(subject: &str) -> RepoCommits {
        RepoCommits {
            repo: "D:\\r".into(),
            commits: vec![Commit {
                sha: "abc".into(),
                committer_date: "2026-09-09T10:00:00+08:00".into(),
                subject: subject.into(),
                patch: "+++ b/x\n+add line\n".into(),
                added: 1,
                removed: 0,
            }],
            error: None,
        }
    }

    fn cfg(log_dir: &std::path::Path) -> Config {
        Config {
            jira_base_url: "http://x".into(),
            jira_user: "u".into(),
            jira_password: None,
            tempo_version: 4,
            worker: "W".into(),
            check_time: crate::config::CheckTime::default(),
            work_start: "09:00".into(),
            work_end: "18:00".into(),
            worklog_start: None,
            total_daily_seconds: 28800,
            log_dir: log_dir.to_path_buf(),
            holidays_dir: log_dir.join("holidays"),
            ai_base_url: "http://ai/v1".into(),
            ai_api_key: "k".into(),
            ai_model: "m".into(),
            repos: vec![],
            git_email: None,
            worklog_search_path: "/rest/tempo-timesheets/4/worklogs/search".into(),
        }
    }

    fn tmp() -> std::path::PathBuf {
        static C: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = C.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("pipe-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const DATE: chrono::NaiveDate = chrono::NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();

    #[test]
    fn single_repo_writes_full_8h() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![RepoDay { repo: "D:\\r".into(), issue_key: "A-1".into(), commits: rc_one("s"), prompt_file: None }];
        let ai = MockAi { text: "增加功能".into(), fail: false };
        let store = MockStore {
            ids: [("A-1".to_string(), 100)].into_iter().collect(),
            existing: vec![],
            reachable_ok: true,
            created: std::sync::Mutex::new(vec![]),
            started: std::sync::Mutex::new(vec![]),
        };
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert_eq!(o.created.len(), 1);
        assert_eq!(o.created[0].1, 28800);
        assert_eq!(store.created.lock().unwrap()[0], "增加功能:28800");
        assert!(o.log_file.as_ref().unwrap().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_worklog_is_skipped() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![RepoDay { repo: "D:\\r".into(), issue_key: "A-1".into(), commits: rc_one("s"), prompt_file: None }];
        let ai = MockAi { text: "x".into(), fail: false };
        let store = MockStore {
            ids: [("A-1".to_string(), 100)].into_iter().collect(),
            existing: vec![100],
            reachable_ok: true,
            created: std::sync::Mutex::new(vec![]),
            started: std::sync::Mutex::new(vec![]),
        };
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert!(o.created.is_empty());
        assert_eq!(o.skipped_existing, vec!["A-1".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ai_failure_writes_nothing() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![RepoDay { repo: "D:\\r".into(), issue_key: "A-1".into(), commits: rc_one("s"), prompt_file: None }];
        let ai = MockAi { text: "".into(), fail: true };
        let store = MockStore {
            ids: [("A-1".to_string(), 100)].into_iter().collect(),
            existing: vec![],
            reachable_ok: true,
            created: std::sync::Mutex::new(vec![]),
            started: std::sync::Mutex::new(vec![]),
        };
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert!(o.created.is_empty());
        assert_eq!(o.failed.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_plans_but_writes_nothing() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![RepoDay { repo: "D:\\r".into(), issue_key: "A-1".into(), commits: rc_one("s"), prompt_file: None }];
        let ai = MockAi { text: "x".into(), fail: false };
        let store = MockStore {
            ids: [("A-1".to_string(), 100)].into_iter().collect(),
            existing: vec![],
            reachable_ok: true,
            created: std::sync::Mutex::new(vec![]),
            started: std::sync::Mutex::new(vec![]),
        };
        let o = process_day(&c, DATE, &rd, &ai, &store, true).unwrap();
        assert!(o.created.is_empty());
        assert_eq!(o.planned, vec![("A-1".to_string(), 28800)]);
        assert!(store.created.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- resolve_started (worklog time: exact configured time) ---

    #[test]
    fn resolve_started_exact_is_fixed() {
        let dir = tmp();
        let mut c = cfg(&dir);
        c.worklog_start = Some("09:00".into());
        let s = resolve_started(&c, DATE);
        assert_eq!(s, "2026-09-09 09:00:00");
        // Repeated calls stay identical.
        assert_eq!(s, resolve_started(&c, DATE));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_started_falls_back_to_work_start_when_absent() {
        let dir = tmp();
        let c = cfg(&dir); // worklog_start: None, work_start 09:00
        assert_eq!(resolve_started(&c, DATE), "2026-09-09 09:00:00");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn process_day_stamps_configured_started() {
        // Exact configured time must be stamped on the worklog verbatim.
        let dir = tmp();
        let mut c = cfg(&dir);
        c.worklog_start = Some("14:00".into());
        let rd = vec![RepoDay { repo: "D:\\r".into(), issue_key: "A-1".into(), commits: rc_one("s"), prompt_file: None }];
        let ai = MockAi { text: "x".into(), fail: false };
        let store = MockStore {
            ids: [("A-1".to_string(), 100)].into_iter().collect(),
            existing: vec![],
            reachable_ok: true,
            created: std::sync::Mutex::new(vec![]),
            started: std::sync::Mutex::new(vec![]),
        };
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert_eq!(o.created.len(), 1);
        assert_eq!(store.started.lock().unwrap()[0], "2026-09-09 14:00:00");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
