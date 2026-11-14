// 額度規劃的前端接合：新版面板（開關 new_panel）、計畫分頁、設定頁的重置券登錄。
// 用 MutationObserver 接在既有畫面後面，不改 views/main.ts、views/settings.ts。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { $, dict, esc, zhTW } from "./util";
import { overviewHtml } from "./views/overview";
import { planHtml } from "./views/plan";
import type { PlanView } from "./views/planTypes";
import { creditsHtml, ct, readForm, type CreditFormState, type CreditKind } from "./views/credits";

type PlanCfg = { new_panel: boolean; model_usage: boolean };
type Deps = { getView: () => string; fit: () => void };

let planView: PlanView | null = null;
let cfg: PlanCfg = { new_panel: false, model_usage: true };
let tab: "now" | "plan" = "now";
let expanded: string | null = null;
// 收起的群組（CLAUDE、CODEX…），記在這台電腦，下次開面板一樣
const GROUPS_KEY = "plan.collapsed";
let collapsed = new Set<string>(loadCollapsed());
function loadCollapsed(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(GROUPS_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}
function saveCollapsed() {
  try {
    localStorage.setItem(GROUPS_KEY, JSON.stringify([...collapsed]));
  } catch {
    // 存不了就只在這次有效
  }
}
let form: CreditFormState = {};
let usedPick: string | null = null;
let deps: Deps;

const zh = () => dict === zhTW;
const tzOffset = () => -new Date().getTimezoneOffset() * 60;

// ---- 主畫面 ----
function paintMain() {
  const groups = $("groups");
  if (!cfg.new_panel || !planView || deps.getView() !== "main" || groups.querySelector(".plan-root")) return;
  const tabs = `<div class="plan-tabs"><button type="button" data-act="plan-tab" data-id="now" class="${tab === "now" ? "on" : ""}">${zh() ? "現在" : "Now"}</button><button type="button" data-act="plan-tab" data-id="plan" class="${tab === "plan" ? "on" : ""}">${zh() ? "計畫" : "Plan"}</button></div>`;
  groups.innerHTML = `<div class="plan-root">${tabs}${tab === "now" ? overviewHtml(planView, expanded, collapsed) : planHtml(planView)}</div>`;
  deps.fit();
}

function repaintMain() {
  $("groups").querySelector(".plan-root")?.remove();
  paintMain();
}

// ---- 設定頁 ----
function paintSettings() {
  const root = $("view-settings");
  const body = root.querySelector<HTMLElement>(".sbody");
  if (!body || deps.getView() !== "settings" || body.querySelector("#plan-settings")) return;
  const html = creditsHtml(planView?.manual_credits ?? [], planView?.claude_accounts ?? [], {
    ...form,
    toggles: { daily_goal: cfg.new_panel, model_usage: cfg.model_usage, codex_auto: true },
  });
  const sec = document.createElement("div");
  sec.id = "plan-settings";
  sec.className = "sec";
  sec.innerHTML = `<h3>${zh() ? "重置券與每日目標" : "Resets and daily target"}</h3>${html}`;
  // Codex 的券固定自動讀取，不提供開關
  sec.querySelector('[data-act="toggle-codex-auto"]')?.closest(".srow")?.remove();
  if (usedPick) {
    const credit = planView?.manual_credits.find((c) => c.id === usedPick);
    const acts = sec.querySelector(`[data-credit-id="${CSS.escape(usedPick)}"] .cr-it-acts`);
    if (credit && acts) {
      const label = (email: string) => planView?.claude_accounts.find((a) => a.email.toLowerCase() === email)?.label ?? email;
      const left = credit.accounts.filter((a) => !credit.used_by.includes(a));
      acts.innerHTML = `${left.map((a) => `<button type="button" class="pill" data-act="credit-used-acct" data-id="${esc(credit.id)}" data-account="${esc(a)}">${esc(label(a))}</button>`).join("")}<button type="button" class="pill" data-act="credit-used-cancel">${zh() ? "取消" : "Cancel"}</button>`;
    }
  }
  body.appendChild(sec);
  deps.fit();
}

function repaintSettings() {
  $("view-settings").querySelector("#plan-settings")?.remove();
  paintSettings();
}

/** 重畫前先把還沒送出的欄位值記下來 */
function keepForm(sec: ParentNode) {
  const val = (act: string) => sec.querySelector<HTMLInputElement | HTMLSelectElement>(`[data-act="${act}"]`)?.value;
  const on = (act: string) => sec.querySelector<HTMLInputElement>(`[data-act="${act}"]`)?.checked;
  form = {
    ...form,
    kind: (sec.querySelector<HTMLElement>('[data-act="credit-kind"].on')?.dataset.kind as CreditKind | undefined) ?? form.kind,
    model: val("credit-model") ?? form.model,
    max_refund_percent: val("credit-max-refund") ?? form.max_refund_percent,
    expires_date: val("credit-date") ?? form.expires_date,
    expires_time: val("credit-time") ?? form.expires_time,
    all_accounts: on("credit-all-accounts") ?? form.all_accounts,
    refill_five_hour: on("credit-refill-five") ?? form.refill_five_hour,
    accounts: sec.querySelector('[data-act="credit-account"]')
      ? Array.from(sec.querySelectorAll<HTMLInputElement>('[data-act="credit-account"]:checked')).map((e) => e.value)
      : form.accounts,
    error: null,
  };
}

async function editCredits(cmd: string, args: Record<string, unknown>): Promise<boolean> {
  const v = await invoke<PlanView | null>(cmd, args).catch(() => null);
  if (!v) return false;
  planView = v;
  return true;
}

async function onSettingsClick(e: Event) {
  const el = (e.target as HTMLElement).closest<HTMLElement>("#plan-settings [data-act]");
  const sec = el?.closest<HTMLElement>("#plan-settings");
  if (!el || !sec) return;
  const act = el.dataset.act;
  if (act === "credit-kind") {
    keepForm(sec);
    form.kind = el.dataset.kind as CreditKind;
  } else if (act === "credit-add") {
    keepForm(sec);
    const input = readForm(sec);
    if (typeof input === "string") form.error = input;
    else if (await editCredits("add_manual_credit", { credit: { id: "", used_by: [], ...input } })) form = { kind: form.kind };
    else form.error = "credits.err.save_failed";
  } else if (act === "credit-remove") {
    await editCredits("remove_manual_credit", { id: el.dataset.id });
  } else if (act === "credit-used") {
    keepForm(sec);
    usedPick = el.dataset.id ?? null;
  } else if (act === "credit-used-acct") {
    await editCredits("mark_credit_used", { id: el.dataset.id, account: el.dataset.account });
    usedPick = null;
  } else if (act === "credit-used-cancel") {
    usedPick = null;
  } else return;
  repaintSettings();
}

async function onSettingsChange(e: Event) {
  const el = (e.target as HTMLElement).closest<HTMLInputElement>("#plan-settings [data-act]");
  const sec = el?.closest<HTMLElement>("#plan-settings");
  if (!el || !sec) return;
  const act = el.dataset.act;
  if (act === "toggle-daily-goal" || act === "toggle-model-usage") {
    const args = act === "toggle-daily-goal" ? { newPanel: el.checked } : { modelUsage: el.checked };
    cfg = await invoke<PlanCfg>("set_plan_cfg", args).catch(() => cfg);
  } else if (act === "credit-all-accounts") {
    keepForm(sec);
    repaintSettings();
  }
}

function onMainClick(e: Event) {
  const el = (e.target as HTMLElement).closest<HTMLElement>(".plan-root [data-act]");
  if (!el) return;
  const act = el.dataset.act;
  if (act === "plan-tab") tab = el.dataset.id === "plan" ? "plan" : "now";
  else if (act === "toggle-row") expanded = expanded === el.dataset.id ? null : (el.dataset.id ?? null);
  else if (act === "toggle-group") {
    const g = el.dataset.id ?? "";
    if (!collapsed.delete(g)) collapsed.add(g);
    saveCollapsed();
  }
  else if (act === "credit-mark" || act === "credit-unmark") {
    // 「我按了」一鍵登記／取消，不跳視窗
    const cmd = act === "credit-mark" ? "mark_credit_used" : "unmark_credit_used";
    void editCredits(cmd, { id: el.dataset.id, account: el.dataset.account }).then(repaintMain);
    return;
  } else if (act === "open-url") {
    e.preventDefault();
    void invoke("open_usage_page", { url: el.dataset.id }).catch(() => {});
    return;
  } else return;
  repaintMain();
}

export function startPlan(d: Deps) {
  deps = d;
  // 既有畫面重畫（#groups、設定頁內容被換掉）後，把新版內容補回去
  new MutationObserver(paintMain).observe($("groups"), { childList: true });
  new MutationObserver(paintSettings).observe($("view-settings"), { childList: true });
  $("groups").addEventListener("click", onMainClick);
  $("view-settings").addEventListener("click", (e) => void onSettingsClick(e));
  $("view-settings").addEventListener("change", (e) => void onSettingsChange(e));
  void listen<PlanView>("plan", (e) => {
    planView = e.payload;
    if (deps.getView() === "main") repaintMain();
  });
  void (async () => {
    cfg = await invoke<PlanCfg>("get_plan_cfg").catch(() => cfg);
    planView = await invoke<PlanView | null>("get_plan_view", { tzOffset: tzOffset() }).catch(() => null);
    paintMain();
  })();
}

export { ct as planText };
