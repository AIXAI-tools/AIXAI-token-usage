import zhTW from "./i18n/zh-TW.json";
import en from "./i18n/en.json";

// ---- 與 Rust 端 model.rs／lib.rs 對應的資料格式 ----
export type Reason = { key: string; detail?: string | null };
export type WindowLabel = { kind: "five_hour" | "weekly" | "minutes"; minutes: number; bucket?: string | null };
export type UsageWindow = {
  label: WindowLabel;
  used_percent: number;
  resets_at?: number | null;
  disabled?: boolean;
  reset_done?: boolean;
  group?: string | null;
  group_models?: string | null;
};
export type Platform = "codex" | "claude" | "antigravity";
export type Provider = {
  id: Platform;
  key: string;
  name: string;
  visible: boolean;
  removable: boolean;
  status: "ok" | "unavailable";
  source?: "live" | "local_log" | "cache" | "offline" | "statusline" | null;
  as_of?: number | null;
  reason?: Reason | null;
  account_masked?: string | null;
  account_full?: string | null;
  offline?: boolean;
  windows: UsageWindow[];
};
export type Snapshot = { updated_at?: number | null; refreshing: boolean; providers: Provider[] };
export type AccountDto = {
  key: string;
  platform: Platform;
  name: string;
  email_masked?: string | null;
  email_full?: string | null;
  visible: boolean;
  removable: boolean;
};
export type OfflineDto = { id: string; email_masked: string; queried_at: number; visible: boolean };
export type Settings = {
  language: string;
  zh: boolean;
  notify_threshold: number;
  notify_on_reset: boolean;
  autostart: boolean;
  accounts: AccountDto[];
  offline: OfflineDto[];
};

export type UpdateState = {
  current: string;
  latest?: string | null;
  has_update: boolean;
  notes: string;
  last_check?: number | null;
  checking: boolean;
  check_error?: string | null;
  stage: "idle" | "download" | "verify" | "install" | "error";
  percent: number;
  fail?: string | null;
};

export type ReplyDto = { rid: string; kind: string; version: string; text: string; urls: string[]; created: string; unread: boolean; can_update: boolean };
export type ReportDto = { id: string; title: string; ts: number; want_reply: boolean; replies: ReplyDto[] };
export type Prepare = { sys: string; status: string; log: string; log_kb: number; shot_b64?: string | null; shot_kb: number };
export type ViewName = "main" | "settings" | "update" | "report" | "reports";

// ---- 語言：預設跟隨系統，之後以 Rust 端設定為準（右鍵選單可切換） ----
export { zhTW };
export let dict: Record<string, string> = navigator.language.toLowerCase().startsWith("zh") ? zhTW : en;
export function applyLang(zh: boolean) {
  dict = zh ? zhTW : en;
  document.documentElement.lang = zh ? "zh-Hant" : "en";
}
export function t(key: string, vars: Record<string, string | number> = {}): string {
  let s = dict[key] ?? key;
  for (const [k, v] of Object.entries(vars)) s = s.replace(`{${k}}`, String(v));
  return s;
}

// ---- 使用者偏好（只存在本機，讀寫失敗也不影響功能） ----
export function load<T>(key: string, fallback: T): T {
  try { const v = localStorage.getItem(key); return v ? (JSON.parse(v) as T) : fallback; } catch { return fallback; }
}
export function save(key: string, value: unknown) {
  try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* 忽略 */ }
}

export const $ = (id: string) => document.getElementById(id)!;
export const esc = (s: string) => s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]!));

export function hhmm(epochSec: number): string {
  const d = new Date(epochSec * 1000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

// 重置倒數：不到 1 小時 9m；不到 1 天 1h9m；1 天以上 2d11h（中英相同）
export function formatRemaining(sec: number): string {
  sec = Math.max(0, Math.floor(sec));
  if (sec < 3600) return `${Math.floor(sec / 60)}m`;
  if (sec < 86400) return `${Math.floor(sec / 3600)}h${Math.floor((sec % 3600) / 60)}m`;
  return `${Math.floor(sec / 86400)}d${Math.floor((sec % 86400) / 3600)}h`;
}

export function mmdd(epochSec: number): string {
  const d = new Date(epochSec * 1000);
  return `${String(d.getMonth() + 1).padStart(2, "0")}/${String(d.getDate()).padStart(2, "0")}`;
}

export function copyText(text: string, done: () => void) {
  navigator.clipboard.writeText(text).then(done).catch(() => {
    const ta = document.createElement("textarea");
    ta.value = text; document.body.appendChild(ta); ta.select();
    try { document.execCommand("copy"); done(); } catch { /* 忽略 */ }
    ta.remove();
  });
}
