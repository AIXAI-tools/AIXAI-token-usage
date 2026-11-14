//! Claude 手動重置券的純資料操作。
use crate::plan_types::{ManualCredit, ResetCredit};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub fn load(path: &Path) -> Vec<ManualCredit> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// 以同目錄暫存檔完整寫入後再原子改名。
pub fn save(path: &Path, list: &[ManualCredit]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_vec_pretty(list)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_extension(format!("tmp-{}-{sequence:x}", std::process::id()));
    let result = (|| {
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

pub fn add(list: &mut Vec<ManualCredit>, mut c: ManualCredit) -> String {
    c.accounts = normalize_unique(c.accounts);
    c.used_by = normalize_unique(c.used_by);
    let id = format!("{:x}{:x}", timestamp_nanos(), NEXT_ID.fetch_add(1, Ordering::Relaxed));
    c.id = id.clone();
    list.push(c);
    id
}

pub fn remove(list: &mut Vec<ManualCredit>, id: &str) -> bool {
    let old_len = list.len();
    list.retain(|c| c.id != id);
    old_len != list.len()
}

pub fn mark_used(list: &mut Vec<ManualCredit>, id: &str, account: &str, now: i64) -> bool {
    let account = account.trim().to_lowercase();
    if account.is_empty() {
        return false;
    }
    let Some(credit) = list.iter_mut().find(|c| c.id == id) else { return false };
    if !credit.accounts.iter().any(|a| a.trim().eq_ignore_ascii_case(&account))
        || credit.used_by.iter().any(|a| a.trim().eq_ignore_ascii_case(&account))
    {
        return false;
    }
    credit.used_at.insert(account.clone(), now);
    credit.used_by.push(account);
    true
}

/// 「我按了」按錯，取消
pub fn unmark_used(list: &mut Vec<ManualCredit>, id: &str, account: &str) -> bool {
    let account = account.trim().to_lowercase();
    let Some(credit) = list.iter_mut().find(|c| c.id == id) else { return false };
    let before = credit.used_by.len();
    credit.used_by.retain(|a| !a.trim().eq_ignore_ascii_case(&account));
    credit.used_at.remove(&account);
    before != credit.used_by.len()
}

/// 只清掉過期的；全部帳號都按過的券留到到期（才能「取消」按錯的）
pub fn prune_expired(list: &mut Vec<ManualCredit>, now: i64) -> usize {
    let old_len = list.len();
    list.retain(|c| !c.expires_at.is_some_and(|expires| expires <= now));
    old_len - list.len()
}

pub fn expand(list: &[ManualCredit], now: i64) -> Vec<ResetCredit> {
    let mut expanded = Vec::new();
    for c in list {
        if c.expires_at.is_some_and(|expires| expires <= now) {
            continue;
        }
        for account in &c.accounts {
            let account = account.trim().to_lowercase();
            if account.is_empty() || c.used_by.iter().any(|used| used.trim().eq_ignore_ascii_case(&account)) {
                continue;
            }
            expanded.push(ResetCredit {
                id: format!("{}:{account}", c.id),
                provider: c.provider.clone(),
                account,
                kind: c.kind.clone(),
                model: c.model.clone(),
                max_refund_percent: c.max_refund_percent,
                refill_five_hour: c.refill_five_hour,
                expires_at: c.expires_at,
                source: "manual".into(),
                title: None,
            });
        }
    }
    expanded
}

pub fn validate(c: &ManualCredit) -> Result<(), String> {
    if !matches!(c.kind.as_str(), "full" | "weekly" | "five_hour" | "model") {
        return Err("kind 必須是 full、weekly、five_hour 或 model".into());
    }
    if c.kind == "model" && c.model.as_deref().is_none_or(|m| m.trim().is_empty()) {
        return Err("kind=model 必須提供 model".into());
    }
    if c.accounts.iter().all(|a| a.trim().is_empty()) {
        return Err("accounts 不可為空".into());
    }
    Ok(())
}

fn normalize_unique(values: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let value = value.trim().to_lowercase();
        if !value.is_empty() && !out.iter().any(|item: &String| item == &value) {
            out.push(value);
        }
    }
    out
}

fn timestamp_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credit(id: &str, accounts: &[&str], expires_at: Option<i64>) -> ManualCredit {
        ManualCredit {
            id: id.into(), provider: "claude".into(), kind: "full".into(), model: None,
            max_refund_percent: None, refill_five_hour: true, expires_at,
            accounts: accounts.iter().map(|s| (*s).into()).collect(), used_by: Vec::new(), used_at: Default::default(),
        }
    }

    #[test]
    fn add_normalizes_deduplicates_and_generates_id() {
        let mut list = Vec::new();
        let id = add(&mut list, credit("ignored", &[" A@X.COM ", "a@x.com", "B@X.COM"], None));
        assert!(!id.is_empty());
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].accounts, ["a@x.com", "b@x.com"]);
    }

    #[test]
    fn validate_rejects_invalid_kind_missing_model_and_empty_accounts() {
        assert!(validate(&credit("x", &["a@x"], None)).is_ok());
        let mut c = credit("x", &["a@x"], None);
        c.kind = "model".into();
        assert!(validate(&c).is_err());
        c.model = Some("fable".into());
        assert!(validate(&c).is_ok());
        c.kind = "unknown".into();
        assert!(validate(&c).is_err());
        c.kind = "full".into(); c.accounts.clear();
        assert!(validate(&c).is_err());
    }

    #[test]
    fn expand_returns_only_unused_accounts_with_scoped_ids() {
        let mut c = credit("m1", &["A@X.COM", "b@x.com"], Some(200));
        c.used_by.push("a@x.com".into());
        let out = expand(&[c.clone()], 100);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "m1:b@x.com");
        assert_eq!(out[0].account, "b@x.com");
        assert_eq!(out[0].source, "manual");
        assert!(expand(&[c], 200).is_empty());
    }

    #[test]
    fn mark_unmark_remove_and_prune_expired() {
        let mut list = vec![credit("one", &["a@x", "b@x"], Some(500)), credit("two", &["c@x"], None), credit("old", &["d@x"], Some(10))];
        assert!(mark_used(&mut list, "one", "A@X", 7));
        assert_eq!(list[0].used_at.get("a@x"), Some(&7));
        assert!(!mark_used(&mut list, "one", "a@x", 8));
        assert!(!mark_used(&mut list, "missing", "a@x", 8));
        assert!(mark_used(&mut list, "two", "c@x", 9));
        // 按錯可以取消
        assert!(unmark_used(&mut list, "two", "C@X"));
        assert!(list[1].used_by.is_empty() && list[1].used_at.is_empty());
        assert!(!unmark_used(&mut list, "two", "c@x"));
        // 全部按過的券也留到到期，只清掉過期的
        assert!(mark_used(&mut list, "two", "c@x", 9));
        assert_eq!(prune_expired(&mut list, 10), 1);
        assert_eq!(list.len(), 2);
        assert!(remove(&mut list, "one"));
        assert!(!remove(&mut list, "one"));
    }

    #[test]
    fn load_save_missing_and_corrupt_files() {
        let base = std::env::temp_dir().join(format!("aitoken-credits-{}-{}", std::process::id(), NEXT_ID.fetch_add(1, Ordering::Relaxed)));
        let path = base.join("reset-credits.json");
        assert!(load(&path).is_empty());
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(&path, "not json").unwrap();
        assert!(load(&path).is_empty());
        let list = vec![credit("m", &["a@x"], None)];
        save(&path, &list).unwrap();
        assert_eq!(load(&path), list);
    }
}
