//! Call an OpenAI-compatible endpoint to turn a repo's day of commits
//! (diff = primary, commit subjects = auxiliary) into a Chinese daily report.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::git_collector::RepoCommits;

/// Cap on the prompt payload size we send to the model (chars). Keeps requests
/// small even for heavy days.
pub const MAX_PROMPT_CHARS: usize = 24_000;

/// Minimal abstraction over the AI backend so the pipeline is testable offline.
pub trait AIClient: Send + Sync {
    fn generate(&self, system: &str, user: &str) -> Result<String>;
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}
#[derive(Deserialize)]
struct Choice {
    message: Message,
}
#[derive(Deserialize)]
struct Message {
    content: String,
}

/// Real backend: OpenAI-compatible `/chat/completions`.
pub struct OpenAiClient {
    http: reqwest::blocking::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiClient {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("build http client");
        Self { http, base_url: base_url.trim_end_matches('/').into(), api_key: api_key.into(), model: model.into() }
    }
}

impl AIClient for OpenAiClient {
    fn generate(&self, system: &str, user: &str) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
        });
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .with_context(|| format!("AI request to {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            anyhow::bail!("AI endpoint returned {status}: {text}");
        }
        let parsed: ChatResponse = resp.json().context("parsing AI response")?;
        let content = parsed.choices.into_iter().next().map(|c| c.message.content).context("AI returned no choices")?;
        Ok(content.trim().to_string())
    }
}

/// System prompt (Chinese) telling the model what to produce.
pub fn system_prompt() -> &'static str {
    "你是软件研发日报撰写助手。用户会提供某个仓库某一天（09:00-18:00）的 git 提交 diff（主要依据）和 commit 信息（辅助参考）。\n\
     请根据这些改动，用中文总结成简洁的工作日报条目，要求：\n\
     - 每条一行，用中文，动宾结构（如“增加xxx功能”“优化xxx性能”“修复xxx问题”）；\n\
     - 聚焦实际改动内容，不要编造 diff 中没有的工作；\n\
     - 合并重复或高度相关的条目，控制在 1 到 6 条之间；\n\
     - 只输出条目本身，不要标题、编号、引号或多余解释。"
}

/// Truncate a single patch to a char budget (keeps the head, appends a marker).
pub fn truncate_patch(patch: &str, budget: usize) -> String {
    if patch.chars().count() <= budget {
        return patch.to_string();
    }
    let mut s: String = patch.chars().take(budget).collect();
    s.push_str("\n…（diff 过长已截断）\n");
    s
}

/// Assemble the user prompt: diff (primary) + commit subjects (aux).
pub fn build_user_prompt(rc: &RepoCommits) -> String {
    let mut out = String::new();
    out.push_str(&format!("仓库：{}\n提交数：{}\n\n", rc.repo, rc.commits.len()));
    out.push_str("===== commit 信息（辅助参考）=====\n");
    for c in &rc.commits {
        out.push_str(&format!("- {}\n", c.subject));
    }
    out.push_str("\n===== diff（主要依据）=====\n");
    for c in &rc.commits {
        out.push_str(&format!("--- commit {} ---\n", &c.sha));
        out.push_str(&truncate_patch(&c.patch, MAX_PROMPT_CHARS));
    }
    // Cap the whole prompt.
    if out.chars().count() > MAX_PROMPT_CHARS {
        out = truncate_patch(&out, MAX_PROMPT_CHARS);
    }
    out
}

/// Generate the report text for one repo's commits.
pub fn report_repo_commits(client: &dyn AIClient, rc: &RepoCommits) -> Result<String> {
    if !rc.has_commits() {
        return Ok(String::new());
    }
    let user = build_user_prompt(rc);
    client.generate(system_prompt(), &user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_collector::Commit;

    struct Mock;
    impl AIClient for Mock {
        fn generate(&self, _system: &str, user: &str) -> Result<String> {
            // Echo a marker proving the diff reached the prompt.
            Ok(if user.contains("+hello") { "增加测试功能".into() } else { "空".into() })
        }
    }

    fn rc() -> RepoCommits {
        RepoCommits {
            repo: "D:\\r".into(),
            commits: vec![Commit {
                sha: "abc".into(),
                committer_date: "2026-09-09T10:00:00+08:00".into(),
                subject: "add test".into(),
                patch: "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@\n+hello\n".into(),
                added: 1,
                removed: 0,
            }],
            error: None,
        }
    }

    #[test]
    fn prompt_contains_diff_and_subjects() {
        let p = build_user_prompt(&rc());
        assert!(p.contains("add test"));
        assert!(p.contains("+hello"));
        assert!(p.contains("仓库"));
    }

    #[test]
    fn generate_returns_report() {
        let mock = Mock;
        let r = report_repo_commits(&mock, &rc()).unwrap();
        assert_eq!(r, "增加测试功能");
    }

    #[test]
    fn empty_repo_yields_empty() {
        let mock = Mock;
        let empty = RepoCommits { repo: "r".into(), commits: vec![], error: None };
        assert_eq!(report_repo_commits(&mock, &empty).unwrap(), "");
    }

    #[test]
    fn truncate_caps_size() {
        let big = "x".repeat(100_000);
        let t = truncate_patch(&big, 100);
        assert!(t.chars().count() < 200);
        assert!(t.contains("截断"));
    }
}
