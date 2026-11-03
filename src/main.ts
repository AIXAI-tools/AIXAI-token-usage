import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import zhTW from "./i18n/zh-TW.json";
import en from "./i18n/en.json";

// ---- 與 Rust 端 model.rs／lib.rs 對應的資料格式 ----
type Reason = { key: string; detail?: string | null };
type WindowLabel = { kind: "five_hour" | "weekly" | "minutes"; minutes: number; bucket?: string | null };
type UsageWindow = {
  label: WindowLabel;
  used_percent: number;
  resets_at?: number | null;
  disabled?: boolean;
  reset_done?: boolean;
  group?: string | null;
  group_models?: string | null;
};
type Platform = "codex" | "claude" | "antigravity";
type Provider = {
  id: Platform;
  key: string;
  name: string;
  visible: boolean;
  removable: boolean;
  status: "ok" | "unavailable";
  source?: "live" | "local_log" | "cache" | "offline" | null;
  as_of?: number | null;
  reason?: Reason | null;
  account_masked?: string | null;
  account_full?: string | null;
  offline?: boolean;
  windows: UsageWindow[];
};
type Snapshot = { updated_at?: number | null; refreshing: boolean; providers: Provider[] };
type AccountDto = {
  key: string;
  platform: Platform;
  name: string;
  email_masked?: string | null;
  email_full?: string | null;
  visible: boolean;
  removable: boolean;
};
type OfflineDto = { id: string; email_masked: string; queried_at: number; visible: boolean };
type Settings = {
  language: string;
  zh: boolean;
  notify_threshold: number;
  notify_on_reset: boolean;
  autostart: boolean;
  accounts: AccountDto[];
  offline: OfflineDto[];
};

type UpdateState = {
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

type ReplyDto = { rid: string; kind: string; version: string; text: string; urls: string[]; created: string; unread: boolean; can_update: boolean };
type ReportDto = { id: string; title: string; ts: number; want_reply: boolean; replies: ReplyDto[] };
type Prepare = { sys: string; status: string; log: string; log_kb: number; shot_b64?: string | null; shot_kb: number };

// ---- 語言：預設跟隨系統，之後以 Rust 端設定為準（右鍵選單可切換） ----
let dict: Record<string, string> = navigator.language.toLowerCase().startsWith("zh") ? zhTW : en;
function applyLang(zh: boolean) {
  dict = zh ? zhTW : en;
  document.documentElement.lang = zh ? "zh-Hant" : "en";
}
applyLang(dict === zhTW);
function t(key: string, vars: Record<string, string | number> = {}): string {
  let s = dict[key] ?? key;
  for (const [k, v] of Object.entries(vars)) s = s.replace(`{${k}}`, String(v));
  return s;
}

// ---- 使用者偏好（只存在本機，讀寫失敗也不影響功能） ----
function load<T>(key: string, fallback: T): T {
  try { const v = localStorage.getItem(key); return v ? (JSON.parse(v) as T) : fallback; } catch { return fallback; }
}
function save(key: string, value: unknown) {
  try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* 忽略 */ }
}
let showUsed = load<boolean>("showUsed", false);
const collapsed = load<Record<string, boolean>>("collapsed", {});

let snapshot: Snapshot = { refreshing: true, providers: [] };
let settings: Settings | null = null;
let view: "main" | "settings" | "update" | "report" | "reports" = "main";
let upd: UpdateState | null = null;
let addPlatform: "claude" | "codex" = "claude";
let addHint: { dir: string; command: string } | null = null;
let detectMsg = "";
let reportUnread = false;

// 回報頁狀態（輸入內容存在這裡，重畫不會掉）
const newForm = () => ({
  kind: "bug" as "bug" | "idea", title: "", body: "", contact: "",
  a_sys: true, a_status: true, a_shot: true, a_log: true, want: true,
  ref: null as string | null, preview: "" as "" | "sys" | "status" | "log" | "all", previewText: "",
  err: "", errDetail: "", sending: false, doneId: "", copied: false,
});
let rep = newForm();
let prep: Prepare | null = null;
let reports: ReportDto[] = [];
let newReplyIds = new Set<string>();
let repChecking = false;

const $ = (id: string) => document.getElementById(id)!;
const esc = (s: string) => s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]!));

function hhmm(epochSec: number): string {
  const d = new Date(epochSec * 1000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

function windowLabel(l: WindowLabel): string {
  let base: string;
  if (l.kind === "five_hour") base = t("window.five_hour");
  else if (l.kind === "weekly") base = t("window.weekly");
  else if (l.minutes % 1440 === 0) base = t("window.days", { n: l.minutes / 1440 });
  else if (l.minutes % 60 === 0) base = t("window.hours", { n: l.minutes / 60 });
  else base = t("window.minutes", { n: l.minutes });
  return l.bucket ? `${l.bucket} · ${base}` : base;
}

// 重置倒數：不到 1 小時 9m；不到 1 天 1h9m；1 天以上 2d11h（中英相同）
function formatRemaining(sec: number): string {
  sec = Math.max(0, Math.floor(sec));
  if (sec < 3600) return `${Math.floor(sec / 60)}m`;
  if (sec < 86400) return `${Math.floor(sec / 3600)}h${Math.floor((sec % 3600) / 60)}m`;
  return `${Math.floor(sec / 86400)}d${Math.floor((sec % 86400) / 3600)}h`;
}

function resetText(w: UsageWindow): string {
  if (w.disabled) return t("reset.paused");
  if (w.reset_done) return t("reset.done");
  if (!w.resets_at) return "—";
  return formatRemaining(w.resets_at - Date.now() / 1000);
}

function level(left: number): "ok" | "warn" | "crit" {
  if (left < 10) return "crit";
  if (left < 30) return "warn";
  return "ok";
}

function tagFor(p: Provider): { cls: string; text: string } {
  if (p.offline) {
    // 每個窗口都還有額度（含重置後恢復的）→「可用」，否則「離線」
    const usable = p.windows.filter((w) => !w.disabled).every((w) => w.used_percent < 100);
    return usable ? { cls: "ok", text: t("tag.available") } : { cls: "mute", text: t("tag.offline") };
  }
  if (p.status === "ok") {
    if (p.source === "local_log") return { cls: "info", text: t("tag.local_log") };
    if (p.source === "cache") return { cls: "mute", text: t("tag.cache") };
    // Claude 用的是非官方端點，標「即時」；Codex／Antigravity 是官方管道
    return p.id === "claude" ? { cls: "ok", text: t("tag.realtime") } : { cls: "ok", text: t("tag.live") };
  }
  return { cls: "mute", text: t("tag.unavailable") };
}

function reasonText(p: Provider, r?: Reason | null): string {
  if (!r) return t("reason.no_data");
  const specific = `reason.${p.id}.${r.key}`;
  return t(specific in dict ? specific : `reason.${r.key}`, { detail: r.detail ?? "" });
}

function rowHtml(w: UsageWindow): string {
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
function groupTitle(w: UsageWindow): string {
  const zh = dict === zhTW;
  let title = (w.group ?? "").replace(/ and /g, zh ? " 和 " : " and ");
  if (w.group_models) title += zh ? `（${w.group_models.replace(/, /g, "、")}）` : ` (${w.group_models})`;
  return title;
}

function windowsHtml(ws: UsageWindow[]): string {
  let out = "";
  let chunk: UsageWindow[] = [];
  let current: string | null | undefined = undefined;
  const flush = () => {
    if (!chunk.length) return;
    if (current) out += `<div class="gtitle">${esc(groupTitle(chunk[0]))}</div>`;
    out += `<div class="rows">${chunk.map(rowHtml).join("")}</div>`;
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

function groupHtml(p: Provider): string {
  const tag = tagFor(p);
  const who = p.name || p.account_masked || "";
  const title = who ? `${t(`platform.${p.id}`)} · ${who}` : t(`platform.${p.id}`);
  const open = !collapsed[p.key];
  let body = "";
  if (open) {
    if (p.status === "ok") {
      body += windowsHtml(p.windows);
      if (p.offline && p.as_of) body += `<div class="msg">${esc(t("offline.since", { time: hhmm(p.as_of) }))}</div>`;
      if (p.source === "local_log" && p.as_of) body += `<div class="msg">${esc(t("asof", { time: hhmm(p.as_of) }))}</div>`;
      if (p.source === "cache" && p.as_of) {
        const why = p.reason ? ` · ${reasonText(p, p.reason)}` : "";
        body += `<div class="msg">${esc(t("lastseen", { time: hhmm(p.as_of) }) + why)}</div>`;
      }
    } else {
      body += `<div class="msg">${esc(reasonText(p, p.reason))}</div>`;
    }
  }
  return `<section class="group">
    <button class="group-head" type="button" data-id="${esc(p.key)}" aria-expanded="${open}">
      <span class="chev">${open ? "▾" : "▸"}</span>
      <span class="gname" title="${esc(p.account_full ?? "")}">${esc(title)}</span>
      <span class="tag ${tag.cls}">${esc(tag.text)}</span>
    </button>${body}</section>`;
}

// ---- 設定頁 ----
function platformName(p: string): string {
  return t(`platform.${p}`);
}

function settingsHtml(s: Settings): string {
  const offRow = (o: OfflineDto) => `<div class="acct">
        <div class="acct-info"><b>${esc(t("settings.offline_name", { p: platformName("claude") }))}</b><small>${esc(o.email_masked)} · ${esc(t("settings.offline_since", { time: hhmm(o.queried_at) }))}</small></div>
        <button class="pill" type="button" data-forget="${esc(o.id)}">${esc(t("settings.forget"))}</button>
        <label class="switch" title="${esc(t("settings.show"))}"><input type="checkbox" data-offvis="${esc(o.id)}" ${o.visible ? "checked" : ""}><span></span></label>
      </div>`;
  const lastClaude = s.accounts.map((a) => a.platform).lastIndexOf("claude");
  const accts = s.accounts
    .map((a, i) => {
      const email = a.email_masked ? `<small title="${esc(a.email_full ?? "")}">${esc(a.email_masked)}</small>` : "";
      const rm = a.removable ? `<button class="icon-btn rm" type="button" data-remove="${esc(a.key)}" title="${esc(t("settings.remove"))}">✕</button>` : "";
      return `<div class="acct">
        <div class="acct-info"><b>${esc(platformName(a.platform))}</b>${email}</div>
        <input class="name-in" type="text" maxlength="24" data-name="${esc(a.key)}" value="${esc(a.name)}" placeholder="${esc(t("settings.name_ph"))}" aria-label="${esc(t("settings.name"))}">
        <label class="switch" title="${esc(t("settings.show"))}"><input type="checkbox" data-vis="${esc(a.key)}" ${a.visible ? "checked" : ""}><span></span></label>${rm}
      </div>${i === lastClaude ? s.offline.map(offRow).join("") : ""}`;
    })
    .join("");
  const thr = [5, 10, 20, 30].map((n) => `<option value="${n}" ${n === s.notify_threshold ? "selected" : ""}>${n}%</option>`).join("");
  return `
  <header class="head">
    <button id="back" class="icon-btn" type="button" aria-label="${esc(t("settings.back"))}" title="${esc(t("settings.back"))}">‹</button>
    <span class="title">${esc(t("settings.title"))}</span>
    <button id="to-update" class="pill" type="button">${esc(t("settings.update"))}${upd?.has_update ? '<i class="dot"></i>' : ""}</button>
    <button id="to-report" class="pill" type="button">${esc(t("settings.report"))}${reportUnread ? '<i class="dot"></i>' : ""}</button>
  </header>
  <div class="sbody">
    <section class="sec">
      <h3>${esc(t("settings.accounts"))}</h3>
      ${accts}
    </section>
    <section class="sec">
      <h3>${esc(t("settings.add"))}</h3>
      <div class="seg">
        <button type="button" data-add="claude" class="${addPlatform === "claude" ? "on" : ""}">Claude</button>
        <button type="button" data-add="codex" class="${addPlatform === "codex" ? "on" : ""}">Codex</button>
      </div>
      <ol class="steps"><li>${esc(t("settings.step1"))}</li><li>${esc(t("settings.step2"))}</li><li>${esc(t("settings.step3"))}</li></ol>
      <div class="cmd"><code id="cmd">${esc(addHint?.command ?? "")}</code></div>
      <div class="cmd-btns">
        <button id="open-ps" class="pill primary" type="button">${esc(t("settings.open_ps"))}</button>
        <button id="copy" class="pill" type="button">${esc(t("settings.copy_only"))}</button>
        <span class="grow"></span>
        <button id="detect" class="pill" type="button">${esc(t("settings.detect"))}</button>
      </div>
      ${detectMsg ? `<div class="msg">${esc(detectMsg)}</div>` : ""}
    </section>
    <section class="sec">
      <h3>${esc(t("settings.notify"))}</h3>
      <div class="srow"><span class="grow">${esc(t("settings.notify_below"))}</span><select id="thr">${thr}</select></div>
      <div class="srow"><span class="grow">${esc(t("settings.notify_reset"))}</span><label class="switch"><input id="on-reset" type="checkbox" ${s.notify_on_reset ? "checked" : ""}><span></span></label></div>
    </section>
    <p class="note">${esc(t("settings.note"))}</p>
  </div>`;
}

// ---- 更新頁 ----
function failText(key: string): string {
  const k = `update.err.${key}`;
  return k in dict ? t(k) : t("update.err.other", { detail: key });
}

function updateHtml(u: UpdateState): string {
  const busy = u.stage === "download" || u.stage === "verify" || u.stage === "install";
  const latestTxt = u.latest ? `v${u.latest}` : "—";
  let body = "";
  if (busy) {
    const cur = u.stage === "download" ? 0 : u.stage === "verify" ? 1 : 2;
    const steps = ["update.step_download", "update.step_verify", "update.step_install"]
      .map((k, i) => `<li>${i < cur ? "✓" : i === cur ? "…" : "·"} ${esc(t(k))}</li>`)
      .join("");
    body = `<div><b>${esc(t("update.downloading", { v: u.latest ?? "", n: u.percent }))}</b></div>
      <div class="progress"><span style="width:${u.percent}%"></span></div>
      <ul class="dl-steps">${steps}</ul>`;
  } else {
    if (u.has_update) {
      body += `<div class="notes-title">${esc(t("update.notes"))}</div><div class="notes-box">${esc(u.notes || "—")}</div>`;
      body += `<button id="do-install" class="btn-wide" type="button">${esc(t("update.install", { v: u.latest ?? "" }))}</button>`;
    } else if (u.latest) {
      body += `<div class="ok-text">${esc(t("update.uptodate"))}</div>`;
    } else if (u.check_error) {
      body += `<div class="err">${esc(t("update.err.check"))}</div>`;
    }
    if (u.stage === "error" && u.fail) {
      body += `<div class="err">${esc(failText(u.fail))} <button id="to-releases" class="link" type="button">${esc(t("update.releases"))}</button></div>`;
    }
  }
  const last = u.checking ? t("update.checking") : u.last_check ? t("update.last_check", { time: hhmm(u.last_check) }) : "";
  return `
  <header class="head">
    <button id="back-settings" class="icon-btn" type="button" aria-label="${esc(t("update.back"))}" title="${esc(t("update.back"))}">‹</button>
    <span class="title">${esc(t("update.title"))}</span>
  </header>
  <div class="upd">
    <div class="kv"><span class="k">${esc(t("update.current"))}</span><span class="v">v${esc(u.current)}</span></div>
    <div class="kv"><span class="k">${esc(t("update.latest"))}</span><span class="v ${u.has_update ? "new" : ""}">${esc(latestTxt)}</span></div>
    ${body}
    ${busy ? "" : `<div class="upd-foot"><button id="recheck" class="link" type="button" ${u.checking ? "disabled" : ""}>${esc(t("update.recheck"))}</button><span class="grow"></span><span>${esc(last)}</span></div>`}
  </div>`;
}

function renderUpdate() {
  if (!upd) return;
  $("view-update").innerHTML = updateHtml(upd);
}

async function openUpdate() {
  upd = await invoke<UpdateState>("get_update_state").catch(() => upd);
  view = "update";
  render();
}

function applyDots() {
  $("gear-dot").hidden = !(upd?.has_update || reportUnread);
}

// ---- 回報頁 ----
function reportErrText(key: string, detail = ""): string {
  const k = `report.err.${key}`;
  return k in dict ? t(k) : t("report.err.other", { detail: detail || key });
}

function reportReq() {
  return {
    kind: rep.kind, title: rep.title, body: rep.body, contact: rep.contact,
    attach_sys: rep.a_sys, attach_status: rep.a_status, attach_log: rep.a_log, attach_shot: rep.a_shot && !!prep?.shot_b64,
    want_reply: rep.want, ref_id: rep.ref,
  };
}

function reportHtml(): string {
  const p = prep;
  const head = `<header class="head">
    <button id="rp-back" class="icon-btn" type="button" aria-label="${esc(t("report.back_settings"))}" title="${esc(t("report.back_settings"))}">‹</button>
    <span class="title">${esc(rep.ref ? t("report.title_reply") : t("report.title"))}</span>
    <button id="rp-mine" class="pill" type="button">${esc(t("report.mine"))}${reportUnread ? '<i class="dot"></i>' : ""}</button>
  </header>`;
  if (rep.doneId) {
    return `${head}<div class="upd"><div class="ok-text">${esc(t("report.done"))}</div>
      <div class="msg" style="text-align:center">${esc(t("report.done_id", { id: rep.doneId }))}</div>
      <button id="rp-done" class="btn-wide" type="button">${esc(t("report.back"))}</button></div>`;
  }
  const pv = (key: "sys" | "status" | "log", txt: string) =>
    `<button class="link" type="button" data-pv="${key}">${esc(rep.preview === key ? t("report.hide") : t("report.preview_item"))}</button>` +
    (rep.preview === key ? `<pre class="pv">${esc(txt || "—")}</pre>` : "");
  const shot = p?.shot_b64
    ? `<img class="thumb" alt="" src="data:image/png;base64,${p.shot_b64}">`
    : `<span class="msg">${esc(t("report.a_shot_none"))}</span>`;
  const logTxt = p ? p.log.split("\n").slice(-40).join("\n") : "";
  const errBlock = rep.err
    ? `<div class="err">${esc(reportErrText(rep.err, rep.errDetail))}</div>
       <div class="cmd-btns"><button id="rp-copy" class="pill" type="button">${esc(rep.copied ? t("report.copied") : t("report.copy"))}</button></div>`
    : "";
  return `${head}
  <div class="sbody">
    <section class="sec">
      ${rep.ref ? "" : `<div class="seg">
        <button type="button" data-kind="bug" class="${rep.kind === "bug" ? "on" : ""}">${esc(t("report.kind_bug"))}</button>
        <button type="button" data-kind="idea" class="${rep.kind === "idea" ? "on" : ""}">${esc(t("report.kind_idea"))}</button></div>`}
      <label class="flabel">${esc(t("report.f_title"))}</label>
      <input id="rp-title" class="tin" type="text" maxlength="100" value="${esc(rep.title)}" placeholder="${esc(t("report.f_title_ph"))}">
      <label class="flabel">${esc(t("report.f_body"))}</label>
      <textarea id="rp-body" class="tin" rows="5" maxlength="5000" placeholder="${esc(t("report.f_body_ph"))}">${esc(rep.body)}</textarea>
      <label class="flabel">${esc(t("report.f_contact"))}</label>
      <input id="rp-contact" class="tin" type="text" maxlength="200" value="${esc(rep.contact)}" placeholder="${esc(t("report.f_contact_ph"))}">
    </section>
    <section class="sec">
      <h3>${esc(t("report.attach"))}</h3>
      <div class="chk"><label><input type="checkbox" data-a="a_sys" ${rep.a_sys ? "checked" : ""}> ${esc(t("report.a_sys"))}</label> ${pv("sys", p?.sys ?? "")}</div>
      <div class="chk"><label><input type="checkbox" data-a="a_status" ${rep.a_status ? "checked" : ""}> ${esc(t("report.a_status"))} <small>${esc(t("report.a_status_hint"))}</small></label> ${pv("status", p?.status ?? "")}</div>
      <div class="chk"><label><input type="checkbox" data-a="a_shot" ${rep.a_shot && p?.shot_b64 ? "checked" : ""} ${p?.shot_b64 ? "" : "disabled"}> ${esc(t("report.a_shot"))}</label> ${shot}</div>
      <div class="chk"><label><input type="checkbox" data-a="a_log" ${rep.a_log ? "checked" : ""}> ${esc(t("report.a_log"))} <small>${esc(t("report.a_log_hint", { kb: p?.log_kb ?? 0 }))}</small></label> ${pv("log", logTxt)}</div>
      <div class="chk"><label><input type="checkbox" data-a="want" ${rep.want ? "checked" : ""}> ${esc(t("report.want_reply"))}</label></div>
    </section>
    <section class="sec">
      <div class="cmd-btns">
        <button id="rp-preview" class="link" type="button">${esc(rep.preview === "all" ? t("report.hide") : t("report.preview_all"))}</button>
        <span class="grow"></span>
        <button id="rp-send" class="pill primary" type="button" ${rep.sending ? "disabled" : ""}>${esc(rep.sending ? t("report.sending") : t("report.send"))}</button>
      </div>
      ${rep.preview === "all" ? `<pre class="pv">${esc(rep.previewText)}</pre>` : ""}
      ${errBlock}
    </section>
    <p class="note">${esc(t("report.privacy"))}</p>
  </div>`;
}

function renderReport() {
  $("view-report").innerHTML = reportHtml();
}

async function openReport(ref: ReportDto | null = null) {
  if (ref) {
    rep = newForm();
    rep.ref = ref.id;
    rep.title = `Re: ${ref.title}`.slice(0, 100);
  } else if (rep.ref || rep.doneId) {
    rep = newForm();
  }
  prep = await invoke<Prepare>("report_prepare").catch(() => prep);
  view = "report";
  render();
}

// ---- 我的回報 ----
function mmdd(epochSec: number): string {
  const d = new Date(epochSec * 1000);
  return `${String(d.getMonth() + 1).padStart(2, "0")}/${String(d.getDate()).padStart(2, "0")}`;
}

function reportsHtml(): string {
  const items = reports.map((r) => {
    let rb = "";
    if (!r.want_reply) rb = `<span class="msg">${esc(t("reports.not_wanted"))}</span>`;
    else if (!r.replies.length) rb = `<span class="msg">${esc(t("reports.no_reply"))}</span>`;
    else {
      rb = r.replies.map((p) => {
        const isNew = newReplyIds.has(p.rid);
        const kindTxt = t(`reports.kind.${p.kind}`);
        const urls = p.urls.length ? `<div class="urls">${p.urls.map((u) => esc(u)).join("<br>")}</div>` : "";
        const fixed = p.kind === "fixed" && p.version
          ? (p.can_update
            ? `<button class="pill primary" type="button" data-upd="1">${esc(t("reports.update_to", { v: p.version }))}</button>`
            : `<span class="msg">${esc(t("reports.fixed_in", { v: p.version }))}</span>`)
          : "";
        return `<div class="reply"><span class="rhead">${esc(t("reports.reply_head", { kind: kindTxt }))}${isNew ? ` <b class="newtag">${esc(t("reports.new"))}</b>` : ""}</span>
          <span class="rtext">${esc(p.text)}</span>${urls}${fixed}</div>`;
      }).join("");
    }
    return `<section class="sec">
      <div class="rtitle"><b class="grow">${esc(r.title)}</b><small>${esc(mmdd(r.ts))}</small></div>
      <small class="msg">${esc(t("reports.id", { id: r.id.slice(0, 8) }))}</small>
      ${rb}
      <button class="link rbtn" type="button" data-reply="${esc(r.id)}">${esc(t("reports.reply_dev"))}</button>
    </section>`;
  }).join("");
  return `<header class="head">
    <button id="rs-back" class="icon-btn" type="button" aria-label="${esc(t("report.back"))}" title="${esc(t("report.back"))}">‹</button>
    <span class="title">${esc(t("report.mine"))}</span>
    <button id="rs-check" class="pill" type="button" ${repChecking ? "disabled" : ""}>${esc(repChecking ? t("reports.checking") : t("reports.recheck"))}</button>
  </header>
  <div class="sbody">${items || `<div class="sec"><span class="msg">${esc(t("reports.empty"))}</span></div>`}</div>`;
}

function renderReports() {
  $("view-reports").innerHTML = reportsHtml();
}

async function loadReports() {
  reports = await invoke<ReportDto[]>("report_list").catch(() => reports);
}

async function openReports() {
  await loadReports();
  newReplyIds = new Set(reports.flatMap((r) => r.replies.filter((p) => p.unread).map((p) => p.rid)));
  view = "reports";
  render();
  // 進來看過就算已讀（本次畫面仍標「新」）
  for (const r of reports) if (r.replies.some((p) => p.unread)) void invoke("report_mark_seen", { id: r.id }).catch(() => {});
}

function copyText(text: string, done: () => void) {
  navigator.clipboard.writeText(text).then(done).catch(() => {
    const ta = document.createElement("textarea");
    ta.value = text; document.body.appendChild(ta); ta.select();
    try { document.execCommand("copy"); done(); } catch { /* 忽略 */ }
    ta.remove();
  });
}

function renderSettings() {
  if (!settings) return;
  $("view-settings").innerHTML = settingsHtml(settings);
}

async function loadHint() {
  addHint = await invoke<{ dir: string; command: string } | null>("add_account_hint", { platform: addPlatform }).catch(() => null);
}

async function openSettings() {
  settings = await invoke<Settings>("get_settings").catch(() => settings);
  detectMsg = "";
  await loadHint();
  view = "settings";
  render();
}

function closeSettings() {
  view = "main";
  render();
}

let lastHeight = 0;
function fit() {
  // 面板高度跟著內容走，交給 Rust 端調整並貼齊右下角
  const h = Math.ceil($("panel").getBoundingClientRect().height);
  if (h > 0 && h !== lastHeight) {
    lastHeight = h;
    invoke("panel_resize", { height: h }).catch(() => {});
  }
}

function render() {
  const isMain = view === "main";
  $("view-main").hidden = !isMain;
  $("view-settings").hidden = view !== "settings";
  $("view-update").hidden = view !== "update";
  $("view-report").hidden = view !== "report";
  $("view-reports").hidden = view !== "reports";
  applyDots();
  if (view === "report") {
    renderReport();
    fit();
    return;
  }
  if (view === "reports") {
    renderReports();
    fit();
    return;
  }
  if (view === "settings") {
    renderSettings();
    fit();
    return;
  }
  if (view === "update") {
    renderUpdate();
    fit();
    return;
  }
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
  $("groups").innerHTML = snapshot.providers.filter((p) => p.visible).map(groupHtml).join("");
  fit();
}

// ---- 事件 ----
$("groups").addEventListener("click", (e) => {
  const btn = (e.target as HTMLElement).closest<HTMLButtonElement>(".group-head");
  if (!btn) return;
  const id = btn.dataset.id!;
  collapsed[id] = !collapsed[id];
  save("collapsed", collapsed);
  render();
});
$("mode").addEventListener("click", () => {
  showUsed = !showUsed;
  save("showUsed", showUsed);
  render();
});
$("refresh").addEventListener("click", () => {
  invoke<Snapshot>("refresh").then((s) => { snapshot = s; render(); }).catch(() => {});
});
$("gear").addEventListener("click", () => {
  // 回報附件用：離開主畫面前，先擷取本程式面板自己的畫面（只擷取這個視窗）
  invoke("capture_panel_shot").catch(() => {}).finally(() => { void openSettings(); });
});

const sv = $("view-settings");
sv.addEventListener("click", (e) => {
  const el = e.target as HTMLElement;
  if (el.closest("#back")) return closeSettings();
  if (el.closest("#to-update")) { void openUpdate(); return; }
  if (el.closest("#to-report")) { void openReport(); return; }
  if (el.closest("#open-ps")) {
    detectMsg = "";
    invoke<boolean>("open_powershell_login", { platform: addPlatform }).then((ok) => {
      if (!ok) { detectMsg = t("settings.ps_failed"); renderSettings(); fit(); }
    }).catch(() => {});
    return;
  }
  const add = el.closest<HTMLElement>("[data-add]");
  if (add) {
    addPlatform = add.dataset.add as "claude" | "codex";
    detectMsg = "";
    void loadHint().then(renderSettings);
    return;
  }
  if (el.closest("#copy")) {
    const text = addHint?.command ?? "";
    const done = () => { const b = $("copy"); b.textContent = t("settings.copied"); setTimeout(() => { b.textContent = t("settings.copy_only"); }, 1500); };
    navigator.clipboard.writeText(text).then(done).catch(() => {
      // 備援：舊式複製
      const ta = document.createElement("textarea");
      ta.value = text; document.body.appendChild(ta); ta.select();
      try { document.execCommand("copy"); done(); } catch { /* 忽略 */ }
      ta.remove();
    });
    return;
  }
  if (el.closest("#detect")) {
    invoke<[Settings, number]>("detect_accounts").then(async ([s, n]) => {
      settings = s;
      detectMsg = n > 0 ? t("settings.detected", { n }) : t("settings.none_found");
      await loadHint();
      renderSettings();
      fit();
    }).catch(() => {});
    return;
  }
  const fg = el.closest<HTMLElement>("[data-forget]");
  if (fg) {
    invoke<Settings>("forget_offline", { id: fg.dataset.forget }).then((s) => { settings = s; renderSettings(); fit(); }).catch(() => {});
    return;
  }
  const rm = el.closest<HTMLElement>("[data-remove]");
  if (rm) {
    invoke<Settings>("remove_account", { key: rm.dataset.remove }).then((s) => { settings = s; renderSettings(); fit(); }).catch(() => {});
  }
});
$("view-update").addEventListener("click", (e) => {
  const el = e.target as HTMLElement;
  if (el.closest("#back-settings")) { view = "settings"; render(); return; }
  if (el.closest("#to-releases")) { void invoke("open_releases_page").catch(() => {}); return; }
  if (el.closest("#do-install")) { void invoke("install_update_cmd").catch(() => {}); return; }
  if (el.closest("#recheck")) {
    invoke<UpdateState>("check_update_cmd").then((s) => { upd = s; if (view === "update") render(); }).catch(() => {});
  }
});
const rv = $("view-report");
rv.addEventListener("input", (e) => {
  const el = e.target as HTMLInputElement | HTMLTextAreaElement;
  if (el.id === "rp-title") rep.title = el.value;
  else if (el.id === "rp-body") rep.body = el.value;
  else if (el.id === "rp-contact") rep.contact = el.value;
});
async function refreshPreview() {
  rep.previewText = await invoke<string>("report_preview", { req: reportReq() }).catch(() => "");
  renderReport();
  fit();
}
rv.addEventListener("change", (e) => {
  const el = e.target as HTMLInputElement;
  const a = el.dataset?.a as "a_sys" | "a_status" | "a_shot" | "a_log" | "want" | undefined;
  if (a) {
    rep[a] = el.checked;
    if (rep.preview === "all") void refreshPreview();
    else { renderReport(); fit(); }
  }
});
rv.addEventListener("click", (e) => {
  const el = e.target as HTMLElement;
  if (el.closest("#rp-back")) { view = "settings"; render(); return; }
  if (el.closest("#rp-mine")) { void openReports(); return; }
  if (el.closest("#rp-done")) { rep = newForm(); view = "settings"; render(); return; }
  const k = el.closest<HTMLElement>("[data-kind]");
  if (k) { rep.kind = k.dataset.kind as "bug" | "idea"; renderReport(); return; }
  const pvb = el.closest<HTMLElement>("[data-pv]");
  if (pvb) { const key = pvb.dataset.pv as "sys" | "status" | "log"; rep.preview = rep.preview === key ? "" : key; renderReport(); fit(); return; }
  if (el.closest("#rp-preview")) {
    if (rep.preview === "all") { rep.preview = ""; renderReport(); fit(); } else { rep.preview = "all"; void refreshPreview(); }
    return;
  }
  if (el.closest("#rp-copy")) {
    void invoke<string>("report_preview", { req: reportReq() }).then((text) => {
      copyText(`${rep.title}\n\n${text}`, () => { rep.copied = true; renderReport(); setTimeout(() => { rep.copied = false; if (view === "report") renderReport(); }, 1500); });
    });
    return;
  }
  if (el.closest("#rp-send")) {
    if (rep.sending) return;
    rep.sending = true; rep.err = ""; rep.errDetail = "";
    renderReport(); fit();
    invoke<string>("report_send", { req: reportReq() }).then((id) => {
      rep = newForm();
      rep.doneId = id;
    }).catch((err) => {
      rep.errDetail = String(err);
      rep.err = /^http_\d+$/.test(String(err)) ? "bad_response" : String(err);
    }).finally(() => { rep.sending = false; if (view === "report") { renderReport(); fit(); } });
  }
});
const rsv = $("view-reports");
rsv.addEventListener("click", (e) => {
  const el = e.target as HTMLElement;
  if (el.closest("#rs-back")) { view = "report"; render(); return; }
  if (el.closest("[data-upd]")) { void openUpdate(); return; }
  const re = el.closest<HTMLElement>("[data-reply]");
  if (re) { const r = reports.find((x) => x.id === re.dataset.reply); if (r) void openReport(r); return; }
  if (el.closest("#rs-check")) {
    repChecking = true; renderReports();
    invoke<ReportDto[]>("report_check_cmd").then((l) => {
      reports = l;
      for (const r of l) for (const p of r.replies) if (p.unread) newReplyIds.add(p.rid);
    }).catch(() => {}).finally(() => {
      repChecking = false;
      if (view === "reports") {
        renderReports();
        for (const r of reports) if (r.replies.some((p) => p.unread)) void invoke("report_mark_seen", { id: r.id }).catch(() => {});
      }
    });
  }
});

sv.addEventListener("change", (e) => {
  const el = e.target as HTMLInputElement | HTMLSelectElement;
  if (el instanceof HTMLInputElement && el.dataset.name) {
    invoke<Settings>("set_account", { key: el.dataset.name, name: el.value }).then((s) => { settings = s; }).catch(() => {});
  } else if (el instanceof HTMLInputElement && el.dataset.offvis) {
    invoke<Settings>("set_offline_visible", { id: el.dataset.offvis, visible: el.checked }).then((s) => { settings = s; }).catch(() => {});
  } else if (el instanceof HTMLInputElement && el.dataset.vis) {
    invoke<Settings>("set_account", { key: el.dataset.vis, visible: el.checked }).then((s) => { settings = s; }).catch(() => {});
  } else if (el.id === "thr" || el.id === "on-reset") {
    const thr = Number((document.getElementById("thr") as HTMLSelectElement).value);
    const onReset = (document.getElementById("on-reset") as HTMLInputElement).checked;
    invoke<Settings>("set_notify", { threshold: thr, onReset }).then((s) => { settings = s; }).catch(() => {});
  }
});

async function syncSettings() {
  const s = await invoke<Settings>("get_settings").catch(() => null);
  if (s) {
    settings = s;
    applyLang(s.zh);
  }
  render();
}

listen<Snapshot>("snapshot", (e) => { snapshot = e.payload; if (view === "main") render(); });
listen("settings-changed", () => { void syncSettings(); });
listen<boolean>("reports-changed", (e) => { reportUnread = e.payload; applyDots(); if (view === "settings") renderSettings(); });
invoke<boolean>("report_unread").then((u) => { reportUnread = u; applyDots(); }).catch(() => {});
listen<UpdateState>("update-state", (e) => { upd = e.payload; render(); });
invoke<UpdateState>("get_update_state").then((s) => { upd = s; if (view === "main") applyDots(); }).catch(() => {});
invoke<Snapshot>("get_snapshot").then((s) => { snapshot = s; render(); }).catch(() => render());
void syncSettings().then(async () => {
  // 測試用：AITOKEN_START_VIEW=settings 直接進設定頁
  const sv0 = await invoke<string>("start_view").catch(() => "");
  if (sv0 === "settings") await openSettings();
  else if (sv0 === "update") await openUpdate();
  else if (sv0 === "report") { await invoke("capture_panel_shot").catch(() => {}); await openReport(); }
  else if (sv0 === "reports") await openReports();
});
// 重置倒數每 30 秒重畫一次（不重新查詢）
setInterval(() => { if (view === "main") render(); }, 30_000);
