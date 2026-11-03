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
  source?: "live" | "local_log" | "cache" | null;
  as_of?: number | null;
  reason?: Reason | null;
  account_masked?: string | null;
  account_full?: string | null;
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
type Settings = {
  language: string;
  zh: boolean;
  notify_threshold: number;
  notify_on_reset: boolean;
  autostart: boolean;
  accounts: AccountDto[];
};

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
let view: "main" | "settings" = "main";
let addPlatform: "claude" | "codex" = "claude";
let addHint: { dir: string; command: string } | null = null;
let detectMsg = "";

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
  const accts = s.accounts
    .map((a) => {
      const email = a.email_masked ? `<small title="${esc(a.email_full ?? "")}">${esc(a.email_masked)}</small>` : "";
      const rm = a.removable ? `<button class="icon-btn rm" type="button" data-remove="${esc(a.key)}" title="${esc(t("settings.remove"))}">✕</button>` : "";
      return `<div class="acct">
        <div class="acct-info"><b>${esc(platformName(a.platform))}</b>${email}</div>
        <input class="name-in" type="text" maxlength="24" data-name="${esc(a.key)}" value="${esc(a.name)}" placeholder="${esc(t("settings.name_ph"))}" aria-label="${esc(t("settings.name"))}">
        <label class="switch" title="${esc(t("settings.show"))}"><input type="checkbox" data-vis="${esc(a.key)}" ${a.visible ? "checked" : ""}><span></span></label>${rm}
      </div>`;
    })
    .join("");
  const thr = [5, 10, 20, 30].map((n) => `<option value="${n}" ${n === s.notify_threshold ? "selected" : ""}>${n}%</option>`).join("");
  return `
  <header class="head">
    <button id="back" class="icon-btn" type="button" aria-label="${esc(t("settings.back"))}" title="${esc(t("settings.back"))}">‹</button>
    <span class="title">${esc(t("settings.title"))}</span>
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
      <div class="cmd"><code id="cmd">${esc(addHint?.command ?? "")}</code><button id="copy" class="pill" type="button">${esc(t("settings.copy"))}</button></div>
      <div class="detect"><span class="grow">${esc(t("settings.after_login"))}</span><button id="detect" class="pill" type="button">${esc(t("settings.detect"))}</button></div>
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
  $("view-settings").hidden = isMain;
  if (!isMain) {
    renderSettings();
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
$("gear").addEventListener("click", () => { void openSettings(); });

const sv = $("view-settings");
sv.addEventListener("click", (e) => {
  const el = e.target as HTMLElement;
  if (el.closest("#back")) return closeSettings();
  const add = el.closest<HTMLElement>("[data-add]");
  if (add) {
    addPlatform = add.dataset.add as "claude" | "codex";
    detectMsg = "";
    void loadHint().then(renderSettings);
    return;
  }
  if (el.closest("#copy")) {
    const text = addHint?.command ?? "";
    const done = () => { const b = $("copy"); b.textContent = t("settings.copied"); setTimeout(() => { b.textContent = t("settings.copy"); }, 1500); };
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
  const rm = el.closest<HTMLElement>("[data-remove]");
  if (rm) {
    invoke<Settings>("remove_account", { key: rm.dataset.remove }).then((s) => { settings = s; renderSettings(); fit(); }).catch(() => {});
  }
});
sv.addEventListener("change", (e) => {
  const el = e.target as HTMLInputElement | HTMLSelectElement;
  if (el instanceof HTMLInputElement && el.dataset.name) {
    invoke<Settings>("set_account", { key: el.dataset.name, name: el.value }).then((s) => { settings = s; }).catch(() => {});
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
invoke<Snapshot>("get_snapshot").then((s) => { snapshot = s; render(); }).catch(() => render());
void syncSettings().then(async () => {
  // 測試用：AITOKEN_START_VIEW=settings 直接進設定頁
  if ((await invoke<string>("start_view").catch(() => "")) === "settings") await openSettings();
});
// 重置倒數每 30 秒重畫一次（不重新查詢）
setInterval(() => { if (view === "main") render(); }, 30_000);
