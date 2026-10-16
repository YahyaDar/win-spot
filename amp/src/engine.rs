//! The `spoterm-engine` helper process, shared with the terminal UI: it plays audio as a
//! Spotify Connect device and makes every Web API call, speaking JSON lines over a pipe.
//!
//! One reader thread blocks on the engine's stdout (no polling) and hands each event to
//! the UI thread through a queue plus a posted window message. Everything else happens
//! on the UI thread. The engine exits when its stdin closes, which Windows does when
//! this process ends for any reason.

use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

use serde_json::{json, Value};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::config::{config_dir, Settings};

pub const WM_ENGINE: u32 = 0x8000 + 1; // WM_APP + 1: events are waiting
pub const WM_TASK: u32 = 0x8000 + 2; // WM_APP + 2: a sign-in finished (wparam: task, lparam: ok)
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct Engine {
    pub bin: PathBuf,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    events: Receiver<(u32, Value)>,
    sender: Sender<(u32, Value)>,
    gen: u32,
    pub ready: bool,           // the player is connected
    pub alive: bool,           // the engine said hello: API calls can flow
    pub device_id: Option<String>,
    pub state: &'static str,   // "", "login", "retrying"
    pub error: String,
    next_id: u64,
    login_pid: std::cell::Cell<u32>,
}

fn post(hwnd: isize, msg: u32, w: usize, l: isize) {
    unsafe { PostMessageW(hwnd as HWND, msg, w, l) };
}

impl Engine {
    pub fn new(bin: PathBuf) -> Engine {
        let (sender, events) = channel();
        Engine { bin, child: None, stdin: None, events, sender, gen: 0, ready: false, alive: false,
                 device_id: None, state: "", error: String::new(), next_id: 0, login_pid: Default::default() }
    }

    pub fn cache_dir() -> PathBuf {
        config_dir().join("engine")
    }

    pub fn log_path() -> PathBuf {
        config_dir().join("engine.log")
    }

    pub fn player_signed_in() -> bool {
        Self::cache_dir().join("credentials.json").is_file()
    }

    pub fn running(&mut self) -> bool {
        self.child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }

    fn command(&self, s: &Settings) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.creation_flags(CREATE_NO_WINDOW);
        // The engine needs nothing of ours: keep secrets and settings out of its environment.
        for (k, _) in std::env::vars() {
            if k.starts_with("SPOTIPY_") || k.starts_with("SPOTERM_") {
                cmd.env_remove(k);
            }
        }
        for (k, v) in s.proxy_vars() {
            cmd.env(k, v);
        }
        if std::env::var_os("RUST_LOG").is_none() {
            cmd.env("RUST_LOG", "warn");
        }
        cmd
    }

    pub fn start(&mut self, s: &Settings, hwnd: isize) -> Result<(), String> {
        self.stop();
        let cache = Self::cache_dir();
        let _ = std::fs::create_dir_all(&cache);
        let log = std::fs::File::create(Self::log_path()).map_err(|e| format!("engine.log: {e}"))?;
        let mut cmd = self.command(s);
        cmd.arg("run").arg("--cache").arg(&cache).arg("--name").arg(&s.device_name)
            .arg("--bitrate").arg(s.bitrate.to_string()).arg("--client-id").arg(&s.client_id)
            .arg("--token").arg(&s.token_path);
        if !s.engine || !Self::player_signed_in() {
            cmd.arg("--no-player");
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(log).spawn()
            .map_err(|e| format!("couldn't start {}: {e}", self.bin.display()))?;
        self.gen += 1;
        let gen = self.gen;
        let stdout = child.stdout.take().ok_or("no engine pipe")?;
        self.stdin = child.stdin.take();
        self.child = Some(child);
        self.ready = false;
        self.alive = false;
        self.device_id = None;
        let tx = self.sender.clone();
        std::thread::Builder::new()
            .name("spoterm-engine".into())
            .stack_size(64 << 10)
            .spawn(move || {
                // Blocks in the OS until the engine writes: costs nothing while idle.
                for line in BufReader::with_capacity(64 << 10, stdout).lines() {
                    let Ok(line) = line else { break };
                    if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(&line) {
                        if tx.send((gen, v)).is_err() {
                            return;
                        }
                        post(hwnd, WM_ENGINE, 0, 0);
                    }
                }
                let _ = tx.send((gen, json!({"ev": "exit"})));
                post(hwnd, WM_ENGINE, 0, 0);
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Engine events waiting for the UI thread, oldest first; stale ones (from an engine
    /// that was stopped or replaced) are dropped. Updates the connection state.
    pub fn drain(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        while let Ok((gen, ev)) = self.events.try_recv() {
            if gen != self.gen {
                continue;
            }
            let dev = ev["device_id"].as_str().map(str::to_string);
            match ev["ev"].as_str().unwrap_or("") {
                "hello" => self.alive = true,
                "ready" => { self.ready = true; self.device_id = dev; self.state = ""; self.error.clear(); }
                "reconnecting" => { self.ready = false; self.state = "retrying"; }
                "reconnected" => { self.ready = true; self.state = ""; self.error.clear(); if dev.is_some() { self.device_id = dev; } }
                "player" => {
                    self.state = match ev["state"].as_str() { Some("login") => "login", Some("retrying") => "retrying", _ => "" };
                    self.error = ev["msg"].as_str().unwrap_or("").to_string();
                }
                "exit" => {
                    self.ready = false;
                    self.alive = false;
                    self.stdin = None;
                    if let Some(mut c) = self.child.take() {
                        let _ = c.wait();
                    }
                }
                _ => {}
            }
            out.push(ev);
        }
        out
    }

    fn write(&mut self, v: &Value) -> bool {
        let Some(stdin) = self.stdin.as_mut() else { return false };
        let mut line = v.to_string();
        line.push('\n');
        stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()).is_ok()
    }

    /// A player command; only sent while the player is connected.
    pub fn send(&mut self, v: Value) -> bool {
        self.ready && self.write(&v)
    }

    pub fn reconnect(&mut self) -> bool {
        self.write(&json!({"cmd": "reconnect"}))
    }

    /// Start a Web API call; the answer arrives later as an `api` event with this id.
    pub fn api(&mut self, method: &str, path: &str, query: Value, body: Option<Value>) -> Option<u64> {
        self.stdin.as_ref()?;
        self.next_id += 1;
        let id = self.next_id;
        let q = if query.as_object().is_some_and(|o| !o.is_empty()) { query } else { Value::Null };
        self.write(&json!({"cmd": "api", "id": id, "method": method, "path": path, "query": q, "body": body}))
            .then_some(id)
    }

    pub fn stop(&mut self) {
        self.gen += 1; // whatever the old one still says is ignored
        self.ready = false;
        self.alive = false;
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.write_all(b"{\"cmd\":\"quit\"}\n");
        }
        if let Some(mut child) = self.child.take() {
            // It says goodbye to Spotify (~200 ms) and exits; don't hang on a stuck one.
            for _ in 0..30 {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let _ = child.kill();
        }
    }

    /// Run a one-time browser sign-in (`weblogin` or the player's `login`) on a thread;
    /// WM_TASK reports the outcome.
    pub fn sign_in(&self, s: &Settings, player: bool, hwnd: isize) {
        let mut cmd = self.command(s);
        if player {
            let cache = Self::cache_dir();
            let _ = std::fs::create_dir_all(&cache);
            cmd.arg("login").arg("--cache").arg(cache);
        } else {
            cmd.arg("weblogin").arg("--client-id").arg(&s.client_id).arg("--token").arg(&s.token_path)
                .arg("--redirect").arg(&s.redirect_uri);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::null());
        if let Ok(f) = std::fs::File::create(config_dir().join("login.log")) {
            cmd.stderr(f);
        }
        let task = player as usize;
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(_) => {
                post(hwnd, WM_TASK, task, 0);
                return;
            }
        };
        self.login_pid.set(child.id());
        std::thread::spawn(move || {
            let ok = child.wait().is_ok_and(|s| s.success());
            post(hwnd, WM_TASK, task, ok as isize);
        });
    }

    /// Stop a sign-in still waiting for the browser (it would hold its port forever).
    pub fn cancel_sign_in(&self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        let pid = self.login_pid.replace(0);
        if pid != 0 {
            unsafe {
                let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
                if !h.is_null() {
                    TerminateProcess(h, 1);
                    CloseHandle(h);
                }
            }
        }
    }

    pub fn sign_in_done(&self) {
        self.login_pid.set(0);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.cancel_sign_in();
        self.stop();
    }
}
