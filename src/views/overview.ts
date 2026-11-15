import { esc, formatRemaining } from "../util";
import zh from "../i18n/plan.zh.json";
import en from "../i18n/plan.en.json";
import type { DetailCredit, PlanView, ProviderId } from "./planTypes";

const isEn = () => document.documentElement.lang.toLowerCase().startsWith("en");
const msg = (key: keyof typeof zh, vars: Record<string, string | number> = {}) => {
  let s = (isEn() ? en : zh)[key];
  for (const [k, v] of Object.entries(vars)) s = s.split(`{${k}}`).join(String(v));
  return s;
};
const names: Record<ProviderId, string> = { claude: "Claude", codex: "Codex", antigravity: "Antigravity" };
const pct = (n: number) => `${Math.round(n)}%`;
const clamp = (n: number) => Math.max(0, Math.min(100, n));
const pad = (n: number) => String(n).padStart(2, "0");
const SIX_HOURS = 6 * 3600;

/** 11/16 23:59 */
function shortDate(t: number): string {
  const d = new Date(t * 1000);
  return `${d.getMonth() + 1}/${d.getDate()} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 週一 08:00 */
function weekdayTime(t: number): string {
  const d = new Date(t * 1000);
  const day = new Intl.DateTimeFormat(isEn() ? "en" : "zh-TW", { weekday: "short" }).format(d);
  return `${day} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

const modelName = (m: string | null | undefined) => (m ? m.charAt(0).toUpperCase() + m.slice(1) : "");

function todoText(todo: PlanView["todos"][number], now: number): string {
  const x = todo as PlanView["todos"][number] & { next_label?: string | null };
  const vars = { provider: names[x.provider], account: x.account_label ?? "", next: x.next_label ?? "", model: modelName(x.model),
    time: formatRemaining(x.value ?? 0), value: Math.round(x.value ?? 0), date: x.expires_at ? shortDate(x.expires_at) : "" };
  switch (x.kind) {
    case "credit_expiring":
      if (x.credit_kind !== "model") return msg("todo.credit_expiring", vars);
      return msg((x.value ?? 0) <= SIX_HOURS ? "todo.model_now" : "todo.model_expiring", vars);
    case "hold": return msg("todo.hold", vars);
    case "behind": return msg("todo.behind", vars);
    case "press_credit": return msg("todo.press_credit", vars);
    case "switch_account": return msg("todo.switch_account", vars);
    case "model_credit": return x.expires_at && x.expires_at - now <= SIX_HOURS ? msg("todo.model_now", { ...vars, time: formatRemaining((x.expires_at ?? now) - now) }) : msg("todo.model_credit", vars);
    default: return `${names[x.provider]} · ${x.kind}`;
  }
}

function statusText(p: PlanView["pools"][number]): string {
  const s = p.status ?? { kind: "learning" };
  if (s.kind === "short" || s.kind === "short_credit") return msg(s.kind === "short" ? "status.short" : "status.short_credit", { time: formatRemaining(s.value ?? 0) });
  if (s.kind === "more") return msg("status.more", { value: Math.round(s.value ?? 0) });
  return msg(s.kind === "ok" ? "status.ok" : "status.learning");
}

function outlookText(o: NonNullable<PlanView["rows"][number]["detail"]>["outlook"] | undefined): string {
  if (!o || o.kind === "learning") return msg("detail.learning");
  if (o.kind === "early") return msg("detail.early", { when: weekdayTime(o.at ?? 0), time: formatRemaining(o.value ?? 0) });
  if (o.kind === "leftover") return msg("detail.leftover", { value: Math.round(o.value ?? 0) });
  return msg("detail.even");
}

function creditLine(c: DetailCredit): string {
  const name = c.kind === "model" ? `${modelName(c.model)} ${isEn() ? "reset" : "重設"}` : msg("detail.full_reset");
  const expires = c.expires_at ? ` · ${msg("detail.expires", { date: shortDate(c.expires_at) })}` : "";
  const ids = c.manual_id && c.account ? `data-id="${esc(c.manual_id)}" data-account="${esc(c.account)}"` : "";
  if (c.hint === "used") {
    const when = c.used_at ? shortDate(c.used_at) : "";
    return `<div class="plan-detail-line"><span class="plan-used">🎟 ${esc(name)}</span><span>${esc(msg("detail.used", { when }))} <button type="button" class="pill plan-mini" data-act="credit-unmark" ${ids}>${msg("detail.undo")}</button></span></div>`;
  }
  const right = ids
    ? `<button type="button" class="pill plan-mini" data-act="credit-mark" ${ids}>${msg("detail.mark")}</button>`
    : c.hint === "scheduled" ? esc(msg("detail.scheduled", { n: Number(c.week_index ?? 0) + 1 })) : "";
  return `<div class="plan-detail-line"><span>🎟 ${esc(name)}${esc(expires)}</span><span>${right}</span></div>`;
}

function details(row: PlanView["rows"][number]): string {
  if (!row.detail) return "";
  const d = row.detail;
  return `<div class="plan-detail"><div><span>${msg("detail.pace")}</span><span>${esc(outlookText(d.outlook))}</span></div>${d.credits.map(creditLine).join("")}</div>`;
}

function todoList(view: PlanView): { text: string; url: string | null }[] {
  return view.todos.slice(0, 2).map((t) => ({ text: todoText(t, view.now), url: t.url }));
}

/** 一個平台一行：平均速度條＋目標白線，下面一行小字講先用誰、夠不夠 */
function paceRow(view: PlanView, p: PlanView["pools"][number]): string {
  const target = p.target_today;
  const pace = p.pace;
  const scale = Math.max(target, pace ?? 0, 1) * 1.05;
  // 低於目標、或照速度會提早用光，都畫橘色
  const short = p.status?.kind === "short" || p.status?.kind === "short_credit";
  const behind = pace !== null && (pace < target - 2 || short);
  const bar = pace === null
    ? `<span class="plan-track"></span>`
    : `<span class="plan-track pace"><span class="${behind ? "is-warn" : "is-good"}" style="width:${clamp(pace / scale * 100)}%"></span><i style="left:${clamp(target / scale * 100)}%"></i></span>`;
  const value = `<span class="plan-value"><b>${pace === null ? "—" : Math.round(pace)}</b> / ${Math.round(target)}%${isEn() ? "/day" : "/天"}</span>`;
  const notes: string[] = [];
  // 現在登入的帳號 → 用完後接著換哪個
  const curLeft = view.rows.find((r) => r.provider === p.provider && r.current)?.weekly_left;
  const then = p.next ? ` → ${isEn() ? "then" : "接著"} <b>${esc(p.next)}</b>` : "";
  if (p.current && curLeft !== undefined && curLeft <= 0) {
    notes.push(`<b>${esc(p.current)}</b> ${isEn() ? "used up" : "用完了"}${p.next ? ` → ${isEn() ? "switch to" : "換"} <b>${esc(p.next)}</b>` : ""}`);
  } else if (p.current) {
    notes.push(`${isEn() ? "Using" : "現在用"} <b>${esc(p.current)}</b>${curLeft !== undefined ? `（${isEn() ? "" : "剩 "}${pct(curLeft)}）` : ""}${then}`);
  } else if (p.next) {
    notes.push(`${isEn() ? "Use" : "先用"} <b>${esc(p.next)}</b>`);
  }
  if (p.status && p.status.kind !== "ok") notes.push(`<span class="is-${esc(p.status.kind)}">${esc(statusText(p))}</span>`);
  return `<div class="plan-today"><span class="plan-name">${names[p.provider]}</span>${bar}${value}</div>${notes.length ? `<div class="plan-use">${notes.join(" · ")}</div>` : ""}`;
}

export function overviewHtml(view: PlanView, expandedId: string | null, collapsed: ReadonlySet<string> = new Set()): string {
  const today = `<section class="plan-sec"><h3>${msg("today.title")}</h3>${view.pools.map((p) => paceRow(view, p)).join("")}</section>`;
  const todos = todoList(view);
  const todoBlock = todos.length ? `<section class="plan-sec">${todos.map((todo) => `<div class="plan-todo"><span>${esc(todo.text)}</span>${todo.url ? `<a href="#" data-act="open-url" data-id="${esc(todo.url)}">${msg("todo.usage_page")}</a>` : ""}</div>`).join("")}</section>` : "";
  const groups = new Map<string, PlanView["rows"]>();
  for (const row of view.rows) groups.set(row.group, [...(groups.get(row.group) ?? []), row]);
  const rows = [...groups].map(([group, items]) => {
    const closed = collapsed.has(group);
    // 收起時標題旁列出各帳號本週剩多少
    const summary = closed ? `<span class="plan-group-sum">${items.map((r) => `${esc(r.label)} ${pct(r.weekly_left)}`).join(" · ")}</span>` : "";
    const head = `<button type="button" class="plan-group" data-act="toggle-group" data-id="${esc(group)}" aria-expanded="${!closed}"><span>${closed ? "▸" : "▾"} ${esc(group)}</span>${summary}</button>`;
    if (closed) return head;
    return `${head}${items.map((row) => {
    const expanded = row.account_id === expandedId && row.detail !== null;
    const arrow = row.detail ? `<small>${expanded ? "▾" : "▸"}</small>` : "";
    const fiveTime = row.five_resets_at ? `<small>${esc(formatRemaining(row.five_resets_at - view.now))}</small>` : "";
    const five = row.five_left === null ? "—" : `${pct(row.five_left)}${fiveTime}`;
    const weekly = clamp(row.weekly_left);
    return `<button type="button" class="plan-account${row.current ? " current" : ""}${expanded ? " expanded" : ""}" ${row.detail ? `data-act="toggle-row" data-id="${esc(row.account_id)}" aria-expanded="${expanded}"` : "disabled"}><span class="plan-dot ${row.state}"></span><span class="plan-label">${esc(row.label)} ${arrow}</span><span class="plan-five${row.five_left !== null && row.five_left < 30 ? " low" : ""}">${five}</span><span class="plan-week-track"><span class="${weekly < 30 ? "is-warn" : ""}" style="width:${weekly}%"></span></span><span class="plan-week-value${weekly < 10 ? " low" : ""}">${pct(row.weekly_left)}</span><span class="plan-reset">${esc(formatRemaining(row.weekly_resets_at - view.now))}</span><span class="plan-credits">${row.credits ? `🎟${row.credits}` : ""}</span></button>${expanded ? details(row) : ""}`;
  }).join("")}`;
  }).join("");
  return `${today}${todoBlock}<section class="plan-sec plan-accounts"><h3 class="plan-columns"><span></span><span>${msg("accounts.title")}</span><span>${msg("accounts.five_hour")}</span><span>${msg("accounts.weekly")}</span><span></span><span>${msg("accounts.reset")}</span><span>${msg("accounts.credits")}</span></h3>${rows}</section>`;
}
