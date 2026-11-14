//! 線上更新：檢查、下載、安裝與相關指令
use crate::model::now_epoch;
use crate::state::AppState;
use crate::tray::PANEL;
use crate::{applog, config, update};
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

pub(crate) const UPDATE_EVENT: &str = "update-state";
pub(crate) const UPDATE_EVERY: Duration = Duration::from_secs(24 * 3600);

pub(crate) fn emit_update(app: &AppHandle) {
    let s = app.state::<AppState>().update.lock().unwrap().state.clone();
    let _ = app.emit_to(PANEL, UPDATE_EVENT, s);
}

pub(crate) fn update_dir() -> Option<PathBuf> {
    config::local_dir().map(|d| d.join("update"))
}

/// 查 GitHub 最新版，結果存在 AppState（失敗靜默，只寫 log）
pub(crate) fn check_update(app: &AppHandle) -> update::UpdateState {
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

pub(crate) fn set_install_stage(app: &AppHandle, stage: &str, percent: u8) {
    {
        let st = app.state::<AppState>();
        let mut u = st.update.lock().unwrap();
        u.state.stage = stage.into();
        u.state.percent = percent;
    }
    emit_update(app);
}

pub(crate) fn run_install(app: &AppHandle, rel: &update::Release) -> Result<(), String> {
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
pub(crate) fn clear_update_dir(dir: &std::path::Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_file() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

pub(crate) fn start_install(app: &AppHandle) {
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
pub(crate) fn get_update_state(state: State<AppState>) -> update::UpdateState {
    state.update.lock().unwrap().state.clone()
}

#[tauri::command]
pub(crate) async fn check_update_cmd(app: AppHandle) -> Result<update::UpdateState, String> {
    tauri::async_runtime::spawn_blocking(move || check_update(&app)).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn install_update_cmd(app: AppHandle) {
    start_install(&app);
}

/// 用預設瀏覽器開發佈頁（網址寫死，不接受前端傳入）
#[tauri::command]
pub(crate) fn open_releases_page() {
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


/// 更新檢查：啟動後 1 分鐘，之後每 24 小時（失敗靜默）。AITOKEN_UPDATE_FIRST_CHECK_SECS 供測試縮短
pub(crate) fn spawn_check_loop(app: &AppHandle) {
    let uh = app.clone();
    std::thread::spawn(move || {
        let first = std::env::var("AITOKEN_UPDATE_FIRST_CHECK_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(60);
        std::thread::sleep(Duration::from_secs(first));
        loop {
            check_update(&uh);
            std::thread::sleep(UPDATE_EVERY);
        }
    });
}
