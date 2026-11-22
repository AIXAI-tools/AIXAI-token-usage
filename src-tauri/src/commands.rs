//! 給面板的指令：面板尺寸、設定、帳號管理、PowerShell 登入
use crate::cache;
use crate::config;
use crate::model::{mask_email, Snapshot};
use crate::refresh::{do_refresh, emit_snapshot, refresh_soon};
use crate::snapshot::rebuild_offline_cards;
use crate::state::{zh_of, AppState};
use crate::tray::{change_language, panel_width, place_panel, reveal_panel, PANEL};
use crate::applog;
use serde::Serialize;
use tauri::{AppHandle, LogicalSize, Manager, State};
use tauri_plugin_autostart::ManagerExt;

#[derive(Serialize)]
pub(crate) struct AccountDto {
    key: String,
    platform: String,
    name: String,
    email_masked: Option<String>,
    email_full: Option<String>,
    visible: bool,
    removable: bool,
}

#[derive(Serialize)]
pub(crate) struct OfflineDto {
    id: String,
    email_masked: String,
    /// 換出前最後一次查詢的時間
    queried_at: i64,
    visible: bool,
}

#[derive(Serialize)]
pub(crate) struct SettingsDto {
    language: String,
    zh: bool,
    notify_threshold: u8,
    notify_on_reset: bool,
    autostart: bool,
    accounts: Vec<AccountDto>,
    offline: Vec<OfflineDto>,
}

#[derive(Serialize)]
pub(crate) struct AddHint {
    dir: String,
    command: String,
}

pub(crate) fn settings_dto(app: &AppHandle) -> SettingsDto {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let snap = state.snapshot.lock().unwrap().clone();
    let accounts = cfg
        .accounts
        .iter()
        .map(|a| {
            let p = snap.providers.iter().find(|p| p.key == a.id);
            AccountDto {
                key: a.id.clone(),
                platform: a.provider.clone(),
                name: a.name.clone(),
                email_masked: p.and_then(|p| p.account_masked.clone()),
                email_full: p.and_then(|p| p.account_full.clone()),
                visible: a.visible,
                removable: p.is_some_and(|p| p.removable),
            }
        })
        .collect();
    let offline = state
        .offline
        .lock()
        .unwrap()
        .iter()
        .map(|e| {
            let id = cache::offline_id(&e.email);
            OfflineDto { visible: !cfg.offline_hidden.contains(&id), id, email_masked: mask_email(&e.email), queried_at: e.queried_at }
        })
        .collect();
    SettingsDto {
        zh: zh_of(&cfg),
        language: cfg.language,
        notify_threshold: cfg.notify_threshold,
        notify_on_reset: cfg.notify_on_reset,
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        accounts,
        offline,
    }
}

#[tauri::command]
pub(crate) fn get_snapshot(state: State<AppState>) -> Snapshot {
    state.snapshot.lock().unwrap().clone()
}

#[tauri::command]
pub(crate) async fn refresh(app: AppHandle) -> Result<Snapshot, String> {
    tauri::async_runtime::spawn_blocking(move || do_refresh(&app)).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn panel_resize(app: AppHandle, height: f64) {
    let Some(win) = app.get_webview_window(PANEL) else { return };
    let h = height.clamp(120.0, 900.0);
    let width = panel_width(zh_of(&app.state::<AppState>().config.lock().unwrap()));
    let _ = win.set_size(LogicalSize::new(width, h));
    place_panel(&win);
    reveal_panel(&app);
}

#[tauri::command]
pub(crate) fn get_settings(app: AppHandle) -> SettingsDto {
    settings_dto(&app)
}

/// 前端每次換頁／輸入時回報：是否停在主畫面以外（失焦不收起），以及未送出的回報草稿
#[tauri::command]
pub(crate) fn panel_state_set(sticky: bool, draft: Option<String>) {
    crate::tray::PANEL_STICKY.store(sticky, std::sync::atomic::Ordering::SeqCst);
    *crate::tray::PANEL_DRAFT.lock().unwrap() = draft.filter(|d| d.len() <= 64 * 1024);
}

#[tauri::command]
pub(crate) fn panel_draft_get() -> Option<String> {
    crate::tray::PANEL_DRAFT.lock().unwrap().clone()
}

/// 標題列的 ✕：收起面板（草稿留在記憶體，下次進回報頁還在）
#[tauri::command]
pub(crate) fn panel_close(app: AppHandle) {
    let h = app.clone();
    let _ = app.run_on_main_thread(move || crate::tray::close_panel(&h));
}

/// 測試用：AITOKEN_START_VIEW=settings 時，面板一開始就進設定頁
#[tauri::command]
pub(crate) fn start_view() -> String {
    std::env::var("AITOKEN_START_VIEW").unwrap_or_default()
}

#[tauri::command]
pub(crate) fn set_account(app: AppHandle, key: String, name: Option<String>, visible: Option<bool>) -> SettingsDto {
    let mut newly_visible = false;
    {
        let state = app.state::<AppState>();
        let mut cfg = state.config.lock().unwrap();
        if let Some(a) = cfg.find_mut(&key) {
            if let Some(n) = name {
                a.name = n.trim().chars().take(24).collect();
            }
            if let Some(v) = visible {
                newly_visible = v && !a.visible;
                a.visible = v;
            }
        }
        let _ = cfg.save(&state.config_path);
        // 同步到目前畫面上的資料
        let mut snap = state.snapshot.lock().unwrap();
        if let (Some(a), Some(p)) = (cfg.accounts.iter().find(|a| a.id == key), snap.providers.iter_mut().find(|p| p.key == key)) {
            p.name = a.name.clone();
            p.visible = a.visible;
        }
    }
    emit_snapshot(&app);
    if newly_visible {
        refresh_soon(&app);
    }
    settings_dto(&app)
}


#[tauri::command]
pub(crate) fn set_offline_visible(app: AppHandle, id: String, visible: bool) -> SettingsDto {
    {
        let state = app.state::<AppState>();
        let mut cfg = state.config.lock().unwrap();
        cfg.offline_hidden.retain(|x| *x != id);
        if !visible {
            cfg.offline_hidden.push(id);
        }
        let _ = cfg.save(&state.config_path);
    }
    rebuild_offline_cards(&app);
    emit_snapshot(&app);
    settings_dto(&app)
}

#[tauri::command]
pub(crate) fn forget_offline(app: AppHandle, id: String) -> SettingsDto {
    {
        let state = app.state::<AppState>();
        let mut off = state.offline.lock().unwrap();
        if cache::forget_offline(&mut off, &id) {
            cache::save_offline(&state.offline_path, &off);
        }
        let mut cfg = state.config.lock().unwrap();
        cfg.offline_hidden.retain(|x| *x != id);
        let _ = cfg.save(&state.config_path);
    }
    rebuild_offline_cards(&app);
    emit_snapshot(&app);
    settings_dto(&app)
}

#[tauri::command]
pub(crate) fn remove_account(app: AppHandle, key: String) -> SettingsDto {
    {
        let state = app.state::<AppState>();
        let removable = state.snapshot.lock().unwrap().providers.iter().any(|p| p.key == key && p.removable);
        if removable {
            let mut cfg = state.config.lock().unwrap();
            cfg.accounts.retain(|a| a.id != key);
            let _ = cfg.save(&state.config_path);
            let mut cache = state.cache.lock().unwrap();
            cache.remove(&key);
            cache::save(&state.cache_path, &cache);
            state.snapshot.lock().unwrap().providers.retain(|p| p.key != key);
        }
    }
    emit_snapshot(&app);
    settings_dto(&app)
}

#[tauri::command]
pub(crate) fn set_notify(app: AppHandle, threshold: u8, on_reset: bool) -> SettingsDto {
    {
        let state = app.state::<AppState>();
        let mut cfg = state.config.lock().unwrap();
        if [5, 10, 20, 30].contains(&threshold) {
            cfg.notify_threshold = threshold;
        }
        cfg.notify_on_reset = on_reset;
        let _ = cfg.save(&state.config_path);
    }
    settings_dto(&app)
}

#[tauri::command]
pub(crate) fn set_language(app: AppHandle, lang: String) -> SettingsDto {
    change_language(&app, &lang);
    settings_dto(&app)
}

/// 掃描家目錄的 .claude-N／.codex-N，新的加進清單。回傳最新設定與新增數量。
#[tauri::command]
pub(crate) fn detect_accounts(app: AppHandle) -> (SettingsDto, usize) {
    let mut added = 0;
    if let Some(home) = config::home_dir() {
        let state = app.state::<AppState>();
        let mut cfg = state.config.lock().unwrap();
        for (provider, dir) in config::scan_extra_dirs(&home) {
            if cfg.add_extra(provider, &dir) {
                added += 1;
            }
        }
        if added > 0 {
            cfg.sort_accounts();
            let _ = cfg.save(&state.config_path);
        }
    }
    if added > 0 {
        refresh_soon(&app);
    }
    (settings_dto(&app), added)
}

/// 新增帳號用的一行 PowerShell 指令（資料夾自動取下一個空號）。只有這兩種固定格式。
pub(crate) fn build_hint(app: &AppHandle, platform: &str) -> Option<AddHint> {
    let home = config::home_dir()?;
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap();
    let (prefix, command) = match platform {
        "claude" => (".claude-", r#"$env:CLAUDE_CONFIG_DIR="$HOME\{dir}"; claude"#),
        "codex" => (".codex-", r#"$env:CODEX_HOME="$HOME\{dir}"; codex login"#),
        _ => return None,
    };
    let dir = config::next_free_dir(&home, prefix, &cfg);
    Some(AddHint { command: command.replace("{dir}", &dir), dir })
}

#[tauri::command]
pub(crate) fn add_account_hint(app: AppHandle, platform: String) -> Option<AddHint> {
    build_hint(&app, &platform)
}

/// 開新的 PowerShell 主控台視窗（-NoExit）執行指令，工作目錄為使用者家目錄
pub(crate) fn spawn_powershell(command: &str) -> bool {
    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args(["-NoExit", "-NoLogo", "-Command", command]);
    if let Some(home) = config::home_dir() {
        cmd.current_dir(home);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }
    cmd.spawn().is_ok()
}

/// 使用者按「開啟 PowerShell 登入」：開一個新的 PowerShell 視窗並執行指令。
/// 這是全程式唯一刻意顯示視窗的子程序；指令由後端依 platform 組成，不接受前端傳來的字串。
#[tauri::command]
pub(crate) fn open_powershell_login(app: AppHandle, platform: String) -> bool {
    let Some(hint) = build_hint(&app, &platform) else { return false };
    let ok = spawn_powershell(&hint.command);
    applog::log(&format!("open powershell login platform={platform} dir={} ok={ok}", hint.dir));
    ok
}


#[cfg(test)]
mod tests {
    /// 手動實測用（會開真的 PowerShell 視窗）：cargo test open_ps -- --ignored
    /// 指令由環境變數 AITOKEN_TEST_PS_CMD 提供
    #[test]
    #[ignore]
    fn open_ps_window_manual() {
        let cmd = std::env::var("AITOKEN_TEST_PS_CMD").unwrap();
        assert!(super::spawn_powershell(&cmd));
    }
}
