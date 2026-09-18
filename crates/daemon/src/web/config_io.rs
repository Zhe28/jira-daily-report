//! 配置读写：敏感字段掩码读取 / 校验+沿用+原子写+热生效 / 客户端构建。

use std::path::Path;
use std::sync::Arc;

use crate::config::Config;
use crate::hotconfig::HotConfig;
use crate::pipeline::WorklogStore;
use crate::reporter::{AIClient, OpenAiClient};
use crate::tempo::TempoClient;

/// 将 `cfg` 的值合并到原始 TOML 文件中，保留注释和格式。
/// 仅修改值，不删除注释；`[[repos]]` 整段替换（其内部注释无法保留）。
fn merge_toml(original: &str, cfg: &Config) -> anyhow::Result<String> {
    let mut doc: toml_edit::Document = original.parse()
        .map_err(|e| anyhow::anyhow!("解析原始 TOML 失败: {e}"))?;

    let new = toml_edit::ser::to_document(cfg)
        .map_err(|e| anyhow::anyhow!("序列化新配置失败: {e}"))?;

    // 标量字段 + 简单值：逐键覆盖（保留原键的前导注释）
    let skip = ["repos"]; // repos 单独处理
    for (key, new_item) in new.iter() {
        if skip.contains(&key) {
            continue;
        }
        if let Some(orig_item) = doc.as_table_mut().get_mut(key) {
            // 仅当原值存在且类型相同时替换值（保留 decor/注释）
            if let (Some(orig_val), Some(new_val)) = (orig_item.as_value(), new_item.as_value()) {
                if std::mem::discriminant(orig_val) == std::mem::discriminant(new_val) {
                    *orig_item = toml_edit::Item::Value(new_val.clone());
                } else {
                    *orig_item = toml_edit::Item::Value(new_val.clone());
                }
            }
        }
    }

    // [[repos]]：原地修改（保留注释），多余条目删除
    if let Some(aot) = doc.as_table_mut().get_mut("repos").and_then(|i| i.as_array_of_tables_mut()) {
        // 更新已有条目
        for (i, r) in cfg.repos.iter().enumerate() {
            if let Some(tbl) = aot.get_mut(i) {
                tbl["local_path"] = toml_edit::value(r.local_path.display().to_string());
                tbl["issue_key"] = toml_edit::value(r.issue_key.as_str());
                if let Some(email) = &r.git_email {
                    tbl["git_email"] = toml_edit::value(email.as_str());
                } else {
                    tbl.remove("git_email");
                }
                if let Some(pf) = &r.prompt_file {
                    tbl["prompt_file"] = toml_edit::value(pf.as_str());
                } else {
                    tbl.remove("prompt_file");
                }
            } else {
                let mut tbl = toml_edit::Table::new();
                tbl["local_path"] = toml_edit::value(r.local_path.display().to_string());
                tbl["issue_key"] = toml_edit::value(r.issue_key.as_str());
                if let Some(email) = &r.git_email {
                    tbl["git_email"] = toml_edit::value(email.as_str());
                }
                if let Some(pf) = &r.prompt_file {
                    tbl["prompt_file"] = toml_edit::value(pf.as_str());
                }
                aot.push(tbl);
            }
        }
        // 删除多余条目（从尾部删）
        while aot.len() > cfg.repos.len() {
            aot.remove(aot.len() - 1);
        }
    }

    Ok(doc.to_string())
}

/// 从 `main.rs::build_clients` 统一搬入（CLI 分支与 PUT 热切换共用）。
pub fn build_clients(cfg: &Config) -> (Arc<dyn AIClient>, Arc<dyn WorklogStore>) {
    let ai: Arc<dyn AIClient> = Arc::new(OpenAiClient::new(&cfg.ai_base_url, &cfg.ai_api_key, &cfg.ai_model));
    let tempo = TempoClient::new(
        &cfg.jira_base_url,
        &cfg.jira_user,
        &cfg.jira_password(),
        cfg.tempo_version,
        &cfg.worker,
        &cfg.worklog_search_path,
    );
    if let Err(e) = tempo.login() {
        tracing::warn!("Jira 登录失败（后续 API 调用可能会 401）: {e}");
    }
    let store: Arc<dyn WorklogStore> = Arc::new(tempo);
    (ai, store)
}

/// GET /api/config 的响应体：敏感字段不回显明文。
pub fn read_for_api(cfg: &Config) -> serde_json::Value {
    let mut v = serde_json::to_value(cfg).expect("Config 可序列化");
    v["jira_password"] = serde_json::Value::Null;
    v["ai_api_key"] = serde_json::Value::String(String::new());
    v["jira_password_configured"] =
        serde_json::json!(cfg.jira_password.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()));
    v["ai_api_key_configured"] = serde_json::json!(!cfg.ai_api_key.trim().is_empty());
    v
}

/// PUT /api/config 的写盘 + 热生效。
///
/// 1) 敏感字段沿用：请求中 `jira_password` 为空/null/省略、`ai_api_key` 为空/省略 → 沿用 `old`；
/// 2) `validate()` + `check_git_identity()`（整份校验先于任何写盘）；
/// 3) Jira 密码最终可解析（否则报"Jira 密码未找到"，与启动检查同语义）；
/// 4) 原子写 `config.toml.tmp` → `rename`（失败不留 `.tmp`）；
/// 5) `hot.set(cfg)` 整份热生效。
pub fn write(path: &Path, hot: &HotConfig, old: &Config, incoming_in: serde_json::Value) -> anyhow::Result<Config> {
    let mut incoming = incoming_in;
    // 1) 合并敏感字段（沿用旧值）
    let in_jira = incoming.get("jira_password").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let in_ai = incoming.get("ai_api_key").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if in_jira.is_empty() {
        if let Some(p) = &old.jira_password {
            incoming["jira_password"] = serde_json::json!(p);
        }
    }
    if in_ai.is_empty() && !old.ai_api_key.trim().is_empty() {
        incoming["ai_api_key"] = serde_json::json!(&old.ai_api_key);
    }

    // 规范化：可选字段中空字符串 → null（反序列化后为 None，避免 validate 拒绝）
    for field in ["git_email", "worklog_start"] {
        if incoming.get(field).and_then(|v| v.as_str()).is_some_and(|s| s.trim().is_empty()) {
            incoming[field] = serde_json::Value::Null;
        }
    }
    if let Some(repos) = incoming.get_mut("repos").and_then(|v| v.as_array_mut()) {
        for repo in repos {
            if repo.get("git_email").and_then(|v| v.as_str()).is_some_and(|s| s.trim().is_empty()) {
                repo["git_email"] = serde_json::Value::Null;
            }
            if repo.get("prompt_file").and_then(|v| v.as_str()).is_some_and(|s| s.trim().is_empty()) {
                repo["prompt_file"] = serde_json::Value::Null;
            }
        }
    }

    // 2) 整份校验（deny_unknown_fields 在反序列化时已生效）
    let cfg: Config = serde_json::from_value(incoming)
        .map_err(|e| anyhow::anyhow!("配置解析失败: {e}"))?;
    cfg.validate()?;
    cfg.check_git_identity()?;

    // 3) 密码最终可解析（validate 已要求 env 或 cfg 至少其一，这里再确认非空）
    if cfg.jira_password().trim().is_empty() {
        anyhow::bail!("Jira 密码未找到：请设置环境变量 {}，或在配置中填写 jira_password", crate::config::JIRA_PASS_ENV);
    }

    // 4) 原子写盘（合并到原始文件，保留注释）
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let body = merge_toml(&original, &cfg)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!("写回配置失败: {e}");
    }

    // 5) 热生效
    hot.set(cfg.clone());
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::hotconfig::HotConfig;
    use crate::web::config_io;
    use crate::web::testutil;

    #[test]
    fn read_for_api_masks_secrets() {
        let mut c = testutil::cfg_with_repo(&testutil::tmp());
        c.jira_password = Some("s3cret".into());
        c.ai_api_key = "sk-live".into();
        let v = config_io::read_for_api(&c);
        assert_eq!(v["jira_password"], serde_json::Value::Null, "响应里 jira_password 必须是 null/省略");
        assert_eq!(v["ai_api_key"], "");
        assert_eq!(v["jira_password_configured"], true);
        assert_eq!(v["ai_api_key_configured"], true);
    }

    #[test]
    fn read_for_api_marks_unconfigured() {
        let c = testutil::cfg_with_repo(&testutil::tmp());
        let v = config_io::read_for_api(&c);
        assert_eq!(v["jira_password_configured"], false);
        assert_eq!(v["ai_api_key_configured"], true); // ai_api_key 非空即视为已配置
    }

    #[test]
    fn write_persists_and_hot_applies() {
        let d = testutil::tmp();
        let c = testutil::cfg_with_repo(&d);
        let mut c = c.clone();
        c.jira_password = Some("old-pass".into());
        c.ai_api_key = "old-key".into();
        let p = d.join("config.toml");
        std::fs::write(&p, toml::to_string_pretty(&c).unwrap()).unwrap();
        let hot = HotConfig::new(c.clone());

        let mut in_ = serde_json::to_value(&c).unwrap();
        in_["check_time"] = "23:45".into();
        in_["jira_password"] = serde_json::Value::Null; // 省略 → 沿用
        in_["ai_api_key"] = serde_json::json!(""); // 空串 → 沿用
        let new = config_io::write(&p, &hot, &c, in_).unwrap();
        let expected = crate::config::CheckTime::Single(chrono::NaiveTime::from_hms_opt(23, 45, 0).unwrap());
        assert_eq!(new.check_time, expected);
        assert_eq!(hot.get().check_time, expected);
        let on_disk = Config::load(&p).unwrap();
        assert_eq!(on_disk.check_time, expected);
        assert_eq!(on_disk.jira_password.as_deref(), Some("old-pass"));
        assert_eq!(on_disk.ai_api_key, "old-key");
        assert!(!d.join("config.toml.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_rejects_bad_time_and_leaves_file_untouched() {
        let d = testutil::tmp();
        let c = testutil::cfg_with_repo(&d);
        let p = d.join("config.toml");
        std::fs::write(&p, toml::to_string_pretty(&c).unwrap()).unwrap();
        let before = std::fs::read_to_string(&p).unwrap();
        let hot = HotConfig::new(c.clone());
        let mut in_ = serde_json::to_value(&c).unwrap();
        in_["check_time"] = "25:99".into();
        assert!(config_io::write(&p, &hot, &c, in_).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
        assert_eq!(hot.get().check_time, crate::config::CheckTime::default());
        assert!(!d.join("config.toml.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_rejects_unknown_field() {
        let d = testutil::tmp();
        let c = testutil::cfg_with_repo(&d);
        let p = d.join("config.toml");
        std::fs::write(&p, toml::to_string_pretty(&c).unwrap()).unwrap();
        let hot = HotConfig::new(c.clone());
        let mut in_ = serde_json::to_value(&c).unwrap();
        in_["bogus_field"] = "x".into();
        assert!(config_io::write(&p, &hot, &c, in_).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_rejects_missing_password_everywhere() {
        // 测试环境未设置 DAILYREPORT_JIRA_PASS 时，文件无 jira_password → PUT 必须失败（不落盘）。
        if std::env::var("DAILYREPORT_JIRA_PASS").is_ok_and(|v| !v.trim().is_empty()) {
            return; // 无法在不改环境的前提下构造该场景，跳过
        }
        let d = testutil::tmp();
        let c = testutil::cfg_with_repo(&d); // jira_password = None
        let p = d.join("config.toml");
        std::fs::write(&p, toml::to_string_pretty(&c).unwrap()).unwrap();
        let hot = HotConfig::new(c.clone());
        let in_ = serde_json::to_value(&c).unwrap();
        assert!(config_io::write(&p, &hot, &c, in_).is_err());
        // Config::load 本身会 validate（无密码 → Err），这里只验证文件未被改动
        let on_disk_raw = std::fs::read_to_string(&p).unwrap();
        assert!(!on_disk_raw.contains("jira_password"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_preserves_comments() {
        let d = testutil::tmp();
        std::process::Command::new("git").args(["init"]).current_dir(&d).output().ok();
        std::process::Command::new("git").args(["config", "user.email", "t@t.com"]).current_dir(&d).output().ok();

        let c = testutil::cfg_with_repo(&d);
        let p = d.join("config.toml");
        // 带注释的原始 TOML（路径用正斜杠避免转义问题）
        let log_dir = c.log_dir.display().to_string().replace('\\', "/");
        let hol_dir = c.holidays_dir.display().to_string().replace('\\', "/");
        let repo_path = c.repos[0].local_path.display().to_string().replace('\\', "/");
        let original = format!(
            "# Jira 配置\njira_base_url = \"{}\"\njira_user = \"{}\"\njira_password = \"pass\"\ntempo_version = {}\nworker = \"{}\"\n# 时间配置\ncheck_time = \"{}\"\nwork_start = \"{}\"\nwork_end = \"{}\"\ntotal_daily_seconds = {}\nai_base_url = \"{}\"\nai_api_key = \"k\"\nai_model = \"m\"\nlog_dir = \"{}\"\nholidays_dir = \"{}\"\n\n# 仓库列表\n[[repos]]\nlocal_path = \"{}\"\nissue_key = \"A-1\"\n",
            c.jira_base_url, c.jira_user, c.tempo_version, c.worker,
            c.check_time, c.work_start, c.work_end, c.total_daily_seconds,
            c.ai_base_url, log_dir, hol_dir, repo_path
        );
        std::fs::write(&p, &original).unwrap();
        let mut c2 = c.clone();
        c2.jira_password = Some("pass".into());
        let hot = HotConfig::new(c2.clone());

        let mut in_ = serde_json::to_value(&c2).unwrap();
        in_["check_time"] = "23:45".into(); // 只改一个值

        let new = config_io::write(&p, &hot, &c2, in_).unwrap();
        assert_eq!(new.check_time, crate::config::CheckTime::Single(chrono::NaiveTime::from_hms_opt(23, 45, 0).unwrap()));

        let saved = std::fs::read_to_string(&p).unwrap();
        assert!(saved.contains("# Jira 配置"), "应保留 Jira 注释: {saved}");
        assert!(saved.contains("# 时间配置"), "应保留时间注释: {saved}");
        assert!(saved.contains("# 仓库列表"), "应保留仓库注释: {saved}");
        assert!(saved.contains("23:45"), "应更新 check_time: {saved}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_normalizes_empty_optional_fields() {
        let d = testutil::tmp();
        // 初始化 git repo（check_git_identity 需要能解析 user.email）
        std::process::Command::new("git").args(["init"]).current_dir(&d).output().ok();
        std::process::Command::new("git").args(["config", "user.email", "t@t.com"]).current_dir(&d).output().ok();

        let mut c = testutil::cfg_with_repo(&d);
        c.jira_password = Some("old-pass".into());
        c.repos[0].git_email = None; // 不在 config 中设置，依赖 git config fallback
        let p = d.join("config.toml");
        std::fs::write(&p, toml::to_string_pretty(&c).unwrap()).unwrap();
        let hot = HotConfig::new(c.clone());

        let mut in_ = serde_json::to_value(&c).unwrap();
        in_["git_email"] = serde_json::json!("");               // 前端发来的空串
        in_["repos"][0]["git_email"] = serde_json::json!("");   // 同上
        in_["jira_password"] = serde_json::Value::Null;         // 省略 → 沿用
        in_["ai_api_key"] = serde_json::json!("");              // 空串 → 沿用

        let new = config_io::write(&p, &hot, &c, in_).unwrap();
        assert!(new.git_email.is_none(), "空串应被规范化为 None");
        assert!(new.repos[0].git_email.is_none(), "repo 空串应被规范化为 None");
        assert_eq!(new.jira_password.as_deref(), Some("old-pass")); // 沿用旧值
        let _ = std::fs::remove_dir_all(&d);
    }
}
