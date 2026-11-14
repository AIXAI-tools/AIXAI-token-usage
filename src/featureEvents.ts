import { invoke } from "@tauri-apps/api/core";
import { $, type Snapshot, type ViewName } from "./util";
import { installClaudeStatusline, setClaudeStatuslineShow, type ClaudeStatuslineState } from "./views/claudeStatus";
import { checkModelsNow, dismissModelChange, setModelWatch, type ModelWatchState } from "./views/modelWatch";

// Claude 狀態列一鍵設定、新模型偵測：主畫面卡片與設定頁上的按鈕與開關
export type FeatureState = {
  claudeStatus: ClaudeStatuslineState | null;
  csInstalling: string | null;
  csFailed: { id: string; error: string } | null;
  modelWatch: ModelWatchState | null;
  mwChecking: boolean;
};

export function bindFeatureEvents(ext: FeatureState, deps: {
  getView: () => ViewName;
  render: () => void;
  setSnapshot: (s: Snapshot) => void;
  openModelReport: () => Promise<void>;
}) {
  const onPage = (...v: ViewName[]) => v.includes(deps.getView());

  function runClaudeInstall(id: string) {
    if (ext.csInstalling) return;
    ext.csInstalling = id;
    ext.csFailed = null;
    deps.render();
    void installClaudeStatusline(id)
      .then((cs) => {
        if ("error" in cs) { ext.csFailed = { id, error: cs.error }; return; }
        ext.claudeStatus = cs;
        return invoke<Snapshot>("refresh").then((s) => { deps.setSnapshot(s); }).catch(() => {});
      })
      .finally(() => {
        ext.csInstalling = null;
        if (onPage("main", "settings")) deps.render();
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
      void deps.openModelReport();
      return;
    }
    const dis = el.closest<HTMLElement>("[data-mw-dismiss]");
    if (dis?.dataset.mwDismiss) {
      void dismissModelChange(dis.dataset.mwDismiss).then((mw) => {
        if (mw) { ext.modelWatch = mw; if (onPage("main")) deps.render(); }
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
    if (!el.closest("#mw-check") || ext.mwChecking) return;
    ext.mwChecking = true;
    deps.render();
    void checkModelsNow().then((mw) => {
      if (mw) ext.modelWatch = mw;
    }).finally(() => {
      ext.mwChecking = false;
      if (onPage("settings", "main")) deps.render();
    });
  });

  $("view-settings").addEventListener("change", (e) => {
    const el = e.target as HTMLInputElement;
    if (el.id === "cs-show") {
      void setClaudeStatuslineShow(el.checked).then((cs) => { if (cs) ext.claudeStatus = cs; });
    } else if (el.id === "mw-enabled") {
      void setModelWatch({ enabled: el.checked }).then((mw) => { if (mw) ext.modelWatch = mw; });
    } else if (el.id === "mw-auto") {
      void setModelWatch({ auto_send: el.checked }).then((mw) => { if (mw) ext.modelWatch = mw; });
    }
  });
}
