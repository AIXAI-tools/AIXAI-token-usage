import { invoke } from "@tauri-apps/api/core";
import { $, type ViewName } from "./util";
import type { ReportForm } from "./views/report";

// ---- 面板收起與還原 ----
// 主畫面點外面就收起；其他頁面點外面不收（要按 ✕）。
// 重開一律回到主畫面（重點是快速看額度），只保留還沒送出的回報草稿，下次進回報頁還在。
// 面板關掉就整個銷毀（省記憶體），所以草稿交給 Rust 放在記憶體裡，結束程式就清掉。
export type PanelDraft = { rep: ReportForm; mwReportPending?: boolean };

/** 回報表單有沒有還沒送出的內容（回覆開發者自動帶的「Re: 標題」不算） */
export function hasDraft(rep: ReportForm): boolean {
  if (rep.doneId) return false;
  return !!(rep.body.trim() || rep.contact.trim() || (!rep.ref && rep.title.trim()));
}

export function createPanel(deps: {
  getView: () => ViewName;
  getRep: () => ReportForm;
  getMwPending: () => boolean;
  restore: (d: PanelDraft) => void;
  onDraftChange: () => void;
}) {
  let ready = false; // 還原完成前不回報，免得把上次的草稿蓋掉
  let had = false;

  function sync() {
    if (!ready) return;
    const rep = deps.getRep();
    const draft = hasDraft(rep);
    const d: PanelDraft = { rep: { ...rep, sending: false, copied: false, preview: "", previewText: "", err: "", errDetail: "" }, mwReportPending: deps.getMwPending() };
    void invoke("panel_state_set", { sticky: deps.getView() !== "main", draft: draft ? JSON.stringify(d) : null }).catch(() => {});
    if (draft !== had) { had = draft; deps.onDraftChange(); }
  }

  /** 啟動時呼叫：只把草稿放回去，不切換頁面 */
  async function start() {
    const raw = await invoke<string | null>("panel_draft_get").catch(() => null);
    let d: PanelDraft | null = null;
    try { d = raw ? JSON.parse(raw) as PanelDraft : null; } catch { d = null; }
    if (d?.rep) deps.restore(d);
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
