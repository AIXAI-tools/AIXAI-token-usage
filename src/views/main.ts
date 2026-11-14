import { invoke } from "@tauri-apps/api/core";
import { missingSetupHtml, statuslineNoteHtml, statuslineTag } from "./claudeStatus";
import { hintBarHtml, type ModelWatchState } from "./modelWatch";
import {
  $,
  dict,
  esc,
  formatRemaining,
  hhmm,
  save,
  t,
  zhTW,
  type Provider,
  type Reason,
  type Snapshot,
  type UsageWindow,
  type WindowLabel,
} from "../util";

export function windowLabel(l: WindowLabel): string {
  let base: string;
  if (l.kind === "five_hour") base = t("window.five_hour");
  else if (l.kind === "weekly") base = t("window.weekly");
  else if (l.minutes % 1440 === 0) base = t("window.days", { n: l.minutes / 1440 });
  else if (l.minutes % 60 === 0) base = t("window.hours", { n: l.minutes / 60 });
  else base = t("window.minutes", { n: l.minutes });
  return l.bucket ? `${l.bucket} · ${base}` : base;
}

export function resetText(w: UsageWindow): string {
  if (w.disabled) return t("reset.paused");
  if (w.reset_done) return t("reset.done");
  if (!w.resets_at) return "—";
  return formatRemaining(w.resets_at - Date.now() / 1000);
}

export function level(left: number): "ok" | "warn" | "crit" {
  if (left < 10) return "crit";
  if (left < 30) return "warn";
  return "ok";
}

export function tagFor(p: Provider): { cls: string; text: string } {
  if (p.offline) {
    // 每個窗口都還有額度（含重置後恢復的）→「可用」，否則「離線」
    const usable = p.windows.filter((w) => !w.disabled).every((w) => w.used_percent < 100);
    return usable ? { cls: "ok", text: t("tag.available") } : { cls: "mute", text: t("tag.offline") };
  }
  if (p.status === "ok") {
    if (p.source === "statusline") return statuslineTag(p.as_of);
    if (p.source === "local_log") return { cls: "info", text: t("tag.local_log") };
    if (p.source === "cache") return { cls: "mute", text: t("tag.cache") };
    return { cls: "ok", text: t("tag.live") };
  }
  if (p.reason?.key === "claude_statusline_missing") return { cls: "off", text: t("tag.need_setup") };
  if (p.reason?.key === "claude_statusline_waiting") return { cls: "idle", text: t("tag.waiting") };
  return { cls: "mute", text: t("tag.unavailable") };
}

export function reasonText(p: Provider, r?: Reason | null): string {
  if (!r) return t("reason.no_data");
  const specific = `reason.${p.id}.${r.key}`;
  return t(specific in dict ? specific : `reason.${r.key}`, { detail: r.detail ?? "" });
}

export function rowHtml(w: UsageWindow, showUsed: boolean): string {
  const label = windowLabel(w.label);
  if (w.disabled) {
    return `<div class="row"><span>${esc(label)}</span><span class="bar"></span><span class="num lv-none-t">—</span><span class="reset">${esc(resetText(w))}</span></div>`;
  }
  const left = Math.max(0, Math.min(100, 100 - w.used_percent));
  const shown = Math.round(showUsed ? 100 - left : left);
  const lv = level(left);
  const numCls = lv === "ok" ? "" : `lv-${lv}-t`;
  return `<div class="row"><span>${esc(label)}</span><span class="bar"><span class="lv-${lv}" style="width:${shown}%"></span></span><span class="num ${numCls}">${shown}%</span><span class="reset">${esc(resetText(w))}</span></div>`;
}

// Antigravity 官方分組標題：Claude and GPT + Opus, Sonnet → 「Claude 和 GPT（Opus、Sonnet）」
export function groupTitle(w: UsageWindow): string {
  const zh = dict === zhTW;
  let title = (w.group ?? "").replace(/ and /g, zh ? " 和 " : " and ");
  if (w.group_models) title += zh ? `（${w.group_models.replace(/, /g, "、")}）` : ` (${w.group_models})`;
  return title;
}

export function windowsHtml(ws: UsageWindow[], showUsed: boolean): string {
  let out = "";
  let chunk: UsageWindow[] = [];
  let current: string | null | undefined = undefined;
  const flush = () => {
    if (!chunk.length) return;
    if (current) out += `<div class="gtitle">${esc(groupTitle(chunk[0]))}</div>`;
    out += `<div class="rows">${chunk.map((w) => rowHtml(w, showUsed)).join("")}</div>`;
    chunk = [];
  };
  for (const w of ws) {
    const g = w.group ?? null;
    if (current !== undefined && g !== current) flush();
    current = g;
    chunk.push(w);
  }
  flush();
  return out;
}

export function groupHtml(
  p: Provider,
  collapsed: Record<string, boolean>,
  showUsed: boolean,
  hintHtml = "",
  csInstallingId: string | null = null,
  csFailed: { id: string; error: string } | null = null,
): string {
  const tag = tagFor(p);
  const who = p.name || p.account_masked || "";
  const title = who ? `${t(`platform.${p.id}`)} · ${who}` : t(`platform.${p.id}`);
  const open = !collapsed[p.key];
  let body = "";
  if (open) {
    if (p.status === "ok") {
      body += windowsHtml(p.windows, showUsed);
      if (p.offline && p.as_of) body += `<div class="msg">${esc(t("offline.since", { time: hhmm(p.as_of) }))}</div>`;
      if (p.source === "statusline" && !p.offline && p.as_of) body += statuslineNoteHtml(p.as_of);
      if (p.source === "local_log" && p.as_of) body += `<div class="msg">${esc(t("asof", { time: hhmm(p.as_of) }))}</div>`;
      if (p.source === "cache" && p.as_of) {
        const why = p.reason ? ` · ${reasonText(p, p.reason)}` : "";
        body += `<div class="msg">${esc(t("lastseen", { time: hhmm(p.as_of) }) + why)}</div>`;
      }
    } else if (p.reason?.key === "claude_statusline_missing") {
      body += missingSetupHtml(p.key, csInstallingId === p.key, csFailed?.id === p.key ? csFailed.error : null);
    } else {
      body += `<div class="msg">${esc(reasonText(p, p.reason))}</div>`;
    }
  }
  return `<section class="group">
    <button class="group-head" type="button" data-id="${esc(p.key)}" aria-expanded="${open}">
      <span class="chev">${open ? "▾" : "▸"}</span>
      <span class="gname" title="${esc(p.account_full ?? "")}">${esc(title)}</span>
      <span class="tag ${tag.cls}">${esc(tag.text)}</span>
    </button>${hintHtml}${body}</section>`;
}

export function renderMain(
  snapshot: Snapshot,
  showUsed: boolean,
  collapsed: Record<string, boolean>,
  modelWatch: ModelWatchState | null = null,
  csInstallingId: string | null = null,
  csFailed: { id: string; error: string } | null = null,
) {
  $("mode").textContent = showUsed ? t("mode.used") : t("mode.left");
  for (const [id, key] of [["refresh", "refresh"], ["gear", "settings.title"]] as const) {
    $(id).setAttribute("aria-label", t(key));
    $(id).setAttribute("title", t(key));
  }
  $("refresh").classList.toggle("spin", snapshot.refreshing);
  $("privacy").textContent = t("privacy");
  $("updated").textContent = snapshot.refreshing && !snapshot.updated_at
    ? t("updating")
    : snapshot.updated_at ? t("updated", { time: hhmm(snapshot.updated_at) }) : "";
  const seenPlatforms = new Set<string>();
  $("groups").innerHTML = snapshot.providers.filter((p) => p.visible).map((p) => {
    let hint = "";
    if (!seenPlatforms.has(p.id)) {
      seenPlatforms.add(p.id);
      const change = modelWatch?.pending.find((c) => c.platform === p.id);
      if (change) hint = hintBarHtml(change);
    }
    return groupHtml(p, collapsed, showUsed, hint, csInstallingId, csFailed);
  }).join("");
}

export type MainEventsCtx = {
  collapsed: Record<string, boolean>;
  toggleShowUsed: () => void;
  setSnapshot: (s: Snapshot) => void;
  render: () => void;
  openSettings: () => Promise<void>;
};

export function bindMainEvents(ctx: MainEventsCtx) {
  $("groups").addEventListener("click", (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLButtonElement>(".group-head");
    if (!btn) return;
    const id = btn.dataset.id!;
    ctx.collapsed[id] = !ctx.collapsed[id];
    save("collapsed", ctx.collapsed);
    ctx.render();
  });
  $("mode").addEventListener("click", () => {
    ctx.toggleShowUsed();
    ctx.render();
  });
  $("refresh").addEventListener("click", () => {
    invoke<Snapshot>("refresh").then((s) => { ctx.setSnapshot(s); ctx.render(); }).catch(() => {});
  });
  $("gear").addEventListener("click", () => {
    // 回報附件用：離開主畫面前，先擷取本程式面板自己的畫面（只擷取這個視窗）
    invoke("capture_panel_shot").catch(() => {}).finally(() => { void ctx.openSettings(); });
  });
}
