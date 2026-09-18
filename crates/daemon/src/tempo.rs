//! Jira + Tempo REST client (cookie-based session auth via login.jsp).
//!
//! - `issue_id(issue_key)` — resolve an issue key to its numeric id (needed to
//!   create a worklog).
//! - `has_worklog_for(issue_id, date)` — whether a worklog already exists for
//!   this issue + work date (used to avoid overwriting hand-written logs).
//! - `create_worklog(...)` — write a new Tempo worklog.
//!
//! The exact search endpoint/method varies across Tempo versions; the search
//! path and method are configurable so they can be adjusted against the live
//! server without code changes.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
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
    /// Jira returns the issue id as a string, e.g. `"155324"`.
    id: String,
}

#[derive(Deserialize)]
struct WorklogJson {
    #[serde(default)]
    issue: Option<IssueRef>,
    /// e.g. "2026-09-01 09:00:00.000" — the work date.
    #[serde(default)]
    started: Option<String>,
}

#[derive(Deserialize)]
struct IssueRef {
    id: u64,
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
            search_path: search_path.trim_start_matches('/').trim_end_matches('/').to_string(),
        }
    }

    /// Execute a request; if the server returns 401, re-login and retry once.
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

    /// Authenticate via the Jira login form (cookie-based session).
    /// Must be called before any API request.
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
        // login.jsp returns 200 on success (or 302 redirect — both ok).
        // The cookie jar now holds JSESSIONID + XSRF token for subsequent requests.
        Ok(())
    }

    /// GET /rest/api/2/issue/{key} -> numeric id.
    pub fn issue_id(&self, issue_key: &str) -> Result<u64> {
        let url = format!("{}/rest/api/2/issue/{}", self.base_url, issue_key);
        let resp = self.send_with_relogin(|| self.http.get(&url))
            .with_context(|| format!("lookup issue {issue_key}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("Jira issue lookup for {issue_key} returned {status}");
        }
        let raw = resp.text().context("reading issue body")?;
        let j: IssueJson = serde_json::from_str(&raw).with_context(|| format!("parsing issue {issue_key}"))?;
        j.id.parse::<u64>()
            .with_context(|| format!("issue {issue_key} id '{}' is not numeric", j.id))
    }

    /// True if any worklog for this issue has its `started` on `date`.
    ///
    /// Verified against the live server: `POST {search_path}` with body
    /// `{"worker":["<worker>"], "from":"YYYY-MM-DD", "to":"YYYY-MM-DD"}`.
    /// `worker` is a string array; `from`/`to` are date-only (a LocalDate).
    /// The endpoint ignores issue filters, so we filter client-side by
    /// `issue.id` AND `started` date (both must match).
    pub fn has_worklog_for(&self, issue_id: u64, date: NaiveDate) -> Result<bool> {
        let day = date.format("%Y-%m-%d").to_string();
        let search = format!("{}/{}", self.base_url.trim_end_matches('/'), self.search_path.trim_start_matches('/'));
        let body = serde_json::json!({
            "worker": [self.worker],
            "from": day,
            "to": day
        });
        let resp = self.send_with_relogin(|| self.http.post(&search).json(&body))
            .with_context(|| format!("searching worklogs (POST {})", self.search_path))?;

        let status = resp.status();
        if !status.is_success() {
            bail!("worklog search returned {status}: {}", resp.text().unwrap_or_default());
        }
        let logs: Vec<WorklogJson> = resp.json().context("parsing worklog search")?;
        Ok(logs.iter().any(|w| {
            w.issue.as_ref().map(|i| i.id == issue_id).unwrap_or(false)
                && w.started
                    .as_deref()
                    .map(|s| s.starts_with(&day))
                    .unwrap_or(false)
        }))
    }

    /// Create a worklog on issue `issue_id` for `seconds`, with `comment` as the
    /// daily-report body, started at `started`.
    ///
    /// Verified against the live server: `POST /rest/tempo-timesheets/{v}/worklogs`
    /// with body `{ "originTaskId": <issueId>, "worker": "<worker>",
    /// "started": "YYYY-MM-DD HH:MM:SS.000", "timeSpentSeconds": <n>, "comment": "<text>" }`.
    /// (The bean `TimesheetWorklogBean` does NOT accept `issueId`/`billable`/
    /// `locationId`/`timeSpent` — `originTaskId` and `timeSpentSeconds` are used.)
    pub fn create_worklog(&self, issue_id: u64, seconds: u64, comment: &str, started: &str, _billable: bool) -> Result<u64> {
        let url = format!("{}/rest/tempo-timesheets/{}/worklogs", self.base_url, self.version);
        // The server expects a millisecond timestamp "YYYY-MM-DD HH:MM:SS.000".
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
        let resp = self.send_with_relogin(|| self.http.post(&url).json(&body))
            .context("creating worklog")?;
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        if !status.is_success() {
            bail!("create worklog returned {status}: {text}");
        }
        // Best-effort: read the created worklog id from the response.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(id) = v.get("tempoWorklogId").or_else(|| v.get("originId")).and_then(|x| x.as_u64()) {
                return Ok(id);
            }
        }
        Ok(0)
    }

    /// Quick reachability probe (used by the readiness check), <=5s budget.
    /// Fails on connection error or any other non-2xx.
    pub fn reachable(&self) -> Result<()> {
        let url = format!("{}/rest/api/2/myself", self.base_url);
        let resp = self.send_with_relogin(|| self.http.get(&url))
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
    fn has_worklog_for(&self, id: u64, date: NaiveDate) -> Result<bool> {
        TempoClient::has_worklog_for(self, id, date)
    }
    fn create_worklog(&self, id: u64, seconds: u64, comment: &str, started: &str, billable: bool) -> Result<u64> {
        TempoClient::create_worklog(self, id, seconds, comment, started, billable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_id_parses() {
        // /rest/api/2/issue returns id as a string.
        let v: IssueJson = serde_json::from_str(r#"{"id":"155324","key":"BKAIZSKXM-5"}"#).unwrap();
        assert_eq!(v.id.parse::<u64>().unwrap(), 155324);
    }

    #[test]
    fn worklog_search_parses_sample_shape() {
        // Mirror of the user's real response shape.
        let json = r#"[
            {"timeSpentSeconds":28800,"issue":{"id":155324,"key":"BKAIZSKXM-5"},"started":"2026-09-01 09:00:00.000"}
        ]"#;
        let logs: Vec<WorklogJson> = serde_json::from_str(json).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].issue.as_ref().unwrap().id, 155324);
    }
}
