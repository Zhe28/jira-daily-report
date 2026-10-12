//! Resident scheduling: startup catch-up + daily loop at the configured time.
//! The blocking pipeline runs on the async runtime via `spawn_blocking`.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{Local, NaiveTime};

use crate::hotconfig::HotConfig;
use crate::pipeline::{self, WorklogStore};
use crate::reporter::AIClient;

/// Today's target date = yesterday (local).
pub fn yesterday() -> chrono::NaiveDate {
    Local::now().date_naive() - chrono::Duration::days(1)
}

/// Next occurrence of `t` from now (today or tomorrow), as a chrono DateTime.
pub fn next_trigger(t: NaiveTime) -> chrono::DateTime<chrono::Local> {
    let now = Local::now();
    let today_at = now.date_naive().and_time(t);
    let target = if today_at > now.naive_local() {
        today_at
    } else {
        (now.date_naive() + chrono::Duration::days(1)).and_time(t)
    };
    target
        .and_local_timezone(chrono::Local)
        .earliest()
        .unwrap_or(now)
}

/// 客户端槽位：PUT /api/config 成功后由 web 层原子替换整体客户端，
/// 调度线程每轮从槽位读取最新实例（热生效）。
pub type ClientSlots = (
    Arc<RwLock<Arc<dyn AIClient>>>,
    Arc<RwLock<Arc<dyn WorklogStore>>>,
);

/// 从槽位读取当前客户端（短暂持读锁后立即释放）。
fn read_clients(slots: &ClientSlots) -> (Arc<dyn AIClient>, Arc<dyn WorklogStore>) {
    (
        slots.0.read().unwrap_or_else(|e| e.into_inner()).clone(),
        slots.1.read().unwrap_or_else(|e| e.into_inner()).clone(),
    )
}

/// Run the pipeline for `date`, blocking (run on a worker thread).
/// 客户端每轮从槽位读取（PUT /api/config 热切换后下一轮生效）。
fn run_for(
    hot: Arc<HotConfig>,
    date: chrono::NaiveDate,
    slots: &ClientSlots,
    dry_run: bool,
    on_done: &dyn Fn(pipeline::DayOutcome, Option<String>),
) {
    let (ai, store) = read_clients(slots);
    let cfg = hot.get();
    match pipeline::run_day(&cfg, date, &*ai, &*store, dry_run) {
        Ok(outcome) => {
            tracing::info!("==== {} 处理完成 ====", date);
            on_done(outcome, None);
        }
        Err(e) => {
            tracing::error!("==== {} 处理出错: {e} ====", date);
            crate::notify::notify("日报处理出错", &format!("{date}: {e}"));
            on_done(pipeline::DayOutcome::default(), Some(e.to_string()));
        }
    }
}

/// Resident loop: catch up on startup if past today's check time, then sleep to
/// each subsequent trigger.
pub fn run_resident(
    hot: Arc<HotConfig>,
    slots: ClientSlots,
    on_done: Box<dyn Fn(pipeline::DayOutcome, Option<String>) + Send + Sync>,
) {
    // 启动补跑用进入函数时的配置快照。
    let check = hot.get().check_time();
    let now = Local::now();
    let today_at = now.date_naive().and_time(check);

    // Startup catch-up: if we've already passed today's check time, run yesterday now.
    if now.naive_local() >= today_at {
        tracing::info!("启动时已过今日 {}，补跑昨日 {}", check, yesterday());
        run_for(hot.clone(), yesterday(), &slots, false, &*on_done);
    } else {
        tracing::info!("启动时未到今日 {}，等待到点", check);
    }

    loop {
        // 每轮从共享配置读取最新 check_time（PUT /api/config 热生效）。
        let check = hot.get().check_time();
        let next = next_trigger(check);
        let delay = (next - Local::now())
            .to_std()
            .unwrap_or_else(|_| Duration::from_secs(3600));
        // The day the pipeline will fill once the trigger fires (it uses `yesterday()`
        // at trigger time).
        let target = next.date_naive() - chrono::Duration::days(1);
        crate::notify::info(
            "下次写日志",
            &format!(
                "{} (约 {} 秒后)，届时自动补填 {} 的日报",
                next.format("%Y-%m-%d %H:%M:%S"),
                delay.as_secs(),
                target
            ),
        );
        std::thread::sleep(delay);

        run_for(hot.clone(), yesterday(), &slots, false, &*on_done);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Mutex;

    use crate::config::{CheckTime, Config};
    use crate::pipeline::DayOutcome;
    use chrono::{NaiveDate, NaiveTime};

    /// AI 客户端版本化 mock：`generate` 返回 `"{v}-report"`。
    struct VersionedAi {
        v: &'static str,
    }
    impl AIClient for VersionedAi {
        fn generate(&self, _system: &str, _user: &str) -> anyhow::Result<String> {
            Ok(format!("{}-report", self.v))
        }
    }

    /// 工作日志 store 版本化 mock：`create_worklog` 返回固定 wid，并捕获 comment。
    /// `comments` 用 Arc 共享，便于在把 store 放入槽位前保留观察句柄。
    struct VersionedStore {
        v: &'static str,
        wid: u64,
        comments: Arc<Mutex<Vec<String>>>,
    }
    impl WorklogStore for VersionedStore {
        fn reachable(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn issue_id(&self, _key: &str) -> anyhow::Result<u64> {
            Ok(1)
        }
        fn worklog_started_times(
            &self,
            _id: u64,
            _date: NaiveDate,
        ) -> anyhow::Result<Vec<Option<NaiveTime>>> {
            // 返回空 = 当天无已有工时 → 允许写入。
            Ok(vec![])
        }
        fn create_worklog(
            &self,
            _id: u64,
            _seconds: u64,
            comment: &str,
            _started: &str,
            _billable: bool,
        ) -> anyhow::Result<u64> {
            self.comments
                .lock()
                .unwrap()
                .push(format!("{}:{}", self.v, comment));
            Ok(self.wid)
        }
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("sched-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 建一个真实 git 仓库并放一笔 2026-09-09 10:00（本地时区）的提交。
    /// 用本机 offset 拼日期串，保证任何时区下 10:00 都落在 09:00–18:00 窗口内。
    fn make_repo(dir: &Path) {
        let date = {
            let off = chrono::Local::now().offset().to_string();
            format!("2026-09-09T10:00:00{off}")
        };
        let run = |args: &[&str]| -> std::process::Output {
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("run git")
        };
        let c = run(&["init", "-q"]);
        assert!(c.status.success(), "git init failed");
        let c = run(&["config", "user.email", "me@corp.com"]);
        assert!(c.status.success(), "git config email failed");
        let c = run(&["config", "user.name", "me"]);
        assert!(c.status.success(), "git config name failed");
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        let c = run(&["add", "a.txt"]);
        assert!(c.status.success(), "git add failed");
        let c = Command::new("git")
            .env("GIT_AUTHOR_NAME", "me")
            .env("GIT_AUTHOR_EMAIL", "me@corp.com")
            .env("GIT_COMMITTER_NAME", "me")
            .env("GIT_COMMITTER_EMAIL", "me@corp.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .args(["commit", "-q", "-m", "test commit", "--no-verify"])
            .current_dir(dir)
            .output()
            .expect("git commit");
        assert!(
            c.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&c.stderr)
        );
    }

    fn cfg_for(log_dir: &Path, repo_dir: &Path) -> Config {
        Config {
            jira_base_url: "http://x".into(),
            jira_user: "u".into(),
            jira_password: None,
            tempo_version: 4,
            worker: "W".into(),
            check_time: CheckTime::default(),
            work_start: "09:00".into(),
            work_end: "18:00".into(),
            worklog_start: None,
            total_daily_seconds: 28800,
            overtime: false,
            log_dir: log_dir.to_path_buf(),
            holidays_dir: log_dir.join("holidays"),
            ai_base_url: "http://ai/v1".into(),
            ai_api_key: "k".into(),
            ai_model: "m".into(),
            repos: vec![crate::config::Repo {
                local_path: repo_dir.to_path_buf(),
                issue_key: "A-1".into(),
                git_email: Some("me@corp.com".into()),
                prompt_file: None,
            }],
            git_email: None,
            worklog_search_path: "/rest/tempo-timesheets/4/worklogs/search".into(),
        }
    }

    /// RUST-004 回归：run_for 每轮从槽位读客户端。
    /// 第一轮必须用槽位里的 V1；槽位换成 V2 后第二轮必须用 V2。
    #[test]
    fn run_for_reads_clients_from_slot_each_round() {
        let root = tmp_dir("hotswap");
        let log_dir = root.join("log");
        let repo_dir = root.join("repo");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::create_dir_all(&repo_dir).unwrap();
        make_repo(&repo_dir);

        let hot = Arc::new(HotConfig::new(cfg_for(&log_dir, &repo_dir)));
        let date = NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();

        // 槽位初始 = V1 客户端（先建共享 comments 句柄再放入槽位）。
        let ai_slot: Arc<RwLock<Arc<dyn AIClient>>> =
            Arc::new(RwLock::new(Arc::new(VersionedAi { v: "v1" })));
        let v1_comments: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let store_slot: Arc<RwLock<Arc<dyn WorklogStore>>> =
            Arc::new(RwLock::new(Arc::new(VersionedStore {
                v: "v1",
                wid: 111,
                comments: v1_comments.clone(),
            })));

        let slots: ClientSlots = (ai_slot.clone(), store_slot.clone());
        let outcome1: Mutex<Option<DayOutcome>> = Mutex::new(None);
        run_for(hot.clone(), date, &slots, false, &|o, e| {
            assert!(e.is_none(), "第一轮不应出错: {e:?}");
            *outcome1.lock().unwrap() = Some(o);
        });
        let o1 = outcome1.lock().unwrap().take().expect("第一轮应完成");
        assert_eq!(o1.created.len(), 1, "第一轮应写入一条工时");
        assert_eq!(o1.created[0].0, "A-1");
        assert_eq!(o1.created[0].2, 111, "第一轮 worklog_id 应来自 V1 store");
        let c1: Vec<String> = v1_comments.lock().unwrap().clone();
        assert_eq!(c1.len(), 1);
        assert!(
            c1[0].starts_with("v1:"),
            "第一轮 comment 应来自 V1 AI: {c1:?}"
        );

        // 把槽位原子替换为 V2 客户端（模拟 PUT /api/config 成功后的热切换）。
        let v2_comments: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        *ai_slot.write().unwrap() = Arc::new(VersionedAi { v: "v2" });
        *store_slot.write().unwrap() = Arc::new(VersionedStore {
            v: "v2",
            wid: 222,
            comments: v2_comments.clone(),
        });

        let outcome2: Mutex<Option<DayOutcome>> = Mutex::new(None);
        run_for(hot.clone(), date, &slots, false, &|o, e| {
            assert!(e.is_none(), "第二轮不应出错: {e:?}");
            *outcome2.lock().unwrap() = Some(o);
        });
        let o2 = outcome2.lock().unwrap().take().expect("第二轮应完成");
        // 关键是 wid 必须来自 V2 store（=222），证明客户端已热切换。
        assert_eq!(o2.created.len(), 1, "第二轮应写入一条工时");
        assert_eq!(
            o2.created[0].2, 222,
            "第二轮 worklog_id 应来自 V2 store（热切换生效）"
        );

        // V2 store 捕获的 comment 应由 V2 AI 生成（"{v}-report"）。
        let c2: Vec<String> = v2_comments.lock().unwrap().clone();
        assert_eq!(c2.len(), 1);
        assert!(
            c2[0].starts_with("v2:"),
            "第二轮 comment 应来自 V2 AI: {c2:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
