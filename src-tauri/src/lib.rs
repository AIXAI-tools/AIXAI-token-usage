mod antigravity;
mod applog;
mod cache;
mod claude;
mod codex;
mod config;
mod feedback;
mod model;
mod notify;
mod reports;
mod ring;
mod shot;
mod update;

use cache::{Cache, CacheEntry, Offline};
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
    /// 換出去的 Claude 帳號（離線卡片）
    offline: Mutex<Offline>,
    offline_path: PathBuf,
    tracker: Mutex<notify::Tracker>,
    menu: Mutex<Option<MenuItems>>,
    update: Mutex<update::Shared>,
    reports: Mutex<reports::Store>,
    reports_path: PathBuf,
    /// 進設定頁前擷取的主畫面（本程式面板自己的畫面，PNG）
    shot: Mutex<Option<Vec<u8>>>,
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
fn fetch_account(a: &AccountCfg, removable: bool, cache: &mut Cache, offline: &mut Offline, now: i64) -> Provider {
    let f = fetch_platform(a);
    match f.outcome {
        Ok((windows, source)) => {
            let mut p = base_provider(a, removable);
            p.status = "ok".into();
            p.source = Some(source.into());
            p.as_of = Some(now);
            // Claude：同一個資料夾換了帳號（/login）→ 舊帳號留成離線卡片；換回來 → 移除離線紀錄
            if a.provider == "claude" && source == "live" {
                if let Some(new_email) = f.email.as_deref() {
                    if cache::track_swap(offline, cache.get(&a.id), new_email) {
                        applog::log(&format!("claude offline accounts changed: now {}", offline.len()));
                    }
                }
            }
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

/// 離線帳號卡片（Claude）：重置時間到的窗口恢復 100%；不發通知、不進圓環
fn offline_provider(e: &cache::OfflineEntry, hidden: &[String], now: i64) -> Provider {
    let id = cache::offline_id(&e.email);
    let mut p = Provider::placeholder("claude", "ok");
    p.visible = !hidden.contains(&id);
    p.key = format!("offline:{id}");
    p.source = Some("offline".into());
    p.as_of = Some(e.queried_at);
    p.account_masked = Some(mask_email(&e.email));
    p.windows = cache::apply_reset(&e.windows, now);
    p.offline = true;
    p
}

/// 把離線卡片接在最後一個 Claude 帳號後面
fn with_offline(mut providers: Vec<Provider>, offline: &Offline, hidden: &[String], now: i64) -> Vec<Provider> {
    providers.retain(|p| !p.offline);
    let at = providers.iter().rposition(|p| p.id == "claude").map_or(providers.len(), |i| i + 1);
    let offs: Vec<Provider> = offline.iter().map(|e| offline_provider(e, hidden, now)).collect();
    providers.splice(at..at, offs);
    providers
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
    let mut offline = state.offline.lock().unwrap().clone();
    let now = now_epoch();
    let mut providers = vec![];
    for a in &accounts {
        let removable = is_removable(a, &agy_current);
        let p = if !a.visible {
            from_cache(a, removable, &cache, None, now)
        } else if a.provider == "antigravity" && a.id != agy_current {
            from_cache(a, removable, &cache, Some(Reason::new("agy_switched")), now)
        } else {
            fetch_account(a, removable, &mut cache, &mut offline, now)
        };
        providers.push(p);
    }
    cache::save(&state.cache_path, &cache);
    *state.cache.lock().unwrap() = cache;
    let offline_changed = offline != *state.offline.lock().unwrap();
    if offline_changed {
        cache::save_offline(&state.offline_path, &offline);
        *state.offline.lock().unwrap() = offline.clone();
    }

    for p in &providers {
        applog::log(&format!(
            "refresh {} visible={} status={} source={} reason={}",
            p.key,
            p.visible,
            p.status,
            p.source.as_deref().unwrap_or("-"),
            p.reason.as_ref().map(|r| r.key.as_str()).unwrap_or("-")
        ));
    }

    send_notifications(app, &providers, now);
    let hidden = state.config.lock().unwrap().offline_hidden.clone();
    let providers = with_offline(providers, &offline, &hidden, now);

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
struct OfflineDto {
    id: String,
    email_masked: String,
    /// 換出前最後一次查詢的時間
    queried_at: i64,
    visible: bool,
}

#[derive(Serialize)]
struct SettingsDto {
    language: String,
    zh: bool,
    notify_threshold: u8,
    notify_on_reset: bool,
    autostart: bool,
    accounts: Vec<AccountDto>,
    offline: Vec<OfflineDto>,
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

/// 離線帳號卡片資料變動後，重建畫面上的離線卡片
fn rebuild_offline_cards(app: &AppHandle) {
    let state = app.state::<AppState>();
    let hidden = state.config.lock().unwrap().offline_hidden.clone();
    let offline = state.offline.lock().unwrap().clone();
    let mut snap = state.snapshot.lock().unwrap();
    snap.providers = with_offline(std::mem::take(&mut snap.providers), &offline, &hidden, now_epoch());
}

#[tauri::command]
fn set_offline_visible(app: AppHandle, id: String, visible: bool) -> SettingsDto {
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
fn forget_offline(app: AppHandle, id: String) -> SettingsDto {
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

/// 新增帳號用的一行 PowerShell 指令（資料夾自動取下一個空號）。只有這兩種固定格式。
fn build_hint(app: &AppHandle, platform: &str) -> Option<AddHint> {
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
fn add_account_hint(app: AppHandle, platform: String) -> Option<AddHint> {
    build_hint(&app, &platform)
}

/// 開新的 PowerShell 主控台視窗（-NoExit）執行指令，工作目錄為使用者家目錄
fn spawn_powershell(command: &str) -> bool {
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
fn open_powershell_login(app: AppHandle, platform: String) -> bool {
    let Some(hint) = build_hint(&app, &platform) else { return false };
    let ok = spawn_powershell(&hint.command);
    applog::log(&format!("open powershell login platform={platform} dir={} ok={ok}", hint.dir));
    ok
}

// ---------- 線上更新 ----------

const UPDATE_EVENT: &str = "update-state";
const UPDATE_EVERY: Duration = Duration::from_secs(24 * 3600);

fn emit_update(app: &AppHandle) {
    let s = app.state::<AppState>().update.lock().unwrap().state.clone();
    let _ = app.emit_to(PANEL, UPDATE_EVENT, s);
}

fn update_dir() -> Option<PathBuf> {
    config::local_dir().map(|d| d.join("update"))
}

/// 查 GitHub 最新版，結果存在 AppState（失敗靜默，只寫 log）
fn check_update(app: &AppHandle) -> update::UpdateState {
    {
        let st = app.state::<AppState>();
        let mut u = st.update.lock().unwrap();
        if u.state.checking {
            return u.state.clone();
        }
        u.state.checking = true;
    }
    emit_update(app);
    let result = update::fetch_latest();
    let now = now_epoch();
    let snap = {
        let st = app.state::<AppState>();
        let mut u = st.update.lock().unwrap();
        match result {
            Ok(rel) => {
                applog::log(&format!("update check ok: latest={} current={}", rel.tag, update::current_version()));
                u.apply_release(rel, now);
            }
            Err(e) => {
                applog::log(&format!("update check failed: {e}"));
                u.state.check_error = Some(e);
                u.state.last_check = Some(now);
            }
        }
        u.state.checking = false;
        u.state.clone()
    };
    emit_update(app);
    snap
}

fn set_install_stage(app: &AppHandle, stage: &str, percent: u8) {
    {
        let st = app.state::<AppState>();
        let mut u = st.update.lock().unwrap();
        u.state.stage = stage.into();
        u.state.percent = percent;
    }
    emit_update(app);
}

fn run_install(app: &AppHandle, rel: &update::Release) -> Result<(), String> {
    let setup = rel.setup.clone().ok_or("no_setup_asset")?;
    let sums = rel.sums.clone().ok_or("sums_missing")?;
    if !update::safe_exe_name(&setup.name) {
        return Err("bad_asset_name".into());
    }
    let dir = update_dir().ok_or("no_dir")?;
    let _ = std::fs::create_dir_all(&dir);
    clear_update_dir(&dir);
    applog::log(&format!("update install start: {}", setup.name));
    // 先抓校驗檔：沒有就不下載、不安裝
    let sums_text = update::fetch_text(&sums.url, 64 * 1024)?;
    let dest = dir.join(&setup.name);
    let mut last = 255u8;
    let dl = update::download_to(&setup.url, &dest, setup.size, |done, total| {
        let pct = if total > 0 { ((done * 100) / total).min(100) as u8 } else { 0 };
        if pct != last {
            last = pct;
            set_install_stage(app, "download", pct);
        }
    });
    if let Err(e) = dl {
        let _ = std::fs::remove_file(&dest); // 程式自己下載的暫存
        return Err(e);
    }
    set_install_stage(app, "verify", 100);
    update::verify_download(&dest, &sums_text, &setup.name)?;
    applog::log("update sha256 ok, launching installer");
    set_install_stage(app, "install", 100);
    // NSIS：/S 靜默、/UPDATE 更新模式（保留捷徑與設定）、/R 裝完以目前使用者重新開啟程式
    let mut cmd = std::process::Command::new(&dest);
    cmd.args(["/S", "/UPDATE", "/R"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().map_err(|_| "launch_failed".to_string())?;
    std::thread::sleep(Duration::from_millis(600));
    app.exit(0);
    Ok(())
}

/// 清掉更新暫存資料夾裡的檔案（都是程式自己下載的）
fn clear_update_dir(dir: &std::path::Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_file() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn start_install(app: &AppHandle) {
    let rel = {
        let st = app.state::<AppState>();
        let mut u = st.update.lock().unwrap();
        if u.installing || !u.state.has_update {
            return;
        }
        let Some(rel) = u.release.clone() else { return };
        u.installing = true;
        u.state.stage = "download".into();
        u.state.percent = 0;
        u.state.fail = None;
        rel
    };
    emit_update(app);
    let app = app.clone();
    std::thread::spawn(move || {
        if let Err(e) = run_install(&app, &rel) {
            applog::log(&format!("update install failed: {e}"));
            {
                let st = app.state::<AppState>();
                let mut u = st.update.lock().unwrap();
                u.installing = false;
                u.state.stage = "error".into();
                u.state.fail = Some(e);
            }
            emit_update(&app);
        }
    });
}

#[tauri::command]
fn get_update_state(state: State<AppState>) -> update::UpdateState {
    state.update.lock().unwrap().state.clone()
}

#[tauri::command]
async fn check_update_cmd(app: AppHandle) -> Result<update::UpdateState, String> {
    tauri::async_runtime::spawn_blocking(move || check_update(&app)).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn install_update_cmd(app: AppHandle) {
    start_install(&app);
}

/// 用預設瀏覽器開發佈頁（網址寫死，不接受前端傳入）
#[tauri::command]
fn open_releases_page() {
    let mut cmd = std::process::Command::new("explorer.exe");
    cmd.arg(update::RELEASES_PAGE);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd.spawn();
}

// ---------- 回報 ----------

const REPLY_EVERY: Duration = Duration::from_secs(6 * 3600);
const REPORTS_EVENT: &str = "reports-changed";

fn emit_reports(app: &AppHandle) {
    let unread = app.state::<AppState>().reports.lock().unwrap().unread_count() > 0;
    let _ = app.emit_to(PANEL, REPORTS_EVENT, unread);
}

/// 查回覆（只送編號）；失敗靜默寫 log。回傳新增則數
fn check_replies(app: &AppHandle) -> usize {
    let state = app.state::<AppState>();
    let ids = state.reports.lock().unwrap().pending_ids(now_epoch());
    if ids.is_empty() {
        return 0;
    }
    match reports::fetch_replies(&ids) {
        Ok(found) => {
            let added = {
                let mut st = state.reports.lock().unwrap();
                let n = st.apply_replies(found);
                st.save(&state.reports_path);
                n
            };
            applog::log(&format!("reply check ok: asked={} new={added}", ids.len()));
            emit_reports(app);
            added
        }
        Err(e) => {
            applog::log(&format!("reply check failed: {e}"));
            0
        }
    }
}

/// 擷取面板自己的畫面（只對面板視窗的 HWND 取圖，不碰螢幕）
#[tauri::command]
fn capture_panel_shot(app: AppHandle) -> bool {
    let Some(win) = app.get_webview_window(PANEL) else { return false };
    let Ok(h) = win.hwnd() else { return false };
    let png = shot::capture_window_png(h.0 as isize);
    let ok = png.is_some();
    *app.state::<AppState>().shot.lock().unwrap() = png;
    applog::log(&format!("panel shot captured={ok}"));
    ok
}

#[derive(Serialize)]
struct PrepareDto {
    sys: String,
    status: String,
    log: String,
    log_kb: u64,
    shot_b64: Option<String>,
    shot_kb: u64,
}

fn build_diagnostics(app: &AppHandle) -> feedback::Diagnostics {
    let state = app.state::<AppState>();
    let lang = {
        let c = state.config.lock().unwrap();
        if zh_of(&c) { "zh-TW" } else { "en" }.to_string()
    };
    let snap = state.snapshot.lock().unwrap().clone();
    let shot_png = state.shot.lock().unwrap().clone();
    feedback::Diagnostics {
        sys: feedback::sys_text(update::current_version(), &shot::windows_version(), &lang),
        status: feedback::status_text(&snap.providers),
        log: applog::read_recent(64),
        shot: shot_png,
    }
}

#[tauri::command]
fn report_prepare(app: AppHandle) -> PrepareDto {
    use base64::Engine;
    let d = build_diagnostics(&app);
    PrepareDto {
        log_kb: (d.log.len() as u64).div_ceil(1024),
        shot_kb: d.shot.as_ref().map(|s| (s.len() as u64).div_ceil(1024)).unwrap_or(0),
        shot_b64: d.shot.as_ref().map(|s| base64::engine::general_purpose::STANDARD.encode(s)),
        sys: d.sys,
        status: d.status,
        log: d.log,
    }
}

/// 預覽「會送出」的內文（與送出用同一個 compose_body）；也給「複製內容」用
#[tauri::command]
fn report_preview(app: AppHandle, req: feedback::ReportRequest) -> String {
    feedback::compose_body(&req, &build_diagnostics(&app))
}

#[tauri::command]
async fn report_send(app: AppHandle, req: feedback::ReportRequest) -> Result<String, String> {
    feedback::validate(&req)?;
    let diag = build_diagnostics(&app);
    let req2 = req.clone();
    let res = tauri::async_runtime::spawn_blocking(move || feedback::submit(&req2, &diag)).await.map_err(|e| e.to_string())?;
    match res {
        Ok(id) => {
            applog::log(&format!("report sent id={id} want_reply={}", req.want_reply));
            {
                let state = app.state::<AppState>();
                let mut st = state.reports.lock().unwrap();
                st.add(reports::Entry {
                    id: id.clone(),
                    title: req.title.trim().to_string(),
                    ts: now_epoch(),
                    want_reply: req.want_reply,
                    ref_id: req.ref_id.clone().filter(|r| feedback::is_report_id(r)),
                    ..Default::default()
                });
                st.save(&state.reports_path);
            }
            Ok(id)
        }
        Err(e) => {
            applog::log(&format!("report send failed: {e}"));
            Err(e)
        }
    }
}

#[derive(Serialize)]
struct ReplyDto {
    #[serde(flatten)]
    reply: reports::Reply,
    unread: bool,
    /// fixed 且帶的版本比目前新 → 面板顯示「更新到 vX」
    can_update: bool,
}

#[derive(Serialize)]
struct ReportDto {
    id: String,
    title: String,
    ts: i64,
    want_reply: bool,
    replies: Vec<ReplyDto>,
}

fn report_dtos(app: &AppHandle) -> Vec<ReportDto> {
    let st = app.state::<AppState>().reports.lock().unwrap().clone();
    st.reports
        .iter()
        .map(|e| ReportDto {
            id: e.id.clone(),
            title: e.title.clone(),
            ts: e.ts,
            want_reply: e.want_reply,
            replies: e
                .replies
                .iter()
                .map(|r| ReplyDto {
                    unread: !e.seen.contains(&r.rid),
                    can_update: r.kind == "fixed" && !r.version.is_empty() && update::is_newer(&r.version, update::current_version()),
                    reply: r.clone(),
                })
                .collect(),
        })
        .collect()
}

#[tauri::command]
fn report_list(app: AppHandle) -> Vec<ReportDto> {
    report_dtos(&app)
}

#[tauri::command]
fn report_mark_seen(app: AppHandle, id: String) {
    {
        let state = app.state::<AppState>();
        let mut st = state.reports.lock().unwrap();
        st.mark_seen(&id);
        st.save(&state.reports_path);
    }
    emit_reports(&app);
}

#[tauri::command]
async fn report_check_cmd(app: AppHandle) -> Result<Vec<ReportDto>, String> {
    let a = app.clone();
    tauri::async_runtime::spawn_blocking(move || check_replies(&a)).await.map_err(|e| e.to_string())?;
    Ok(report_dtos(&app))
}

#[tauri::command]
fn report_unread(state: State<AppState>) -> bool {
    state.reports.lock().unwrap().unread_count() > 0
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
            set_offline_visible,
            forget_offline,
            set_notify,
            set_language,
            detect_accounts,
            add_account_hint,
            open_powershell_login,
            get_update_state,
            check_update_cmd,
            install_update_cmd,
            open_releases_page,
            capture_panel_shot,
            report_prepare,
            report_preview,
            report_send,
            report_list,
            report_mark_seen,
            report_check_cmd,
            report_unread
        ])
        .setup(|app| {
            // 設定與快取資料夾
            let dir = config::data_dir(app.path().app_config_dir().ok()).ok_or("no config dir")?;
            let config_path = dir.join("config.json");
            let cache_path = dir.join("cache.json");
            let offline_path = dir.join("offline.json");
            let mut cfg = Config::load(&config_path);
            cfg.ensure_defaults();
            cfg.sort_accounts();

            // 第一次執行：預設開啟開機自動啟動
            if !cfg.autostart_initialized {
                let _ = app.autolaunch().enable();
                cfg.autostart_initialized = true;
            }
            let _ = cfg.save(&config_path);

            if let Some(base) = config::local_dir() {
                applog::init(&base);
            }
            applog::log(&format!("start v{}", update::current_version()));
            // 上次更新留下的安裝檔暫存（新版啟動時安裝已結束）
            if let Some(d) = update_dir() {
                clear_update_dir(&d);
            }

            let reports_path = config::local_dir().map(|d| d.join("reports.json")).unwrap_or_else(|| dir.join("reports.json"));
            let cache = cache::load(&cache_path);
            // 剛啟動時先用快取顯示，第一次查詢完成後換成即時資料
            let now = now_epoch();
            let offline = cache::load_offline(&offline_path);
            let providers: Vec<Provider> = cfg.accounts.iter().map(|a| from_cache(a, a.extra, &cache, None, now)).collect();
            let providers = with_offline(providers, &offline, &cfg.offline_hidden, now);
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
                offline: Mutex::new(offline),
                offline_path,
                tracker: Mutex::new(notify::Tracker::default()),
                menu: Mutex::new(None),
                update: Mutex::new(update::Shared::new()),
                reports: Mutex::new(reports::Store::load(&reports_path)),
                reports_path,
                shot: Mutex::new(None),
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

            // 更新檢查：啟動後 1 分鐘，之後每 24 小時（失敗靜默）。AITOKEN_UPDATE_FIRST_CHECK_SECS 供測試縮短
            let uh = app.handle().clone();
            std::thread::spawn(move || {
                let first = std::env::var("AITOKEN_UPDATE_FIRST_CHECK_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(60);
                std::thread::sleep(Duration::from_secs(first));
                loop {
                    check_update(&uh);
                    std::thread::sleep(UPDATE_EVERY);
                }
            });

            // 查開發者回覆：啟動後 2 分鐘，之後每 6 小時（失敗靜默）。AITOKEN_REPLY_FIRST_CHECK_SECS 供測試縮短
            let rh = app.handle().clone();
            std::thread::spawn(move || {
                let first = std::env::var("AITOKEN_REPLY_FIRST_CHECK_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(120);
                std::thread::sleep(Duration::from_secs(first));
                loop {
                    check_replies(&rh);
                    std::thread::sleep(REPLY_EVERY);
                }
            });

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_cards_follow_claude_and_apply_reset() {
        let mut codex = Provider::placeholder("codex", "ok");
        codex.key = "codex:default".into();
        let mut claude = Provider::placeholder("claude", "ok");
        claude.key = "claude:default".into();
        let mut agy = Provider::placeholder("antigravity", "ok");
        agy.key = "antigravity:x".into();
        let w = |used: f64, reset: Option<i64>| UsageWindow::new(model::WindowLabel::from_minutes(300, None), used, reset);
        let off = vec![cache::OfflineEntry { email: "aixa.test@gmail.com".into(), queried_at: 5, windows: vec![w(100.0, Some(10)), w(61.0, Some(i64::MAX))] }];
        let out = with_offline(vec![codex, claude, agy], &off, &[], 100);
        let keys: Vec<&str> = out.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys[..2], ["codex:default", "claude:default"]);
        assert!(keys[2].starts_with("offline:") && keys[3] == "antigravity:x");
        let o = &out[2];
        assert!(o.offline && o.visible && o.lowest_left().is_none() && o.account_full.is_none());
        assert_eq!(o.account_masked.as_deref(), Some("aixa•••@gmail.com"));
        assert_eq!(o.windows[0].used_percent, 0.0);
        assert!(o.windows[0].reset_done);
        assert_eq!(o.windows[1].used_percent, 61.0);
        // 重建不會重複；隱藏後 visible=false
        let again = with_offline(out, &off, &[cache::offline_id("aixa.test@gmail.com")], 100);
        assert_eq!(again.iter().filter(|p| p.offline).count(), 1);
        assert!(!again.iter().find(|p| p.offline).unwrap().visible);
    }

    /// 手動實測用（會開真的 PowerShell 視窗）：cargo test open_ps -- --ignored
    /// 指令由環境變數 AITOKEN_TEST_PS_CMD 提供
    #[test]
    #[ignore]
    fn open_ps_window_manual() {
        let cmd = std::env::var("AITOKEN_TEST_PS_CMD").unwrap();
        assert!(super::spawn_powershell(&cmd));
    }
}
