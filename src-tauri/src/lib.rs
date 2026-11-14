mod antigravity;
mod applog;
mod cache;
mod claude;
mod codex;
mod commands;
mod config;
mod feedback;
mod model;
mod models;
mod notify;
mod refresh;
mod reporting;
mod reports;
mod ring;
mod shot;
mod snapshot;
mod state;
mod statusline;
mod tray;
mod update;
mod updater;
mod watch;

use config::Config;
use model::{now_epoch, Provider, Snapshot};
use snapshot::{from_cache, with_offline};
use state::{zh_of, AppState};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use tauri::{Manager, RunEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

pub fn run() {
    tauri::Builder::default()
        // 只允許一份：重複開啟時改為打開原本那份的面板
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if !app.get_webview_window(tray::PANEL).is_some_and(|w| w.is_visible().unwrap_or(false)) {
                tray::toggle_panel(app);
            }
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None::<Vec<&'static str>>))
        .invoke_handler(tauri::generate_handler![
            statusline::get_claude_statusline,
            statusline::install_claude_statusline,
            statusline::set_claude_statusline_show,
            commands::get_snapshot,
            commands::refresh,
            commands::panel_resize,
            commands::get_settings,
            commands::start_view,
            commands::set_account,
            commands::remove_account,
            commands::set_offline_visible,
            commands::forget_offline,
            commands::set_notify,
            commands::set_language,
            commands::detect_accounts,
            commands::add_account_hint,
            commands::open_powershell_login,
            updater::get_update_state,
            updater::check_update_cmd,
            updater::install_update_cmd,
            updater::open_releases_page,
            reporting::capture_panel_shot,
            reporting::report_prepare,
            reporting::report_preview,
            reporting::report_send,
            reporting::report_list,
            reporting::report_mark_seen,
            reporting::report_check_cmd,
            reporting::report_unread,
            watch::get_model_watch,
            watch::set_model_watch,
            watch::check_models_now,
            watch::dismiss_model_change,
            watch::report_model_change
        ])
        .setup(|app| {
            // 設定與快取資料夾
            let dir = config::data_dir(app.path().app_config_dir().ok()).ok_or("no config dir")?;
            let config_path = dir.join("config.json");
            let cache_path = dir.join("cache.json");
            let offline_path = dir.join("offline.json");
            // Claude 狀態列腳本：已安裝的舊版換成這一版
            if statusline::refresh_script(&dir) {
                applog::log("claude statusline script updated");
            }
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
            if let Some(d) = updater::update_dir() {
                updater::clear_update_dir(&d);
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

            tray::build_tray(app, zh, &lang)?;

            updater::spawn_check_loop(app.handle());
            reporting::spawn_reply_loop(app.handle());
            refresh::spawn_refresh_loop(app.handle());
            watch::start(app.handle());
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
