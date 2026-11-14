import { invoke } from "@tauri-apps/api/core";
import { $, closeBtn, copyText, dict, esc, mmdd, t, type Prepare, type ReportDto, type ViewName } from "../util";

// 回報頁狀態（輸入內容存在這裡，重畫不會掉）
export const newForm = () => ({
  kind: "bug" as "bug" | "idea", title: "", body: "", contact: "",
  a_sys: true, a_status: true, a_shot: true, a_log: true, want: true,
  ref: null as string | null, preview: "" as "" | "sys" | "status" | "log" | "all", previewText: "",
  err: "", errDetail: "", sending: false, doneId: "", copied: false,
});

export type ReportForm = ReturnType<typeof newForm>;

export type ReportState = {
  rep: ReportForm;
  prep: Prepare | null;
  reportUnread: boolean;
  reports: ReportDto[];
  newReplyIds: Set<string>;
  repChecking: boolean;
};

export function reportErrText(key: string, detail = ""): string {
  const k = `report.err.${key}`;
  return k in dict ? t(k) : t("report.err.other", { detail: detail || key });
}

export function reportReq(rep: ReportForm, prep: Prepare | null) {
  return {
    kind: rep.kind, title: rep.title, body: rep.body, contact: rep.contact,
    attach_sys: rep.a_sys, attach_status: rep.a_status, attach_log: rep.a_log, attach_shot: rep.a_shot && !!prep?.shot_b64,
    want_reply: rep.want, ref_id: rep.ref,
  };
}

export function reportHtml(rep: ReportForm, prep: Prepare | null, reportUnread: boolean): string {
  const p = prep;
  const head = `<header class="head">
    <button id="rp-back" class="icon-btn" type="button" aria-label="${esc(t("report.back_settings"))}" title="${esc(t("report.back_settings"))}">‹</button>
    <span class="title">${esc(rep.ref ? t("report.title_reply") : t("report.title"))}</span>
    <button id="rp-mine" class="pill" type="button">${esc(t("report.mine"))}${reportUnread ? '<i class="dot"></i>' : ""}</button>
    ${closeBtn()}
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

export function renderReport(rep: ReportForm, prep: Prepare | null, reportUnread: boolean) {
  $("view-report").innerHTML = reportHtml(rep, prep, reportUnread);
}

// ---- 我的回報 ----
export function reportsHtml(reports: ReportDto[], newReplyIds: Set<string>, repChecking: boolean): string {
  const items = reports.map((r) => {
    let rb = "";
    if (!r.want_reply) rb = `<span class="msg">${esc(t("reports.not_wanted"))}</span>`;
    else if (!r.replies.length) rb = `<span class="msg">${esc(t("reports.no_reply"))}</span>`;
    else {
      rb = r.replies.map((p) => {
        const isNew = newReplyIds.has(p.rid);
        const kindTxt = t(`reports.kind.${p.kind}`);
        const urls = p.urls.length ? `<div class="urls">${p.urls.map((u) => esc(u)).join("<br>")}</div>` : "";
        // 回覆的版本可能帶 v（例 v0.2.5），文字本身已有 v，先去掉避免顯示 vv0.2.5
        const ver = (p.version ?? "").replace(/^[vV]/, "");
        const fixed = p.kind === "fixed" && ver
          ? (p.can_update
            ? `<button class="pill primary" type="button" data-upd="1">${esc(t("reports.update_to", { v: ver }))}</button>`
            : `<span class="msg">${esc(t("reports.fixed_in", { v: ver }))}</span>`)
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
    ${closeBtn()}
  </header>
  <div class="sbody">${items || `<div class="sec"><span class="msg">${esc(t("reports.empty"))}</span></div>`}</div>`;
}

export function renderReports(reports: ReportDto[], newReplyIds: Set<string>, repChecking: boolean) {
  $("view-reports").innerHTML = reportsHtml(reports, newReplyIds, repChecking);
}

export function bindReportEvents(
  getState: () => ReportState,
  actions: {
    getView: () => ViewName;
    setView: (v: ViewName) => void;
    setRep: (r: ReportForm) => void;
    setReports: (l: ReportDto[]) => void;
    setRepChecking: (b: boolean) => void;
    render: () => void;
    fit: () => void;
    openReports: () => Promise<void>;
    openReport: (ref?: ReportDto | null) => Promise<void>;
    openUpdate: () => Promise<void>;
    onReportSent?: () => void;
  },
) {
  const rerenderReport = () => { const st = getState(); renderReport(st.rep, st.prep, st.reportUnread); };
  const rerenderReports = () => { const st = getState(); renderReports(st.reports, st.newReplyIds, st.repChecking); };

  const rv = $("view-report");
  rv.addEventListener("input", (e) => {
    const el = e.target as HTMLInputElement | HTMLTextAreaElement;
    const { rep } = getState();
    if (el.id === "rp-title") rep.title = el.value;
    else if (el.id === "rp-body") rep.body = el.value;
    else if (el.id === "rp-contact") rep.contact = el.value;
  });
  async function refreshPreview() {
    const st = getState();
    st.rep.previewText = await invoke<string>("report_preview", { req: reportReq(st.rep, st.prep) }).catch(() => "");
    rerenderReport();
    actions.fit();
  }
  rv.addEventListener("change", (e) => {
    const el = e.target as HTMLInputElement;
    const a = el.dataset?.a as "a_sys" | "a_status" | "a_shot" | "a_log" | "want" | undefined;
    if (a) {
      const { rep } = getState();
      rep[a] = el.checked;
      if (rep.preview === "all") void refreshPreview();
      else { rerenderReport(); actions.fit(); }
    }
  });
  rv.addEventListener("click", (e) => {
    const el = e.target as HTMLElement;
    const st = getState();
    if (el.closest("#rp-back")) { actions.setView("settings"); actions.render(); return; }
    if (el.closest("#rp-mine")) { void actions.openReports(); return; }
    if (el.closest("#rp-done")) { actions.setRep(newForm()); actions.setView("settings"); actions.render(); return; }
    const k = el.closest<HTMLElement>("[data-kind]");
    if (k) { st.rep.kind = k.dataset.kind as "bug" | "idea"; rerenderReport(); return; }
    const pvb = el.closest<HTMLElement>("[data-pv]");
    if (pvb) { const key = pvb.dataset.pv as "sys" | "status" | "log"; st.rep.preview = st.rep.preview === key ? "" : key; rerenderReport(); actions.fit(); return; }
    if (el.closest("#rp-preview")) {
      if (st.rep.preview === "all") { st.rep.preview = ""; rerenderReport(); actions.fit(); } else { st.rep.preview = "all"; void refreshPreview(); }
      return;
    }
    if (el.closest("#rp-copy")) {
      void invoke<string>("report_preview", { req: reportReq(st.rep, st.prep) }).then((text) => {
        copyText(`${st.rep.title}\n\n${text}`, () => { st.rep.copied = true; rerenderReport(); setTimeout(() => { st.rep.copied = false; if (actions.getView() === "report") rerenderReport(); }, 1500); });
      });
      return;
    }
    if (el.closest("#rp-send")) {
      if (st.rep.sending) return;
      st.rep.sending = true; st.rep.err = ""; st.rep.errDetail = "";
      rerenderReport(); actions.fit();
      invoke<string>("report_send", { req: reportReq(st.rep, st.prep) }).then((id) => {
        const next = newForm();
        next.doneId = id;
        actions.setRep(next);
        actions.onReportSent?.();
      }).catch((err) => {
        st.rep.errDetail = String(err);
        st.rep.err = /^http_\d+$/.test(String(err)) ? "bad_response" : String(err);
      }).finally(() => { getState().rep.sending = false; if (actions.getView() === "report") { rerenderReport(); actions.fit(); } });
    }
  });

  const rsv = $("view-reports");
  rsv.addEventListener("click", (e) => {
    const el = e.target as HTMLElement;
    const st = getState();
    if (el.closest("#rs-back")) { actions.setView("report"); actions.render(); return; }
    if (el.closest("[data-upd]")) { void actions.openUpdate(); return; }
    const re = el.closest<HTMLElement>("[data-reply]");
    if (re) { const r = st.reports.find((x) => x.id === re.dataset.reply); if (r) void actions.openReport(r); return; }
    if (el.closest("#rs-check")) {
      actions.setRepChecking(true); rerenderReports();
      invoke<ReportDto[]>("report_check_cmd").then((l) => {
        actions.setReports(l);
        for (const r of l) for (const p of r.replies) if (p.unread) st.newReplyIds.add(p.rid);
      }).catch(() => {}).finally(() => {
        actions.setRepChecking(false);
        if (actions.getView() === "reports") {
          rerenderReports();
          for (const r of getState().reports) if (r.replies.some((p) => p.unread)) void invoke("report_mark_seen", { id: r.id }).catch(() => {});
        }
      });
    }
  });
}
