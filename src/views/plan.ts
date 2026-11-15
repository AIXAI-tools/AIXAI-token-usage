import { esc, formatRemaining } from "../util";
import zh from "../i18n/plan.zh.json";
import en from "../i18n/plan.en.json";
import type { PlanView, ProviderId } from "./planTypes";

const isEn = () => document.documentElement.lang.toLowerCase().startsWith("en");
const msg = (key: keyof typeof zh, vars: Record<string, string | number> = {}) => {
  let s = (isEn() ? en : zh)[key];
  for (const [k, v] of Object.entries(vars)) s = s.split(`{${k}}`).join(String(v));
  return s;
};
const names: Record<ProviderId, string> = { claude: "Claude", codex: "Codex", antigravity: "Antigravity" };
const clamp = (n: number) => Math.max(0, Math.min(100, n));

function goalBar(p: PlanView["pools"][number]): { bar: string; value: string } {
  const target = p.target_today;
  const used = p.used_today;
  const ratio = target > 0 ? (used / target) * 100 : (used > 0 ? 100 : 0);
  const width = clamp(Math.round(ratio));
  const over = used > target;
  const bar = `<span class="plan-track pace"><span class="${over ? "is-warn" : "is-good"}" style="width:${width}%"></span><i style="left:100%"></i></span>`;
  const value = `<span class="plan-value"><b>${Math.round(used)}</b> / ${Math.round(target)}%</span>`;
  return { bar, value };
}

function statusText(p: PlanView["pools"][number]): string {
  const s = p.status ?? { kind: "learning" };
  if (s.kind === "exhausted") return msg("status.exhausted", { group: p.group ?? "", time: formatRemaining(s.value ?? 0) });
  if (s.kind === "short" || s.kind === "short_credit") return msg(s.kind === "short" ? "status.short" : "status.short_credit", { time: formatRemaining(s.value ?? 0) });
  if (s.kind === "more") return msg("status.more", { value: Math.round(s.value ?? 0) });
  return msg(s.kind === "ok" ? "status.ok" : "status.learning");
}

function goalsHtml(view: PlanView, goalsOpen: boolean): string {
  const summaryParts: string[] = [];
  for (const p of view.pools) {
    if (p.status?.kind === "exhausted") continue;
    const name = p.group ?? names[p.provider] ?? p.provider;
    summaryParts.push(`${name} <b>${Math.round(p.used_today)}</b>/${Math.round(p.target_today)}`);
  }
  const summaryHtml = goalsOpen ? "" : `<span class="plan-group-sum">${summaryParts.join(" · ")}</span>`;
  const title = goalsOpen ? msg("goals.title") : msg("goals.title_short");
  const head = `<button type="button" class="plan-group" data-act="toggle-group" data-id="__goals" aria-expanded="${goalsOpen}"><span>${goalsOpen ? "▾" : "▸"} ${title}</span>${summaryHtml}</button>`;

  if (!goalsOpen) {
    return `<section class="plan-sec">${head}</section>`;
  }

  const rendered = new Set<PlanView["pools"][number]>();
  const rows: string[] = [];

  for (const p of view.pools) {
    if (rendered.has(p)) continue;
    if (p.provider === "antigravity" && Boolean(p.group)) {
      const agyList = view.pools.filter((x) => x.provider === "antigravity" && Boolean(x.group));
      for (const x of agyList) rendered.add(x);
      const subRows = agyList.map((x) => {
        if (x.status?.kind === "exhausted") {
          return `<div class="plan-gone">${esc(statusText(x))}</div>`;
        }
        const { bar, value } = goalBar(x);
        return `<div class="plan-today sub"><span class="plan-name">${esc(x.group ?? "")}</span>${bar}${value}</div>`;
      }).join("");
      rows.push(`<div class="plan-agy"><div class="plan-today"><span class="plan-name">Antigravity</span><span></span><span></span></div>${subRows}</div>`);
    } else {
      rendered.add(p);
      const { bar, value } = goalBar(p);
      rows.push(`<div class="plan-today"><span class="plan-name">${names[p.provider] ?? p.provider}</span>${bar}${value}</div>`);
    }
  }

  return `<section class="plan-sec">${head}${rows.join("")}</section>`;
}

export function planHtml(view: PlanView, collapsed: ReadonlySet<string> = new Set()): string {
  const goalsOpen = !collapsed.has("__goals");
  const goals = goalsHtml(view, goalsOpen);

  const laneProviders = [...new Set(view.plan.lanes.map((l) => l.provider))];
  const weekName = (i: number) => (i === 0 ? msg("plan.this_week") : i === 1 ? msg("plan.next_week") : msg("plan.week_n", { n: i + 1 }));
  const lanes = laneProviders.map((provider) => {
    const mine = view.plan.lanes.filter((l) => l.provider === provider);
    const head = `<div class="pl-row pl-head"><span></span>${[0, 1, 2, 3].map((i) => `<span>${weekName(i)}</span>`).join("")}</div>`;
    const rows = mine.map((lane) => `<div class="pl-row"><span class="pl-label">${esc(lane.label)}</span>${[0, 1, 2, 3].map((i) => {
      const cell = lane.cells[i];
      if (!cell) return `<span class="pl-cell empty">—</span>`;
      const base = i === 0 ? msg("plan.left", { value: Math.round(cell.base) }) : `${Math.round(cell.base)}%`;
      return `<span class="pl-cell${cell.credits > 0 ? " has-credit" : ""}">${base}${cell.credits > 0 ? msg("plan.credit") : ""}</span>`;
    }).join("")}</div>`).join("");
    return `<section class="plan-sec"><h3>${names[provider]} · ${msg("plan.weekly_credits")}</h3>${head}${rows}</section>`;
  }).join("");

  return `${goals}${lanes}`;
}
