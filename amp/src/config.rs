//! Settings, read the same way the terminal UI reads them (spoterm/config.py): real
//! environment variables, then `.env` next to the exe, then the config folder's `.env`.
//! Only SPOTIPY_*, SPOTERM_* and the proxy variables are taken from `.env` files, and
//! never from the current folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const ENGINE_EXE: &str = "spoterm-engine.exe";

pub struct Settings {
    pub client_id: String,
    pub redirect_uri: String,
    pub token_path: PathBuf,
    pub engine: bool,
    pub device_name: String,
    pub bitrate: u32,
    pub engine_bin: String,
    vars: HashMap<String, String>,
}

impl Settings {
    /// Variables to hand to the engine: none of ours (its argv says what it needs).
    pub fn proxy_vars(&self) -> impl Iterator<Item = (&String, &String)> {
        self.vars.iter().filter(|(k, _)| !k.starts_with("SPOTIPY_") && !k.starts_with("SPOTERM_"))
    }
}

pub fn config_dir() -> PathBuf {
    // Not %APPDATA%: Microsoft Store Python redirects writes there, so the terminal UI
    // uses ~/.config/spoterm and both UIs must agree.
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("spoterm")
}

pub fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default()
}

/// The project folder, when running from a source checkout (amp/target/<profile>/).
fn project_dir() -> Option<PathBuf> {
    let dir = exe_dir();
    let root = dir.parent()?.parent()?.parent()?;
    root.join("engine").join("Cargo.toml").is_file().then(|| root.to_path_buf())
}

pub fn load() -> Settings {
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut folders = vec![exe_dir()];
    folders.extend(project_dir());
    folders.push(config_dir());
    for folder in folders {
        let Ok(text) = std::fs::read_to_string(folder.join(".env")) else { continue };
        for (k, v) in parse_env(text.trim_start_matches('\u{feff}')) {
            let ok = k.starts_with("SPOTIPY_") || k.starts_with("SPOTERM_")
                || matches!(k.as_str(), "HTTPS_PROXY" | "https_proxy" | "NO_PROXY" | "no_proxy");
            if ok {
                vars.entry(k).or_insert(v);
            }
        }
    }
    // Real environment variables win.
    for (k, v) in std::env::vars() {
        if k.starts_with("SPOTIPY_") || k.starts_with("SPOTERM_") {
            vars.insert(k, v);
        }
    }
    let get = |k: &str| vars.get(k).map(|v| v.trim().to_string()).unwrap_or_default();
    let flag = |k: &str, default: bool| {
        let v = get(k).to_ascii_lowercase();
        if v.is_empty() { default } else { matches!(v.as_str(), "1" | "true" | "yes" | "on") }
    };
    let token = get("SPOTERM_TOKEN_PATH");
    let bitrate = get("SPOTERM_BITRATE");
    let redirect = get("SPOTIPY_REDIRECT_URI");
    let name = get("SPOTERM_DEVICE_NAME");
    Settings {
        client_id: get("SPOTIPY_CLIENT_ID"),
        redirect_uri: if redirect.is_empty() { "http://127.0.0.1:8888/callback".into() } else { redirect },
        token_path: if token.is_empty() { config_dir().join("token.json") } else { PathBuf::from(token) },
        engine: flag("SPOTERM_ENGINE", true),
        device_name: if name.is_empty() { "SpoTerm".into() } else { name },
        bitrate: match bitrate.as_str() { "96" => 96, "320" => 320, _ => 160 },
        engine_bin: get("SPOTERM_ENGINE_BIN"),
        vars,
    }
}

/// KEY=VALUE lines, optional `export `, # comments, '...' and "..." quoting. Nothing is
/// expanded; malformed lines are skipped.
fn parse_env(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim_start();
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim();
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || k.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let v = v.trim();
        let value = if let Some(q) = v.chars().next().filter(|c| *c == '"' || *c == '\'') {
            let mut buf = String::new();
            let mut chars = v[1..].chars();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if q == '"' && c == '\\' {
                    match chars.next() {
                        Some('n') => buf.push('\n'),
                        Some('t') => buf.push('\t'),
                        Some('r') => buf.push('\r'),
                        Some(o @ ('"' | '\\')) => buf.push(o),
                        Some(o) => { buf.push('\\'); buf.push(o); }
                        None => break,
                    }
                } else if c == q {
                    closed = true;
                    break;
                } else {
                    buf.push(c);
                }
            }
            if !closed {
                continue;
            }
            buf
        } else {
            let mut end = v.len();
            let b = v.as_bytes();
            for i in 1..b.len() {
                if b[i] == b'#' && (b[i - 1] == b' ' || b[i - 1] == b'\t') {
                    end = i;
                    break;
                }
            }
            v[..end].trim_end().to_string()
        };
        if !value.contains('\0') {
            out.push((k.to_string(), value));
        }
    }
    out
}

pub fn valid_client_id(cid: &str) -> bool {
    cid.len() == 32 && cid.chars().all(|c| c.is_ascii_hexdigit())
}

/// Append SPOTIPY_CLIENT_ID to the config folder's .env (first run).
pub fn save_client_id(cid: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    std::fs::create_dir_all(config_dir())?;
    let path = config_dir().join(".env");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "\nSPOTIPY_CLIENT_ID={cid}")?;
    Ok(path)
}

/// SPOTERM_ENGINE_BIN, then next to this exe, then the project's own engine build,
/// then <config>/bin, then PATH.
pub fn find_engine(explicit: &str) -> Option<PathBuf> {
    if !explicit.is_empty() {
        let p = PathBuf::from(explicit);
        return p.is_file().then_some(p);
    }
    let mut dirs = vec![exe_dir()];
    if let Some(root) = project_dir() {
        dirs.push(root.join("engine").join("target").join("release"));
    }
    dirs.push(config_dir().join("bin"));
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.into_iter().map(|d| d.join(ENGINE_EXE)).find(|p| p.is_file())
}

/// SpoTerm's own sign-in exists and covers every scope the library needs.
pub fn signed_in(token: &Path) -> bool {
    const SCOPES: &[&str] = &[
        "user-read-playback-state", "user-modify-playback-state", "user-read-currently-playing",
        "user-library-read", "user-library-modify", "playlist-read-private",
        "playlist-read-collaborative", "streaming",
    ];
    let Ok(text) = std::fs::read_to_string(token) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    let granted: Vec<&str> = v["scope"].as_str().unwrap_or("").split_whitespace().collect();
    v["access_token"].as_str().is_some_and(|t| !t.is_empty()) && SCOPES.iter().all(|s| granted.contains(s))
}

// ── Window state (amp.cfg) ───────────────────────────────────────────────────
#[derive(Clone, Debug, PartialEq)]
pub struct UiState {
    pub x: i32,
    pub y: i32,
    pub pl_x: i32,
    pub pl_y: i32,
    pub pl_w: i32,
    pub pl_h: i32,
    pub pl_open: bool,
    pub docked: bool,
    pub scale: i32, // zoom in percent; 0: follow the screen's DPI
    pub on_top: bool,
    pub scroll: bool,
    pub remaining: bool,
    pub shade: bool,
}

impl Default for UiState {
    fn default() -> Self {
        UiState { x: i32::MIN, y: i32::MIN, pl_x: 0, pl_y: 0, pl_w: 275, pl_h: 232, pl_open: true, docked: true,
                  scale: 0, on_top: false, scroll: true, remaining: false, shade: false }
    }
}

fn state_path() -> PathBuf {
    config_dir().join("amp.cfg")
}

pub fn parse_state(text: &str) -> UiState {
    let mut s = UiState::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let n = v.trim().parse::<i32>().ok();
        let b = n.map(|n| n != 0);
        match k.trim() {
            "x" => s.x = n.unwrap_or(s.x),
            "y" => s.y = n.unwrap_or(s.y),
            "pl_x" => s.pl_x = n.unwrap_or(s.pl_x),
            "pl_y" => s.pl_y = n.unwrap_or(s.pl_y),
            "pl_w" => s.pl_w = n.unwrap_or(s.pl_w).clamp(275, 4000),
            "pl_h" => s.pl_h = n.unwrap_or(s.pl_h).clamp(116, 4000),
            "pl_open" => s.pl_open = b.unwrap_or(s.pl_open),
            "docked" => s.docked = b.unwrap_or(s.docked),
            // Percent, or 0 to follow the screen. Older builds saved 1 or 2 (1x / 2x), and
            // always wrote 1 even when it was only the default.
            "scale" => s.scale = match n.unwrap_or(0) {
                2..=4 => n.unwrap_or(0) * 100,
                p @ 100..=300 => p,
                _ => 0,
            }.min(300),
            "on_top" => s.on_top = b.unwrap_or(s.on_top),
            "scroll" => s.scroll = b.unwrap_or(s.scroll),
            "remaining" => s.remaining = b.unwrap_or(s.remaining),
            "shade" => s.shade = b.unwrap_or(s.shade),
            _ => {}
        }
    }
    s
}

pub fn format_state(s: &UiState) -> String {
    format!(
        "x={}\ny={}\npl_x={}\npl_y={}\npl_w={}\npl_h={}\npl_open={}\ndocked={}\nscale={}\non_top={}\nscroll={}\nremaining={}\nshade={}\n",
        s.x, s.y, s.pl_x, s.pl_y, s.pl_w, s.pl_h, s.pl_open as i32, s.docked as i32, s.scale, s.on_top as i32,
        s.scroll as i32, s.remaining as i32, s.shade as i32
    )
}

pub fn load_state() -> UiState {
    let Ok(text) = std::fs::read_to_string(state_path()) else { return UiState::default() };
    parse_state(&text)
}

pub fn save_state(s: &UiState) {
    let text = format_state(s);
    let _ = std::fs::create_dir_all(config_dir());
    let tmp = state_path().with_extension("cfg.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, state_path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deliberate_ci_fail_amp() {
        assert!(false, "deliberate amp failure for CI verification");
    }

    #[test]
    fn test_parse_env_extended() {
        // basic comments and export
        let v = parse_env("# c\nexport A=1\nB = \"x\\ny\" \nC='q # r'\nD=v # comment\n1X=no\nE=\"open\n");
        assert_eq!(v, vec![
            ("A".into(), "1".into()), ("B".into(), "x\ny".into()), ("C".into(), "q # r".into()), ("D".into(), "v".into())
        ]);

        // duplicates are returned in file order (document: callers use first-wins, BUG-04)
        let dup = parse_env("DUP=first\nDUP=second\n");
        assert_eq!(dup, vec![("DUP".into(), "first".into()), ("DUP".into(), "second".into())]);

        // escapes in double quotes
        let esc = parse_env("E=\"\\n\\t\\r\\\"\\\\\\a\"\n");
        assert_eq!(esc, vec![("E".into(), "\n\t\r\"\\\\a".into())]);

        // unterminated quote skipped
        let unterm = parse_env("BAD=\"unterminated\nGOOD=1\n");
        assert_eq!(unterm, vec![("GOOD".into(), "1".into())]);

        // BOM is not handled by parse_env itself (callers strip it)
        let bom = parse_env("\u{feff}KEY=val\n");
        assert!(bom.is_empty(), "BOM should cause the key to be rejected by parse_env");

        // NUL skipped
        let nul = parse_env("NUL=val\0with_nul\nOK=2\n");
        assert_eq!(nul, vec![("OK".into(), "2".into())]);

        // inline # comment vs # inside value
        let comments = parse_env("C1=hello # comment\nC2=hello#not_comment\nC3=hello\t# tab comment\n");
        assert_eq!(comments, vec![
            ("C1".into(), "hello".into()),
            ("C2".into(), "hello#not_comment".into()),
            ("C3".into(), "hello".into()),
        ]);
    }

    #[test]
    fn test_valid_client_id_table() {
        // 32 hex lowercase
        assert!(valid_client_id("0123456789abcdef0123456789abcdef"));
        // 32 hex uppercase
        assert!(valid_client_id("0123456789ABCDEF0123456789ABCDEF"));
        // 31 chars
        assert!(!valid_client_id("0123456789abcdef0123456789abcde"));
        // 33 chars
        assert!(!valid_client_id("0123456789abcdef0123456789abcdef0"));
        // non-hex letters
        assert!(!valid_client_id("0123456789abcdef0123456789abcdeg"));
        // empty
        assert!(!valid_client_id(""));
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(prefix: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("spoterm-amp-test-{}-{}-{}", prefix, std::process::id(), id));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_signed_in() {
        let tmp = TempDir::new("signed_in");
        let token_path = tmp.path().join("token.json");

        let all_scopes = "user-read-playback-state user-modify-playback-state user-read-currently-playing \
                          user-library-read user-library-modify playlist-read-private \
                          playlist-read-collaborative streaming";

        // all scopes present is true
        let valid_json = format!(r#"{{"access_token":"token123","scope":"{all_scopes}"}}"#);
        std::fs::write(&token_path, valid_json).unwrap();
        assert!(signed_in(&token_path));

        // extra scopes are fine
        let extra_scopes = format!("{all_scopes} user-follow-read user-top-read");
        std::fs::write(&token_path, format!(r#"{{"access_token":"token123","scope":"{extra_scopes}"}}"#)).unwrap();
        assert!(signed_in(&token_path));

        // scope string with repeated spaces is fine
        let spaced_scopes = "   user-read-playback-state    user-modify-playback-state    user-read-currently-playing    \
                             user-library-read    user-library-modify    playlist-read-private    \
                             playlist-read-collaborative    streaming   ";
        std::fs::write(&token_path, format!(r#"{{"access_token":"token123","scope":"{spaced_scopes}"}}"#)).unwrap();
        assert!(signed_in(&token_path));

        // one scope missing is false
        let missing_scope = "user-read-playback-state user-modify-playback-state user-read-currently-playing \
                             user-library-read user-library-modify playlist-read-private \
                             playlist-read-collaborative";
        std::fs::write(&token_path, format!(r#"{{"access_token":"token123","scope":"{missing_scope}"}}"#)).unwrap();
        assert!(!signed_in(&token_path));

        // empty access_token is false
        std::fs::write(&token_path, format!(r#"{{"access_token":"","scope":"{all_scopes}"}}"#)).unwrap();
        assert!(!signed_in(&token_path));

        // invalid JSON is false
        std::fs::write(&token_path, "not json").unwrap();
        assert!(!signed_in(&token_path));

        // missing file is false
        let missing_path = tmp.path().join("does_not_exist.json");
        assert!(!signed_in(&missing_path));
    }

    #[test]
    fn test_find_engine() {
        let tmp = TempDir::new("find_engine");
        let fake_engine = tmp.path().join("my-engine.exe");
        std::fs::write(&fake_engine, b"").unwrap();

        // explicit path to existing file returns Some
        assert_eq!(find_engine(fake_engine.to_str().unwrap()), Some(fake_engine.clone()));

        // explicit missing path returns None
        let missing = tmp.path().join("missing.exe");
        assert_eq!(find_engine(missing.to_str().unwrap()), None);
    }

    #[test]
    fn test_parse_and_format_state() {
        // defaults for empty text
        let def = parse_state("");
        assert_eq!(def, UiState::default());

        // round trip of a fully populated state
        let full = UiState {
            x: 100,
            y: 200,
            pl_x: 100,
            pl_y: 316,
            pl_w: 350,
            pl_h: 400,
            pl_open: false,
            docked: false,
            scale: 200,
            on_top: true,
            scroll: false,
            remaining: true,
            shade: true,
        };
        let formatted = format_state(&full);
        let parsed = parse_state(&formatted);
        assert_eq!(parsed, full);

        // legacy scale values
        assert_eq!(parse_state("scale=1\n").scale, 0);
        assert_eq!(parse_state("scale=2\n").scale, 200);
        assert_eq!(parse_state("scale=3\n").scale, 300);
        assert_eq!(parse_state("scale=4\n").scale, 300); // 400 capped at 300
        assert_eq!(parse_state("scale=100\n").scale, 100);
        assert_eq!(parse_state("scale=250\n").scale, 250);
        assert_eq!(parse_state("scale=300\n").scale, 300);
        assert_eq!(parse_state("scale=0\n").scale, 0);
        assert_eq!(parse_state("scale=50\n").scale, 0);
        assert_eq!(parse_state("scale=500\n").scale, 0);

        // pl_w clamped to 275..=4000
        assert_eq!(parse_state("pl_w=100\n").pl_w, 275);
        assert_eq!(parse_state("pl_w=5000\n").pl_w, 4000);
        assert_eq!(parse_state("pl_w=500\n").pl_w, 500);

        // pl_h clamped to 116..=4000
        assert_eq!(parse_state("pl_h=50\n").pl_h, 116);
        assert_eq!(parse_state("pl_h=6000\n").pl_h, 4000);
        assert_eq!(parse_state("pl_h=300\n").pl_h, 300);

        // non-numeric values keep defaults
        let non_num = parse_state("x=abc\npl_w=wide\nscale=huge\n");
        assert_eq!(non_num.x, UiState::default().x);
        assert_eq!(non_num.pl_w, UiState::default().pl_w);
        assert_eq!(non_num.scale, UiState::default().scale);

        // unknown keys and lines without '=' ignored
        let unknown = parse_state("random_line\nunknown_key=123\nx=50\n");
        assert_eq!(unknown.x, 50);

        // negative coordinates preserved
        let neg = parse_state("x=-100\ny=-200\npl_x=-50\npl_y=-75\n");
        assert_eq!(neg.x, -100);
        assert_eq!(neg.y, -200);
        assert_eq!(neg.pl_x, -50);
        assert_eq!(neg.pl_y, -75);
    }
}
