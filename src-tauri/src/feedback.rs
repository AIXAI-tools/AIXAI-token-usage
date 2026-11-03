// 回報：依 feedback-relay 協定（002.Projects/_ProjectFactory/feedback-relay/PROTOCOL.md）送到開發者。
// 安全：
//  - 對外只連 HOSTS（https、精確網域）；Apps Script 的 302 轉址手動處理，每一跳都檢查白名單、最多 5 跳
//  - 送出內容只有使用者按下「直接送出」時才組出；診斷附件逐項可取消
//  - 診斷資訊不含 token（狀態只用平台、狀態、來源、原因 key、遮罩 email）
//  - 重試沿用同一個 id（同一個序列化後的內容），收件端 6 小時內同 id 只收一次
use crate::model::Provider;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::time::{Duration, Instant};

pub const ENDPOINT: &str = "https://script.google.com/macros/s/AKfycbwwgrA7unx2l2cNDO9h8YLX74Nu4OX1099WJAD-0OPEhAz8llSV1PS3YYESMXEXecRJ7Q/exec";
pub const TOOL: &str = "AIXAI-token-usage";
// 寫入用通行碼，設計上可公開（只用來擋隨手掃描）
const PASS: &str = "aixai-token-usage-feedback-1";
/// 回報只允許連這些網域
pub const HOSTS: &[&str] = &["script.google.com", "script.googleusercontent.com"];
const MAX_REDIRECTS: usize = 5;
const ATTEMPTS: usize = 3;
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(45);
const RETRY_DELAY: Duration = Duration::from_secs(2);
/// 解碼後附件合計上限（收件端 15 MB，留餘裕）
const MAX_FILES_BYTES: usize = 12 << 20;
pub const TITLE_MAX: usize = 100;
pub const BODY_MAX: usize = 5000;
const UA: &str = concat!("aixai-token-usage/", env!("CARGO_PKG_VERSION"));

/// 只允許 https + 白名單網域（精確比對，不接受 userinfo／連接埠）
pub fn url_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    HOSTS.contains(&host)
}

// ---------- 使用者填的回報與診斷 ----------

#[derive(Deserialize, Clone, Debug, Default)]
pub struct ReportRequest {
    /// "bug" | "idea"
    pub kind: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub contact: String,
    #[serde(default)]
    pub attach_sys: bool,
    #[serde(default)]
    pub attach_status: bool,
    #[serde(default)]
    pub attach_log: bool,
    #[serde(default)]
    pub attach_shot: bool,
    #[serde(default)]
    pub want_reply: bool,
    /// 接續的前一份回報 id
    #[serde(default)]
    pub ref_id: Option<String>,
}

/// 本機即時產生的診斷資料（已遮蔽）
#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub sys: String,
    pub status: String,
    pub log: String,
    pub shot: Option<Vec<u8>>,
}

pub fn is_report_id(s: &str) -> bool {
    s.len() == 24 && s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

pub fn new_id() -> String {
    let mut b = [0u8; 12];
    if getrandom::getrandom(&mut b).is_err() {
        // 極少見：退而用時間產生（仍是 24 位十六進位）
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        return format!("{:024x}", n & ((1u128 << 96) - 1));
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 檢查使用者輸入，錯誤回傳 key（面板對應 report.err.<key>）
pub fn validate(req: &ReportRequest) -> Result<(), String> {
    if req.title.trim().is_empty() {
        return Err("title_empty".into());
    }
    if req.title.chars().count() > TITLE_MAX {
        return Err("title_long".into());
    }
    if req.body.trim().is_empty() {
        return Err("body_empty".into());
    }
    if req.body.chars().count() > BODY_MAX {
        return Err("body_long".into());
    }
    if req.ref_id.as_deref().is_some_and(|r| !r.is_empty() && !is_report_id(r)) {
        return Err("bad_ref".into());
    }
    Ok(())
}

// ---------- 診斷文字 ----------

pub fn sys_text(app_version: &str, os: &str, lang: &str) -> String {
    format!("- App version: {app_version}\n- OS: {os}\n- UI language: {lang}\n")
}

/// 各平台查詢狀態。只用平台、狀態、來源、原因 key、遮罩 email；不含 token、帳號 key、路徑。
pub fn status_text(providers: &[Provider]) -> String {
    if providers.is_empty() {
        return "(no data yet)\n".into();
    }
    let mut out = String::new();
    for (i, p) in providers.iter().enumerate() {
        let acct = p.account_masked.as_deref().unwrap_or("-");
        let src = p.source.as_deref().unwrap_or("-");
        let reason = p.reason.as_ref().map(|r| r.key.as_str()).unwrap_or("-");
        out.push_str(&format!("- #{} {}: status={} source={} reason={} account={}\n", i + 1, p.id, p.status, src, reason, acct));
    }
    crate::applog::redact(&out)
}

fn kind_label(kind: &str) -> &'static str {
    if kind == "idea" { "建議 (idea)" } else { "問題 (bug)" }
}

/// 組出會送出的內文（預覽與送出用同一個函式）
pub fn compose_body(req: &ReportRequest, d: &Diagnostics) -> String {
    let mut s = format!("類型：{}\n\n## 說明\n{}\n", kind_label(&req.kind), req.body.trim());
    if !req.contact.trim().is_empty() {
        s.push_str(&format!("\n## 聯絡方式\n{}\n", req.contact.trim()));
    }
    if req.attach_sys && !d.sys.is_empty() {
        s.push_str(&format!("\n## 版本與系統\n{}", d.sys));
    }
    if req.attach_status && !d.status.is_empty() {
        s.push_str(&format!("\n## 各平台查詢狀態\n{}", d.status));
    }
    let mut names = vec![];
    if req.attach_shot && d.shot.is_some() {
        names.push("panel.png");
    }
    if req.attach_log && !d.log.is_empty() {
        names.push("app.log");
    }
    if !names.is_empty() {
        s.push_str(&format!("\n（附件：{}）\n", names.join("、")));
    }
    s
}

#[derive(Serialize)]
struct FileItem {
    name: String,
    data: String,
}

#[derive(Serialize)]
struct Payload<'a> {
    pass: &'a str,
    tool: &'a str,
    id: &'a str,
    title: String,
    body: String,
    files: Vec<FileItem>,
    want_reply: bool,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    ref_id: Option<&'a str>,
}

/// 組出要 POST 的 JSON 字串（id 由呼叫端決定；重試時沿用同一個字串）
pub fn build_payload(req: &ReportRequest, d: &Diagnostics, id: &str) -> Result<String, String> {
    validate(req)?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut files = vec![];
    let mut total = 0usize;
    if req.attach_shot {
        if let Some(png) = &d.shot {
            total += png.len();
            files.push(FileItem { name: "panel.png".into(), data: b64.encode(png) });
        }
    }
    if req.attach_log && !d.log.is_empty() {
        total += d.log.len();
        files.push(FileItem { name: "app.log".into(), data: b64.encode(d.log.as_bytes()) });
    }
    if total > MAX_FILES_BYTES {
        return Err("too_large".into());
    }
    let payload = Payload {
        pass: PASS,
        tool: TOOL,
        id,
        title: req.title.trim().to_string(),
        body: compose_body(req, d),
        files,
        want_reply: req.want_reply,
        ref_id: req.ref_id.as_deref().filter(|r| is_report_id(r)),
    };
    serde_json::to_string(&payload).map_err(|_| "build_failed".to_string())
}

// ---------- 送出（含重試） ----------

/// 一次嘗試的結果：(HTTP 狀態, 內文)；Err 是連線層失敗
pub type Transport<'a> = dyn FnMut(&str) -> Result<(u16, String), String> + 'a;

/// 依協定重試：連線失敗、逾時、HTTP 5xx、讀不懂的回應 → 重試（最多 attempts 次、間隔 delay、內容與 id 不變）。
/// 收件端明確拒絕（ok:false）→ 不重試，回傳錯誤碼。
pub fn send_with(transport: &mut Transport, payload: &str, attempts: usize, delay: Duration) -> Result<(), String> {
    let mut last = "network".to_string();
    for n in 0..attempts {
        if n > 0 {
            std::thread::sleep(delay);
        }
        match transport(payload) {
            Err(e) => last = e,
            Ok((code, _)) if code >= 500 => last = format!("http_{code}"),
            Ok((200, body)) => match serde_json::from_str::<serde_json::Value>(body.trim_start_matches('\u{feff}')) {
                Ok(v) if v.get("ok").and_then(|x| x.as_bool()) == Some(true) => return Ok(()),
                Ok(v) if v.get("ok").and_then(|x| x.as_bool()) == Some(false) => {
                    return Err(v.get("error").and_then(|e| e.as_str()).unwrap_or("rejected").to_string());
                }
                _ => last = "bad_response".into(),
            },
            Ok((code, _)) => return Err(format!("http_{code}")),
        }
    }
    Err(last)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).redirects(0).build()
}

fn read_limited(r: ureq::Response, max: u64) -> Result<String, String> {
    let mut s = String::new();
    r.into_reader().take(max).read_to_string(&mut s).map_err(|_| "network".to_string())?;
    Ok(s)
}

/// 跟隨轉址（改用 GET）：每一跳都檢查白名單、最多 5 跳
fn follow(mut resp: ureq::Response, deadline: Instant) -> Result<(u16, String), String> {
    let ag = agent();
    for _ in 0..=MAX_REDIRECTS {
        let code = resp.status();
        if !(300..400).contains(&code) {
            return Ok((code, read_limited(resp, 256 * 1024)?));
        }
        if code == 307 || code == 308 {
            return Err("bad_redirect".into());
        }
        let loc = resp.header("Location").ok_or("bad_redirect")?.to_string();
        if !url_allowed(&loc) {
            return Err("blocked_host".into());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("network".into());
        }
        resp = match ag.get(&loc).timeout(left).set("User-Agent", UA).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(c, r)) => {
                if (300..400).contains(&c) {
                    r
                } else {
                    return Ok((c, String::new()));
                }
            }
            Err(_) => return Err("network".into()),
        };
    }
    Err("too_many_redirects".into())
}

/// 實際對 Apps Script 發一次 POST（單次逾時 45 秒含轉址）
fn post_once(payload: &str) -> Result<(u16, String), String> {
    if !url_allowed(ENDPOINT) {
        return Err("blocked_host".into());
    }
    let deadline = Instant::now() + ATTEMPT_TIMEOUT;
    let resp = match agent().post(ENDPOINT).timeout(ATTEMPT_TIMEOUT).set("Content-Type", "application/json").set("User-Agent", UA).send_string(payload) {
        Ok(r) => r,
        Err(ureq::Error::Status(c, r)) => {
            if (300..400).contains(&c) {
                r
            } else {
                return Ok((c, String::new()));
            }
        }
        Err(_) => return Err("network".into()),
    };
    follow(resp, deadline)
}

/// GET（回覆查詢用）：逾時 15 秒，手動轉址
pub fn get_follow(url: &str) -> Result<String, String> {
    if !url_allowed(url) {
        return Err("blocked_host".into());
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let resp = match agent().get(url).timeout(Duration::from_secs(15)).set("User-Agent", UA).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(c, r)) if (300..400).contains(&c) => r,
        Err(ureq::Error::Status(c, _)) => return Err(format!("http_{c}")),
        Err(_) => return Err("network".into()),
    };
    let (code, body) = follow(resp, deadline)?;
    if code == 200 { Ok(body) } else { Err(format!("http_{code}")) }
}

/// 送出一份回報，成功回傳 id。面板的「直接送出」與測試共用這個函式。
pub fn submit(req: &ReportRequest, diag: &Diagnostics) -> Result<String, String> {
    let id = new_id();
    let payload = build_payload(req, diag, &id)?;
    send_with(&mut |p| post_once(p), &payload, ATTEMPTS, RETRY_DELAY)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Reason;

    fn req() -> ReportRequest {
        ReportRequest { kind: "bug".into(), title: "標題".into(), body: "說明".into(), attach_sys: true, attach_status: true, attach_log: true, attach_shot: true, want_reply: true, ..Default::default() }
    }

    fn diag() -> Diagnostics {
        Diagnostics { sys: sys_text("0.1.0", "Windows 11 (10.0.1)", "zh-TW"), status: "- #1 codex: status=ok\n".into(), log: "line\n".into(), shot: Some(vec![1, 2, 3]) }
    }

    #[test]
    fn host_allowlist() {
        assert!(url_allowed("https://script.google.com/macros/s/AK/exec"));
        assert!(url_allowed("https://script.googleusercontent.com/macros/echo?user_content_key=x"));
        assert!(!url_allowed("https://script.google.com.evil.com/x"));
        assert!(!url_allowed("https://evil.com/script.google.com"));
        assert!(!url_allowed("https://evil.com/?u=https://script.google.com/"));
        assert!(!url_allowed("http://script.google.com/x"));
        assert!(!url_allowed("https://script.google.com@evil.com/x"));
        assert!(!url_allowed("https://script.google.com:8443/x"));
        assert!(!url_allowed("https://google.com/x"));
        assert!(!url_allowed("https://github.com/x"));
        assert!(url_allowed(ENDPOINT));
        // 更新用的白名單不能連回報網域，反之亦然
        assert!(!crate::update::url_allowed(ENDPOINT));
        assert!(!url_allowed("https://api.github.com/x"));
    }

    #[test]
    fn id_format() {
        let a = new_id();
        assert!(is_report_id(&a));
        assert_ne!(a, new_id());
        assert!(!is_report_id("ABCDEF0123456789abcdef01"));
        assert!(!is_report_id("short"));
    }

    #[test]
    fn validation() {
        let mut r = req();
        assert!(validate(&r).is_ok());
        r.title = "  ".into();
        assert_eq!(validate(&r).unwrap_err(), "title_empty");
        r.title = "字".repeat(101);
        assert_eq!(validate(&r).unwrap_err(), "title_long");
        r.title = "字".repeat(100);
        r.body = "字".repeat(5001);
        assert_eq!(validate(&r).unwrap_err(), "body_long");
        r.body = "".into();
        assert_eq!(validate(&r).unwrap_err(), "body_empty");
        r.body = "x".into();
        r.ref_id = Some("zzz".into());
        assert_eq!(validate(&r).unwrap_err(), "bad_ref");
    }

    #[test]
    fn payload_shape_and_options() {
        let id = "0123456789abcdef01234567";
        let mut r = req();
        r.ref_id = Some("aaaaaaaaaaaaaaaaaaaaaaaa".into());
        let p = build_payload(&r, &diag(), id).unwrap();
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v["tool"], "AIXAI-token-usage");
        assert_eq!(v["pass"], "aixai-token-usage-feedback-1");
        assert_eq!(v["id"], id);
        assert_eq!(v["want_reply"], true);
        assert_eq!(v["ref"], "aaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(v["files"].as_array().unwrap().len(), 2);
        assert!(v["body"].as_str().unwrap().contains("App version: 0.1.0"));
        // 取消勾選就不放
        r.attach_sys = false;
        r.attach_log = false;
        r.attach_shot = false;
        r.ref_id = None;
        let v: serde_json::Value = serde_json::from_str(&build_payload(&r, &diag(), id).unwrap()).unwrap();
        assert!(!v["body"].as_str().unwrap().contains("App version"));
        assert_eq!(v["files"].as_array().unwrap().len(), 0);
        assert!(v.get("ref").is_none());
    }

    #[test]
    fn status_text_has_no_token_or_full_email() {
        let mut p = Provider::placeholder("claude", "ok");
        p.key = "claude:secret-dir".into();
        p.account_masked = Some("alic•••@example.com".into());
        p.account_full = Some("alice.long@example.com".into());
        p.reason = Some(Reason { key: "expired".into(), detail: Some("Bearer abc123token".into()) });
        let t = status_text(&[p]);
        assert!(t.contains("claude") && t.contains("reason=expired") && t.contains("alic•••@example.com"));
        assert!(!t.contains("alice.long") && !t.contains("abc123token") && !t.contains("secret-dir"));
    }

    #[test]
    fn retry_keeps_same_payload_and_id() {
        let id = "0123456789abcdef01234567";
        let payload = build_payload(&req(), &diag(), id).unwrap();
        let mut seen: Vec<String> = vec![];
        let mut n = 0;
        let mut t = |p: &str| -> Result<(u16, String), String> {
            seen.push(p.to_string());
            n += 1;
            match n {
                1 => Err("network".into()),
                2 => Ok((503, String::new())),
                _ => Ok((200, r#"{"ok":true}"#.into())),
            }
        };
        assert!(send_with(&mut t, &payload, 3, Duration::ZERO).is_ok());
        assert_eq!(seen.len(), 3);
        assert!(seen.iter().all(|s| s == &payload && s.contains(id)));
    }

    #[test]
    fn gives_up_after_three_and_does_not_retry_rejections() {
        let mut calls = 0;
        let mut t = |_: &str| -> Result<(u16, String), String> {
            calls += 1;
            Err("network".into())
        };
        assert_eq!(send_with(&mut t, "{}", 3, Duration::ZERO).unwrap_err(), "network");
        assert_eq!(calls, 3);
        let mut calls = 0;
        let mut t = |_: &str| -> Result<(u16, String), String> {
            calls += 1;
            Ok((200, r#"{"ok":false,"error":"daily_limit"}"#.into()))
        };
        assert_eq!(send_with(&mut t, "{}", 3, Duration::ZERO).unwrap_err(), "daily_limit");
        assert_eq!(calls, 1);
        let mut t = |_: &str| -> Result<(u16, String), String> { Ok((200, "<html>oops</html>".into())) };
        assert_eq!(send_with(&mut t, "{}", 2, Duration::ZERO).unwrap_err(), "bad_response");
    }

    /// 真實送出測試（只送一次）：cargo test real_send -- --ignored --nocapture
    /// 與面板「直接送出」呼叫同一個 submit()
    #[test]
    #[ignore]
    fn real_send_test_report() {
        let r = ReportRequest { kind: "bug".into(), title: "測試，可忽略".into(), body: "TASK-005 施工者的真實送出測試，請忽略。".into(), attach_sys: true, attach_status: true, attach_log: false, attach_shot: false, want_reply: true, ..Default::default() };
        let d = Diagnostics { sys: sys_text(env!("CARGO_PKG_VERSION"), &crate::shot::windows_version(), "zh-TW"), status: "- (test: no live data)\n".into(), log: String::new(), shot: None };
        let id = submit(&r, &d).expect("send failed");
        println!("REPORT_ID={id}");
    }
}
