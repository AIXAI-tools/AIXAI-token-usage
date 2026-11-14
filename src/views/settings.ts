import { invoke } from "@tauri-apps/api/core";
import { $, closeBtn, esc, hhmm, t, type OfflineDto, type Settings, type UpdateState } from "../util";
import { settingsSectionHtml as claudeStatusSectionHtml, type ClaudeStatuslineState, type TermState } from "./claudeStatus";
import { settingsSectionHtml, type ModelWatchState } from "./modelWatch";

export type SettingsState = {
  settings: Settings | null;
  upd: UpdateState | null;
  reportUnread: boolean;
  addPlatform: "claude" | "codex";
  addHint: { dir: string; command: string } | null;
  detectMsg: string;
  modelWatch?: ModelWatchState | null;
  mwChecking?: boolean;
  claudeStatus?: ClaudeStatuslineState | null;
  term?: TermState | null;
  csInstalling?: string | null;
  csFailed?: { id: string; error: string } | null;
};

export function platformName(p: string): string {
  return t(`platform.${p}`);
}

export function settingsHtml(s: Settings, st: SettingsState): string {
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
  const accountNames = Object.fromEntries(
    s.accounts.filter((a) => a.platform === "claude").map((a) => [a.key, a.name || a.email_masked || ""]),
  );
  return `
  <header class="head">
    <button id="back" class="icon-btn" type="button" aria-label="${esc(t("settings.back"))}" title="${esc(t("settings.back"))}">‹</button>
    <span class="title">${esc(t("settings.title"))}</span>
    <button id="to-update" class="pill" type="button">${esc(t("settings.update"))}${st.upd?.has_update ? '<i class="dot"></i>' : ""}</button>
    <button id="to-report" class="pill" type="button">${esc(t("settings.report"))}${st.reportUnread ? '<i class="dot"></i>' : ""}</button>
    ${closeBtn()}
  </header>
  <div class="sbody">
    <section class="sec">
      <h3>${esc(t("settings.accounts"))}</h3>
      ${accts}
    </section>
    <section class="sec">
      <h3>${esc(t("settings.add"))}</h3>
      <div class="seg">
        <button type="button" data-add="claude" class="${st.addPlatform === "claude" ? "on" : ""}">Claude</button>
        <button type="button" data-add="codex" class="${st.addPlatform === "codex" ? "on" : ""}">Codex</button>
      </div>
      <ol class="steps"><li>${esc(t("settings.step1"))}</li><li>${esc(t("settings.step2"))}</li><li>${esc(t("settings.step3"))}</li></ol>
      <div class="cmd"><code id="cmd">${esc(st.addHint?.command ?? "")}</code></div>
      <div class="cmd-btns">
        <button id="open-ps" class="pill primary" type="button">${esc(t("settings.open_ps"))}</button>
        <button id="copy" class="pill" type="button">${esc(t("settings.copy_only"))}</button>
        <span class="grow"></span>
        <button id="detect" class="pill" type="button">${esc(t("settings.detect"))}</button>
      </div>
      ${st.detectMsg ? `<div class="msg">${esc(st.detectMsg)}</div>` : ""}
    </section>
    ${claudeStatusSectionHtml(st.claudeStatus ?? null, accountNames, st.csInstalling ?? null, st.csFailed ?? null, st.term ?? null)}
    <section class="sec">
      <h3>${esc(t("settings.notify"))}</h3>
      <div class="srow"><span class="grow">${esc(t("settings.notify_below"))}</span><select id="thr">${thr}</select></div>
      <div class="srow"><span class="grow">${esc(t("settings.notify_reset"))}</span><label class="switch"><input id="on-reset" type="checkbox" ${s.notify_on_reset ? "checked" : ""}><span></span></label></div>
    </section>
    ${settingsSectionHtml(st.modelWatch ?? null, st.mwChecking)}
    <p class="note">${esc(t("settings.note"))}</p>
  </div>`;
}

export function renderSettings(st: SettingsState) {
  if (!st.settings) return;
  // 重畫會換掉 .sbody，先記下捲動位置再還原（例：按「立即檢查」後不要跳回最上面）
  const root = $("view-settings");
  const top = root.querySelector<HTMLElement>(".sbody")?.scrollTop ?? 0;
  root.innerHTML = settingsHtml(st.settings, st);
  const body = root.querySelector<HTMLElement>(".sbody");
  if (body && top) body.scrollTop = top;
}

export function bindSettingsEvents(
  getState: () => SettingsState,
  actions: {
    setSettings: (s: Settings) => void;
    setAddPlatform: (p: "claude" | "codex") => void;
    setDetectMsg: (m: string) => void;
    loadHint: () => Promise<void>;
    closeSettings: () => void;
    openUpdate: () => Promise<void>;
    openReport: () => Promise<void>;
    fit: () => void;
  },
) {
  const sv = $("view-settings");
  const rerender = () => renderSettings(getState());
  sv.addEventListener("click", (e) => {
    const el = e.target as HTMLElement;
    if (el.closest("#back")) return actions.closeSettings();
    if (el.closest("#to-update")) { void actions.openUpdate(); return; }
    if (el.closest("#to-report")) { void actions.openReport(); return; }
    if (el.closest("#open-ps")) {
      actions.setDetectMsg("");
      invoke<boolean>("open_powershell_login", { platform: getState().addPlatform }).then((ok) => {
        if (!ok) { actions.setDetectMsg(t("settings.ps_failed")); rerender(); actions.fit(); }
      }).catch(() => {});
      return;
    }
    const add = el.closest<HTMLElement>("[data-add]");
    if (add) {
      actions.setAddPlatform(add.dataset.add as "claude" | "codex");
      actions.setDetectMsg("");
      void actions.loadHint().then(rerender);
      return;
    }
    if (el.closest("#copy")) {
      const text = getState().addHint?.command ?? "";
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
        actions.setSettings(s);
        actions.setDetectMsg(n > 0 ? t("settings.detected", { n }) : t("settings.none_found"));
        await actions.loadHint();
        rerender();
        actions.fit();
      }).catch(() => {});
      return;
    }
    const fg = el.closest<HTMLElement>("[data-forget]");
    if (fg) {
      invoke<Settings>("forget_offline", { id: fg.dataset.forget }).then((s) => { actions.setSettings(s); rerender(); actions.fit(); }).catch(() => {});
      return;
    }
    const rm = el.closest<HTMLElement>("[data-remove]");
    if (rm) {
      invoke<Settings>("remove_account", { key: rm.dataset.remove }).then((s) => { actions.setSettings(s); rerender(); actions.fit(); }).catch(() => {});
    }
  });

  sv.addEventListener("change", (e) => {
    const el = e.target as HTMLInputElement | HTMLSelectElement;
    if (el instanceof HTMLInputElement && el.dataset.name) {
      invoke<Settings>("set_account", { key: el.dataset.name, name: el.value }).then((s) => { actions.setSettings(s); }).catch(() => {});
    } else if (el instanceof HTMLInputElement && el.dataset.offvis) {
      invoke<Settings>("set_offline_visible", { id: el.dataset.offvis, visible: el.checked }).then((s) => { actions.setSettings(s); }).catch(() => {});
    } else if (el instanceof HTMLInputElement && el.dataset.vis) {
      invoke<Settings>("set_account", { key: el.dataset.vis, visible: el.checked }).then((s) => { actions.setSettings(s); }).catch(() => {});
    } else if (el.id === "thr" || el.id === "on-reset") {
      const thr = Number((document.getElementById("thr") as HTMLSelectElement).value);
      const onReset = (document.getElementById("on-reset") as HTMLInputElement).checked;
      invoke<Settings>("set_notify", { threshold: thr, onReset }).then((s) => { actions.setSettings(s); }).catch(() => {});
    }
  });
}
