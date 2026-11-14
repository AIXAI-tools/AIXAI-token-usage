import { invoke } from "@tauri-apps/api/core";
import { $, dict, esc, hhmm, t, type UpdateState, type ViewName } from "../util";

export function failText(key: string): string {
  const k = `update.err.${key}`;
  return k in dict ? t(k) : t("update.err.other", { detail: key });
}

export function updateHtml(u: UpdateState): string {
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

export function renderUpdate(upd: UpdateState | null) {
  if (!upd) return;
  $("view-update").innerHTML = updateHtml(upd);
}

export function bindUpdateEvents(actions: {
  getView: () => ViewName;
  setView: (v: ViewName) => void;
  setUpd: (u: UpdateState) => void;
  render: () => void;
}) {
  $("view-update").addEventListener("click", (e) => {
    const el = e.target as HTMLElement;
    if (el.closest("#back-settings")) { actions.setView("settings"); actions.render(); return; }
    if (el.closest("#to-releases")) { void invoke("open_releases_page").catch(() => {}); return; }
    if (el.closest("#do-install")) { void invoke("install_update_cmd").catch(() => {}); return; }
    if (el.closest("#recheck")) {
      invoke<UpdateState>("check_update_cmd").then((s) => { actions.setUpd(s); if (actions.getView() === "update") actions.render(); }).catch(() => {});
    }
  });
}
