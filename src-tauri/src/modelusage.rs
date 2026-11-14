use crate::plan_types::{ModelShare, ModelUsage, ResetCredit};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Clone, Copy, Debug)]
pub struct Weights {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

const WEIGHT_TABLE: [(&str, Weights); 5] = [
    ("haiku", Weights { input: 1.0, output: 5.0, cache_read: 0.1, cache_write: 1.25 }),
    ("sonnet", Weights { input: 3.0, output: 15.0, cache_read: 0.3, cache_write: 3.75 }),
    ("opus", Weights { input: 5.0, output: 25.0, cache_read: 0.5, cache_write: 6.25 }),
    ("fable", Weights { input: 10.0, output: 50.0, cache_read: 1.0, cache_write: 12.5 }),
    ("other", Weights { input: 1.0, output: 5.0, cache_read: 0.1, cache_write: 1.25 }),
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLine { timestamp: Option<String>, request_id: Option<String>, message: Option<RawMessage> }
#[derive(Deserialize)]
struct RawMessage { id: Option<String>, model: Option<String>, usage: Option<RawUsage> }
#[derive(Deserialize)]
struct RawUsage {
    input_tokens: Option<u64>, output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>, cache_creation_input_tokens: Option<u64>,
}

pub fn family(model: &str) -> &'static str {
    let model = model.to_ascii_lowercase();
    ["opus", "sonnet", "haiku", "fable"].into_iter().find(|f| model.contains(f)).unwrap_or("other")
}

pub fn weight(family: &str) -> Weights {
    WEIGHT_TABLE.iter().find(|(name, _)| *name == family).map(|(_, w)| *w).unwrap_or(WEIGHT_TABLE[4].1)
}

fn weighted(f: &str, input: u64, output: u64, cache_read: u64, cache_write: u64) -> f64 {
    let w = weight(f);
    input as f64 * w.input + output as f64 * w.output + cache_read as f64 * w.cache_read + cache_write as f64 * w.cache_write
}

/// 回傳（時間, 用量, 去重用的鍵）。同一則回應會在紀錄裡出現多次（每個內容區塊一行），鍵相同的只算一次
fn parse_usage(line: &str) -> Option<(i64, ModelUsage, Option<String>)> {
    let row: RawLine = serde_json::from_str(line).ok()?;
    let timestamp = parse_iso8601(row.timestamp.as_deref()?)?;
    let msg = row.message?;
    let key = msg.id.as_ref().map(|id| format!("{id}:{}", row.request_id.as_deref().unwrap_or("")));
    let model = msg.model?;
    if model == "<synthetic>" { return None; }
    let usage = msg.usage?;
    let number = |v: Option<u64>| v.unwrap_or(0);
    let fam = family(&model);
    let (input, output, cache_read, cache_write) = (
        number(usage.input_tokens), number(usage.output_tokens),
        number(usage.cache_read_input_tokens), number(usage.cache_creation_input_tokens),
    );
    Some((timestamp, ModelUsage {
        family: fam.to_string(), input, output, cache_read, cache_write,
        weighted: weighted(fam, input, output, cache_read, cache_write),
    }, key))
}

pub fn scan(projects_dir: &Path, since: i64) -> Vec<(i64, ModelUsage)> {
    fn visit(path: &Path, since: i64, rows: &mut Vec<(i64, ModelUsage)>, seen: &mut std::collections::HashSet<String>) {
        let Ok(entries) = fs::read_dir(path) else { return };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() { visit(&p, since, rows, seen); continue; }
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") { continue; }
            let Ok(meta) = fs::metadata(&p) else { continue };
            let Ok(modified) = meta.modified() else { continue };
            let Ok(modified) = modified.duration_since(UNIX_EPOCH) else { continue };
            if modified.as_secs() < since.max(0) as u64 { continue; }
            let Ok(file) = fs::File::open(&p) else { continue };
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if !line.contains("\"usage\"") { continue; }
                if let Some((t, usage, key)) = parse_usage(&line) {
                    if key.is_some_and(|k| !seen.insert(k)) { continue; }
                    rows.push((t, usage));
                }
            }
        }
    }
    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    visit(projects_dir, since, &mut rows, &mut seen);
    rows
}

pub fn by_account(
    rows: &[(i64, ModelUsage)],
    account_at: &dyn Fn(i64) -> Option<String>,
    since_of: &dyn Fn(&str) -> i64,
) -> HashMap<String, Vec<ModelUsage>> {
    let mut out: HashMap<String, Vec<ModelUsage>> = HashMap::new();
    for (t, usage) in rows {
        if let Some(account) = account_at(*t) {
            if *t >= since_of(&account) { out.entry(account).or_default().push(usage.clone()); }
        }
    }
    out
}

pub fn shares(usages: &[ModelUsage], weekly_used: f64) -> Vec<ModelShare> {
    let total: f64 = usages.iter().map(|u| u.weighted).sum();
    if total <= 0.0 || !total.is_finite() { return Vec::new(); }
    let mut totals: HashMap<&str, f64> = HashMap::new();
    for u in usages { *totals.entry(&u.family).or_default() += u.weighted; }
    let mut result: Vec<_> = totals.into_iter().map(|(f, w)| ModelShare {
        family: f.to_string(), percent_of_week: weekly_used * w / total,
    }).collect();
    result.sort_by(|a, b| b.percent_of_week.total_cmp(&a.percent_of_week).then_with(|| a.family.cmp(&b.family)));
    result
}

pub fn summary(shares: &[ModelShare], credit_families: &[String]) -> Vec<ModelShare> {
    let Some(rep) = shares.iter().max_by(|a, b| a.percent_of_week.total_cmp(&b.percent_of_week)) else { return Vec::new() };
    let mut out = vec![rep.clone()];
    for s in shares {
        if s.family != rep.family && credit_families.iter().any(|f| f == &s.family) { out.push(s.clone()); }
    }
    out
}

pub fn refund_estimate(shares: &[ModelShare], credit: &ResetCredit) -> f64 {
    let percent = shares.iter().find(|s| Some(s.family.as_str()) == credit.model.as_deref()).map(|s| s.percent_of_week).unwrap_or(0.0);
    percent.min(credit.max_refund_percent.unwrap_or(f64::INFINITY)).max(0.0)
}

fn parse_iso8601(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' { return None; }
    let n = |a: usize, z: usize| std::str::from_utf8(b.get(a..z)?).ok()?.parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (n(0,4)?, n(5,7)?, n(8,10)?, n(11,13)?, n(14,16)?, n(17,19)?);
    if !(1..=12).contains(&m) || !(0..=23).contains(&hh) || !(0..=59).contains(&mm) || !(0..=59).contains(&ss) { return None; }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if d < 1 || d > days[(m - 1) as usize] { return None; }
    // Gregorian calendar to days since 1970-01-01.
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = era * 146097 + doe - 719468;
    let mut tz_pos = 19;
    if b.get(tz_pos) == Some(&b'.') {
        tz_pos += 1;
        let fraction_start = tz_pos;
        while b.get(tz_pos).is_some_and(u8::is_ascii_digit) { tz_pos += 1; }
        if tz_pos == fraction_start { return None; }
    }
    let offset = match b.get(tz_pos) {
        Some(b'Z') if tz_pos + 1 == b.len() => 0,
        Some(sign @ (b'+' | b'-')) if tz_pos + 6 == b.len() && b[tz_pos+3] == b':' => {
            let oh = n(tz_pos+1, tz_pos+3)?; let om = n(tz_pos+4, tz_pos+6)?;
            if oh > 23 || om > 59 { return None; }
            (oh * 3600 + om * 60) * if *sign == b'+' { 1 } else { -1 }
        },
        _ => return None,
    };
    Some(days_since_epoch * 86400 + hh * 3600 + mm * 60 + ss - offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn families_and_iso_times() {
        assert_eq!(family("claude-opus-5-5"), "opus"); assert_eq!(family("sonnet"), "sonnet");
        assert_eq!(family("haiku"), "haiku"); assert_eq!(family("fable"), "fable"); assert_eq!(family("unknown"), "other");
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601("1970-01-01T00:00:00.123Z"), Some(0));
        assert_eq!(parse_iso8601("2026-11-14T06:20:03.123Z"), Some(1794637203));
    }

    #[test]
    fn scan_filters_rows_and_old_files() {
        let root = std::env::temp_dir().join(format!("modelusage-{}", std::process::id()));
        fs::create_dir_all(root.join("nested")).unwrap();
        let file = root.join("nested/a.jsonl");
        fs::write(&file, concat!(
            "{\"timestamp\":\"2026-11-14T06:20:03.123Z\",\"message\":{\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1},\"content\":[\"PRIVATE\"]}}\n",
            "{\"timestamp\":\"2026-11-14T06:20:03Z\",\"message\":{\"model\":\"x\"}}\n",
            "{\"timestamp\":\"2026-11-14T06:20:03Z\",\"message\":{\"model\":\"<synthetic>\",\"usage\":{}}}\n",
            "{broken \"usage\"\n")).unwrap();
        let modified = fs::metadata(&file).unwrap().modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        assert!(scan(&root, modified + 1).is_empty());
        assert_eq!(scan(&root, 0).len(), 1);
    }

    #[test]
    fn accounts_shares_summary_and_refund() {
        let u = ModelUsage { family:"opus".into(), input:1, weighted:2.0, ..Default::default() };
        let rows = vec![(10,u.clone()),(20,u.clone()),(30,u.clone())];
        let grouped = by_account(&rows, &|t| if t == 30 { None } else if t == 10 { Some("a".into()) } else { Some("b".into()) }, &|a| if a == "b" { 21 } else { 0 });
        assert_eq!(grouped.get("a").unwrap().len(),1); assert!(!grouped.contains_key("b"));
        let usages = vec![u, ModelUsage { family:"fable".into(), weighted:1.0, ..Default::default() }];
        let s = shares(&usages, 12.0); assert!((s.iter().map(|x| x.percent_of_week).sum::<f64>() - 12.0).abs() < 1e-9);
        assert!(shares(&[ModelUsage::default()], 12.0).is_empty());
        assert_eq!(summary(&s, &["fable".into()]).len(),2);
        let credit = ResetCredit { id:"x".into(), provider:"claude".into(), account:"a".into(), kind:"model".into(), model:Some("opus".into()), max_refund_percent:Some(1.0), refill_five_hour:false, expires_at:None, source:"manual".into(), title:None };
        assert_eq!(refund_estimate(&s, &credit),1.0);
    }

    #[test]
    fn same_response_logged_twice_counts_once() {
        let dir = std::env::temp_dir().join(format!("aitoken-mu-dedup-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let line = r#"{"timestamp":"2026-11-14T06:20:03.123Z","requestId":"req_1","message":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let other = r#"{"timestamp":"2026-11-14T06:21:00Z","requestId":"req_2","message":{"id":"msg_2","model":"claude-opus-5-5","usage":{"input_tokens":1,"output_tokens":2}}}"#;
        fs::write(dir.join("a.jsonl"), format!("{line}
{line}
{other}
")).unwrap();
        let rows = scan(&dir, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().map(|(_, u)| u.output).sum::<u64>(), 22);
    }
}
