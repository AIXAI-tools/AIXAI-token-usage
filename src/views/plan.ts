import { esc } from "../util";
import zh from "../i18n/plan.zh.json";
import en from "../i18n/plan.en.json";
import type { PlanView, ProviderId } from "./planTypes";

const msg = (key: keyof typeof zh, vars: Record<string, string | number> = {}) => {
  let s = (document.documentElement.lang.toLowerCase().startsWith("en") ? en : zh)[key];
  for (const [k, v] of Object.entries(vars)) s = s.split(`{${k}}`).join(String(v));
  return s;
};
const names: Record<ProviderId, string> = { claude: "Claude", codex: "Codex", antigravity: "Antigravity" };
const pct = (n: number) => `${Math.round(n)}%`;

export function planHtml(view: PlanView): string {
  const daily = view.plan.daily.map((entry) => {
    const target = view.pools.find((p) => p.provider === entry.provider)?.target_today ?? 0;
    const scale = Math.max(60, target, ...entry.days.map((d) => d.used ?? 0));
    return `<section class="plan-sec"><h3>${names[entry.provider]} · ${msg("plan.days")}</h3><div class="plan-chart">${entry.days.map((d, i) => {
      const day = new Date(d.day * 1000);
      const label = i === entry.days.length - 1 ? msg("plan.today") : new Intl.DateTimeFormat(document.documentElement.lang.toLowerCase().startsWith("en") ? "en" : "zh-TW", { weekday: "short" }).format(day);
      // 沒資料的日子只畫空框，不寫 0%
      if (d.used === null) return `<div class="plan-bar-wrap"><span class="plan-bar-value"></span><span class="plan-bar none"></span><span class="plan-bar-label">${esc(label)}</span></div>`;
      const height = Math.max(2, Math.min(100, d.used / scale * 100));
      return `<div class="plan-bar-wrap"><span class="plan-bar-value">${pct(d.used)}</span><span class="plan-bar${i === entry.days.length - 1 ? " today" : ""}" style="height:${height}%"></span><span class="plan-bar-label">${esc(label)}</span></div>`;
    }).join("")}<span class="plan-goal-line" style="bottom:${Math.min(100, target / scale * 100)}%"><span>${msg("plan.target", { value: Math.round(target * 10) / 10 })}</span></span></div></section>`;
  }).join("");
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
  return `${daily}${lanes}`;
}
