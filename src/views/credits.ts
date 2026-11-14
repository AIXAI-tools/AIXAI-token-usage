import { invoke } from "@tauri-apps/api/core";
import creditsEn from "../i18n/credits.en.json";
import creditsZh from "../i18n/credits.zh.json";
import { dict, esc, hhmm, mmdd, t, zhTW } from "../util";

// ---- 與 Rust plan_types.rs / 約定第 5 節對應的資料格式 ----

export type CreditKind = "full" | "weekly" | "five_hour" | "model";

export type ManualCredit = {
  id: string;
  provider: string;
  kind: CreditKind | string;
  model?: string | null;
  max_refund_percent?: number | null;
  refill_five_hour: boolean;
  expires_at?: number | null;
  accounts: string[];
  used_by: string[];
};

export type ManualCreditInput = {
  provider: "claude";
  kind: CreditKind;
  model: string | null;
  max_refund_percent: number | null;
  refill_five_hour: boolean;
  expires_at: number | null;
  accounts: string[];
};

export type ClaudeAccount = {
  email: string;
  label: string;
};

export type CreditToggles = {
  daily_goal: boolean;
  model_usage: boolean;
  codex_auto: boolean;
};

export type CreditFormState = {
  toggles?: Partial<CreditToggles>;
  kind?: CreditKind;
  model?: string;
  max_refund_percent?: number | string;
  expires_date?: string;
  expires_time?: string;
  all_accounts?: boolean;
  accounts?: string[];
  refill_five_hour?: boolean;
  error?: string | null;
};

const KINDS: readonly CreditKind[] = ["full", "weekly", "five_hour", "model"];
const MODELS: readonly string[] = ["fable", "opus", "sonnet", "haiku"];

const zhDict: Record<string, string> = creditsZh;
const enDict: Record<string, string> = creditsEn;

/** 讀取重置券專用翻譯（若全域字典已併入則優先使用全域字典） */
export function ct(key: string, vars: Record<string, string | number> = {}): string {
  const globalVal = t(key, vars);
  if (globalVal !== key) return globalVal;
  const table = dict === zhTW ? zhDict : enDict;
  let s = table[key] ?? key;
  for (const [k, v] of Object.entries(vars)) s = s.replace(`{${k}}`, String(v));
  return s;
}

// ---- Tauri 指令包裝（失敗回 null，不報錯） ----

export function addManualCredit(credit: ManualCreditInput): Promise<string | null> {
  return invoke<string>("add_manual_credit", { credit }).catch(() => null);
}

export function removeManualCredit(id: string): Promise<boolean | null> {
  return invoke<boolean>("remove_manual_credit", { id }).catch(() => null);
}

export function markCreditUsed(id: string, account: string): Promise<boolean | null> {
  return invoke<boolean>("mark_credit_used", { id, account }).catch(() => null);
}

// ---- 純函式：HTML 產生與表單驗證 ----

function formatModelName(model: string): string {
  const trimmed = model.trim();
  if (!trimmed) return "";
  return trimmed.charAt(0).toUpperCase() + trimmed.slice(1);
}

function toggleRowHtml(act: string, titleKey: string, descKey: string, on: boolean): string {
  return `<div class="srow"><span class="grow"><span>${esc(ct(titleKey))}</span><small>${esc(ct(descKey))}</small></span><label class="switch"><input type="checkbox" data-act="${esc(act)}" ${on ? "checked" : ""}><span></span></label></div>`;
}

function accountChecklistHtml(accounts: ClaudeAccount[], selectedLower: Set<string>): string {
  if (accounts.length === 0) return "";
  const items = accounts
    .map((acc) => {
      const emailLower = acc.email.trim().toLowerCase();
      const checked = selectedLower.has(emailLower);
      return `<label class="cr-chk cr-acct-item"><input type="checkbox" data-act="credit-account" value="${esc(acc.email)}" ${checked ? "checked" : ""}><span>${esc(acc.label)}</span><small>${esc(acc.email)}</small></label>`;
    })
    .join("");
  return `<div class="cr-accts">${items}</div>`;
}

function creditListItemHtml(credit: ManualCredit, labelByEmail: Map<string, string>): string {
  const kindText =
    credit.kind === "model" && credit.model
      ? `${ct("credits.kind.model")}：${formatModelName(credit.model)}`
      : ct(`credits.kind.${credit.kind}`);
  const expText =
    credit.expires_at != null ? `${mmdd(credit.expires_at)} ${hhmm(credit.expires_at)}` : ct("credits.no_expiry");
  const titleLine = `${kindText} · ${expText}`;

  const sep = dict === zhTW ? "、" : ", ";
  const acctNames = credit.accounts
    .map((email) => labelByEmail.get(email.toLowerCase()) ?? email)
    .join(sep);
  const subParts = [acctNames];
  if (credit.kind === "model" && credit.max_refund_percent != null) {
    subParts.push(ct("credits.max_refund_item", { n: credit.max_refund_percent }));
  }
  const subLine = subParts.filter(Boolean).join(" · ");

  return `<div class="cr-it" data-credit-id="${esc(credit.id)}"><span class="cr-it-info"><span>${esc(titleLine)}</span><small>${esc(subLine)}</small></span><span class="cr-it-acts"><button type="button" class="pill" data-act="credit-used" data-id="${esc(credit.id)}">${esc(ct("credits.mark_used"))}</button><button type="button" class="pill" data-act="credit-remove" data-id="${esc(credit.id)}">${esc(ct("credits.remove"))}</button></span></div>`;
}

/** 產生「設定頁 → 重置券與目標」區塊 HTML */
export function creditsHtml(
  manual: ManualCredit[],
  accounts: ClaudeAccount[],
  form: CreditFormState = {},
): string {
  const toggles: CreditToggles = {
    daily_goal: form.toggles?.daily_goal ?? true,
    model_usage: form.toggles?.model_usage ?? true,
    codex_auto: form.toggles?.codex_auto ?? true,
  };
  const kind: CreditKind = form.kind && KINDS.includes(form.kind) ? form.kind : "full";
  const model = form.model ?? "fable";
  const maxRefund = form.max_refund_percent ?? 50;
  const expiresDate = form.expires_date ?? "";
  const expiresTime = form.expires_time ?? "23:59";
  const allAccounts = form.all_accounts ?? true;
  const selectedSet = new Set((form.accounts ?? []).map((e) => e.trim().toLowerCase()));
  const refillFive = form.refill_five_hour ?? true;

  const togglesHtml = `<div class="cr-toggles">${toggleRowHtml("toggle-daily-goal", "credits.toggle.daily_goal", "credits.toggle.daily_goal_desc", toggles.daily_goal)}${toggleRowHtml("toggle-model-usage", "credits.toggle.model_usage", "credits.toggle.model_usage_desc", toggles.model_usage)}${toggleRowHtml("toggle-codex-auto", "credits.toggle.codex_auto", "credits.toggle.codex_auto_desc", toggles.codex_auto)}</div>`;

  const kindBtns = KINDS.map((k) => {
    const active = k === kind;
    return `<button type="button" class="cr-seg-btn${active ? " on" : ""}" data-act="credit-kind" data-kind="${esc(k)}" aria-pressed="${active ? "true" : "false"}">${esc(ct(`credits.kind.${k}`))}</button>`;
  }).join("");

  const modelOpts = MODELS.map((m) => {
    const sel = m === model.toLowerCase() ? " selected" : "";
    return `<option value="${esc(m)}"${sel}>${esc(formatModelName(m))}</option>`;
  }).join("");

  const modelFieldsHtml =
    kind === "model"
      ? `<div class="cr-two"><label class="cr-field"><span class="cr-label">${esc(ct("credits.model"))}</span><select class="cr-in" data-act="credit-model">${modelOpts}</select></label><label class="cr-field"><span class="cr-label">${esc(ct("credits.max_refund"))}</span><div class="cr-pct"><input type="number" class="cr-in" data-act="credit-max-refund" min="1" max="100" step="1" value="${esc(String(maxRefund))}"><span>%</span></div></label></div>`
      : "";

  const expiryFieldsHtml = `<div class="cr-two"><label class="cr-field"><span class="cr-label">${esc(ct("credits.expires_date"))}</span><input type="date" class="cr-in" data-act="credit-date" value="${esc(expiresDate)}"></label><label class="cr-field"><span class="cr-label">${esc(ct("credits.expires_time"))}</span><input type="time" class="cr-in" data-act="credit-time" value="${esc(expiresTime)}"></label></div>`;

  const allEmailsAttr = accounts.map((a) => a.email.trim().toLowerCase()).join(",");
  const allAccountsChk = `<label class="cr-chk"><input type="checkbox" data-act="credit-all-accounts" data-accounts="${esc(allEmailsAttr)}" ${allAccounts ? "checked" : ""}><span>${esc(ct("credits.all_accounts", { n: accounts.length }))}</span></label>`;
  const individualAcctsHtml = allAccounts ? "" : accountChecklistHtml(accounts, selectedSet);
  const refillChk = `<label class="cr-chk"><input type="checkbox" data-act="credit-refill-five" ${refillFive ? "checked" : ""}><span>${esc(ct("credits.refill_five"))}</span></label>`;
  const errHtml = form.error ? `<div class="cr-err" role="alert">${esc(ct(form.error))}</div>` : "";
  const addBtnHtml = `<div class="cr-actions"><button type="button" class="btnp" data-act="credit-add">${esc(ct("credits.add"))}</button></div>`;

  const labelByEmail = new Map<string, string>();
  for (const acc of accounts) {
    labelByEmail.set(acc.email.trim().toLowerCase(), acc.label);
  }
  const listHtml =
    manual.length > 0
      ? `<div class="cr-list">${manual.map((c) => creditListItemHtml(c, labelByEmail)).join("")}</div>`
      : `<div class="cr-empty">${esc(ct("credits.empty"))}</div>`;

  return `<div class="cr-wrap">
  ${togglesHtml}
  <h3 class="cr-subh">${esc(ct("credits.section_add"))}</h3>
  <div class="cr-field"><span class="cr-label">${esc(ct("credits.kind"))}</span><div class="cr-seg">${kindBtns}</div></div>
  ${modelFieldsHtml}
  ${expiryFieldsHtml}
  ${allAccountsChk}
  ${individualAcctsHtml}
  ${refillChk}
  ${errHtml}
  ${addBtnHtml}
  <h3 class="cr-subh">${esc(ct("credits.registered"))}</h3>
  ${listHtml}
</div>`;
}

/** 讀取並驗證表單欄位；通過回傳 ManualCreditInput，失敗回傳 i18n 錯誤 key */
export function readForm(
  root: ParentNode,
  nowSec = Math.floor(Date.now() / 1000),
): ManualCreditInput | string {
  const activeKindEl = root.querySelector<HTMLElement>('[data-act="credit-kind"].on');
  const rawKind = activeKindEl?.dataset.kind ?? "";
  if (!KINDS.includes(rawKind as CreditKind)) {
    return "credits.err.kind";
  }
  const kind = rawKind as CreditKind;

  let model: string | null = null;
  let maxRefund: number | null = null;
  if (kind === "model") {
    const modelEl = root.querySelector<HTMLSelectElement | HTMLInputElement>('[data-act="credit-model"]');
    const modelVal = (modelEl?.value ?? "").trim().toLowerCase();
    if (!modelVal) return "credits.err.model_required";
    model = modelVal;

    const refundEl = root.querySelector<HTMLInputElement>('[data-act="credit-max-refund"]');
    const refundNum = Number((refundEl?.value ?? "").trim());
    if (!Number.isFinite(refundNum) || refundNum <= 0 || refundNum > 100) {
      return "credits.err.max_refund_invalid";
    }
    maxRefund = refundNum;
  }

  const dateVal = (root.querySelector<HTMLInputElement>('[data-act="credit-date"]')?.value ?? "").trim();
  const timeVal = (root.querySelector<HTMLInputElement>('[data-act="credit-time"]')?.value ?? "").trim();
  if (!dateVal || !timeVal) return "credits.err.expires_required";

  const parsedMs = new Date(`${dateVal}T${timeVal}:00`).getTime();
  if (Number.isNaN(parsedMs)) return "credits.err.expires_invalid";
  const expiresAt = Math.floor(parsedMs / 1000);
  if (expiresAt <= nowSec) return "credits.err.expires_past";

  const allEl = root.querySelector<HTMLInputElement>('[data-act="credit-all-accounts"]');
  let rawAccounts: string[] = [];
  if (allEl?.checked) {
    rawAccounts = (allEl.dataset.accounts ?? "")
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
  } else {
    const checked = root.querySelectorAll<HTMLInputElement>('[data-act="credit-account"]:checked');
    rawAccounts = Array.from(checked)
      .map((el) => el.value.trim())
      .filter(Boolean);
  }
  const accounts = Array.from(new Set(rawAccounts.map((e) => e.toLowerCase())));
  if (accounts.length === 0) return "credits.err.accounts_empty";

  const refillFive = Boolean(root.querySelector<HTMLInputElement>('[data-act="credit-refill-five"]')?.checked);

  return {
    provider: "claude",
    kind,
    model,
    max_refund_percent: maxRefund,
    refill_five_hour: refillFive,
    expires_at: expiresAt,
    accounts,
  };
}
