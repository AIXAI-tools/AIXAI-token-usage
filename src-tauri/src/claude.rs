// Claude 額度只讀本機狀態列檔案。
use crate::config::home_dir;
use crate::model::{now_epoch, Reason, UsageWindow, WindowLabel};
use crate::statusline;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct ClaudeResult {
    pub email: Option<String>,
    pub captured_at: Option<i64>,
    pub windows: Result<Vec<UsageWindow>, Reason>,
}

pub fn default_dir() -> Option<PathBuf> {
    // 測試用：AITOKEN_CLAUDE_HOME 可把預設資料夾指到別處
    if let Some(p) = std::env::var_os("AITOKEN_CLAUDE_HOME") {
        return Some(PathBuf::from(p));
    }
    home_dir().map(|h| h.join(".claude"))
}

/// 帳號 email：預設資料夾在 ~/.claude.json，額外資料夾在 <資料夾>\.claude.json
fn read_email(dir: &Path) -> Option<String> {
    let home = home_dir();
    let is_default = home.as_ref().is_some_and(|h| h.join(".claude") == dir);
    let file = if is_default { home?.join(".claude.json") } else { dir.join(".claude.json") };
    let v: Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    v.pointer("/oauthAccount/emailAddress").and_then(Value::as_str).map(str::to_string)
}

pub fn fetch(app_dir: &Path, account_id: &str, dir: &Path) -> ClaudeResult {
    let result = std::fs::read_to_string(statusline::status_path(app_dir, account_id));
    let value = match result {
        Ok(text) => serde_json::from_str::<Value>(&text).map_err(|_| Reason::new("no_data")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Reason::new(
            if statusline::is_installed(app_dir, account_id, dir) {
                "claude_statusline_waiting"
            } else {
                "claude_statusline_missing"
            }
        )),
        Err(_) => Err(Reason::new("no_data")),
    };
    match value {
        Ok(v) => {
            let file_email = v.get("email").and_then(Value::as_str).map(str::to_string);
            let current = read_email(dir);
            // /login 換了帳號、還沒在 Claude Code 回應過：狀態檔是上一個帳號的，不能算在新帳號頭上
            if let (Some(f), Some(c)) = (&file_email, &current) {
                if !f.eq_ignore_ascii_case(c) {
                    return ClaudeResult { email: current, captured_at: None, windows: Err(Reason::new("claude_statusline_switched")) };
                }
            }
            let email = file_email.or(current);
            let captured_at = v.get("captured_at").and_then(Value::as_i64);
            let mut windows = vec![];
            for (key, minutes) in [("five_hour", 300), ("seven_day", 10080)] {
                let Some(w) = v.get(key) else { continue };
                let Some(used) = w.get("used_percentage").and_then(Value::as_f64) else { continue };
                let resets = w.get("resets_at").and_then(Value::as_i64);
                let mut window = UsageWindow::new(WindowLabel::from_minutes(minutes, None), used, resets);
                if resets.is_some_and(|r| r <= now_epoch()) {
                    window.used_percent = 0.0;
                    window.reset_done = true;
                }
                windows.push(window);
            }
            let windows = if v.get("v").and_then(Value::as_i64) != Some(1) || captured_at.is_none() || windows.is_empty() {
                Err(Reason::new("no_data"))
            } else { Ok(windows) };
            ClaudeResult { email, captured_at, windows }
        }
        Err(reason) => ClaudeResult { email: read_email(dir), captured_at: None, windows: Err(reason) },
    }
}

// Antigravity 仍使用這個時間解析函式。
pub fn parse_iso(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_file_windows_email_reset_and_missing_window() {
        let root = std::env::temp_dir().join(format!("aitoken-claude-{}", now_epoch()));
        let app = root.join("app");
        let dir = root.join("claude");
        std::fs::create_dir_all(&dir).unwrap();
        let path = statusline::status_path(&app, "claude:test");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut v = json!({"v":1,"captured_at":123,"email":"file@example.invalid",
            "five_hour":{"used_percentage":46.0,"resets_at":i64::MAX},
            "seven_day":{"used_percentage":20.0,"resets_at":1}});
        std::fs::write(&path, v.to_string()).unwrap();
        let r = fetch(&app, "claude:test", &dir);
        assert_eq!(r.email.as_deref(), Some("file@example.invalid"));
        assert_eq!(r.captured_at, Some(123));
        let w = r.windows.unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].label.kind, "five_hour");
        assert_eq!(w[0].used_percent, 46.0);
        assert_eq!(w[1].label.kind, "weekly");
        assert_eq!(w[1].used_percent, 0.0);
        assert!(w[1].reset_done);
        v.as_object_mut().unwrap().remove("five_hour");
        v["email"] = Value::Null;
        std::fs::write(dir.join(".claude.json"), json!({"oauthAccount":{"emailAddress":"fallback@example.invalid"}}).to_string()).unwrap();
        std::fs::write(&path, v.to_string()).unwrap();
        let r = fetch(&app, "claude:test", &dir);
        assert_eq!(r.windows.unwrap().len(), 1);
        assert_eq!(r.email.as_deref(), Some("fallback@example.invalid"));
    }

    #[test]
    fn missing_status_installed_waiting_otherwise_missing() {
        let root = std::env::temp_dir().join(format!("aitoken-claude-missing-{}", now_epoch()));
        let app = root.join("app");
        let dir = root.join("claude");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(fetch(&app, "claude:test", &dir).windows.unwrap_err().key, "claude_statusline_missing");
        statusline::install(&app, "claude:test", &dir).unwrap();
        assert_eq!(fetch(&app, "claude:test", &dir).windows.unwrap_err().key, "claude_statusline_waiting");
        let path = statusline::status_path(&app, "claude:test");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for text in ["broken", "{}", r#"{"v":1,"captured_at":1}"#] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(fetch(&app, "claude:test", &dir).windows.unwrap_err().key, "no_data");
        }
    }
}
