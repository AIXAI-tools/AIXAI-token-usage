//! 共用狀態：AppState、右鍵選單項目、語言判斷
use crate::cache::{Cache, Offline};
use crate::config::Config;
use crate::model::Snapshot;
use crate::{notify, reports, update};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::Instant;
use tauri::menu::{CheckMenuItem, MenuItem, Submenu};

/// 右鍵選單上要隨語言／狀態更新的項目
pub(crate) struct MenuItems {
    pub(crate) refresh: MenuItem<tauri::Wry>,
    pub(crate) lang_menu: Submenu<tauri::Wry>,
    pub(crate) lang_system: CheckMenuItem<tauri::Wry>,
    pub(crate) lang_zh: CheckMenuItem<tauri::Wry>,
    pub(crate) lang_en: CheckMenuItem<tauri::Wry>,
    pub(crate) autostart: CheckMenuItem<tauri::Wry>,
    pub(crate) quit: MenuItem<tauri::Wry>,
}

pub(crate) struct AppState {
    pub(crate) snapshot: Mutex<Snapshot>,
    pub(crate) refreshing: AtomicBool,
    pub(crate) last_hidden: Mutex<Option<Instant>>,
    pub(crate) config: Mutex<Config>,
    pub(crate) config_path: PathBuf,
    pub(crate) cache: Mutex<Cache>,
    pub(crate) cache_path: PathBuf,
    /// 換出去的 Claude 帳號（離線卡片）
    pub(crate) offline: Mutex<Offline>,
    pub(crate) offline_path: PathBuf,
    pub(crate) tracker: Mutex<notify::Tracker>,
    pub(crate) menu: Mutex<Option<MenuItems>>,
    pub(crate) update: Mutex<update::Shared>,
    pub(crate) reports: Mutex<reports::Store>,
    pub(crate) reports_path: PathBuf,
    /// 進設定頁前擷取的主畫面（本程式面板自己的畫面，PNG）
    pub(crate) shot: Mutex<Option<Vec<u8>>>,
}

pub(crate) fn is_zh_system() -> bool {
    sys_locale::get_locale().is_some_and(|l| l.to_lowercase().starts_with("zh"))
}

pub(crate) fn zh_of(cfg: &Config) -> bool {
    zh_for_language(&cfg.language, is_zh_system())
}

pub(crate) fn zh_for_language(language: &str, system_zh: bool) -> bool {
    match language {
        "zh-TW" => true,
        "en" => false,
        _ => system_zh,
    }
}

pub(crate) fn platform_name(id: &str) -> &'static str {
    match id {
        "codex" => "Codex",
        "claude" => "Claude",
        _ => "Antigravity",
    }
}
