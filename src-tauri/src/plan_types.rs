// 額度規劃共用資料格式（約定：docs/ai/contracts/quota-plan.md）。
// 只放資料結構，不放邏輯；credits / history / planner / modelusage 四個模組共用。
use serde::{Deserialize, Serialize};

/// 一張可用的重置券（自動讀到的，或手動登錄展開後的每帳號一張）
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ResetCredit {
    pub id: String,
    /// codex | claude
    pub provider: String,
    /// 帳號識別：email 小寫；不知道時為空字串
    pub account: String,
    /// full（每週＋5 小時）| weekly | five_hour | model（模型專屬）
    pub kind: String,
    /// kind == model 時的模型家族，小寫：fable / opus / sonnet / haiku
    #[serde(default)]
    pub model: Option<String>,
    /// kind == model 時最多退回每週額度的幾 %
    #[serde(default)]
    pub max_refund_percent: Option<f64>,
    /// 是否同時補滿 5 小時
    #[serde(default)]
    pub refill_five_hour: bool,
    /// Unix 秒；None＝不會到期
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// auto | manual
    pub source: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// 手動登錄的券（reset-credits.json 的一筆），可套用多個帳號
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ManualCredit {
    pub id: String,
    pub provider: String,
    pub kind: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub max_refund_percent: Option<f64>,
    #[serde(default)]
    pub refill_five_hour: bool,
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// 套用的帳號（email 小寫）
    pub accounts: Vec<String>,
    /// 已經用掉的帳號（email 小寫）
    #[serde(default)]
    pub used_by: Vec<String>,
    /// 帳號 → 按「我按了」的時間（Unix 秒）
    #[serde(default)]
    pub used_at: std::collections::BTreeMap<String, i64>,
}

/// 用量歷史的一筆（history.json）
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Sample {
    pub t: i64,
    /// 池內帳號識別，見約定第 2 節 pool_key
    pub account: String,
    pub provider: String,
    pub weekly_used: f64,
    #[serde(default)]
    pub weekly_resets_at: Option<i64>,
    #[serde(default)]
    pub five_used: Option<f64>,
}

/// planner 的輸入：一個平台＝一池
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PoolAccount {
    pub id: String,
    pub label: String,
    /// 本週剩餘 0～100
    pub weekly_left: f64,
    /// 本週重置時間（Unix 秒）
    pub resets_at: i64,
    /// 目前登入中（不是離線卡片）
    pub live: bool,
    /// 這個帳號可用的券
    pub credits: Vec<ResetCredit>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PlanInput {
    pub provider: String,
    pub now: i64,
    pub accounts: Vec<PoolAccount>,
    /// 今天（本地 00:00 起）整池已用掉的 %
    pub used_today: f64,
    /// 今天過了多少比例 0～1
    pub day_fraction: f64,
    /// 最近 7 天平均每日用量；資料不足 1 天為 None
    pub avg_daily: Option<f64>,
    /// 最近 14 天用最多的一天
    pub max_daily: Option<f64>,
    /// 整池平均每天用多少 %（各帳號「已用 ÷ 已過天數」相加）；算不出來是 None
    #[serde(default)]
    pub pace: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct OrderItem {
    pub id: String,
    pub label: String,
    pub weekly_left: f64,
    pub resets_at: i64,
    pub exhausted: bool,
}

/// 某帳號某一個週期要用掉多少
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WeekCell {
    pub account_id: String,
    /// 0＝本週期
    pub index: u32,
    pub start: i64,
    pub end: i64,
    /// 這個週期本來的額度（本週期＝目前剩餘，之後＝100）
    pub base: f64,
    /// 排在這個週期用的券 id
    pub credit_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Todo {
    /// press_credit | short_gap | credit_expiring | switch_account | behind | hold | model_credit
    pub kind: String,
    pub provider: String,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub credit_id: Option<String>,
    /// 給畫面組句子的數字（例：到期秒數、落後 %、可退回 %）
    #[serde(default)]
    pub value: Option<f64>,
    /// 接著建議用的帳號（short_gap：最先重置的帳號）
    #[serde(default)]
    pub next_account_id: Option<String>,
    /// press_credit、short_gap：整池照平均速度全用完的時間
    #[serde(default)]
    pub at: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PlanOutput {
    pub provider: String,
    pub target_today: f64,
    pub target_without_credits: f64,
    pub used_today: f64,
    pub order: Vec<OrderItem>,
    pub weeks: Vec<WeekCell>,
    /// 目標超過 max_daily 時為 false
    pub feasible: bool,
    /// 照 avg_daily 會浪費多少 %；None＝資料不足或不會浪費
    pub waste_estimate: Option<f64>,
    pub todos: Vec<Todo>,
}

/// 某模型在某帳號本週的用量
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ModelUsage {
    /// 模型家族小寫：fable / opus / sonnet / haiku / other
    pub family: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// 加權後的量（跨模型比較用）
    pub weighted: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ModelShare {
    pub family: String,
    /// 佔每週額度的 %（估算）
    pub percent_of_week: f64,
}
