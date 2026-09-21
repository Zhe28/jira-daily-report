//! Alerting: always print to the CLI; additionally, best-effort, raise a
//! Windows system toast. The toast never blocks the pipeline and silently
//! degrades to CLI-only if it's unavailable (no Windows / no PowerShell /
//! user dismissed the consent prompt).

/// Fire an alert: CLI + (Windows) system toast.
pub fn notify(title: &str, body: &str) {
    tracing::warn!("[ALERT] {title}: {body}");
    eprintln!("\n[ALERT] {title}: {body}\n");

    #[cfg(windows)]
    send_toast(title, body);
}

/// Informational hint: CLI (info) + best-effort Windows toast.
pub fn info(title: &str, body: &str) {
    tracing::info!("[提示] {title}: {body}");
    println!("\n[提示] {title}: {body}\n");

    #[cfg(windows)]
    send_toast(title, body);
}

/// Toast 使用的 AppID（须在 HKCU 注册表中注册过）。
#[cfg(windows)]
const TOAST_APP_ID: &str = "DailyReport";

#[cfg(windows)]
fn send_toast(title: &str, body: &str) {
    use std::process::{Command, Stdio};
    use std::sync::Once;
    use std::thread;
    use std::time::Duration;
    use wait_timeout::ChildExt;

    // 只注册一次 AppID（写入 HKCU，不需要管理员权限）。
    static REGISTER_APPID: Once = Once::new();
    REGISTER_APPID.call_once(|| {
        let _ = Command::new("reg")
            .args([
                "add",
                r"HKCU\Software\Classes\AppUserModelId\DailyReport",
                "/v", "DisplayName",
                "/t", "REG_SZ",
                "/d", "Daily Report",
                "/f",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });

    let script = build_toast_script(title, body);
    let encoded = encode_powershell_command(&script);

    let mut child = match Command::new("powershell")
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-EncodedCommand")
        .arg(encoded)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    // 最多等 3s，避免异常挂起。
    let _ = thread::spawn(move || {
        let _ = child.wait_timeout(Duration::from_secs(3));
        let _ = child.kill();
    });
}

/// 生成 PowerShell 脚本。不依赖 Windows，方便单测。
#[allow(dead_code)]
fn build_toast_script(title: &str, body: &str) -> String {
    let t = xml_escape(title);
    let b = xml_escape(body);

    // 注意：ToastText02 模板的 text 用 id="1"/id="2"。
    let xml = format!(
        r#"<toast><visual><binding template="ToastText02"><text id="1">{t}</text><text id="2">{b}</text></binding></visual></toast>"#
    );

    // 注意这里的 {{ }} 是给 Rust format! 转义用的。
    // catch 里把错误写到 %TEMP%\daily_report_toast.log，方便排查。
    format!(
        r#"$ErrorActionPreference = 'Stop'
try {{
  [void][Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
  [void][Windows.UI.Notifications.ToastNotification, Windows.UI.Notifications, ContentType = WindowsRuntime]
  [void][Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime]

  $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
  $xml.LoadXml('{xml}')

  $toast = New-Object Windows.UI.Notifications.ToastNotification $xml
  [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{app_id}').Show($toast)
}} catch {{
  $msg = "[" + (Get-Date -Format o) + "] " + $_.Exception.ToString() + "`r`n"
  Add-Content -Path (Join-Path $env:TEMP 'daily_report_toast.log') -Value $msg
}}"#,
        app_id = TOAST_APP_ID,
    )
}

/// XML 文本转义。注意先转义 & 再转义其他。
#[allow(dead_code)]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 把脚本编码成 UTF-16LE + Base64，给 -EncodedCommand 用。
#[allow(dead_code)]
fn encode_powershell_command(script: &str) -> String {
    let utf16: Vec<u16> = script.encode_utf16().collect();
    let mut bytes = Vec::with_capacity(utf16.len() * 2);
    for u in utf16 {
        bytes.push((u & 0xff) as u8);
        bytes.push((u >> 8) as u8);
    }

    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);

    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);

        out.push(CHARS[((n >> 18) & 63) as usize] as char);
        out.push(CHARS[((n >> 12) & 63) as usize] as char);

        if chunk.len() > 1 {
            out.push(CHARS[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_prints_without_panic() {
        notify("test", "body line");
    }

    #[test]
    fn info_prints_without_panic() {
        info("下次写日志", "2026-09-14 13:00:00 (约 3600 秒后)");
    }

    #[test]
    fn xml_escape_escapes_special_chars() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
    }

    #[test]
    fn build_toast_script_uses_correct_xml() {
        let s = build_toast_script("标题 & <x>", "正文 \"引号\" '单引号'");
        // 属性应为单对引号
        assert!(s.contains(r#"template="ToastText02""#));
        // 用 id 而不是 ref
        assert!(s.contains(r#"id="1""#));
        assert!(s.contains(r#"id="2""#));
        assert!(!s.contains("ref="));
        // 特殊字符转义
        assert!(s.contains("标题 &amp; &lt;x&gt;"));
        assert!(s.contains("正文 &quot;引号&quot; &apos;单引号&apos;"));
        // AppID 正确
        assert!(s.contains(&format!("CreateToastNotifier('{}')", TOAST_APP_ID)));
    }

    #[test]
    fn encode_powershell_is_base64ish() {
        let encoded = encode_powershell_command("Write-Host 'hi'");
        assert!(!encoded.is_empty());
        assert!(encoded
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='));
    }
}