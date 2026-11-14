//! 額度規劃的接合：把額度、重置券、用量歷史、模型用量組成面板資料（約定 quota-plan.md 第 5 節），
//! 並負責兩種通知（每週用完、券快到期）與相關指令。計算本身在 planner.rs。
use crate::model::{now_epoch, Provider, UsageWindow};
use crate::plan_types::*;
use crate::state::{zh_of, AppState};
use crate::tray::PANEL;
use crate::{applog, cache, credits, history, modelusage, pace, planner, snapshot};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

const HOUR: i64 = 3600;
const DAY: i64 = 86_400;
const WEEK: i64 = 7 * DAY;
const CLAUDE_USAGE_URL: &str = "https://claude.ai/settings/usage";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/codex/settings/usage";
const MODEL_SCAN_EVERY: i64 = 30 * 60;
const POOLS: [&str; 3] = ["claude", "codex", "antigravity"];

/// plan.json：新版面板開關等（獨立一檔，不動 config.json）
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct PlanCfg {
    #[serde(default)]
    pub new_panel: bool,
    #[serde(default = "yes")]
    pub model_usage: bool,
    /// 本地時區與 UTC 的秒差，由面板回報（後端沒有時區資料）
    #[serde(default)]
    pub tz_offset: i64,
}
fn yes() -> bool {
    true
}
impl Default for PlanCfg {
    fn default() -> Self {
        Self { new_panel: false, model_usage: true, tz_offset: 0 }
    }
}

#[derive(Default)]
struct Shared {
    view: Option<Value>,
    notified: HashSet<String>,
    model_rows: Vec<(i64, ModelUsage)>,
    model_scanned_at: i64,
}
static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

fn with_shared<T>(f: impl FnOnce(&mut Shared) -> T) -> T {
    let mut g = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Shared::default))
}

fn app_dir(app: &AppHandle) -> PathBuf {
    app.state::<AppState>().cache_path.parent().map(Path::to_path_buf).unwrap_or_default()
}
fn cfg_path(dir: &Path) -> PathBuf {
    dir.join("plan.json")
}
pub(crate) fn load_cfg(dir: &Path) -> PlanCfg {
    std::fs::read_to_string(cfg_path(dir)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}
fn save_cfg(dir: &Path, cfg: &PlanCfg) {
    if let Ok(t) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(cfg_path(dir), t);
    }
}

// ───────── 純函式：組資料 ─────────

/// 面板上的一列（一個帳號，或 Antigravity 的一組額度）
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub provider: String,
    pub group: String,
    /// 池內識別（也是用量歷史的 account）
    pub id: String,
    pub email: String,
    pub label: String,
    pub masked: String,
    pub offline: bool,
    /// 這次查詢拿到的是即時資料（才記進用量歷史、才發「用完了」通知）
    pub fresh: bool,
    pub five_left: Option<f64>,
    /// 5 小時多久後補滿（沒用到、或沒有 5 小時限制時為 None）
    pub five_resets_at: Option<i64>,
    pub weekly_left: f64,
    pub resets_at: i64,
    pub as_of: Option<i64>,
    pub source: String,
    pub credits: Vec<ResetCredit>,
}

pub(crate) struct Inputs<'a> {
    pub providers: &'a [Provider],
    /// provider.key → email（小寫）；離線卡片的 email 只有這裡有
    pub emails: &'a HashMap<String, String>,
    pub codex_credits: &'a [ResetCredit],
    pub manual: &'a [ManualCredit],
    pub history: &'a [Sample],
    /// 池內識別 → 各模型佔每週的 %
    pub shares: &'a HashMap<String, Vec<ModelShare>>,
    pub now: i64,
    pub tz_offset: i64,
}

/// 剩餘 %；重置時間已過（離線帳號的舊資料）當成已補滿
fn left_of(w: &UsageWindow, now: i64) -> f64 {
    if w.reset_done || w.resets_at.is_some_and(|t| t <= now) { 100.0 } else { (100.0 - w.used_percent).clamp(0.0, 100.0) }
}

fn short_group(g: &str) -> String {
    match g {
        "Gemini 4 Argon" => "Argon".into(),
        "Claude and GPT" => "Claude/GPT".into(),
        other => other.into(),
    }
}

pub(crate) fn rows(inp: &Inputs) -> Vec<Row> {
    let manual = credits::expand(inp.manual, inp.now);
    let mut out = Vec::new();
    for p in inp.providers {
        if !p.visible || p.status != "ok" || p.reason.as_ref().is_some_and(|r| r.key == "agy_switched") {
            continue;
        }
        let email = inp.emails.get(&p.key).cloned().or_else(|| p.account_full.as_ref().map(|e| e.to_lowercase())).unwrap_or_default();
        let masked = p.account_masked.clone().unwrap_or_default();
        let who = if !p.name.is_empty() { p.name.clone() } else { masked.split('@').next().unwrap_or("").trim_end_matches('•').to_string() };
        let fresh = !p.offline && matches!(p.source.as_deref(), Some("live") | Some("local_log") | Some("statusline"));
        let ident = if email.is_empty() { p.key.clone() } else { email.clone() };
        let base = |id: String, group: String, label: String, five: Option<&UsageWindow>, weekly: &UsageWindow, credits: Vec<ResetCredit>| Row {
            provider: p.id.clone(),
            group,
            id,
            email: email.clone(),
            label,
            masked: masked.clone(),
            offline: p.offline,
            fresh,
            five_left: five.map(|w| left_of(w, inp.now)),
            // 顯示時四捨五入；99.6% 會顯示成 100%，就不要再掛補滿倒數
            five_resets_at: five.filter(|w| left_of(w, inp.now).round() < 100.0).and_then(|w| w.resets_at).filter(|t| *t > inp.now),
            weekly_left: left_of(weekly, inp.now),
            resets_at: weekly.resets_at.filter(|t| *t > inp.now).unwrap_or(inp.now + WEEK),
            as_of: p.as_of,
            source: p.source.clone().unwrap_or_default(),
            credits,
        };
        if p.id == "antigravity" {
            let mut groups: Vec<&str> = Vec::new();
            for w in &p.windows {
                if let Some(g) = w.group.as_deref() {
                    if !groups.contains(&g) {
                        groups.push(g);
                    }
                }
            }
            for g in groups {
                let find = |kind: &str| p.windows.iter().find(|w| w.group.as_deref() == Some(g) && w.label.kind == kind && !w.disabled);
                if let Some(weekly) = find("weekly") {
                    out.push(base(format!("antigravity:{ident}:{g}"), format!("ANTIGRAVITY · {who}"), short_group(g), find("five_hour"), weekly, vec![]));
                }
            }
        } else {
            let find = |kind: &str| p.windows.iter().find(|w| w.label.kind == kind && w.label.bucket.is_none() && w.group.is_none() && !w.disabled);
            let Some(weekly) = find("weekly") else { continue };
            let credits: Vec<ResetCredit> = if p.id == "codex" {
                inp.codex_credits.iter().filter(|c| c.account.is_empty() || c.account == email).cloned().collect()
            } else {
                manual.iter().filter(|c| c.provider == p.id && !email.is_empty() && c.account == email).cloned().collect()
            };
            out.push(base(format!("{}:{ident}", p.id), p.id.to_uppercase(), who.clone(), find("five_hour"), weekly, credits));
        }
    }
    out
}

/// 這次查詢要記進用量歷史的樣本
pub(crate) fn samples(rows: &[Row], now: i64) -> Vec<Sample> {
    rows.iter()
        .filter(|r| r.fresh)
        .map(|r| Sample {
            t: now,
            account: r.id.clone(),
            provider: r.provider.clone(),
            weekly_used: 100.0 - r.weekly_left,
            weekly_resets_at: Some(r.resets_at),
            five_used: r.five_left.map(|l| 100.0 - l),
        })
        .collect()
}

fn day_start(now: i64, tz: i64) -> i64 {
    (now + tz).div_euclid(DAY) * DAY - tz
}

/// 資料時間（離線帳號是換出時間）
fn as_of(r: &Row, now: i64) -> i64 {
    r.as_of.unwrap_or(now).min(now)
}

/// 這個帳號平均每天用多少 %：（已用＋本週按券退回）÷ 週期已過天數
fn row_rate(r: &Row, inp: &Inputs) -> Option<f64> {
    let refunded = history::refunds_since(inp.history, &r.id, pace::cycle_start(r.resets_at));
    pace::rate(100.0 - r.weekly_left, refunded, r.resets_at, as_of(r, inp.now))
}

/// 整池平均速度＝各帳號相加（週期錯開，相加＝每天實際消耗）；一個都算不出來是 None
fn pool_pace(rows: &[&Row], inp: &Inputs) -> Option<f64> {
    let rates: Vec<f64> = rows.iter().filter_map(|r| row_rate(r, inp)).collect();
    (!rates.is_empty()).then(|| rates.iter().sum())
}

/// has_credit：池內有完整重設可用 → 不夠時改說「那時按券」
fn status_json(s: pace::Status, now: i64, has_credit: bool) -> Value {
    match s {
        pace::Status::Learning => json!({ "kind": "learning" }),
        pace::Status::Short(t) => json!({ "kind": if has_credit { "short_credit" } else { "short" }, "value": t - now }),
        pace::Status::More(v) => json!({ "kind": "more", "value": v.round() }),
        pace::Status::Ok => json!({ "kind": "ok" }),
    }
}

fn outlook_json(r: &Row, inp: &Inputs) -> Value {
    let Some(rate) = row_rate(r, inp) else { return json!({ "kind": "learning" }) };
    match pace::outlook(r.weekly_left, rate, r.resets_at, inp.now) {
        pace::Outlook::Early(t) => json!({ "kind": "early", "at": t, "value": r.resets_at - t }),
        pace::Outlook::Leftover(v) => json!({ "kind": "leftover", "value": v.round() }),
        pace::Outlook::Even => json!({ "kind": "even" }),
    }
}

fn plan_input(provider: &str, rows: &[&Row], inp: &Inputs) -> PlanInput {
    let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    let start = day_start(inp.now, inp.tz_offset);
    let used_today: f64 = ids.iter().map(|id| history::used_between(inp.history, id, start, inp.now + 1)).sum();
    let daily = history::daily_totals(inp.history, &ids, 15, inp.tz_offset, inp.now);
    let first = inp.history.iter().filter(|s| ids.contains(&s.account)).map(|s| s.t).min();
    let full_days = first.map_or(0, |t| ((start - t).max(0) as f64 / DAY as f64).ceil() as usize).min(14);
    let past: Vec<f64> = daily.iter().rev().skip(1).take(full_days).map(|d| d.1).collect();
    let recent = &past[..past.len().min(7)];
    let avg_daily = (!recent.is_empty()).then(|| recent.iter().sum::<f64>() / recent.len() as f64);
    // 至少看過 3 天才判斷「可能用不完」
    let max_daily = (past.len() >= 3).then(|| past.iter().copied().fold(used_today, f64::max));
    PlanInput {
        provider: provider.into(),
        now: inp.now,
        accounts: rows
            .iter()
            .map(|r| PoolAccount { id: r.id.clone(), label: r.label.clone(), weekly_left: r.weekly_left, resets_at: r.resets_at, live: r.fresh && provider != "antigravity", credits: r.credits.clone() })
            .collect(),
        used_today,
        // 今天一開始就有紀錄才判斷「進度落後」（剛安裝的第一天不判斷）
        day_fraction: if first.is_some_and(|t| t <= start + HOUR) { (inp.now - start) as f64 / DAY as f64 } else { 0.0 },
        avg_daily,
        max_daily,
        pace: pool_pace(rows, inp),
    }
}

fn usage_url(provider: &str) -> Option<&'static str> {
    match provider {
        "claude" => Some(CLAUDE_USAGE_URL),
        "codex" => Some(CODEX_USAGE_URL),
        _ => None,
    }
}

fn refund_of(inp: &Inputs, row: &Row, c: &ResetCredit) -> f64 {
    let v = inp.shares.get(&row.id).map_or(0.0, |s| modelusage::refund_estimate(s, c));
    (v * 10.0).round() / 10.0
}

fn credit_hint(row: &Row, c: &ResetCredit, plan: &PlanOutput, now: i64) -> (&'static str, Option<u32>) {
    let expiring = c.expires_at.is_some_and(|t| t - now <= 48 * HOUR);
    if c.kind == "model" {
        return (if expiring { "expiring" } else { "model_wait" }, None);
    }
    let week = plan.weeks.iter().find(|w| w.account_id == row.id && w.credit_ids.contains(&c.id)).map(|w| w.index);
    match week {
        Some(0) | None => {
            if row.resets_at - now < DAY {
                ("dont_press", None)
            } else if row.weekly_left <= 5.0 && row.resets_at - now >= 2 * DAY {
                ("press_now", None)
            } else if expiring {
                ("expiring", None)
            } else {
                ("hold", None)
            }
        }
        Some(i) => (if expiring { "expiring" } else { "scheduled" }, Some(i)),
    }
}

const TODO_ORDER: [&str; 6] = ["press_credit", "credit_expiring", "switch_account", "behind", "model_credit", "hold"];

pub(crate) struct Built {
    pub view: Value,
    pub rows: Vec<Row>,
    pub plans: Vec<PlanOutput>,
    /// 平台 → 整池平均每天用多少 %
    pub paces: HashMap<String, f64>,
}

pub(crate) fn build(inp: &Inputs) -> Built {
    let all = rows(inp);
    let mut plans = Vec::new();
    let mut pools = Vec::new();
    let mut todos: Vec<(usize, Value)> = Vec::new();
    let mut view_rows = Vec::new();
    let mut daily = Vec::new();
    let mut lanes = Vec::new();
    let mut headroom = Vec::new();
    let mut paces = HashMap::new();

    for provider in POOLS {
        let mine: Vec<&Row> = all.iter().filter(|r| r.provider == provider).collect();
        if mine.is_empty() {
            continue;
        }
        let input = plan_input(provider, &mine, inp);
        let plan = planner::plan(&input);
        let by_id = |id: &str| mine.iter().copied().find(|r| r.id == id);
        let multi = provider != "antigravity" && mine.len() > 1;
        let usable: Vec<&OrderItem> = plan.order.iter().filter(|o| !o.exhausted).collect();
        let recommended = if multi { usable.first().map(|o| o.id.clone()) } else { None };
        let use_now: Vec<String> = if multi { usable.iter().take(2).filter_map(|o| by_id(&o.id)).map(|r| r.label.clone()).collect() } else { vec![] };

        let accs: Vec<(f64, i64)> = mine.iter().map(|r| (r.weekly_left, r.resets_at)).collect();
        let has_credit = mine.iter().any(|r| r.credits.iter().any(|c| matches!(c.kind.as_str(), "full" | "weekly")));
        let status = status_json(pace::status(&accs, input.pace, plan.target_today, inp.now), inp.now, has_credit);
        if let Some(p) = input.pace {
            paces.insert(provider.to_string(), p);
        }
        pools.push(json!({
            "provider": provider, "target_today": plan.target_today, "target_without_credits": plan.target_without_credits,
            "used_today": plan.used_today, "feasible": plan.feasible, "waste_estimate": plan.waste_estimate, "use_now": use_now,
            "status": status, "pace": input.pace.map(|p| (p * 10.0).round() / 10.0),
        }));

        for t in &plan.todos {
            let row = t.account_id.as_deref().and_then(by_id);
            let credit = row.and_then(|r| t.credit_id.as_ref().and_then(|id| r.credits.iter().find(|c| &c.id == id)));
            let wants_url = matches!(t.kind.as_str(), "press_credit" | "credit_expiring" | "model_credit");
            let rank = TODO_ORDER.iter().position(|k| *k == t.kind).unwrap_or(TODO_ORDER.len());
            todos.push((rank, json!({
                "kind": t.kind, "provider": provider,
                "account_label": row.map(|r| r.label.clone()),
                "next_label": t.next_account_id.as_deref().and_then(by_id).map(|r| r.label.clone()),
                "credit_kind": credit.map(|c| c.kind.clone()),
                "model": credit.and_then(|c| c.model.clone()),
                "expires_at": credit.and_then(|c| c.expires_at),
                "value": t.value,
                "refund": match (row, credit) { (Some(r), Some(c)) if c.kind == "model" => Some(refund_of(inp, r, c)), _ => None },
                "url": if wants_url { usage_url(provider) } else { None },
            })));
        }

        // 列的順序＝建議先用的順序
        for o in &plan.order {
            let Some(r) = by_id(&o.id) else { continue };
            let state = if r.weekly_left <= 0.0 { "exhausted" } else if r.offline { "offline" } else { "live" };
            let families: Vec<String> = r.credits.iter().filter_map(|c| c.model.clone()).collect();
            let models: Vec<Value> = inp.shares.get(&r.id).map(|s| modelusage::summary(s, &families)).unwrap_or_default().iter()
                .map(|m| json!({ "family": m.family, "percent_of_week": (m.percent_of_week * 10.0).round() / 10.0 })).collect();
            let mut credit_rows: Vec<Value> = r.credits.iter().map(|c| {
                let (hint, week) = credit_hint(r, c, &plan, inp.now);
                // 手動券：展開後的 id 是「登錄 id:帳號」，「我按了」要用回登錄 id
                let manual_id = (c.source == "manual").then(|| c.id.split_once(':').map_or(c.id.clone(), |(id, _)| id.to_string()));
                json!({ "kind": c.kind, "model": c.model, "expires_at": c.expires_at, "hint": hint, "week_index": week,
                        "refund": if c.kind == "model" { Some(refund_of(inp, r, c)) } else { None }, "refill_five_hour": c.refill_five_hour,
                        "manual_id": manual_id, "account": manual_id.as_ref().map(|_| c.account.clone()), "used_at": Value::Null })
            }).collect();
            // 已按過的手動券：顯示劃掉＋可取消
            if !r.email.is_empty() {
                for m in inp.manual.iter().filter(|m| m.provider == r.provider && !m.expires_at.is_some_and(|t| t <= inp.now)) {
                    if let Some(used) = m.used_by.iter().find(|u| u.eq_ignore_ascii_case(&r.email)) {
                        credit_rows.push(json!({ "kind": m.kind, "model": m.model, "expires_at": m.expires_at, "hint": "used", "week_index": Value::Null,
                            "refund": Value::Null, "refill_five_hour": m.refill_five_hour, "manual_id": m.id, "account": used,
                            "used_at": m.used_at.get(used) }));
                    }
                }
            }
            let has_detail = provider != "antigravity";
            let note_key = if r.offline { "swapped_out" } else { r.source.as_str() };
            view_rows.push(json!({
                "provider": provider, "group": r.group, "account_id": r.id, "label": r.label, "state": state,
                "five_left": r.five_left.map(f64::round), "five_resets_at": r.five_resets_at, "weekly_left": r.weekly_left.round(), "weekly_resets_at": r.resets_at,
                "credits": r.credits.len(), "recommended": recommended.as_deref() == Some(r.id.as_str()),
                "detail": if has_detail { json!({ "account_masked": r.masked, "note_key": note_key, "as_of": r.as_of.unwrap_or(inp.now),
                    "weekly_used": (100.0 - r.weekly_left).round(), "models": models, "credits": credit_rows, "outlook": outlook_json(r, inp) }) } else { Value::Null },
            }));
        }

        if provider != "antigravity" {
            let ids: Vec<String> = mine.iter().map(|r| r.id.clone()).collect();
            let recorded = history::daily_totals(inp.history, &ids, 7, inp.tz_offset, inp.now);
            let est: Vec<pace::EstAcc> = mine.iter().map(|r| pace::EstAcc {
                rate: row_rate(r, inp), cycle_start: pace::cycle_start(r.resets_at), as_of: as_of(r, inp.now),
                first_sample: history::first_sample(inp.history, &r.id),
            }).collect();
            // 沒資料的日子 used＝null（畫空框），不畫成 0%
            let days: Vec<Value> = pace::fill_days(&recorded, &est).iter()
                .map(|(d, u)| json!({ "day": d, "used": u.map(|u| (u * 10.0).round() / 10.0) })).collect();
            daily.push(json!({ "provider": provider, "target": plan.target_today, "days": days }));
            for o in &plan.order {
                let Some(r) = by_id(&o.id) else { continue };
                let cells: Vec<Value> = plan.weeks.iter().filter(|w| w.account_id == r.id && w.index < 4)
                    .map(|w| json!({ "base": w.base.round(), "credits": w.credit_ids.len() })).collect();
                lanes.push(json!({ "provider": provider, "label": r.label, "cells": cells }));
            }
        }
        let spare = plan.target_today - plan.used_today;
        if spare >= 1.0 {
            headroom.push(json!({ "provider": provider, "percent": spare.round() }));
        }
        plans.push(plan);
    }

    todos.sort_by_key(|t| t.0);
    let todos: Vec<Value> = todos.into_iter().take(3).map(|t| t.1).collect();
    let claude_accounts: Vec<Value> = all.iter().filter(|r| r.provider == "claude" && !r.email.is_empty())
        .map(|r| json!({ "email": r.email, "label": r.label })).collect();
    let view = json!({
        "now": inp.now, "pools": pools, "todos": todos, "rows": view_rows,
        "plan": { "daily": daily, "lanes": lanes, "headroom": headroom },
        "manual_credits": inp.manual, "claude_accounts": claude_accounts,
    });
    Built { view, rows: all, plans, paces }
}

fn fmt_left(secs: i64) -> String {
    crate::model::format_remaining(secs)
}

/// 兩種通知：① 每週用完 ② 券快到期（24 小時、6 小時）。回傳（標題, 內文）；notified 記住發過的
pub(crate) fn notices(rows: &[Row], plans: &[PlanOutput], paces: &HashMap<String, f64>, now: i64, zh: bool, notified: &mut HashSet<String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let pname = |p: &str| crate::state::platform_name(p);
    // ① 每週用完：照整池平均速度推算，其他帳號撐不撐得到這個帳號重置
    for r in rows.iter().filter(|r| r.fresh && r.provider != "antigravity" && r.weekly_left <= 0.0) {
        if !notified.insert(format!("out:{}:{}", r.id, r.resets_at)) {
            continue;
        }
        let left = r.resets_at - now;
        let has_credit = r.credits.iter().any(|c| matches!(c.kind.as_str(), "full" | "weekly") && c.expires_at.map_or(true, |t| t > now));
        let next = plans.iter().find(|p| p.provider == r.provider).and_then(|p| p.order.iter().find(|o| !o.exhausted && o.id != r.id));
        let others: Vec<(f64, i64)> = rows.iter().filter(|o| o.provider == r.provider && o.id != r.id).map(|o| (o.weekly_left, o.resets_at)).collect();
        let covered = paces.get(&r.provider).map(|p| pace::run_out(&others, *p, now, r.resets_at).is_none());
        let switch = next.map(|o| if zh { format!("改用 {}。", o.label) } else { format!(" Switch to {}.", o.label) }).unwrap_or_default();
        let title = if zh { format!("{} {} 本週用完了", pname(&r.provider), r.label) } else { format!("{} {}: weekly limit used up", pname(&r.provider), r.label) };
        let press = has_credit && left >= 2 * DAY;
        let body = match (covered, zh) {
            (Some(true), true) => format!("其他帳號撐得到重置，不用按券。{switch}"),
            (Some(true), false) => format!("Other accounts will last until the reset; no need to use a reset.{switch}"),
            (Some(false), true) if press => format!("其他帳號撐不到重置。現在按 {} 的完整重設最划算：打開面板點「用量頁」。", r.label),
            (Some(false), false) if press => format!("Other accounts won't last until the reset. Use {}'s reset now: open the panel and click Usage page.", r.label),
            (Some(false), true) => format!("其他帳號撐不到重置，{} 後才重置。{switch}", fmt_left(left)),
            (Some(false), false) => format!("Other accounts won't last; resets in {}.{switch}", fmt_left(left)),
            // 還算不出速度：照重置時間判斷
            (None, true) if press => format!("離重置還有 {}。現在按完整重設可換回整整一週：打開面板點「用量頁」。", fmt_left(left)),
            (None, false) if press => format!("Resets in {}. Using a reset now gives back a full week: open the panel and click Usage page.", fmt_left(left)),
            (None, true) => format!("{} 後重置，不用按券。{switch}", fmt_left(left)),
            (None, false) => format!("Resets in {}; no need to use a reset.{switch}", fmt_left(left)),
        };
        out.push((title, body));
    }
    // ② 券快到期：同平台、同到期時間合成一則
    let mut groups: Vec<(String, i64)> = Vec::new();
    for r in rows {
        for c in &r.credits {
            if let Some(t) = c.expires_at.filter(|t| *t > now && *t - now <= 24 * HOUR) {
                if !groups.contains(&(r.provider.clone(), t)) {
                    groups.push((r.provider.clone(), t));
                }
            }
        }
    }
    for (provider, t) in groups {
        let stage = if t - now <= 6 * HOUR { "6" } else { "24" };
        let fresh_key = notified.insert(format!("exp:{provider}:{t}:{stage}"));
        if stage == "6" {
            notified.insert(format!("exp:{provider}:{t}:24"));
        }
        if !fresh_key {
            continue;
        }
        let mut labels: Vec<String> = Vec::new();
        let mut model: Option<String> = None;
        for r in rows.iter().filter(|r| r.provider == provider) {
            for c in r.credits.iter().filter(|c| c.expires_at == Some(t)) {
                if c.kind == "model" {
                    model = c.model.clone();
                }
                if !labels.contains(&r.label) {
                    labels.push(r.label.clone());
                }
            }
        }
        let what = model.map(|m| format!("{}{} 重設", m[..1].to_uppercase(), &m[1..])).unwrap_or_else(|| if zh { "重置券".into() } else { "reset".into() });
        let title = if zh { format!("{} {} {} 後到期", pname(&provider), what, fmt_left(t - now)) } else { format!("{} {} expires in {}", pname(&provider), what, fmt_left(t - now)) };
        // Claude 的券要手動登記「我按了」；Codex 會自動讀到
        let manual = provider == "claude";
        let body = match (zh, manual) {
            (true, true) => format!("現在按最划算：{}。按完在面板點「我按了」。", labels.join("、")),
            (true, false) => format!("現在按最划算：{}。打開面板點「用量頁」。", labels.join("、")),
            (false, true) => format!("Best to use it now: {}. Then click \"I used it\" in the panel.", labels.join(", ")),
            (false, false) => format!("Best to use it now: {}. Open the panel and click Usage page.", labels.join(", ")),
        };
        out.push((title, body));
    }
    out
}

// ───────── 接合：讀寫檔、Tauri 指令 ─────────

fn emails_of(app: &AppHandle, providers: &[Provider]) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = providers.iter().filter_map(|p| p.account_full.as_ref().map(|e| (p.key.clone(), e.to_lowercase()))).collect();
    for e in app.state::<AppState>().offline.lock().unwrap().iter() {
        map.insert(format!("offline:{}", cache::offline_id(&e.email)), e.email.to_lowercase());
    }
    map
}

fn claude_project_dirs(app: &AppHandle) -> Vec<PathBuf> {
    let cfg = app.state::<AppState>().config.lock().unwrap().clone();
    let mut dirs: Vec<PathBuf> = cfg.accounts.iter().filter(|a| a.provider == "claude").filter_map(snapshot::cfg_dir).map(|d| d.join("projects")).collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

fn model_shares(app: &AppHandle, cfg: &PlanCfg, rows: &[Row], hist: &[Sample], now: i64) -> HashMap<String, Vec<ModelShare>> {
    if !cfg.model_usage || !rows.iter().any(|r| r.provider == "claude") {
        return HashMap::new();
    }
    let stale = with_shared(|s| now - s.model_scanned_at >= MODEL_SCAN_EVERY);
    if stale {
        let since = now - 8 * DAY;
        let scanned: Vec<(i64, ModelUsage)> = claude_project_dirs(app).iter().flat_map(|d| modelusage::scan(d, since)).collect();
        applog::log(&format!("model usage scan rows={}", scanned.len()));
        with_shared(|s| {
            s.model_rows = scanned;
            s.model_scanned_at = now;
        });
    }
    let since_of = |account: &str| rows.iter().find(|r| r.id == account).map_or(i64::MAX, |r| r.resets_at - WEEK);
    let account_at = |t: i64| history::account_at(hist, "claude", t);
    let grouped = with_shared(|s| modelusage::by_account(&s.model_rows, &account_at, &since_of));
    grouped
        .into_iter()
        .filter_map(|(account, usages)| {
            let used = rows.iter().find(|r| r.id == account).map(|r| 100.0 - r.weekly_left)?;
            Some((account, modelusage::shares(&usages, used)))
        })
        .collect()
}

/// 每次查詢完呼叫：記歷史、重算、發通知、通知面板
pub(crate) fn on_refresh(app: &AppHandle, providers: &[Provider]) {
    let dir = app_dir(app);
    let cfg = load_cfg(&dir);
    let now = now_epoch();
    let emails = emails_of(app, providers);
    let codex_credits = crate::codex::last_credits();
    let mut manual = credits::load(&dir.join("reset-credits.json"));
    if credits::prune_expired(&mut manual, now) > 0 {
        let _ = credits::save(&dir.join("reset-credits.json"), &manual);
    }
    let hist_path = dir.join("history.json");
    let mut hist = history::load(&hist_path);
    let empty = HashMap::new();
    let current = rows(&Inputs { providers, emails: &emails, codex_credits: &codex_credits, manual: &manual, history: &[], shares: &empty, now, tz_offset: cfg.tz_offset });
    let before = hist.len();
    let fresh = samples(&current, now);
    // 用券偵測：每週用量在重置時間前大降。記下前後的重置時間，用來確認用券後重置日期會不會變
    for cur in &fresh {
        if let Some(prev) = hist.iter().filter(|s| s.account == cur.account).max_by_key(|s| s.t) {
            if history::looks_like_credit_use(prev, cur) {
                applog::log(&format!(
                    "credit use detected provider={} used {:.0}->{:.0} resets_at {:?}->{:?}",
                    cur.provider, prev.weekly_used, cur.weekly_used, prev.weekly_resets_at, cur.weekly_resets_at
                ));
            }
        }
    }
    history::record(&mut hist, &fresh, now);
    if hist.len() != before {
        let _ = history::save(&hist_path, &hist);
    }
    let shares = model_shares(app, &cfg, &current, &hist, now);
    let built = build(&Inputs { providers, emails: &emails, codex_credits: &codex_credits, manual: &manual, history: &hist, shares: &shares, now, tz_offset: cfg.tz_offset });
    let zh = zh_of(&app.state::<AppState>().config.lock().unwrap());
    let notes = with_shared(|s| {
        s.view = Some(built.view.clone());
        notices(&built.rows, &built.plans, &built.paces, now, zh, &mut s.notified)
    });
    for (title, body) in notes {
        applog::log("plan notification sent");
        let _ = app.notification().builder().title(title).body(body).show();
    }
    let _ = app.emit_to(PANEL, "plan", built.view);
}

/// 用目前的快照重算（券或設定變動後）
fn rebuild(app: &AppHandle) -> Value {
    let providers = app.state::<AppState>().snapshot.lock().unwrap().providers.clone();
    on_refresh(app, &providers);
    with_shared(|s| s.view.clone()).unwrap_or(Value::Null)
}

#[tauri::command]
pub(crate) fn get_plan_view(app: AppHandle, tz_offset: Option<i64>) -> Value {
    let dir = app_dir(&app);
    let mut cfg = load_cfg(&dir);
    let tz_changed = tz_offset.is_some_and(|tz| tz.abs() <= 14 * HOUR && tz != cfg.tz_offset);
    if let (true, Some(tz)) = (tz_changed, tz_offset) {
        cfg.tz_offset = tz;
        save_cfg(&dir, &cfg);
    }
    match with_shared(|s| s.view.clone()) {
        Some(v) if !tz_changed => v,
        _ => rebuild(&app),
    }
}

#[tauri::command]
pub(crate) fn get_plan_cfg(app: AppHandle) -> PlanCfg {
    load_cfg(&app_dir(&app))
}

#[tauri::command]
pub(crate) fn set_plan_cfg(app: AppHandle, new_panel: Option<bool>, model_usage: Option<bool>) -> PlanCfg {
    let dir = app_dir(&app);
    let mut cfg = load_cfg(&dir);
    if let Some(v) = new_panel {
        cfg.new_panel = v;
    }
    if let Some(v) = model_usage {
        cfg.model_usage = v;
    }
    save_cfg(&dir, &cfg);
    cfg
}

fn edit_credits(app: &AppHandle, f: impl FnOnce(&mut Vec<ManualCredit>) -> Result<(), String>) -> Result<Value, String> {
    let path = app_dir(app).join("reset-credits.json");
    let mut list = credits::load(&path);
    f(&mut list)?;
    credits::save(&path, &list).map_err(|e| e.to_string())?;
    Ok(rebuild(app))
}

#[tauri::command]
pub(crate) fn add_manual_credit(app: AppHandle, credit: ManualCredit) -> Result<Value, String> {
    credits::validate(&credit)?;
    edit_credits(&app, |list| {
        credits::add(list, credit);
        Ok(())
    })
}

#[tauri::command]
pub(crate) fn remove_manual_credit(app: AppHandle, id: String) -> Result<Value, String> {
    edit_credits(&app, |list| credits::remove(list, &id).then_some(()).ok_or_else(|| "not_found".to_string()))
}

#[tauri::command]
pub(crate) fn mark_credit_used(app: AppHandle, id: String, account: String) -> Result<Value, String> {
    edit_credits(&app, |list| credits::mark_used(list, &id, &account, now_epoch()).then_some(()).ok_or_else(|| "not_found".to_string()))
}

#[tauri::command]
pub(crate) fn unmark_credit_used(app: AppHandle, id: String, account: String) -> Result<Value, String> {
    edit_credits(&app, |list| credits::unmark_used(list, &id, &account).then_some(()).ok_or_else(|| "not_found".to_string()))
}

/// 用預設瀏覽器開官方用量頁。只開寫死的兩個網址，不接受其他值
#[tauri::command]
pub(crate) fn open_usage_page(url: String) -> bool {
    let Some(target) = [CLAUDE_USAGE_URL, CODEX_USAGE_URL].into_iter().find(|u| *u == url) else { return false };
    let mut cmd = std::process::Command::new("explorer.exe");
    cmd.arg(target);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WindowLabel;

    fn win(minutes: u64, used: f64, resets: i64, group: Option<&str>) -> UsageWindow {
        let mut w = UsageWindow::new(WindowLabel::from_minutes(minutes, None), used, Some(resets));
        w.group = group.map(str::to_string);
        w
    }
    fn prov(id: &str, key: &str, email: &str, source: &str, windows: Vec<UsageWindow>) -> Provider {
        let mut p = Provider::placeholder(id, "ok");
        p.key = key.into();
        p.source = Some(source.into());
        p.account_full = (!email.is_empty()).then(|| email.to_string());
        p.account_masked = Some(crate::model::mask_email(email));
        p.offline = source == "offline";
        p.windows = windows;
        p
    }
    fn manual(kind: &str, exp: i64, accounts: &[&str]) -> ManualCredit {
        ManualCredit { id: format!("m-{kind}"), provider: "claude".into(), kind: kind.into(), model: (kind == "model").then(|| "fable".to_string()),
            max_refund_percent: (kind == "model").then_some(50.0), refill_five_hour: true, expires_at: Some(exp),
            accounts: accounts.iter().map(|s| s.to_string()).collect(), used_by: vec![], used_at: Default::default() }
    }
    const NOW: i64 = 1_000_000;

    fn fixture() -> (Vec<Provider>, HashMap<String, String>, Vec<ResetCredit>, Vec<ManualCredit>) {
        let providers = vec![
            prov("codex", "codex:default", "me@example.com", "live", vec![win(300, 0.0, NOW + 5 * HOUR, None), win(10080, 44.0, NOW + 5 * DAY, None)]),
            prov("claude", "claude:default", "a@example.com", "statusline", vec![win(300, 56.0, NOW + 3 * HOUR, None), win(10080, 100.0, NOW + 3 * DAY, None)]),
            prov("claude", "offline:x", "", "offline", vec![win(300, 0.0, NOW + HOUR, None), win(10080, 27.0, NOW + 4 * DAY, None)]),
            prov("antigravity", "antigravity:me", "me@example.com", "live", vec![
                win(300, 0.0, NOW + HOUR, Some("Gemini")), win(10080, 3.0, NOW + 6 * DAY, Some("Gemini")),
                win(10080, 0.0, NOW + 6 * DAY, Some("Gemini 4 Argon")),
            ]),
        ];
        let mut providers = providers;
        providers[2].account_masked = Some("b•••@example.com".into());
        let emails = HashMap::from([("offline:x".to_string(), "b@example.com".to_string())]);
        let codex = vec![ResetCredit { id: "rc1".into(), provider: "codex".into(), account: "me@example.com".into(), kind: "full".into(), model: None,
            max_refund_percent: None, refill_five_hour: true, expires_at: Some(NOW + 16 * DAY), source: "auto".into(), title: None }];
        let manual = vec![manual("full", NOW + 20 * DAY, &["a@example.com", "b@example.com"]), manual("model", NOW + 5 * HOUR, &["a@example.com", "b@example.com"])];
        (providers, emails, codex, manual)
    }

    #[test]
    fn builds_rows_pools_and_todos() {
        let (providers, emails, codex, manual) = fixture();
        let shares = HashMap::from([("claude:b@example.com".to_string(), vec![ModelShare { family: "opus".into(), percent_of_week: 15.0 }, ModelShare { family: "fable".into(), percent_of_week: 9.0 }])]);
        let b = build(&Inputs { providers: &providers, emails: &emails, codex_credits: &codex, manual: &manual, history: &[], shares: &shares, now: NOW, tz_offset: 0 });
        assert_eq!(b.rows.len(), 5);
        let v = &b.view;
        assert_eq!(v["pools"].as_array().unwrap().len(), 3);
        // Claude：用完的 a 排後面、建議用離線的 b
        let claude: Vec<&Value> = v["rows"].as_array().unwrap().iter().filter(|r| r["provider"] == "claude").collect();
        assert_eq!(claude[0]["account_id"], "claude:b@example.com");
        assert_eq!(claude[0]["state"], "offline");
        assert_eq!(claude[0]["recommended"], true);
        assert_eq!(claude[0]["credits"], 2);
        assert_eq!(claude[1]["state"], "exhausted");
        // 模型用量：代表（opus）＋有券的（fable）；Fable 券估算退回 9%
        assert_eq!(claude[0]["detail"]["models"].as_array().unwrap().len(), 2);
        let fable = claude[0]["detail"]["credits"].as_array().unwrap().iter().find(|c| c["kind"] == "model").unwrap();
        assert_eq!(fable["refund"], 9.0);
        assert_eq!(fable["hint"], "expiring");
        // Antigravity 兩組各一列、不能展開
        let agy: Vec<&Value> = v["rows"].as_array().unwrap().iter().filter(|r| r["provider"] == "antigravity").collect();
        assert_eq!(agy.len(), 2);
        assert!(agy.iter().any(|r| r["label"] == "Argon" && r["five_left"].is_null() && r["detail"].is_null()));
        // 該做的事最多 3 條，該按券排最前面
        let todos = v["todos"].as_array().unwrap();
        assert_eq!(todos.len(), 3);
        assert_eq!(todos[0]["kind"], "press_credit");
        assert_eq!(todos[0]["url"], CLAUDE_USAGE_URL);
        assert_eq!(v["claude_accounts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn samples_only_from_fresh_rows() {
        let (providers, emails, codex, manual) = fixture();
        let empty = HashMap::new();
        let r = rows(&Inputs { providers: &providers, emails: &emails, codex_credits: &codex, manual: &manual, history: &[], shares: &empty, now: NOW, tz_offset: 0 });
        let s = samples(&r, NOW);
        assert_eq!(s.len(), 4); // 離線帳號不記
        assert!(s.iter().all(|x| x.account != "claude:b@example.com"));
        assert!(s.iter().any(|x| x.account == "antigravity:me@example.com:Gemini" && x.weekly_used == 3.0));
    }

    #[test]
    fn notifies_weekly_out_and_expiring_once() {
        let (providers, emails, codex, manual) = fixture();
        let empty = HashMap::new();
        let b = build(&Inputs { providers: &providers, emails: &emails, codex_credits: &codex, manual: &manual, history: &[], shares: &empty, now: NOW, tz_offset: 0 });
        let mut seen = HashSet::new();
        let none = HashMap::new();
        let n = notices(&b.rows, &b.plans, &none, NOW, true, &mut seen);
        assert_eq!(n.len(), 2, "{n:?}");
        // 還算不出速度：照重置時間（3 天）建議按券
        assert!(n[0].0.contains("本週用完了") && n[0].1.contains("完整重設"), "{n:?}");
        assert!(n[1].0.contains("Fable 重設") && n[1].1.contains("現在按最划算") && n[1].1.contains("我按了"), "{n:?}");
        // 同一輪不重複；6 小時那則發過後 24 小時那則也不再發
        assert!(notices(&b.rows, &b.plans, &none, NOW, true, &mut seen).is_empty());
        assert!(seen.iter().any(|k| k.ends_with(":24")));
        // 有速度：b 剩 73%、每天 10% → 撐得到 a 重置，不用按
        let slow = HashMap::from([("claude".to_string(), 10.0)]);
        let n = notices(&b.rows, &b.plans, &slow, NOW, true, &mut HashSet::new());
        assert!(n[0].1.contains("撐得到重置") && n[0].1.contains("改用 b"), "{n:?}");
        // 每天 60% → 撐不到，按 a 的完整重設
        let fast = HashMap::from([("claude".to_string(), 60.0)]);
        let n = notices(&b.rows, &b.plans, &fast, NOW, true, &mut HashSet::new());
        assert!(n[0].1.contains("撐不到重置") && n[0].1.contains("按 a 的完整重設"), "{n:?}");
    }

    #[test]
    fn pace_status_outlook_and_estimated_days() {
        // a 用完（3 天後重置，已用 100% / 過了 4 天 → 25%/天）；b 離線剩 73%（4 天後重置，已用 27% / 3 天 → 9%/天）
        let (providers, emails, codex, manual) = fixture();
        let empty = HashMap::new();
        let b = build(&Inputs { providers: &providers, emails: &emails, codex_credits: &codex, manual: &manual, history: &[], shares: &empty, now: NOW, tz_offset: 0 });
        assert!((b.paces["claude"] - 34.0).abs() < 0.01, "{:?}", b.paces);
        let claude = b.view["pools"].as_array().unwrap().iter().find(|p| p["provider"] == "claude").unwrap();
        assert!(claude["status"]["kind"].is_string());
        let row = b.view["rows"].as_array().unwrap().iter().find(|r| r["account_id"] == "claude:b@example.com").unwrap();
        // 73% ÷ 9%/天 ＝ 8 天 > 4 天 → 會剩
        assert_eq!(row["detail"]["outlook"]["kind"], "leftover");
        // 5 小時：用了 56% 的 a 有補滿時間；沒用的 Codex 沒有
        let a = b.view["rows"].as_array().unwrap().iter().find(|r| r["account_id"] == "claude:a@example.com").unwrap();
        assert_eq!(a["five_resets_at"], NOW + 3 * HOUR);
        let codex_row = b.view["rows"].as_array().unwrap().iter().find(|r| r["provider"] == "codex").unwrap();
        assert!(codex_row["five_resets_at"].is_null());
        // 長條：週期開始前的日子是 null，週期內的有推算值
        let days = b.view["plan"]["daily"].as_array().unwrap().iter().find(|d| d["provider"] == "claude").unwrap()["days"].as_array().unwrap().clone();
        assert!(days[0]["used"].is_null());
        assert!(days[6]["used"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn used_manual_credit_shows_with_undo_and_leaves_plan() {
        let (providers, emails, codex, mut manual) = fixture();
        manual[1].used_by.push("b@example.com".into());
        manual[1].used_at.insert("b@example.com".into(), NOW - 60);
        let empty = HashMap::new();
        let b = build(&Inputs { providers: &providers, emails: &emails, codex_credits: &codex, manual: &manual, history: &[], shares: &empty, now: NOW, tz_offset: 0 });
        let row = b.view["rows"].as_array().unwrap().iter().find(|r| r["account_id"] == "claude:b@example.com").unwrap();
        assert_eq!(row["credits"], 1);
        let credits = row["detail"]["credits"].as_array().unwrap();
        let used = credits.iter().find(|c| c["hint"] == "used").unwrap();
        assert_eq!(used["manual_id"], "m-model");
        assert_eq!(used["account"], "b@example.com");
        assert_eq!(used["used_at"], NOW - 60);
        let full = credits.iter().find(|c| c["kind"] == "full").unwrap();
        assert_eq!(full["manual_id"], "m-full");
    }

    #[test]
    fn usage_page_only_opens_known_urls() {
        assert!(!open_usage_page("https://example.com".into()));
        assert!(!open_usage_page(String::new()));
    }

    /// 手動：`cargo test --offline real_data_view -- --ignored --nocapture`
    /// 用本機真實的 cache.json／offline.json／reset-credits.json／history.json 產生面板資料，寫到 AITOKEN_VIEW_OUT
    #[test]
    #[ignore]
    fn real_data_view() {
        let dir = PathBuf::from(std::env::var("APPDATA").unwrap()).join("tools.aixai.token-usage");
        let cfg = crate::config::Config::load(&dir.join("config.json"));
        let cache = cache::load(&dir.join("cache.json"));
        let offline = cache::load_offline(&dir.join("offline.json"));
        let now = now_epoch();
        let providers: Vec<Provider> = cfg.accounts.iter().map(|a| snapshot::from_cache(a, a.extra, &cache, None, now)).collect();
        let mut providers = snapshot::with_offline(providers, &offline, &cfg.offline_hidden, now);
        for p in providers.iter_mut().filter(|p| !p.offline) {
            p.source = Some("live".into());
        }
        let mut emails: HashMap<String, String> = providers.iter().filter_map(|p| p.account_full.as_ref().map(|e| (p.key.clone(), e.to_lowercase()))).collect();
        for e in offline.iter() {
            emails.insert(format!("offline:{}", cache::offline_id(&e.email)), e.email.to_lowercase());
        }
        let manual = credits::load(&dir.join("reset-credits.json"));
        let hist = history::load(&dir.join("history.json"));
        let codex: Vec<ResetCredit> = std::env::var("AITOKEN_CODEX_CREDITS").ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let empty = HashMap::new();
        let b = build(&Inputs { providers: &providers, emails: &emails, codex_credits: &codex, manual: &manual, history: &hist, shares: &empty, now, tz_offset: 8 * HOUR });
        let out = std::env::var("AITOKEN_VIEW_OUT").expect("AITOKEN_VIEW_OUT");
        std::fs::write(out, serde_json::to_string_pretty(&b.view).unwrap()).unwrap();
        println!("rows={} pools={}", b.rows.len(), b.view["pools"].as_array().unwrap().len());
    }
}
