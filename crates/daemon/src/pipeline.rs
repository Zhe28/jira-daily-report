//! The daily pipeline: holiday check -> git collect -> (skip?) -> AI report ->
//! hour allocation -> merged .log -> Tempo worklog write (per issue) -> state.

use std::collections::BTreeMap;

use chrono::{NaiveDate, NaiveTime};
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
    /// `started` times of this issue's worklogs on `date`; `None` = entry
    /// exists but its `started` could not be parsed (treat as blocking both
    /// the normal and the overtime write).
    fn worklog_started_times(&self, id: u64, date: NaiveDate) -> anyhow::Result<Vec<Option<NaiveTime>>>;
    fn create_worklog(&self, id: u64, seconds: u64, comment: &str, started: &str, billable: bool) -> anyhow::Result<u64>;
}

/// One repo that had commits on the target day.
#[derive(Debug, Clone)]
pub struct RepoDay {
    pub repo: String,
    pub issue_key: String,
    /// Commits in the normal work window ([work_start, work_end)).
    pub commits: RepoCommits,
    /// Commits from work_end until end of day (overtime; empty when none, or
    /// when `overtime` is disabled in config).
    pub overtime_commits: RepoCommits,
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
    /// Total overtime seconds written (or planned) for the day; None when the
    /// day had no overtime worklog.
    pub overtime_seconds: Option<u64>,
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

    // 2) Git collect per repo. Normal pass: [work_start, work_end). Overtime
    //    pass (when enabled): [work_end, end of day).
    let mut repo_days = Vec::new();
    for r in &cfg.repos {
        let email = cfg.resolve_git_email(r).ok_or_else(|| {
            anyhow::anyhow!(
                "repo {} 无可用 git 提交者身份（应在启动检查时已拦截）",
                r.local_path.display()
            )
        })?;
        let rc = git_collector::collect_for(&r.local_path, &email, date, cfg.work_start_time(), cfg.work_end_time());
        let rc_ot = if cfg.overtime {
            git_collector::collect_for_overtime(&r.local_path, &email, date, cfg.work_end_time())
        } else {
            RepoCommits { repo: r.local_path.display().to_string(), commits: vec![], error: None }
        };
        if let Some(e) = &rc.error {
            tracing::warn!("repo {}: {}", r.local_path.display(), e);
        }
        if rc.has_commits() || rc_ot.has_commits() {
            repo_days.push(RepoDay {
                repo: r.local_path.display().to_string(),
                issue_key: r.issue_key.clone(),
                commits: rc,
                overtime_commits: rc_ot,
                prompt_file: r.prompt_file.clone(),
            });
        }
    }

    if repo_days.is_empty() {
        tracing::info!("{} 所有仓库在窗口内（含加班窗口）均无提交，整日跳过", date);
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

    // 1) Normal pass: AI report + weights for repos with work-window commits.
    let mut reports: BTreeMap<String, Result<String, String>> = BTreeMap::new();
    let mut weights: Vec<RepoWeight> = Vec::new();
    for rd in repo_days.iter().filter(|rd| rd.commits.has_commits()) {
        weights.push(weight_of(&rd.commits, &rd.repo, &rd.issue_key));
        reports.insert(rd.issue_key.clone(), generate_report(ai, rd, &rd.commits, false));
    }
    let normal_allocs = worklog_plan::allocate(cfg.total_daily_seconds, &weights);

    // 2) Overtime pass (when enabled): one global duration from the latest
    //    overtime commit, split across overtime repos by the same weighting.
    let mut ot_reports: BTreeMap<String, Result<String, String>> = BTreeMap::new();
    let ot_total = if cfg.overtime {
        let last = repo_days
            .iter()
            .filter(|rd| rd.overtime_commits.has_commits())
            .flat_map(|rd| rd.overtime_commits.commits.iter())
            .filter_map(|c| parse_local_time(&c.committer_date))
            .max();
        match last {
            Some(last) => {
                let ot_weights: Vec<RepoWeight> = repo_days
                    .iter()
                    .filter(|rd| rd.overtime_commits.has_commits())
                    .map(|rd| weight_of(&rd.overtime_commits, &rd.repo, &rd.issue_key))
                    .collect();
                let ot_total = worklog_plan::overtime_total_seconds(last, cfg.work_end_time());
                for rd in repo_days.iter().filter(|rd| rd.overtime_commits.has_commits()) {
                    ot_reports.insert(rd.issue_key.clone(), generate_report(ai, rd, &rd.overtime_commits, true));
                }
                outcome.overtime_seconds = Some(ot_total);
                Some((ot_total, worklog_plan::allocate(ot_total, &ot_weights)))
            }
            None => None,
        }
    } else {
        None
    };

    // 3) Readiness check (<=5s) — notify on failure, continue (best effort).
    if let Err(e) = store.reachable() {
        notify::notify("Jira 检查超时/失败", &format!("{e} — 请手动处理，工具仍会尝试补跑"));
    }

    // 4) Merged .log (local artifact; written even in dry-run for review).
    let mut sections: Vec<LogSection> = Vec::new();
    for a in &normal_allocs {
        sections.push(section_from(&a.issue_key, &a.repo, a.seconds, reports.get(&a.issue_key), false));
    }
    if let Some((_, ot_allocs)) = &ot_total {
        for a in ot_allocs {
            sections.push(section_from(&a.issue_key, &a.repo, a.seconds, ot_reports.get(&a.issue_key), true));
        }
    }
    let normal_total = if normal_allocs.is_empty() { 0 } else { cfg.total_daily_seconds };
    let log_total = normal_total + ot_total.as_ref().map(|(t, _)| *t).unwrap_or(0);
    let log_file = logfile::write_log(&cfg.log_dir, date, log_total, &sections)?;
    outcome.log_file = Some(log_file.clone());

    // 5) Tempo writes (gated by dry_run; per-issue dedup via live check).
    //    One snapshot of existing `started` times per issue, shared by both the
    //    normal (day, started < work_end) and overtime (evening, started >=
    //    work_end) decisions.
    let started_normal = resolve_started(cfg, date);
    let started_ot = started_str(date.and_time(cfg.work_end_time()));
    let work_end = cfg.work_end_time();
    let mut jobs: Vec<(String, worklog_plan::Allocated, &Result<String, String>, String /*started*/, bool /*evening*/)> = Vec::new();
    for a in &normal_allocs {
        if let Some(r) = reports.get(&a.issue_key) {
            jobs.push((a.issue_key.clone(), a.clone(), r, started_normal.clone(), false));
        }
    }
    if let Some((_, ot_allocs)) = &ot_total {
        for a in ot_allocs {
            if let Some(r) = ot_reports.get(&a.issue_key) {
                jobs.push((a.issue_key.clone(), a.clone(), r, started_ot.clone(), true));
            }
        }
    }

    let mut snapshot: BTreeMap<String, Option<Result<Vec<Option<NaiveTime>>, String>>> = BTreeMap::new();
    for (issue_key, ..) in &jobs {
        if snapshot.contains_key(issue_key) {
            continue;
        }
        let res = match store.issue_id(issue_key) {
            Ok(id) => match store.worklog_started_times(id, date) {
                Ok(times) => Ok(times),
                Err(e) => {
                    tracing::warn!("worklog 检查失败 ({issue_key}): {e}，为安全起见跳过写入");
                    Err(e.to_string())
                }
            },
            Err(e) => {
                let msg = e.to_string();
                notify::notify("issue 解析失败", &format!("{issue_key}: {msg}"));
                Err(msg)
            }
        };
        snapshot.insert(issue_key.clone(), Some(res));
    }
    for (issue_key, a, report_res, started, evening) in jobs {
        let Some(Some(res)) = snapshot.get(&issue_key) else { continue; };
        // On a check error, assume "may exist" and skip, to avoid duplicates.
        let times = match res {
            Ok(t) => t,
            Err(_) => {
                outcome.skipped_existing.push(issue_key.clone());
                continue;
            }
        };
        // A `None` entry (unparseable started) conservatively blocks both buckets.
        let blocked = if evening {
            times.iter().any(|t| t.is_none() || t.is_some_and(|t| t >= work_end))
        } else {
            times.iter().any(|t| t.is_none() || t.is_some_and(|t| t < work_end))
        };
        if blocked {
            outcome.skipped_existing.push(issue_key.clone());
            continue;
        }
        match report_res {
            Ok(report) => {
                let id = match store.issue_id(&issue_key) {
                    Ok(id) => id,
                    Err(e) => {
                        let msg = e.to_string();
                        outcome.failed.push((issue_key.clone(), msg));
                        continue;
                    }
                };
                if dry_run {
                    outcome.planned.push((issue_key.clone(), a.seconds));
                    continue;
                }
                match store.create_worklog(id, a.seconds, report, &started, true) {
                    Ok(wid) => {
                        outcome.created.push((issue_key.clone(), a.seconds, wid));
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        let what = if evening { "加班" } else { "日常" };
                        notify::notify("写入 Tempo 失败", &format!("issue {issue_key}（{what}）: {msg}"));
                        outcome.failed.push((issue_key.clone(), msg));
                    }
                }
            }
            Err(_) => {
                let label = if evening { format!("{issue_key}（加班）") } else { issue_key.clone() };
                outcome.failed.push((label, "AI 生成失败，未写入".into()));
            }
        }
    }

    // 6) Persist the report drafts (for the .log we already wrote them).
    for (k, r) in &reports {
        if let Ok(t) = r {
            outcome.report_by_issue.insert(k.clone(), t.clone());
        }
    }
    for (k, r) in &ot_reports {
        if let Ok(t) = r {
            outcome.report_by_issue.insert(format!("{k}（加班）"), t.clone());
        }
    }

    Ok(outcome)
}

fn weight_of(commits: &RepoCommits, repo: &str, issue_key: &str) -> RepoWeight {
    RepoWeight {
        repo: repo.to_string(),
        issue_key: issue_key.to_string(),
        commit_count: commits.commits.len() as u64,
        added: commits.total_added(),
        removed: commits.total_removed(),
    }
}

fn generate_report(ai: &dyn AIClient, rd: &RepoDay, commits: &RepoCommits, overtime: bool) -> Result<String, String> {
    let repo_path = std::path::Path::new(&rd.repo);
    let custom_prompt = rd.prompt_file.as_deref().and_then(|pf| reporter::read_repo_prompt(repo_path, pf));
    match reporter::report_repo_commits(ai, commits, custom_prompt.as_deref(), overtime) {
        Ok(text) => Ok(text),
        Err(e) => {
            let what = if overtime { "加班日报" } else { "日报" };
            let msg = e.to_string();
            tracing::error!("issue {} {what}生成失败: {}", rd.issue_key, msg);
            notify::notify(&format!("AI {what}生成失败"), &format!("issue {} ({}): {}", rd.issue_key, rd.repo, msg));
            Err(msg)
        }
    }
}

fn section_from(issue_key: &str, repo: &str, seconds: u64, report: Option<&Result<String, String>>, overtime: bool) -> LogSection {
    let text = report
        .map(|r| match r {
            Ok(t) => t.clone(),
            Err(e) => format!("（AI 生成失败: {e}）"),
        })
        .unwrap_or_default();
    LogSection { issue_key: issue_key.to_string(), repo: repo.to_string(), seconds, report: text, overtime }
}

/// Parse an RFC3339 commit timestamp to local time of day (for overtime duration).
fn parse_local_time(cdate: &str) -> Option<NaiveTime> {
    chrono::DateTime::parse_from_rfc3339(cdate).ok().map(|dt| dt.with_timezone(&chrono::Local).time())
}

/// Record the outcome in state (only for real, non-dry-run runs).
pub fn save_state(cfg: &Config, date: NaiveDate, outcome: &DayOutcome, dry_run: bool) {
    if dry_run {
        return;
    }
    let mut st = State::load(&cfg.log_dir);
    let mut wl = BTreeMap::new();
    for (k, s, _) in &outcome.created {
        *wl.entry(k.clone()).or_insert(0) += s;
    }
    for (k, s) in &outcome.planned {
        *wl.entry(k.clone()).or_insert(0) += s;
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
    if let Some(s) = o.overtime_seconds {
        tracing::info!("加班工时合计: {} 秒", s);
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
        /// issue id -> `started` times of existing worklogs on the target day
        /// (a `None` entry simulates an unparseable `started`).
        existing: BTreeMap<u64, Vec<Option<NaiveTime>>>,
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
        fn worklog_started_times(&self, id: u64, _date: NaiveDate) -> anyhow::Result<Vec<Option<NaiveTime>>> {
            Ok(self.existing.get(&id).cloned().unwrap_or_default())
        }
        fn create_worklog(&self, _id: u64, seconds: u64, comment: &str, started: &str, _billable: bool) -> anyhow::Result<u64> {
            self.started.lock().unwrap().push(started.to_string());
            self.created.lock().unwrap().push(format!("{comment}:{seconds}"));
            Ok(1)
        }
    }

    fn nt(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
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

    fn rc_empty() -> RepoCommits {
        RepoCommits { repo: "D:\\r".into(), commits: vec![], error: None }
    }

    fn rc_overtime(subject: &str, time: &str) -> RepoCommits {
        RepoCommits {
            repo: "D:\\r".into(),
            commits: vec![Commit {
                sha: "def".into(),
                committer_date: format!("2026-09-09T{time}:00+08:00"),
                subject: subject.into(),
                patch: "+++ b/y\n+ot line\n".into(),
                added: 1,
                removed: 0,
            }],
            error: None,
        }
    }

    fn rd_normal(subject: &str) -> RepoDay {
        RepoDay { repo: "D:\\r".into(), issue_key: "A-1".into(), commits: rc_one(subject), overtime_commits: rc_empty(), prompt_file: None }
    }

    fn rd_overtime(subject: &str, time: &str) -> RepoDay {
        RepoDay { repo: "D:\\r".into(), issue_key: "A-1".into(), commits: rc_empty(), overtime_commits: rc_overtime(subject, time), prompt_file: None }
    }

    fn rd_both() -> RepoDay {
        RepoDay { repo: "D:\\r".into(), issue_key: "A-1".into(), commits: rc_one("day"), overtime_commits: rc_overtime("night", "20:00"), prompt_file: None }
    }

    fn store_with(ids: &[(String, u64)], existing: Vec<(u64, Vec<Option<NaiveTime>>)>) -> MockStore {
        MockStore {
            ids: ids.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            existing: existing.into_iter().collect(),
            reachable_ok: true,
            created: std::sync::Mutex::new(vec![]),
            started: std::sync::Mutex::new(vec![]),
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
            overtime: true,
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
    const ID_A1: u64 = 100;

    #[test]
    fn single_repo_writes_full_8h() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![rd_normal("s")];
        let ai = MockAi { text: "增加功能".into(), fail: false };
        let store = store_with(&[("A-1".to_string(), ID_A1)], vec![]);
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert_eq!(o.created.len(), 1);
        assert_eq!(o.created[0].1, 28800);
        assert_eq!(store.created.lock().unwrap()[0], "增加功能:28800");
        assert_eq!(o.overtime_seconds, None);
        assert!(o.log_file.as_ref().unwrap().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_worklog_is_skipped() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![rd_normal("s")];
        let ai = MockAi { text: "x".into(), fail: false };
        // A hand-written day entry (09:00 < 18:00) blocks the normal write.
        let store = store_with(&[("A-1".to_string(), ID_A1)], vec![(ID_A1, vec![Some(nt(9, 0))])]);
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert!(o.created.is_empty());
        assert_eq!(o.skipped_existing, vec!["A-1".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ai_failure_writes_nothing() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![rd_normal("s")];
        let ai = MockAi { text: "".into(), fail: true };
        let store = store_with(&[("A-1".to_string(), ID_A1)], vec![]);
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert!(o.created.is_empty());
        assert_eq!(o.failed.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_plans_but_writes_nothing() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![rd_normal("s")];
        let ai = MockAi { text: "x".into(), fail: false };
        let store = store_with(&[("A-1".to_string(), ID_A1)], vec![]);
        let o = process_day(&c, DATE, &rd, &ai, &store, true).unwrap();
        assert!(o.created.is_empty());
        assert_eq!(o.planned, vec![("A-1".to_string(), 28800)]);
        assert!(store.created.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- overtime ---

    #[test]
    fn overtime_writes_second_worklog_at_work_end() {
        let dir = tmp();
        let c = cfg(&dir);
        // Day commit at 10:00 + overtime commit at 20:00 -> 2h overtime.
        let rd = vec![rd_both()];
        let ai = MockAi { text: "日报".into(), fail: false };
        let store = store_with(&[("A-1".to_string(), ID_A1)], vec![]);
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert_eq!(o.created.len(), 2);
        let secs: Vec<u64> = o.created.iter().map(|x| x.1).collect();
        assert_eq!(secs, vec![28800, 7200]);
        let starteds = store.started.lock().unwrap().clone();
        assert_eq!(starteds, vec!["2026-09-09 09:00:00", "2026-09-09 18:00:00"]);
        assert_eq!(o.overtime_seconds, Some(7200));
        // Merged log: 8h + 2h = 10h file, overtime section marked.
        let log_file = o.log_file.as_ref().unwrap().clone();
        assert_eq!(log_file.file_name().unwrap().to_string_lossy(), "2026-09-09-10小时.log");
        let body = std::fs::read_to_string(&log_file).unwrap();
        assert!(body.contains("加班"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overtime_skipped_when_evening_entry_exists() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![rd_both()];
        let ai = MockAi { text: "日报".into(), fail: false };
        // A hand-written evening entry (18:30 >= 18:00) blocks only overtime.
        let store = store_with(&[("A-1".to_string(), ID_A1)], vec![(ID_A1, vec![Some(nt(18, 30))])]);
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert_eq!(o.created.len(), 1);
        assert_eq!(o.created[0].1, 28800);
        assert_eq!(o.skipped_existing, vec!["A-1".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normal_skipped_when_day_entry_exists() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![rd_both()];
        let ai = MockAi { text: "日报".into(), fail: false };
        // A day entry (09:00 < 18:00) blocks only the normal write.
        let store = store_with(&[("A-1".to_string(), ID_A1)], vec![(ID_A1, vec![Some(nt(9, 0))])]);
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert_eq!(o.created.len(), 1);
        assert_eq!(o.created[0].1, 7200);
        assert_eq!(store.started.lock().unwrap()[0], "2026-09-09 18:00:00");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rerun_writes_nothing() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![rd_both()];
        let ai = MockAi { text: "日报".into(), fail: false };
        // Both of the tool's own entries exist -> both blocked.
        let store = store_with(&[("A-1".to_string(), ID_A1)], vec![(ID_A1, vec![Some(nt(9, 0)), Some(nt(18, 0))])]);
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert!(o.created.is_empty());
        assert_eq!(o.skipped_existing.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overtime_only_day_writes_only_overtime() {
        let dir = tmp();
        let c = cfg(&dir);
        // No day commits, one overtime commit at 20:00 -> 2h only, no 8h entry.
        let rd = vec![rd_overtime("night", "20:00")];
        let ai = MockAi { text: "加班日报".into(), fail: false };
        let store = store_with(&[("A-1".to_string(), ID_A1)], vec![]);
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert_eq!(o.created.len(), 1);
        assert_eq!(o.created[0].1, 7200);
        assert_eq!(store.started.lock().unwrap()[0], "2026-09-09 18:00:00");
        let log_file = o.log_file.as_ref().unwrap().clone();
        assert_eq!(log_file.file_name().unwrap().to_string_lossy(), "2026-09-09-2小时.log");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multi_repo_overtime_split_sums_to_total() {
        let dir = tmp();
        let c = cfg(&dir);
        // Two repos with overtime commits; latest 20:35 -> 3h total.
        let rd_b = {
            let mut rd = rd_overtime("b-night", "19:00");
            rd.issue_key = "B-1".into();
            rd
        };
        let rd_a = rd_overtime("a-night", "20:35");
        let rd = vec![rd_a, rd_b];
        let ai = MockAi { text: "日报".into(), fail: false };
        let store = store_with(&[("A-1".to_string(), ID_A1), ("B-1".to_string(), 200)], vec![]);
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert_eq!(o.created.len(), 2);
        let sum: u64 = o.created.iter().map(|x| x.1).sum();
        assert_eq!(sum, 10800);
        assert_eq!(o.overtime_seconds, Some(10800));
        // Both stamped at work_end.
        for s in store.started.lock().unwrap().iter() {
            assert_eq!(s.as_str(), "2026-09-09 18:00:00");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overtime_disabled_by_default() {
        let dir = tmp();
        let mut c = cfg(&dir);
        c.overtime = false;
        // Overtime commits present but the feature is off -> today's behavior:
        // exactly one 8h write, no evening write.
        let rd = vec![rd_both()];
        let ai = MockAi { text: "日报".into(), fail: false };
        let store = store_with(&[("A-1".to_string(), ID_A1)], vec![]);
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert_eq!(o.created.len(), 1);
        assert_eq!(o.created[0].1, 28800);
        assert_eq!(o.overtime_seconds, None);
        let log_file = o.log_file.as_ref().unwrap().clone();
        assert_eq!(log_file.file_name().unwrap().to_string_lossy(), "2026-09-09-8小时.log");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_error_skips_safely() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![rd_both()];
        let ai = MockAi { text: "日报".into(), fail: false };
        // worklog_started_times errors -> both entries for the issue skipped.
        let store = ErrStore { store: store_with(&[("A-1".to_string(), ID_A1)], vec![]) };
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert!(o.created.is_empty());
        assert_eq!(o.skipped_existing.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct ErrStore {
        store: MockStore,
    }
    impl WorklogStore for ErrStore {
        fn reachable(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn issue_id(&self, key: &str) -> anyhow::Result<u64> {
            self.store.issue_id(key)
        }
        fn worklog_started_times(&self, _id: u64, _date: NaiveDate) -> anyhow::Result<Vec<Option<NaiveTime>>> {
            anyhow::bail!("search down")
        }
        fn create_worklog(&self, id: u64, s: u64, c: &str, st: &str, b: bool) -> anyhow::Result<u64> {
            self.store.create_worklog(id, s, c, st, b)
        }
    }

    #[test]
    fn save_state_sums_both_entries() {
        let dir = tmp();
        let c = cfg(&dir);
        let rd = vec![rd_both()];
        let ai = MockAi { text: "日报".into(), fail: false };
        let store = store_with(&[("A-1".to_string(), ID_A1)], vec![]);
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        save_state(&c, DATE, &o, false);
        let st = State::load(&c.log_dir);
        assert_eq!(st.record(DATE).unwrap().worklogs.get("A-1"), Some(&36000));
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
        let rd = vec![rd_normal("s")];
        let ai = MockAi { text: "x".into(), fail: false };
        let store = store_with(&[("A-1".to_string(), ID_A1)], vec![]);
        let o = process_day(&c, DATE, &rd, &ai, &store, false).unwrap();
        assert_eq!(o.created.len(), 1);
        assert_eq!(store.started.lock().unwrap()[0], "2026-09-09 14:00:00");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
