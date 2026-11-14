use crate::plan_types::Sample;
use std::path::Path;

const RETENTION: i64 = 14 * 24 * 60 * 60;
const THIRTY_MINUTES: i64 = 30 * 60;
const SIX_HOURS: i64 = 6 * 60 * 60;

pub fn load(path: &Path) -> Vec<Sample> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, h: &[Sample]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(h).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

pub fn record(h: &mut Vec<Sample>, new: &[Sample], now: i64) {
    h.retain(|sample| sample.t >= now.saturating_sub(RETENTION));
    for sample in new {
        let duplicate = h
            .iter()
            .filter(|old| old.account == sample.account && old.provider == sample.provider && old.t <= sample.t)
            .max_by_key(|old| old.t)
            .is_some_and(|old| {
                sample.t.saturating_sub(old.t) < THIRTY_MINUTES
                    && old.weekly_used == sample.weekly_used
                    && old.weekly_resets_at == sample.weekly_resets_at
                    && old.five_used == sample.five_used
            });
        if !duplicate {
            h.push(sample.clone());
        }
    }
    h.retain(|sample| sample.t >= now.saturating_sub(RETENTION));
    h.sort_by_key(|sample| sample.t);
}

/// 已用量大降多少以上才算「重置或用券」，以下當成來源的小誤差
const BIG_DROP: f64 = 20.0;

/// 兩筆相鄰樣本之間用掉多少：上升＝用掉；大降且重置時間沒變、還沒到重置＝按了券（不算用量）；
/// 大降而且已過重置＝新週期，用量是新的已用；小幅回降＝誤差（0）
fn step(prev: &Sample, cur: &Sample) -> f64 {
    if cur.weekly_used >= prev.weekly_used {
        return cur.weekly_used - prev.weekly_used;
    }
    if prev.weekly_used - cur.weekly_used < BIG_DROP {
        return 0.0;
    }
    if is_credit_drop(prev, cur) { 0.0 } else { cur.weekly_used }
}

/// 大降、重置時間沒變、還沒到重置 → 按了券
fn is_credit_drop(prev: &Sample, cur: &Sample) -> bool {
    prev.weekly_used - cur.weekly_used >= BIG_DROP
        && prev.weekly_resets_at.is_some()
        && prev.weekly_resets_at == cur.weekly_resets_at
        && prev.weekly_resets_at.is_some_and(|reset| cur.t < reset)
}

/// 某帳號從 from 起按券退回的量（已用量大降的總和）
pub fn refunds_since(h: &[Sample], account: &str, from: i64) -> f64 {
    let mut rows: Vec<&Sample> = h.iter().filter(|s| s.account == account && s.t >= from).collect();
    rows.sort_by_key(|s| s.t);
    rows.windows(2).filter(|p| is_credit_drop(p[0], p[1])).map(|p| p[0].weekly_used - p[1].weekly_used).sum()
}

/// 某帳號最早的一筆紀錄時間
pub fn first_sample(h: &[Sample], account: &str) -> Option<i64> {
    h.iter().filter(|s| s.account == account).map(|s| s.t).min()
}

pub fn used_between(h: &[Sample], account: &str, from: i64, to: i64) -> f64 {
    if to <= from { return 0.0; }
    let mut rows: Vec<&Sample> = h.iter().filter(|s| s.account == account && s.t >= from && s.t < to).collect();
    rows.sort_by_key(|s| s.t);
    rows.windows(2).map(|p| step(p[0], p[1])).sum()
}

pub fn daily_totals(h: &[Sample], accounts: &[String], days: u32, tz_offset: i64, now: i64) -> Vec<(i64, f64)> {
    if days == 0 { return Vec::new(); }
    let local_day = (now.saturating_add(tz_offset)).div_euclid(86400) * 86400 - tz_offset;
    let first_day = local_day.saturating_sub(i64::from(days.saturating_sub(1)) * 86400);
    let mut totals: Vec<(i64, f64)> = (0..i64::from(days)).map(|i| (first_day + i * 86400, 0.0)).collect();
    let mut rows: Vec<&Sample> = h.iter().filter(|s| accounts.contains(&s.account) && s.t <= now).collect();
    rows.sort_by_key(|s| s.t);
    for account in accounts {
        let own: Vec<&Sample> = rows.iter().copied().filter(|s| &s.account == account).collect();
        for pair in own.windows(2) {
            let (prev, cur) = (pair[0], pair[1]);
            let amount = step(prev, cur);
            if cur.t >= first_day && cur.t <= now {
                let index = ((cur.t + tz_offset).div_euclid(86400) * 86400 - tz_offset - first_day) / 86400;
                if let Some(day) = totals.get_mut(index as usize) { day.1 += amount; }
            }
        }
    }
    totals
}

pub fn looks_like_credit_use(prev: &Sample, cur: &Sample) -> bool {
    prev.provider == cur.provider && prev.account == cur.account
        && prev.weekly_resets_at.is_some_and(|reset| cur.t < reset)
        && prev.weekly_used - cur.weekly_used >= 50.0
}

pub fn account_at(h: &[Sample], provider: &str, t: i64) -> Option<String> {
    h.iter()
        .filter(|s| s.provider == provider && s.t <= t && t.saturating_sub(s.t) <= SIX_HOURS)
        .max_by_key(|s| s.t)
        .map(|s| s.account.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t: i64, account: &str, provider: &str, used: f64, reset: Option<i64>) -> Sample {
        Sample { t, account: account.into(), provider: provider.into(), weekly_used: used, weekly_resets_at: reset, five_used: None }
    }

    #[test]
    fn record_deduplicates_and_prunes() {
        let mut h = vec![sample(100, "a", "claude", 2.0, None)];
        record(&mut h, &[sample(100 + 29 * 60, "a", "claude", 2.0, None)], 100 + 29 * 60);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].weekly_used, 2.0);
        record(&mut h, &[], 100 + RETENTION + 1);
        assert!(h.is_empty());
    }

    #[test]
    fn computes_usage_with_resets_and_bounds() {
        let h = vec![sample(0,"a","claude",10.0,None),sample(1,"a","claude",25.0,None),sample(2,"a","claude",40.0,None), sample(3,"b","claude",90.0,None),sample(4,"b","claude",5.0,None),sample(5,"b","claude",20.0,None)];
        assert_eq!(used_between(&h,"a",0,3),30.0);
        assert_eq!(used_between(&h,"b",3,6),20.0);
        assert_eq!(used_between(&h,"a",1,3),15.0);
    }

    #[test]
    fn aggregates_local_days_and_empty_days() {
        let now = 3 * 86400 + 100;
        let h = vec![sample(now-2*86400,"a","claude",10.0,None),sample(now-86400,"a","claude",20.0,None), sample(now-86400+1,"b","claude",5.0,None),sample(now,"b","claude",15.0,None)];
        let days = daily_totals(&h,&["a".into(),"b".into()],4,0,now);
        assert_eq!(days, vec![(0,0.0),(86400,0.0),(172800,10.0),(259200,10.0)]);
        // 台灣時區（+8）：日界線是本地 00:00
        let tz = 8 * 3600;
        let h = vec![sample(86400 - tz - 10,"a","claude",10.0,None),sample(86400 - tz + 10,"a","claude",30.0,None)];
        let days = daily_totals(&h,&["a".into()],2,tz,86400 - tz + 20);
        assert_eq!(days, vec![(-tz,0.0),(86400 - tz,20.0)]);
    }

    #[test]
    fn small_drops_and_credit_drops_are_not_usage() {
        let r = Some(1000);
        // 4.8 → 5.3 → 4.0：只算 0.5，不把 4.0 當成重置後新用的
        let h = vec![sample(0,"g","agy",4.8,r),sample(1,"g","agy",5.3,r),sample(2,"g","agy",4.0,r)];
        assert!((used_between(&h,"g",0,3) - 0.5).abs() < 1e-9);
        // 按券：99 → 62（重置時間沒變）不算用量，記成退回 37
        let h = vec![sample(0,"a","claude",90.0,r),sample(10,"a","claude",99.0,r),sample(20,"a","claude",62.0,r),sample(30,"a","claude",70.0,r)];
        assert_eq!(used_between(&h,"a",0,31), 17.0);
        assert_eq!(refunds_since(&h,"a",0), 37.0);
        assert_eq!(refunds_since(&h,"a",15), 0.0);
        // 過了重置時間的大降＝新週期
        let h = vec![sample(0,"a","claude",90.0,Some(5)),sample(10,"a","claude",3.0,Some(5 + 604800))];
        assert_eq!(used_between(&h,"a",0,11), 3.0);
        assert_eq!(first_sample(&h,"a"), Some(0));
    }

    #[test]
    fn detects_credit_use_only_before_reset_and_at_threshold() {
        let prev = sample(10,"a","claude",96.0,Some(100));
        assert!(looks_like_credit_use(&prev,&sample(20,"a","claude",0.0,Some(100))));
        assert!(!looks_like_credit_use(&prev,&sample(100,"a","claude",0.0,Some(100))));
        assert!(!looks_like_credit_use(&prev,&sample(20,"a","claude",76.0,Some(100))));
    }

    #[test]
    fn resolves_recent_matching_provider() {
        let h = vec![sample(100,"old","claude",0.0,None),sample(200,"new","claude",0.0,None),sample(250,"other","codex",0.0,None)];
        assert_eq!(account_at(&h,"claude",300),Some("new".into()));
        assert_eq!(account_at(&h,"claude",200+SIX_HOURS+1),None);
        assert_eq!(account_at(&h,"codex",300),Some("other".into()));
    }
}
