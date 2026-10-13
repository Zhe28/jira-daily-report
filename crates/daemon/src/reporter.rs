//! 调用 OpenAI 兼容端点,将仓库一天的提交(diff 为主要依据,commit 标题为辅助
//! 参考)转写成中文日报。

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::git_collector::RepoCommits;

/// 发送给模型的提示词载荷大小上限(字符数)。即使忙碌的日子也能保持请求较小。
pub const MAX_PROMPT_CHARS: usize = 24_000;

/// 对 AI 后端的最小抽象,使流水线可离线测试。
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

/// 真实后端:OpenAI 兼容的 `/chat/completions`。
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
        Self {
            http,
            base_url: base_url.trim_end_matches('/').into(),
            api_key: api_key.into(),
            model: model.into(),
        }
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
        let content = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .context("AI returned no choices")?;
        Ok(content.trim().to_string())
    }
}

/// 系统提示词(中文),告诉模型要产出什么。
/// `overtime` 选择下班后的变体(日常变体与原提示词逐字节一致)。
pub fn system_prompt(overtime: bool) -> &'static str {
    if overtime {
        "你是软件研发日报撰写助手。用户会提供某个仓库某一天下班后的加班时段（18:00 之后至当天结束）的 git 提交 diff（主要依据）和 commit 信息（辅助参考）。\n\
     请根据这些改动，用中文总结成简洁的工作日报条目，要求：\n\
     - 每条一行，用中文，动宾结构（如“增加xxx功能”“优化xxx性能”“修复xxx问题”）；\n\
     - 聚焦实际改动内容，不要编造 diff 中没有的工作；\n\
     - 合并重复或高度相关的条目，控制在 1 到 6 条之间；\n\
     - 只输出条目本身，不要标题、编号、引号或多余解释。"
    } else {
        "你是软件研发日报撰写助手。用户会提供某个仓库某一天（09:00-18:00）的 git 提交 diff（主要依据）和 commit 信息（辅助参考）。\n\
     请根据这些改动，用中文总结成简洁的工作日报条目，要求：\n\
     - 每条一行，用中文，动宾结构（如“增加xxx功能”“优化xxx性能”“修复xxx问题”）；\n\
     - 聚焦实际改动内容，不要编造 diff 中没有的工作；\n\
     - 合并重复或高度相关的条目，控制在 1 到 6 条之间；\n\
     - 只输出条目本身，不要标题、编号、引号或多余解释。"
    }
}

/// 尝试读取每仓库的提示词文件。文件不存在或为空时返回 None。路径相对于仓库根目录。
pub fn read_repo_prompt(repo_path: &std::path::Path, prompt_file: &str) -> Option<String> {
    let full_path = repo_path.join(prompt_file);
    match std::fs::read_to_string(&full_path) {
        Ok(content) if !content.trim().is_empty() => {
            tracing::info!("using custom prompt from {}", full_path.display());
            Some(content)
        }
        Ok(_) => {
            tracing::info!(
                "prompt file {} is empty, using default",
                full_path.display()
            );
            None
        }
        Err(e) => {
            tracing::debug!(
                "prompt file {} not found ({}), using default",
                full_path.display(),
                e
            );
            None
        }
    }
}

/// 将单个 patch 截断到字符预算内(保留开头,追加标记)。
pub fn truncate_patch(patch: &str, budget: usize) -> String {
    if patch.chars().count() <= budget {
        return patch.to_string();
    }
    let mut s: String = patch.chars().take(budget).collect();
    s.push_str("\n…（diff 过长已截断）\n");
    s
}

/// 组装用户提示词:diff(主要依据)+ commit 标题(辅助参考)。
pub fn build_user_prompt(rc: &RepoCommits) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "仓库：{}\n提交数：{}\n\n",
        rc.repo,
        rc.commits.len()
    ));
    out.push_str("===== commit 信息（辅助参考）=====\n");
    for c in &rc.commits {
        out.push_str(&format!("- {}\n", c.subject));
    }
    out.push_str("\n===== diff（主要依据）=====\n");
    for c in &rc.commits {
        out.push_str(&format!("--- commit {} ---\n", &c.sha));
        out.push_str(&truncate_patch(&c.patch, MAX_PROMPT_CHARS));
    }
    // 控制整个提示词的大小。
    if out.chars().count() > MAX_PROMPT_CHARS {
        out = truncate_patch(&out, MAX_PROMPT_CHARS);
    }
    out
}

/// 为某仓库的提交生成日报文本。
/// 若提供 `custom_prompt`,则替换默认系统提示词。
pub fn report_repo_commits(
    client: &dyn AIClient,
    rc: &RepoCommits,
    custom_prompt: Option<&str>,
    overtime: bool,
) -> Result<String> {
    if !rc.has_commits() {
        return Ok(String::new());
    }
    let user = build_user_prompt(rc);
    let system = match custom_prompt {
        Some(cp) => cp,
        None => system_prompt(overtime),
    };
    client.generate(system, &user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_collector::Commit;

    struct Mock;
    impl AIClient for Mock {
        fn generate(&self, _system: &str, user: &str) -> Result<String> {
            // 回显一个标记,证明 diff 已到达提示词。
            Ok(if user.contains("+hello") {
                "增加测试功能".into()
            } else {
                "空".into()
            })
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
        let r = report_repo_commits(&mock, &rc(), None, false).unwrap();
        assert_eq!(r, "增加测试功能");
    }

    #[test]
    fn empty_repo_yields_empty() {
        let mock = Mock;
        let empty = RepoCommits {
            repo: "r".into(),
            commits: vec![],
            error: None,
        };
        assert_eq!(report_repo_commits(&mock, &empty, None, false).unwrap(), "");
    }

    #[test]
    fn overtime_system_prompt_differs() {
        // 日常变体是原提示词(提及 09:00-18:00)。
        assert!(system_prompt(false).contains("09:00-18:00"));
        assert!(!system_prompt(false).contains("加班"));
        // 加班变体提及下班后的时段,而非 09:00-18:00。
        assert!(system_prompt(true).contains("加班"));
        assert!(!system_prompt(true).contains("09:00-18:00"));
    }

    #[test]
    fn custom_prompt_is_passed_through() {
        struct PromptCapture {
            captured_system: std::sync::Mutex<String>,
        }
        impl AIClient for PromptCapture {
            fn generate(&self, system: &str, _user: &str) -> Result<String> {
                *self.captured_system.lock().unwrap() = system.to_string();
                Ok("ok".into())
            }
        }
        let cap = PromptCapture {
            captured_system: std::sync::Mutex::new(String::new()),
        };
        report_repo_commits(&cap, &rc(), Some("my custom prompt"), false).unwrap();
        assert_eq!(*cap.captured_system.lock().unwrap(), "my custom prompt");
    }

    #[test]
    fn truncate_caps_size() {
        let big = "x".repeat(100_000);
        let t = truncate_patch(&big, 100);
        assert!(t.chars().count() < 200);
        assert!(t.contains("截断"));
    }
}
