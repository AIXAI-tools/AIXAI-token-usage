// 傳給面板的資料格式（與 src/main.ts 的型別對應）
use serde::{Deserialize, Serialize};

#[derive(Serialize, Clone, Debug)]
pub struct Reason {
    pub key: String,
    pub detail: Option<String>,
}

impl Reason {
    pub fn new(key: &str) -> Self {
        Self { key: key.into(), detail: None }
    }
    pub fn error(detail: impl Into<String>) -> Self {
        Self { key: "error".into(), detail: Some(detail.into()) }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WindowLabel {
    /// five_hour | weekly | minutes
    pub kind: String,
    pub minutes: u64,
    /// 額度桶名稱；主要桶（codex）為 None
    pub bucket: Option<String>,
}

impl WindowLabel {
    pub fn from_minutes(minutes: u64, bucket: Option<String>) -> Self {
        let kind = match minutes {
            300 => "five_hour",
            10080 => "weekly",
            _ => "minutes",
        };
        Self { kind: kind.into(), minutes, bucket }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UsageWindow {
    pub label: WindowLabel,
    pub used_percent: f64,
    pub resets_at: Option<i64>,
    /// 官方暫停計算（agy 的 disabled 窗口）
    #[serde(default)]
    pub disabled: bool,
    /// 快取的重置時間已過：視為已重置（剩餘 100%）
    #[serde(default)]
    pub reset_done: bool,
    /// Antigravity 官方分組名稱與組內模型（逗號分隔）
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub group_models: Option<String>,
}

impl UsageWindow {
    pub fn new(label: WindowLabel, used_percent: f64, resets_at: Option<i64>) -> Self {
        Self { label, used_percent, resets_at, disabled: false, reset_done: false, group: None, group_models: None }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Provider {
    /// 平台：codex | claude | antigravity
    pub id: String,
    /// 帳號唯一鍵（config 裡的帳號 id）
    pub key: String,
    /// 使用者取的名稱（可為空）
    pub name: String,
    pub visible: bool,
    pub removable: bool,
    /// ok | unavailable
    pub status: String,
    /// live | local_log | cache
    pub source: Option<String>,
    pub as_of: Option<i64>,
    pub reason: Option<Reason>,
    pub account_masked: Option<String>,
    pub account_full: Option<String>,
    pub windows: Vec<UsageWindow>,
}

impl Provider {
    pub fn placeholder(id: &str, status: &str) -> Self {
        Self {
            id: id.into(),
            key: String::new(),
            name: String::new(),
            visible: true,
            removable: false,
            status: status.into(),
            source: None,
            as_of: None,
            reason: None,
            account_masked: None,
            account_full: None,
            windows: vec![],
        }
    }

    /// 該帳號「最緊」的那個額度還剩幾 %（沒有資料回傳 None）。暫停計算的窗口不算；已切換掉的 agy 舊帳號不算。
    pub fn lowest_left(&self) -> Option<f64> {
        if self.status != "ok" || self.reason.as_ref().is_some_and(|r| r.key == "agy_switched") {
            return None;
        }
        self.windows
            .iter()
            .filter(|w| !w.disabled)
            .map(|w| (100.0 - w.used_percent).clamp(0.0, 100.0))
            .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| a.min(v))))
    }
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Snapshot {
    pub updated_at: Option<i64>,
    pub refreshing: bool,
    pub providers: Vec<Provider>,
}

/// 遮罩 email：someone@gmail.com → some•••@gmail.com
pub fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let keep = if local.chars().count() > 4 { 4 } else { 1 };
            let head: String = local.chars().take(keep).collect();
            format!("{head}•••@{domain}")
        }
        None => "•••".into(),
    }
}

/// 重置倒數文字：不到 1 小時 `9m`；不到 1 天 `1h9m`；1 天以上 `2d11h`
pub fn format_remaining(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h{}m", secs / 3600, secs % 3600 / 60)
    } else {
        format!("{}d{}h", secs / 86400, secs % 86400 / 3600)
    }
}

pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_long_and_short() {
        assert_eq!(mask_email("someone@gmail.com"), "some•••@gmail.com");
        assert_eq!(mask_email("abc@x.com"), "a•••@x.com");
        assert_eq!(mask_email("no-at-sign"), "•••");
    }

    #[test]
    fn remaining_format() {
        assert_eq!(format_remaining(9 * 60 + 30), "9m");
        assert_eq!(format_remaining(3600 + 9 * 60), "1h9m");
        assert_eq!(format_remaining(2 * 86400 + 11 * 3600 + 59), "2d11h");
        assert_eq!(format_remaining(-5), "0m");
    }

    #[test]
    fn lowest_left_picks_tightest() {
        let mut p = Provider::placeholder("codex", "ok");
        for (u, m) in [(6.0, 300), (93.0, 10080)] {
            p.windows.push(UsageWindow { label: WindowLabel::from_minutes(m, None), used_percent: u, resets_at: None, disabled: false, reset_done: false, group: None, group_models: None });
        }
        assert_eq!(p.lowest_left(), Some(7.0));
        assert_eq!(Provider::placeholder("claude", "unavailable").lowest_left(), None);
        let mut d = UsageWindow::new(WindowLabel::from_minutes(300, None), 99.0, None);
        d.disabled = true;
        p.windows.push(d);
        assert_eq!(p.lowest_left(), Some(7.0));
    }
}
