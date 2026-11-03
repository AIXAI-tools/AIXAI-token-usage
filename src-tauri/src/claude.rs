// Claude 額度：呼叫 Claude Code `/usage` 背後的端點 api.anthropic.com/api/oauth/usage。
// 安全：token 只在查詢當下讀進記憶體、只送往白名單網域；
//       絕不換新 token、絕不寫入任何憑證檔；錯誤訊息與 log 不含 token。
use crate::config::home_dir;
use crate::model::{now_epoch, Reason, UsageWindow, WindowLabel};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 對外連線只允許這些網域（送出前檢查）
pub const ALLOWED_HOSTS: &[&str] = &["api.anthropic.com"];
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

pub struct ClaudeResult {
    pub email: Option<String>,
    pub windows: Result<Vec<UsageWindow>, Reason>,
}

pub fn host_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    ALLOWED_HOSTS.contains(&host)
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

pub fn fetch(dir: &Path) -> ClaudeResult {
    let email = read_email(dir);
    ClaudeResult { email, windows: fetch_windows(dir) }
}

fn fetch_windows(dir: &Path) -> Result<Vec<UsageWindow>, Reason> {
    if std::env::var_os("AITOKEN_OFFLINE").is_some() {
        return Err(Reason::new("offline"));
    }
    // token 只活在這個區塊的區域變數裡
    let (token, expires_ms) = {
        let text = std::fs::read_to_string(dir.join(".credentials.json")).map_err(|_| Reason::new("not_logged_in"))?;
        let v: Value = serde_json::from_str(&text).map_err(|_| Reason::new("not_logged_in"))?;
        let o = v.get("claudeAiOauth").ok_or_else(|| Reason::new("not_logged_in"))?;
        let token = o.get("accessToken").and_then(Value::as_str).ok_or_else(|| Reason::new("not_logged_in"))?.to_string();
        (token, o.get("expiresAt").and_then(Value::as_i64))
    };
    if expires_ms.is_some_and(|e| e / 1000 <= now_epoch()) {
        return Err(Reason::new("claude_expired"));
    }
    let body = http_get(USAGE_URL, &token)?;
    let v: Value = serde_json::from_str(&body).map_err(|_| Reason::new("no_data"))?;
    let w = parse_usage(&v);
    if w.is_empty() {
        return Err(Reason::new("no_data"));
    }
    Ok(w)
}

fn http_get(url: &str, token: &str) -> Result<String, Reason> {
    if !host_allowed(url) {
        return Err(Reason::error("blocked host"));
    }
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(15)).redirects(0).build();
    let res = agent
        .get(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("User-Agent", concat!("aixai-token-usage/", env!("CARGO_PKG_VERSION")))
        .call();
    match res {
        Ok(r) => {
            let mut s = String::new();
            r.into_reader().take(1_000_000).read_to_string(&mut s).map_err(|_| Reason::new("no_data"))?;
            Ok(s)
        }
        Err(ureq::Error::Status(401 | 403, _)) => Err(Reason::new("claude_expired")),
        Err(ureq::Error::Status(code, _)) => Err(Reason::error(format!("HTTP {code}"))),
        Err(_) => Err(Reason::error("network")),
    }
}

pub fn parse_iso(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp())
}

fn pretty_kind(kind: &str) -> String {
    let rest = kind.strip_prefix("weekly_").unwrap_or(kind).replace('_', " ");
    let mut c = rest.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// 解析 /usage 回應：優先用 limits 陣列（percent＝已用 %），沒有時退回 five_hour／seven_day。
pub fn parse_usage(v: &Value) -> Vec<UsageWindow> {
    let mut out = vec![];
    if let Some(limits) = v.get("limits").and_then(Value::as_array) {
        for l in limits {
            let Some(percent) = l.get("percent").and_then(Value::as_f64) else { continue };
            let kind = l.get("kind").and_then(Value::as_str).unwrap_or("");
            let group = l.get("group").and_then(Value::as_str).unwrap_or("");
            let resets = l.get("resets_at").and_then(Value::as_str).and_then(parse_iso);
            let (minutes, bucket) = match kind {
                "session" => (300, None),
                "weekly_all" => (10080, None),
                other => {
                    let mins = if group == "session" { 300 } else { 10080 };
                    let name = l.get("scope").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| pretty_kind(other));
                    (mins, Some(name))
                }
            };
            out.push(UsageWindow::new(WindowLabel::from_minutes(minutes, bucket), percent, resets));
        }
    }
    if out.is_empty() {
        for (key, mins) in [("five_hour", 300), ("seven_day", 10080)] {
            let Some(w) = v.get(key).filter(|w| w.is_object()) else { continue };
            let Some(used) = w.get("utilization").and_then(Value::as_f64) else { continue };
            let resets = w.get("resets_at").and_then(Value::as_str).and_then(parse_iso);
            out.push(UsageWindow::new(WindowLabel::from_minutes(mins, None), used, resets));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_limits() {
        let v = json!({"five_hour":{"utilization":87.0},"limits":[
            {"kind":"session","group":"session","percent":87,"resets_at":"2026-11-03T04:59:59.810310+00:00","scope":null},
            {"kind":"weekly_all","group":"weekly","percent":88,"resets_at":"2026-11-04T20:59:59.810327+00:00","scope":null},
            {"kind":"weekly_fable","group":"weekly","percent":12.5,"resets_at":null,"scope":null}]});
        let w = parse_usage(&v);
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].label.kind, "five_hour");
        assert_eq!(w[0].used_percent, 87.0);
        assert_eq!(w[0].resets_at, Some(1793681999));
        assert_eq!(w[1].label.kind, "weekly");
        assert!(w[1].label.bucket.is_none());
        assert_eq!(w[2].label.bucket.as_deref(), Some("Fable"));
        assert_eq!(w[2].resets_at, None);
    }

    #[test]
    fn falls_back_to_five_hour_seven_day() {
        let v = json!({"five_hour":{"utilization":10.0,"resets_at":"2026-11-03T04:59:59+00:00"},"seven_day":{"utilization":20.0,"resets_at":null},"seven_day_opus":null});
        let w = parse_usage(&v);
        assert_eq!(w.len(), 2);
        assert_eq!(w[1].used_percent, 20.0);
        assert!(parse_usage(&json!({})).is_empty());
    }

    #[test]
    fn only_anthropic_host_is_allowed() {
        assert!(host_allowed(USAGE_URL));
        assert!(!host_allowed("https://evil.example.com/api/oauth/usage"));
        assert!(!host_allowed("https://api.anthropic.com.evil.com/x"));
        assert!(!host_allowed("http://api.anthropic.com/x"));
        assert!(!host_allowed("https://api.anthropic.com@evil.com/x"));
    }

    #[test]
    fn missing_credentials_is_not_logged_in() {
        let r = fetch_windows(Path::new("C:\\no\\such\\dir")).unwrap_err();
        assert!(r.key == "not_logged_in" || r.key == "offline");
    }
}
