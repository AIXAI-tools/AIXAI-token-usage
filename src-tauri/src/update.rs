// 線上更新：查 GitHub Releases、下載安裝檔與 SHA256SUMS.txt、核對、交給 NSIS 安裝檔。
// 安全：
//  - 對外連線只允許 UPDATE_HOSTS（https、精確網域）；轉址手動處理，每一跳都檢查、最多 5 跳
//  - 安裝檔必須出現在 SHA256SUMS.txt 且雜湊相符；缺檔或不符 → 刪暫存、不安裝
//  - 下載檔名只取資產名稱，拒絕含 \ / .. 的名稱，只接受 .exe
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

pub const RELEASES_PAGE: &str = "https://github.com/AIXAI-tools/AIXAI-token-usage/releases";
const API_LATEST: &str = "https://api.github.com/repos/AIXAI-tools/AIXAI-token-usage/releases/latest";
pub const SUMS_NAME: &str = "SHA256SUMS.txt";
/// 更新只允許連這些網域
pub const UPDATE_HOSTS: &[&str] = &["api.github.com", "github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"];
const MAX_REDIRECTS: usize = 5;
const NOTES_MAX_CHARS: usize = 2000;

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 只允許 https + 白名單網域（精確比對，不接受 userinfo／連接埠）
pub fn url_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    UPDATE_HOSTS.contains(&host)
}

// ---------- 版本比較 ----------

/// "v1.2.3-beta.1" → ([1,2,3], Some("beta.1"))；格式不對回 None
pub fn parse_version(v: &str) -> Option<([u64; 3], Option<String>)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let (core, pre) = match v.split_once(['-', '+']) {
        Some((c, p)) => (c, Some(p.to_string())),
        None => (v, None),
    };
    let mut nums = [0u64; 3];
    let mut n = 0;
    for part in core.split('.') {
        if n >= 3 {
            return None;
        }
        nums[n] = part.parse().ok()?;
        n += 1;
    }
    (n >= 1).then_some((nums, pre))
}

/// latest 是否比 current 新（有 pre-release 的比同版本正式版小；無法解析視為不新）
pub fn is_newer(latest: &str, current: &str) -> bool {
    let (Some((a, ap)), Some((b, bp))) = (parse_version(latest), parse_version(current)) else { return false };
    if a != b {
        return a > b;
    }
    match (ap, bp) {
        (None, Some(_)) => true,
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

// ---------- Release JSON ----------

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub tag: String,
    pub body: String,
    pub setup: Option<Asset>,
    pub sums: Option<Asset>,
}

/// 下載檔名只准是單純的 .exe 檔名
pub fn safe_exe_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['\\', '/', ':']) && !name.contains("..") && name.to_lowercase().ends_with(".exe")
}

pub fn parse_release(v: &serde_json::Value) -> Option<Release> {
    let tag = v.get("tag_name")?.as_str()?.to_string();
    parse_version(&tag)?;
    let body: String = v.get("body").and_then(|b| b.as_str()).unwrap_or("").chars().take(NOTES_MAX_CHARS).collect();
    let mut setup = None;
    let mut sums = None;
    if let Some(list) = v.get("assets").and_then(|a| a.as_array()) {
        for a in list {
            let (Some(name), Some(url)) = (a.get("name").and_then(|x| x.as_str()), a.get("browser_download_url").and_then(|x| x.as_str())) else { continue };
            let asset = Asset { name: name.to_string(), url: url.to_string(), size: a.get("size").and_then(|x| x.as_u64()).unwrap_or(0) };
            if name == SUMS_NAME {
                sums = Some(asset);
            } else if setup.is_none() && safe_exe_name(name) && name.contains("setup") {
                setup = Some(asset);
            }
        }
    }
    Some(Release { tag, body, setup, sums })
}

// ---------- SHA256SUMS ----------

/// 解析 `<hash>  <檔名>`（CRLF／單空白／`*` 二進位標記都容忍），找出指定檔名的雜湊（小寫）
pub fn parse_sums(text: &str, file: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        let Some((hash, name)) = line.split_once(char::is_whitespace) else { continue };
        let name = name.trim().trim_start_matches('*');
        if name == file && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hash.to_lowercase());
        }
    }
    None
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// 核對下載檔。失敗（缺校驗項目、不符、讀不到）一律刪掉暫存檔並回傳原因。
pub fn verify_download(file: &Path, sums_text: &str, name: &str) -> Result<(), String> {
    let result = (|| {
        let want = parse_sums(sums_text, name).ok_or_else(|| "sums_missing_entry".to_string())?;
        let got = sha256_file(file).map_err(|_| "read_failed".to_string())?;
        if got == want { Ok(()) } else { Err("sha_mismatch".to_string()) }
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(file); // 程式自己下載的暫存，可刪
    }
    result
}

// ---------- 網路（手動處理轉址） ----------

fn agent(total: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout(total).redirects(0).build()
}

/// GET，手動跟轉址：每一跳都檢查白名單、最多 5 跳
fn get_follow(url: &str, total: Duration) -> Result<ureq::Response, String> {
    let ag = agent(total);
    let mut cur = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        if !url_allowed(&cur) {
            return Err("blocked_host".into());
        }
        let res = ag
            .get(&cur)
            .set("User-Agent", concat!("aixai-token-usage/", env!("CARGO_PKG_VERSION")))
            .set("Accept", "application/vnd.github+json, application/octet-stream, */*")
            .call();
        match res {
            Ok(r) if (300..400).contains(&r.status()) => {
                cur = r.header("Location").ok_or_else(|| "bad_redirect".to_string())?.to_string();
            }
            Ok(r) => return Ok(r),
            Err(ureq::Error::Status(c, _)) if (300..400).contains(&c) => return Err("bad_redirect".into()),
            Err(ureq::Error::Status(c, _)) => return Err(format!("http_{c}")),
            Err(_) => return Err("network".into()),
        }
    }
    Err("too_many_redirects".into())
}

fn read_limited(r: ureq::Response, max: u64) -> Result<String, String> {
    let mut s = String::new();
    r.into_reader().take(max).read_to_string(&mut s).map_err(|_| "network".to_string())?;
    Ok(s)
}

/// 查最新 Release。測試：AITOKEN_UPDATE_API=file:///C:/path/release.json 讀本機檔（不連網）；
/// 其他值必須通過白名單，否則忽略。
pub fn fetch_latest() -> Result<Release, String> {
    let body = match std::env::var("AITOKEN_UPDATE_API") {
        Ok(v) if v.starts_with("file:///") => std::fs::read_to_string(&v["file:///".len()..]).map_err(|_| "read_failed".to_string())?,
        Ok(v) if url_allowed(&v) => read_limited(get_follow(&v, Duration::from_secs(15))?, 2_000_000)?,
        _ => read_limited(get_follow(API_LATEST, Duration::from_secs(15))?, 2_000_000)?,
    };
    let v: serde_json::Value = serde_json::from_str(body.trim_start_matches('\u{feff}')).map_err(|_| "bad_json".to_string())?;
    parse_release(&v).ok_or_else(|| "bad_release".to_string())
}

pub fn fetch_text(url: &str, max: u64) -> Result<String, String> {
    read_limited(get_follow(url, Duration::from_secs(30))?, max)
}

/// 下載到檔案；progress(已下載, 總大小)
pub fn download_to(url: &str, dest: &Path, expected_size: u64, mut progress: impl FnMut(u64, u64)) -> Result<(), String> {
    let r = get_follow(url, Duration::from_secs(900))?;
    let total = r.header("Content-Length").and_then(|s| s.parse::<u64>().ok()).filter(|n| *n > 0).unwrap_or(expected_size);
    let mut reader = r.into_reader().take(300 * 1024 * 1024);
    let mut f = std::fs::File::create(dest).map_err(|_| "write_failed".to_string())?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|_| "network".to_string())?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n]).map_err(|_| "write_failed".to_string())?;
        done += n as u64;
        progress(done, total);
    }
    f.flush().map_err(|_| "write_failed".to_string())?;
    if done == 0 {
        return Err("empty_download".into());
    }
    Ok(())
}

// ---------- 給面板的狀態 ----------

#[derive(Serialize, Clone, Debug, Default)]
pub struct UpdateState {
    pub current: String,
    pub latest: Option<String>,
    pub has_update: bool,
    pub notes: String,
    pub last_check: Option<i64>,
    pub checking: bool,
    /// 上次檢查失敗的原因 key
    pub check_error: Option<String>,
    /// idle | download | verify | install | error
    pub stage: String,
    pub percent: u8,
    /// 安裝流程失敗的原因 key
    pub fail: Option<String>,
}

#[derive(Default)]
pub struct Shared {
    pub state: UpdateState,
    pub release: Option<Release>,
    pub installing: bool,
}

impl Shared {
    pub fn new() -> Self {
        Self { state: UpdateState { current: current_version().into(), stage: "idle".into(), ..Default::default() }, ..Default::default() }
    }

    pub fn apply_release(&mut self, rel: Release, now: i64) {
        let has = is_newer(&rel.tag, current_version());
        self.state.latest = Some(rel.tag.trim_start_matches(['v', 'V']).to_string());
        self.state.has_update = has;
        self.state.notes = rel.body.clone();
        self.state.last_check = Some(now);
        self.state.check_error = None;
        self.release = Some(rel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_allowlist() {
        assert!(url_allowed("https://api.github.com/repos/x/y/releases/latest"));
        assert!(url_allowed("https://github.com/AIXAI-tools/AIXAI-token-usage/releases/download/v1/a.exe"));
        assert!(url_allowed("https://objects.githubusercontent.com/abc?x=1"));
        assert!(url_allowed("https://release-assets.githubusercontent.com/abc"));
        assert!(!url_allowed("https://github.com.evil.com/x"));
        assert!(!url_allowed("https://evil.com/github.com"));
        assert!(!url_allowed("https://evil.com/?u=https://github.com/"));
        assert!(!url_allowed("http://github.com/x"));
        assert!(!url_allowed("https://github.com@evil.com/x"));
        assert!(!url_allowed("https://github.com:8443/x"));
        assert!(!url_allowed("https://evilgithub.com/x"));
        assert!(!url_allowed("https://raw.githubusercontent.com/x"));
        assert!(!url_allowed("file:///C:/x"));
    }

    #[test]
    fn semver_compare() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        // pre-release 比同版正式版小
        assert!(!is_newer("1.0.0-beta.1", "1.0.0"));
        assert!(is_newer("1.0.0", "1.0.0-rc.1"));
        assert!(is_newer("1.0.1-beta", "1.0.0"));
        assert!(is_newer("1.0.0-beta.2", "1.0.0-beta.1"));
        // 缺段視為 0；無法解析不算新
        assert!(is_newer("1.1", "1.0.9"));
        assert!(!is_newer("garbage", "0.1.0"));
        assert!(!is_newer("1.2.3.4", "0.1.0"));
    }

    #[test]
    fn sums_parse_tolerates_crlf() {
        let h1 = "a".repeat(64);
        let h2 = "B".repeat(64);
        let text = format!("{h1}  AIXAI-Token-Usage_0.2.0_x64-setup.exe\r\n{h2} *other.exe\r\n\r\n");
        assert_eq!(parse_sums(&text, "AIXAI-Token-Usage_0.2.0_x64-setup.exe").as_deref(), Some(h1.as_str()));
        assert_eq!(parse_sums(&text, "other.exe").as_deref(), Some("b".repeat(64).as_str()));
        assert_eq!(parse_sums(&text, "missing.exe"), None);
        assert_eq!(parse_sums("short  a.exe", "a.exe"), None);
    }

    #[test]
    fn asset_names() {
        assert!(safe_exe_name("AIXAI-Token-Usage_0.2.0_x64-setup.exe"));
        assert!(!safe_exe_name("..\\evil.exe"));
        assert!(!safe_exe_name("a/b.exe"));
        assert!(!safe_exe_name("a..exe.exe"));
        assert!(!safe_exe_name("setup.msi"));
        assert!(!safe_exe_name("C:evil.exe"));
    }

    #[test]
    fn parses_release_json() {
        let v = json!({"tag_name":"v0.2.0","body":"notes","assets":[
            {"name":"SHA256SUMS.txt","browser_download_url":"https://github.com/x/SHA256SUMS.txt","size":100},
            {"name":"../evil.exe","browser_download_url":"https://github.com/x/evil.exe","size":1},
            {"name":"AIXAI-Token-Usage_0.2.0_x64-setup.exe","browser_download_url":"https://github.com/x/s.exe","size":5}]});
        let r = parse_release(&v).unwrap();
        assert_eq!(r.tag, "v0.2.0");
        assert_eq!(r.setup.unwrap().name, "AIXAI-Token-Usage_0.2.0_x64-setup.exe");
        assert!(r.sums.is_some());
        let long = json!({"tag_name":"v0.2.0","body":"字".repeat(5000)});
        assert_eq!(parse_release(&long).unwrap().body.chars().count(), 2000);
        assert!(parse_release(&json!({"tag_name":"nonsense"})).is_none());
    }

    #[test]
    fn sha_mismatch_deletes_file_and_missing_entry_too() {
        let dir = std::env::temp_dir().join(format!("aitoken-upd-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("setup.exe");
        std::fs::write(&f, b"hello").unwrap();
        // "hello" 的 SHA256
        let good = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert_eq!(sha256_file(&f).unwrap(), good);
        // 正確 → 通過、檔案保留
        assert!(verify_download(&f, &format!("{good}  setup.exe\r\n"), "setup.exe").is_ok());
        assert!(f.exists());
        // 不符 → 拒絕並刪檔
        let bad = "0".repeat(64);
        assert_eq!(verify_download(&f, &format!("{bad}  setup.exe\r\n"), "setup.exe").unwrap_err(), "sha_mismatch");
        assert!(!f.exists());
        // 缺校驗項目 → 拒絕並刪檔
        std::fs::write(&f, b"hello").unwrap();
        assert_eq!(verify_download(&f, &format!("{good}  other.exe\r\n"), "setup.exe").unwrap_err(), "sums_missing_entry");
        assert!(!f.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
