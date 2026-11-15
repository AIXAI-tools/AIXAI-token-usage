// 提醒視窗（TASK-024，示意圖 notify-buttons v2）：卡片由後端 alert.rs 管，這裡只負責畫與回傳按鈕
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Card = { key: string; title: string; body: string; tone: "warn" | "info"; buttons: { text: string; action: string; primary: boolean }[] };

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
const root = document.getElementById("cards")!;

function render(cards: Card[]) {
  root.innerHTML = cards
    .map((c) => `<section class="card ${c.tone}" data-key="${esc(c.key)}">
      <button type="button" class="x" data-close aria-label="close">✕</button>
      <div class="app"><img src="/app-icon.png" alt="">AIXAI Token Usage</div>
      <div class="t">${esc(c.title)}</div>
      <div class="b">${esc(c.body)}</div>
      ${c.buttons.length ? `<div class="acts">${c.buttons.map((b) => `<button type="button" class="${b.primary ? "pri" : ""}" data-action="${esc(b.action)}">${esc(b.text)}</button>`).join("")}</div>` : ""}
    </section>`)
    .join("");
  // 畫好後把實際高度告訴後端，由後端調整大小、貼到右下角並顯示
  requestAnimationFrame(() => void invoke("alert_resize", { height: Math.ceil(root.getBoundingClientRect().height) }).catch(() => {}));
}

root.addEventListener("click", (e) => {
  const el = e.target as HTMLElement;
  const card = el.closest<HTMLElement>(".card");
  const key = card?.dataset.key;
  if (!key) return;
  if (el.closest("[data-close]")) {
    void invoke("alert_close", { key }).catch(() => {});
    return;
  }
  const btn = el.closest<HTMLButtonElement>("button[data-action]");
  if (!btn) return;
  // 按了先鎖住，避免連點登記兩次；後端重算後會送新的卡片內容
  btn.disabled = true;
  void invoke("alert_action", { key, action: btn.dataset.action }).catch(() => {}).finally(() => {
    if (btn.dataset.action?.startsWith("open:")) btn.disabled = false;
  });
});

void listen<Card[]>("alerts", (e) => render(e.payload));
void invoke<Card[]>("get_alerts").then(render).catch(() => {});
