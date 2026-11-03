# AIXAI Token Usage

Windows 系統匣小工具：即時查看 Codex／Claude／Antigravity 的訂閱額度剩多少、什麼時候重置。
A Windows tray app that shows how much of your Codex / Claude / Antigravity plan limits are left.

- 右下角三段圓環：綠＝Codex、橘＝Claude、紫＝Antigravity，亮起的長度＝該平台最緊額度的剩餘 %
- 點圖示打開面板；面板頭部的齒輪進入設定頁（帳號名稱／顯示、新增帳號、通知）
- 右鍵選單：立即重新整理、語言（跟隨系統／繁體中文／English）、開機自動啟動、結束
- 每 5 分鐘自動更新；介面預設跟隨系統語言；第一次執行預設開機自動啟動
- 低於門檻（預設 10%）與額度重置時發系統通知

## 目前支援

| 平台 | 資料來源 | 多帳號 |
|---|---|---|
| Codex | 官方 `codex app-server` 的 `account/rateLimits/read`；失敗時改讀本機紀錄 | 用 `CODEX_HOME` 分資料夾（`~/.codex-2`…） |
| Claude（含 Claude Code） | Claude Code `/usage` 背後的 `api.anthropic.com/api/oauth/usage` | 用 `CLAUDE_CONFIG_DIR` 分資料夾（`~/.claude-2`…） |
| Antigravity | 官方 `agy -p "/usage" --output-format json`（不耗額度） | agy 一個 Windows 使用者只能登入一個帳號；切換後舊帳號以快取顯示 |

查詢失敗時顯示最後一次成功的快取與「最後查到 hh:mm」。

## 安全

- 不保存任何帳號密碼、不要求 API 金鑰；設定檔與快取只存名稱、資料夾位置、百分比與時間
- 對外連線只有 `api.anthropic.com`（寫死的白名單）
- Claude 的 token 只在查詢當下讀進記憶體，只送往白名單網域；絕不換新 token、絕不寫入任何憑證檔
- Codex 的登入與連線完全交給 Codex 自己，本程式不讀 `auth.json` 內容（只檢查存在）
- Codex 只呼叫唯讀方法（白名單），不會登出、不會消耗任何額度
- 查詢不呼叫 AI 模型，不耗 token；沒有遙測

English summary: no passwords or API keys are stored; the only outbound host is `api.anthropic.com` (hard-coded allowlist); Claude tokens are read into memory only at query time and never refreshed or written to disk; Codex auth is handled entirely by Codex itself; only read-only Codex methods are called; queries never call an AI model or spend tokens; no telemetry.

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
- 測試用：`AITOKEN_DATA_DIR`（設定與快取改放別處）、`AITOKEN_OFFLINE=1`（不查 Claude／Antigravity，只用快取）、`AITOKEN_CLAUDE_HOME`（預設 Claude 資料夾）、`AITOKEN_START_VIEW=settings`（直接進設定頁）

## 授權 / License

MIT，詳見 [LICENSE](LICENSE)。 MIT, see [LICENSE](LICENSE).
