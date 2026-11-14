//! 回報：診斷擷取、送出、回覆查詢與相關指令
use crate::model::now_epoch;
use crate::state::{zh_of, AppState};
use crate::tray::PANEL;
use crate::{applog, feedback, reports, shot, update};
use serde::Serialize;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

pub(crate) const REPLY_EVERY: Duration = Duration::from_secs(6 * 3600);
pub(crate) const REPORTS_EVENT: &str = "reports-changed";

pub(crate) fn emit_reports(app: &AppHandle) {
    let unread = app.state::<AppState>().reports.lock().unwrap().unread_count() > 0;
    let _ = app.emit_to(PANEL, REPORTS_EVENT, unread);
}

/// 查回覆（只送編號）；失敗靜默寫 log。回傳新增則數
pub(crate) fn check_replies(app: &AppHandle) -> usize {
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
pub(crate) fn capture_panel_shot(app: AppHandle) -> bool {
    let Some(win) = app.get_webview_window(PANEL) else { return false };
    let Ok(h) = win.hwnd() else { return false };
    let png = shot::capture_window_png(h.0 as isize);
    let ok = png.is_some();
    *app.state::<AppState>().shot.lock().unwrap() = png;
    applog::log(&format!("panel shot captured={ok}"));
    ok
}

#[derive(Serialize)]
pub(crate) struct PrepareDto {
    sys: String,
    status: String,
    log: String,
    log_kb: u64,
    shot_b64: Option<String>,
    shot_kb: u64,
}

pub(crate) fn build_diagnostics(app: &AppHandle) -> feedback::Diagnostics {
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
pub(crate) fn report_prepare(app: AppHandle) -> PrepareDto {
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
pub(crate) fn report_preview(app: AppHandle, req: feedback::ReportRequest) -> String {
    feedback::compose_body(&req, &build_diagnostics(&app))
}

#[tauri::command]
pub(crate) async fn report_send(app: AppHandle, req: feedback::ReportRequest) -> Result<String, String> {
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
pub(crate) struct ReplyDto {
    #[serde(flatten)]
    reply: reports::Reply,
    unread: bool,
    /// fixed 且帶的版本比目前新 → 面板顯示「更新到 vX」
    can_update: bool,
}

#[derive(Serialize)]
pub(crate) struct ReportDto {
    id: String,
    title: String,
    ts: i64,
    want_reply: bool,
    replies: Vec<ReplyDto>,
}

pub(crate) fn report_dtos(app: &AppHandle) -> Vec<ReportDto> {
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
pub(crate) fn report_list(app: AppHandle) -> Vec<ReportDto> {
    report_dtos(&app)
}

#[tauri::command]
pub(crate) fn report_mark_seen(app: AppHandle, id: String) {
    {
        let state = app.state::<AppState>();
        let mut st = state.reports.lock().unwrap();
        st.mark_seen(&id);
        st.save(&state.reports_path);
    }
    emit_reports(&app);
}

#[tauri::command]
pub(crate) async fn report_check_cmd(app: AppHandle) -> Result<Vec<ReportDto>, String> {
    let a = app.clone();
    tauri::async_runtime::spawn_blocking(move || check_replies(&a)).await.map_err(|e| e.to_string())?;
    Ok(report_dtos(&app))
}

#[tauri::command]
pub(crate) fn report_unread(state: State<AppState>) -> bool {
    state.reports.lock().unwrap().unread_count() > 0
}


/// 查開發者回覆：啟動後 2 分鐘，之後每 6 小時（失敗靜默）。AITOKEN_REPLY_FIRST_CHECK_SECS 供測試縮短
pub(crate) fn spawn_reply_loop(app: &AppHandle) {
    let rh = app.clone();
    std::thread::spawn(move || {
        let first = std::env::var("AITOKEN_REPLY_FIRST_CHECK_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(120);
        std::thread::sleep(Duration::from_secs(first));
        loop {
            check_replies(&rh);
            std::thread::sleep(REPLY_EVERY);
        }
    });
}
