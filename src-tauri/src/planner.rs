// 額度規劃的計算：整池每日目標、券排程、帳號順序、該做的事。
// 純函式：不讀檔、不取系統時間。規則見 docs/ai/contracts/quota-plan.md 第 3 節。
use crate::pace;
use crate::plan_types::*;

const HOUR: i64 = 3600;
const DAY: i64 = 86_400;
const WEEK: i64 = 7 * DAY;
/// 離重置不到這麼久的額度當成來不及用（不算進目標）
const LOST_WINDOW: i64 = 6 * HOUR;
/// 用券後至少要有這麼久才值得（否則等自然重置）
const MIN_CREDIT_WINDOW: i64 = 2 * DAY;
const EXPIRING_SOON: i64 = 48 * HOUR;
const MAX_TODOS: usize = 3;

struct Acc<'a> {
    src: &'a PoolAccount,
    left: f64,
    resets_at: i64,
}

fn round1(v: f64) -> f64 {
    if v.is_finite() { (v.max(0.0) * 10.0).round() / 10.0 } else { 0.0 }
}

fn days(secs: i64) -> f64 {
    secs as f64 / DAY as f64
}

fn schedulable(c: &ResetCredit, now: i64) -> bool {
    matches!(c.kind.as_str(), "full" | "weekly") && c.expires_at.map_or(true, |t| t > now)
}

fn overlaps(a: &WeekCell, b: &WeekCell) -> bool {
    a.start < b.end && b.start < a.end
}

/// 整理輸入：剩餘限制在 0～100；重置時間已過的帳號視為已重置
fn normalize(input: &PlanInput) -> Vec<Acc<'_>> {
    input
        .accounts
        .iter()
        .map(|a| {
            let mut left = if a.weekly_left.is_finite() { a.weekly_left.clamp(0.0, 100.0) } else { 0.0 };
            let mut resets_at = a.resets_at;
            if resets_at <= input.now {
                let behind = (input.now - resets_at) / WEEK + 1;
                resets_at += behind * WEEK;
                left = 100.0;
            }
            Acc { src: a, left, resets_at }
        })
        .collect()
}

fn build_cycles(accs: &[Acc], now: i64, horizon: i64) -> Vec<WeekCell> {
    let mut cells = Vec::new();
    for a in accs {
        cells.push(WeekCell { account_id: a.src.id.clone(), index: 0, start: now, end: a.resets_at, base: a.left, credit_ids: vec![] });
        let mut k = 1u32;
        loop {
            let start = a.resets_at + (k as i64 - 1) * WEEK;
            let end = start + WEEK;
            if end > horizon || k > 60 {
                break;
            }
            cells.push(WeekCell { account_id: a.src.id.clone(), index: k, start, end, base: 100.0, credit_ids: vec![] });
            k += 1;
        }
    }
    cells
}

/// 把 full／weekly 券排進週期：到期早的先排、一個週期一張、同池盡量錯開
fn schedule(cells: &mut [WeekCell], accs: &[Acc], now: i64) {
    let mut credits: Vec<(&str, &ResetCredit)> = accs
        .iter()
        .flat_map(|a| a.src.credits.iter().filter(|c| schedulable(c, now)).map(move |c| (a.src.id.as_str(), c)))
        .collect();
    credits.sort_by_key(|(_, c)| c.expires_at.unwrap_or(i64::MAX));
    for (acc_id, c) in credits {
        let exp = c.expires_at.unwrap_or(i64::MAX);
        let mine: Vec<usize> = (0..cells.len()).filter(|&i| cells[i].account_id == acc_id).collect();
        let ok = |cell: &WeekCell| {
            cell.credit_ids.is_empty()
                && cell.end - cell.start.max(now) >= MIN_CREDIT_WINDOW
                && (cell.index == 0 || cell.start.saturating_add(DAY) <= exp)
        };
        let crowd = |i: usize| {
            cells.iter().filter(|o| o.account_id != acc_id && !o.credit_ids.is_empty() && overlaps(o, &cells[i])).count()
        };
        let pick = mine
            .iter()
            .copied()
            .filter(|&i| ok(&cells[i]))
            .min_by_key(|&i| (crowd(i), cells[i].index))
            // 沒有合適的週期：放進到期前最後一個開始的週期（至少是週期 0）
            .or_else(|| mine.iter().copied().filter(|&i| cells[i].start < exp).max_by_key(|&i| cells[i].index))
            .or_else(|| mine.first().copied());
        if let Some(i) = pick {
            cells[i].credit_ids.push(c.id.clone());
        }
    }
}

/// 能趕上所有期限的最低每日速度；with_credits=false 時不算券
fn required_rate(cells: &[WeekCell], now: i64, with_credits: bool) -> f64 {
    let live: Vec<&WeekCell> = cells.iter().filter(|c| c.end - now >= LOST_WINDOW).collect();
    let work = |c: &WeekCell| c.base + if with_credits { 100.0 * c.credit_ids.len() as f64 } else { 0.0 };
    live.iter()
        .map(|d| {
            let total: f64 = live.iter().filter(|c| c.end <= d.end).map(|c| work(c)).sum();
            total / days(d.end - now).max(0.25)
        })
        .fold(0.0, f64::max)
}

fn order_of(accs: &[Acc]) -> Vec<OrderItem> {
    let mut v: Vec<&Acc> = accs.iter().collect();
    v.sort_by(|a, b| {
        let (ae, be) = (a.left <= 0.0, b.left <= 0.0);
        ae.cmp(&be).then_with(|| {
            if (a.resets_at - b.resets_at).abs() < HOUR {
                b.left.partial_cmp(&a.left).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                a.resets_at.cmp(&b.resets_at)
            }
        })
    });
    v.iter()
        .map(|a| OrderItem { id: a.src.id.clone(), label: a.src.label.clone(), weekly_left: round1(a.left), resets_at: a.resets_at, exhausted: a.left <= 0.0 })
        .collect()
}

fn todo(kind: &str, provider: &str) -> Todo {
    Todo { kind: kind.into(), provider: provider.into(), account_id: None, credit_id: None, value: None, next_account_id: None, at: None }
}

fn build_todos(input: &PlanInput, accs: &[Acc], order: &[OrderItem], target: f64) -> Vec<Todo> {
    let now = input.now;
    let p = input.provider.as_str();
    let mut out = Vec::new();

    // 1. 按券：照平均速度推算整池何時全用完；在那時按，真正多用到 ≥ 50%（或券留著也會過期）才建議（約定 11.1）
    let mut pressed: Vec<&str> = Vec::new();
    let first_credit = |a: &Acc, after: i64| a.src.credits.iter().filter(|c| schedulable(c, now) && c.expires_at.map_or(true, |e| e > after)).min_by_key(|c| c.expires_at.unwrap_or(i64::MAX)).cloned();
    if let Some(rate) = input.pace.filter(|r| r.is_finite() && *r > 0.0) {
        let all: Vec<(f64, i64)> = accs.iter().map(|a| (a.left, a.resets_at)).collect();
        let until = accs.iter().map(|a| a.resets_at).max().unwrap_or(now);
        if let Some(t_out) = pace::run_out(&all, rate, now, until) {
            // 每個有券的帳號：在 t_out 按它的券，到它下一次重置前能補上的缺口
            let best = accs.iter()
                .filter_map(|a| {
                    let c = first_credit(a, t_out)?;
                    let reset = pace::next_reset_after(a.resets_at, t_out);
                    Some((a, c, pace::shortfall(&all, rate, now, reset).min(100.0), reset))
                })
                .max_by(|x, y| x.2.total_cmp(&y.2).then(y.1.expires_at.unwrap_or(i64::MAX).cmp(&x.1.expires_at.unwrap_or(i64::MAX))));
            match best {
                Some((a, c, gain, reset)) if pace::worth_pressing(gain, c.expires_at, reset) => {
                    pressed.push(a.src.id.as_str());
                    out.push(Todo { account_id: Some(a.src.id.clone()), credit_id: Some(c.id.clone()), value: Some(round1(gain)), at: Some(t_out), ..todo("press_credit", p) });
                }
                Some(_) => {
                    // 不值得按：只提醒會差多久（到最先重置的帳號補回來為止）
                    let next = accs.iter().min_by_key(|a| pace::next_reset_after(a.resets_at, t_out));
                    if let Some(n) = next {
                        let gap = pace::next_reset_after(n.resets_at, t_out) - t_out;
                        out.push(Todo { next_account_id: Some(n.src.id.clone()), value: Some(gap as f64), at: Some(t_out), ..todo("short_gap", p) });
                    }
                }
                None => {}
            }
        }
    } else {
        // 還算不出速度：照舊（剩 ≤ 5%、離重置 ≥ 2 天、有券）
        for a in accs {
            if let Some(c) = first_credit(a, now) {
                if a.left <= 5.0 && a.resets_at - now >= MIN_CREDIT_WINDOW {
                    pressed.push(a.src.id.as_str());
                    out.push(Todo { account_id: Some(a.src.id.clone()), credit_id: Some(c.id.clone()), value: Some(round1(a.left)), ..todo("press_credit", p) });
                }
            }
        }
    }
    // 2. 券快到期：同一個到期時間只出一條
    let mut soon: Vec<(i64, &Acc, &ResetCredit)> = accs
        .iter()
        .flat_map(|a| a.src.credits.iter().map(move |c| (a, c)))
        .filter_map(|(a, c)| c.expires_at.filter(|t| *t > now && *t - now <= EXPIRING_SOON).map(|t| (t, a, c)))
        .collect();
    soon.sort_by_key(|x| x.0);
    soon.dedup_by_key(|x| x.0);
    for (t, a, c) in soon {
        out.push(Todo { account_id: Some(a.src.id.clone()), credit_id: Some(c.id.clone()), value: Some((t - now) as f64), ..todo("credit_expiring", p) });
    }
    // 3. 登入中的帳號用完了 → 換帳號
    let next = order.iter().find(|o| !o.exhausted);
    for a in accs.iter().filter(|a| a.src.live && a.left <= 0.0) {
        if let Some(n) = next {
            out.push(Todo { account_id: Some(a.src.id.clone()), next_account_id: Some(n.id.clone()), ..todo("switch_account", p) });
        }
    }
    // 4. 進度落後
    let frac = if input.day_fraction.is_finite() { input.day_fraction.clamp(0.0, 1.0) } else { 0.0 };
    let used = if input.used_today.is_finite() { input.used_today.max(0.0) } else { 0.0 };
    if frac >= 0.5 && target > 0.0 && used < target * frac * 0.6 {
        out.push(Todo { value: Some(round1(target - used)), ..todo("behind", p) });
    }
    // 5. 模型專屬的券（還沒到 48 小時內）：同一個到期時間只出一條
    let mut model: Vec<(i64, &Acc, &ResetCredit)> = accs
        .iter()
        .flat_map(|a| a.src.credits.iter().map(move |c| (a, c)))
        .filter(|(_, c)| c.kind == "model" && c.expires_at.map_or(true, |t| t - now > EXPIRING_SOON))
        .map(|(a, c)| (c.expires_at.unwrap_or(i64::MAX), a, c))
        .collect();
    model.sort_by_key(|x| x.0);
    model.dedup_by_key(|x| x.0);
    for (_, a, c) in model {
        out.push(Todo { account_id: Some(a.src.id.clone()), credit_id: Some(c.id.clone()), ..todo("model_credit", p) });
    }
    // 6. 有券但還不用按：只講順序最前面的那個帳號
    let holder = order.iter().find_map(|o| {
        accs.iter().find(|a| a.src.id == o.id && !pressed.contains(&o.id.as_str()) && a.src.credits.iter().any(|c| schedulable(c, now)))
    });
    if let Some(a) = holder {
        out.push(Todo { account_id: Some(a.src.id.clone()), value: Some(round1(a.left)), ..todo("hold", p) });
    }
    out.truncate(MAX_TODOS);
    out
}

pub fn plan(input: &PlanInput) -> PlanOutput {
    let now = input.now;
    let used_today = if input.used_today.is_finite() { round1(input.used_today) } else { 0.0 };
    let accs = normalize(input);
    if accs.is_empty() {
        return PlanOutput {
            provider: input.provider.clone(),
            target_today: 0.0,
            target_without_credits: 0.0,
            used_today,
            order: vec![],
            weeks: vec![],
            feasible: true,
            waste_estimate: None,
            todos: vec![],
        };
    }
    let latest_expiry = accs
        .iter()
        .flat_map(|a| a.src.credits.iter())
        .filter(|c| schedulable(c, now))
        .filter_map(|c| c.expires_at)
        .max();
    // 至少看到每個帳號的下一個完整週期，否則剛重置的帳號那 100% 不會算進目標
    let next_full = accs.iter().map(|a| a.resets_at + WEEK).max().unwrap_or(now + WEEK);
    let horizon = latest_expiry.unwrap_or(0).max(next_full);

    let mut cells = build_cycles(&accs, now, horizon);
    schedule(&mut cells, &accs, now);

    let target = required_rate(&cells, now, true);
    let target_plain = required_rate(&cells, now, false);
    let order = order_of(&accs);

    let feasible = input.max_daily.filter(|m| m.is_finite()).map_or(true, |m| target <= m.max(0.0));
    let waste_estimate = input.avg_daily.filter(|a| a.is_finite() && *a >= 0.0).and_then(|avg| {
        let end = cells.iter().map(|c| c.end).max().unwrap_or(now);
        let total: f64 = cells.iter().map(|c| c.base + 100.0 * c.credit_ids.len() as f64).sum();
        let waste = total - avg * days(end - now);
        (waste > 0.05).then(|| round1(waste))
    });
    let todos = build_todos(input, &accs, &order, target);
    for c in &mut cells {
        c.base = round1(c.base);
    }
    PlanOutput {
        provider: input.provider.clone(),
        target_today: round1(target),
        target_without_credits: round1(target_plain),
        used_today,
        order,
        weeks: cells,
        feasible,
        waste_estimate,
        todos,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(x: f64) -> i64 {
        (x * DAY as f64) as i64
    }
    fn acc(id: &str, left: f64, reset_days: f64, credits: Vec<ResetCredit>) -> PoolAccount {
        PoolAccount { id: id.into(), label: id.into(), weekly_left: left, resets_at: d(reset_days), live: false, credits }
    }
    fn credit(id: &str, kind: &str, exp_days: f64) -> ResetCredit {
        ResetCredit {
            id: id.into(),
            provider: "claude".into(),
            account: String::new(),
            kind: kind.into(),
            model: (kind == "model").then(|| "fable".to_string()),
            max_refund_percent: None,
            refill_five_hour: true,
            expires_at: Some(d(exp_days)),
            source: "manual".into(),
            title: None,
        }
    }
    fn input(accounts: Vec<PoolAccount>) -> PlanInput {
        PlanInput { provider: "claude".into(), now: 0, accounts, used_today: 0.0, day_fraction: 0.0, avg_daily: None, max_daily: None, pace: None }
    }
    fn cell_of<'a>(out: &'a PlanOutput, credit: &str) -> &'a WeekCell {
        out.weeks.iter().find(|w| w.credit_ids.iter().any(|c| c == credit)).expect("券沒被排到")
    }
    fn kinds(out: &PlanOutput) -> Vec<&str> {
        out.todos.iter().map(|t| t.kind.as_str()).collect()
    }

    #[test]
    fn case_a_three_accounts_no_credits() {
        let out = plan(&input(vec![acc("c", 100.0, 7.2, vec![]), acc("a", 8.0, 3.3, vec![]), acc("b", 73.0, 4.6, vec![])]));
        // 本週剩 181%（7.2 天內）＋三個帳號各一個完整的下一週 → 481% ÷ 14.2 天
        assert_eq!(out.target_today, 33.9);
        assert_eq!(out.target_today, out.target_without_credits);
        assert_eq!(out.order.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(out.weeks.len(), 6);
        assert!(out.todos.is_empty());
    }

    #[test]
    fn case_b_single_account_three_credits() {
        let credits = vec![credit("k3", "full", 28.7), credit("k1", "full", 16.6), credit("k2", "full", 17.6)];
        let out = plan(&input(vec![acc("x", 56.0, 5.6, credits)]));
        assert_eq!(cell_of(&out, "k1").index, 0);
        assert_eq!(cell_of(&out, "k2").index, 1);
        assert_eq!(cell_of(&out, "k3").index, 2);
        assert!(out.target_today >= 27.8, "{}", out.target_today);
        // 不算券：四個週期共 356% ÷ 26.6 天
        assert_eq!(out.target_without_credits, 13.4);
    }

    #[test]
    fn case_c_empty_pool() {
        let out = plan(&input(vec![]));
        assert_eq!(out.target_today, 0.0);
        assert!(out.order.is_empty() && out.weeks.is_empty() && out.todos.is_empty());
        assert!(out.feasible);
    }

    #[test]
    fn same_expiry_credits_are_spread_across_accounts() {
        let out = plan(&input(vec![
            acc("a", 60.0, 3.0, vec![credit("ka", "full", 26.0)]),
            acc("b", 60.0, 5.0, vec![credit("kb", "full", 26.0)]),
            acc("c", 60.0, 7.0, vec![credit("kc", "full", 26.0)]),
        ]));
        let (a, b, c) = (cell_of(&out, "ka"), cell_of(&out, "kb"), cell_of(&out, "kc"));
        assert!(!overlaps(a, b) && !overlaps(b, c) && !overlaps(a, c), "{a:?} {b:?} {c:?}");
        assert!(out.target_today > out.target_without_credits);
    }

    #[test]
    fn credit_is_not_scheduled_right_before_natural_reset() {
        // 5 小時後就重置：券不排在本週期
        let out = plan(&input(vec![acc("a", 0.0, 0.2, vec![credit("k", "full", 20.0)])]));
        assert_eq!(cell_of(&out, "k").index, 1);
        assert!(!kinds(&out).contains(&"press_credit"));
    }

    #[test]
    fn quota_about_to_reset_is_not_counted() {
        // 1 小時後重置的 50% 來不及用，不能把目標拉到 200%
        let out = plan(&input(vec![acc("a", 50.0, 0.04, vec![]), acc("b", 70.0, 7.0, vec![])]));
        // b 本週 70% ÷ 7 天 ＝ 10；加上 a 重置後的新一週：170% ÷ 7.04 天 ＝ 24.1
        assert_eq!(out.target_today, 24.1);
    }

    #[test]
    fn past_reset_time_counts_as_reset() {
        let mut a = acc("a", 0.0, 0.0, vec![]);
        a.resets_at = -DAY;
        let out = plan(&PlanInput { now: 0, ..input(vec![a]) });
        assert_eq!(out.order[0].weekly_left, 100.0);
        assert_eq!(out.order[0].resets_at, 6 * DAY);
        assert!(!out.order[0].exhausted);
    }

    #[test]
    fn exhausted_accounts_go_last_and_trigger_switch() {
        let mut gone = acc("gone", 0.0, 1.0, vec![]);
        gone.live = true;
        let out = plan(&input(vec![gone, acc("ok", 40.0, 4.0, vec![])]));
        assert_eq!(out.order.last().unwrap().id, "gone");
        assert!(out.order.last().unwrap().exhausted);
        let t = out.todos.iter().find(|t| t.kind == "switch_account").unwrap();
        assert_eq!(t.next_account_id.as_deref(), Some("ok"));
    }

    #[test]
    fn todo_press_credit_and_hold() {
        let out = plan(&input(vec![
            acc("low", 3.0, 4.0, vec![credit("k1", "full", 20.0)]),
            acc("high", 80.0, 5.0, vec![credit("k2", "full", 20.0)]),
        ]));
        assert_eq!(kinds(&out), ["press_credit", "hold"]);
        assert_eq!(out.todos[0].account_id.as_deref(), Some("low"));
        assert_eq!(out.todos[1].account_id.as_deref(), Some("high"));
    }

    #[test]
    fn no_press_when_other_accounts_last_until_reset() {
        let mut i = input(vec![
            acc("low", 3.0, 4.0, vec![credit("k1", "full", 20.0)]),
            acc("high", 80.0, 5.0, vec![]),
        ]);
        // 每天 15%：high 的 80% 撐得過 4 天 → 不用按
        i.pace = Some(15.0);
        assert!(!kinds(&plan(&i)).contains(&"press_credit"));
        // 每天 40%：2 天就用光 → 該按
        i.pace = Some(40.0);
        assert_eq!(kinds(&plan(&i))[0], "press_credit");
    }

    #[test]
    fn small_gap_is_a_reminder_not_a_press() {
        // 11-15 實例：每天 47%，ist7 1%（2.44 天後重置、有券 26 天後到期）、aixa 25%（3.78 天）、ruby 87%（6.4 天）
        // → 約 2.39 天後全用完，只比 ist7 重置早 1 小時多：按了只多用到約 3%，不值得
        let mut i = input(vec![
            acc("ist7", 1.0, 2.44, vec![credit("k1", "full", 26.0)]),
            acc("aixa", 25.0, 3.78, vec![]),
            acc("ruby", 87.0, 6.4, vec![]),
        ]);
        i.pace = Some(47.0);
        let out = plan(&i);
        assert!(!kinds(&out).contains(&"press_credit"));
        let gap = out.todos.iter().find(|t| t.kind == "short_gap").expect("要有 short_gap");
        assert_eq!(gap.next_account_id.as_deref(), Some("ist7"));
        let at = gap.at.unwrap();
        assert!(at > d(2.3) && at < d(2.44), "{at}");
        assert!(gap.value.unwrap() < 3.0 * 3600.0);
        // 同樣的缺口，但券 5 天後就過期（留到下週也用不到）→ 用掉
        i.accounts[0].credits = vec![credit("k1", "full", 5.0)];
        let out = plan(&i);
        let press = out.todos.iter().find(|t| t.kind == "press_credit").expect("要按");
        assert_eq!(press.at, Some(at));
        assert!(press.value.unwrap() < 5.0);
    }

    #[test]
    fn todo_expiring_dedups_same_expiry_and_model_credit() {
        let out = plan(&input(vec![
            acc("a", 50.0, 4.0, vec![credit("m1", "model", 1.0), credit("f1", "model", 9.0)]),
            acc("b", 50.0, 5.0, vec![credit("m2", "model", 1.0)]),
        ]));
        assert_eq!(kinds(&out), ["credit_expiring", "model_credit"]);
        assert_eq!(out.todos[0].value, Some(DAY as f64));
    }

    #[test]
    fn todo_behind_and_limit_of_three() {
        let mut i = input(vec![
            acc("a", 2.0, 4.0, vec![credit("k1", "full", 1.5), credit("m", "model", 12.0)]),
            acc("b", 90.0, 5.0, vec![credit("k2", "full", 30.0)]),
        ]);
        i.day_fraction = 0.8;
        i.used_today = 1.0;
        let out = plan(&i);
        assert_eq!(out.todos.len(), 3);
        assert_eq!(kinds(&out), ["press_credit", "credit_expiring", "behind"]);
        let behind = out.todos[2].value.unwrap();
        assert!((behind - (out.target_today - 1.0)).abs() < 0.11);
    }

    #[test]
    fn feasible_and_waste() {
        let mut i = input(vec![acc("a", 70.0, 7.0, vec![])]);
        i.max_daily = Some(5.0);
        i.avg_daily = Some(4.0);
        let out = plan(&i);
        assert_eq!(out.target_today, 12.1); // 170% ÷ 14 天
        assert!(!out.feasible);
        assert_eq!(out.waste_estimate, Some(114.0)); // 170 − 4 × 14
        i.max_daily = Some(30.0);
        i.avg_daily = Some(20.0);
        let out = plan(&i);
        assert!(out.feasible);
        assert_eq!(out.waste_estimate, None);
    }

    #[test]
    fn bad_numbers_never_leak() {
        let mut i = input(vec![acc("a", f64::NAN, 3.0, vec![]), acc("b", 250.0, 4.0, vec![]), acc("c", -5.0, 5.0, vec![])]);
        i.used_today = f64::INFINITY;
        i.day_fraction = f64::NAN;
        i.avg_daily = Some(f64::NAN);
        i.max_daily = Some(f64::NAN);
        let out = plan(&i);
        for v in [out.target_today, out.target_without_credits, out.used_today] {
            assert!(v.is_finite() && v >= 0.0);
        }
        assert!(out.order.iter().all(|o| (0.0..=100.0).contains(&o.weekly_left)));
        assert!(out.weeks.iter().all(|w| w.base.is_finite() && w.base >= 0.0));
    }
}
