//! autoGenDailyReport
//!
//! Resident tool that watches local git repos, generates a Chinese daily report
//! from yesterday's (09:00–18:00) commits via an OpenAI-compatible AI endpoint,
//! saves a merged `.log` to a local folder, and auto-fills a Jira Tempo worklog
//! for any repo whose issue has no worklog for the target day yet.

pub mod config;
pub mod holidays;
pub mod git_collector;
pub mod reporter;
pub mod worklog_plan;
pub mod logfile;
pub mod tempo;
pub mod state;
pub mod notify;
pub mod pipeline;
pub mod scheduler;
