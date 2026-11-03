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
