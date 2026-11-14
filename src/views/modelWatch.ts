import { invoke } from "@tauri-apps/api/core";
import { dict, esc, hhmm, t } from "../util";

// ---- 與 Rust 端 models.rs／model-watch 約定對應的資料格式 ----
export type ModelEntry = { id: string; name: string };
export type QuotaHint =
  | { kind: "own"; bucket: string }
  | { kind: "shared"; group: string }
  | { kind: "unknown" };
export type ModelChange = {
  platform: string;
  added: ModelEntry[];
  removed: ModelEntry[];
  hints: [string, QuotaHint][];
};
export type ModelWatchState = {
  enabled: boolean;
  auto_send: boolean;
  checked_at?: number | null;
  counts: Record<string, number>;
  pending: ModelChange[];
};
export type ModelReportDraft = { title: string; body: string };

// ---- Tauri 指令包裝（指令不存在或失敗時回傳 null，不報錯） ----
export function getModelWatch(): Promise<ModelWatchState | null> {
  return invoke<ModelWatchState>("get_model_watch").catch(() => null);
}

export function setModelWatch(p: { enabled?: boolean; auto_send?: boolean }): Promise<ModelWatchState | null> {
  return invoke<ModelWatchState | null>("set_model_watch", p).catch(() => null);
}

export function checkModelsNow(): Promise<ModelWatchState | null> {
  return invoke<ModelWatchState>("check_models_now").catch(() => null);
}

export function dismissModelChange(platform: string): Promise<ModelWatchState | null> {
  return invoke<ModelWatchState | null>("dismiss_model_change", { platform }).catch(() => null);
}

export function reportModelChange(): Promise<ModelReportDraft | null> {
  return invoke<ModelReportDraft>("report_model_change").catch(() => null);
}

// ---- 純函式（只回傳 HTML 字串） ----
function hintText(hint: QuotaHint): string {
  if (hint.kind === "own") return t("mw.hint.own");
  if (hint.kind === "shared") return t("mw.hint.shared", { group: hint.group });
  return t("mw.hint.unknown");
}

/** 產生主畫面卡片標題下方的新模型提示條 HTML。 */
export function hintBarHtml(change: ModelChange): string {
  if (!change.added.length) return "";
  const names = change.added.map((m) => m.name || m.id).join(t("mw.sep"));
  const firstId = change.added[0].id;
  const firstHint = change.hints.find(([id]) => id === firstId)?.[1] ?? change.hints[0]?.[1] ?? { kind: "unknown" };
  const text = t("mw.prefix", { names, hint: hintText(firstHint) });
  return `<div class="mw-bar"><span class="grow">${esc(text)}</span><button class="mw-act" type="button" data-mw-report="${esc(change.platform)}">${esc(t("mw.report"))}</button><button class="mw-act mw-x" type="button" data-mw-dismiss="${esc(change.platform)}" aria-label="${esc(t("mw.dismiss"))}" title="${esc(t("mw.dismiss"))}">✕</button></div>`;
}

const PLATFORM_ORDER = ["claude", "codex", "antigravity"];

function countsHtml(counts: Record<string, number>): string {
  const keys = [
    ...PLATFORM_ORDER.filter((k) => typeof counts[k] === "number"),
    ...Object.keys(counts).filter((k) => !PLATFORM_ORDER.includes(k) && typeof counts[k] === "number"),
  ];
  if (!keys.length) return "";
  const parts = keys.map((k) => {
    const label = `platform.${k}` in dict ? t(`platform.${k}`) : k;
    return `${label} ${counts[k]}`;
  });
  return `<small>${esc(t("mw.counts", { counts: parts.join(t("mw.counts_sep")) }))}</small>`;
}

/** 產生設定頁的「新模型偵測」區塊 HTML；state 為 null 時回傳空字串。 */
export function settingsSectionHtml(state: ModelWatchState | null, checking = false): string {
  if (!state) return "";
  const checkedText = state.checked_at ? t("mw.last_check", { time: hhmm(state.checked_at) }) : t("mw.never_checked");
  return `<section class="sec">
      <h3>${esc(t("mw.title"))}<span class="mw-new">${esc(t("mw.badge"))}</span></h3>
      <div class="srow"><span class="grow mw-label"><span>${esc(t("mw.enabled"))}</span><small>${esc(t("mw.enabled_hint"))}</small></span><label class="switch"><input id="mw-enabled" type="checkbox" ${state.enabled ? "checked" : ""}><span></span></label></div>
      <div class="srow"><span class="grow mw-label"><span>${esc(t("mw.auto"))}</span><small>${esc(t("mw.auto_hint"))}</small></span><label class="switch"><input id="mw-auto" type="checkbox" ${state.auto_send ? "checked" : ""}><span></span></label></div>
      <div class="srow"><span class="grow mw-label"><span>${esc(checkedText)}</span>${countsHtml(state.counts ?? {})}</span><button id="mw-check" class="pill" type="button" ${checking ? "disabled" : ""}>${esc(checking ? t("mw.checking") : t("mw.check"))}</button></div>
    </section>`;
}
