//! Jira + Tempo REST 客户端(基于 cookie 的会话认证,经 login.jsp)。
//!
//! - `issue_id(issue_key)` — 将 issue key 解析为数字 id(创建工时所需)。
//! - `worklog_started_times(issue_id, date)` — 该 issue 在指定日期上所有工时的
//!   `started` 时间(用于避免覆盖手写记录,并允许在日常之外单独写一条加班工时)。
//! - `create_worklog(...)` — 写入新的 Tempo 工时。
//!
//! 搜索端点/方法在不同 Tempo 版本间有差异;搜索路径与方法可配置,以便在不改代码的情况下针对线上服务器调整。

use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct TempoClient {
    http: reqwest::blocking::Client,
    base_url: String,
    user: String,
    pass: String,
    version: u32,
    worker: String,
    search_path: String,
}

#[derive(Deserialize)]
struct IssueJson {
    /// Jira 返回的 issue id 是字符串,如 `"155324"`。
    id: String,
}

#[derive(Deserialize)]
struct WorklogJson {
    #[serde(default)]
    issue: Option<IssueRef>,
    /// 例如 "2026-09-01 09:00:00.000" — 工作日期。
    #[serde(default)]
    started: Option<String>,
}

#[derive(Deserialize)]
struct IssueRef {
    id: u64,
}

/// 将 Tempo 的 `started` 值("YYYY-MM-DD HH:MM:SS.000")解析为其时刻部分。
fn parse_started_time(s: &str) -> Option<NaiveTime> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
        .ok()
        .map(|dt| dt.time())
}

impl TempoClient {
    pub fn new(
        base_url: &str,
        user: &str,
        pass: &str,
        version: u32,
        worker: &str,
        search_path: &str,
    ) -> Self {
        let http = reqwest::blocking::Client::builder()
            .cookie_store(true)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("build http client");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').into(),
            user: user.into(),
            pass: pass.into(),
            version,
            worker: worker.into(),
            search_path: search_path
                .trim_start_matches('/')
                .trim_end_matches('/')
                .to_string(),
        }
    }

    /// 执行请求;若服务器返回 401,则重新登录后重试一次。
    fn send_with_relogin(
        &self,
        build: impl Fn() -> reqwest::blocking::RequestBuilder,
    ) -> Result<reqwest::blocking::Response> {
        let resp = build().send().context("Jira request failed")?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            tracing::info!("Jira 401 — 正在重新登录…");
            self.login()?;
            return build().send().context("Jira request failed after re-login");
        }
        Ok(resp)
    }

    /// 通过 Jira 登录表单进行认证(cookie 会话)。
    /// 必须在任何 API 请求之前调用。
    pub fn login(&self) -> Result<()> {
        let url = format!("{}/login.jsp", self.base_url);
        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "os_username={}&os_password={}&os_cookie=true&os_destination=&user_role=&atl_token=&login=%E7%99%BB%E5%BD%95",
                urlencoding::encode(&self.user),
                urlencoding::encode(&self.pass),
            ))
            .send()
            .context("Jira login request failed")?;
        let status = resp.status();
        if status.is_server_error() {
            bail!("Jira login returned {status}");
        }
        // login.jsp 成功时返回 200(或 302 重定向——均可)。
        // Cookie 存储之后现在保存了 JSESSIONID + XSRF token,供后续请求使用。
        Ok(())
    }

    /// GET /rest/api/2/issue/{key} → 数字 id。
    pub fn issue_id(&self, issue_key: &str) -> Result<u64> {
        let url = format!("{}/rest/api/2/issue/{}", self.base_url, issue_key);
        let resp = self
            .send_with_relogin(|| self.http.get(&url))
            .with_context(|| format!("lookup issue {issue_key}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("Jira issue lookup for {issue_key} returned {status}");
        }
        let raw = resp.text().context("reading issue body")?;
        let j: IssueJson =
            serde_json::from_str(&raw).with_context(|| format!("parsing issue {issue_key}"))?;
        j.id.parse::<u64>()
            .with_context(|| format!("issue {issue_key} id '{}' is not numeric", j.id))
    }

    /// 该 issue 在 `date` 上所有工时的 `started` 时间。
    ///
    /// 已在线上服务器验证:`POST {search_path}`,请求体为
    /// `{"worker":["<worker>"], "from":"YYYY-MM-DD", "to":"YYYY-MM-DD"}`。
    /// `worker` 是字符串数组;`from`/`to` 只到日期(LocalDate)。
    /// 该端点忽略 issue 过滤条件,故需在客户端按 `issue.id` 且 `started` 日期
    /// 两者都匹配来过滤。无法解析 `started` 的条目返回 `None` — 调用方必须保守处理
    /// (视为该条目的存在会阻塞写入)。
    pub fn worklog_started_times(
        &self,
        issue_id: u64,
        date: NaiveDate,
    ) -> Result<Vec<Option<NaiveTime>>> {
        let day = date.format("%Y-%m-%d").to_string();
        let search = format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            self.search_path.trim_start_matches('/')
        );
        let body = serde_json::json!({
            "worker": [self.worker],
            "from": day,
            "to": day
        });
        let resp = self
            .send_with_relogin(|| self.http.post(&search).json(&body))
            .with_context(|| format!("searching worklogs (POST {})", self.search_path))?;

        let status = resp.status();
        if !status.is_success() {
            bail!(
                "worklog search returned {status}: {}",
                resp.text().unwrap_or_default()
            );
        }
        let logs: Vec<WorklogJson> = resp.json().context("parsing worklog search")?;
        Ok(logs
            .iter()
            .filter(|w| {
                w.issue.as_ref().map(|i| i.id == issue_id).unwrap_or(false)
                    && w.started
                        .as_deref()
                        .map(|s| s.starts_with(&day))
                        .unwrap_or(false)
            })
            .map(|w| w.started.as_deref().and_then(parse_started_time))
            .collect())
    }

    /// 在 issue `issue_id` 上为 `seconds` 秒创建工时,`comment` 作为日报正文,started 为 `started`。
    ///
    /// 已在线上服务器验证:`POST /rest/tempo-timesheets/{v}/worklogs`,请求体为
    /// `{ "originTaskId": <issueId>, "worker": "<worker>",
    /// "started": "YYYY-MM-DD HH:MM:SS.000", "timeSpentSeconds": <n>, "comment": "<text>" }`。
    /// (`TimesheetWorklogBean` 不接受 `issueId`/`billable`/`locationId`/`timeSpent` — 使用 `originTaskId` 与 `timeSpentSeconds`。)
    pub fn create_worklog(
        &self,
        issue_id: u64,
        seconds: u64,
        comment: &str,
        started: &str,
        _billable: bool,
    ) -> Result<u64> {
        let url = format!(
            "{}/rest/tempo-timesheets/{}/worklogs",
            self.base_url, self.version
        );
        // 服务器期望毫秒级时间戳 "YYYY-MM-DD HH:MM:SS.000"。
        let started = if started.contains('.') {
            started.to_string()
        } else {
            format!("{started}.000")
        };
        let body = serde_json::json!({
            "originTaskId": issue_id,
            "timeSpentSeconds": seconds,
            "comment": comment,
            "started": started,
            "worker": self.worker
        });
        let resp = self
            .send_with_relogin(|| self.http.post(&url).json(&body))
            .context("creating worklog")?;
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        if !status.is_success() {
            bail!("create worklog returned {status}: {text}");
        }
        // 尽力而为:从响应中读取创建的工时 id。
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(id) = v
                .get("tempoWorklogId")
                .or_else(|| v.get("originId"))
                .and_then(|x| x.as_u64())
            {
                return Ok(id);
            }
        }
        Ok(0)
    }

    /// 快速可达性探测(就绪检查使用),预算 <=5s。
    /// 连接错误或任何非 2xx 均失败。
    pub fn reachable(&self) -> Result<()> {
        let url = format!("{}/rest/api/2/myself", self.base_url);
        let resp = self
            .send_with_relogin(|| self.http.get(&url))
            .context("connecting to Jira")?;
        let status = resp.status();
        if !status.is_success() {
            bail!("Jira returned {status} (unreachable or bad response)");
        }
        Ok(())
    }
}

impl crate::pipeline::WorklogStore for TempoClient {
    fn reachable(&self) -> Result<()> {
        TempoClient::reachable(self)
    }
    fn issue_id(&self, key: &str) -> Result<u64> {
        TempoClient::issue_id(self, key)
    }
    fn worklog_started_times(&self, id: u64, date: NaiveDate) -> Result<Vec<Option<NaiveTime>>> {
        TempoClient::worklog_started_times(self, id, date)
    }
    fn create_worklog(
        &self,
        id: u64,
        seconds: u64,
        comment: &str,
        started: &str,
        billable: bool,
    ) -> Result<u64> {
        TempoClient::create_worklog(self, id, seconds, comment, started, billable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_id_parses() {
        // /rest/api/2/issue 返回的 id 是字符串。
        let v: IssueJson = serde_json::from_str(r#"{"id":"155324","key":"BKAIZSKXM-5"}"#).unwrap();
        assert_eq!(v.id.parse::<u64>().unwrap(), 155324);
    }

    #[test]
    fn worklog_search_parses_sample_shape() {
        // 复刻用户真实响应的结构。
        let json = r#"[
            {"timeSpentSeconds":28800,"issue":{"id":155324,"key":"BKAIZSKXM-5"},"started":"2026-09-01 09:00:00.000"}
        ]"#;
        let logs: Vec<WorklogJson> = serde_json::from_str(json).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].issue.as_ref().unwrap().id, 155324);
    }

    #[test]
    fn parse_started_time_formats() {
        assert_eq!(
            parse_started_time("2026-09-01 09:00:00.000"),
            Some(NaiveTime::from_hms_opt(9, 0, 0).unwrap())
        );
        assert_eq!(
            parse_started_time("2026-09-01 18:00:00.000"),
            Some(NaiveTime::from_hms_opt(18, 0, 0).unwrap())
        );
        // 服务器发送 ".000";缺少小数部分也能解析(`%.f`)。
        assert_eq!(
            parse_started_time("2026-09-01 18:00:00"),
            Some(NaiveTime::from_hms_opt(18, 0, 0).unwrap())
        );
        assert_eq!(parse_started_time("garbage"), None);
    }
}
