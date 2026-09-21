//! Resident scheduling: startup catch-up + daily loop at the configured time.
//! The blocking pipeline runs on the async runtime via `spawn_blocking`.

use std::sync::Arc;
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

/// Run the pipeline for `date`, blocking (run on a worker thread).
fn run_for(
    hot: Arc<HotConfig>,
    date: chrono::NaiveDate,
    ai: Arc<dyn AIClient>,
    store: Arc<dyn WorklogStore>,
    dry_run: bool,
    on_done: &dyn Fn(pipeline::DayOutcome, Option<String>),
) {
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
    ai: Arc<dyn AIClient>,
    store: Arc<dyn WorklogStore>,
    on_done: Box<dyn Fn(pipeline::DayOutcome, Option<String>) + Send + Sync>,
) {
    // 启动补跑用进入函数时的配置快照。
    let check = hot.get().check_time();
    let now = Local::now();
    let today_at = now.date_naive().and_time(check);

    // Startup catch-up: if we've already passed today's check time, run yesterday now.
    if now.naive_local() >= today_at {
        tracing::info!("启动时已过今日 {}，补跑昨日 {}", check, yesterday());
        run_for(hot.clone(), yesterday(), ai.clone(), store.clone(), false, &*on_done);
    } else {
        tracing::info!("启动时未到今日 {}，等待到点", check);
    }

    loop {
        // 每轮从共享配置读取最新 check_time（PUT /api/config 热生效）。
        let check = hot.get().check_time();
        let next = next_trigger(check);
        let delay = (next - Local::now()).to_std().unwrap_or_else(|_| Duration::from_secs(3600));
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

        run_for(hot.clone(), yesterday(), ai.clone(), store.clone(), false, &*on_done);
    }
}
