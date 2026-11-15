import { invoke } from "@tauri-apps/api/core";
import { esc, hhmm, t } from "../util";

// ---- 與 Rust 端 statusline 約定（第 6 節）對應的資料格式 ----
export type ClaudeStatusAccount = {
  id: string;
  state: "installed" | "not_installed";
  last_at: number | null;
};

export type ClaudeStatuslineState = {
  show: boolean;
  accounts: ClaudeStatusAccount[];
};

/** 各 Claude 帳號這次額度從哪裡來（取自 snapshot 的 source／as_of；key＝帳號 key） */
export type ClaudeSources = Record<string, { source?: string | null; as_of?: number | null }>;

const FRESH_SEC = 600;

// ---- Tauri 指令包裝（指令不存在或失敗時回傳 null，不報錯） ----
export function getClaudeStatusline(): Promise<ClaudeStatuslineState | null> {
  return invoke<ClaudeStatuslineState>("get_claude_statusline").catch(() => null);
}

/** 失敗時回傳 { error }（例 settings_invalid），讓畫面能提示；指令不存在也算失敗。 */
export function installClaudeStatusline(id?: string): Promise<ClaudeStatuslineState | { error: string }> {
  return invoke<ClaudeStatuslineState>("install_claude_statusline", id ? { id } : {}).catch((e) => ({ error: String(e) }));
}

function failText(error: string): string {
  return error === "settings_invalid" ? t("cs.fail_invalid") : t("cs.fail");
}

export function setClaudeStatuslineShow(show: boolean): Promise<ClaudeStatuslineState | null> {
  return invoke<ClaudeStatuslineState>("set_claude_statusline_show", { show }).catch(() => null);
}

// ---- 純函式（時間判斷與 HTML 產生） ----

/** 判斷狀態列資料是否仍在新鮮期（10 分鐘內）。 */
export function isStatuslineFresh(asOf: number, nowSec = Math.floor(Date.now() / 1000)): boolean {
  return nowSec - asOf < FRESH_SEC;
}

/** 產生相對時間文字（N 分鐘前／N 小時前／N 天前）。 */
export function relativeTimeText(asOf: number, nowSec = Math.floor(Date.now() / 1000)): string {
  const diff = Math.max(0, Math.floor(nowSec - asOf));
  if (diff < 3600) return t("cs.ago_min", { n: Math.max(1, Math.floor(diff / 60)) });
  if (diff < 86400) return t("cs.ago_hour", { n: Math.floor(diff / 3600) });
  return t("cs.ago_day", { n: Math.floor(diff / 86400) });
}

/** 依更新時間回傳狀態列卡片的標籤樣式與文字。 */
export function statuslineTag(
  asOf?: number | null,
  nowSec = Math.floor(Date.now() / 1000),
): { cls: string; text: string } {
  if (asOf == null || isStatuslineFresh(asOf, nowSec)) {
    return { cls: "ok", text: t("tag.realtime") };
  }
  return { cls: "idle", text: relativeTimeText(asOf, nowSec) };
}

/** 產生狀態列卡片窗口下方的來源說明列 HTML。 */
export function statuslineNoteHtml(asOf: number, nowSec = Math.floor(Date.now() / 1000)): string {
  const text = isStatuslineFresh(asOf, nowSec)
    ? t("cs.src_fresh", { time: hhmm(asOf) })
    : t("cs.src_idle");
  return `<div class="src">${esc(text)}</div>`;
}

/** 產生需要設定狀態列時的卡片內容（說明文字＋一鍵設定按鈕）HTML。 */
export function missingSetupHtml(providerKey: string, installing = false, error: string | null = null): string {
  const btnText = installing ? t("cs.installing") : t("cs.install_btn");
  return `<div class="src warn">${esc(t("reason.claude_statusline_missing"))}</div><div class="cs-acts"><button class="btnp" type="button" data-cs-install="${esc(providerKey)}" ${installing ? "disabled" : ""}>${esc(btnText)}</button><span class="cs-dim">${esc(t("cs.install_hint"))}</span></div>${error ? `<div class="src warn">${esc(failText(error))}</div>` : ""}`;
}

/** 產生設定頁的「Claude 額度來源」區塊 HTML；state 為 null 時回傳空字串。 */
export function settingsSectionHtml(
  state: ClaudeStatuslineState | null,
  accountNames: Record<string, string> = {},
  installingId: string | null = null,
  failed: { id: string; error: string } | null = null,
  term: TermState | null = null,
  sources: ClaudeSources = {},
): string {
  if (!state) return "";
  const multi = state.accounts.length > 1;
  const rows = state.accounts
    .map((acct) => {
      const labelBase = t("cs.row_label");
      const acctLabel = accountNames[acct.id];
      const title = multi && acctLabel ? `${labelBase} · ${acctLabel}` : labelBase;
      const sub =
        acct.state === "installed"
          ? acct.last_at != null
            ? t("cs.state_installed_at", { time: hhmm(acct.last_at) })
            : t("cs.state_installed")
          : t("cs.state_not_installed");
      const busy = installingId === acct.id;
      const err = failed?.id === acct.id ? failText(failed.error) : "";
      const btnText = busy
        ? t("cs.installing")
        : acct.state === "installed"
          ? t("cs.reinstall_btn")
          : t("cs.install_btn");
      const src = sources[acct.id];
      const usageTitle = multi && acctLabel ? `${t("cs.usage_label")} · ${acctLabel}` : t("cs.usage_label");
      const usageSub = !src
        ? { cls: "", text: t("cs.usage_every") }
        : src.source === "live" && src.as_of != null
          ? { cls: "cs-ok", text: t("cs.usage_ok", { time: hhmm(src.as_of) }) }
          : { cls: "cs-warn", text: t(src.source === "statusline" ? "cs.usage_fallback" : "cs.usage_fail") };
      const usageRow = `<div class="srow"><span class="grow cs-label"><span>${esc(usageTitle)}<span class="cs-tag main">${esc(t("cs.tag_main"))}</span></span><small class="${usageSub.cls}">${esc(usageSub.text)}</small></span></div>`;
      return `${usageRow}<div class="srow"><span class="grow cs-label"><span>${esc(title)}<span class="cs-tag">${esc(t("cs.tag_backup"))}</span></span><small>${esc(sub)}</small><small>${esc(t("cs.backup_hint"))}</small>${err ? `<small class="cs-err">${esc(err)}</small>` : ""}</span><button class="pill" type="button" data-cs-install="${esc(acct.id)}" ${busy ? "disabled" : ""}>${esc(btnText)}</button></div>`;
    })
    .join("");
  return `<section class="sec">
      <h3>${esc(t("cs.title"))}</h3>
      ${rows}
      <div class="srow"><span class="grow cs-label"><span>${esc(t("cs.show_label"))}</span><small>${esc(t("cs.show_hint"))}</small></span><label class="switch"><input id="cs-show" type="checkbox" ${state.show ? "checked" : ""}><span></span></label></div>
      ${termSectionHtml(term, state.show)}
    </section>`;
}

// ---- 終端機要顯示哪些（Rust terminal.rs 的 get_terminal_line／set_terminal_line） ----
export type TermCfg = { claude: boolean; codex: boolean; agy: boolean; agy_off: string[]; used: boolean };
export type TermState = {
  cfg: TermCfg;
  agy_groups: { id: string; label: string; models: string | null; five: boolean }[];
};

export function getTerminalLine(): Promise<TermState | null> {
  return invoke<TermState>("get_terminal_line").catch(() => null);
}

export function setTerminalLine(cfg: TermCfg): Promise<TermState | null> {
  return invoke<TermState>("set_terminal_line", { cfg }).catch(() => null);
}

/** 設定頁「終端機要顯示哪些」：show（在終端機顯示額度）關掉時整區變灰 */
export function termSectionHtml(term: TermState | null, show: boolean): string {
  if (!term) return "";
  const c = term.cfg;
  const off = show ? "" : "disabled";
  const box = (attr: string, on: boolean, label: string, hint: string, sub = false) =>
    `<div class="term-opt${sub ? " sub" : ""}"><label><input type="checkbox" ${attr} ${on ? "checked" : ""} ${off}> ${esc(label)}</label>${hint ? ` <small>${esc(hint)}</small>` : ""}</div>`;
  const both = t("cs.term_both");
  const groups = term.agy_groups.map((g) => {
    const kinds = g.five ? both : t("cs.term_week_only");
    const hint = g.models ? `${kinds} · ${g.models.replace(/, /g, "、")}` : kinds;
    return box(`data-term-group="${esc(g.id)}"`, c.agy && !c.agy_off.includes(g.id), g.label, hint, true);
  }).join("");
  return `<div class="term-sec${show ? "" : " dim"}">
      <div class="term-h">${esc(t("cs.term_title"))}</div>
      ${box('data-term="claude"', c.claude, "Claude", both)}
      ${box('data-term="codex"', c.codex, "Codex", both)}
      ${box('data-term="agy"', c.agy, "Antigravity", "")}
      ${groups}
      <div class="srow"><span class="grow">${esc(t("cs.term_num"))}</span><span class="seg term-seg">
        <button type="button" data-term-used="0" class="${c.used ? "" : "on"}" ${off}>${esc(t("cs.term_left"))}</button>
        <button type="button" data-term-used="1" class="${c.used ? "on" : ""}" ${off}>${esc(t("cs.term_used"))}</button></span></div>
    </div>`;
}
