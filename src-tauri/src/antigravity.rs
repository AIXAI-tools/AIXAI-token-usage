// Antigravity 額度：官方 `agy -p "/usage" --output-format json`（不啟動對話、不耗額度）。
// 安全：每次回應都檢查 usage.total_tokens 與 num_turns 必須是 0，否則視為異常並停止查詢。
use crate::claude::parse_iso;
use crate::config::home_dir;
use crate::model::{Reason, UsageWindow, WindowLabel};
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(30);
/// 發現異常（花了 token 或進了對話）後，本次執行不再查詢
static STOPPED: AtomicBool = AtomicBool::new(false);

pub struct AgyResult {
    pub email: Option<String>,
    pub windows: Result<Vec<UsageWindow>, Reason>,
}

pub fn fetch() -> AgyResult {
    let email = active_email();
    AgyResult { email, windows: fetch_windows() }
}

/// 目前登入的帳號：只讀 ~/.gemini/google_accounts.json 的 active
pub fn active_email() -> Option<String> {
    let f = home_dir()?.join(".gemini").join("google_accounts.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(f).ok()?).ok()?;
    v.get("active").and_then(Value::as_str).filter(|s| s.contains('@')).map(str::to_string)
}

fn find_agy() -> Option<PathBuf> {
    // 測試用：AITOKEN_AGY_PATH 設了就只用它（指到不存在的檔案＝模擬找不到）
    if let Some(p) = std::env::var_os("AITOKEN_AGY_PATH") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join("agy.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let local = std::env::var_os("LOCALAPPDATA")?;
    let p = PathBuf::from(local).join("agy").join("bin").join("agy.exe");
    p.is_file().then_some(p)
}

fn fetch_windows() -> Result<Vec<UsageWindow>, Reason> {
    if STOPPED.load(Ordering::SeqCst) {
        return Err(Reason::new("agy_abnormal"));
    }
    if std::env::var_os("AITOKEN_OFFLINE").is_some() {
        return Err(Reason::new("offline"));
    }
    let exe = find_agy().ok_or_else(|| Reason::new("agy_not_found"))?;
    let out = run_usage(&exe)?;
    let v: Value = serde_json::from_str(&out).map_err(|_| Reason::new("no_data"))?;
    let r = parse_usage(&v);
    if matches!(&r, Err(e) if e.key == "agy_abnormal") {
        STOPPED.store(true, Ordering::SeqCst);
    }
    r
}

/// 本程式專用的 agy 家目錄（%LOCALAPPDATA%\tools.aixai.token-usage\agy-home；測試時跟著 AITOKEN_DATA_DIR）。
/// 每次呼叫都把 last_check.timestamp 的修改時間設成現在 → agy 走「15 分鐘內已檢查」路徑，不開更新程序。
fn private_home() -> Result<PathBuf, Reason> {
    let base = std::env::var_os("AITOKEN_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("tools.aixai.token-usage")))
        .ok_or_else(|| Reason::error("no data dir"))?;
    let home = base.join("agy-home");
    touch_last_check(&home).map_err(|e| Reason::error(e.kind().to_string()))?;
    Ok(home)
}

fn touch_last_check(home: &std::path::Path) -> std::io::Result<()> {
    let dir = home.join(".gemini").join("antigravity-cli");
    std::fs::create_dir_all(&dir)?;
    let f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("last_check.timestamp"))?;
    f.set_modified(std::time::SystemTime::now())
}

fn run_usage(exe: &PathBuf) -> Result<String, Reason> {
    let mut cmd = Command::new(exe);
    cmd.args(["-p", "/usage", "--output-format", "json"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    // agy 每次啟動若距上次檢查超過 15 分鐘，會另開「新主控台」的背景更新程序，
    // Windows 11 會把它交給 Windows Terminal 顯示 → 使用者看到黑窗閃一下（2026-11-03 實測）。
    // CREATE_NO_WINDOW、隱形桌面都擋不住（Terminal 由系統另外啟動），
    // AGY_CLI_DISABLE_AUTO_UPDATE 在 -p 模式也無效（實測）。
    // 做法：給 agy 本程式專用的家目錄，每次查詢前把那裡的「上次檢查時間」設成現在。
    // 登入在 Windows 金鑰圈，與家目錄無關；使用者自己的 ~/.gemini 完全不碰，自己用 agy 時照常更新。
    let home = private_home()?;
    cmd.env("USERPROFILE", &home).env("HOME", &home);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| Reason::error(e.kind().to_string()))?;
    let mut stdout = child.stdout.take().ok_or_else(|| Reason::error("stdout"))?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut stdout).take(2_000_000).read_to_string(&mut s);
        let _ = tx.send(s);
    });
    match rx.recv_timeout(TIMEOUT) {
        Ok(s) => {
            let _ = child.wait();
            Ok(s)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(Reason::new("timeout"))
        }
    }
}

/// 解析 `/usage` 的 JSON。官方分組：每組有 name、description、buckets。
pub fn parse_usage(v: &Value) -> Result<Vec<UsageWindow>, Reason> {
    let total = v.pointer("/usage/total_tokens").and_then(Value::as_u64);
    let turns = v.get("num_turns").and_then(Value::as_u64);
    if total != Some(0) || turns != Some(0) {
        return Err(Reason::new("agy_abnormal"));
    }
    let groups = v.pointer("/command/data/groups").and_then(Value::as_array).ok_or_else(|| Reason::new("no_data"))?;
    let mut out = vec![];
    for g in groups {
        let name = g.get("name").and_then(Value::as_str).unwrap_or("");
        let title = clean_group_name(name);
        let models = group_models(name, g.get("description").and_then(Value::as_str).unwrap_or(""));
        let mut wins = vec![];
        for b in g.get("buckets").and_then(Value::as_array).into_iter().flatten() {
            let Some(left) = b.get("remaining_fraction").and_then(Value::as_f64) else { continue };
            let minutes = match b.get("window").and_then(Value::as_str) {
                Some("5h") => 300,
                Some("weekly") => 10080,
                _ => continue,
            };
            let resets = b.get("reset_time").and_then(Value::as_str).and_then(parse_iso);
            let mut w = UsageWindow::new(WindowLabel::from_minutes(minutes, None), ((1.0 - left) * 100.0).clamp(0.0, 100.0), resets);
            w.disabled = b.get("disabled").and_then(Value::as_bool).unwrap_or(false);
            w.group = Some(title.clone());
            w.group_models = models.clone();
            wins.push(w);
        }
        wins.sort_by_key(|w| w.label.minutes); // 5 小時在前、每週在後
        out.extend(wins);
    }
    if out.is_empty() {
        return Err(Reason::new("no_data"));
    }
    Ok(out)
}

/// "Gemini Models" → "Gemini"；"Claude and GPT models" → "Claude and GPT"；其他原樣
fn clean_group_name(name: &str) -> String {
    let n = name.trim();
    for suffix in [" models", " Models"] {
        if let Some(s) = n.strip_suffix(suffix) {
            return s.to_string();
        }
    }
    n.to_string()
}

/// "Models within this group: Gemini Flash, Gemini Pro" → "Flash, Pro"；格式不符回傳 None
fn group_models(group_name: &str, desc: &str) -> Option<String> {
    let list = desc.strip_prefix("Models within this group:")?.trim();
    let first = group_name.split_whitespace().next().unwrap_or("");
    let prefix = format!("{first} ");
    let items: Vec<String> = list
        .split(',')
        .map(|m| m.trim().strip_prefix(prefix.as_str()).unwrap_or(m.trim()).trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();
    (!items.is_empty()).then(|| items.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_last_check_sets_recent_mtime() {
        let home = std::env::temp_dir().join(format!("aitoken-test-agyhome-{}", std::process::id()));
        let f = home.join(".gemini").join("antigravity-cli").join("last_check.timestamp");
        touch_last_check(&home).unwrap();
        // 把時間改舊，再 touch 一次，必須回到「現在」
        std::fs::OpenOptions::new().append(true).open(&f).unwrap().set_modified(std::time::SystemTime::UNIX_EPOCH).unwrap();
        touch_last_check(&home).unwrap();
        let age = std::fs::metadata(&f).unwrap().modified().unwrap().elapsed().unwrap_or_default();
        assert!(age < std::time::Duration::from_secs(60));
        assert_eq!(std::fs::metadata(&f).unwrap().len(), 0); // 不寫內容
    }
    use serde_json::json;

    fn sample() -> Value {
        json!({"conversation_id":"","status":"SUCCESS","num_turns":0,
          "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0},
          "command":{"name":"usage","data":{"groups":[
            {"name":"Gemini Models","description":"Models within this group: Gemini Flash, Gemini Pro","buckets":[
              {"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":0.5377,"reset_time":"2026-11-04T04:44:28Z"},
              {"id":"gemini-5h","window":"5h","remaining_fraction":1,"reset_time":"2026-11-03T10:07:00Z"}]},
            {"name":"Claude and GPT models","description":"Models within this group: Claude Opus, Claude Sonnet, GPT-OSS","buckets":[
              {"id":"3p-weekly","window":"weekly","remaining_fraction":0,"reset_time":"2026-11-04T17:24:42Z"},
              {"id":"3p-5h","window":"5h","disabled":true,"remaining_fraction":1}]},
            {"name":"Gemini 4 Argon","description":"For paid Pro plans, Argon quota is separate...","buckets":[
              {"id":"gemini-argon-wtu-weekly","window":"weekly","remaining_fraction":0,"reset_time":"2026-11-04T08:09:38Z"}]}]}}})
    }

    #[test]
    fn parses_groups_and_windows() {
        let w = parse_usage(&sample()).unwrap();
        assert_eq!(w.len(), 5);
        // 第一組：5h 在前，每週在後
        assert_eq!(w[0].label.kind, "five_hour");
        assert_eq!(w[0].group.as_deref(), Some("Gemini"));
        assert_eq!(w[0].group_models.as_deref(), Some("Flash, Pro"));
        assert_eq!(w[0].used_percent, 0.0);
        assert_eq!(w[1].label.kind, "weekly");
        assert!((w[1].used_percent - 46.23).abs() < 0.01);
        // 第二組：disabled 的 5h
        assert_eq!(w[2].group.as_deref(), Some("Claude and GPT"));
        assert_eq!(w[2].group_models.as_deref(), Some("Opus, Sonnet, GPT-OSS"));
        assert!(w[2].disabled && w[2].resets_at.is_none());
        assert_eq!(w[3].used_percent, 100.0);
        // 第三組：描述不是模型清單，不顯示小字
        assert_eq!(w[4].group.as_deref(), Some("Gemini 4 Argon"));
        assert!(w[4].group_models.is_none());
    }

    #[test]
    fn nonzero_tokens_or_turns_is_abnormal() {
        let mut v = sample();
        v["usage"]["total_tokens"] = json!(5);
        assert_eq!(parse_usage(&v).unwrap_err().key, "agy_abnormal");
        let mut v = sample();
        v["num_turns"] = json!(1);
        assert_eq!(parse_usage(&v).unwrap_err().key, "agy_abnormal");
        let mut v = sample();
        v.as_object_mut().unwrap().remove("num_turns");
        assert_eq!(parse_usage(&v).unwrap_err().key, "agy_abnormal");
    }

    #[test]
    fn empty_groups_is_no_data() {
        let v = json!({"num_turns":0,"usage":{"total_tokens":0},"command":{"data":{"groups":[]}}});
        assert_eq!(parse_usage(&v).unwrap_err().key, "no_data");
    }
}
