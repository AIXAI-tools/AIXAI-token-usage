// 通知判斷：任一窗口剩餘跌破門檻通知一次（同一窗口重置前不重複）；窗口重置時通知一次。
// 只用即時查到的資料判斷（快取不觸發通知）。狀態只放記憶體。
use crate::model::{format_remaining, UsageWindow, WindowLabel};
use std::collections::HashMap;

/// 重置通知只在「重置前已用掉一半以上」時發，避免幾乎沒用的窗口一直打擾
const RESET_MIN_USED: f64 = 50.0;

#[derive(Debug, PartialEq)]
pub enum NoticeKind {
    Low { left: f64, resets_in: Option<i64> },
    Reset,
}

#[derive(Debug, PartialEq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub label: WindowLabel,
    pub group: Option<String>,
}

struct WinState {
    low_notified: bool,
    resets_at: Option<i64>,
    used: f64,
}

#[derive(Default)]
pub struct Tracker {
    states: HashMap<String, WinState>,
}

impl Tracker {
    pub fn evaluate(&mut self, account_key: &str, windows: &[UsageWindow], threshold: u8, on_reset: bool, now: i64) -> Vec<Notice> {
        let mut out = vec![];
        for w in windows.iter().filter(|w| !w.disabled) {
            let id = format!(
                "{account_key}|{}|{}|{}",
                w.label.kind,
                w.label.bucket.as_deref().unwrap_or(""),
                w.group.as_deref().unwrap_or("")
            );
            let left = (100.0 - w.used_percent).clamp(0.0, 100.0);
            let st = self.states.entry(id).or_insert(WinState { low_notified: false, resets_at: None, used: w.used_percent });
            // 上次記下的重置時間已過 → 這個窗口重置過了
            if st.resets_at.is_some_and(|r| r <= now) {
                st.low_notified = false;
                if on_reset && st.used >= RESET_MIN_USED {
                    out.push(Notice { kind: NoticeKind::Reset, label: w.label.clone(), group: w.group.clone() });
                }
            }
            if !st.low_notified && left < threshold as f64 {
                st.low_notified = true;
                out.push(Notice {
                    kind: NoticeKind::Low { left, resets_in: w.resets_at.map(|r| r - now) },
                    label: w.label.clone(),
                    group: w.group.clone(),
                });
            }
            st.resets_at = w.resets_at;
            st.used = w.used_percent;
        }
        out
    }
}

fn label_text(zh: bool, l: &WindowLabel, group: &Option<String>) -> String {
    let base = match (l.kind.as_str(), zh) {
        ("five_hour", true) => "5 小時".to_string(),
        ("five_hour", false) => "5-hour".to_string(),
        ("weekly", true) => "每週".to_string(),
        ("weekly", false) => "weekly".to_string(),
        (_, true) => format!("{} 分鐘", l.minutes),
        (_, false) => format!("{} min", l.minutes),
    };
    let mut parts = vec![];
    if let Some(g) = group {
        parts.push(g.clone());
    }
    if let Some(b) = &l.bucket {
        parts.push(b.clone());
    }
    parts.push(base);
    parts.join(" · ")
}

/// 組出通知的標題與內文。account＝面板上的組名（例「Claude · 個人」）
pub fn message(zh: bool, account: &str, n: &Notice) -> (String, String) {
    let label = label_text(zh, &n.label, &n.group);
    match &n.kind {
        NoticeKind::Low { left, resets_in } => {
            let pct = left.round() as i64;
            let tail = resets_in.map(|s| {
                let t = format_remaining(s);
                if zh { format!("，{t} 後重置") } else { format!(", resets in {t}") }
            });
            let tail = tail.unwrap_or_default();
            if zh {
                ("額度快用完了".into(), format!("{account} 的 {label} 只剩 {pct}%{tail}"))
            } else {
                ("Quota running low".into(), format!("{account}: {label} has {pct}% left{tail}"))
            }
        }
        NoticeKind::Reset => {
            if zh {
                ("額度已重置".into(), format!("{account} 的 {label} 已重置"))
            } else {
                ("Quota reset".into(), format!("{account}: {label} has been reset"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(used: f64, resets: Option<i64>) -> UsageWindow {
        UsageWindow::new(WindowLabel::from_minutes(300, None), used, resets)
    }

    #[test]
    fn low_notifies_once_until_reset() {
        let mut t = Tracker::default();
        // 剩 5% < 10：通知一次
        let n = t.evaluate("a", &[win(95.0, Some(2000))], 10, true, 1000);
        assert_eq!(n.len(), 1);
        assert!(matches!(n[0].kind, NoticeKind::Low { .. }));
        // 同一窗口再查：不重複
        assert!(t.evaluate("a", &[win(96.0, Some(2000))], 10, true, 1300).is_empty());
        // 重置時間已過、用量掉回去：發重置通知，且不再是低額度
        let n = t.evaluate("a", &[win(2.0, Some(20000))], 10, true, 2100);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].kind, NoticeKind::Reset);
        // 新一輪又跌破門檻：可以再通知
        let n = t.evaluate("a", &[win(95.0, Some(20000))], 10, true, 3000);
        assert_eq!(n.len(), 1);
        assert!(matches!(n[0].kind, NoticeKind::Low { .. }));
    }

    #[test]
    fn healthy_window_is_silent_and_reset_toggle_respected() {
        let mut t = Tracker::default();
        assert!(t.evaluate("a", &[win(30.0, Some(2000))], 10, true, 1000).is_empty());
        // 用量沒過半的窗口重置不通知
        assert!(t.evaluate("a", &[win(0.0, Some(9000))], 10, true, 2500).is_empty());
        let mut t = Tracker::default();
        t.evaluate("a", &[win(80.0, Some(2000))], 10, false, 1000);
        // 關掉重置通知
        assert!(t.evaluate("a", &[win(0.0, Some(9000))], 10, false, 2500).is_empty());
    }

    #[test]
    fn disabled_windows_and_accounts_are_independent() {
        let mut t = Tracker::default();
        let mut d = win(100.0, None);
        d.disabled = true;
        assert!(t.evaluate("a", &[d], 10, true, 1000).is_empty());
        assert_eq!(t.evaluate("a", &[win(99.0, None)], 10, true, 1000).len(), 1);
        assert_eq!(t.evaluate("b", &[win(99.0, None)], 10, true, 1000).len(), 1);
    }

    #[test]
    fn message_text() {
        let n = Notice { kind: NoticeKind::Low { left: 7.2, resets_in: Some(3600 + 540) }, label: WindowLabel::from_minutes(300, None), group: None };
        let (_, body) = message(true, "Claude · 個人", &n);
        assert_eq!(body, "Claude · 個人 的 5 小時 只剩 7%，1h9m 後重置");
        let (title, _) = message(false, "Codex", &Notice { kind: NoticeKind::Reset, label: WindowLabel::from_minutes(10080, None), group: None });
        assert_eq!(title, "Quota reset");
    }
}
