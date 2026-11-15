//! Provider 組裝、快取／離線卡片
use crate::cache::{self, Cache, CacheEntry, Offline};
use crate::config::AccountCfg;
use crate::model::{mask_email, now_epoch, Provider, Reason, UsageWindow};
use crate::state::AppState;
use crate::{antigravity, applog, claude, codex};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

// ---------- 組出每個帳號的結果 ----------

/// 一次查詢的結果：成功＝(窗口, 來源)；失敗＝原因
pub(crate) struct Fetched {
    email: Option<String>,
    as_of: Option<i64>,
    outcome: Result<(Vec<UsageWindow>, &'static str), Reason>,
}

pub(crate) fn base_provider(a: &AccountCfg, removable: bool) -> Provider {
    let mut p = Provider::placeholder(&a.provider, "unavailable");
    p.key = a.id.clone();
    p.name = a.name.clone();
    p.visible = a.visible;
    p.removable = removable;
    p
}

pub(crate) fn set_email(p: &mut Provider, email: Option<String>) {
    p.account_masked = email.as_deref().map(mask_email);
    p.account_full = email;
}

/// 只用快取組出結果（隱藏的帳號、切換掉的 agy 舊帳號、剛啟動時用）
pub(crate) fn from_cache(a: &AccountCfg, removable: bool, cache: &Cache, reason: Option<Reason>, now: i64) -> Provider {
    let mut p = base_provider(a, removable);
    match cache.get(&a.id) {
        Some(e) => {
            p.status = "ok".into();
            p.source = Some("cache".into());
            p.as_of = Some(e.queried_at);
            p.windows = cache::apply_reset(&e.windows, now);
            if a.provider == "claude" {
                p.windows = cache::hide_idle_extras(p.windows);
            }
            p.reason = reason;
            set_email(&mut p, e.email.clone());
        }
        None => p.reason = Some(reason.unwrap_or_else(|| Reason::new("no_data"))),
    }
    p
}

pub(crate) fn cfg_dir(a: &AccountCfg) -> Option<PathBuf> {
    match a.provider.as_str() {
        "claude" => a.dir.as_ref().map(PathBuf::from).or_else(claude::default_dir),
        "codex" => a.dir.as_ref().map(PathBuf::from),
        _ => None,
    }
}

fn fetch_platform_in(app_dir: &Path, a: &AccountCfg) -> Fetched {
    match a.provider.as_str() {
        "codex" => {
            let dir = cfg_dir(a);
            let p = codex::fetch(dir.as_deref());
            let email = p.account_full.clone();
            if p.status == "ok" {
                let src = if p.source.as_deref() == Some("local_log") { "local_log" } else { "live" };
                Fetched { email, as_of: None, outcome: Ok((p.windows, src)) }
            } else {
                Fetched { email, as_of: None, outcome: Err(p.reason.unwrap_or_else(|| Reason::new("no_data"))) }
            }
        }
        "claude" => {
            let dir = cfg_dir(a).unwrap_or_default();
            let r = claude::fetch(app_dir, &a.id, &dir);
            let src = if r.live { "live" } else { "statusline" };
            Fetched { email: r.email, as_of: r.captured_at, outcome: r.windows.map(|w| (w, src)) }
        }
        _ => {
            let r = antigravity::fetch();
            Fetched { email: r.email, as_of: None, outcome: r.windows.map(|w| (w, "live")) }
        }
    }
}

/// 查詢一個帳號；成功且為即時資料時更新快取；失敗時退回快取
pub(crate) fn fetch_account(a: &AccountCfg, removable: bool, cache: &mut Cache, offline: &mut Offline, now: i64) -> Provider {
    // 保留 refresh 呼叫介面；Windows 資料根目錄與 Tauri app_config_dir 相同。
    let app_dir = crate::config::data_dir(std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("tools.aixai.token-usage")));
    let Some(app_dir) = app_dir else { return from_cache(a, removable, cache, Some(Reason::new("no_data")), now) };
    fetch_account_in(&app_dir, a, removable, cache, offline, now)
}

fn fetch_account_in(app_dir: &Path, a: &AccountCfg, removable: bool, cache: &mut Cache, offline: &mut Offline, now: i64) -> Provider {
    let f = fetch_platform_in(app_dir, a);
    match f.outcome {
        Ok((windows, source)) => {
            let mut p = base_provider(a, removable);
            p.status = "ok".into();
            p.source = Some(source.into());
            p.as_of = Some(f.as_of.unwrap_or(now));
            // Claude：同一個資料夾換了帳號（/login）→ 舊帳號留成離線卡片；換回來 → 移除離線紀錄
            if a.provider == "claude" && matches!(source, "live" | "statusline") {
                if let Some(new_email) = f.email.as_deref() {
                    if cache::track_swap(offline, cache.get(&a.id), new_email) {
                        applog::log(&format!("claude offline accounts changed: now {}", offline.len()));
                    }
                }
            }
            let email = f.email.or_else(|| cache.get(&a.id).and_then(|e| e.email.clone()));
            if matches!(source, "live" | "statusline") {
                cache.insert(a.id.clone(), CacheEntry { email: email.clone(), queried_at: p.as_of.unwrap_or(now), windows: windows.clone() });
            }
            set_email(&mut p, email);
            p.windows = windows;
            p
        }
        // /login 換了帳號、新帳號還沒回應過：舊帳號 → 離線卡片；新帳號若有離線時的數字就先拿來顯示（標「已換帳號」）
        Err(reason) if reason.key == "claude_statusline_switched" => {
            if let Some(new_email) = f.email.as_deref() {
                let back = offline.iter().find(|o| o.email.eq_ignore_ascii_case(new_email)).cloned();
                if cache::track_swap(offline, cache.get(&a.id), new_email) {
                    applog::log(&format!("claude offline accounts changed: now {}", offline.len()));
                }
                match back {
                    Some(o) => { cache.insert(a.id.clone(), CacheEntry { email: Some(o.email), queried_at: o.queried_at, windows: o.windows }); }
                    // 快取還是舊帳號的就拿掉；已經是新帳號的（上一輪放進去的）保留
                    None => {
                        if cache.get(&a.id).and_then(|e| e.email.as_deref()).is_some_and(|m| !m.eq_ignore_ascii_case(new_email)) {
                            cache.remove(&a.id);
                        }
                    }
                }
            }
            let mut p = from_cache(a, removable, cache, Some(reason), now);
            if p.account_full.is_none() {
                set_email(&mut p, f.email);
            }
            p
        }
        // 還沒設定狀態列／等待第一筆資料：不退回快取（快取可能是舊 token 做法留下的數字），
        // 讓卡片顯示「需要設定」「等待資料」
        Err(reason) if matches!(reason.key.as_str(), "claude_statusline_missing" | "claude_statusline_waiting") => {
            let mut p = base_provider(a, removable);
            p.reason = Some(reason);
            set_email(&mut p, f.email);
            p
        }
        Err(reason) => {
            let mut p = from_cache(a, removable, cache, Some(reason), now);
            if p.account_full.is_none() {
                set_email(&mut p, f.email);
            }
            p
        }
    }
}

/// 離線帳號卡片（Claude）：重置時間到的窗口恢復 100%；不發通知、不進圓環
pub(crate) fn offline_provider(e: &cache::OfflineEntry, hidden: &[String], now: i64) -> Provider {
    let id = cache::offline_id(&e.email);
    let mut p = Provider::placeholder("claude", "ok");
    p.visible = !hidden.contains(&id);
    p.key = format!("offline:{id}");
    p.source = Some("offline".into());
    p.as_of = Some(e.queried_at);
    p.account_masked = Some(mask_email(&e.email));
    p.windows = cache::hide_idle_extras(cache::apply_reset(&e.windows, now));
    p.offline = true;
    p
}

/// 把離線卡片接在最後一個 Claude 帳號後面
pub(crate) fn with_offline(mut providers: Vec<Provider>, offline: &Offline, hidden: &[String], now: i64) -> Vec<Provider> {
    providers.retain(|p| !p.offline);
    let at = providers.iter().rposition(|p| p.id == "claude").map_or(providers.len(), |i| i + 1);
    let offs: Vec<Provider> = offline.iter().map(|e| offline_provider(e, hidden, now)).collect();
    providers.splice(at..at, offs);
    providers
}

pub(crate) fn agy_id(email: &Option<String>) -> String {
    format!("antigravity:{}", email.as_deref().unwrap_or("default"))
}

/// 目前設定裡的帳號清單（含 agy 目前登入的帳號）
pub(crate) fn current_accounts(app: &AppHandle) -> (Vec<AccountCfg>, String) {
    let state = app.state::<AppState>();
    let agy_current = agy_id(&antigravity::active_email());
    let mut cfg = state.config.lock().unwrap();
    if !cfg.accounts.iter().any(|a| a.id == agy_current) {
        let email = agy_current.strip_prefix("antigravity:").unwrap_or("default").to_string();
        cfg.accounts.push(AccountCfg { id: agy_current.clone(), provider: "antigravity".into(), dir: Some(email), name: String::new(), visible: true, extra: false });
        cfg.sort_accounts();
        let _ = cfg.save(&state.config_path);
    }
    (cfg.accounts.clone(), agy_current)
}

pub(crate) fn is_removable(a: &AccountCfg, agy_current: &str) -> bool {
    a.extra || (a.provider == "antigravity" && a.id != agy_current)
}

/// 離線帳號卡片資料變動後，重建畫面上的離線卡片
pub(crate) fn rebuild_offline_cards(app: &AppHandle) {
    let state = app.state::<AppState>();
    let hidden = state.config.lock().unwrap().offline_hidden.clone();
    let offline = state.offline.lock().unwrap().clone();
    let mut snap = state.snapshot.lock().unwrap();
    snap.providers = with_offline(std::mem::take(&mut snap.providers), &offline, &hidden, now_epoch());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model;

    #[test]
    fn statusline_caches_capture_time_and_tracks_login_swap() {
        let root = std::env::temp_dir().join(format!("aitoken-snapshot-{}-{}", std::process::id(), now_epoch()));
        let app = root.join("app");
        let dir = root.join("claude");
        std::fs::create_dir_all(&dir).unwrap();
        let a = AccountCfg { id: "claude:test".into(), provider: "claude".into(), dir: Some(dir.to_string_lossy().into()), name: String::new(), visible: true, extra: true };
        let path = crate::statusline::status_path(&app, &a.id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let write = |email: &str, at: i64| std::fs::write(&path, serde_json::json!({
            "v":1,"captured_at":at,"email":email,
            "five_hour":{"used_percentage":46,"resets_at":i64::MAX},
            "seven_day":{"used_percentage":20,"resets_at":i64::MAX}
        }).to_string()).unwrap();
        let mut cache = Cache::new();
        let mut offline = Offline::new();
        write("first@example.invalid", 123);
        let p = fetch_account_in(&app, &a, true, &mut cache, &mut offline, 1000);
        assert_eq!(p.source.as_deref(), Some("statusline"));
        assert_eq!(p.as_of, Some(123));
        assert_eq!(p.windows.len(), 2);
        assert_eq!(p.account_full.as_deref(), Some("first@example.invalid"));
        assert_eq!(cache[&a.id].queried_at, 123);
        cache::save(&app.join("cache.json"), &cache);
        assert_eq!(cache::load(&app.join("cache.json")), cache);
        write("second@example.invalid", 456);
        fetch_account_in(&app, &a, true, &mut cache, &mut offline, 2000);
        assert_eq!(offline.len(), 1);
        assert_eq!(offline[0].email, "first@example.invalid");
        assert_eq!(offline[0].queried_at, 123);
        assert_eq!(offline[0].windows[0].used_percent, 46.0);
        cache::save_offline(&app.join("offline.json"), &offline);
        assert_eq!(cache::load_offline(&app.join("offline.json")), offline);
        write("first@example.invalid", 789);
        fetch_account_in(&app, &a, true, &mut cache, &mut offline, 3000);
        assert_eq!(offline.len(), 1);
        assert_eq!(offline[0].email, "second@example.invalid");
        std::fs::write(&path, "broken").unwrap();
        let p = fetch_account_in(&app, &a, true, &mut cache, &mut offline, 4000);
        assert_eq!(p.source.as_deref(), Some("cache"));
        assert_eq!(p.as_of, Some(789));
    }

    #[test]
    fn login_switch_detected_before_first_reply() {
        let root = std::env::temp_dir().join(format!("aitoken-snapshot-switch-{}-{}", std::process::id(), now_epoch()));
        let app = root.join("app");
        let dir = root.join("claude");
        std::fs::create_dir_all(&dir).unwrap();
        let a = AccountCfg { id: "claude:test".into(), provider: "claude".into(), dir: Some(dir.to_string_lossy().into()), name: String::new(), visible: true, extra: true };
        let path = crate::statusline::status_path(&app, &a.id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let status = |email: &str, used: f64| std::fs::write(&path, serde_json::json!({
            "v":1,"captured_at":500,"email":email,
            "five_hour":{"used_percentage":used,"resets_at":i64::MAX},"seven_day":{"used_percentage":used,"resets_at":i64::MAX}
        }).to_string()).unwrap();
        let login = |email: &str| std::fs::write(dir.join(".claude.json"), serde_json::json!({"oauthAccount":{"emailAddress":email}}).to_string()).unwrap();
        // 目前是 a 帳號在用；b 帳號之前換出去，留有離線數字
        status("a@example.invalid", 30.0);
        login("a@example.invalid");
        let mut cache = Cache::new();
        let mut offline = vec![cache::OfflineEntry { email: "b@example.invalid".into(), queried_at: 100, windows: vec![UsageWindow::new(model::WindowLabel::from_minutes(10080, None), 75.0, Some(i64::MAX))] }];
        fetch_account_in(&app, &a, true, &mut cache, &mut offline, 1000);
        // /login 換成 b，還沒問任何一句
        login("b@example.invalid");
        for _ in 0..2 {
            let p = fetch_account_in(&app, &a, true, &mut cache, &mut offline, 2000);
            assert_eq!(p.reason.as_ref().map(|r| r.key.as_str()), Some("claude_statusline_switched"));
            assert_eq!(p.account_full.as_deref(), Some("b@example.invalid"));
            assert_eq!(p.source.as_deref(), Some("cache"));
            assert_eq!(p.windows[0].used_percent, 75.0);
            assert_eq!(offline.len(), 1);
            assert_eq!(offline[0].email, "a@example.invalid");
            assert_eq!(offline[0].windows[0].used_percent, 30.0);
        }
        // b 在 Claude Code 回應一次 → 狀態檔換成 b
        status("b@example.invalid", 10.0);
        let p = fetch_account_in(&app, &a, true, &mut cache, &mut offline, 3000);
        assert_eq!(p.source.as_deref(), Some("statusline"));
        assert_eq!(p.windows[0].used_percent, 10.0);
        assert_eq!(offline.len(), 1);
    }

    #[test]
    fn statusline_missing_ignores_old_cache() {
        let root = std::env::temp_dir().join(format!("aitoken-snapshot-missing-{}-{}", std::process::id(), now_epoch()));
        let app = root.join("app");
        let dir = root.join("claude");
        std::fs::create_dir_all(&dir).unwrap();
        let a = AccountCfg { id: "claude:test".into(), provider: "claude".into(), dir: Some(dir.to_string_lossy().into()), name: String::new(), visible: true, extra: true };
        // 舊版（token 做法）留下的快取
        let mut cache = Cache::new();
        cache.insert(a.id.clone(), CacheEntry { email: None, queried_at: 100, windows: vec![UsageWindow::new(model::WindowLabel::from_minutes(300, None), 50.0, None)] });
        let mut offline = Offline::new();
        let p = fetch_account_in(&app, &a, true, &mut cache, &mut offline, 1000);
        assert_eq!(p.status, "unavailable");
        assert_eq!(p.reason.as_ref().map(|r| r.key.as_str()), Some("claude_statusline_missing"));
        assert!(p.windows.is_empty());
        crate::statusline::install(&app, &a.id, &dir).unwrap();
        let p = fetch_account_in(&app, &a, true, &mut cache, &mut offline, 1000);
        assert_eq!(p.reason.as_ref().map(|r| r.key.as_str()), Some("claude_statusline_waiting"));
    }

    #[test]
    fn offline_cards_follow_claude_and_apply_reset() {
        let mut codex = Provider::placeholder("codex", "ok");
        codex.key = "codex:default".into();
        let mut claude = Provider::placeholder("claude", "ok");
        claude.key = "claude:default".into();
        let mut agy = Provider::placeholder("antigravity", "ok");
        agy.key = "antigravity:x".into();
        let w = |used: f64, reset: Option<i64>| UsageWindow::new(model::WindowLabel::from_minutes(300, None), used, reset);
        let off = vec![cache::OfflineEntry { email: "aixa.test@gmail.com".into(), queried_at: 5, windows: vec![w(100.0, Some(10)), w(61.0, Some(i64::MAX))] }];
        let out = with_offline(vec![codex, claude, agy], &off, &[], 100);
        let keys: Vec<&str> = out.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys[..2], ["codex:default", "claude:default"]);
        assert!(keys[2].starts_with("offline:") && keys[3] == "antigravity:x");
        let o = &out[2];
        assert!(o.offline && o.visible && o.lowest_left().is_none() && o.account_full.is_none());
        assert_eq!(o.account_masked.as_deref(), Some("aixa•••@gmail.com"));
        assert_eq!(o.windows[0].used_percent, 0.0);
        assert!(o.windows[0].reset_done);
        assert_eq!(o.windows[1].used_percent, 61.0);
        // 重建不會重複；隱藏後 visible=false
        let again = with_offline(out, &off, &[cache::offline_id("aixa.test@gmail.com")], 100);
        assert_eq!(again.iter().filter(|p| p.offline).count(), 1);
        assert!(!again.iter().find(|p| p.offline).unwrap().visible);
    }
}
