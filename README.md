# AIXAI Token Usage

Windows 系統匣小工具：即時查看 Codex／Claude／Antigravity 的訂閱額度剩多少、什麼時候重置。
A Windows tray app that shows how much of your Codex / Claude / Antigravity plan limits are left.

- 右下角三段圓環：綠＝Codex、橘＝Claude、紫＝Antigravity，亮起的長度＝該平台最緊額度的剩餘 %
- 點圖示打開面板；面板頭部的齒輪進入設定頁（帳號名稱／顯示、新增帳號、通知；有新版時齒輪出現紅點，設定頁的「更新」可下載安裝）
- 右鍵選單：立即重新整理、語言（跟隨系統／繁體中文／English）、開機自動啟動、結束
- 每 5 分鐘自動更新；介面預設跟隨系統語言；第一次執行預設開機自動啟動
- 低於門檻（預設 10%）與額度重置時發系統通知
- 新模型偵測：每 12 小時看一次三家的模型清單（不耗額度），有新模型時通知並在卡片上提示，可一鍵回報給開發者（預設不自動寄出）

## 目前支援

| 平台 | 資料來源 | 多帳號 |
|---|---|---|
| Codex | 官方 `codex app-server` 的 `account/rateLimits/read`；失敗時改讀本機紀錄 | 用 `CODEX_HOME` 分資料夾（`~/.codex-2`…） |
| Claude（含 Claude Code） | Claude Code 官方狀態列（[statusLine](https://code.claude.com/docs/en/statusline)）交給腳本的 `rate_limits`：5 小時與每週；只在你用 Claude Code 時更新 | 用 `CLAUDE_CONFIG_DIR` 分資料夾（`~/.claude-2`…）；用 `/login` 換帳號時，舊帳號保留成「離線」卡片 |
| Antigravity | 官方 `agy -p "/usage" --output-format json`（不耗額度） | agy 一個 Windows 使用者只能登入一個帳號；切換後舊帳號以快取顯示 |

查詢失敗時顯示最後一次成功的快取與「最後查到 hh:mm」。

Claude 第一次使用：在 Claude 卡片按「一鍵設定」，會在 Claude Code 的使用者設定 `settings.json` 加一行 `statusLine`（改之前先備份成 `settings.json.aitoken-bak-…`；你原本已有狀態列的話會接在它後面，原本的顯示照舊）。之後在 Claude Code 問任何一句，卡片就會出現數字。設定頁可以關掉終端機裡的額度顯示（仍會存檔給本程式）。

## 安全

- 不保存任何帳號密碼、不要求 API 金鑰；設定檔與快取只存名稱、資料夾位置、百分比與時間
- 對外連線只有：`platform.claude.com`（新模型偵測讀公開的模型清單頁，可在設定頁關閉）、GitHub（檢查／下載更新，只在啟動後與每天一次、或按下「更新」時）、Google Apps Script（`script.google.com`、`script.googleusercontent.com`，只在你按「直接送出」回報、或查開發者回覆時）；網域寫死白名單，轉址每一跳都檢查
- 更新只在你按下「下載並安裝」才下載；安裝檔必須與 Release 的 `SHA256SUMS.txt` 相符才會執行，不符就取消
- 紀錄檔：`%LOCALAPPDATA%\tools.aixai.token-usage\logs\app.log`（超過 512 KB 輪替成 `app.1.log`）。只記啟動、各帳號查詢結果、更新步驟與錯誤；不含 token、Authorization 或憑證內容，email 只記遮罩版，使用者資料夾路徑記成 `%USERPROFILE%`
- 回報（隱私）：只有你按下「直接送出」才會送；送出前可預覽完整內容，診斷附件（版本與系統、各平台查詢狀態、面板截圖、紀錄檔）每項都能取消勾選，狀態與紀錄已遮蔽、不含 token。回報會寄給開發者，並保存在開發者的 Google 帳號（不公開）。本機只在 `%LOCALAPPDATA%\tools.aixai.token-usage\reports.json` 記下回報編號、標題、日期與收到的回覆。面板截圖只擷取本程式自己的視窗，不會擷取你的螢幕
- 查回覆：只有勾了「接收開發者的回覆」的回報才會查，啟動後 2 分鐘與每 6 小時一次，只送回報編號（隨機碼）、最多查 90 天；沒有新回覆時不會有任何提示，有新回覆只亮小紅點
- Claude：本程式不讀 `.credentials.json`、不碰任何 token、不連 Anthropic 的 API；額度只讀 Claude Code 狀態列腳本寫在 `%APPDATA%\tools.aixai.token-usage\claude-status\` 的百分比與重置時間
- Codex 的登入與連線完全交給 Codex 自己，本程式不讀 `auth.json` 內容（只檢查存在）
- Codex 只呼叫唯讀方法（白名單），不會登出、不會消耗任何額度
- 查詢不呼叫 AI 模型，不耗 token；沒有遙測

English summary: no passwords or API keys are stored; outbound hosts are `platform.claude.com` (new-model check reads the public model list page; can be turned off), GitHub (update check/download, only after startup, once a day, or when you press Update) and Google Apps Script (`script.google.com`, only when you press Send on a report or when looking up replies); hard-coded allowlist, every redirect hop is checked; a report is sent only when you press Send, you can preview it and untick each diagnostic attachment (version/system, query status, panel screenshot of this app's own window only, masked log); reports are emailed to the developer and kept in the developer's Google account (not public); locally `reports.json` stores report IDs, titles, dates and replies; replies are looked up only for reports where you ticked "Receive the developer's reply", at startup+2 min and every 6 hours, sending only the report ID, for up to 90 days; updates are installed only if the installer matches the release's `SHA256SUMS.txt`; a local log (`%LOCALAPPDATA%\tools.aixai.token-usage\logs\app.log`, rotated at 512 KB) never contains tokens, and emails/paths are masked; Claude: never reads `.credentials.json` or any token and never calls Anthropic's API; quota comes from the official Claude Code status line (`rate_limits`), which the one-click setup adds to your user `settings.json` after backing it up; Codex auth is handled entirely by Codex itself; only read-only Codex methods are called; queries never call an AI model or spend tokens; no telemetry.

## 下載與安裝 / Download

1. 到 [Releases](../../releases) 下載最新的 `*-setup.exe` 與 `SHA256SUMS.txt`。
   Download the latest `*-setup.exe` and `SHA256SUMS.txt` from Releases.
2. 在 PowerShell 核對檢查碼，輸出的雜湊值要與 `SHA256SUMS.txt` 內該檔那一行相同：
   Verify the checksum in PowerShell; the hash must match the line for that file in `SHA256SUMS.txt`:

```powershell
Get-FileHash .\<安裝檔名稱>.exe -Algorithm SHA256
```

3. 本程式未做程式碼簽章，第一次執行 Windows SmartScreen 會警告：點「其他資訊」→「仍要執行」。
   The installer is not code-signed, so SmartScreen will warn on first run: click "More info" -> "Run anyway".

## 開發 / Build from source

需求 / Requirements：Node.js、Rust（rustup）、Visual Studio Build Tools（C++ 桌面開發）、WebView2（Windows 11 內建）

```powershell
npm ci
npm run tauri dev      # 開發模式 / dev mode
npm run tauri build    # 打包，產生 NSIS 安裝檔 / build the NSIS installer
cd src-tauri; cargo test
```

- 可選：若想把 Rust 編譯暫存放到別處（例如專案位於雲端同步資料夾），自行建立 `src-tauri/.cargo/config.toml`（已被 `.gitignore` 排除）：
  Optional: to put Rust build output elsewhere, create `src-tauri/.cargo/config.toml` (git-ignored):

```toml
[build]
target-dir = "C:/path/to/build/target"
```

- 測試用：環境變數 `AITOKEN_SHOW_ON_START=1` 會在啟動後直接打開面板
- 指定 Codex 位置：環境變數 `AITOKEN_CODEX_PATH`
- 指定 Antigravity 位置：環境變數 `AITOKEN_AGY_PATH`（設了就只用它）
- 測試用：`AITOKEN_DATA_DIR`（設定與快取改放別處）、`AITOKEN_OFFLINE=1`（不查 Antigravity，只用快取）、`AITOKEN_CLAUDE_HOME`（預設 Claude 資料夾）、`AITOKEN_START_VIEW=settings` 或 `update`（直接進設定頁／更新頁）、`AITOKEN_UPDATE_API=file:///C:/path/release.json`（用本機檔當 Release JSON，不連網）、`AITOKEN_UPDATE_FIRST_CHECK_SECS`（縮短啟動後首次檢查等待秒數，預設 60）

## 授權 / License

MIT，詳見 [LICENSE](LICENSE)。 MIT, see [LICENSE](LICENSE).
