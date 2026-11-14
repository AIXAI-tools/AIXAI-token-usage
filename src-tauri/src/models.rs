// 第二波才接排程與畫面；目前對外介面供串接與離線測試使用。
#![allow(dead_code)]
use crate::model::{Reason, UsageWindow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct ModelEntry { pub id: String, pub name: String }

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PlatformModels { pub platform: String, pub models: Vec<ModelEntry> }

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuotaHint { Own { bucket: String }, Shared { group: String }, Unknown }

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct ModelChange {
    pub platform: String,
    pub added: Vec<ModelEntry>,
    pub removed: Vec<ModelEntry>,
    pub hints: Vec<(String, QuotaHint)>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WatchState {
    pub checked_at: Option<i64>,
    pub baseline: Vec<PlatformModels>,
    pub pending: Vec<ModelChange>,
}

const CLAUDE_URL: &str = "https://platform.claude.com/docs/en/models/overview";
const MAX_BODY: u64 = 2 * 1024 * 1024;

fn allowed_claude_url(url: &str) -> bool {
    // 固定 https authority；拒絕 userinfo、替代連接埠及相似網域。
    url.strip_prefix("https://platform.claude.com/").is_some()
}

fn read_html(reader: impl Read) -> Result<String, Reason> {
    let mut bytes = Vec::new();
    reader.take(MAX_BODY + 1).read_to_end(&mut bytes).map_err(|_| Reason::new("no_data"))?;
    if bytes.len() as u64 > MAX_BODY { return Err(Reason::new("response_too_large")); }
    String::from_utf8(bytes).map_err(|_| Reason::new("no_data"))
}

fn list_claude_models() -> Result<Vec<ModelEntry>, Reason> {
    if !allowed_claude_url(CLAUDE_URL) { return Err(Reason::new("blocked_host")); }
    // 不讀憑證、不加 Authorization；自動轉址關閉，改成手動跟隨「同網域」轉址最多 3 次，
    // 每次都重新檢查白名單（2026-11-14 官方把 /docs/en/about-claude/models/overview 307 轉到現址）。
    let agent = ureq::AgentBuilder::new().redirects(0).timeout(Duration::from_secs(15)).build();
    let mut url = CLAUDE_URL.to_string();
    for _ in 0..=3 {
        let response = match agent.get(&url).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(_) => return Err(Reason::new("unavailable")),
        };
        match response.status() {
            200 => return parse_claude_models(&read_html(response.into_reader())?),
            301 | 302 | 303 | 307 | 308 => {
                let next = response.header("location").map(redirect_target).ok_or_else(|| Reason::new("no_data"))?;
                if !allowed_claude_url(&next) { return Err(Reason::new("blocked_host")); }
                url = next;
            }
            _ => return Err(Reason::new("no_data")),
        }
    }
    Err(Reason::new("no_data"))
}

/// 轉址目標：絕對網址原樣（之後由白名單檢查）；以單一 / 開頭的相對路徑接在同網域後；其他形式回空字串（白名單會擋）
fn redirect_target(location: &str) -> String {
    if location.starts_with("https://") || location.starts_with("http://") {
        location.to_string()
    } else if location.starts_with('/') && !location.starts_with("//") {
        format!("https://platform.claude.com{location}")
    } else {
        String::new()
    }
}

fn entries(map: BTreeMap<String, String>) -> Result<Vec<ModelEntry>, Reason> {
    if map.is_empty() { return Err(Reason::new("no_data")); }
    Ok(map.into_iter().map(|(id, name)| ModelEntry { id, name }).collect())
}

pub(crate) fn parse_claude_models(html: &str) -> Result<Vec<ModelEntry>, Reason> {
    let mut out = BTreeMap::new();
    // 等價掃描 claude-[a-z]+-\d+(?:-\d+)*，不引入會改動 Cargo.lock 的新相依。
    for (start, _) in html.match_indices("claude-") {
        let tail = &html[start + 7..];
        let family_len = tail.bytes().take_while(u8::is_ascii_lowercase).count();
        let rest = &tail[family_len..];
        if family_len == 0 || !rest.starts_with('-') { continue; }
        let mut end = 1;
        loop {
            let digits = rest[end..].bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 { break; }
            end += digits;
            if rest.as_bytes().get(end) != Some(&b'-') || !rest.as_bytes().get(end + 1).is_some_and(u8::is_ascii_digit) { break; }
            end += 1;
        }
        if end == 1 { continue; }
        let mut version = &rest[1..end];
        if let Some((base, date)) = version.rsplit_once('-') {
            if date.len() == 8 { version = base; }
        }
        let family = &tail[..family_len];
        let title = format!("{}{}", family[..1].to_ascii_uppercase(), &family[1..]);
        out.insert(format!("claude-{family}-{version}"), format!("Claude {title} {}", version.replace('-', ".")));
    }
    entries(out)
}

pub(crate) fn collect_codex_pages(mut call: impl FnMut(u64, Option<&str>) -> Result<Value, Reason>) -> Result<Vec<ModelEntry>, Reason> {
    let mut out = BTreeMap::new();
    let mut cursor: Option<String> = None;
    for page in 0..5 {
        let response = call(page, cursor.as_deref())?;
        let data = response.get("data").and_then(Value::as_array).ok_or_else(|| Reason::new("no_data"))?;
        for model in data {
            if model.get("hidden").and_then(Value::as_bool) == Some(true) { continue; }
            let id = model.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()).ok_or_else(|| Reason::new("no_data"))?;
            let name = model.get("displayName").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(id);
            out.insert(id.to_string(), name.to_string());
        }
        cursor = match response.get("nextCursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.is_empty() => None,
            Some(Value::String(s)) => Some(s.clone()),
            _ => return Err(Reason::new("no_data")),
        };
        if cursor.is_none() { return Ok(out.into_iter().map(|(id, name)| ModelEntry { id, name }).collect()); }
    }
    // 不把被截斷的清單當成功，避免將第六頁模型誤判為消失。
    Err(Reason::new("model_page_limit"))
}

pub(crate) fn parse_agy_models(text: &str) -> Result<Vec<ModelEntry>, Reason> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let Some((id, name)) = line.split_once('\t') else { continue; };
        let (mut id, mut name) = (id.trim(), name.trim());
        if id.is_empty() || name.is_empty() || id.eq_ignore_ascii_case("id") { continue; }
        for level in ["low", "medium", "high", "xhigh", "max"] {
            if let Some(base) = id.strip_suffix(&format!("-{level}")) { id = base; break; }
        }
        for level in ["Low", "Medium", "High", "XHigh", "Xhigh", "Max"] {
            if let Some(base) = name.strip_suffix(&format!(" ({level})")) { name = base; break; }
        }
        out.insert(id.to_string(), name.to_string());
    }
    entries(out)
}

pub fn fetch_all() -> Vec<(String, Result<Vec<ModelEntry>, Reason>)> {
    vec![
        ("claude".into(), list_claude_models()),
        ("codex".into(), crate::codex::list_models(None)),
        ("antigravity".into(), crate::antigravity::list_models()),
    ]
}

pub fn apply_check(state: &mut WatchState, fetched: Vec<(String, Result<Vec<ModelEntry>, Reason>)>, windows: &[(String, Vec<UsageWindow>)], now: i64) -> Vec<ModelChange> {
    let mut changes = Vec::new();
    for (platform, result) in fetched {
        let Ok(mut models) = result else { continue; };
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models.dedup_by(|a, b| a.id == b.id);
        if let Some(old) = state.baseline.iter_mut().find(|b| b.platform == platform) {
            let added: Vec<_> = models.iter().filter(|m| !old.models.iter().any(|o| o.id == m.id)).cloned().collect();
            let removed: Vec<_> = old.models.iter().filter(|m| !models.iter().any(|n| n.id == m.id)).cloned().collect();
            let wins = platform_windows(windows, &platform);
            let hints = added.iter().map(|m| (m.id.clone(), quota_hint(&platform, m, wins))).collect();
            let change = ModelChange { platform: platform.clone(), added, removed, hints };
            if (!change.added.is_empty() || !change.removed.is_empty()) && !state.pending.iter().any(|p| same_change(p, &change)) {
                state.pending.push(change.clone());
                changes.push(change);
            }
            old.models = models;
        } else {
            state.baseline.push(PlatformModels { platform, models });
        }
    }
    state.checked_at = Some(now);
    changes
}

fn same_change(a: &ModelChange, b: &ModelChange) -> bool {
    fn ids(models: &[ModelEntry]) -> std::collections::BTreeSet<&str> { models.iter().map(|m| m.id.as_str()).collect() }
    a.platform == b.platform && ids(&a.added) == ids(&b.added) && ids(&a.removed) == ids(&b.removed)
}

fn platform_windows<'a>(windows: &'a [(String, Vec<UsageWindow>)], platform: &str) -> &'a [UsageWindow] {
    windows.iter().find(|(p, _)| p == platform).map_or(&[], |(_, w)| w.as_slice())
}

pub fn quota_hint(platform: &str, model: &ModelEntry, windows: &[UsageWindow]) -> QuotaHint {
    for w in windows {
        if platform == "claude" || platform == "codex" {
            if let Some(bucket) = w.label.bucket.as_ref().filter(|b| !b.is_empty() && model.name.to_lowercase().contains(&b.to_lowercase())) {
                return QuotaHint::Own { bucket: bucket.clone() };
            }
        } else if platform == "antigravity" {
            let name = model.name.strip_prefix("Gemini ").unwrap_or(&model.name);
            if !name.is_empty() && w.group_models.as_ref().is_some_and(|m| m.contains(name)) {
                if let Some(group) = &w.group { return QuotaHint::Shared { group: group.clone() }; }
            }
        }
    }
    QuotaHint::Unknown
}

pub fn report_text(changes: &[ModelChange], windows: &[(String, Vec<UsageWindow>)], now: i64, version: &str) -> (String, String) {
    let names: Vec<_> = changes.iter().flat_map(|c| &c.added).map(|m| m.name.as_str()).collect();
    let title = if names.is_empty() { "模型清單變動".to_string() } else { format!("新模型：{}", names.join("、")) };
    let mut body = String::new();
    for change in changes {
        let platform = match change.platform.as_str() { "claude" => "Claude", "codex" => "Codex", "antigravity" => "Antigravity", other => other };
        body.push_str(&format!("【{platform}】\n"));
        for (label, models) in [("新增", &change.added), ("消失", &change.removed)] {
            for model in models { body.push_str(&format!("{label}：{}  {}\n", model.id, model.name)); }
        }
        let wins = platform_windows(windows, &change.platform);
        let mut lines = Vec::new();
        for w in wins {
            let line = if change.platform == "antigravity" {
                w.group.as_ref().map(|g| match &w.group_models { Some(m) => format!("・{g}：{m}"), None => format!("・{g}") })
            } else {
                let duration = match w.label.kind.as_str() { "five_hour" => "5 小時".to_string(), "weekly" => "每週".to_string(), _ => format!("{} 分鐘", w.label.minutes) };
                Some(match &w.label.bucket { Some(b) => format!("{b} {duration}（獨立）"), None => duration })
            };
            if let Some(line) = line { if !lines.contains(&line) { lines.push(line); } }
        }
        if change.platform == "antigravity" {
            body.push_str("官方分組（/quota）：\n");
            body.push_str(if lines.is_empty() { "扣哪組未確認" } else { "" });
            body.push_str(&lines.join("\n"));
        } else {
            body.push_str(&format!("額度條：{}", if lines.is_empty() { "扣哪組未確認".into() } else { lines.join("、") }));
        }
        body.push_str("\n\n");
    }
    let time = chrono::DateTime::from_timestamp(now, 0)
        .and_then(|t| chrono::FixedOffset::east_opt(8 * 3600).map(|offset| t.with_timezone(&offset).format("%Y-%m-%d %H:%M").to_string()))
        .unwrap_or_else(|| now.to_string());
    body.push_str(&format!("偵測時間：{time}（台灣）\nApp 版本：{version}"));
    (title, body)
}

pub fn load_state(path: &Path) -> WatchState {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_state(path: &Path, s: &WatchState) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) { std::fs::create_dir_all(parent)?; }
    std::fs::write(path, serde_json::to_vec_pretty(s)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WindowLabel;
    use serde_json::json;

    fn model(id: &str, name: &str) -> ModelEntry { ModelEntry { id: id.into(), name: name.into() } }
    fn fetched(ids: &[&str]) -> Vec<(String, Result<Vec<ModelEntry>, Reason>)> {
        vec![("claude".into(), Ok(ids.iter().map(|id| model(id, id)).collect()))]
    }
    fn bucket(name: &str) -> UsageWindow { UsageWindow::new(WindowLabel::from_minutes(10080, Some(name.into())), 0.0, None) }

    #[test]
    fn claude_parsing_dates_deduplicated_and_names_generated() {
        let html = r#"<code>claude-opus-4-5-20251101</code> "claude-opus-4-5" claude-fable-5-2 claude-sonnet-4 claude-invalid-x"#;
        assert_eq!(parse_claude_models(html).unwrap(), vec![model("claude-fable-5-2", "Claude Fable 5.2"), model("claude-opus-4-5", "Claude Opus 4.5"), model("claude-sonnet-4", "Claude Sonnet 4")]);
        assert!(parse_claude_models("<html>error</html>").is_err());
    }

    #[test]
    fn claude_redirect_only_same_host() {
        let ok = redirect_target("/docs/en/models/overview");
        assert_eq!(ok, "https://platform.claude.com/docs/en/models/overview");
        assert!(allowed_claude_url(&ok));
        assert!(allowed_claude_url(&redirect_target("https://platform.claude.com/x")));
        assert!(!allowed_claude_url(&redirect_target("https://evil.example.com/x")));
        assert!(!allowed_claude_url(&redirect_target("//evil.example.com/x")));
        assert!(!allowed_claude_url(&redirect_target("http://platform.claude.com/x")));
        assert!(!allowed_claude_url(&redirect_target("https://platform.claude.com.evil.com/x")));
    }

    #[test]
    fn claude_host_allowlist_and_body_limit() {
        assert!(allowed_claude_url(CLAUDE_URL));
        for url in ["http://platform.claude.com/docs", "https://platform.claude.com.evil/docs", "https://platform.claude.com@evil/docs", "https://api.anthropic.com/v1/models", "https://platform.claude.com:443/docs"] { assert!(!allowed_claude_url(url)); }
        assert!(read_html(std::io::repeat(b'x').take(MAX_BODY)).is_ok());
        assert_eq!(read_html(std::io::repeat(b'x').take(MAX_BODY + 1)).unwrap_err().key, "response_too_large");
        assert!(read_html(&[255][..]).is_err());
    }

    #[test]
    fn codex_parsing_paginates_and_skips_hidden() {
        let mut calls = Vec::new();
        let models = collect_codex_pages(|page, cursor| {
            calls.push((page, cursor.map(str::to_string)));
            Ok(if page == 0 { json!({"data":[{"id":"gpt-6", "displayName":"GPT 6"},{"id":"hidden","hidden":true}],"nextCursor":"next"}) }
                else { json!({"data":[{"id":"gpt-6"},{"id":"codex","displayName":"Codex"}],"nextCursor":null}) })
        }).unwrap();
        assert_eq!(calls, vec![(0, None), (1, Some("next".into()))]);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0], model("codex", "Codex"));
        assert!(!models.iter().any(|m| m.id == "hidden"));
    }

    #[test]
    fn codex_page_limit_and_errors_never_return_partial_success() {
        let mut count = 0;
        assert_eq!(collect_codex_pages(|_, _| { count += 1; Ok(json!({"data":[],"nextCursor":"more"})) }).unwrap_err().key, "model_page_limit");
        assert_eq!(count, 5);
        assert!(collect_codex_pages(|_, _| Ok(json!({"data":null}))).is_err());
        assert!(collect_codex_pages(|_, _| Err(Reason::new("timeout"))).is_err());
        assert!(collect_codex_pages(|_, _| Ok(json!({"data":[],"nextCursor":12}))).is_err());
        assert!(collect_codex_pages(|_, _| Ok(json!({"data":[],"nextCursor":null}))).unwrap().is_empty());
        assert_eq!(collect_codex_pages(|page, _| Ok(json!({"data":[{"id":format!("model-{page}")}],"nextCursor":if page == 4 { Value::Null } else { json!("more") }}))).unwrap().len(), 5);
        assert!(collect_codex_pages(|page, _| if page == 0 { Ok(json!({"data":[{"id":"first"}],"nextCursor":"more"})) } else { Err(Reason::new("timeout")) }).is_err());
    }

    #[test]
    fn agy_parsing_effort_levels_merge_and_orchestrator_preserved() {
        let text = "id\tName\ngemini-4-hydrogen-low\tGemini 4 Hydrogen (Low)\ngemini-4-hydrogen-high\tGemini 4 Hydrogen (High)\norchestrator\tOrchestrator (Argon)\nignored line";
        assert_eq!(parse_agy_models(text).unwrap(), vec![model("gemini-4-hydrogen", "Gemini 4 Hydrogen"), model("orchestrator", "Orchestrator (Argon)")]);
        for (suffix, title) in [("low", "Low"), ("medium", "Medium"), ("high", "High"), ("xhigh", "XHigh"), ("max", "Max")] {
            assert_eq!(parse_agy_models(&format!("gemini-4-{suffix}\tGemini 4 ({title})")).unwrap(), vec![model("gemini-4", "Gemini 4")]);
        }
        assert!(parse_agy_models("not a model list").is_err());
    }

    #[test]
    fn apply_first_only_builds_baseline() {
        let mut state = WatchState::default();
        assert!(apply_check(&mut state, fetched(&["a"]), &[], 10).is_empty());
        assert_eq!(state.baseline[0].models[0].id, "a");
        assert_eq!(state.checked_at, Some(10));
        assert!(state.pending.is_empty());
    }

    #[test]
    fn apply_addition_removal_and_hints() {
        let mut state = WatchState::default();
        apply_check(&mut state, fetched(&["a"]), &[], 1);
        let changes = apply_check(&mut state, fetched(&["Fable"]), &[("claude".into(), vec![bucket("Fable")])], 2);
        assert_eq!(changes[0].added, vec![model("Fable", "Fable")]);
        assert_eq!(changes[0].removed, vec![model("a", "a")]);
        assert_eq!(changes[0].hints, vec![("Fable".into(), QuotaHint::Own { bucket: "Fable".into() })]);
        assert_eq!(state.pending, changes);
        assert_eq!(state.baseline[0].models[0].id, "Fable");
    }

    #[test]
    fn apply_failed_platform_preserves_baseline_others_continue() {
        let mut state = WatchState::default();
        apply_check(&mut state, fetched(&["a"]), &[], 1);
        assert!(apply_check(&mut state, vec![("claude".into(), Err(Reason::new("timeout"))), ("codex".into(), Ok(vec![model("b", "b")]))], &[], 2).is_empty());
        assert_eq!(state.baseline[0].models[0].id, "a");
        assert_eq!(state.baseline[1].platform, "codex");
        assert!(state.pending.is_empty());
    }

    #[test]
    fn apply_pending_not_duplicated_even_after_reversion_and_hint_change() {
        let mut state = WatchState::default();
        apply_check(&mut state, fetched(&["a"]), &[], 1);
        apply_check(&mut state, fetched(&["Fable"]), &[], 2);
        assert!(apply_check(&mut state, fetched(&["Fable"]), &[], 3).is_empty());
        apply_check(&mut state, fetched(&["a"]), &[], 4);
        assert!(apply_check(&mut state, fetched(&["Fable"]), &[("claude".into(), vec![bucket("Fable")])], 5).is_empty());
        assert_eq!(state.pending.len(), 2);
    }

    #[test]
    fn quota_hint_claude_bucket() {
        assert_eq!(quota_hint("claude", &model("fable", "Claude Fable 5.2"), &[bucket("Fable")]), QuotaHint::Own { bucket: "Fable".into() });
        assert_eq!(quota_hint("claude", &model("opus", "Claude Opus 4.5"), &[bucket("Fable")]), QuotaHint::Unknown);
    }

    #[test]
    fn quota_hint_codex_bucket() {
        assert_eq!(quota_hint("codex", &model("gpt", "GPT Pro"), &[bucket("Pro")]), QuotaHint::Own { bucket: "Pro".into() });
        assert_eq!(quota_hint("codex", &model("gpt", "GPT"), &[bucket("")]), QuotaHint::Unknown);
    }

    #[test]
    fn quota_hint_agy_official_group() {
        let mut w = bucket("");
        w.group = Some("Gemini".into());
        w.group_models = Some("3.8 Flash, 4 Hydrogen".into());
        assert_eq!(quota_hint("antigravity", &model("hydrogen", "Gemini 4 Hydrogen"), &[w.clone()]), QuotaHint::Shared { group: "Gemini".into() });
        assert_eq!(quota_hint("antigravity", &model("carbon", "Gemini 4 Carbon"), &[w]), QuotaHint::Unknown);
    }

    #[test]
    fn report_text_matches_mockup_with_groups_removals_and_taiwan_time() {
        let mut state = WatchState::default();
        apply_check(&mut state, fetched(&["old"]), &[], 0);
        let changes = apply_check(&mut state, vec![("claude".into(), Ok(vec![model("claude-fable-5-2", "Claude Fable 5.2")]))], &[], 1);
        let wins = vec![UsageWindow::new(WindowLabel::from_minutes(300, None), 0.0, None), UsageWindow::new(WindowLabel::from_minutes(10080, None), 0.0, None), bucket("Fable")];
        let (title, body) = report_text(&changes, &[("claude".into(), wins)], 0, "0.2.1");
        assert_eq!(title, "新模型：Claude Fable 5.2");
        assert_eq!(body, "【Claude】\n新增：claude-fable-5-2  Claude Fable 5.2\n消失：old  old\n額度條：5 小時、每週、Fable 每週（獨立）\n\n偵測時間：1970-01-01 08:00（台灣）\nApp 版本：0.2.1");
        let change = ModelChange { platform: "antigravity".into(), added: vec![], removed: vec![model("old", "Old")], hints: vec![] };
        let mut w = bucket(""); w.group = Some("Gemini".into()); w.group_models = Some("4 Hydrogen".into());
        let (title, body) = report_text(&[change], &[("antigravity".into(), vec![w.clone(), w])], 0, "1");
        assert_eq!(title, "模型清單變動");
        assert!(body.contains("官方分組（/quota）：\n・Gemini：4 Hydrogen\n\n"));
        assert_eq!(body.matches("・Gemini").count(), 1);
    }

    #[test]
    fn state_round_trip_missing_and_corrupt_files() {
        let path = std::env::temp_dir().join(format!("aitoken-models-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())).join("models.json");
        assert!(load_state(&path).baseline.is_empty());
        let mut state = WatchState::default();
        apply_check(&mut state, fetched(&["a"]), &[], 1);
        apply_check(&mut state, fetched(&["b"]), &[], 2);
        save_state(&path, &state).unwrap();
        assert_eq!(serde_json::to_value(load_state(&path)).unwrap(), serde_json::to_value(&state).unwrap());
        std::fs::write(&path, "invalid").unwrap();
        assert!(load_state(&path).pending.is_empty());
        save_state(&path, &state).unwrap(); // 保留測試產物，不刪檔。
    }

    #[test]
    #[ignore = "需明確執行；會連 Claude 公開頁及啟動 codex/agy 唯讀查詢"]
    fn list_models_live() {
        for (platform, result) in fetch_all() {
            match result { Ok(models) => println!("{platform}: {} models", models.len()), Err(reason) => println!("{platform}: unavailable ({})", reason.key) }
        }
    }
}
