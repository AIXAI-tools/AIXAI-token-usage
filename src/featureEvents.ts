import { invoke } from "@tauri-apps/api/core";
import { $, type Snapshot, type ViewName } from "./util";
import { installClaudeStatusline, setClaudeStatuslineShow, setTerminalLine, type ClaudeStatuslineState, type TermState } from "./views/claudeStatus";
import { checkModelsNow, dismissModelChange, setModelWatch, type ModelWatchState } from "./views/modelWatch";

// Claude 狀態列一鍵設定、新模型偵測：主畫面卡片與設定頁上的按鈕與開關
export type FeatureState = {
  claudeStatus: ClaudeStatuslineState | null;
  csInstalling: string | null;
  csFailed: { id: string; error: string } | null;
  modelWatch: ModelWatchState | null;
  mwChecking: boolean;
  term: TermState | null;
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

  // 終端機要顯示哪些：存檔後 Rust 立刻重寫 terminal-line.json，下次 Claude Code 回應就生效
  function saveTerm(cfg: TermState["cfg"]) {
    void setTerminalLine(cfg).then((s) => { if (s) ext.term = s; if (onPage("settings")) deps.render(); });
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
    const usedBtn = el.closest<HTMLElement>("[data-term-used]");
    if (usedBtn && ext.term) {
      saveTerm({ ...ext.term.cfg, used: usedBtn.dataset.termUsed === "1" });
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
      void setClaudeStatuslineShow(el.checked).then((cs) => { if (cs) { ext.claudeStatus = cs; if (onPage("settings")) deps.render(); } });
    } else if (ext.term && (el.dataset.term || el.dataset.termGroup)) {
      const cfg = { ...ext.term.cfg, agy_off: [...ext.term.cfg.agy_off] };
      const k = el.dataset.term as "claude" | "codex" | "agy" | undefined;
      if (k) cfg[k] = el.checked;
      const g = el.dataset.termGroup;
      if (g) {
        cfg.agy_off = cfg.agy_off.filter((x) => x !== g);
        if (!el.checked) cfg.agy_off.push(g);
        else cfg.agy = true; // 勾了某一組，Antigravity 本身也打開
      }
      saveTerm(cfg);
    } else if (el.id === "mw-enabled") {
      void setModelWatch({ enabled: el.checked }).then((mw) => { if (mw) ext.modelWatch = mw; });
    } else if (el.id === "mw-auto") {
      void setModelWatch({ auto_send: el.checked }).then((mw) => { if (mw) ext.modelWatch = mw; });
    }
  });
}
