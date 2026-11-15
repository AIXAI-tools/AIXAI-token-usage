// 提醒視窗（TASK-024，示意圖 notify-buttons v2）：要你動手的通知改由 AItoken 自己跳小視窗，
// 停在右下角、蓋在其他視窗上、不搶焦點，按了按鈕或 × 才關。Windows 勿擾模式擋不到（11-15 實測
// Windows 通知在勿擾下連優先 App 都不跳）。只是告知的通知照舊走通知外掛。
// 省記憶體：跟面板一樣，有卡片才建立視窗，卡片清空就整個關掉。
use crate::applog;
use crate::planview::{self, Notice, Row};
use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_notification::NotificationExt;

pub(crate) const ALERT: &str = "alert";
const WIDTH: f64 = 316.0;
/// 最多同時疊幾張
const MAX_CARDS: usize = 3;

static CARDS: Mutex<Vec<Notice>> = Mutex::new(Vec::new());

fn with_cards<T>(f: impl FnOnce(&mut Vec<Notice>) -> T) -> T {
    let mut g = CARDS.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

/// 按鈕動作
#[derive(Debug, PartialEq)]
pub(crate) enum Action {
    /// 打開官方用量頁（只接受 claude、codex）
    Open(&'static str),
    /// 手動券在某帳號按了：（登錄 id, 帳號 email）
    Used(String, String),
    Dismiss,
    Unknown,
}

pub(crate) fn parse(action: &str) -> Action {
    match action.split_once(':') {
        Some(("open", "claude")) => Action::Open("claude"),
        Some(("open", "codex")) => Action::Open("codex"),
        Some(("used", rest)) => match rest.split_once(':') {
            Some((id, account)) if !id.is_empty() && account.contains('@') && !account.contains(':') => Action::Used(id.to_string(), account.to_string()),
            _ => Action::Unknown,
        },
        None if action == "dismiss" => Action::Dismiss,
        _ => Action::Unknown,
    }
}

/// 加入或更新卡片（同 key 取代，新的放最後；超過上限丟最舊的）
pub(crate) fn upsert(cards: &mut Vec<Notice>, n: Notice) {
    cards.retain(|c| c.key != n.key);
    cards.push(n);
    while cards.len() > MAX_CARDS {
        cards.remove(0);
    }
}

/// 每次重算後檢查卡片還成不成立：券都按了／用完的帳號已恢復 → 收掉；券還在 → 換成最新內容（剩下的帳號）
pub(crate) fn revalidate(cards: &mut Vec<Notice>, rows: &[Row], now: i64, zh: bool) {
    let mut kept = Vec::new();
    for c in cards.drain(..) {
        if let Some(rest) = c.key.strip_prefix("exp:") {
            let fresh = rest.split_once(':').and_then(|(p, t)| t.parse::<i64>().ok().filter(|t| *t > now).and_then(|t| planview::expiring_notice(rows, p, t, now, zh)));
            if let Some(n) = fresh {
                kept.push(n);
            }
        } else if let Some(id) = c.key.strip_prefix("out:") {
            // 找不到這個帳號（例如換出去後還沒資料）就先留著，等你自己關
            if rows.iter().find(|r| r.id == id).map_or(true, |r| r.weekly_left <= 0.0) {
                kept.push(c);
            }
        } else {
            kept.push(c);
        }
    }
    *cards = kept;
}

/// 發一則通知：停留型進提醒視窗，其他走通知外掛（勿擾時收進通知中心）
pub(crate) fn show(app: &AppHandle, n: Notice) {
    applog::log(&format!("plan notification sent stay={} buttons={}", n.stay, n.buttons.len()));
    if n.stay {
        with_cards(|c| upsert(c, n));
        sync(app);
    } else {
        let _ = app.notification().builder().title(n.title).body(n.body).show();
    }
}

/// 重算後呼叫
pub(crate) fn on_rows(app: &AppHandle, rows: &[Row], now: i64, zh: bool) {
    let changed = with_cards(|c| {
        if c.is_empty() {
            return false;
        }
        let before = c.clone();
        revalidate(c, rows, now, zh);
        *c != before
    });
    if changed {
        sync(app);
    }
}

/// 卡片有就開視窗並通知它重畫，沒有就關掉視窗
fn sync(app: &AppHandle) {
    let empty = with_cards(|c| c.is_empty());
    let win = app.get_webview_window(ALERT);
    match (empty, win) {
        (true, Some(w)) => {
            let _ = w.close();
        }
        (true, None) => {}
        (false, Some(w)) => {
            let _ = w.emit_to(ALERT, "alerts", cards_dto());
        }
        (false, None) => open_window(app),
    }
}

fn open_window(app: &AppHandle) {
    // 可能從同步指令（主執行緒）走到這裡，例如面板登錄一張快到期的券；在主執行緒建 WebView 會卡死，另開執行緒建
    let app = app.clone();
    std::thread::spawn(move || build_window(&app));
}

fn build_window(app: &AppHandle) {
    if app.get_webview_window(ALERT).is_some() {
        let _ = app.emit_to(ALERT, "alerts", cards_dto());
        return;
    }
    let built = WebviewWindowBuilder::new(app, ALERT, WebviewUrl::App("alert.html".into()))
        .title("AIXAI Token Usage")
        .inner_size(WIDTH, 140.0)
        .visible(false)
        .decorations(false)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .shadow(true)
        // 不搶焦點：不打斷你正在打字的視窗
        .focused(false)
        .focusable(false)
        .additional_browser_args("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-gpu")
        .build();
    if let Err(e) = built {
        applog::log(&format!("alert window failed: {e}"));
    }
    // 內容畫好後由前端呼叫 alert_resize，那時才定位並顯示
}

#[derive(Serialize, Clone)]
pub(crate) struct CardDto {
    key: String,
    title: String,
    body: String,
    /// warn＝本週用完要按券（橘）；info＝券快到期（藍）
    tone: &'static str,
    buttons: Vec<ButtonDto>,
}
#[derive(Serialize, Clone)]
struct ButtonDto {
    text: String,
    action: String,
    primary: bool,
}

fn cards_dto() -> Vec<CardDto> {
    with_cards(|c| {
        c.iter()
            .map(|n| CardDto {
                key: n.key.clone(),
                title: n.title.clone(),
                body: n.body.clone(),
                tone: if n.key.starts_with("out:") { "warn" } else { "info" },
                buttons: n.buttons.iter().map(|(t, a)| ButtonDto { text: t.clone(), action: a.clone(), primary: a != "dismiss" }).collect(),
            })
            .collect()
    })
}

#[tauri::command]
pub(crate) fn get_alerts() -> Vec<CardDto> {
    cards_dto()
}

/// 前端量好內容高度後呼叫：調整大小、貼在工作區右下角、顯示（不搶焦點）
#[tauri::command]
pub(crate) fn alert_resize(app: AppHandle, height: f64) {
    let Some(win) = app.get_webview_window(ALERT) else { return };
    let monitor = win.primary_monitor().ok().flatten().or_else(|| win.current_monitor().ok().flatten());
    let Some(m) = monitor else { return };
    let scale = m.scale_factor();
    let (w, h) = ((WIDTH * scale) as u32, (height.clamp(60.0, 640.0) * scale) as u32);
    let _ = win.set_size(PhysicalSize::new(w, h));
    let area = m.work_area();
    let margin = (12.0 * scale) as i32;
    let x = area.position.x + area.size.width as i32 - w as i32 - margin;
    let y = area.position.y + area.size.height as i32 - h as i32 - margin;
    let _ = win.set_position(PhysicalPosition::new(x, y));
    let _ = win.show();
}

/// 卡片上的按鈕
#[tauri::command]
pub(crate) fn alert_action(app: AppHandle, key: String, action: String) {
    match parse(&action) {
        Action::Open(provider) => {
            applog::log("alert action=open");
            // 開頁面後卡片留著：Claude 還要回來按「○○ 按了」；Codex 按了券會自動讀到、卡片自己收
            planview::open_usage_for(provider);
        }
        Action::Used(id, account) => {
            applog::log("alert action=used");
            // 跟面板「我按了」同一條路；重算後 on_rows 會把卡片換成只剩沒按的帳號，全按完就收掉
            planview::mark_used_from_alert(&app, &id, &account);
        }
        Action::Dismiss => {
            applog::log("alert action=dismiss");
            alert_close(app, key);
        }
        Action::Unknown => applog::log("alert action=unknown"),
    }
}

/// 卡片右上角的 ×
#[tauri::command]
pub(crate) fn alert_close(app: AppHandle, key: String) {
    with_cards(|c| c.retain(|n| n.key != key));
    sync(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(key: &str) -> Notice {
        Notice { key: key.into(), title: key.into(), body: String::new(), stay: true, buttons: vec![] }
    }

    #[test]
    fn parses_actions() {
        assert_eq!(parse("open:claude"), Action::Open("claude"));
        assert_eq!(parse("open:codex"), Action::Open("codex"));
        assert_eq!(parse("used:18e7ecf6:a@example.com"), Action::Used("18e7ecf6".into(), "a@example.com".into()));
        assert_eq!(parse("dismiss"), Action::Dismiss);
        // 亂寫的一律不做事
        for bad in ["", "open:", "open:https://evil.example", "used:", "used::a@example.com", "used:id:not-an-email", "used:id:a@b.com:x", "dismiss:1", "rm -rf"] {
            assert_eq!(parse(bad), Action::Unknown, "{bad}");
        }
    }

    #[test]
    fn upsert_replaces_same_key_and_caps() {
        let mut c = vec![];
        upsert(&mut c, n("a"));
        upsert(&mut c, n("b"));
        upsert(&mut c, n("a"));
        assert_eq!(c.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        upsert(&mut c, n("c"));
        upsert(&mut c, n("d"));
        assert_eq!(c.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["a", "c", "d"]);
    }
}
