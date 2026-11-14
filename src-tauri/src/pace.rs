//! 平均速度：本週已用 ÷ 週期已過天數，以及照這個速度往後推算（池子撐不撐得到重置、每天大約用多少）。
//! 純函式：不讀檔、不取系統時間。規則見 docs/ai/contracts/quota-plan.md 第 7 節。

const HOUR: i64 = 3600;
const DAY: i64 = 86_400;
const WEEK: i64 = 7 * DAY;
/// 週期開始不到這麼久，速度不準，不算
const MIN_ELAPSED: i64 = 12 * HOUR;

fn days(secs: i64) -> f64 {
    secs as f64 / DAY as f64
}

/// 週期開始時間
pub fn cycle_start(resets_at: i64) -> i64 {
    resets_at - WEEK
}

/// 每天平均用多少 %：（現在已用＋本週按券退回的）÷ 週期已過天數。as_of＝資料時間
pub fn rate(used: f64, refunded: f64, resets_at: i64, as_of: i64) -> Option<f64> {
    let elapsed = as_of - cycle_start(resets_at);
    if elapsed < MIN_ELAPSED || !used.is_finite() {
        return None;
    }
    Some(((used.max(0.0) + refunded.max(0.0)) / days(elapsed)).max(0.0))
}

/// 照每天 rate% 的速度從 now 用下去，帳號照「最快重置的先用」，重置就補回 100%。
/// 回傳池子第一次全空的時間；撐到 until 都沒空就是 None。accs：（剩餘 %, 重置時間）
pub fn run_out(accs: &[(f64, i64)], rate: f64, now: i64, until: i64) -> Option<i64> {
    if !(rate > 0.0) || accs.is_empty() {
        return None;
    }
    let mut state: Vec<(f64, i64)> = accs.iter().map(|&(l, r)| (l.clamp(0.0, 100.0), r)).collect();
    let per_hour = rate / 24.0;
    let mut t = now;
    let end = until.min(now + 15 * DAY);
    while t < end {
        for a in state.iter_mut() {
            while a.1 <= t {
                a.0 = 100.0;
                a.1 += WEEK;
            }
        }
        state.sort_by_key(|a| a.1);
        let mut need = per_hour;
        for a in state.iter_mut() {
            let take = need.min(a.0);
            a.0 -= take;
            need -= take;
            if need <= 1e-9 {
                break;
            }
        }
        if need > 1e-9 {
            return Some(t);
        }
        t += HOUR;
    }
    None
}

/// 單一帳號照自己的速度：會提早用完（回傳用完時間）、還會剩多少、或剛好
#[derive(Debug, PartialEq)]
pub enum Outlook {
    Early(i64),
    Leftover(f64),
    Even,
}

pub fn outlook(left: f64, rate: f64, resets_at: i64, now: i64) -> Outlook {
    let span = days(resets_at - now).max(0.0);
    if rate <= 0.0 {
        return if left >= 5.0 { Outlook::Leftover(left) } else { Outlook::Even };
    }
    let finish = left / rate;
    if finish < span - 0.25 {
        return Outlook::Early(now + (finish * DAY as f64) as i64);
    }
    let rest = left - rate * span;
    if rest >= 5.0 { Outlook::Leftover(rest) } else { Outlook::Even }
}

/// 一個平台一句話
#[derive(Debug, PartialEq)]
pub enum Status {
    /// 還沒有任何帳號的速度
    Learning,
    /// 照速度會在這個時間全用光
    Short(i64),
    /// 用不完：每天可以多用這麼多
    More(f64),
    Ok,
}

pub fn status(accs: &[(f64, i64)], pace: Option<f64>, target: f64, now: i64) -> Status {
    let Some(pace) = pace else { return Status::Learning };
    let until = accs.iter().map(|a| a.1).max().unwrap_or(now);
    if let Some(t) = run_out(accs, pace, now, until) {
        return Status::Short(t);
    }
    let more = target - pace;
    if more >= 2.0 { Status::More(more) } else { Status::Ok }
}

/// 推算用的帳號資料
pub struct EstAcc {
    pub rate: Option<f64>,
    pub cycle_start: i64,
    /// 資料時間（離線帳號是換出時間）
    pub as_of: i64,
    /// 用量歷史裡最早的一筆；之後的用量看紀錄，不推算
    pub first_sample: Option<i64>,
}

/// 每一天的用量：紀錄＋還沒有紀錄的那段用平均速度推算；完全看不到的日子是 None
pub fn fill_days(recorded: &[(i64, f64)], accs: &[EstAcc]) -> Vec<(i64, Option<f64>)> {
    let coverage = accs
        .iter()
        .filter_map(|a| match (a.rate, a.first_sample) {
            (Some(_), Some(f)) => Some(a.cycle_start.min(f)),
            (Some(_), None) => Some(a.cycle_start),
            (None, f) => f,
        })
        .min();
    recorded
        .iter()
        .map(|&(day, used)| {
            let end = day + DAY;
            if coverage.map_or(true, |c| end <= c) {
                return (day, None);
            }
            let est: f64 = accs
                .iter()
                .filter_map(|a| {
                    let r = a.rate?;
                    let stop = a.first_sample.map_or(a.as_of, |f| f.min(a.as_of));
                    let overlap = end.min(stop) - day.max(a.cycle_start);
                    (overlap > 0).then(|| r * days(overlap))
                })
                .sum();
            (day, Some(used + est))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_from_used_and_elapsed() {
        // 用了 50%、過了 5 天 → 10%/天；按券退回 30% 要加回去
        let reset = 7 * DAY;
        assert_eq!(rate(50.0, 0.0, reset, 5 * DAY), Some(10.0));
        assert_eq!(rate(20.0, 30.0, reset, 5 * DAY), Some(10.0));
        // 週期剛開始不到 12 小時不算
        assert_eq!(rate(13.0, 0.0, reset, 2 * HOUR), None);
    }

    #[test]
    fn outlook_early_leftover_even() {
        // 剩 50%、2 天後重置：每天 10% → 剩 30%；每天 25% → 剛好；每天 50% → 1 天後用完
        assert_eq!(outlook(50.0, 10.0, 2 * DAY, 0), Outlook::Leftover(30.0));
        assert_eq!(outlook(50.0, 25.0, 2 * DAY, 0), Outlook::Even);
        assert_eq!(outlook(50.0, 50.0, 2 * DAY, 0), Outlook::Early(DAY));
    }

    #[test]
    fn pool_run_out_uses_soonest_reset_first_and_refills() {
        // 實例：aixa 47%（4.3 天）、ruby 87%（6.9 天）、ist 1%（3 天）共 135%；每天 44% 撐得過 ist 重置（2.96 天要 130%）
        let accs = [(1.0, (2.96 * DAY as f64) as i64), (47.0, (4.29 * DAY as f64) as i64), (87.0, (6.92 * DAY as f64) as i64)];
        assert_eq!(run_out(&accs, 44.0, 0, 7 * DAY), None);
        // 每天 60% 就撐不到
        let t = run_out(&accs, 60.0, 0, 7 * DAY).unwrap();
        assert!(t > 2 * DAY && t < 3 * DAY, "{t}");
        assert_eq!(run_out(&accs, 0.0, 0, 7 * DAY), None);
    }

    #[test]
    fn status_sentences() {
        let accs = [(50.0, 3 * DAY), (80.0, 6 * DAY)];
        assert_eq!(status(&accs, None, 30.0, 0), Status::Learning);
        assert_eq!(status(&accs, Some(20.0), 27.0, 0), Status::More(7.0));
        assert_eq!(status(&accs, Some(26.0), 27.0, 0), Status::Ok);
        assert!(matches!(status(&accs, Some(80.0), 27.0, 0), Status::Short(_)));
    }

    #[test]
    fn days_are_estimated_until_first_record() {
        // 帳號週期從第 2 天開始、每天 20%；第 4 天中午開始有紀錄
        let accs = [EstAcc { rate: Some(20.0), cycle_start: 2 * DAY, as_of: 5 * DAY, first_sample: Some(4 * DAY + DAY / 2) }];
        let recorded: Vec<(i64, f64)> = (0..6).map(|i| (i * DAY, if i >= 4 { 3.0 } else { 0.0 })).collect();
        let out = fill_days(&recorded, &accs);
        assert_eq!(out[0], (0, None));
        assert_eq!(out[1], (DAY, None));
        assert_eq!(out[2], (2 * DAY, Some(20.0)));
        assert_eq!(out[3], (3 * DAY, Some(20.0)));
        assert_eq!(out[4], (4 * DAY, Some(13.0)));
        assert_eq!(out[5], (5 * DAY, Some(3.0)));
    }
}
