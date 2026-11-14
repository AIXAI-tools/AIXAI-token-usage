import { invoke } from "@tauri-apps/api/core";
import { $, type ViewName } from "./util";
import type { ReportForm } from "./views/report";

// ---- 面板收起與還原 ----
// 主畫面點外面就收起；其他頁面點外面不收（要按 ✕），收起後下次打開回到同一頁、保留回報草稿。
// 面板關掉就整個銷毀（省記憶體），所以頁面與草稿交給 Rust 放在記憶體裡。
export type PanelDraft = { view: ViewName; rep?: ReportForm; mwReportPending?: boolean };

export function createPanel(deps: {
  getView: () => ViewName;
  getRep: () => ReportForm;
  getMwPending: () => boolean;
  restore: (d: PanelDraft) => Promise<void>;
}) {
  let ready = false; // 還原完成前不回報，免得把上次的草稿蓋掉

  function sync() {
    if (!ready) return;
    const view = deps.getView();
    const sticky = view !== "main";
    const d: PanelDraft = { view, mwReportPending: deps.getMwPending() };
    if (view === "report") d.rep = { ...deps.getRep(), sending: false, copied: false, preview: "", previewText: "" };
    void invoke("panel_state_set", { sticky, draft: sticky ? JSON.stringify(d) : null }).catch(() => {});
  }

  /** 啟動時呼叫：restoreDraft 為 false（測試用 AITOKEN_START_VIEW）時不還原，但之後照常回報 */
  async function start(restoreDraft: boolean) {
    if (restoreDraft) {
      const raw = await invoke<string | null>("panel_draft_get").catch(() => null);
      let d: PanelDraft | null = null;
      try { d = raw ? JSON.parse(raw) as PanelDraft : null; } catch { d = null; }
      if (d) await deps.restore(d);
    }
    ready = true;
    sync();
  }

  document.addEventListener("click", (e) => {
    if (!(e.target as HTMLElement).closest("[data-close]")) return;
    sync();
    void invoke("panel_close").catch(() => {});
  });
  // 回報表單打字、勾選不會重畫，另外回報草稿
  $("view-report").addEventListener("input", sync);
  $("view-report").addEventListener("change", sync);

  return { sync, start };
}
