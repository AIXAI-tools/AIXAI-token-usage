//! 模型監看：排程、持久化、前端約定與最小附件自動回報。
use crate::config::Config;
use crate::model::{now_epoch, UsageWindow};
use crate::models::{self, ModelChange, WatchState};
use crate::state::{zh_of, AppState};
use crate::{applog, feedback, reporting, reports, shot, update};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

const INTERVAL: i64 = 12 * 3600;

struct Watch {
    state: Mutex<WatchState>,
    checking: Mutex<()>,
    path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct WatchDto {
    enabled: bool,
    auto_send: bool,
    checked_at: Option<i64>,
    counts: BTreeMap<String, usize>,
    pending: Vec<ModelChange>,
}

fn dto(cfg: &Config, state: &WatchState) -> WatchDto {
    WatchDto {
        enabled: cfg.model_watch_enabled,
        auto_send: cfg.model_watch_auto_send,
        checked_at: state.checked_at,
        counts: state.baseline.iter().map(|p| (p.platform.clone(), p.models.len())).collect(),
        pending: state.pending.clone(),
    }
}

fn due(enabled: bool, checked_at: Option<i64>, now: i64) -> bool {
    enabled && checked_at.is_none_or(|last| now.saturating_sub(last) >= INTERVAL)
}

#[tauri::command]
pub(crate) fn get_model_watch(app: AppHandle) -> WatchDto {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap();
    let watch = app.state::<Watch>();
    let current = watch.state.lock().unwrap();
    dto(&cfg, &current)
}

fn emit(app: &AppHandle) {
    let _ = app.emit("model-watch", get_model_watch(app.clone()));
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn set_model_watch(app: AppHandle, enabled: Option<bool>, auto_send: Option<bool>) -> Result<WatchDto, String> {
    {
        let state = app.state::<AppState>();
        let mut cfg = state.config.lock().unwrap();
        let mut next = cfg.clone();
        if let Some(value) = enabled { next.model_watch_enabled = value; }
        if let Some(value) = auto_send { next.model_watch_auto_send = value; }
        next.save(&state.config_path).map_err(|e| e.to_string())?;
        *cfg = next;
    }
    emit(&app);
    Ok(get_model_watch(app))
}

#[tauri::command]
pub(crate) async fn check_models_now(app: AppHandle) -> Result<WatchDto, String> {
    tauri::async_runtime::spawn_blocking(move || check(&app, true)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn dismiss_model_change(app: AppHandle, platform: String) -> Result<(), String> {
    {
        let watch = app.state::<Watch>();
        let mut current = watch.state.lock().unwrap();
        let mut next = current.clone();
        next.pending.retain(|p| p.platform != platform);
        models::save_state(&watch.path, &next).map_err(|e| e.to_string())?;
        *current = next;
    }
    emit(&app);
    Ok(())
}

type Windows = Vec<(String, Vec<UsageWindow>)>;

// 只複製目前帳號快取的額度窗口，不讀憑證，也不帶 email 或帳號路徑。
fn windows(app: &AppHandle) -> Windows {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap();
    let cache = state.cache.lock().unwrap();
    let mut out: BTreeMap<String, Vec<UsageWindow>> = BTreeMap::new();
    for account in &cfg.accounts {
        if let Some(entry) = cache.get(&account.id) {
            out.entry(account.provider.clone()).or_default().extend(entry.windows.clone());
        }
    }
    out.into_iter().collect()
}

#[derive(Serialize)]
pub(crate) struct ReportText { title: String, body: String }

#[tauri::command]
pub(crate) fn report_model_change(app: AppHandle) -> ReportText {
    let pending = app.state::<Watch>().state.lock().unwrap().pending.clone();
    let (title, body) = models::report_text(&pending, &windows(&app), now_epoch(), update::current_version());
    ReportText { title, body }
}

fn auto_request(changes: &[ModelChange], windows: &Windows, now: i64) -> feedback::ReportRequest {
    let (title, body) = models::report_text(changes, windows, now, update::current_version());
    feedback::ReportRequest {
        kind: "idea".into(), title, body, attach_sys: true, want_reply: true,
        ..Default::default()
    }
}

// 注入送出函式，測試不會碰任何回報端點。成功才產生「我的回報」記錄。
fn send_batch(req: &feedback::ReportRequest, diag: &feedback::Diagnostics, now: i64,
    send: impl FnOnce(&feedback::ReportRequest, &feedback::Diagnostics) -> Result<String, String>,
) -> Result<reports::Entry, String> {
    feedback::validate(req)?;
    let id = send(req, diag)?;
    Ok(reports::Entry { id, title: req.title.trim().into(), ts: now, want_reply: req.want_reply, ..Default::default() })
}

fn clear_batch(state: &mut WatchState, changes: &[ModelChange]) {
    state.pending.retain(|p| !changes.contains(p));
}

fn notification_text(changes: &[ModelChange], sent: bool, zh: bool) -> (String, String) {
    let count: usize = changes.iter().map(|p| p.added.len()).sum();
    let mut lines: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for change in changes {
        let name = match change.platform.as_str() { "claude" => "Claude", "codex" => "Codex", "antigravity" => "Antigravity", p => p };
        lines.entry(name).or_default().extend(change.added.iter().map(|m| m.name.as_str()));
    }
    // 跟介面語言一致：英文介面不跳中文通知
    let mut body: Vec<_> = lines.into_iter().map(|(p, names)| if zh {
        format!("{p}：{}", if names.is_empty() { "模型清單有變動".into() } else { names.join("、") })
    } else {
        format!("{p}: {}", if names.is_empty() { "model list changed".into() } else { names.join(", ") })
    }).collect();
    body.push(match (zh, sent) {
        (true, true) => "已自動回報給開發者",
        (true, false) => "打開面板可回報給開發者",
        (false, true) => "Reported to the developer automatically",
        (false, false) => "Open the panel to report it to the developer",
    }.into());
    let title = if zh { format!("發現 {count} 個新模型") } else { format!("{count} new model(s) found") };
    (title, body.join("\n"))
}

fn check(app: &AppHandle, force: bool) -> Result<WatchDto, String> {
    let watch = app.state::<Watch>();
    // 手動與排程共用鎖，避免同一批重複寄出；不鎖住 UI 讀取狀態。
    let _checking = watch.checking.lock().unwrap();
    let before = get_model_watch(app.clone());
    if !before.enabled || (!force && !due(before.enabled, before.checked_at, now_epoch())) {
        return Ok(before);
    }
    let fetched = models::fetch_all();
    let windows = windows(app);
    let now = now_epoch();
    let changes = {
        let mut current = watch.state.lock().unwrap();
        let mut next = current.clone();
        let changes = models::apply_check(&mut next, fetched, &windows, now);
        models::save_state(&watch.path, &next).map_err(|e| e.to_string())?;
        *current = next;
        changes
    };
    emit(app);
    if !changes.is_empty() {
        let state = app.state::<AppState>();
        let cfg = state.config.lock().unwrap().clone();
        let mut sent = false;
        if cfg.model_watch_auto_send {
            let req = auto_request(&changes, &windows, now);
            // 不使用手動回報的 build_diagnostics，避免讀取紀錄檔或截圖。
            let diag = feedback::Diagnostics {
                sys: feedback::sys_text(update::current_version(), &shot::windows_version(), if zh_of(&cfg) { "zh-TW" } else { "en" }),
                ..Default::default()
            };
            match send_batch(&req, &diag, now, feedback::submit) {
                Ok(entry) => {
                    {
                        let mut store = state.reports.lock().unwrap();
                        store.add(entry);
                        store.save(&state.reports_path);
                    }
                    sent = true;
                    reporting::emit_reports(app);
                    let result = {
                        let mut current = watch.state.lock().unwrap();
                        let mut next = current.clone();
                        clear_batch(&mut next, &changes);
                        models::save_state(&watch.path, &next).map(|()| *current = next)
                    };
                    emit(app);
                    if let Err(e) = result { applog::log(&format!("model watch save failed: {e}")); }
                }
                Err(e) => applog::log(&format!("model watch auto report failed: {e}")),
            }
        }
        let (title, body) = notification_text(&changes, sent, zh_of(&cfg));
        let _ = app.notification().builder().title(title).body(body).show();
    }
    Ok(get_model_watch(app.clone()))
}

pub(crate) fn start(app: &AppHandle) {
    let path = app.state::<AppState>().cache_path.with_file_name("models.json");
    app.manage(Watch { state: Mutex::new(models::load_state(&path)), checking: Mutex::new(()), path });
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(120));
        loop {
            if let Err(e) = check(&app, false) { applog::log(&format!("model watch check failed: {e}")); }
            std::thread::sleep(Duration::from_secs(INTERVAL as u64));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ModelEntry, PlatformModels, QuotaHint};
    use serde_json::json;

    fn change() -> ModelChange {
        ModelChange { platform: "claude".into(), added: vec![ModelEntry { id: "claude-fable-5-2".into(), name: "Claude Fable 5.2".into() }], removed: vec![], hints: vec![("claude-fable-5-2".into(), QuotaHint::Own { bucket: "Fable".into() })] }
    }

    #[test]
    fn schedule_skips_recent_and_disabled() {
        assert!(due(true, None, 100));
        assert!(!due(false, None, 100));
        assert!(!due(false, Some(0), INTERVAL));
        assert!(!due(true, Some(100), 100 + INTERVAL - 1));
        assert!(due(true, Some(100), 100 + INTERVAL));
        assert!(!due(true, Some(200), 100));
    }

    #[test]
    fn notification_manual_auto_and_failed_send() {
        let changes = vec![change()];
        assert_eq!(notification_text(&changes, false, true), ("發現 1 個新模型".into(), "Claude：Claude Fable 5.2\n打開面板可回報給開發者".into()));
        assert_eq!(notification_text(&changes, true, true).1, "Claude：Claude Fable 5.2\n已自動回報給開發者");
        let mut multi = changes;
        let mut codex = change(); codex.platform = "codex".into();
        multi.push(codex);
        assert_eq!(notification_text(&multi, false, true).0, "發現 2 個新模型");
        assert!(notification_text(&multi, false, true).1.contains("\nCodex："));
        assert_eq!(notification_text(&multi, true, false), ("2 new model(s) found".into(), "Claude: Claude Fable 5.2\nCodex: Claude Fable 5.2\nReported to the developer automatically".into()));
    }

    #[test]
    fn auto_request_only_sys_and_want_with_fake_transport() {
        let req = auto_request(&[change()], &vec![], 0);
        assert_eq!(req.kind, "idea");
        assert!(req.attach_sys && req.want_reply);
        assert!(!req.attach_status && !req.attach_log && !req.attach_shot);
        assert!(req.contact.is_empty() && req.ref_id.is_none());
        let diag = feedback::Diagnostics { sys: "測試系統".into(), ..Default::default() };
        let entry = send_batch(&req, &diag, 123, |r, d| {
            assert!(d.log.is_empty() && d.status.is_empty() && d.shot.is_none());
            assert!(feedback::compose_body(r, d).contains("測試系統"));
            Ok("0123456789abcdef01234567".into())
        }).unwrap();
        let mut store = reports::Store::default();
        store.add(entry);
        assert_eq!(store.reports[0].ts, 123);
        assert_eq!(store.reports[0].title, req.title);
        assert_eq!(store.pending_ids(123), vec!["0123456789abcdef01234567"]);
        assert!(send_batch(&req, &diag, 123, |_, _| Err("測試失敗".into())).is_err());
    }

    #[test]
    fn get_model_watch_json_matches_contract() {
        let state = WatchState { checked_at: Some(1794592320), baseline: vec![PlatformModels { platform: "claude".into(), models: change().added }], pending: vec![change()] };
        assert_eq!(serde_json::to_value(dto(&Config::default(), &state)).unwrap(), json!({
            "enabled": true, "auto_send": false, "checked_at": 1794592320, "counts": {"claude":1},
            "pending": [{"platform":"claude", "added":[{"id":"claude-fable-5-2","name":"Claude Fable 5.2"}], "removed":[], "hints":[["claude-fable-5-2",{"kind":"own","bucket":"Fable"}]]}]
        }));
        let empty = serde_json::to_value(dto(&Config::default(), &WatchState::default())).unwrap();
        assert_eq!(empty["checked_at"], json!(null));
        assert_eq!(empty["counts"], json!({}));
        assert_eq!(empty["pending"], json!([]));
        assert_eq!(serde_json::to_value(ReportText {title:"標題".into(), body:"內容".into()}).unwrap(), json!({"title":"標題","body":"內容"}));
    }

    #[test]
    fn successful_batch_clears_only_sent_pending_failure_keeps_all() {
        let batch = vec![change()];
        let mut previous = change(); previous.platform = "codex".into();
        let mut state = WatchState { pending: vec![previous.clone(), change()], ..Default::default() };
        let req = auto_request(&batch, &vec![], 0);
        let diag = feedback::Diagnostics::default();
        let before = state.pending.clone();
        let failed = send_batch(&req, &diag, 0, |_, _| Err("離線".into()));
        if failed.is_ok() { clear_batch(&mut state, &batch); }
        assert_eq!(state.pending, before);
        let sent = send_batch(&req, &diag, 0, |_, _| Ok("0123456789abcdef01234567".into()));
        if sent.is_ok() { clear_batch(&mut state, &batch); }
        assert_eq!(state.pending, vec![previous]);
    }
}
