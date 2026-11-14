//! 終端機額度那一行：依設定把 Codex／Antigravity 額度組成文字，寫成 terminal-line.json，
//! 由 Claude Code 狀態列腳本（statusline.ps1）接在 Claude 那段後面。腳本只讀檔、不解析 cache.json。
use crate::model::{now_epoch, Provider, Snapshot, UsageWindow};
use crate::state::{zh_of, AppState};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::Path;
use tauri::{AppHandle, Manager};

/// 終端機要顯示哪些（存在 terminal.json）
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TermCfg {
    #[serde(default = "yes")]
    pub claude: bool,
    #[serde(default = "yes")]
    pub codex: bool,
    #[serde(default = "yes")]
    pub agy: bool,
    /// 不顯示的 Antigravity 分組（官方組名，例 "Claude and GPT"）
    #[serde(default)]
    pub agy_off: Vec<String>,
    /// true＝顯示已用％；預設顯示剩餘％（跟面板一樣）
    #[serde(default)]
    pub used: bool,
}

fn yes() -> bool {
    true
}

impl Default for TermCfg {
    fn default() -> Self {
        Self { claude: true, codex: true, agy: true, agy_off: vec![], used: false }
    }
}

pub fn load(app_dir: &Path) -> TermCfg {
    fs::read_to_string(app_dir.join("terminal.json"))
        .ok()
        .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

fn save(app_dir: &Path, cfg: &TermCfg) -> Result<(), String> {
    fs::create_dir_all(app_dir).map_err(|_| "io")?;
    fs::write(app_dir.join("terminal.json"), serde_json::to_vec_pretty(cfg).map_err(|_| "io")?).map_err(|_| "io".into())
}

struct Words {
    five: &'static str,
    week: &'static str,
    stale: &'static str,
    join: &'static str,
}

fn words(zh: bool) -> Words {
    if zh {
        Words { five: "5h", week: "週", stale: "（舊）", join: "｜" }
    } else {
        Words { five: "5h", week: "wk", stale: " (stale)", join: " | " }
    }
}

/// 終端機上的分組短名（官方組名 → 短名）
pub fn group_label(group: &str, zh: bool) -> String {
    match group {
        "Gemini 4 Argon" => "Argon".into(),
        "Claude and GPT" => if zh { "Claude和GPT".into() } else { "Claude+GPT".into() },
        g => g.into(),
    }
}

/// 剩 30% 以下黃、10% 以下紅，其餘綠（跟面板一致）
fn paint(left: f64, s: &str) -> String {
    let code = if left < 10.0 { 31 } else if left < 30.0 { 33 } else { 32 };
    format!("\x1b[{code}m{s}\x1b[0m")
}

/// 一組窗口 → 「5h 87%·週 57%」；暫停計算的窗口不顯示
fn windows_text(ws: &[&UsageWindow], used: bool, w: &Words, color: bool, now: i64) -> String {
    let mut parts = vec![];
    for kind in ["five_hour", "weekly"] {
        let Some(win) = ws.iter().find(|x| x.label.kind == kind && !x.disabled) else { continue };
        let reset = win.reset_done || win.resets_at.is_some_and(|r| r <= now);
        let u = if reset { 0.0 } else { win.used_percent.clamp(0.0, 100.0) };
        let left = 100.0 - u;
        let num = format!("{:.0}%", if used { u } else { left }.round());
        let label = if kind == "five_hour" { w.five } else { w.week };
        parts.push(format!("{label} {}", if color { paint(left, &num) } else { num }));
    }
    parts.join("·")
}

fn pick<'a>(snap: &'a Snapshot, id: &str, prefer_key: Option<&str>) -> Option<&'a Provider> {
    let ok = |p: &&Provider| {
        p.id == id && p.visible && !p.offline && !p.windows.is_empty()
            && p.reason.as_ref().is_none_or(|r| r.key != "agy_switched")
    };
    prefer_key
        .and_then(|k| snap.providers.iter().filter(ok).find(|p| p.key == k))
        .or_else(|| snap.providers.iter().find(ok))
}

/// 一小段（不可再切開）：sep＝接在前一段後面時的分隔，cont＝換行後開頭要補的字（agy 的第二組起補「agy 」）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Piece {
    pub s: String,
    pub t: String,
    pub c: String,
}

/// Codex＋Antigravity 那段文字（不含 Claude；Claude 由腳本用當下的即時數字組）
pub fn build(snap: &Snapshot, cfg: &TermCfg, zh: bool, color: bool, now: i64) -> String {
    pieces(snap, cfg, zh, color, now).iter().enumerate().map(|(i, p)| if i == 0 { p.t.clone() } else { format!("{}{}", p.s, p.t) }).collect()
}

/// 同 build，但切成可換行的小段：終端機太窄時腳本只在段與段之間換行
pub fn pieces(snap: &Snapshot, cfg: &TermCfg, zh: bool, color: bool, now: i64) -> Vec<Piece> {
    let w = words(zh);
    let mut out = vec![];
    let piece = |s: &str, t: String, c: &str| Piece { s: s.into(), t, c: c.into() };
    if cfg.codex {
        if let Some(p) = pick(snap, "codex", Some("codex:default")) {
            let ws: Vec<&UsageWindow> = p.windows.iter().filter(|x| x.group.is_none()).collect();
            let t = windows_text(&ws, cfg.used, &w, color, now);
            if !t.is_empty() { out.push(piece(w.join, format!("Codex {t}"), "")); }
        }
    }
    if cfg.agy {
        if let Some(p) = pick(snap, "antigravity", None) {
            let mut groups: Vec<String> = vec![];
            for g in p.windows.iter().filter_map(|x| x.group.clone()) {
                if !groups.contains(&g) { groups.push(g); }
            }
            let parts: Vec<String> = groups.iter().filter(|g| !cfg.agy_off.contains(g)).filter_map(|g| {
                let ws: Vec<&UsageWindow> = p.windows.iter().filter(|x| x.group.as_deref() == Some(g)).collect();
                let t = windows_text(&ws, cfg.used, &w, color, now);
                (!t.is_empty()).then(|| format!("{} {t}", group_label(g, zh)))
            }).collect();
            for (i, t) in parts.into_iter().enumerate() {
                if i == 0 { out.push(piece(w.join, format!("agy {t}"), "")); } else { out.push(piece(" / ", t, "agy ")); }
            }
        }
    }
    out
}

/// 目前快照裡 Antigravity 的分組（官方組名），給設定頁列勾選
pub fn agy_groups(snap: &Snapshot) -> Vec<String> {
    let mut groups = vec![];
    if let Some(p) = pick(snap, "antigravity", None) {
        for g in p.windows.iter().filter_map(|x| x.group.clone()) {
            if !groups.contains(&g) { groups.push(g); }
        }
    }
    groups
}

/// 寫 terminal-line.json（腳本讀）：Claude 段的標籤、Codex／agy 的新鮮與過期兩種文字
pub fn write(app_dir: &Path, snap: &Snapshot, cfg: &TermCfg, zh: bool) {
    let now = now_epoch();
    let w = words(zh);
    let plain = build(snap, cfg, zh, false, now);
    let stale = if plain.is_empty() { String::new() } else { format!("\x1b[90m{plain}{}\x1b[0m", w.stale) };
    // 過期版：每段各自灰色，最後一段補「（舊）」
    let mut pieces_stale = pieces(snap, cfg, zh, false, now);
    let n = pieces_stale.len();
    for (i, p) in pieces_stale.iter_mut().enumerate() {
        let tail = if i + 1 == n { w.stale } else { "" };
        p.t = format!("\x1b[90m{}{tail}\x1b[0m", p.t);
        p.s = format!("\x1b[90m{}\x1b[0m", p.s);
    }
    let claude_label = match (zh, cfg.used) {
        (true, false) => "Claude 剩",
        (true, true) => "Claude 已用",
        (false, false) => "Claude left",
        (false, true) => "Claude used",
    };
    let v = json!({
        "v": 1,
        "updated_at": snap.updated_at.unwrap_or(now),
        "claude": cfg.claude,
        "used": cfg.used,
        "claude_label": claude_label,
        "l5": w.five,
        "lw": w.week,
        "dot": "·",
        "join": w.join,
        "extra": build(snap, cfg, zh, true, now),
        "extra_stale": stale,
        "pieces": pieces(snap, cfg, zh, true, now),
        "pieces_stale": pieces_stale,
    });
    let path = app_dir.join("terminal-line.json");
    let tmp = app_dir.join("terminal-line.json.tmp");
    if fs::write(&tmp, v.to_string()).is_ok() {
        let _ = fs::rename(&tmp, &path);
    }
}

/// 每次查完額度、或改了設定時呼叫
pub fn write_now(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Some(app_dir) = state.cache_path.parent() else { return };
    let zh = zh_of(&state.config.lock().unwrap());
    let snap = state.snapshot.lock().unwrap().clone();
    write(app_dir, &snap, &load(app_dir), zh);
}

#[derive(Serialize)]
pub(crate) struct TermDto {
    cfg: TermCfg,
    agy_groups: Vec<AgyGroup>,
}

#[derive(Serialize)]
struct AgyGroup {
    /// 官方組名（存進 agy_off 用）
    id: String,
    /// 顯示名
    label: String,
    /// 組內模型（設定頁小字）
    models: Option<String>,
    /// 有沒有 5 小時那條（Argon 只有每週）
    five: bool,
}

fn dto(app: &AppHandle) -> TermDto {
    let state = app.state::<AppState>();
    let zh = zh_of(&state.config.lock().unwrap());
    let snap = state.snapshot.lock().unwrap().clone();
    let cfg = state.cache_path.parent().map(load).unwrap_or_default();
    let agy = pick(&snap, "antigravity", None);
    let agy_groups = agy_groups(&snap).into_iter().map(|g| {
        let ws: Vec<&UsageWindow> = agy.map(|p| p.windows.iter().filter(|x| x.group.as_deref() == Some(g.as_str())).collect()).unwrap_or_default();
        AgyGroup {
            label: group_label(&g, zh),
            models: ws.iter().find_map(|x| x.group_models.clone()),
            five: ws.iter().any(|x| x.label.kind == "five_hour" && !x.disabled),
            id: g,
        }
    }).collect();
    TermDto { cfg, agy_groups }
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn get_terminal_line(app: AppHandle) -> TermDto {
    dto(&app)
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn set_terminal_line(app: AppHandle, cfg: TermCfg) -> Result<TermDto, String> {
    {
        let state = app.state::<AppState>();
        let app_dir = state.cache_path.parent().ok_or("io")?;
        save(app_dir, &cfg)?;
    }
    write_now(&app);
    Ok(dto(&app))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WindowLabel;

    fn win(kind_min: u64, used: f64, group: Option<&str>, disabled: bool) -> UsageWindow {
        let mut w = UsageWindow::new(WindowLabel::from_minutes(kind_min, None), used, Some(i64::MAX));
        w.group = group.map(str::to_string);
        w.disabled = disabled;
        w
    }

    fn prov(id: &str, key: &str, windows: Vec<UsageWindow>) -> Provider {
        let mut p = Provider::placeholder(id, "ok");
        p.key = key.into();
        p.windows = windows;
        p
    }

    fn snap() -> Snapshot {
        Snapshot {
            providers: vec![
                prov("codex", "codex:default", vec![win(300, 13.0, None, false), win(10080, 43.0, None, false)]),
                prov("antigravity", "antigravity:a", vec![
                    win(300, 2.4, Some("Gemini"), false),
                    win(10080, 0.7, Some("Gemini"), false),
                    win(300, 0.0, Some("Claude and GPT"), true),
                    win(10080, 100.0, Some("Claude and GPT"), false),
                    win(10080, 0.0, Some("Gemini 4 Argon"), false),
                ]),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn remaining_by_default_groups_follow_real_windows() {
        let t = build(&snap(), &TermCfg::default(), true, false, 0);
        assert_eq!(t, "Codex 5h 87%·週 57%｜agy Gemini 5h 98%·週 99% / Claude和GPT 週 0% / Argon 週 100%");
    }

    #[test]
    fn used_mode_hidden_groups_and_platforms() {
        let cfg = TermCfg { codex: false, agy_off: vec!["Claude and GPT".into()], used: true, ..Default::default() };
        assert_eq!(build(&snap(), &cfg, false, false, 0), "agy Gemini 5h 2%·wk 1% / Argon wk 0%");
        let none = TermCfg { codex: false, agy: false, ..Default::default() };
        assert_eq!(build(&snap(), &none, true, false, 0), "");
    }

    #[test]
    fn colors_follow_panel_levels() {
        let t = build(&snap(), &TermCfg { agy: false, ..Default::default() }, true, true, 0);
        assert_eq!(t, "Codex 5h \x1b[32m87%\x1b[0m·週 \x1b[32m57%\x1b[0m");
        assert_eq!(paint(29.0, "x"), "\x1b[33mx\x1b[0m");
        assert_eq!(paint(9.0, "x"), "\x1b[31mx\x1b[0m");
    }

    #[test]
    fn passed_reset_counts_as_full() {
        let mut s = snap();
        s.providers[0].windows[0].resets_at = Some(10);
        let t = build(&s, &TermCfg { agy: false, ..Default::default() }, true, false, 100);
        assert_eq!(t, "Codex 5h 100%·週 57%");
    }
}
