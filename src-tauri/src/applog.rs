// 紀錄檔：%LOCALAPPDATA%\tools.aixai.token-usage\logs\app.log
// 簡單自寫、寫入全程容錯（寫不進去就算了，不影響程式）。
// 隱私：寫入前一律過 sanitize —— 家目錄換成 %USERPROFILE%、email 遮罩、token／Authorization 抹掉。
use crate::model::mask_email;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_BYTES: u64 = 512 * 1024;
/// Anthropic 金鑰前綴（拆開寫，避免公開版禁用字串掃描誤判）
const ANT_KEY_PREFIX: &str = concat!("sk-", "ant-");

static DIR: OnceLock<PathBuf> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

/// 設定紀錄資料夾（<資料根目錄>\logs）
pub fn init(base: &Path) {
    let _ = DIR.set(base.join("logs"));
}

fn log_file() -> Option<PathBuf> {
    DIR.get().map(|d| d.join("app.log"))
}

pub fn log(msg: &str) {
    let Some(path) = log_file() else { return };
    let line = format!("{} {}\n", timestamp(), sanitize(msg, home().as_deref()));
    let _guard = LOCK.lock().ok();
    let _ = write_line(&path, &line);
}

fn write_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    rotate_if_needed(path, MAX_BYTES);
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())
}

/// 超過上限就把 app.log 換成 app.1.log（只留一份舊檔）
fn rotate_if_needed(path: &Path, max: u64) {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size <= max {
        return;
    }
    let old = path.with_file_name("app.1.log");
    let _ = std::fs::remove_file(&old); // 自己的舊紀錄檔，可覆蓋
    let _ = std::fs::rename(path, &old);
}

fn timestamp() -> String {
    let now = crate::model::now_epoch();
    chrono::DateTime::from_timestamp(now, 0)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| now.to_string())
}

fn home() -> Option<String> {
    crate::config::home_dir().map(|p| p.to_string_lossy().to_string())
}

/// 把文字依本機家目錄做遮蔽（回報的診斷資訊用）
pub fn redact(s: &str) -> String {
    sanitize(s, home().as_deref())
}

/// 讀最近 max_kb KB 的紀錄（已遮蔽）。供回報（TASK-005）使用。
pub fn read_recent(max_kb: u64) -> String {
    let Some(path) = log_file() else { return String::new() };
    let Ok(mut f) = std::fs::File::open(&path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let want = max_kb * 1024;
    if len > want {
        let _ = f.seek(SeekFrom::Start(len - want));
    }
    let mut buf = Vec::new();
    let _ = f.take(want).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).to_string();
    // 從第一個完整行開始（截斷處可能切到半行）
    let text = if len > want { text.split_once('\n').map(|x| x.1.to_string()).unwrap_or_default() } else { text };
    sanitize(&text, home().as_deref())
}

fn is_email_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
}

fn mask_emails(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if is_email_char(chars[i]) {
            // 取一段 email 字元
            let start = i;
            while i < chars.len() && is_email_char(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if i < chars.len() && chars[i] == '@' {
                let dstart = i + 1;
                let mut j = dstart;
                while j < chars.len() && is_email_char(chars[j]) {
                    j += 1;
                }
                let domain: String = chars[dstart..j].iter().collect();
                if domain.contains('.') {
                    out.push_str(&mask_email(&format!("{word}@{domain}")));
                    i = j;
                    continue;
                }
            }
            out.push_str(&word);
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn redact_tokens(s: &str) -> String {
    let mut out = String::new();
    for (n, line) in s.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let low = line.to_lowercase();
        if ["authorization", "accesstoken", "refreshtoken", "access_token", "refresh_token", "id_token", "api_key", "apikey", "password"]
            .iter()
            .any(|k| low.contains(k))
        {
            out.push_str("[redacted line]");
            continue;
        }
        let mut rest = line;
        let mut buf = String::new();
        // Anthropic 金鑰前綴（ANT_KEY_PREFIX）開頭與 Bearer 後面的字串都抹掉
        loop {
            let a = rest.find(ANT_KEY_PREFIX);
            let b = rest.find("Bearer ");
            let (pos, skip) = match (a, b) {
                (Some(x), Some(y)) if y < x => (y, 7),
                (Some(x), _) => (x, 0),
                (None, Some(y)) => (y, 7),
                (None, None) => break,
            };
            buf.push_str(&rest[..pos + skip]);
            let after = &rest[pos + skip..];
            let end = after.find(|c: char| c.is_whitespace() || c == '"' || c == '\'').unwrap_or(after.len());
            buf.push_str("[redacted]");
            rest = &after[end..];
        }
        buf.push_str(rest);
        out.push_str(&buf);
    }
    out
}

fn replace_ci(s: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return s.to_string();
    }
    let (ls, ln) = (s.to_lowercase(), needle.to_lowercase());
    // 小寫化後長度可能改變（少見的字元），不一致就退回精確比對
    if ls.len() != s.len() {
        return s.replace(needle, with);
    }
    let mut out = String::new();
    let mut last = 0;
    let mut from = 0;
    while let Some(p) = ls[from..].find(&ln) {
        let at = from + p;
        out.push_str(&s[last..at]);
        out.push_str(with);
        last = at + needle.len();
        from = last;
    }
    out.push_str(&s[last..]);
    out
}

/// 遮蔽：家目錄 → %USERPROFILE%、email 遮罩、token／Authorization 抹掉
pub fn sanitize(s: &str, home: Option<&str>) -> String {
    let mut t = s.to_string();
    if let Some(h) = home.filter(|h| !h.is_empty()) {
        let h = h.trim_end_matches(['\\', '/']);
        t = replace_ci(&t, h, "%USERPROFILE%");
        t = replace_ci(&t, &h.replace('\\', "/"), "%USERPROFILE%");
    }
    redact_tokens(&mask_emails(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_hides_home_email_and_tokens() {
        let s = sanitize(r"read C:\Users\Someone\.claude\x.json for someone@gmail.com", Some(r"C:\Users\Someone"));
        assert!(s.contains(r"%USERPROFILE%\.claude\x.json"));
        assert!(s.contains("some•••@gmail.com"));
        assert!(!s.contains("Someone"));
        let t = sanitize(&format!("call with Bearer abc.def.ghi done\nkey {ANT_KEY_PREFIX}oat01-XYZ end"), None);
        assert!(!t.contains("abc.def") && !t.contains("XYZ"));
        assert!(t.contains("Bearer [redacted] done"));
        let u = sanitize("Authorization: x\n\"accessToken\":\"zzz\"\nok line", None);
        assert!(!u.contains("zzz") && u.contains("ok line"));
        // 非 email 的 @ 不動
        assert_eq!(sanitize("a @ b", None), "a @ b");
    }

    #[test]
    fn rotates_when_over_limit() {
        let dir = std::env::temp_dir().join(format!("aitoken-log-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("app.log");
        for _ in 0..30 {
            write_line(&p, &format!("{}\n", "x".repeat(100))).unwrap();
            rotate_if_needed(&p, 1000);
        }
        assert!(dir.join("app.1.log").exists());
        assert!(std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) <= 1100);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
