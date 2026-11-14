import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  $,
  applyLang,
  dict,
  load,
  save,
  zhTW,
  type Prepare,
  type ReportDto,
  type Settings,
  type Snapshot,
  type UpdateState,
  type ViewName,
} from "./util";
import { bindMainEvents, renderMain } from "./views/main";
import { bindSettingsEvents, renderSettings as renderSettingsView } from "./views/settings";
import { bindUpdateEvents, renderUpdate } from "./views/update";
import {
  bindReportEvents,
  newForm,
  renderReport as renderReportView,
  renderReports as renderReportsView,
  type ReportForm,
} from "./views/report";
import { getClaudeStatusline, getTerminalLine } from "./views/claudeStatus";
import { dismissModelChange, getModelWatch, reportModelChange, type ModelWatchState } from "./views/modelWatch";
import { bindFeatureEvents, type FeatureState } from "./featureEvents";
import { createPanel, hasDraft } from "./panel";

applyLang(dict === zhTW);
let showUsed = load<boolean>("showUsed", false);
const collapsed = load<Record<string, boolean>>("collapsed", {});

let snapshot: Snapshot = { refreshing: true, providers: [] };
let settings: Settings | null = null;
let view: ViewName = "main";
let upd: UpdateState | null = null;
let addPlatform: "claude" | "codex" = "claude";
let addHint: { dir: string; command: string } | null = null;
let detectMsg = "";
let reportUnread = false;

let rep: ReportForm = newForm();
let prep: Prepare | null = null;
let reports: ReportDto[] = [];
let newReplyIds = new Set<string>();
let repChecking = false;
let mwReportPending = false;
// Claude 狀態列、新模型偵測的狀態（事件在 featureEvents.ts）
const ext: FeatureState = { claudeStatus: null, csInstalling: null, csFailed: null, modelWatch: null, mwChecking: false, term: null };

async function openUpdate() {
  upd = await invoke<UpdateState>("get_update_state").catch(() => upd);
  view = "update";
  render();
}

function applyDots() {
  $("gear-dot").hidden = !(upd?.has_update || reportUnread || hasDraft(rep));
}

function renderReport() {
  renderReportView(rep, prep, reportUnread);
}

async function openReport(ref: ReportDto | null = null) {
  // 有沒送出的草稿就接著寫：同一則回覆、或從「回報」按鈕回來時都保留
  if (ref) {
    if (rep.ref === ref.id && hasDraft(rep)) { /* 同一則回覆的草稿 */ } else {
      rep = newForm();
      rep.ref = ref.id;
      rep.title = `Re: ${ref.title}`.slice(0, 100);
      mwReportPending = false;
    }
  } else if ((rep.ref || rep.doneId) && !hasDraft(rep)) {
    rep = newForm();
    mwReportPending = false;
  }
  prep = await invoke<Prepare>("report_prepare").catch(() => prep);
  view = "report";
  render();
}

async function openModelReport() {
  const draft = await reportModelChange();
  if (!draft) return;
  rep = newForm();
  rep.kind = "idea";
  rep.title = draft.title.slice(0, 100);
  rep.body = draft.body.slice(0, 5000);
  rep.a_sys = true;
  rep.a_status = false;
  rep.a_shot = false;
  rep.a_log = false;
  rep.want = true;
  mwReportPending = true;
  prep = await invoke<Prepare>("report_prepare").catch(() => prep);
  view = "report";
  render();
}

function renderReports() {
  renderReportsView(reports, newReplyIds, repChecking);
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

function renderSettings() {
  renderSettingsView({ settings, upd, reportUnread: reportUnread || hasDraft(rep), addPlatform, addHint, detectMsg, ...ext });
}

async function loadHint() {
  addHint = await invoke<{ dir: string; command: string } | null>("add_account_hint", { platform: addPlatform }).catch(() => null);
}

async function openSettings() {
  settings = await invoke<Settings>("get_settings").catch(() => settings);
  detectMsg = "";
  await Promise.all([
    loadHint(),
    getClaudeStatusline().then((cs) => { if (cs) ext.claudeStatus = cs; }),
    getTerminalLine().then((tl) => { if (tl) ext.term = tl; }),
  ]);
  view = "settings";
  $("view-settings").innerHTML = ""; // 重新打開時從最上面開始（不沿用上次的捲動位置）
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
  panel.sync();
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
    renderUpdate(upd);
    fit();
    return;
  }
  renderMain(snapshot, showUsed, collapsed, ext.modelWatch, ext.csInstalling, ext.csFailed);
  fit();
}

// ---- 事件 ----
bindMainEvents({
  collapsed,
  toggleShowUsed: () => {
    showUsed = !showUsed;
    save("showUsed", showUsed);
  },
  setSnapshot: (s) => { snapshot = s; },
  render,
  openSettings,
});

bindSettingsEvents(
  () => ({ settings, upd, reportUnread: reportUnread || hasDraft(rep), addPlatform, addHint, detectMsg, ...ext }),
  {
    setSettings: (s) => { settings = s; },
    setAddPlatform: (p) => { addPlatform = p; },
    setDetectMsg: (m) => { detectMsg = m; },
    loadHint,
    closeSettings,
    openUpdate,
    openReport,
    fit,
  },
);
bindUpdateEvents({
  getView: () => view,
  setView: (v) => { view = v; },
  setUpd: (u) => { upd = u; },
  render,
});
bindReportEvents(
  () => ({ rep, prep, reportUnread, reports, newReplyIds, repChecking }),
  {
    getView: () => view,
    setView: (v) => { view = v; },
    setRep: (r) => { rep = r; },
    setReports: (l) => { reports = l; },
    setRepChecking: (b) => { repChecking = b; },
    render,
    fit,
    openReports,
    openReport,
    openUpdate,
    onReportSent: () => {
      panel.sync(); // 送出成功後草稿換成「已送出」畫面，避免重開又送一次
      if (!mwReportPending) return;
      mwReportPending = false;
      void (async () => {
        const mw = ext.modelWatch ?? await getModelWatch();
        for (const c of mw?.pending ?? []) {
          const next = await dismissModelChange(c.platform);
          if (next) ext.modelWatch = next;
        }
        if (view === "main" || view === "settings") render();
      })();
    },
  },
);

bindFeatureEvents(ext, {
  getView: () => view,
  render,
  setSnapshot: (s) => { snapshot = s; },
  openModelReport,
});

// 面板收起與還原（panel.ts）：放在 bindReportEvents 之後，回報表單先更新內容、草稿才同步到最新
const panel = createPanel({
  getView: () => view,
  getRep: () => rep,
  getMwPending: () => mwReportPending,
  restore: (d) => {
    rep = { ...newForm(), ...d.rep };
    mwReportPending = !!d.mwReportPending;
  },
  onDraftChange: () => { applyDots(); if (view === "settings") renderSettings(); },
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
listen<ModelWatchState>("model-watch", (e) => { ext.modelWatch = e.payload; if (view === "main" || view === "settings") render(); });
listen("open-model-report", () => { void openModelReport(); });
void getModelWatch().then((mw) => { if (mw) { ext.modelWatch = mw; if (view === "main" || view === "settings") render(); } });
void getClaudeStatusline().then((cs) => { if (cs) { ext.claudeStatus = cs; if (view === "settings") render(); } });
invoke<Snapshot>("get_snapshot").then((s) => { snapshot = s; render(); }).catch(() => render());
void syncSettings().then(async () => {
  // 測試用：AITOKEN_START_VIEW=settings 直接進設定頁
  await panel.start(); // 只放回回報草稿，頁面一律從主畫面開始
  const sv0 = await invoke<string>("start_view").catch(() => "");
  if (sv0 === "settings") await openSettings();
  else if (sv0 === "update") await openUpdate();
  else if (sv0 === "report") { await invoke("capture_panel_shot").catch(() => {}); await openReport(); }
  else if (sv0 === "reports") await openReports();
});
// 重置倒數每 30 秒重畫一次（不重新查詢）
setInterval(() => { if (view === "main") render(); }, 30_000);
