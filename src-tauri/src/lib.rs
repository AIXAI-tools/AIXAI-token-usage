mod antigravity;
mod cache;
mod claude;
mod codex;
mod config;
mod model;
mod notify;
mod ring;

use cache::{Cache, CacheEntry};
use config::{AccountCfg, Config};
use model::{mask_email, now_epoch, Provider, Reason, Snapshot, UsageWindow};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, RunEvent, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_notification::NotificationExt;

const TRAY_ID: &str = "main";
const PANEL: &str = "panel";
const PANEL_WIDTH: f64 = 336.0;
const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);

/// 右鍵選單上要隨語言／狀態更新的項目
struct MenuItems {
    refresh: MenuItem<tauri::Wry>,
    lang_menu: Submenu<tauri::Wry>,
    lang_system: CheckMenuItem<tauri::Wry>,
    lang_zh: CheckMenuItem<tauri::Wry>,
    lang_en: CheckMenuItem<tauri::Wry>,
    autostart: CheckMenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
}

struct AppState {
    snapshot: Mutex<Snapshot>,
    refreshing: AtomicBool,
    last_hidden: Mutex<Option<Instant>>,
    config: Mutex<Config>,
    config_path: PathBuf,
    cache: Mutex<Cache>,
    cache_path: PathBuf,
    tracker: Mutex<notify::Tracker>,
    menu: Mutex<Option<MenuItems>>,
}

fn is_zh_system() -> bool {
    sys_locale::get_locale().is_some_and(|l| l.to_lowercase().starts_with("zh"))
}

fn zh_of(cfg: &Config) -> bool {
    match cfg.language.as_str() {
        "zh-TW" => true,
        "en" => false,
        _ => is_zh_system(),
    }
}

fn platform_name(id: &str) -> &'static str {
    match id {
        "codex" => "Codex",
        "claude" => "Claude",
        _ => "Antigravity",
    }
}

// ---------- 組出每個帳號的結果 ----------

/// 一次查詢的結果：成功＝(窗口, 來源)；失敗＝原因
struct Fetched {
    email: Option<String>,
    outcome: Result<(Vec<UsageWindow>, &'static str), Reason>,
}

fn base_provider(a: &AccountCfg, removable: bool) -> Provider {
    let mut p = Provider::placeholder(&a.provider, "unavailable");
    p.key = a.id.clone();
    p.name = a.name.clone();
    p.visible = a.visible;
    p.removable = removable;
    p
}

fn set_email(p: &mut Provider, email: Option<String>) {
    p.account_masked = email.as_deref().map(mask_email);
    p.account_full = email;
}

/// 只用快取組出結果（隱藏的帳號、切換掉的 agy 舊帳號、剛啟動時用）
fn from_cache(a: &AccountCfg, removable: bool, cache: &Cache, reason: Option<Reason>, now: i64) -> Provider {
    let mut p = base_provider(a, removable);
    match cache.get(&a.id) {
        Some(e) => {
            p.status = "ok".into();
            p.source = Some("cache".into());
            p.as_of = Some(e.queried_at);
            p.windows = cache::apply_reset(&e.windows, now);
            p.reason = reason;
            set_email(&mut p, e.email.clone());
        }
        None => p.reason = Some(reason.unwrap_or_else(|| Reason::new("no_data"))),
    }
    p
}

fn cfg_dir(a: &AccountCfg) -> Option<PathBuf> {
    match a.provider.as_str() {
        "claude" => a.dir.as_ref().map(PathBuf::from).or_else(claude::default_dir),
        "codex" => a.dir.as_ref().map(PathBuf::from),
        _ => None,
    }
}

fn fetch_platform(a: &AccountCfg) -> Fetched {
    match a.provider.as_str() {
        "codex" => {
            let dir = cfg_dir(a);
            let p = codex::fetch(dir.as_deref());
            let email = p.account_full.clone();
            if p.status == "ok" {
                let src = if p.source.as_deref() == Some("local_log") { "local_log" } else { "live" };
                Fetched { email, outcome: Ok((p.windows, src)) }
            } else {
                Fetched { email, outcome: Err(p.reason.unwrap_or_else(|| Reason::new("no_data"))) }
            }
        }
        "claude" => {
            let dir = cfg_dir(a).unwrap_or_default();
            let r = claude::fetch(&dir);
            Fetched { email: r.email, outcome: r.windows.map(|w| (w, "live")) }
        }
        _ => {
            let r = antigravity::fetch();
            Fetched { email: r.email, outcome: r.windows.map(|w| (w, "live")) }
        }
    }
}

/// 查詢一個帳號；成功且為即時資料時更新快取；失敗時退回快取
fn fetch_account(a: &AccountCfg, removable: bool, cache: &mut Cache, now: i64) -> Provider {
    let f = fetch_platform(a);
    match f.outcome {
        Ok((windows, source)) => {
            let mut p = base_provider(a, removable);
            p.status = "ok".into();
            p.source = Some(source.into());
            p.as_of = Some(now);
            let email = f.email.or_else(|| cache.get(&a.id).and_then(|e| e.email.clone()));
            if source == "live" {
                cache.insert(a.id.clone(), CacheEntry { email: email.clone(), queried_at: now, windows: windows.clone() });
            }
            set_email(&mut p, email);
            p.windows = windows;
            p
        }
        Err(reason) => {
            let mut p = from_cache(a, removable, cache, Some(reason), now);
            if p.account_full.is_none() {
                set_email(&mut p, f.email);
            }
            p
        }
    }
}

fn agy_id(email: &Option<String>) -> String {
    format!("antigravity:{}", email.as_deref().unwrap_or("default"))
}

/// 目前設定裡的帳號清單（含 agy 目前登入的帳號）
fn current_accounts(app: &AppHandle) -> (Vec<AccountCfg>, String) {
    let state = app.state::<AppState>();
    let agy_current = agy_id(&antigravity::active_email());
    let mut cfg = state.config.lock().unwrap();
    if !cfg.accounts.iter().any(|a| a.id == agy_current) {
        let email = agy_current.strip_prefix("antigravity:").unwrap_or("default").to_string();
        cfg.accounts.push(AccountCfg { id: agy_current.clone(), provider: "antigravity".into(), dir: Some(email), name: String::new(), visible: true, extra: false });
        cfg.sort_accounts();
        let _ = cfg.save(&state.config_path);
    }
    (cfg.accounts.clone(), agy_current)
}

fn is_removable(a: &AccountCfg, agy_current: &str) -> bool {
    a.extra || (a.provider == "antigravity" && a.id != agy_current)
}

/// 查詢所有帳號、更新圖示與面板、發通知。同時間只跑一次。
fn do_refresh(app: &AppHandle) -> Snapshot {
    let state = app.state::<AppState>();
    if state.refreshing.swap(true, Ordering::SeqCst) {
        return state.snapshot.lock().unwrap().clone();
    }
    {
        let mut s = state.snapshot.lock().unwrap();
        s.refreshing = true;
        let _ = app.emit_to(PANEL, "snapshot", s.clone());
    }

    let (accounts, agy_current) = current_accounts(app);
    let mut cache = state.cache.lock().unwrap().clone();
    let now = now_epoch();
    let mut providers = vec![];
    for a in &accounts {
        let removable = is_removable(a, &agy_current);
        let p = if !a.visible {
            from_cache(a, removable, &cache, None, now)
        } else if a.provider == "antigravity" && a.id != agy_current {
            from_cache(a, removable, &cache, Some(Reason::new("agy_switched")), now)
        } else {
            fetch_account(a, removable, &mut cache, now)
        };
        providers.push(p);
    }
    cache::save(&state.cache_path, &cache);
    *state.cache.lock().unwrap() = cache;

    send_notifications(app, &providers, now);

    let snap = {
        let mut s = state.snapshot.lock().unwrap();
        s.providers = providers;
        s.updated_at = Some(now_epoch());
        s.refreshing = false;
        s.clone()
    };
    state.refreshing.store(false, Ordering::SeqCst);
    update_tray(app, &snap);
    let _ = app.emit_to(PANEL, "snapshot", snap.clone());
    snap
}

fn account_title(p: &Provider) -> String {
    let who = if !p.name.is_empty() { p.name.clone() } else { p.account_masked.clone().unwrap_or_default() };
    if who.is_empty() {
        platform_name(&p.id).to_string()
    } else {
        format!("{} · {}", platform_name(&p.id), who)
    }
}

fn send_notifications(app: &AppHandle, providers: &[Provider], now: i64) {
    let state = app.state::<AppState>();
    let (zh, threshold, on_reset) = {
        let c = state.config.lock().unwrap();
        (zh_of(&c), c.notify_threshold, c.notify_on_reset)
    };
    let mut tracker = state.tracker.lock().unwrap();
    for p in providers.iter().filter(|p| p.visible && p.status == "ok" && matches!(p.source.as_deref(), Some("live") | Some("local_log"))) {
        for n in tracker.evaluate(&p.key, &p.windows, threshold, on_reset, now) {
            let (title, body) = notify::message(zh, &account_title(p), &n);
            let _ = app.notification().builder().title(title).body(body).show();
        }
    }
}

/// 設定變動後，不重新查詢，只用現有資料更新名稱／顯示並重畫圖示
fn emit_snapshot(app: &AppHandle) {
    let state = app.state::<AppState>();
    let snap = state.snapshot.lock().unwrap().clone();
    update_tray(app, &snap);
    let _ = app.emit_to(PANEL, "snapshot", snap);
}

/// 等目前這輪查詢結束後，再重新查詢一次（設定變動後使用）
fn refresh_soon(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        for _ in 0..300 {
            if !app.state::<AppState>().refreshing.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        do_refresh(&app);
    });
}

/// 每個平台取所有「顯示中」帳號的最低剩餘 %
fn lefts(snap: &Snapshot) -> [Option<f64>; 3] {
    let get = |id: &str| {
        snap.providers
            .iter()
            .filter(|p| p.id == id && p.visible)
            .filter_map(Provider::lowest_left)
            .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| a.min(v))))
    };
    [get("codex"), get("claude"), get("antigravity")]
}

fn update_tray(app: &AppHandle, snap: &Snapshot) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let l = lefts(snap);
    let _ = tray.set_icon(Some(Image::new_owned(ring::draw(l), ring::SIZE, ring::SIZE)));
    let fmt = |v: Option<f64>| v.map_or("—".to_string(), |x| format!("{}%", x.round()));
    let tip = format!(
        "AIXAI Token Usage\nCodex {} · Claude {} · Antigravity {}",
        fmt(l[0]),
        fmt(l[1]),
        fmt(l[2])
    );
    let _ = tray.set_tooltip(Some(tip));
}

/// 面板貼在螢幕工作區（不含工作列）的右下角
fn place_panel(win: &WebviewWindow) {
    let monitor = win.current_monitor().ok().flatten().or_else(|| win.primary_monitor().ok().flatten());
    let (Some(m), Ok(size)) = (monitor, win.outer_size()) else { return };
    let area = m.work_area();
    let margin = (12.0 * m.scale_factor()) as i32;
    let x = area.position.x + area.size.width as i32 - size.width as i32 - margin;
    let y = area.position.y + area.size.height as i32 - size.height as i32 - margin;
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

// 省記憶體：面板（WebView2，約 65 MB）只在打開時存在；收起就整個關掉，背景只剩主程式（約 4 MB）。
// 圓環圖示、定時查詢、通知都在 Rust 端，不需要 WebView。

/// 面板已建立、但內容還沒畫好（尚未顯示）
static PANEL_PENDING: AtomicBool = AtomicBool::new(false);

fn toggle_panel(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(PANEL) {
        if win.is_visible().unwrap_or(false) {
            close_panel(app);
        }
        return; // 建立中：等它自己顯示
    }
    // 點圖示時面板會先失焦收起，緊接著的點擊不要又把它打開
    if let Some(state) = app.try_state::<AppState>() {
        if state.last_hidden.lock().unwrap().is_some_and(|t| t.elapsed() < Duration::from_millis(300)) {
            return;
        }
    }
    open_panel(app);
}

fn open_panel(app: &AppHandle) {
    PANEL_PENDING.store(true, Ordering::SeqCst);
    let built = WebviewWindowBuilder::new(app, PANEL, WebviewUrl::App("index.html".into()))
        .title("AIXAI Token Usage")
        .inner_size(PANEL_WIDTH, 420.0)
        .visible(false)
        .decorations(false)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .shadow(true)
        .focused(true)
        // 面板很簡單，不需要顯示卡加速：關掉 GPU 程序省記憶體。
        // 設了這個會取代 wry 的預設參數，所以預設的 --disable-features 要自己寫回來。
        .additional_browser_args("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-gpu")
        .build();
    let Ok(win) = built else {
        PANEL_PENDING.store(false, Ordering::SeqCst);
        return;
    };
    let handle = app.clone();
    win.on_window_event(move |e| {
        // 顯示前（建立過程）的失焦不算
        if matches!(e, WindowEvent::Focused(false)) && !PANEL_PENDING.load(Ordering::SeqCst) {
            let h = handle.clone();
            let _ = handle.run_on_main_thread(move || close_panel(&h));
        }
    });
    // 保險：前端沒回報尺寸時，1.5 秒後照樣顯示
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        let h = handle.clone();
        let _ = handle.run_on_main_thread(move || reveal_panel(&h));
    });
}

/// 內容畫好、尺寸算好之後才顯示，避免先閃一個錯的大小
fn reveal_panel(app: &AppHandle) {
    if !PANEL_PENDING.swap(false, Ordering::SeqCst) {
        return;
    }
    if let Some(win) = app.get_webview_window(PANEL) {
        place_panel(&win);
        let _ = win.show();
        let _ = win.set_focus();
    }
}

fn close_panel(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        *state.last_hidden.lock().unwrap() = Some(Instant::now());
    }
    PANEL_PENDING.store(false, Ordering::SeqCst);
    if let Some(win) = app.get_webview_window(PANEL) {
        let _ = win.destroy();
    }
}

// ---------- 右鍵選單 ----------

fn apply_menu_language(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (zh, lang) = {
        let c = state.config.lock().unwrap();
        (zh_of(&c), c.language.clone())
    };
    let Some(m) = state.menu.lock().unwrap().take() else { return };
    let tr = |a: &'static str, b: &'static str| if zh { a } else { b };
    let _ = m.refresh.set_text(tr("立即重新整理", "Refresh now"));
    let _ = m.lang_menu.set_text(tr("語言", "Language"));
    let _ = m.lang_system.set_text(tr("跟隨系統", "System default"));
    let _ = m.lang_zh.set_text("繁體中文");
    let _ = m.lang_en.set_text("English");
    let _ = m.autostart.set_text(tr("開機自動啟動", "Start with Windows"));
    let _ = m.quit.set_text(tr("結束", "Quit"));
    let _ = m.lang_system.set_checked(lang == "system");
    let _ = m.lang_zh.set_checked(lang == "zh-TW");
    let _ = m.lang_en.set_checked(lang == "en");
    let _ = m.autostart.set_checked(app.autolaunch().is_enabled().unwrap_or(false));
    *state.menu.lock().unwrap() = Some(m);
}

fn change_language(app: &AppHandle, lang: &str) {
    if !["system", "zh-TW", "en"].contains(&lang) {
        return;
    }
    {
        let state = app.state::<AppState>();
        let mut c = state.config.lock().unwrap();
        c.language = lang.into();
        let _ = c.save(&state.config_path);
    }
    apply_menu_language(app);
    let _ = app.emit_to(PANEL, "settings-changed", ());
}

fn toggle_autostart(app: &AppHandle) {
    let al = app.autolaunch();
    if al.is_enabled().unwrap_or(false) {
        let _ = al.disable();
    } else {
        let _ = al.enable();
    }
    apply_menu_language(app);
    let _ = app.emit_to(PANEL, "settings-changed", ());
}

// ---------- 給面板的指令 ----------

#[derive(Serialize)]
struct AccountDto {
    key: String,
    platform: String,
    name: String,
    email_masked: Option<String>,
    email_full: Option<String>,
    visible: bool,
    removable: bool,
}

#[derive(Serialize)]
struct SettingsDto {
    language: String,
    zh: bool,
    notify_threshold: u8,
    notify_on_reset: bool,
    autostart: bool,
    accounts: Vec<AccountDto>,
}

#[derive(Serialize)]
struct AddHint {
    dir: String,
    command: String,
}

fn settings_dto(app: &AppHandle) -> SettingsDto {
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
    SettingsDto {
        zh: zh_of(&cfg),
        language: cfg.language,
        notify_threshold: cfg.notify_threshold,
        notify_on_reset: cfg.notify_on_reset,
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        accounts,
    }
}

#[tauri::command]
fn get_snapshot(state: State<AppState>) -> Snapshot {
    state.snapshot.lock().unwrap().clone()
}

#[tauri::command]
async fn refresh(app: AppHandle) -> Result<Snapshot, String> {
    tauri::async_runtime::spawn_blocking(move || do_refresh(&app)).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn panel_resize(app: AppHandle, height: f64) {
    let Some(win) = app.get_webview_window(PANEL) else { return };
    let h = height.clamp(120.0, 900.0);
    let _ = win.set_size(LogicalSize::new(PANEL_WIDTH, h));
    place_panel(&win);
    reveal_panel(&app);
}

#[tauri::command]
fn get_settings(app: AppHandle) -> SettingsDto {
    settings_dto(&app)
}

/// 測試用：AITOKEN_START_VIEW=settings 時，面板一開始就進設定頁
#[tauri::command]
fn start_view() -> String {
    std::env::var("AITOKEN_START_VIEW").unwrap_or_default()
}

#[tauri::command]
fn set_account(app: AppHandle, key: String, name: Option<String>, visible: Option<bool>) -> SettingsDto {
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
fn remove_account(app: AppHandle, key: String) -> SettingsDto {
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
fn set_notify(app: AppHandle, threshold: u8, on_reset: bool) -> SettingsDto {
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
fn set_language(app: AppHandle, lang: String) -> SettingsDto {
    change_language(&app, &lang);
    settings_dto(&app)
}

/// 掃描家目錄的 .claude-N／.codex-N，新的加進清單。回傳最新設定與新增數量。
#[tauri::command]
fn detect_accounts(app: AppHandle) -> (SettingsDto, usize) {
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

/// 新增帳號用的一行 PowerShell 指令（資料夾自動取下一個空號）
#[tauri::command]
fn add_account_hint(app: AppHandle, platform: String) -> Option<AddHint> {
    let home = config::home_dir()?;
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap();
    let (prefix, command) = match platform.as_str() {
        "claude" => (".claude-", r#"$env:CLAUDE_CONFIG_DIR="$HOME\{dir}"; claude"#),
        "codex" => (".codex-", r#"$env:CODEX_HOME="$HOME\{dir}"; codex login"#),
        _ => return None,
    };
    let dir = config::next_free_dir(&home, prefix, &cfg);
    Some(AddHint { command: command.replace("{dir}", &dir), dir })
}

pub fn run() {
    tauri::Builder::default()
        // 只允許一份：重複開啟時改為打開原本那份的面板
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if !app.get_webview_window(PANEL).is_some_and(|w| w.is_visible().unwrap_or(false)) {
                toggle_panel(app);
            }
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None::<Vec<&'static str>>))
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            refresh,
            panel_resize,
            get_settings,
            start_view,
            set_account,
            remove_account,
            set_notify,
            set_language,
            detect_accounts,
            add_account_hint
        ])
        .setup(|app| {
            // 設定與快取資料夾
            let dir = config::data_dir(app.path().app_config_dir().ok()).ok_or("no config dir")?;
            let config_path = dir.join("config.json");
            let cache_path = dir.join("cache.json");
            let mut cfg = Config::load(&config_path);
            cfg.ensure_defaults();
            cfg.sort_accounts();

            // 第一次執行：預設開啟開機自動啟動
            if !cfg.autostart_initialized {
                let _ = app.autolaunch().enable();
                cfg.autostart_initialized = true;
            }
            let _ = cfg.save(&config_path);

            let cache = cache::load(&cache_path);
            // 剛啟動時先用快取顯示，第一次查詢完成後換成即時資料
            let now = now_epoch();
            let providers: Vec<Provider> = cfg.accounts.iter().map(|a| from_cache(a, a.extra, &cache, None, now)).collect();
            let zh = zh_of(&cfg);
            let lang = cfg.language.clone();
            app.manage(AppState {
                snapshot: Mutex::new(Snapshot { updated_at: None, refreshing: true, providers }),
                refreshing: AtomicBool::new(false),
                last_hidden: Mutex::new(None),
                config: Mutex::new(cfg),
                config_path,
                cache: Mutex::new(cache),
                cache_path,
                tracker: Mutex::new(notify::Tracker::default()),
                menu: Mutex::new(None),
            });

            // 右鍵選單：立即重新整理／語言 ▸／開機自動啟動／結束
            let tr = |a: &'static str, b: &'static str| if zh { a } else { b };
            let refresh_item = MenuItem::with_id(app, "refresh", tr("立即重新整理", "Refresh now"), true, None::<&str>)?;
            let lang_system = CheckMenuItem::with_id(app, "lang:system", tr("跟隨系統", "System default"), true, lang == "system", None::<&str>)?;
            let lang_zh = CheckMenuItem::with_id(app, "lang:zh-TW", "繁體中文", true, lang == "zh-TW", None::<&str>)?;
            let lang_en = CheckMenuItem::with_id(app, "lang:en", "English", true, lang == "en", None::<&str>)?;
            let lang_menu = Submenu::with_items(app, tr("語言", "Language"), true, &[&lang_system, &lang_zh, &lang_en])?;
            let autostart = CheckMenuItem::with_id(
                app,
                "autostart",
                tr("開機自動啟動", "Start with Windows"),
                true,
                app.autolaunch().is_enabled().unwrap_or(false),
                None::<&str>,
            )?;
            let quit_item = MenuItem::with_id(app, "quit", tr("結束", "Quit"), true, None::<&str>)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&refresh_item, &sep1, &lang_menu, &autostart, &sep2, &quit_item])?;
            *app.state::<AppState>().menu.lock().unwrap() = Some(MenuItems {
                refresh: refresh_item,
                lang_menu,
                lang_system,
                lang_zh,
                lang_en,
                autostart,
                quit: quit_item,
            });

            TrayIconBuilder::with_id(TRAY_ID)
                .icon(Image::new_owned(ring::draw([None, None, None]), ring::SIZE, ring::SIZE))
                .tooltip("AIXAI Token Usage")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "refresh" => refresh_soon(app),
                    "autostart" => toggle_autostart(app),
                    "quit" => app.exit(0),
                    id => {
                        if let Some(lang) = id.strip_prefix("lang:") {
                            change_language(app, lang);
                        }
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        toggle_panel(tray.app_handle());
                    }
                })
                .build(app)?;

            // 背景定時更新：啟動時一次，之後每 5 分鐘
            let handle = app.handle().clone();
            // 測試用：設了 AITOKEN_SHOW_ON_START 就在第一次查詢後直接打開面板（方便截圖驗證）
            let show_on_start = std::env::var_os("AITOKEN_SHOW_ON_START").is_some();
            std::thread::spawn(move || {
                let mut first = true;
                loop {
                    do_refresh(&handle);
                    if first && show_on_start {
                        // 等網頁載入完成再打開，太早顯示會被初始化時的失焦事件收起來
                        std::thread::sleep(Duration::from_secs(2));
                        let h = handle.clone();
                        let _ = handle.run_on_main_thread(move || toggle_panel(&h));
                    }
                    first = false;
                    std::thread::sleep(REFRESH_EVERY);
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building AIXAI Token Usage")
        .run(|_app, event| {
            // 面板關掉後沒有任何視窗，Tauri 預設會結束程式；只有右鍵「結束」（帶 exit code）才真的結束
            if let RunEvent::ExitRequested { code: None, api, .. } = event {
                api.prevent_exit();
            }
        });
}
