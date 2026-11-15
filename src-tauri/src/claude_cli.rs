// Claude 額度（主要來源）：官方 `claude -p "/usage" --output-format json`。
// 內建指令在本機處理，不送對話、不耗額度（2026-11-15 實測 total_cost_usd 0、output_tokens 0）。
// AItoken 不碰登入 token（ADR-013 精神不變）；失敗時由 claude.rs 退回狀態列檔案。
// 安全：每次回應都檢查花費與 token 必須是 0、num_turns 0，否則視為異常，本次執行不再查詢。
use crate::model::{Reason, UsageWindow, WindowLabel};
use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(45);
static STOPPED: AtomicBool = AtomicBool::new(false);

fn find_claude() -> Option<PathBuf> {
    // 測試用：AITOKEN_CLAUDE_PATH 設了就只用它（指到不存在的檔案＝模擬找不到）
    if let Some(p) = std::env::var_os("AITOKEN_CLAUDE_PATH") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join("claude.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let p = crate::config::home_dir()?.join(".local").join("bin").join("claude.exe");
    p.is_file().then_some(p)
}

/// config_dir：額外帳號的 Claude 資料夾（預設資料夾傳 None）
pub fn fetch(config_dir: Option<&Path>, now: i64) -> Result<Vec<UsageWindow>, Reason> {
    // 單元測試不執行真的 claude（真的跑用 real_claude_usage）
    if cfg!(test) {
        return Err(Reason::new("claude_cli_not_found"));
    }
    fetch_real(config_dir, now)
}

fn fetch_real(config_dir: Option<&Path>, now: i64) -> Result<Vec<UsageWindow>, Reason> {
    if STOPPED.load(Ordering::SeqCst) {
        return Err(Reason::new("claude_cli_abnormal"));
    }
    if std::env::var_os("AITOKEN_OFFLINE").is_some() {
        return Err(Reason::new("offline"));
    }
    let exe = find_claude().ok_or_else(|| Reason::new("claude_cli_not_found"))?;
    let out = run_usage(&exe, config_dir)?;
    let v: Value = serde_json::from_str(&out).map_err(|_| Reason::new("no_data"))?;
    let r = parse_usage(&v, now);
    if matches!(&r, Err(e) if e.key == "claude_cli_abnormal") {
        STOPPED.store(true, Ordering::SeqCst);
        crate::applog::log("claude /usage abnormal (cost or turns > 0); stop querying");
    }
    r
}

fn run_usage(exe: &Path, config_dir: Option<&Path>) -> Result<String, Reason> {
    let mut cmd = Command::new(exe);
    cmd.args(["-p", "/usage", "--output-format", "json", "--no-session-persistence", "--strict-mcp-config"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    // 固定工作目錄，避免讀到某個專案的設定；也只會在 ~/.claude/projects 留一個空資料夾
    if let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("tools.aixai.token-usage")) {
        if std::fs::create_dir_all(&dir).is_ok() {
            cmd.current_dir(dir);
        }
    }
    if let Some(d) = config_dir {
        cmd.env("CLAUDE_CONFIG_DIR", d);
    }
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

/// 解析 `/usage` 的 JSON：result 是文字，一行一個窗口
/// 「Current session: 5% used · resets Nov 15, 11:49pm (Asia/Taipei)」
/// 「Current week (all models): 87% used · resets Nov 19, 4:59am (Asia/Taipei)」
/// 「Current week (Fable): 56% used · …」→ 額外每週窗口，名稱取括號內；0% 不顯示（同 TASK-008）
pub fn parse_usage(v: &Value, now: i64) -> Result<Vec<UsageWindow>, Reason> {
    let cost = v.get("total_cost_usd").and_then(Value::as_f64);
    let turns = v.get("num_turns").and_then(Value::as_u64);
    let out_tokens = v.pointer("/usage/output_tokens").and_then(Value::as_u64).unwrap_or(0);
    if cost != Some(0.0) || turns != Some(0) || out_tokens != 0 {
        return Err(Reason::new("claude_cli_abnormal"));
    }
    if v.get("is_error").and_then(Value::as_bool) == Some(true) {
        return Err(Reason::new("no_data"));
    }
    let text = v.get("result").and_then(Value::as_str).ok_or_else(|| Reason::new("no_data"))?;
    let mut five = None;
    let mut week = None;
    let mut scoped = vec![];
    for line in text.lines().map(str::trim) {
        let Some((name, rest)) = line.split_once(':') else { continue };
        let Some(used) = percent(rest) else { continue };
        let resets = rest.split_once("resets ").and_then(|(_, r)| parse_reset(r, now));
        match name.trim() {
            "Current session" => five = Some(UsageWindow::new(WindowLabel::from_minutes(300, None), used, resets)),
            "Current week (all models)" => week = Some(UsageWindow::new(WindowLabel::from_minutes(10080, None), used, resets)),
            n => {
                if let Some(model) = n.strip_prefix("Current week (").and_then(|s| s.strip_suffix(')')) {
                    if used > 0.0 {
                        scoped.push(UsageWindow::new(WindowLabel::from_minutes(10080, Some(model.to_string())), used, resets));
                    }
                }
            }
        }
    }
    let mut out: Vec<UsageWindow> = five.into_iter().chain(week).collect();
    if out.is_empty() {
        return Err(Reason::new("no_data"));
    }
    out.extend(scoped);
    Ok(out)
}

/// 「 5% used · resets …」→ 5.0
fn percent(rest: &str) -> Option<f64> {
    let (num, _) = rest.split_once("% used")?;
    num.trim().parse::<f64>().ok().map(|p| p.clamp(0.0, 100.0))
}

/// 「Nov 15, 11:49pm (Asia/Taipei)」或「11:49pm (Asia/Taipei)」→ epoch。時間以本機時區解讀（Claude Code 也用本機時區顯示）。
/// 沒寫年份：取 now 前 1 天之後最近的那一次。
fn parse_reset(s: &str, now: i64) -> Option<i64> {
    let s = s.split(" (").next()?.trim();
    let (date, time) = match s.rsplit_once(", ") {
        Some((d, t)) => (Some(d.trim()), t.trim()),
        None => (None, s),
    };
    let t = parse_time(time)?;
    let today = Local.timestamp_opt(now, 0).single()?.date_naive();
    let day = match date {
        Some(d) => {
            let (mon, dd) = d.split_once(' ')?;
            let m = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]
                .iter().position(|x| mon.to_ascii_lowercase().starts_with(x))? as u32 + 1;
            let dd: u32 = dd.trim().parse().ok()?;
            let mut nd = NaiveDate::from_ymd_opt(today.year(), m, dd)?;
            if (nd - today).num_days() < -1 {
                nd = NaiveDate::from_ymd_opt(today.year() + 1, m, dd)?;
            }
            nd
        }
        None => today,
    };
    let local = Local.from_local_datetime(&NaiveDateTime::new(day, t)).earliest()?;
    let mut at = local.timestamp();
    if date.is_none() && at < now {
        at += 86400;
    }
    Some(at)
}

/// 「11:49pm」「4am」→ NaiveTime
fn parse_time(s: &str) -> Option<NaiveTime> {
    let s = s.to_ascii_lowercase();
    let (body, pm) = if let Some(b) = s.strip_suffix("pm") { (b, true) } else { (s.strip_suffix("am")?, false) };
    let (h, m) = match body.trim().split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (body.trim().parse::<u32>().ok()?, 0),
    };
    if !(1..=12).contains(&h) {
        return None;
    }
    let h24 = (h % 12) + if pm { 12 } else { 0 };
    NaiveTime::from_hms_opt(h24, m, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).earliest().unwrap().timestamp()
    }

    fn resp(text: &str) -> Value {
        json!({"total_cost_usd":0,"num_turns":0,"is_error":false,"usage":{"input_tokens":0,"output_tokens":0},"result":text})
    }

    const SAMPLE: &str = "You are currently using your subscription to power your Claude Code usage\n\n\
Current session: 5% used · resets Nov 15, 11:49pm (Asia/Taipei)\n\
Current week (all models): 87% used · resets Nov 19, 4:59am (Asia/Taipei)\n\
Current week (Fable): 56% used · resets Nov 19, 4:59am (Asia/Taipei)\n\n\
What's contributing to your usage?\nLast 24h · 2019 requests · 13 sessions\n  76%  Start fresh when you switch tasks · run /clear\n";

    #[test]
    fn parses_real_sample() {
        let now = local(2026, 11, 15, 19, 0);
        let w = parse_usage(&resp(SAMPLE), now).unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].label.kind, "five_hour");
        assert_eq!(w[0].used_percent, 5.0);
        assert_eq!(w[0].resets_at, Some(local(2026, 11, 15, 23, 49)));
        assert_eq!(w[1].label.kind, "weekly");
        assert_eq!(w[1].used_percent, 87.0);
        assert_eq!(w[1].resets_at, Some(local(2026, 11, 19, 4, 59)));
        assert_eq!(w[2].label.bucket.as_deref(), Some("Fable"));
        assert_eq!(w[2].used_percent, 56.0);
    }

    #[test]
    fn scoped_zero_hidden_and_year_rollover() {
        let now = local(2026, 12, 30, 12, 0);
        let text = "Current session: 0% used\nCurrent week (all models): 3% used · resets Jan 2, 9am (X)\nCurrent week (Fable): 0% used · resets Jan 2, 9am (X)";
        let w = parse_usage(&resp(text), now).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].resets_at, None);
        assert_eq!(w[1].resets_at, Some(local(2027, 1, 2, 9, 0)));
        // 只有時間、已過 → 明天
        assert_eq!(parse_reset("11:00am (X)", now), Some(local(2026, 12, 31, 11, 0)));
        assert_eq!(parse_reset("12:30pm (X)", now), Some(local(2026, 12, 30, 12, 30)));
    }

    #[test]
    fn abnormal_and_unrecognized() {
        let now = local(2026, 11, 15, 19, 0);
        let mut v = resp(SAMPLE);
        v["total_cost_usd"] = json!(0.15);
        assert_eq!(parse_usage(&v, now).unwrap_err().key, "claude_cli_abnormal");
        let mut v = resp(SAMPLE);
        v["num_turns"] = json!(1);
        assert_eq!(parse_usage(&v, now).unwrap_err().key, "claude_cli_abnormal");
        assert_eq!(parse_usage(&resp("Unknown command: /usage"), now).unwrap_err().key, "no_data");
        let mut v = resp(SAMPLE);
        v["is_error"] = json!(true);
        assert_eq!(parse_usage(&v, now).unwrap_err().key, "no_data");
    }

    /// 真的跑一次 `claude -p "/usage"`（cargo test -- --ignored real_claude_usage --nocapture）
    #[test]
    #[ignore]
    fn real_claude_usage() {
        let now = chrono::Utc::now().timestamp();
        let w = fetch_real(None, now).unwrap();
        for x in &w {
            println!("{} {:?} {} {:?}", x.label.kind, x.label.bucket, x.used_percent, x.resets_at);
        }
        assert!(w.len() >= 2);
    }
}
