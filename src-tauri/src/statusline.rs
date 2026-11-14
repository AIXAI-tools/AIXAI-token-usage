//! Claude Code 狀態列：路徑、安裝、設定與前端指令。所有磁碟介面均明確傳入 app_dir。
use crate::{claude, config::AccountCfg, refresh, state::AppState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

const SCRIPT: &str = include_str!("statusline.ps1");
static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

pub fn key(account_id: &str) -> String {
    format!("{:x}", Sha256::digest(account_id.as_bytes()))[..16].to_string()
}

pub fn status_path(app_dir: &Path, account_id: &str) -> PathBuf {
    app_dir.join("claude-status").join(format!("{}.json", key(account_id)))
}

pub fn is_installed(app_dir: &Path, account_id: &str, dir: &Path) -> bool {
    read_json(&dir.join("settings.json")).ok().is_some_and(|v| {
        v.pointer("/statusLine/command").and_then(Value::as_str).is_some_and(|command| {
            command.contains("statusline.ps1")
                && command.contains(&format!("-Key {}", key(account_id)))
                && app_dir.join("statusline.ps1").is_file()
        })
    })
}

#[derive(Deserialize, Serialize)]
struct ScriptConfig {
    #[serde(default = "show_default")]
    show: bool,
    #[serde(default)]
    prev: BTreeMap<String, String>,
}

fn show_default() -> bool { true }

impl Default for ScriptConfig {
    fn default() -> Self { Self { show: true, prev: BTreeMap::new() } }
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|_| "io")?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| "settings_invalid".into())
}

fn load_config(app_dir: &Path) -> Result<ScriptConfig, String> {
    match fs::read_to_string(app_dir.join("statusline.json")) {
        Ok(text) => serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| "io".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ScriptConfig::default()),
        Err(_) => Err("io".into()),
    }
}

fn save_config(app_dir: &Path, config: &ScriptConfig) -> Result<(), String> {
    fs::create_dir_all(app_dir).map_err(|_| "io")?;
    fs::write(app_dir.join("statusline.json"), serde_json::to_vec_pretty(config).map_err(|_| "io")?).map_err(|_| "io".into())
}

// 同秒重新設定仍保留每份備份，絕不覆寫前次備份。
fn backup(dir: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut time = chrono::DateTime::<chrono::Utc>::from_timestamp(crate::model::now_epoch(), 0).ok_or("io")?;
    loop {
        let path = dir.join(format!("settings.json.aitoken-bak-{}", time.format("%Y%m%d%H%M%S")));
        match fs::OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => return file.write_all(bytes).map_err(|_| "io".into()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => time += chrono::Duration::seconds(1),
            Err(_) => return Err("io".into()),
        }
    }
}

fn script_bytes() -> Vec<u8> {
    let mut script = b"\xef\xbb\xbf".to_vec();
    script.extend_from_slice(SCRIPT.as_bytes());
    script
}

/// App 啟動時呼叫：已安裝的腳本若是舊版就換成這一版（沒安裝過就不動）。回傳是否有更新。
pub fn refresh_script(app_dir: &Path) -> bool {
    let path = app_dir.join("statusline.ps1");
    let want = script_bytes();
    match fs::read(&path) {
        Ok(current) if current != want => fs::write(&path, want).is_ok(),
        _ => false,
    }
}

pub fn install(app_dir: &Path, account_id: &str, dir: &Path) -> Result<(), String> {
    let _guard = SETTINGS_LOCK.lock().map_err(|_| "io")?;
    let path = dir.join("settings.json");
    let original = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("io".into()),
    };
    let mut settings = match &original {
        Some(bytes) => {
            let text = std::str::from_utf8(bytes).map_err(|_| "settings_invalid")?;
            serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')).map_err(|_| "settings_invalid")?
        }
        None => json!({}),
    };
    if !settings.is_object() { return Err("settings_invalid".into()); }
    let mut config = load_config(app_dir)?;
    let account_key = key(account_id);
    if let Some(command) = settings.pointer("/statusLine/command").and_then(Value::as_str) {
        if !command.contains("statusline.ps1") { config.prev.insert(account_key.clone(), command.into()); }
    }
    fs::create_dir_all(app_dir).map_err(|_| "io")?;
    fs::write(app_dir.join("statusline.ps1"), script_bytes()).map_err(|_| "io")?;
    fs::create_dir_all(dir).map_err(|_| "io")?;
    if let Some(bytes) = original { backup(dir, &bytes)?; }
    let command = format!(
        "powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\" -Key {} -Dir \"{}\"",
        app_dir.join("statusline.ps1").display(), account_key, dir.display()
    );
    settings["statusLine"] = json!({ "type": "command", "command": command, "padding": 0 });
    fs::write(path, serde_json::to_vec_pretty(&settings).map_err(|_| "io")?).map_err(|_| "io")?;
    save_config(app_dir, &config)
}

#[derive(Serialize)]
pub(crate) struct StatusDto {
    show: bool,
    accounts: Vec<AccountStatus>,
}

#[derive(Serialize)]
struct AccountStatus {
    id: String,
    state: &'static str,
    last_at: Option<i64>,
}

fn account_dir(account: &AccountCfg) -> Option<PathBuf> {
    if account.id == "claude:default" { claude::default_dir() } else { account.dir.as_ref().map(PathBuf::from) }
}

fn dto(app_dir: &Path, accounts: &[AccountCfg]) -> Result<StatusDto, String> {
    Ok(StatusDto {
        show: load_config(app_dir)?.show,
        accounts: accounts.iter().filter(|a| a.provider == "claude").map(|a| AccountStatus {
            id: a.id.clone(),
            state: if account_dir(a).is_some_and(|dir| is_installed(app_dir, &a.id, &dir)) { "installed" } else { "not_installed" },
            last_at: read_json(&status_path(app_dir, &a.id)).ok().and_then(|v| v.get("captured_at").and_then(Value::as_i64)),
        }).collect(),
    })
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn get_claude_statusline(app: AppHandle) -> Result<StatusDto, String> {
    let state = app.state::<AppState>();
    let accounts = state.config.lock().map_err(|_| "io")?.accounts.clone();
    dto(state.cache_path.parent().ok_or("io")?, &accounts)
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn install_claude_statusline(app: AppHandle, id: Option<String>) -> Result<StatusDto, String> {
    let state = app.state::<AppState>();
    let accounts = state.config.lock().map_err(|_| "io")?.accounts.clone();
    let app_dir = state.cache_path.parent().ok_or("io")?;
    for account in accounts.iter().filter(|a| a.provider == "claude" && id.as_ref().is_none_or(|id| id == &a.id)) {
        install(app_dir, &account.id, &account_dir(account).ok_or("io")?)?;
    }
    refresh::refresh_soon(&app);
    dto(app_dir, &accounts)
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn set_claude_statusline_show(app: AppHandle, show: bool) -> Result<StatusDto, String> {
    let state = app.state::<AppState>();
    let app_dir = state.cache_path.parent().ok_or("io")?;
    {
        let _guard = SETTINGS_LOCK.lock().map_err(|_| "io")?;
        let mut config = load_config(app_dir)?;
        config.show = show;
        save_config(app_dir, &config)?;
    }
    get_claude_statusline(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 不清除資料夾：保留失敗現場，也遵守本專案不刪檔規則。
    fn temp() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aitoken-statusline-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn fixture() -> (PathBuf, PathBuf) {
        let root = temp();
        (root.join("app space 中文"), root.join("claude space 中文"))
    }

    fn write_settings(dir: &Path, text: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("settings.json"), text).unwrap();
    }

    #[test]
    fn key_fixed_and_status_path() {
        let app = temp();
        assert_eq!(key("claude:default"), "fc491a4a265ed4de");
        assert_eq!(status_path(&app, "claude:default"), app.join("claude-status/fc491a4a265ed4de.json"));
    }

    #[test]
    fn install_creates_settings_and_bom_script() {
        let (app, dir) = fixture();
        install(&app, "claude:default", &dir).unwrap();
        assert!(is_installed(&app, "claude:default", &dir));
        let v = read_json(&dir.join("settings.json")).unwrap();
        assert_eq!(v["statusLine"]["type"], "command");
        assert_eq!(v["statusLine"]["padding"], 0);
        assert_eq!(v["statusLine"]["command"], format!(
            "powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\" -Key fc491a4a265ed4de -Dir \"{}\"",
            app.join("statusline.ps1").display(), dir.display()
        ));
        let bytes = fs::read(app.join("statusline.ps1")).unwrap();
        assert!(bytes.starts_with(b"\xef\xbb\xbf"));
        assert_eq!(&bytes[3..], SCRIPT.as_bytes());
        assert!(load_config(&app).unwrap().show);
    }

    #[test]
    fn install_preserves_other_keys_prev_and_backups_on_reinstall() {
        let (app, dir) = fixture();
        let original = r#"{"permissions":{"allow":["Read"]},"env":{"LANG":"zh_TW"},"statusLine":{"type":"command","command":"cmd /c echo hi"}}"#;
        write_settings(&dir, original);
        install(&app, "claude:default", &dir).unwrap();
        let first = fs::read(dir.join("settings.json")).unwrap();
        install(&app, "claude:default", &dir).unwrap();
        let v = read_json(&dir.join("settings.json")).unwrap();
        assert_eq!(v["permissions"], json!({"allow":["Read"]}));
        assert_eq!(v["env"], json!({"LANG":"zh_TW"}));
        let config = load_config(&app).unwrap();
        assert_eq!(config.prev.len(), 1);
        assert_eq!(config.prev[&key("claude:default")], "cmd /c echo hi");
        let backups: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path())
            .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("settings.json.aitoken-bak-")).collect();
        assert_eq!(backups.len(), 2);
        let contents: Vec<_> = backups.iter().map(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            let stamp = name.strip_prefix("settings.json.aitoken-bak-").unwrap();
            assert_eq!(stamp.len(), 14);
            assert!(stamp.chars().all(|c| c.is_ascii_digit()));
            fs::read(p).unwrap()
        }).collect();
        assert!(contents.contains(&original.as_bytes().to_vec()));
        assert!(contents.contains(&first));
    }

    #[test]
    fn invalid_settings_are_unchanged_and_have_no_side_effects() {
        for invalid in ["{broken", "[]", "null", "42"] {
            let (app, dir) = fixture();
            write_settings(&dir, invalid);
            assert_eq!(install(&app, "claude:default", &dir), Err("settings_invalid".into()));
            assert_eq!(fs::read_to_string(dir.join("settings.json")).unwrap(), invalid);
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
            assert!(!app.exists());
        }
    }

    #[test]
    fn is_installed_requires_matching_key_command_and_script() {
        let (app, dir) = fixture();
        assert!(!is_installed(&app, "claude:default", &dir));
        install(&app, "claude:default", &dir).unwrap();
        assert!(is_installed(&app, "claude:default", &dir));
        assert!(!is_installed(&app, "claude:other", &dir));
        assert!(!is_installed(&app.join("missing"), "claude:default", &dir));
        write_settings(&dir, r#"{"statusLine":{"command":"echo hello"}}"#);
        assert!(!is_installed(&app, "claude:default", &dir));
    }

    #[test]
    fn dto_filters_accounts_and_returns_show_state_last_at() {
        let (app, dir) = fixture();
        let account = AccountCfg { id: "claude:extra".into(), provider: "claude".into(), dir: Some(dir.to_string_lossy().into()), name: String::new(), visible: false, extra: true };
        let mut other = account.clone();
        other.provider = "codex".into();
        let accounts = vec![account, other];
        let before = serde_json::to_value(dto(&app, &accounts).unwrap()).unwrap();
        assert_eq!(before, json!({"show":true,"accounts":[{"id":"claude:extra","state":"not_installed","last_at":null}]}));
        install(&app, "claude:extra", &dir).unwrap();
        fs::create_dir_all(app.join("claude-status")).unwrap();
        fs::write(status_path(&app, "claude:extra"), r#"{"captured_at":1794600000}"#).unwrap();
        let mut config = load_config(&app).unwrap();
        config.show = false;
        config.prev.insert("another-key".into(), "echo retained".into());
        save_config(&app, &config).unwrap();
        let after = serde_json::to_value(dto(&app, &accounts).unwrap()).unwrap();
        assert_eq!(after, json!({"show":false,"accounts":[{"id":"claude:extra","state":"installed","last_at":1794600000}]}));
        assert_eq!(load_config(&app).unwrap().prev["another-key"], "echo retained");
    }

    #[test]
    fn refresh_script_replaces_old_only_when_installed() {
        let (app, dir) = fixture();
        assert!(!refresh_script(&app));
        assert!(!app.join("statusline.ps1").exists());
        install(&app, "claude:default", &dir).unwrap();
        assert!(!refresh_script(&app));
        fs::write(app.join("statusline.ps1"), "old script").unwrap();
        assert!(refresh_script(&app));
        assert_eq!(fs::read(app.join("statusline.ps1")).unwrap(), script_bytes());
    }

    #[cfg(windows)]
    #[test]
    fn script_reads_email_despite_duplicate_case_keys() {
        let (app, dir) = fixture();
        install(&app, "claude:default", &dir).unwrap();
        // 真實 .claude.json 會有大小寫不同的專案路徑鍵，PowerShell 5.1 的 ConvertFrom-Json 會失敗
        fs::write(dir.join(".claude.json"), r#"{"projects":{"C:/a":{},"c:/a":{}},"oauthAccount":{"accountUuid":"x","emailAddress":"dup@example.invalid","organizationName":"o"}}"#).unwrap();
        run_script(&app, &dir, r#"{"rate_limits":{"five_hour":{"used_percentage":1,"resets_at":null}}}"#);
        assert_eq!(read_json(&status_path(&app, "claude:default")).unwrap()["email"], "dup@example.invalid");
    }

    #[test]
    fn script_non_ascii_is_limited_to_contract() {
        assert!(SCRIPT.chars().filter(|c| !c.is_ascii()).all(|c| "本週｜".contains(c)));
    }

    #[cfg(windows)]
    fn run_script(app: &Path, dir: &Path, raw: &str) -> String {
        use std::process::{Command, Stdio};
        let home = app.parent().unwrap().join("fake-home");
        fs::create_dir_all(&home).unwrap();
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(app.join("statusline.ps1")).arg("-Key").arg(key("claude:default"))
            .arg("-Dir").arg(dir).env("USERPROFILE", &home)
            .env("APPDATA", &home).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().unwrap();
        child.stdin.take().unwrap().write_all(raw.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{:?}", out);
        assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn script_official_json_writes_fields_and_renders_utf8() {
        let (app, dir) = fixture();
        install(&app, "claude:default", &dir).unwrap();
        fs::write(dir.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"test@example.invalid"}}"#).unwrap();
        let raw = r#"{"rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":1794612000},"seven_day":{"used_percentage":41.2,"resets_at":1795000000}}}"#;
        let before = crate::model::now_epoch();
        assert_eq!(run_script(&app, &dir, raw).trim(), "Claude 5h 23.5%｜本週 41.2%");
        let bytes = fs::read(status_path(&app, "claude:default")).unwrap();
        assert!(!bytes.starts_with(b"\xef\xbb\xbf"));
        let status: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status["v"], 1);
        assert_eq!(status["email"], "test@example.invalid");
        assert!((before..=crate::model::now_epoch()).contains(&status["captured_at"].as_i64().unwrap()));
        let input: Value = serde_json::from_str(raw).unwrap();
        assert_eq!(status["five_hour"], input["rate_limits"]["five_hour"]);
        assert_eq!(status["seven_day"], input["rate_limits"]["seven_day"]);
        assert!(!status_path(&app, "claude:default").with_extension("json.tmp").exists());
        // 同一路徑再寫一次，確認覆寫及缺窗口省略。
        assert_eq!(run_script(&app, &dir, r#"{"rate_limits":{"five_hour":{"used_percentage":0,"resets_at":null}}}"#).trim(), "Claude 5h 0%");
        assert!(read_json(&status_path(&app, "claude:default")).unwrap().get("seven_day").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn script_missing_limits_preserves_last_file_and_swallows_errors() {
        let (app, dir) = fixture();
        install(&app, "claude:default", &dir).unwrap();
        let path = status_path(&app, "claude:default");
        for raw in ["{}", r#"{"rate_limits":{}}"#, "broken json"] {
            assert_eq!(run_script(&app, &dir, raw), "");
            assert!(!path.exists());
        }
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "previous bytes").unwrap();
        assert_eq!(run_script(&app, &dir, "{}"), "");
        assert_eq!(fs::read_to_string(path).unwrap(), "previous bytes");
        fs::write(app.join("statusline.json"), "broken json").unwrap();
        assert_eq!(run_script(&app, &dir, "broken json"), "");
    }

    #[cfg(windows)]
    #[test]
    fn script_prev_prints_hi_and_receives_identical_stdin_even_on_invalid_json() {
        let (app, dir) = fixture();
        write_settings(&dir, r#"{"statusLine":{"command":"cmd /c echo hi"}}"#);
        install(&app, "claude:default", &dir).unwrap();
        assert_eq!(run_script(&app, &dir, "{}"), "hi\r\n");
        let mut config = load_config(&app).unwrap();
        config.show = false;
        // 由暫存腳本逐位元組回傳輸入，驗證空白、中文及 CRLF 不經 JSON 重組。
        let echo = app.join("echo-input.ps1");
        fs::write(&echo, "[Console]::OpenStandardInput().CopyTo([Console]::OpenStandardOutput())").unwrap();
        config.prev.insert(key("claude:default"), format!(
            "powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"", echo.display()
        ));
        save_config(&app, &config).unwrap();
        let raw = " { invalid JSON 中文 }\r\nsecond line\r\n";
        assert_eq!(run_script(&app, &dir, raw), raw);
    }

    #[cfg(windows)]
    #[test]
    fn script_show_false_still_captures_and_default_email_uses_fake_home() {
        let (app, _) = fixture();
        let home = app.parent().unwrap().join("fake-home");
        let dir = home.join(".claude");
        install(&app, "claude:default", &dir).unwrap();
        fs::write(home.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"default@example.invalid"}}"#).unwrap();
        let mut config = load_config(&app).unwrap();
        config.show = false;
        save_config(&app, &config).unwrap();
        let raw = r#"{"rate_limits":{"seven_day":{"used_percentage":41.2,"resets_at":null}}}"#;
        assert_eq!(run_script(&app, &dir, raw), "");
        let status = read_json(&status_path(&app, "claude:default")).unwrap();
        assert_eq!(status["email"], "default@example.invalid");
        assert!(status.get("five_hour").is_none());
    }
}
