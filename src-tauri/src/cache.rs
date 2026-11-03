// 快取 cache.json：每個帳號最後一次成功的結果。
// 只存百分比、重置時間、email、查詢時間；不存 token。
use crate::model::UsageWindow;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CacheEntry {
    #[serde(default)]
    pub email: Option<String>,
    pub queried_at: i64,
    pub windows: Vec<UsageWindow>,
}

pub type Cache = HashMap<String, CacheEntry>;

pub fn load(path: &Path) -> Cache {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(path: &Path, cache: &Cache) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(path, text);
    }
}

/// 換出去的 Claude 帳號（離線帳號）：只存 email、換出前最後一次查詢時間與百分比／重置時間
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct OfflineEntry {
    pub email: String,
    pub queried_at: i64,
    pub windows: Vec<UsageWindow>,
}

pub type Offline = Vec<OfflineEntry>;

pub fn load_offline(path: &Path) -> Offline {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_offline(path: &Path, offline: &Offline) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(offline) {
        let _ = std::fs::write(path, text);
    }
}

/// 離線帳號的短代號（email 轉小寫後做 FNV-1a），讓面板不需要拿到完整 email
pub fn offline_id(email: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in email.trim().to_lowercase().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// 某個 Claude 帳號查詢成功後呼叫：`old` 是它查詢前的快取、`new_email` 是這次查到的 email。
/// - email 與舊快取不同 → 舊快取存成離線帳號（同 email 只留最新一筆）
/// - 查到的 email 等於某個離線帳號 → 移除該離線紀錄（換回來）
/// 回傳 offline 是否有變動。
pub fn track_swap(offline: &mut Offline, old: Option<&CacheEntry>, new_email: &str) -> bool {
    let before = offline.len();
    let mut changed = false;
    offline.retain(|o| !o.email.eq_ignore_ascii_case(new_email));
    if offline.len() != before {
        changed = true;
    }
    if let Some(e) = old {
        if let Some(old_email) = e.email.as_deref().filter(|m| !m.is_empty() && !m.eq_ignore_ascii_case(new_email)) {
            offline.retain(|o| !o.email.eq_ignore_ascii_case(old_email));
            offline.push(OfflineEntry { email: old_email.to_string(), queried_at: e.queried_at, windows: e.windows.clone() });
            changed = true;
        }
    }
    changed
}

/// 忘記一筆離線帳號（依短代號）。回傳是否真的刪到。
pub fn forget_offline(offline: &mut Offline, id: &str) -> bool {
    let before = offline.len();
    offline.retain(|o| offline_id(&o.email) != id);
    offline.len() != before
}

/// 快取的窗口：重置時間已過的視為已重置（剩餘 100%，標記 reset_done）
pub fn apply_reset(windows: &[UsageWindow], now: i64) -> Vec<UsageWindow> {
    windows
        .iter()
        .cloned()
        .map(|mut w| {
            if w.resets_at.is_some_and(|r| r <= now) {
                w.used_percent = 0.0;
                w.reset_done = true;
                w.resets_at = None;
            }
            w
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WindowLabel;

    fn w(used: f64, resets: Option<i64>) -> UsageWindow {
        UsageWindow::new(WindowLabel::from_minutes(300, None), used, resets)
    }

    #[test]
    fn passed_reset_becomes_full_and_marked() {
        let out = apply_reset(&[w(87.0, Some(1000)), w(40.0, Some(5000)), w(10.0, None)], 2000);
        assert_eq!(out[0].used_percent, 0.0);
        assert!(out[0].reset_done);
        assert_eq!(out[1].used_percent, 40.0);
        assert!(!out[1].reset_done);
        assert_eq!(out[2].used_percent, 10.0);
    }

    fn entry(email: &str, at: i64, used: f64) -> CacheEntry {
        CacheEntry { email: Some(email.into()), queried_at: at, windows: vec![w(used, Some(9999))] }
    }

    #[test]
    fn email_change_creates_offline_entry() {
        let mut off = Offline::new();
        // 沒換帳號、第一次查：不產生
        assert!(!track_swap(&mut off, None, "a@x.com"));
        assert!(!track_swap(&mut off, Some(&entry("a@x.com", 10, 40.0)), "A@x.com"));
        assert!(off.is_empty());
        // 換 email：舊帳號留下
        assert!(track_swap(&mut off, Some(&entry("a@x.com", 10, 40.0)), "b@x.com"));
        assert_eq!(off.len(), 1);
        assert_eq!(off[0].email, "a@x.com");
        assert_eq!(off[0].queried_at, 10);
        assert_eq!(off[0].windows[0].used_percent, 40.0);
    }

    #[test]
    fn same_email_overwrites_latest() {
        let mut off = Offline::new();
        track_swap(&mut off, Some(&entry("a@x.com", 10, 40.0)), "b@x.com");
        track_swap(&mut off, Some(&entry("c@x.com", 15, 5.0)), "a@x.com"); // a 換回，c 離線
        track_swap(&mut off, Some(&entry("a@x.com", 30, 70.0)), "c@x.com"); // 再換出 a，c 換回
        assert_eq!(off.len(), 1);
        assert_eq!(off[0].email, "a@x.com");
        assert_eq!(off[0].queried_at, 30);
        // 直接再次覆蓋同 email
        track_swap(&mut off, Some(&entry("A@x.com", 50, 80.0)), "d@x.com");
        assert_eq!(off.iter().filter(|o| o.email.eq_ignore_ascii_case("a@x.com")).count(), 1);
        assert_eq!(off.iter().find(|o| o.email.eq_ignore_ascii_case("a@x.com")).unwrap().queried_at, 50);
    }

    #[test]
    fn switching_back_removes_offline() {
        let mut off = Offline::new();
        track_swap(&mut off, Some(&entry("a@x.com", 10, 40.0)), "b@x.com");
        assert_eq!(off.len(), 1);
        // 另一個資料夾查到 a：合併回即時
        assert!(track_swap(&mut off, None, "a@x.com"));
        assert!(off.is_empty());
    }

    #[test]
    fn forget_removes_by_id() {
        let mut off = Offline::new();
        track_swap(&mut off, Some(&entry("a@x.com", 10, 40.0)), "b@x.com");
        track_swap(&mut off, Some(&entry("c@x.com", 11, 40.0)), "b@x.com");
        assert!(!forget_offline(&mut off, "nope"));
        assert!(forget_offline(&mut off, &offline_id("A@X.com")));
        assert_eq!(off.len(), 1);
        assert_eq!(off[0].email, "c@x.com");
    }

    #[test]
    fn offline_file_roundtrip_without_secrets() {
        let p = std::env::temp_dir().join(format!("aitoken-offline-{}.json", std::process::id()));
        let mut off = Offline::new();
        track_swap(&mut off, Some(&entry("a@x.com", 10, 40.0)), "b@x.com");
        save_offline(&p, &off);
        assert_eq!(load_offline(&p), off);
        assert!(!std::fs::read_to_string(&p).unwrap().to_lowercase().contains("token"));
        assert!(load_offline(&p.with_extension("none")).is_empty());
    }

    #[test]
    fn roundtrip_without_secrets() {
        let p = std::env::temp_dir().join(format!("aitoken-cache-{}.json", std::process::id()));
        let mut c = Cache::new();
        c.insert("claude:default".into(), CacheEntry { email: Some("a@b.c".into()), queried_at: 5, windows: vec![w(1.0, Some(9))] });
        save(&p, &c);
        assert_eq!(load(&p), c);
        assert!(!std::fs::read_to_string(&p).unwrap().to_lowercase().contains("token"));
        assert!(load(&p.with_extension("none")).is_empty());
    }
}
