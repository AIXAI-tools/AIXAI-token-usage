// Codex 額度：透過官方 `codex app-server`（stdio JSON-RPC）查詢。
// 安全：本程式不讀 Codex 的任何憑證檔，登入與連線全部由 codex 自己處理；
//       只允許呼叫白名單內的唯讀方法。
use crate::model::{mask_email, now_epoch, Provider, Reason, UsageWindow, WindowLabel};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// 只准呼叫這些方法。協定裡另有 logout、消耗重置額度等會改變狀態的方法，一律禁止。
const ALLOWED_METHODS: &[&str] = &["initialize", "initialized", "account/read", "account/rateLimits/read"];
const TIMEOUT: Duration = Duration::from_secs(20);

/// dir＝None 用預設的 ~/.codex；額外帳號傳入 CODEX_HOME 資料夾
pub fn fetch(dir: Option<&Path>) -> Provider {
    match fetch_live(dir) {
        Ok(p) => p,
        Err(reason) => {
            // 未登入／API 金鑰模式時，本機紀錄沒有意義，直接回報
            if reason.key != "not_logged_in" && reason.key != "api_key_mode" {
                if let Some(p) = fetch_local_log(dir) {
                    return p;
                }
            }
            let mut p = Provider::placeholder("codex", "unavailable");
            p.reason = Some(reason);
            p
        }
    }
}

// ---------- 找 codex 執行檔 ----------

fn find_codex() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AITOKEN_CODEX_PATH") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join("codex.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    // npm 全域安裝：codex.cmd 只是殼，直接用裡面的原生執行檔（不會閃出主控台視窗）
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let base = PathBuf::from(appdata).join(r"npm\node_modules\@openai\codex\node_modules\@openai");
        for rel in [
            r"codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe",
            r"codex-win32-arm64\vendor\aarch64-pc-windows-msvc\bin\codex.exe",
        ] {
            let p = base.join(rel);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

// ---------- JSON-RPC ----------

struct Rpc {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<Value>,
    deadline: Instant,
}

impl Rpc {
    fn start(exe: &PathBuf, codex_home: Option<&Path>) -> Result<Self, Reason> {
        let mut cmd = Command::new(exe);
        if let Some(h) = codex_home {
            cmd.env("CODEX_HOME", h);
        }
        cmd.arg("app-server").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| Reason::error(e.to_string()))?;
        let stdin = child.stdin.take().ok_or_else(|| Reason::error("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| Reason::error("stdout"))?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        Ok(Self { child, stdin, rx, deadline: Instant::now() + TIMEOUT })
    }

    fn send(&mut self, id: Option<u64>, method: &str, params: Value) -> Result<(), Reason> {
        if !ALLOWED_METHODS.contains(&method) {
            return Err(Reason::error(format!("blocked method {method}")));
        }
        let mut msg = json!({ "method": method, "params": params });
        if let Some(id) = id {
            msg["id"] = json!(id);
        }
        writeln!(self.stdin, "{msg}").and_then(|_| self.stdin.flush()).map_err(|e| Reason::error(e.to_string()))
    }

    fn call(&mut self, id: u64, method: &str, params: Value) -> Result<Value, Reason> {
        self.send(Some(id), method, params)?;
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Reason::new("timeout"));
            }
            match self.rx.recv_timeout(left) {
                Ok(v) if v.get("id").and_then(Value::as_u64) == Some(id) => {
                    if let Some(err) = v.get("error") {
                        let msg = err.get("message").and_then(Value::as_str).unwrap_or("unknown");
                        return Err(Reason::error(msg.to_string()));
                    }
                    return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                }
                Ok(_) => continue, // 通知或其他回應，略過
                Err(mpsc::RecvTimeoutError::Timeout) => return Err(Reason::new("timeout")),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Reason::error("codex app-server exited")),
            }
        }
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fetch_live(dir: Option<&Path>) -> Result<Provider, Reason> {
    // 額外帳號：只檢查登入檔「是否存在」，不讀內容
    if dir.is_some_and(|d| !d.join("auth.json").is_file()) {
        return Err(Reason::new("not_logged_in"));
    }
    let exe = find_codex().ok_or_else(|| Reason::new("codex_not_found"))?;
    let mut rpc = Rpc::start(&exe, dir)?;
    rpc.call(1, "initialize", json!({ "clientInfo": { "name": "aixai-token-usage", "version": env!("CARGO_PKG_VERSION") } }))?;
    rpc.send(None, "initialized", Value::Null)?;

    let acct = rpc.call(2, "account/read", json!({ "refreshToken": false }))?;
    let account = acct.get("account").cloned().unwrap_or(Value::Null);
    let email = match account.get("type").and_then(Value::as_str) {
        None => return Err(Reason::new("not_logged_in")),
        Some("apiKey") => return Err(Reason::new("api_key_mode")),
        Some(_) => account.get("email").and_then(Value::as_str).map(str::to_string),
    };

    let limits = rpc.call(3, "account/rateLimits/read", Value::Null)?;
    let windows = parse_live_limits(&limits);
    if windows.is_empty() {
        return Err(Reason::new("no_data"));
    }
    let mut p = Provider::placeholder("codex", "ok");
    p.source = Some("live".into());
    p.as_of = Some(now_epoch());
    p.account_masked = email.as_deref().map(mask_email);
    p.account_full = email;
    p.windows = windows;
    Ok(p)
}

/// 解析 app-server 回應（camelCase）。優先用多額度桶 rateLimitsByLimitId。
pub fn parse_live_limits(v: &Value) -> Vec<UsageWindow> {
    let mut buckets: Vec<&Value> = match v.get("rateLimitsByLimitId").and_then(Value::as_object) {
        Some(m) if !m.is_empty() => m.values().collect(),
        _ => v.get("rateLimits").into_iter().collect(),
    };
    // 主要桶 codex 排最前面
    buckets.sort_by_key(|b| b.get("limitId").and_then(Value::as_str) != Some("codex"));
    let mut out = vec![];
    for b in buckets {
        let id = b.get("limitId").and_then(Value::as_str).unwrap_or("codex");
        let bucket = if id == "codex" {
            None
        } else {
            Some(b.get("limitName").and_then(Value::as_str).unwrap_or(id).to_string())
        };
        for key in ["primary", "secondary"] {
            let w = match b.get(key) {
                Some(w) if w.is_object() => w,
                _ => continue,
            };
            let (Some(used), Some(mins)) = (
                w.get("usedPercent").and_then(Value::as_f64),
                w.get("windowDurationMins").and_then(Value::as_u64),
            ) else {
                continue;
            };
            out.push(UsageWindow::new(
                WindowLabel::from_minutes(mins, bucket.clone()),
                used,
                w.get("resetsAt").and_then(Value::as_i64),
            ));
        }
    }
    out
}

// ---------- 備援：讀本機對話紀錄最後一筆 rate_limits（只取額度欄位） ----------

fn fetch_local_log(dir: Option<&Path>) -> Option<Provider> {
    let base = match dir {
        Some(d) => d.to_path_buf(),
        None => PathBuf::from(std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?).join(".codex"),
    };
    let sessions = base.join("sessions");
    let file = newest_rollout(&sessions)?;
    let modified = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let text = std::fs::read_to_string(&file).ok()?;
    let line = text.lines().rev().find(|l| l.contains("\"rate_limits\""))?;
    let v: Value = serde_json::from_str(line).ok()?;
    let rl = find_key(&v, "rate_limits")?;
    let windows = parse_log_limits(rl);
    if windows.is_empty() {
        return None;
    }
    let mut p = Provider::placeholder("codex", "ok");
    p.source = Some("local_log".into());
    p.as_of = modified;
    p.windows = windows;
    Some(p)
}

/// sessions/YYYY/MM/DD/rollout-*.jsonl：依名稱倒序找最新一個檔案
fn newest_rollout(dir: &PathBuf) -> Option<PathBuf> {
    fn sorted_desc(dir: &PathBuf) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir).ok().into_iter().flatten().flatten().map(|e| e.path()).collect();
        v.sort();
        v.reverse();
        v
    }
    for y in sorted_desc(dir).into_iter().filter(|p| p.is_dir()) {
        for m in sorted_desc(&y).into_iter().filter(|p| p.is_dir()) {
            for d in sorted_desc(&m).into_iter().filter(|p| p.is_dir()) {
                if let Some(f) = sorted_desc(&d).into_iter().find(|p| {
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
                }) {
                    return Some(f);
                }
            }
        }
    }
    None
}

fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(key).or_else(|| m.values().find_map(|x| find_key(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

/// 解析本機紀錄（snake_case）
pub fn parse_log_limits(rl: &Value) -> Vec<UsageWindow> {
    let mut out = vec![];
    for key in ["primary", "secondary"] {
        let Some(w) = rl.get(key).filter(|w| w.is_object()) else { continue };
        let (Some(used), Some(mins)) = (
            w.get("used_percent").and_then(Value::as_f64),
            w.get("window_minutes").and_then(Value::as_u64),
        ) else {
            continue;
        };
        out.push(UsageWindow::new(
            WindowLabel::from_minutes(mins, None),
            used,
            w.get("resets_at").and_then(Value::as_i64),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_live_buckets_and_skips_empty() {
        let v = json!({
            "rateLimitsByLimitId": {
                "premium": { "limitId": "premium", "limitName": null, "primary": null, "secondary": null },
                "codex": { "limitId": "codex", "limitName": null,
                    "primary": { "usedPercent": 6, "windowDurationMins": 300, "resetsAt": 1793686963 },
                    "secondary": { "usedPercent": 93, "windowDurationMins": 10080, "resetsAt": 1793895360 } }
            }
        });
        let w = parse_live_limits(&v);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].label.kind, "five_hour");
        assert_eq!(w[1].label.kind, "weekly");
        assert_eq!(w[1].used_percent, 93.0);
        assert!(w[0].label.bucket.is_none());
    }

    #[test]
    fn non_main_bucket_gets_name() {
        let v = json!({ "rateLimitsByLimitId": { "x": { "limitId": "gpt-pro", "limitName": "Pro 模型",
            "primary": { "usedPercent": 10, "windowDurationMins": 300, "resetsAt": null } } } });
        let w = parse_live_limits(&v);
        assert_eq!(w[0].label.bucket.as_deref(), Some("Pro 模型"));
    }

    #[test]
    fn parses_log_line() {
        let line = r#"{"timestamp":"x","payload":{"type":"token_count","rate_limits":{"limit_id":"codex","primary":{"used_percent":6.0,"window_minutes":300,"resets_at":1793686963},"secondary":{"used_percent":93.0,"window_minutes":10080,"resets_at":1793895360}}}}"#;
        let v: Value = serde_json::from_str(line).unwrap();
        let w = parse_log_limits(find_key(&v, "rate_limits").unwrap());
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].used_percent, 6.0);
    }

    #[test]
    fn blocks_methods_outside_whitelist() {
        assert!(!ALLOWED_METHODS.contains(&"account/logout"));
        assert!(!ALLOWED_METHODS.contains(&"account/rateLimitResetCredit/consume"));
    }
}
