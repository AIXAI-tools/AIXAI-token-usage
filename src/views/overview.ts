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

/** 15:59 */
function timeOnly(t: number): string {
  const d = new Date(t * 1000);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 週一 08:00 */
function weekdayTime(t: number): string {
  const d = new Date(t * 1000);
  const day = new Intl.DateTimeFormat(isEn() ? "en" : "zh-TW", { weekday: "short" }).format(d);
  return `${day} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

const modelName = (m: string | null | undefined) => (m ? m.charAt(0).toUpperCase() + m.slice(1) : "");

type ParsedTodo = {
  main: string;
  sub: string;
  urgent: boolean;
  url: string | null;
};

function parseTodo(t: PlanView["todos"][number], now: number): ParsedTodo {
  const provider = names[t.provider] ?? t.provider;
  const account = t.account_label ?? "";
  const next = t.next_label ?? "";
  const model = modelName(t.model);
  const at = t.at ? weekdayTime(t.at) : "";
  const next_reset_at = t.next_reset_at ? timeOnly(t.next_reset_at) : "";
  const date = t.expires_at ? shortDate(t.expires_at) : "";
  const value = Math.round(t.value ?? 0);
  const time = formatRemaining(t.value ?? 0);
  const refund = Math.round(t.refund ?? 0);
  const urgent = Boolean(t.urgent);
  const url = t.url ?? null;

  switch (t.kind) {
    case "press_credit": {
      const isNow = t.at === null || t.at === undefined || t.at - now <= 3600;
      const main = isNow
        ? msg("todo.press_credit_now", { account })
        : msg("todo.press_credit_later", { provider, at, account });
      const sub = msg("todo.press_credit_sub", { value });
      return { main, sub, urgent, url };
    }
    case "short_gap": {
      const main = msg("todo.short_gap", { provider, at, next, next_reset_at });
      const sub = msg("todo.short_gap_sub", { time });
      return { main, sub, urgent, url };
    }
    case "credit_expiring": {
      if (t.credit_kind === "model") {
        const main = msg("todo.expiring_model", { account, model, date });
        const sub = (t.refund ?? 0) > 0
          ? msg("todo.expiring_model_refund", { refund })
          : msg("todo.expiring_model_zero", { model });
        return { main, sub, urgent, url };
      }
      const main = msg("todo.expiring_full", { account, date });
      const sub = msg("todo.expiring_full_sub");
      return { main, sub, urgent, url };
    }
    case "model_credit": {
      const main = msg("todo.model_credit_main", { account, model, date });
      const sub = (t.refund ?? 0) > 0
        ? msg("todo.model_credit_refund", { refund })
        : msg("todo.model_credit_zero", { model });
      return { main, sub, urgent, url };
    }
    case "hold":
    case "behind":
    case "switch_account": {
      return { main: todoText(t, now), sub: "", urgent, url };
    }
    default: {
      return { main: todoText(t, now), sub: "", urgent, url };
    }
  }
}

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
  if (s.kind === "exhausted") return msg("status.exhausted", { group: p.group ?? "", time: formatRemaining(s.value ?? 0) });
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

function formatResetRecent(cycleStart: number, now: number): string {
  const resetD = new Date(cycleStart * 1000);
  const readyD = new Date((cycleStart + 12 * 3600) * 1000);
  const nowD = new Date(now * 1000);
  const sameDay =
    resetD.getFullYear() === nowD.getFullYear() &&
    resetD.getMonth() === nowD.getMonth() &&
    resetD.getDate() === nowD.getDate();
  const readySameDay =
    readyD.getFullYear() === nowD.getFullYear() &&
    readyD.getMonth() === nowD.getMonth() &&
    readyD.getDate() === nowD.getDate();

  if (isEn()) {
    const resetStr = sameDay ? `today ${timeOnly(cycleStart)}` : weekdayTime(cycleStart);
    const readyStr = readySameDay ? `today ${timeOnly(cycleStart + 12 * 3600)}` : weekdayTime(cycleStart + 12 * 3600);
    return msg("status.just_reset", { resetTime: resetStr, readyTime: readyStr });
  }
  const hour = resetD.getHours();
  const resetPrefix = sameDay ? (hour < 12 ? `今早 ${timeOnly(cycleStart)}` : `今天 ${timeOnly(cycleStart)}`) : weekdayTime(cycleStart);
  const readyHour = readyD.getHours();
  const readyPrefix = readySameDay
    ? readyHour >= 18
      ? `今晚 ${timeOnly(cycleStart + 12 * 3600)}`
      : readyHour >= 12
      ? `下午 ${timeOnly(cycleStart + 12 * 3600)}`
      : `今天 ${timeOnly(cycleStart + 12 * 3600)}`
    : weekdayTime(cycleStart + 12 * 3600);
  return msg("status.just_reset", { resetTime: resetPrefix, readyTime: readyPrefix });
}

/** 今天額度：今天 00:00 時剩多少平均分到重置，一整天固定；舊資料沒有就退回 target_today */
function budgetOf(p: PlanView["pools"][number]): number {
  return p.budget_today ?? p.target_today;
}

function todayBar(p: PlanView["pools"][number]): { bar: string; value: string } {
  const budget = budgetOf(p);
  const used = p.used_today;
  const ratio = budget > 0 ? (used / budget) * 100 : (used > 0 ? 100 : 0);
  const width = clamp(Math.round(ratio));
  const over = Math.round(used) > Math.round(budget);
  const bar = `<span class="plan-track pace"><span class="${over ? "is-warn" : "is-good"}" style="width:${width}%"></span><i style="left:100%"></i></span>`;
  const value = `<span class="plan-value"><b>${Math.round(used)}</b> / ${Math.round(budget)}%</span>`;
  return { bar, value };
}

/** 小字第一句：還能用 X%／今天額度用完了／超用 X%，接下來每天約 Y% */
function budgetText(p: PlanView["pools"][number]): string {
  const budget = Math.round(budgetOf(p));
  const used = Math.round(p.used_today);
  if (used < budget) return msg("budget.left", { value: budget - used });
  if (used === budget) return msg("budget.done");
  const after = Math.max(0, Math.round(p.after_today ?? p.target_today));
  return msg("budget.over", { over: used - budget, after });
}

/** 12/1 */
function monthDay(t: number): string {
  const d = new Date(t * 1000);
  return `${d.getMonth() + 1}/${d.getDate()}`;
}

function todayStatusText(p: PlanView["pools"][number], now: number): string {
  if (p.pace === null) {
    if (p.cycle_start && now - p.cycle_start < 12 * 3600 && now - p.cycle_start >= 0) {
      return formatResetRecent(p.cycle_start, now);
    }
    return msg("status.learning");
  }
  const s = p.status ?? { kind: "learning" };
  const pacePrefix = msg("status.pace_prefix", { pace: Math.round(p.pace) });
  if (s.kind === "short") {
    const t = weekdayTime(now + (s.value ?? 0));
    return `${pacePrefix} · <span class="is-short">${esc(msg("status.short", { time: t }))}</span>`;
  }
  if (s.kind === "short_credit") {
    const t = weekdayTime(now + (s.value ?? 0));
    const text = p.press_credit_expires ? msg("status.short_credit_on", { time: t, date: monthDay(p.press_credit_expires) }) : msg("status.short_credit", { time: t });
    return `${pacePrefix} · <span class="is-short_credit">${esc(text)}</span>`;
  }
  if (s.kind === "more") {
    return `${pacePrefix} · <span class="is-more">${esc(msg("status.more", { value: Math.round(s.value ?? 0) }))}</span>`;
  }
  if (s.kind === "ok") {
    return `${pacePrefix} · ${msg("status.ok")}`;
  }
  return `${pacePrefix} · <span class="is-learning">${msg("status.learning")}</span>`;
}

/** 一個平台一行：今天用量條＋目標白線，下面小字講先用誰、平均速度與夠不夠 */
function todayRow(view: PlanView, p: PlanView["pools"][number]): string {
  const { bar, value } = todayBar(p);
  const lines: string[] = [];
  const curLeft = view.rows.find((r) => r.provider === p.provider && r.current)?.weekly_left;
  const then = p.next ? ` → ${isEn() ? "then" : "接著"} <b>${esc(p.next)}</b>` : "";
  if (p.current && curLeft !== undefined && curLeft <= 0) {
    lines.push(`<b>${esc(p.current)}</b> ${isEn() ? "used up" : "用完了"}${p.next ? ` → ${isEn() ? "switch to" : "換"} <b>${esc(p.next)}</b>` : ""}`);
  } else if (p.current) {
    lines.push(`${isEn() ? "Using" : "現在用"} <b>${esc(p.current)}</b>${curLeft !== undefined ? `（${isEn() ? "" : "剩 "}${pct(curLeft)}）` : ""}${then}`);
  } else if (p.next) {
    lines.push(`${isEn() ? "Use" : "先用"} <b>${esc(p.next)}</b>`);
  }

  const st = todayStatusText(p, view.now);
  lines.push(st ? `${budgetText(p)} · ${st}` : budgetText(p));

  const use = lines.length ? `<div class="plan-use">${lines.join("<br>")}</div>` : "";
  return `<div class="plan-today"><span class="plan-name">${names[p.provider] ?? p.provider}</span>${bar}${value}</div>${use}`;
}

function agySubRow(p: PlanView["pools"][number], now: number): string {
  if (p.status?.kind === "exhausted") {
    return `<div class="plan-gone">${esc(statusText(p))}</div>`;
  }
  const { bar, value } = todayBar(p);
  const st = todayStatusText(p, now);
  const use = `<div class="plan-use">${st ? `${budgetText(p)} · ${st}` : budgetText(p)}</div>`;
  return `<div class="plan-today sub"><span class="plan-name">${esc(p.group ?? "")}</span>${bar}${value}</div>${use}`;
}

function poolsHtml(view: PlanView): string {
  const rendered = new Set<PlanView["pools"][number]>();
  const items: string[] = [];
  for (const p of view.pools) {
    if (rendered.has(p)) continue;
    if (p.provider === "antigravity" && Boolean(p.group)) {
      const agyList = view.pools.filter((x) => x.provider === "antigravity" && Boolean(x.group));
      for (const x of agyList) rendered.add(x);
      const subRows = agyList.map((x) => agySubRow(x, view.now)).join("");
      items.push(`<div class="plan-agy"><div class="plan-today"><span class="plan-name">Antigravity</span><span></span><span></span></div>${subRows}</div>`);
    } else {
      rendered.add(p);
      items.push(todayRow(view, p));
    }
  }
  return items.join("");
}

function todaySection(view: PlanView, collapsed: ReadonlySet<string>): string {
  const open = !collapsed.has("__today");
  const summaryParts: string[] = [];
  for (const p of view.pools) {
    if (p.status?.kind === "exhausted") continue;
    const name = p.group ?? names[p.provider] ?? p.provider;
    summaryParts.push(`${name} <b>${Math.round(p.used_today)}</b>/${Math.round(budgetOf(p))}`);
  }
  const summaryHtml = open ? "" : `<span class="plan-group-sum">${summaryParts.join(" · ")}</span>`;
  const head = `<button type="button" class="plan-group two-line" data-act="toggle-group" data-id="__today" aria-expanded="${open}"><span class="plan-group-title">${open ? "▾" : "▸"} ${msg("today.title")}</span>${summaryHtml}</button>`;
  if (!open) {
    return `<section class="plan-sec">${head}</section>`;
  }
  return `<section class="plan-sec">${head}${poolsHtml(view)}</section>`;
}

function remindersSection(
  view: PlanView,
  collapsed: ReadonlySet<string>,
  remindersOpen?: boolean
): string {
  const todos = (view.todos ?? []).slice(0, 3).map((t) => parseTodo(t, view.now));
  if (todos.length === 0) return "";
  const open = remindersOpen !== undefined ? remindersOpen : !collapsed.has("__reminders");
  const hasUrgent = todos.some((t) => t.urgent);
  const badge = hasUrgent ? `<span class="plan-badge">${msg("reminders.badge_expiring")}</span>` : "";
  const summary = open ? "" : `<span class="plan-group-sum">${esc(todos.map((t) => t.main).join(" · "))}</span>`;
  const head = `<button type="button" class="plan-group two-line" data-act="toggle-group" data-id="__reminders" aria-expanded="${open}"><span class="plan-group-title">${open ? "▾" : "▸"} ${msg("reminders.title", { count: todos.length })}${badge}</span>${summary}</button>`;
  const cards = open
    ? todos.map((item) => {
        const cls = item.urgent ? "urgent" : "calm";
        const subHtml = item.sub ? `<small>${esc(item.sub)}</small>` : "";
        const linkHtml = item.url ? `<a href="#" data-act="open-url" data-id="${esc(item.url)}">${msg("todo.usage_page")}</a>` : "";
        return `<div class="plan-todo ${cls}"><div>${esc(item.main)}${subHtml}</div>${linkHtml}</div>`;
      }).join("")
    : "";
  return `<section class="plan-sec">${head}${cards}</section>`;
}

function creditsSection(view: PlanView, collapsed: ReadonlySet<string>): string {
  const credits = view.credits;
  if (!credits || credits.length === 0) return "";

  const claudeCount = credits.filter((c) => c.provider === "claude").reduce((acc, c) => acc + c.count, 0);
  const codexCount = credits.filter((c) => c.provider === "codex").reduce((acc, c) => acc + c.count, 0);
  const countParts: string[] = [];
  if (claudeCount > 0) countParts.push(msg("credits.count_claude", { count: claudeCount }));
  if (codexCount > 0) countParts.push(msg("credits.count_codex", { count: codexCount }));
  if (countParts.length === 0) countParts.push(msg("credits.count_general", { count: credits.reduce((acc, c) => acc + c.count, 0) }));
  const title = msg("credits.title", { counts: countParts.join(" · ") });

  const open = !collapsed.has("__credits");
  const head = `<button type="button" class="plan-group" data-act="toggle-group" data-id="__credits" aria-expanded="${open}"><span>${open ? "▾" : "▸"} ${title}</span></button>`;
  if (!open) {
    return `<section class="plan-sec">${head}</section>`;
  }

  const rows = credits.map((c) => {
    const acctPrefix = c.provider === "codex" ? `Codex ${c.account_label}` : c.account_label;
    const kindName = c.kind === "model" ? msg("credits.model_reset", { model: modelName(c.model) }) : msg("credits.full_reset");
    const countSuffix = c.count > 1 ? ` ×${c.count}` : "";
    const left = `${esc(acctPrefix)} · ${esc(kindName)}${countSuffix}`;

    const whenText = c.expires_at ? msg("credits.expires", { date: shortDate(c.expires_at) }) : "—";
    const soon = c.expires_at !== null && (c.expires_at - view.now <= 48 * 3600);
    const whenHtml = `<span class="when${soon ? " is-soon" : ""}">${esc(whenText)}</span>`;

    let sub = "";
    switch (c.hint) {
      case "press_now": {
        // 沒有用光時間（還算不出速度、照舊規則）＝現在按
        const imminent = c.press_at === null || c.press_at === undefined || c.press_at - view.now <= 3600;
        sub = imminent
          ? msg("credits.press_now_imminent")
          : msg("credits.press_now_later", { time: c.press_at ? weekdayTime(c.press_at) : "" });
        break;
      }
      case "hold": {
        sub = msg("credits.hold");
        break;
      }
      case "dont_press": {
        sub = msg("credits.dont_press", { account: c.account_label });
        break;
      }
      case "expiring": {
        if (c.kind === "model") {
          const m = modelName(c.model);
          sub = (c.refund ?? 0) > 0
            ? msg("todo.expiring_model_refund", { refund: Math.round(c.refund ?? 0) })
            : msg("todo.expiring_model_zero", { model: m });
        } else {
          sub = msg("credits.expiring_full");
        }
        break;
      }
      case "model_wait": {
        const m = modelName(c.model);
        sub = (c.refund ?? 0) > 0
          ? msg("todo.model_credit_refund", { refund: Math.round(c.refund ?? 0) })
          : msg("todo.model_credit_zero", { model: m });
        break;
      }
      default: {
        sub = "";
      }
    }
    const subHtml = sub ? `<small>${esc(sub)}</small>` : "";
    return `<div class="plan-cr"><span>${left}</span>${whenHtml}${subHtml}</div>`;
  }).join("");

  return `<section class="plan-sec">${head}${rows}</section>`;
}

function accountsSection(
  view: PlanView,
  expandedId: string | null,
  collapsed: ReadonlySet<string>
): string {
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
  return `<section class="plan-sec plan-accounts"><h3 class="plan-columns"><span></span><span>${msg("accounts.title")}</span><span>${msg("accounts.five_hour")}</span><span>${msg("accounts.weekly")}</span><span></span><span>${msg("accounts.reset")}</span><span>${msg("accounts.credits")}</span></h3>${rows}</section>`;
}

export function overviewHtml(
  view: PlanView,
  expandedId: string | null,
  collapsed: ReadonlySet<string> = new Set(),
  remindersOpen?: boolean
): string {
  const today = todaySection(view, collapsed);
  const todoBlock = remindersSection(view, collapsed, remindersOpen);
  const creditsBlock = creditsSection(view, collapsed);
  const accountsBlock = accountsSection(view, expandedId, collapsed);
  return `${today}${todoBlock}${creditsBlock}${accountsBlock}`;
}
