//! autoGenDailyReport
//!
//! 常驻工具:监视本地 git 仓库,通过 OpenAI 兼容的 AI 端点基于昨日(09:00–18:00)
//! 提交生成中文日报,把合并后的 `.log` 保存到本地文件夹,并为目标日尚无工时记录的
//! 仓库自动补填 Jira Tempo 工时。

pub mod config;
pub mod git_collector;
pub mod holidays;
pub mod hotconfig;
pub mod logfile;
pub mod notify;
pub mod pipeline;
pub mod reporter;
pub mod scheduler;
pub mod state;
pub mod tempo;
pub mod tray;
pub mod web;
pub mod worklog_plan;
