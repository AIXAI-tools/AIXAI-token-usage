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
import {
  getClaudeStatusline,
  installClaudeStatusline,
  setClaudeStatuslineShow,
  type ClaudeStatuslineState,
} from "./views/claudeStatus";
import {
  checkModelsNow,
  dismissModelChange,
  getModelWatch,
  reportModelChange,
  setModelWatch,
  type ModelWatchState,
} from "./views/modelWatch";

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
let modelWatch: ModelWatchState | null = null;
let mwChecking = false;
let mwReportPending = false;
let claudeStatus: ClaudeStatuslineState | null = null;
let csInstalling: string | null = null;
let csFailed: { id: string; error: string } | null = null;

async function openUpdate() {
  upd = await invoke<UpdateState>("get_update_state").catch(() => upd);
  view = "update";
  render();
}

function applyDots() {
  $("gear-dot").hidden = !(upd?.has_update || reportUnread);
}

function renderReport() {
  renderReportView(rep, prep, reportUnread);
}

async function openReport(ref: ReportDto | null = null) {
  if (ref) {
    rep = newForm();
    rep.ref = ref.id;
    rep.title = `Re: ${ref.title}`.slice(0, 100);
    mwReportPending = false;
  } else if (rep.ref || rep.doneId) {
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
  renderSettingsView({ settings, upd, reportUnread, addPlatform, addHint, detectMsg, modelWatch, mwChecking, claudeStatus, csInstalling, csFailed });
}

async function loadHint() {
  addHint = await invoke<{ dir: string; command: string } | null>("add_account_hint", { platform: addPlatform }).catch(() => null);
}

async function openSettings() {
  settings = await invoke<Settings>("get_settings").catch(() => settings);
  detectMsg = "";
  await Promise.all([loadHint(), getClaudeStatusline().then((cs) => { if (cs) claudeStatus = cs; })]);
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
  renderMain(snapshot, showUsed, collapsed, modelWatch, csInstalling, csFailed);
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
  () => ({ settings, upd, reportUnread, addPlatform, addHint, detectMsg, modelWatch, mwChecking, claudeStatus, csInstalling, csFailed }),
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
      if (!mwReportPending) return;
      mwReportPending = false;
      void (async () => {
        const mw = modelWatch ?? await getModelWatch();
        for (const c of mw?.pending ?? []) {
          const next = await dismissModelChange(c.platform);
          if (next) modelWatch = next;
        }
        if (view === "main" || view === "settings") render();
      })();
    },
  },
);

function runClaudeInstall(id: string) {
  if (csInstalling) return;
  csInstalling = id;
  csFailed = null;
  render();
  void installClaudeStatusline(id)
    .then((cs) => {
      if ("error" in cs) { csFailed = { id, error: cs.error }; return; }
      claudeStatus = cs;
      return invoke<Snapshot>("refresh").then((s) => { snapshot = s; }).catch(() => {});
    })
    .finally(() => {
      csInstalling = null;
      if (view === "main" || view === "settings") render();
    });
}

$("groups").addEventListener("click", (e) => {
  const el = e.target as HTMLElement;
  const csBtn = el.closest<HTMLElement>("[data-cs-install]");
  if (csBtn?.dataset.csInstall) {
    runClaudeInstall(csBtn.dataset.csInstall);
    return;
  }
  if (el.closest("[data-mw-report]")) {
    void openModelReport();
    return;
  }
  const dis = el.closest<HTMLElement>("[data-mw-dismiss]");
  if (dis?.dataset.mwDismiss) {
    void dismissModelChange(dis.dataset.mwDismiss).then((mw) => {
      if (mw) { modelWatch = mw; if (view === "main") render(); }
    });
  }
});

$("view-settings").addEventListener("click", (e) => {
  const el = e.target as HTMLElement;
  const csBtn = el.closest<HTMLElement>("[data-cs-install]");
  if (csBtn?.dataset.csInstall) {
    runClaudeInstall(csBtn.dataset.csInstall);
    return;
  }
  if (!el.closest("#mw-check") || mwChecking) return;
  mwChecking = true;
  render();
  void checkModelsNow().then((mw) => {
    if (mw) modelWatch = mw;
  }).finally(() => {
    mwChecking = false;
    if (view === "settings" || view === "main") render();
  });
});

$("view-settings").addEventListener("change", (e) => {
  const el = e.target as HTMLInputElement;
  if (el.id === "cs-show") {
    void setClaudeStatuslineShow(el.checked).then((cs) => { if (cs) claudeStatus = cs; });
  } else if (el.id === "mw-enabled") {
    void setModelWatch({ enabled: el.checked }).then((mw) => { if (mw) modelWatch = mw; });
  } else if (el.id === "mw-auto") {
    void setModelWatch({ auto_send: el.checked }).then((mw) => { if (mw) modelWatch = mw; });
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
listen<ModelWatchState>("model-watch", (e) => { modelWatch = e.payload; if (view === "main" || view === "settings") render(); });
listen("open-model-report", () => { void openModelReport(); });
void getModelWatch().then((mw) => { if (mw) { modelWatch = mw; if (view === "main" || view === "settings") render(); } });
void getClaudeStatusline().then((cs) => { if (cs) { claudeStatus = cs; if (view === "settings") render(); } });
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
