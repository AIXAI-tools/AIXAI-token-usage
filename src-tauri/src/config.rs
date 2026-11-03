// 設定檔 config.json：只存語言、通知、開機啟動旗標、帳號清單（平台／資料夾／名稱／是否顯示）。
// 不含任何機密（密碼、token 都不會寫進來）。
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PLATFORMS: [&str; 3] = ["codex", "claude", "antigravity"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AccountCfg {
    /// 帳號唯一鍵：codex:default／claude:default／<平台>:<資料夾>／antigravity:<email>
    pub id: String,
    pub provider: String,
    /// 設定資料夾；None＝使用預設位置（~/.claude、~/.codex）；agy 存 email
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default = "yes")]
    pub visible: bool,
    /// 使用者新增（偵測到）的額外帳號
    #[serde(default)]
    pub extra: bool,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Config {
    /// system | zh-TW | en
    #[serde(default = "default_lang")]
    pub language: String,
    /// 第一次執行是否已處理過「預設開啟開機啟動」
    #[serde(default)]
    pub autostart_initialized: bool,
    #[serde(default = "default_threshold")]
    pub notify_threshold: u8,
    #[serde(default = "yes")]
    pub notify_on_reset: bool,
    #[serde(default)]
    pub accounts: Vec<AccountCfg>,
    /// 主畫面隱藏的離線帳號（offline_id）
    #[serde(default)]
    pub offline_hidden: Vec<String>,
}

fn default_lang() -> String {
    "system".into()
}
fn default_threshold() -> u8 {
    10
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: default_lang(),
            autostart_initialized: false,
            notify_threshold: 10,
            notify_on_reset: true,
            accounts: vec![],
            offline_hidden: vec![],
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Config {
        let mut c: Config = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
            .unwrap_or_default();
        c.sanitize();
        c
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    fn sanitize(&mut self) {
        if !["system", "zh-TW", "en"].contains(&self.language.as_str()) {
            self.language = default_lang();
        }
        if ![5, 10, 20, 30].contains(&self.notify_threshold) {
            self.notify_threshold = 10;
        }
        self.accounts.retain(|a| PLATFORMS.contains(&a.provider.as_str()));
    }

    /// 確保每個平台都有預設帳號（Codex／Claude 的預設資料夾；agy 另由偵測 email 決定）
    pub fn ensure_defaults(&mut self) {
        for p in ["codex", "claude"] {
            let id = format!("{p}:default");
            if !self.accounts.iter().any(|a| a.id == id) {
                self.accounts.push(AccountCfg { id, provider: p.into(), dir: None, name: String::new(), visible: true, extra: false });
            }
        }
    }

    /// 依平台順序排好（同平台維持原順序）
    pub fn sort_accounts(&mut self) {
        self.accounts.sort_by_key(|a| PLATFORMS.iter().position(|p| *p == a.provider).unwrap_or(9));
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut AccountCfg> {
        self.accounts.iter_mut().find(|a| a.id == id)
    }

    /// 加入額外帳號（資料夾已存在清單中就略過）。回傳是否新增。
    pub fn add_extra(&mut self, provider: &str, dir: &str) -> bool {
        let id = format!("{provider}:{dir}");
        if self.accounts.iter().any(|a| a.id == id) {
            return false;
        }
        self.accounts.push(AccountCfg { id, provider: provider.into(), dir: Some(dir.into()), name: String::new(), visible: true, extra: true });
        true
    }
}

/// 設定／快取資料夾。測試時可用 AITOKEN_DATA_DIR 指到別處，不碰真正的設定。
pub fn data_dir(fallback: Option<PathBuf>) -> Option<PathBuf> {
    std::env::var_os("AITOKEN_DATA_DIR").map(PathBuf::from).or(fallback)
}

/// 本機資料根目錄（紀錄檔、更新暫存、agy 專用家目錄）：%LOCALAPPDATA%\tools.aixai.token-usage
/// 測試時跟著 AITOKEN_DATA_DIR。
pub fn local_dir() -> Option<PathBuf> {
    std::env::var_os("AITOKEN_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("tools.aixai.token-usage")))
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

/// 掃描家目錄下的 .claude-N／.codex-N，回傳 (平台, 完整路徑)。
/// 只檢查登入檔「是否存在」，不讀內容。
pub fn scan_extra_dirs(home: &Path) -> Vec<(&'static str, String)> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(home) else { return out };
    let mut names: Vec<String> = rd.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
    names.sort();
    for n in names {
        for (prefix, provider, marker) in [(".claude-", "claude", ".credentials.json"), (".codex-", "codex", "auth.json")] {
            let Some(rest) = n.strip_prefix(prefix) else { continue };
            if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let dir = home.join(&n);
            if dir.join(marker).is_file() {
                out.push((provider, dir.to_string_lossy().into_owned()));
            }
        }
    }
    out
}

/// 下一個空的資料夾編號：.claude-2、.claude-3…（資料夾已存在或已在清單中就跳過）
pub fn next_free_dir(home: &Path, prefix: &str, cfg: &Config) -> String {
    let mut n = 2;
    loop {
        let name = format!("{prefix}{n}");
        let full = home.join(&name);
        let taken = full.exists() || cfg.accounts.iter().any(|a| a.dir.as_deref().is_some_and(|d| Path::new(d) == full));
        if !taken {
            return name;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let d = std::env::temp_dir().join(format!("aitoken-test-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn roundtrip_and_defaults() {
        let d = tmp("cfg");
        let p = d.join("sub").join("config.json");
        let mut c = Config::default();
        c.ensure_defaults();
        c.language = "en".into();
        c.notify_threshold = 20;
        assert!(c.add_extra("claude", "C:\\Users\\x\\.claude-2"));
        assert!(!c.add_extra("claude", "C:\\Users\\x\\.claude-2"));
        c.save(&p).unwrap();
        let back = Config::load(&p);
        assert_eq!(back, c);
        assert_eq!(back.accounts.len(), 3);
        // 檔案內不含任何 token 字樣
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.to_lowercase().contains("token"));
    }

    #[test]
    fn bad_values_are_sanitized() {
        let d = tmp("bad");
        let p = d.join("config.json");
        std::fs::write(&p, r#"{"language":"xx","notify_threshold":7,"accounts":[{"id":"a","provider":"chatgpt"}]}"#).unwrap();
        let c = Config::load(&p);
        assert_eq!(c.language, "system");
        assert_eq!(c.notify_threshold, 10);
        assert!(c.accounts.is_empty());
        assert_eq!(Config::load(&d.join("none.json")), Config::default());
    }

    #[test]
    fn config_with_bom_is_read() {
        let d = tmp("bom");
        let p = d.join("config.json");
        std::fs::write(&p, "\u{feff}{\"language\":\"en\"}").unwrap();
        assert_eq!(Config::load(&p).language, "en");
    }

    #[test]
    fn scan_and_next_dir() {
        let h = tmp("home");
        std::fs::create_dir_all(h.join(".claude-2")).unwrap();
        std::fs::write(h.join(".claude-2").join(".credentials.json"), "{}").unwrap();
        std::fs::create_dir_all(h.join(".claude-3")).unwrap(); // 沒登入檔，不算
        std::fs::create_dir_all(h.join(".codex-2")).unwrap();
        std::fs::write(h.join(".codex-2").join("auth.json"), "{}").unwrap();
        std::fs::create_dir_all(h.join(".claude-x")).unwrap();
        let found = scan_extra_dirs(&h);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "claude");
        assert_eq!(found[1].0, "codex");
        let cfg = Config::default();
        assert_eq!(next_free_dir(&h, ".claude-", &cfg), ".claude-4");
        assert_eq!(next_free_dir(&h, ".codex-", &cfg), ".codex-3");
        assert_eq!(next_free_dir(&h, ".gemini-", &cfg), ".gemini-2");
    }
}
