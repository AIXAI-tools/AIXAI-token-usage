// 我的回報：本機 reports.json 記錄送出過的回報與開發者回覆。
// 查回覆只送回報編號（96-bit 隨機碼），只查勾了「接收回覆」且 90 天內的回報；失敗靜默。
use crate::feedback;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub const REPLY_DAYS: i64 = 90;
const MAX_IDS: usize = 20;
const MAX_ENTRIES: usize = 100;
const TEXT_MAX: usize = 2000;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Reply {
    pub rid: String,
    /// fixed | test | info | wontfix
    pub kind: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub created: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub ts: i64,
    pub want_reply: bool,
    #[serde(default)]
    pub ref_id: Option<String>,
    #[serde(default)]
    pub replies: Vec<Reply>,
    /// 已讀的回覆 rid
    #[serde(default)]
    pub seen: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Store {
    #[serde(default)]
    pub reports: Vec<Entry>,
}

impl Store {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, s);
        }
    }

    pub fn add(&mut self, e: Entry) {
        self.reports.insert(0, e);
        self.reports.truncate(MAX_ENTRIES);
    }

    pub fn unread_count(&self) -> usize {
        self.reports.iter().map(|e| e.replies.iter().filter(|r| !e.seen.contains(&r.rid)).count()).sum()
    }

    pub fn mark_seen(&mut self, id: &str) {
        if let Some(e) = self.reports.iter_mut().find(|e| e.id == id) {
            e.seen = e.replies.iter().map(|r| r.rid.clone()).collect();
        }
    }

    /// 要查回覆的編號：勾了接收回覆、90 天內、最新優先、最多 20 個
    pub fn pending_ids(&self, now: i64) -> Vec<String> {
        self.reports
            .iter()
            .filter(|e| e.want_reply && now - e.ts < REPLY_DAYS * 86400 && feedback::is_report_id(&e.id))
            .take(MAX_IDS)
            .map(|e| e.id.clone())
            .collect()
    }

    /// 合併查到的回覆，回傳新增的則數
    pub fn apply_replies(&mut self, found: HashMap<String, Vec<Reply>>) -> usize {
        let mut added = 0;
        for e in self.reports.iter_mut() {
            if let Some(list) = found.get(&e.id) {
                for r in list {
                    if !e.replies.iter().any(|x| x.rid == r.rid) {
                        added += 1;
                    }
                }
                e.replies = list.clone();
            }
        }
        added
    }
}

pub fn query_url(ids: &[String]) -> String {
    format!("{}?action=replies&tool={}&ids={}", feedback::ENDPOINT, feedback::TOOL, ids.join(","))
}

/// 解析 replies 回應；只收我們問過的編號，欄位清理（kind 白名單、urls 只留 http(s)、text 截斷）
pub fn parse_replies(body: &str, asked: &[String]) -> Result<HashMap<String, Vec<Reply>>, String> {
    let v: serde_json::Value = serde_json::from_str(body.trim_start_matches('\u{feff}')).map_err(|_| "bad_json".to_string())?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        return Err("rejected".into());
    }
    let mut out = HashMap::new();
    let Some(map) = v.get("replies").and_then(|r| r.as_object()) else { return Ok(out) };
    for (id, list) in map {
        if !asked.contains(id) {
            continue;
        }
        let Some(arr) = list.as_array() else { continue };
        let mut rs = vec![];
        for item in arr {
            let s = |k: &str| item.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let rid = s("rid");
            if rid.is_empty() {
                continue;
            }
            let kind = match s("kind").as_str() {
                k @ ("fixed" | "test" | "info" | "wontfix") => k.to_string(),
                _ => "info".to_string(),
            };
            let urls = item
                .get("urls")
                .and_then(|u| u.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str()).filter(|u| u.starts_with("https://") || u.starts_with("http://")).take(5).map(|u| u.chars().take(300).collect()).collect())
                .unwrap_or_default();
            let created = match item.get("created") {
                Some(serde_json::Value::String(c)) => c.clone(),
                Some(serde_json::Value::Number(n)) => n.to_string(),
                _ => String::new(),
            };
            rs.push(Reply { rid, kind, version: s("version").chars().take(32).collect(), text: s("text").chars().take(TEXT_MAX).collect(), urls, created });
        }
        if !rs.is_empty() {
            out.insert(id.clone(), rs);
        }
    }
    Ok(out)
}

/// 實際查詢（只送編號）
pub fn fetch_replies(ids: &[String]) -> Result<HashMap<String, Vec<Reply>>, String> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let body = feedback::get_follow(&query_url(ids))?;
    parse_replies(&body, ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbb";

    fn entry(id: &str, ts: i64, want: bool) -> Entry {
        Entry { id: id.into(), title: "t".into(), ts, want_reply: want, ..Default::default() }
    }

    #[test]
    fn parses_and_cleans_replies() {
        let body = format!(
            r#"{{"ok":true,"replies":{{"{A}":[{{"rid":"r1","kind":"fixed","version":"0.2.0","text":"hi","urls":["https://x.io/a","javascript:alert(1)","ftp://x"],"created":"2026-11-03"}},{{"rid":"r2","kind":"weird","text":"x"}}],"{B}":[{{"rid":"z","kind":"info"}}],"cccccccccccccccccccccccc":[{{"rid":"q"}}]}}}}"#
        );
        let r = parse_replies(&body, &[A.to_string(), B.to_string()]).unwrap();
        assert_eq!(r.len(), 2); // 沒問過的 id 被丟掉
        let a = &r[A];
        assert_eq!(a[0].kind, "fixed");
        assert_eq!(a[0].version, "0.2.0");
        assert_eq!(a[0].urls, vec!["https://x.io/a".to_string()]);
        assert_eq!(a[1].kind, "info");
        assert!(parse_replies(r#"{"ok":false}"#, &[]).is_err());
        assert!(parse_replies("<html>", &[]).is_err());
        assert!(parse_replies(r#"{"ok":true}"#, &[]).unwrap().is_empty());
    }

    #[test]
    fn pending_ids_filter() {
        let now = 1_000_000_000;
        let mut s = Store::default();
        s.add(entry(A, now - 10, true));
        s.add(entry(B, now - 10, false)); // 沒勾接收
        s.add(entry("cccccccccccccccccccccccc", now - 91 * 86400, true)); // 超過 90 天
        s.add(entry("bad", now, true)); // id 格式不對
        assert_eq!(s.pending_ids(now), vec![A.to_string()]);
    }

    #[test]
    fn query_url_sends_only_ids() {
        let u = query_url(&[A.to_string(), B.to_string()]);
        assert!(u.starts_with(feedback::ENDPOINT));
        assert!(u.ends_with(&format!("&tool=AIXAI-token-usage&ids={A},{B}")) || u.contains(&format!("ids={A},{B}")));
        assert!(feedback::url_allowed(&u));
    }

    #[test]
    fn unread_and_seen() {
        let mut s = Store::default();
        s.add(entry(A, 1, true));
        let mut m = HashMap::new();
        m.insert(A.to_string(), vec![Reply { rid: "r1".into(), kind: "fixed".into(), ..Default::default() }]);
        assert_eq!(s.apply_replies(m.clone()), 1);
        assert_eq!(s.unread_count(), 1);
        assert_eq!(s.apply_replies(m), 0); // 同一則不重複算
        s.mark_seen(A);
        assert_eq!(s.unread_count(), 0);
    }

    #[test]
    fn store_roundtrip() {
        let dir = std::env::temp_dir().join(format!("aitoken-rep-test-{}", std::process::id()));
        let p = dir.join("reports.json");
        let mut s = Store::default();
        s.add(entry(A, 5, true));
        s.save(&p);
        assert_eq!(Store::load(&p).reports.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
