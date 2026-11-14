//! 定時查詢、do_refresh、通知觸發
use crate::cache;
use crate::model::{now_epoch, Provider, Reason, Snapshot};
use crate::snapshot::{current_accounts, fetch_account, from_cache, is_removable, with_offline};
use crate::state::{platform_name, zh_of, AppState};
use crate::tray::{update_tray, PANEL};
use crate::{applog, notify};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

pub(crate) const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);

/// 查詢所有帳號、更新圖示與面板、發通知。同時間只跑一次。
pub(crate) fn do_refresh(app: &AppHandle) -> Snapshot {
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
    crate::terminal::write_now(app); // Claude Code 狀態列後面接的 Codex／agy 額度
    crate::planview::on_refresh(app, &snap.providers); // 重置券、每日目標、模型用量
    update_tray(app, &snap);
    let _ = app.emit_to(PANEL, "snapshot", snap.clone());
    snap
}

pub(crate) fn account_title(p: &Provider) -> String {
    let who = if !p.name.is_empty() { p.name.clone() } else { p.account_masked.clone().unwrap_or_default() };
    if who.is_empty() {
        platform_name(&p.id).to_string()
    } else {
        format!("{} · {}", platform_name(&p.id), who)
    }
}

pub(crate) fn send_notifications(app: &AppHandle, providers: &[Provider], now: i64) {
    let state = app.state::<AppState>();
    let (zh, threshold, on_reset) = {
        let c = state.config.lock().unwrap();
        (zh_of(&c), c.notify_threshold, c.notify_on_reset)
    };
    let mut tracker = state.tracker.lock().unwrap();
    for p in providers.iter().filter(|p| p.visible && p.status == "ok" && matches!(p.source.as_deref(), Some("live") | Some("local_log") | Some("statusline"))) {
        for n in tracker.evaluate(&p.key, &p.windows, threshold, on_reset, now) {
            let (title, body) = notify::message(zh, &account_title(p), &n);
            let _ = app.notification().builder().title(title).body(body).show();
        }
    }
}

/// 設定變動後，不重新查詢，只用現有資料更新名稱／顯示並重畫圖示
pub(crate) fn emit_snapshot(app: &AppHandle) {
    let state = app.state::<AppState>();
    let snap = state.snapshot.lock().unwrap().clone();
    update_tray(app, &snap);
    let _ = app.emit_to(PANEL, "snapshot", snap);
}

/// 等目前這輪查詢結束後，再重新查詢一次（設定變動後使用）
pub(crate) fn refresh_soon(app: &AppHandle) {
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

/// 背景定時更新：啟動時一次，之後每 5 分鐘
pub(crate) fn spawn_refresh_loop(app: &AppHandle) {
    let handle = app.clone();
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
                let _ = handle.run_on_main_thread(move || crate::tray::toggle_panel(&h));
            }
            first = false;
            std::thread::sleep(REFRESH_EVERY);
        }
    });
}
