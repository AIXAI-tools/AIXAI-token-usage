//! 系統匣圖示、右鍵選單、面板視窗建立／銷毀
use crate::model::{Provider, Snapshot};
use crate::refresh::refresh_soon;
use crate::state::{zh_of, AppState, MenuItems};
use crate::ring;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::ManagerExt;

pub(crate) const TRAY_ID: &str = "main";
pub(crate) const PANEL: &str = "panel";
/// 依實際生效語言選擇邏輯寬度；英文寬度由現有表頭與欄距量測。
pub(crate) fn panel_width(zh: bool) -> f64 {
    if zh { 336.0 } else { 440.0 }
}

/// 每個平台取所有「顯示中」帳號的最低剩餘 %
pub(crate) fn lefts(snap: &Snapshot) -> [Option<f64>; 3] {
    let get = |id: &str| {
        snap.providers
            .iter()
            .filter(|p| p.id == id && p.visible)
            .filter_map(Provider::lowest_left)
            .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| a.min(v))))
    };
    [get("codex"), get("claude"), get("antigravity")]
}

pub(crate) fn update_tray(app: &AppHandle, snap: &Snapshot) {
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
pub(crate) fn place_panel(win: &WebviewWindow) {
    let monitor = win.current_monitor().ok().flatten().or_else(|| win.primary_monitor().ok().flatten());
    let (Some(m), Ok(mut size)) = (monitor, win.outer_size()) else { return };
    let area = m.work_area();
    // 工作區比面板還小時先縮到放得下，再貼齊右下角。
    if size.width > area.size.width || size.height > area.size.height {
        if let Ok(inner) = win.inner_size() {
            let fitted = tauri::PhysicalSize::new(
                inner.width.saturating_sub(size.width.saturating_sub(area.size.width)).max(1),
                inner.height.saturating_sub(size.height.saturating_sub(area.size.height)).max(1),
            );
            if win.set_size(fitted).is_ok() {
                size.width = size.width.min(area.size.width);
                size.height = size.height.min(area.size.height);
            }
        }
    }
    let margin = (12.0 * m.scale_factor()) as i32;
    let x = panel_axis(area.position.x, area.size.width, size.width, margin);
    let y = panel_axis(area.position.y, area.size.height, size.height, margin);
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

fn panel_axis(origin: i32, available: u32, size: u32, margin: i32) -> i32 {
    origin + (available as i32 - size as i32 - margin).max(0)
}

// 省記憶體：面板（WebView2，約 65 MB）只在打開時存在；收起就整個關掉，背景只剩主程式（約 4 MB）。
// 圓環圖示、定時查詢、通知都在 Rust 端，不需要 WebView。

/// 面板已建立、但內容還沒畫好（尚未顯示）
pub(crate) static PANEL_PENDING: AtomicBool = AtomicBool::new(false);
/// 主畫面以外的頁面（設定、回報…）：點面板外面不收起，改用標題列的 ✕
pub(crate) static PANEL_STICKY: AtomicBool = AtomicBool::new(false);
/// 未送出的回報草稿（前端的 JSON，只放記憶體，結束程式就清掉）；重開面板一律從主畫面開始
pub(crate) static PANEL_DRAFT: Mutex<Option<String>> = Mutex::new(None);

/// 失焦時要不要收起：建立中不算、停在主畫面以外的頁面不收
pub(crate) fn closes_on_blur(pending: bool, sticky: bool) -> bool {
    !pending && !sticky
}

pub(crate) fn toggle_panel(app: &AppHandle) {
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

pub(crate) fn open_panel(app: &AppHandle) {
    let width = panel_width(zh_of(&app.state::<AppState>().config.lock().unwrap()));
    PANEL_PENDING.store(true, Ordering::SeqCst);
    PANEL_STICKY.store(false, Ordering::SeqCst); // 前端還原頁面後會再設定
    let built = WebviewWindowBuilder::new(app, PANEL, WebviewUrl::App("index.html".into()))
        .title("AIXAI Token Usage")
        .inner_size(width, 420.0)
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
        if matches!(e, WindowEvent::Focused(false))
            && closes_on_blur(PANEL_PENDING.load(Ordering::SeqCst), PANEL_STICKY.load(Ordering::SeqCst))
        {
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
pub(crate) fn reveal_panel(app: &AppHandle) {
    if !PANEL_PENDING.swap(false, Ordering::SeqCst) {
        return;
    }
    if let Some(win) = app.get_webview_window(PANEL) {
        place_panel(&win);
        let _ = win.show();
        let _ = win.set_focus();
    }
}

pub(crate) fn close_panel(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        *state.last_hidden.lock().unwrap() = Some(Instant::now());
    }
    PANEL_PENDING.store(false, Ordering::SeqCst);
    if let Some(win) = app.get_webview_window(PANEL) {
        let _ = win.destroy();
    }
}

// ---------- 右鍵選單 ----------

pub(crate) fn apply_menu_language(app: &AppHandle) {
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

pub(crate) fn change_language(app: &AppHandle, lang: &str) {
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
    if let Some(win) = app.get_webview_window(PANEL) {
        let width = panel_width(zh_of(&app.state::<AppState>().config.lock().unwrap()));
        if let (Ok(size), Ok(scale)) = (win.inner_size(), win.scale_factor()) {
            let _ = win.set_size(tauri::LogicalSize::new(width, size.height as f64 / scale));
            place_panel(&win);
        }
    }
    let _ = app.emit_to(PANEL, "settings-changed", ());
}

pub(crate) fn toggle_autostart(app: &AppHandle) {
    let al = app.autolaunch();
    if al.is_enabled().unwrap_or(false) {
        let _ = al.disable();
    } else {
        let _ = al.enable();
    }
    apply_menu_language(app);
    let _ = app.emit_to(PANEL, "settings-changed", ());
}

/// 建立右鍵選單與系統匣圖示（setup 內呼叫）
pub(crate) fn build_tray(app: &mut tauri::App, zh: bool, lang: &str) -> tauri::Result<()> {
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_follows_effective_language() {
        use crate::state::zh_for_language;
        assert_eq!(panel_width(zh_for_language("zh-TW", false)), 336.0);
        assert_eq!(panel_width(zh_for_language("en", true)), 440.0);
        assert_eq!(panel_width(zh_for_language("system", true)), 336.0);
        assert_eq!(panel_width(zh_for_language("system", false)), 440.0);
    }

    #[test]
    fn placement_stays_in_work_area() {
        for (origin, available, size, margin) in [
            (0, 1920, 428, 12), (-1920, 1920, 642, 18),
            (1920, 900, 856, 24), (0, 428, 428, 12), (0, 900, 900, 12),
        ] {
            let position = panel_axis(origin, available, size, margin);
            assert!(position >= origin);
            assert!(position + size as i32 <= origin + available as i32);
        }
    }

    #[test]
    fn blur_closes_only_main_view_after_shown() {
        assert!(closes_on_blur(false, false)); // 主畫面：點外面收起
        assert!(!closes_on_blur(false, true)); // 設定、回報…：點外面不收
        assert!(!closes_on_blur(true, false)); // 建立中的失焦不算
    }
}
