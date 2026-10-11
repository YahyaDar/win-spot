//! All state and behaviour, on the UI thread (a port of spoterm/app.py's App).
//!
//! Why it stays near 0% CPU:
//!   * The UI thread sleeps in GetMessage. It wakes for input, an engine event, or the
//!     one timer, which is armed for the next thing that is actually due: the next
//!     whole second of the song (to tick the clock), a marquee step (only while the
//!     title is too long to fit and the window is visible), a poll of another device,
//!     or a debounce deadline. Paused with nothing to do, it never wakes at all.
//!   * When SpoTerm's own engine is the player, commands go down a pipe and state comes
//!     back as pushed events: no Web API calls and no polling.
//!   * Web API answers come back as events too, so there are no worker threads: every
//!     request is a numbered entry in `pending` that its answer completes.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{InvalidateRect, HFONT};
use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, KillTimer, SetTimer};

use crate::config::{self, Settings, UiState};
use crate::engine::Engine;
use crate::gfx::Surface;
use crate::skin::Skin;
use crate::spotify::{self as sp, ApiError, Playback, Playlist, Repeat, Track};

pub const POLL_PLAYING: Duration = Duration::from_secs(5);
pub const POLL_IDLE: Duration = Duration::from_secs(10);
pub const POLL_ERROR: Duration = Duration::from_secs(15);
pub const VOLUME_STEP: i32 = 5;
pub const SEEK_STEP_MS: i64 = 5_000;
pub const MARQUEE_STEP: Duration = Duration::from_millis(200);
const PREFETCH_ROWS: usize = 25;
pub const TIMER_ID: usize = 1;

/// Remembers the last value asked of the engine, so late echoes of earlier presses
/// (volume 55, then 60, while you're already at 65) don't make the display jump back.
struct Echo<T> {
    target: Option<T>,
    until: Option<Instant>,
}

impl<T> Default for Echo<T> {
    fn default() -> Self {
        Echo { target: None, until: None }
    }
}

impl<T: PartialEq> Echo<T> {
    fn sent(&mut self, now: Instant, v: T) {
        self.target = Some(v);
        self.until = Some(now + Duration::from_millis(1500));
    }

    fn accept(&self, now: Instant, v: &T) -> bool {
        self.until.is_none_or(|u| now >= u) || self.target.as_ref() == Some(v)
    }

    fn clear(&mut self) {
        self.until = None;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Liked,
    Playlist,
    Search,
}

pub struct TrackList {
    pub key: String,
    pub kind: Kind,
    pub title: String,
    pub context_uri: Option<String>,
    pub playlist_id: Option<String>,
    pub query: String,
    pub tracks: Vec<Rc<Track>>,
    pub total: u32,
    pub next_offset: Option<u32>,
    pub loading: bool,
    pub error: String,
    pub locked: bool, // Spotify won't let apps list this playlist
    pub stale: bool,
    gen: u32,
    retry_at: Option<Instant>,
    pub sel: usize,
    pub top: usize,
}

impl TrackList {
    fn new(key: &str, kind: Kind, title: &str) -> TrackList {
        TrackList { key: key.into(), kind, title: title.into(), context_uri: None, playlist_id: None, query: String::new(),
                    tracks: Vec::new(), total: 0, next_offset: Some(0), loading: false, error: String::new(), locked: false,
                    stale: false, gen: 0, retry_at: None, sel: 0, top: 0 }
    }

    fn reset(&mut self) {
        self.gen += 1;
        self.tracks.clear();
        self.total = 0;
        self.next_offset = Some(0);
        self.loading = false;
        self.error.clear();
        self.locked = false;
        self.stale = false;
        self.retry_at = None;
        self.sel = 0;
        self.top = 0;
    }
}

enum Req {
    Me,
    Playlists,
    Page { key: String, gen: u32 },
    Liked { uri: String },
    SetLiked { track: Rc<Track>, new: bool },
    Poll { seq: u32 },
    Devices,
    /// A command for another device; `key` lets a newer press of the same kind win.
    Cmd { then: Then },
    Volume { v: u8 },
    Retry,
}

#[derive(Clone, Copy, PartialEq)]
enum Then {
    Nothing,
    RemotePlay,
    Transfer,
}

/// What a press of play/enter asked for while the player was still starting.
#[derive(Clone)]
struct PlayReq {
    key: String,
    uri: Option<String>,
}

/// After a remote play or skip: the URI wanted (or one to move away from) and until when
/// Spotify's reports of the previous track are ignored.
struct RemoteExpect {
    want: Option<String>,
    want_name: String,
    avoid: Option<String>,
    until: Instant,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Phase {
    SignIn,
    PlayerSignIn,
    Running,
    Failed,
}

/// What the mouse is holding down.
#[derive(Clone, Copy, PartialEq)]
pub enum Hold {
    None,
    Button(u32),     // a main window button id, pressed
    PlButton(u32),   // a playlist window button id
    Volume,
    Seek(u32),       // the position the thumb is dragged to
    PlScroll(i32),   // grab offset in the scroll handle
}

pub struct App {
    pub s: Settings,
    pub ui: UiState,
    pub skin: Skin,
    pub engine: Engine,
    pub hmain: HWND,
    pub hpl: HWND,
    pub main_sf: Surface,
    pub pl_sf: Surface,
    pub pl_font: HFONT,
    pub small_font: HFONT,
    pub scale: i32,
    pub main_active: bool,
    pub pl_active: bool,
    pub phase: Phase,
    pub hold: Hold,

    // playback
    pub pb: Option<Playback>,
    pub local: bool,
    pub liked_uri: Option<String>,
    pub liked: Option<bool>,
    engine_volume: Option<u8>,
    vol_echo: Echo<u8>,
    shuffle_echo: Echo<bool>,
    repeat_echo: Echo<Repeat>,
    play_echo: Echo<bool>,
    seek_echo: Echo<u32>,
    expect_uri: Option<String>,
    expect_name: String,
    expect_until: Option<Instant>,
    expect_retry: Option<(String, String)>, // (context uri, track uri): plan B via the Web API
    held_track: Option<(Value, Instant)>,
    held_state: Option<(Value, Instant)>,
    local_grace_until: Option<Instant>,
    pending_play: Option<(PlayReq, Instant)>,
    last_load: Option<Value>,
    reload_needed: bool,
    player_warned: bool,
    next_poll: Option<Instant>,
    poll_inflight: bool,
    cmd_seq: u32,
    burst: u32,
    remote_expect: Option<RemoteExpect>,
    cmd_gen: HashMap<&'static str, u32>,
    vol_target: Option<u8>,
    vol_deadline: Option<Instant>,
    seek_target: Option<u32>,
    seek_deadline: Option<Instant>,
    restarts: Vec<Instant>,
    restart_at: Option<Instant>,
    poll_err: String,

    // library
    pub playlists: Vec<Playlist>,
    pub playlists_loaded: bool,
    playlists_acc: Vec<Playlist>,
    user_id: Option<String>,
    pub lists: HashMap<String, TrackList>,
    pub cur: String,
    pub typing: bool,
    /// Drop the next typed character: it belongs to the key that opened search.
    pub eat_char: bool,
    pub query: String,
    pub devices: Vec<sp::Device>,
    pub devices_wanted: bool,

    pending: HashMap<u64, Req>,
    pub status: String,
    status_until: Option<Instant>,
    pub marquee_px: i32,
    pub marquee_key: String,
    marquee_next: Option<Instant>,
    pub marquee_scrolls: bool,
    timer_at: Option<Instant>,
    /// The playlist needs a full redraw (otherwise a paint only refreshes its clock).
    pub pl_full: std::cell::Cell<bool>,
}

pub fn invalidate(h: HWND) {
    if !h.is_null() {
        unsafe { InvalidateRect(h, std::ptr::null(), 0) };
    }
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(s: Settings, ui: UiState, skin: Skin, engine: Engine, pl_font: HFONT, small_font: HFONT, scale: i32) -> App {
        let mut lists = HashMap::new();
        lists.insert("liked".to_string(), TrackList::new("liked", Kind::Liked, "Liked Songs"));
        lists.insert("search".to_string(), TrackList::new("search", Kind::Search, "Search"));
        let (pw, ph) = (ui.pl_w, ui.pl_h);
        App {
            s, ui, skin, engine, hmain: std::ptr::null_mut(), hpl: std::ptr::null_mut(),
            main_sf: Surface::new(275, 116), pl_sf: Surface::new(pw, ph), pl_font, small_font, scale,
            main_active: true, pl_active: false, phase: Phase::Running, hold: Hold::None,
            pb: None, local: false, liked_uri: None, liked: None, engine_volume: None,
            vol_echo: Echo::default(), shuffle_echo: Echo::default(), repeat_echo: Echo::default(),
            play_echo: Echo::default(), seek_echo: Echo::default(),
            expect_uri: None, expect_name: String::new(), expect_until: None, expect_retry: None,
            held_track: None, held_state: None, local_grace_until: None, pending_play: None,
            last_load: None, reload_needed: false, player_warned: false,
            next_poll: None, poll_inflight: false, cmd_seq: 0, burst: 0, remote_expect: None,
            cmd_gen: HashMap::new(), vol_target: None, vol_deadline: None, seek_target: None, seek_deadline: None,
            restarts: Vec::new(), restart_at: None, poll_err: String::new(),
            playlists: Vec::new(), playlists_loaded: false, playlists_acc: Vec::new(), user_id: None,
            lists, cur: "liked".into(), typing: false, eat_char: false, query: String::new(), devices: Vec::new(), devices_wanted: false,
            pending: HashMap::new(), status: String::new(), status_until: None,
            marquee_px: 0, marquee_key: String::new(), marquee_next: None, marquee_scrolls: false, timer_at: None,
            pl_full: std::cell::Cell::new(true),
        }
    }

    // ── Redraw and the timer ─────────────────────────────────────────────────
    pub fn inv_main(&self) {
        invalidate(self.hmain);
    }

    /// The playlist's contents changed: redraw all of it on the next paint.
    pub fn inv_pl(&self) {
        self.pl_full.set(true);
        invalidate(self.hpl);
    }

    /// Only the playlist's mini clock changed (the once-a-second tick).
    fn inv_pl_clock(&self) {
        if !self.hpl.is_null() && self.ui.pl_open {
            invalidate_rect(self.hpl, self.ui.pl_w - 150 + 66, self.ui.pl_h - 38 + 23, 25, 6, self.scale);
        }
    }

    pub fn inv_all(&self) {
        self.inv_main();
        self.inv_pl();
    }

    pub fn flash(&mut self, msg: impl Into<String>, warn: bool) {
        self.status = msg.into();
        self.status_until = Some(Instant::now() + Duration::from_millis(if warn { 5000 } else { 2500 }));
        self.inv_main();
    }

    fn visible(&self) -> bool {
        unsafe { IsIconic(self.hmain) == 0 }
    }

    /// Arm the one timer for whatever is due next, or stop it when nothing is.
    pub fn reschedule(&mut self) {
        let now = Instant::now();
        let mut due: Option<Instant> = None;
        let mut at = |t: Option<Instant>| {
            if let Some(t) = t {
                due = Some(due.map_or(t, |d| d.min(t)));
            }
        };
        at(self.next_poll.filter(|_| !self.poll_inflight));
        at(self.status_until);
        at(self.vol_deadline);
        at(self.seek_deadline);
        at(self.restart_at);
        at(self.expect_until.filter(|_| self.expect_uri.is_some()));
        at(self.pending_play.as_ref().map(|p| p.1));
        if let Some(pb) = &self.pb {
            if pb.playing && pb.track.is_some() && self.visible() {
                let ms = pb.progress(now) % 1000;
                at(Some(now + Duration::from_millis((1000 - ms) as u64 + 5)));
            }
        }
        if self.marquee_scrolls && self.ui.scroll && self.visible() {
            at(self.marquee_next.or(Some(now + MARQUEE_STEP)));
        }
        let ms = due.map(|d| d.saturating_duration_since(now).as_millis().clamp(10, 0x7FFF_FFFF) as u32);
        unsafe {
            match ms {
                Some(ms) => {
                    SetTimer(self.hmain, TIMER_ID, ms, None);
                }
                None => {
                    KillTimer(self.hmain, TIMER_ID);
                }
            }
        }
        self.timer_at = due;
    }

    pub fn on_timer(&mut self) {
        let now = Instant::now();
        if !self.poll_inflight && self.next_poll.is_some_and(|t| now >= t) {
            self.poll();
        }
        if self.vol_deadline.is_some_and(|t| now >= t) {
            self.flush_volume();
        }
        if self.seek_deadline.is_some_and(|t| now >= t) {
            self.flush_seek();
        }
        if self.restart_at.is_some_and(|t| now >= t) {
            self.restart_at = None;
            self.start_engine();
        }
        if self.expect_uri.is_some() && self.expect_until.is_some_and(|t| now >= t) {
            self.expect_done(now);
        }
        if self.pending_play.as_ref().is_some_and(|p| now >= p.1) {
            self.pending_play = None;
            self.flash("SpoTerm's player didn't start: retry from the menu, or pick a device", true);
        }
        if self.status_until.is_some_and(|t| now >= t) {
            self.status.clear();
            self.status_until = None;
            self.inv_main();
        }
        if self.marquee_scrolls && self.ui.scroll && self.marquee_next.is_none_or(|t| now >= t) {
            self.marquee_px += 5;
            self.marquee_next = Some(now + MARQUEE_STEP);
            invalidate_rect(self.hmain, 111, 22, 154, 16, self.scale);
        }
        if self.pb.as_ref().is_some_and(|p| p.playing) {
            // The clock, the position bar and the playlist's mini time.
            invalidate_rect(self.hmain, 36, 26, 63, 13, self.scale);
            invalidate_rect(self.hmain, 16, 72, 248, 10, self.scale);
            self.inv_pl_clock();
        }
        self.reschedule();
    }

    // ── Startup and sign-in ──────────────────────────────────────────────────
    pub fn begin(&mut self) {
        if !config::signed_in(&self.s.token_path) {
            self.phase = Phase::SignIn;
            self.flash_long("Sign in to Spotify in your browser (1/2)");
            self.engine.sign_in(&self.s, false, self.hmain as isize);
        } else {
            self.after_web_sign_in();
        }
    }

    fn flash_long(&mut self, msg: &str) {
        self.status = msg.into();
        self.status_until = None; // stays until replaced
        self.inv_main();
    }

    fn after_web_sign_in(&mut self) {
        if self.s.engine && !Engine::player_signed_in() {
            self.phase = Phase::PlayerSignIn;
            self.flash_long("Sign in SpoTerm's player in your browser (2/2)");
            self.engine.sign_in(&self.s, true, self.hmain as isize);
        } else {
            self.start_engine();
        }
    }

    /// A sign-in process finished: `player` says which one.
    pub fn on_sign_in(&mut self, player: bool, ok: bool) {
        self.engine.sign_in_done();
        if !player {
            if ok {
                self.status.clear();
                if self.engine.running() {
                    self.engine.stop(); // pick up the new login
                }
                self.after_web_sign_in();
            } else {
                self.phase = Phase::Failed;
                self.flash_long("Spotify sign-in didn't finish: right-click > Sign in to try again");
            }
        } else {
            if !ok {
                self.flash("Player sign-in skipped: SpoTermAmp will control other devices", true);
            }
            self.start_engine();
        }
    }

    pub fn sign_in_again(&mut self) {
        self.engine.cancel_sign_in();
        self.engine.stop();
        self.phase = Phase::SignIn;
        self.flash_long("Sign in to Spotify in your browser");
        self.engine.sign_in(&self.s, false, self.hmain as isize);
    }

    pub fn start_engine(&mut self) {
        self.phase = Phase::Running;
        if self.status.starts_with("Sign in") {
            self.status.clear();
        }
        self.pending.clear();
        self.poll_inflight = false;
        if let Err(e) = self.engine.start(&self.s, self.hmain as isize) {
            self.flash(e, true);
        }
        self.inv_main();
    }

    /// Ask for everything the windows show, once the engine can make calls.
    fn load_all(&mut self) {
        self.request("GET", "me", json!({}), None, Req::Me);
        self.load_playlists();
        let cur = self.cur.clone();
        self.open_list(&cur);
        self.poll();
    }

    // ── Web API plumbing ─────────────────────────────────────────────────────
    fn request(&mut self, method: &str, path: &str, query: Value, body: Option<Value>, req: Req) -> bool {
        match self.engine.api(method, path, query, body) {
            Some(id) => {
                self.pending.insert(id, req);
                true
            }
            None => {
                self.on_reply(req, Err(ApiError { status: 0, msg: "SpoTerm's engine isn't running".into(), login: false, retry_after: 0 }));
                false
            }
        }
    }

    pub fn on_engine_events(&mut self) {
        for ev in self.engine.drain() {
            if ev["ev"] == "api" {
                let id = sp::num(&ev["id"]);
                if let Some(req) = self.pending.remove(&id) {
                    let res = match ApiError::from_event(&ev) {
                        // Player commands answer 200 with a bare id that older engines call
                        // malformed: the command worked.
                        Some(e) if e.status == 0 && e.msg == "Spotify sent a malformed response" && matches!(req, Req::Cmd { .. } | Req::Volume { .. } | Req::Retry) => Ok(Value::Null),
                        Some(e) => Err(e),
                        None => Ok(ev["body"].clone()),
                    };
                    self.on_reply(req, res);
                }
            } else {
                self.on_engine(&ev);
            }
        }
        self.reschedule();
    }

    fn on_reply(&mut self, req: Req, res: Result<Value, ApiError>) {
        if let Err(e) = &res {
            if e.login && self.phase == Phase::Running {
                self.sign_in_again();
                return;
            }
        }
        match req {
            Req::Me => {
                if let Ok(v) = res {
                    self.user_id = v["id"].as_str().map(sp::clean);
                }
            }
            Req::Playlists => match res {
                Ok(v) => {
                    sp::playlists(&v, &mut self.playlists_acc);
                    let next = v["next"].as_str().filter(|_| self.playlists_acc.len() < 10_000).map(str::to_string);
                    match next {
                        Some(url) => {
                            self.request("GET", &url, json!({}), None, Req::Playlists);
                        }
                        None => {
                            self.playlists = std::mem::take(&mut self.playlists_acc);
                            self.playlists_loaded = true;
                        }
                    }
                }
                Err(e) => {
                    self.playlists = std::mem::take(&mut self.playlists_acc);
                    self.playlists_loaded = true;
                    self.flash(e.describe(), true);
                }
            },
            Req::Page { key, gen } => self.on_page(&key, gen, res),
            Req::Liked { uri } => match res {
                Ok(v) if self.liked_uri.as_deref() == Some(&uri) => {
                    self.liked = Some(v.as_array().and_then(|a| a.first()).is_some_and(|b| b == true));
                }
                Err(_) if self.liked_uri.as_deref() == Some(&uri) => self.liked_uri = None,
                _ => {}
            },
            Req::SetLiked { track, new } => match res {
                Ok(_) => {
                    self.flash(if new { "Added to Liked Songs" } else { "Removed from Liked Songs" }, false);
                    if let Some(l) = self.lists.get_mut("liked") {
                        if !l.tracks.is_empty() {
                            if new && l.tracks.iter().all(|x| x.uri != track.uri) {
                                l.tracks.insert(0, track);
                                l.total += 1;
                                if l.sel > 0 || l.tracks.len() > 1 {
                                    l.sel += 1;
                                }
                            } else if !new {
                                let before = l.tracks.len();
                                l.tracks.retain(|x| x.uri != track.uri);
                                l.total = l.total.saturating_sub((before - l.tracks.len()) as u32);
                                l.sel = l.sel.min(l.tracks.len().saturating_sub(1));
                            }
                        }
                    }
                    self.inv_pl();
                }
                Err(e) => {
                    if self.liked_uri.as_deref() == Some(&track.uri) {
                        self.liked = Some(!new);
                    }
                    self.flash(e.describe(), true);
                }
            },
            Req::Poll { seq } => self.on_poll(seq, res),
            Req::Devices => match res {
                Ok(v) => {
                    self.devices = sp::devices(&v);
                    if self.devices.is_empty() {
                        self.flash("Spotify lists no devices: open Spotify somewhere", true);
                        self.devices_wanted = false;
                    }
                }
                Err(e) => {
                    self.devices_wanted = false;
                    self.flash(e.describe(), true);
                }
            },
            Req::Cmd { then } => match res {
                Ok(_) => {
                    if then == Then::Transfer {
                        self.flash("Playback moved", false);
                    }
                    if then == Then::RemotePlay {
                        if let Some(x) = &mut self.remote_expect {
                            x.until = x.until.max(Instant::now() + Duration::from_secs(4));
                        }
                    }
                    self.poll_soon(1, Duration::from_millis(250));
                }
                Err(e) => {
                    if then == Then::RemotePlay && e.no_device() && self.engine_can_play() {
                        // The phone went away (app closed): play here instead of failing.
                        self.remote_expect = None;
                        self.pb = None;
                        let key = self.cur.clone();
                        let uri = self.selected().map(|t| t.uri.clone());
                        self.play_here(PlayReq { key, uri });
                        return;
                    }
                    self.flash(self.describe(&e), true);
                    self.poll_soon(1, Duration::from_millis(350));
                }
            },
            Req::Volume { v } => {
                if let Err(e) = res {
                    self.vol_target = None;
                    self.flash(self.describe(&e), true);
                    self.poll_soon(1, Duration::from_millis(350));
                } else if self.vol_target == Some(v) {
                    self.vol_target = None;
                }
            }
            Req::Retry => {}
        }
        self.inv_all();
    }

    fn describe(&self, e: &ApiError) -> String {
        if e.no_device() && self.engine.state == "retrying" {
            return "No active device, and SpoTerm's player can't connect yet (retrying)".into();
        }
        if e.no_device() {
            return "Spotify lists no active device: open Spotify somewhere, or pick a device".into();
        }
        e.describe()
    }

    // ── Engine events ────────────────────────────────────────────────────────
    fn on_engine(&mut self, ev: &Value) {
        let now = Instant::now();
        let kind = ev["ev"].as_str().unwrap_or("");
        self.inv_main();
        match kind {
            "hello" => {
                self.load_all();
                return;
            }
            "ready" => {
                if !ev["volume"].is_null() {
                    self.engine_volume = Some(sp::num(&ev["volume"]).min(100) as u8);
                }
                if self.player_warned {
                    self.player_warned = false;
                    self.flash("SpoTerm's player is connected", false);
                }
                if let Some((req, _)) = self.pending_play.take() {
                    self.play_here(req);
                }
                return;
            }
            "exit" => {
                let was_local = std::mem::replace(&mut self.local, false);
                self.pending_play = None;
                self.expect_uri = None;
                self.expect_retry = None;
                self.held_track = None;
                self.held_state = None;
                self.pending.clear();
                self.poll_inflight = false;
                self.next_poll = Some(now);
                if was_local {
                    self.pb = None;
                }
                if self.phase != Phase::Running {
                    return; // stopped on purpose for a sign-in
                }
                // The engine carries the player and every API call: bring it back. A crash
                // loop gets three tries a minute, then waits for the menu's Retry.
                self.restarts.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
                self.restarts.push(now);
                if self.restarts.len() <= 3 {
                    self.flash("SpoTerm's engine stopped: restarting it", true);
                    self.restart_at = Some(now + Duration::from_secs(1));
                } else {
                    self.flash("SpoTerm's engine keeps stopping (see engine.log): use Retry", true);
                }
                return;
            }
            "error" => {
                let msg = sp::text(&ev["msg"]);
                self.flash(format!("Player: {}", if msg.is_empty() { "command failed".into() } else { msg }), true);
                return;
            }
            "player" => {
                if ev["state"] == "login" {
                    self.pending_play = None;
                    self.flash("SpoTerm's player needs its sign-in: right-click > Sign in the player", true);
                } else if ev["state"] == "retrying" {
                    let why = sp::text(&ev["msg"]);
                    if self.pending_play.take().is_some() {
                        self.player_warned = false; // this press deserves an answer
                    }
                    if !self.player_warned {
                        self.player_warned = true;
                        if why.contains("503") || why.to_lowercase().contains("unavailable") {
                            self.flash("Spotify's playback service is down (their side): retrying", true);
                        } else {
                            self.flash(format!("SpoTerm's player can't connect ({why}): retrying"), true);
                        }
                    }
                }
                return;
            }
            "reconnecting" => {
                if self.local {
                    if let Some(pb) = &mut self.pb {
                        pb.rebase(now);
                        pb.playing = false; // the audio stopped with the connection
                        self.reload_needed = true;
                    }
                }
                self.next_poll = Some(self.next_poll.map_or(now + POLL_IDLE, |t| t.min(now + POLL_IDLE)));
                self.flash("Connection to Spotify lost, reconnecting...", true);
                return;
            }
            "reconnected" => {
                self.player_warned = false;
                self.flash(if self.reload_needed { "Reconnected: press play to resume" } else { "Reconnected to Spotify" }, false);
                return;
            }
            "active" => {
                if ev["on"] != true && self.local && self.local_grace_until.is_none_or(|t| now >= t) {
                    self.poll_soon(0, Duration::from_millis(350)); // playback may have moved away
                }
                return;
            }
            "volume" => {
                let v = sp::num(&ev["pct"]).min(100) as u8;
                if self.vol_echo.accept(now, &v) {
                    self.engine_volume = Some(v);
                    if self.local {
                        if let Some(pb) = &mut self.pb {
                            pb.volume = Some(v);
                        }
                    }
                }
                return;
            }
            "shuffle" => {
                let on = ev["on"] == true;
                if self.shuffle_echo.accept(now, &on) && self.local {
                    if let Some(pb) = &mut self.pb {
                        pb.shuffle = on;
                    }
                }
                return;
            }
            "repeat" => {
                let r = Repeat::parse(ev["mode"].as_str().unwrap_or("off"));
                if self.repeat_echo.accept(now, &r) && self.local {
                    if let Some(pb) = &mut self.pb {
                        pb.repeat = r;
                    }
                }
                return;
            }
            "track" | "playing" | "paused" | "loading" | "pos" | "stopped" | "end" | "unavailable" => {}
            _ => return,
        }

        if self.expect_uri.is_some() && self.expect_until.is_some_and(|t| now >= t) {
            self.expect_done(now);
        }
        if let Some(want) = self.expect_uri.clone() {
            // Just after you pick a track, the engine still reports on the one before it.
            // Only the picked track may update the screen until it shows up; the rest is
            // held back (not dropped) in case it never does.
            let uri = ev["uri"].as_str().unwrap_or("");
            if uri == want || (kind == "track" && sp::text(&ev["name"]) == self.expect_name) {
                if matches!(kind, "track" | "playing" | "paused" | "unavailable") {
                    self.expect_uri = None;
                    self.expect_retry = None;
                    self.held_track = None;
                    self.held_state = None;
                }
            } else if kind == "track" && self.expect_retry.is_some() {
                // The engine started another track (librespot only sees the first page of a
                // long playlist): have Spotify start the right one on this device instead.
                let (ctx, track_uri) = self.expect_retry.take().unwrap_or_default();
                self.held_track = Some((ev.clone(), now));
                self.held_state = None;
                self.expect_until = Some(now + Duration::from_secs(6));
                if let Some(dev) = self.engine.device_id.clone() {
                    self.request("PUT", "me/player/play", json!({"device_id": dev}),
                                 Some(json!({"context_uri": ctx, "offset": {"uri": track_uri}})), Req::Retry);
                }
                return;
            } else {
                if kind == "track" {
                    self.held_track = Some((ev.clone(), now));
                    self.held_state = None;
                } else if matches!(kind, "playing" | "paused" | "pos" | "stopped") {
                    self.held_state = Some((ev.clone(), now));
                }
                return;
            }
        }
        self.apply_engine(ev, now, now);
    }

    fn expect_done(&mut self, now: Instant) {
        self.expect_uri = None;
        self.expect_retry = None;
        for (ev, at) in [self.held_track.take(), self.held_state.take()].into_iter().flatten() {
            self.apply_engine(&ev, at, now);
        }
        self.inv_all();
    }

    /// Our own device takes over the display.
    fn local_pb(&mut self, now: Instant) -> &mut Playback {
        let dev = self.engine.device_id.clone();
        let mine = self.local && self.pb.as_ref().is_some_and(|p| p.device_id == dev);
        if !mine {
            let old = self.pb.take();
            let same_dev = old.as_ref().is_some_and(|p| p.device_id == dev);
            // Shuffle and repeat follow you from device to device, as in Spotify's apps.
            self.pb = Some(Playback {
                track: if same_dev { old.as_ref().and_then(|p| p.track.clone()) } else { None },
                playing: false,
                progress_ms: 0,
                at: now,
                device_id: dev,
                device_name: self.s.device_name.clone(),
                volume: if same_dev { old.as_ref().and_then(|p| p.volume) } else { self.engine_volume },
                shuffle: old.as_ref().is_some_and(|p| p.shuffle),
                repeat: old.as_ref().map_or(Repeat::Off, |p| p.repeat),
                context_uri: None,
            });
            self.local = true;
            self.remote_expect = None;
            self.next_poll = None; // events keep us current: stop polling
        }
        self.pb.as_mut().expect("just set")
    }

    fn apply_engine(&mut self, ev: &Value, at: Instant, now: Instant) {
        let kind = ev["ev"].as_str().unwrap_or("");
        let uri = sp::text(&ev["uri"]);
        let pos = sp::num(&ev["pos"]).min(u32::MAX as u64) as u32;
        match kind {
            "unavailable" => {
                if self.local && self.pb.as_ref().and_then(|p| p.track.as_ref()).is_some_and(|t| t.uri == uri) {
                    self.flash("That track isn't available here, skipping", true);
                }
                return;
            }
            "end" => return,
            "playing" | "paused" if !self.play_echo.accept(now, &(kind == "playing")) => return,
            "pos" if !self.seek_echo.accept(now, &pos) => return,
            _ => {}
        }
        let grace = self.local_grace_until;
        let pb = self.local_pb(now);
        match kind {
            "track" => {
                if !pb.track.as_ref().is_some_and(|t| t.uri == uri) {
                    let name = sp::text(&ev["name"]);
                    pb.track = Some(Rc::new(Track {
                        uri,
                        name: if name.is_empty() { "Unknown".into() } else { name },
                        artists: sp::text(&ev["artists"]),
                        album: sp::text(&ev["album"]),
                        duration_ms: sp::num(&ev["duration_ms"]).min(u32::MAX as u64) as u32,
                        pos: 0,
                        playable: true,
                    }));
                }
                pb.progress_ms = 0;
                pb.at = now;
                let t = pb.track.clone();
                self.track_changed(t);
                self.inv_pl();
            }
            "playing" | "paused" | "loading" | "pos" => {
                if kind == "playing" {
                    pb.playing = true;
                } else if kind == "paused" {
                    pb.playing = false;
                }
                let late = if pb.playing && kind != "loading" { now.saturating_duration_since(at).as_millis() as u32 } else { 0 };
                pb.progress_ms = pos + late;
                pb.at = now;
            }
            "stopped" => {
                pb.rebase(now);
                pb.playing = false;
                if grace.is_none_or(|t| now >= t) {
                    self.poll_soon(0, Duration::from_millis(350)); // often another device took over
                }
            }
            _ => {}
        }
    }

    fn track_changed(&mut self, track: Option<Rc<Track>>) {
        let Some(t) = track else { return };
        if self.liked_uri.as_deref() == Some(&t.uri) {
            return;
        }
        self.liked_uri = Some(t.uri.clone());
        self.liked = None;
        self.marquee_px = 0;
        if t.is_track() {
            self.request("GET", "me/library/contains", json!({"uris": t.uri}), None, Req::Liked { uri: t.uri.clone() });
        }
    }

    // ── Polling another device ───────────────────────────────────────────────
    pub fn poll(&mut self) {
        if !self.engine.alive {
            self.next_poll = None; // the engine's hello starts polling again
            return;
        }
        self.poll_inflight = true;
        self.next_poll = None;
        let seq = self.cmd_seq;
        self.request("GET", "me/player", json!({"additional_types": "episode", "market": "from_token"}), None, Req::Poll { seq });
    }

    fn poll_soon(&mut self, bursts: u32, delay: Duration) {
        self.burst = self.burst.max(bursts);
        if !self.poll_inflight {
            let t = Instant::now() + delay;
            self.next_poll = Some(self.next_poll.map_or(t, |n| n.min(t)));
        }
    }

    fn on_poll(&mut self, seq: u32, res: Result<Value, ApiError>) {
        self.poll_inflight = false;
        let now = Instant::now();
        self.next_poll = Some(now + POLL_ERROR);
        let v = match res {
            Ok(v) => v,
            Err(e) => {
                let wait = Duration::from_secs(e.retry_after.clamp(15, 300));
                self.next_poll = Some(now + wait);
                let msg = self.describe(&e);
                if msg != self.poll_err {
                    self.poll_err = msg.clone(); // once, not on every poll while it lasts
                    self.flash(msg, true);
                }
                return;
            }
        };
        self.poll_err.clear();
        if seq != self.cmd_seq {
            // Started before the latest command: it would undo the instant update.
            self.next_poll = Some(now + Duration::from_millis(600));
            return;
        }
        let pb = sp::playback(&v, now);
        let eng = self.engine.device_id.clone();
        if self.local {
            if pb.is_none() || (eng.is_some() && pb.as_ref().unwrap().device_id == eng) {
                // Still ours: the engine's events are fresher, unless it's reconnecting.
                self.next_poll = if self.engine.ready { None } else { Some(now + POLL_IDLE) };
                return;
            }
            if let Some(g) = self.local_grace_until.filter(|g| now < *g) {
                self.next_poll = Some(g + Duration::from_millis(500)); // Spotify lags behind us
                return;
            }
            self.local = false; // playback moved to another device
        }
        if let Some(x) = &self.remote_expect {
            let t = pb.as_ref().and_then(|p| p.track.as_ref());
            let arrived = t.is_some_and(|t| match &x.want {
                Some(w) => &t.uri == w || t.name == x.want_name,
                None => Some(&t.uri) != x.avoid.as_ref(),
            });
            if now >= x.until || arrived {
                self.remote_expect = None;
            } else {
                self.next_poll = Some(now + Duration::from_millis(700)); // still the old track
                return;
            }
        }
        let mut pb = pb;
        if let (Some(p), Some(v)) = (pb.as_mut(), self.vol_target) {
            p.volume = Some(v);
        }
        // Same track: keep the same object so nothing is redrawn for nothing.
        if let (Some(p), Some(old)) = (pb.as_mut(), self.pb.as_ref().and_then(|o| o.track.clone())) {
            if p.track.as_ref().is_some_and(|t| t.uri == old.uri && t.duration_ms == old.duration_ms) {
                p.track = Some(old);
            }
        }
        let track = pb.as_ref().and_then(|p| p.track.clone());
        let playing = pb.as_ref().is_some_and(|p| p.playing);
        self.pb = pb;
        self.track_changed(track.clone());
        let delay = if self.burst > 0 {
            self.burst -= 1;
            Duration::from_millis(800)
        } else if let (true, Some(t)) = (playing, &track) {
            let left = t.duration_ms.saturating_sub(self.pb.as_ref().map_or(0, |p| p.progress(now)));
            if t.duration_ms > 0 { POLL_PLAYING.min(Duration::from_millis(left as u64 + 400).max(Duration::from_millis(500))) } else { POLL_PLAYING }
        } else if playing {
            POLL_PLAYING
        } else {
            POLL_IDLE
        };
        self.next_poll = Some(now + delay);
        self.inv_all();
    }

    // ── Playback commands ────────────────────────────────────────────────────
    fn send(&mut self, v: Value) -> bool {
        self.local && self.engine.send(v)
    }

    /// A Web API command for another device (always the one shown).
    fn command(&mut self, method: &str, action: &str, mut query: Map<String, Value>, body: Option<Value>, then: Then) {
        self.cmd_seq += 1;
        if let Some(dev) = self.pb.as_ref().and_then(|p| p.device_id.clone()) {
            query.entry("device_id").or_insert(Value::String(dev));
        }
        let path = if action.is_empty() { "me/player".to_string() } else { format!("me/player/{action}") };
        self.request(method, &path, Value::Object(query), body, Req::Cmd { then });
    }

    fn player_offline(&mut self) -> bool {
        if self.local && !self.engine.ready {
            self.flash("SpoTerm's player is reconnecting to Spotify...", true);
            return true;
        }
        false
    }

    pub fn engine_can_play(&mut self) -> bool {
        self.s.engine && Engine::player_signed_in() && self.engine.running() && self.engine.state != "login"
    }

    pub fn toggle_play(&mut self) {
        if self.player_offline() {
            return;
        }
        let now = Instant::now();
        if let Some(pb) = &mut self.pb {
            pb.rebase(now);
        }
        let playing = self.pb.as_ref().is_some_and(|p| p.playing);
        if self.local && self.reload_needed && !playing && self.last_load.is_some() && self.pb.as_ref().is_some_and(|p| p.track.is_some()) {
            self.resume_load(now);
        } else if playing {
            self.pause();
        } else {
            self.play();
        }
        self.inv_all();
    }

    pub fn pause(&mut self) {
        let now = Instant::now();
        let Some(pb) = &mut self.pb else { return };
        if !pb.playing {
            return;
        }
        pb.rebase(now);
        pb.playing = false;
        if self.send(json!({"cmd": "pause"})) {
            self.play_echo.sent(now, false);
        } else {
            self.command("PUT", "pause", Map::new(), None, Then::Nothing);
        }
        self.inv_all();
    }

    /// Resume; with nothing to resume anywhere, play the selection here.
    pub fn play(&mut self) {
        if self.player_offline() {
            return;
        }
        let now = Instant::now();
        let has_device = self.pb.as_ref().is_some_and(|p| p.device_id.is_some());
        if !has_device && self.engine_can_play() {
            self.play_selected();
            return;
        }
        if self.pb.as_ref().is_some_and(|p| p.playing) {
            return;
        }
        if let Some(pb) = &mut self.pb {
            pb.rebase(now);
            if pb.track.is_some() {
                pb.playing = true;
            }
        }
        if self.send(json!({"cmd": "play"})) {
            self.play_echo.sent(now, true);
        } else {
            self.command("PUT", "play", Map::new(), None, Then::Nothing);
        }
        self.inv_all();
    }

    /// Winamp's stop: Spotify has none, so pause and go back to the start.
    pub fn stop(&mut self) {
        self.pause();
        if self.pb.as_ref().is_some_and(|p| p.track.is_some()) {
            let pos = self.pb.as_ref().map_or(0, |p| p.progress(Instant::now()));
            self.seek_to(0);
            if pos > 0 {
                self.flash("Stopped", false);
            }
        }
    }

    fn resume_load(&mut self, now: Instant) {
        let Some(mut load) = self.last_load.clone() else { return };
        let Some(pb) = &self.pb else { return };
        let Some(t) = &pb.track else { return };
        load["position_ms"] = json!(pb.progress(now));
        let key = if load["cmd"] == "play_tracks" { "start_uri" } else { "track_uri" };
        load[key] = json!(t.uri);
        load["shuffle"] = json!(pb.shuffle);
        load["repeat"] = json!(pb.repeat.as_str());
        self.local_grace_until = Some(now + Duration::from_secs(8));
        if self.send(load) {
            self.reload_needed = false;
            if let Some(pb) = &mut self.pb {
                pb.playing = true;
            }
            self.play_echo.clear();
        }
    }

    pub fn skip(&mut self, forward: bool) {
        if self.player_offline() {
            return;
        }
        let now = Instant::now();
        self.seek_target = None;
        self.seek_deadline = None;
        let changes = self.pb.as_ref().is_some_and(|p| p.track.is_some() && (forward || p.progress(now) < 3000));
        let old = self.pb.as_ref().and_then(|p| p.track.as_ref()).map(|t| t.uri.clone());
        if let Some(pb) = &mut self.pb {
            if pb.track.is_some() {
                pb.progress_ms = 0;
                pb.at = now;
            }
        }
        if !self.send(json!({"cmd": if forward { "next" } else { "prev" }})) {
            if changes && !self.local {
                // Until Spotify moves on, polls still name the old track: don't show it again.
                self.remote_expect = Some(RemoteExpect { want: None, want_name: String::new(), avoid: old, until: now + Duration::from_secs(4) });
            }
            self.command("POST", if forward { "next" } else { "previous" }, Map::new(), None, Then::Nothing);
        }
        self.inv_all();
    }

    pub fn seek_by(&mut self, delta: i64) {
        let Some(pb) = &self.pb else { return };
        let now = Instant::now();
        let pos = (pb.progress(now) as i64 + delta).max(0) as u32;
        self.seek_to(pos);
    }

    pub fn seek_to(&mut self, pos: u32) {
        if self.player_offline() {
            return;
        }
        let now = Instant::now();
        let Some(pb) = &mut self.pb else { return };
        let Some(t) = &pb.track else { return };
        let mut pos = pos;
        if t.duration_ms > 1000 {
            pos = pos.min(t.duration_ms - 1000); // 0 means unknown: don't clamp to it
        }
        pb.progress_ms = pos;
        pb.at = now;
        self.cmd_seq += 1; // a poll already in flight predates this: don't snap back
        if self.send(json!({"cmd": "seek", "ms": pos})) {
            self.seek_echo.sent(now, pos);
        } else {
            self.seek_target = Some(pos);
            self.seek_deadline = Some(now + Duration::from_millis(300)); // one request per burst
        }
        self.inv_all();
    }

    fn flush_seek(&mut self) {
        self.seek_deadline = None;
        if let Some(pos) = self.seek_target.take() {
            let mut q = Map::new();
            q.insert("position_ms".into(), json!(pos));
            self.command("PUT", "seek", q, None, Then::Nothing);
        }
    }

    pub fn volume(&self) -> Option<u8> {
        self.vol_target.or(self.pb.as_ref().and_then(|p| p.volume)).or(if self.pb.is_none() { self.engine_volume } else { None })
    }

    pub fn change_volume(&mut self, delta: i32) {
        let base = self.volume();
        let Some(base) = base else {
            self.flash(if self.pb.is_some() { "Volume can't be changed on this device" } else { "Nothing is playing" }, true);
            return;
        };
        self.set_volume((base as i32 + delta).clamp(0, 100) as u8);
    }

    pub fn set_volume(&mut self, v: u8) {
        if self.player_offline() {
            return;
        }
        let now = Instant::now();
        self.flash(format!("Volume: {v}%"), false);
        if self.pb.is_none() {
            // Nothing playing yet: set our own player's volume for when it does.
            self.engine_volume = Some(v);
            if self.engine.ready && self.engine.send(json!({"cmd": "volume", "pct": v})) {
                self.vol_echo.sent(now, v);
            }
            return;
        }
        if let Some(pb) = &mut self.pb {
            if pb.volume.is_none() {
                self.flash("Volume can't be changed on this device", true);
                return;
            }
            pb.volume = Some(v);
        }
        if self.send(json!({"cmd": "volume", "pct": v})) {
            self.vol_echo.sent(now, v); // local: applied instantly, no debounce
            self.engine_volume = Some(v);
        } else {
            self.vol_target = Some(v);
            self.vol_deadline = Some(now + Duration::from_millis(250));
        }
        self.inv_main();
    }

    fn flush_volume(&mut self) {
        self.vol_deadline = None;
        let Some(v) = self.vol_target else { return };
        self.cmd_seq += 1;
        let mut q = Map::new();
        q.insert("volume_percent".into(), json!(v));
        if let Some(dev) = self.pb.as_ref().and_then(|p| p.device_id.clone()) {
            q.insert("device_id".into(), json!(dev));
        }
        self.request("PUT", "me/player/volume", Value::Object(q), None, Req::Volume { v });
    }

    pub fn toggle_shuffle(&mut self) {
        if self.player_offline() {
            return;
        }
        let Some(pb) = &mut self.pb else {
            self.flash("Nothing is playing", false);
            return;
        };
        pb.shuffle = !pb.shuffle;
        let on = pb.shuffle;
        self.flash(if on { "Shuffle: on" } else { "Shuffle: off" }, false);
        if self.send(json!({"cmd": "shuffle", "on": on})) {
            self.shuffle_echo.sent(Instant::now(), on);
        } else {
            self.next_gen("shuffle");
            let mut q = Map::new();
            q.insert("state".into(), json!(if on { "true" } else { "false" }));
            self.command("PUT", "shuffle", q, None, Then::Nothing);
        }
        self.inv_main();
    }

    pub fn cycle_repeat(&mut self) {
        let next = self.pb.as_ref().map_or(Repeat::Context, |p| p.repeat.next());
        self.set_repeat(next);
    }

    pub fn set_repeat(&mut self, r: Repeat) {
        if self.player_offline() {
            return;
        }
        let Some(pb) = &mut self.pb else {
            self.flash("Nothing is playing", false);
            return;
        };
        pb.repeat = r;
        self.flash(match r { Repeat::Off => "Repeat: off", Repeat::Context => "Repeat: all", Repeat::Track => "Repeat: one" }, false);
        if self.send(json!({"cmd": "repeat", "mode": r.as_str()})) {
            self.repeat_echo.sent(Instant::now(), r);
        } else {
            self.next_gen("repeat");
            let mut q = Map::new();
            q.insert("state".into(), json!(r.as_str()));
            self.command("PUT", "repeat", q, None, Then::Nothing);
        }
        self.inv_main();
    }

    fn next_gen(&mut self, key: &'static str) {
        *self.cmd_gen.entry(key).or_default() += 1;
    }

    pub fn toggle_like(&mut self) {
        let Some(t) = self.pb.as_ref().and_then(|p| p.track.clone()) else { return };
        if !t.is_track() {
            return;
        }
        if self.liked.is_none() || self.liked_uri.as_deref() != Some(&t.uri) {
            self.flash("Still checking whether this track is liked...", false);
            self.liked_uri = None;
            self.track_changed(Some(t));
            return;
        }
        let new = !self.liked.unwrap_or(false);
        self.liked = Some(new);
        let q = json!({"uris": format!("spotify:track:{}", t.id())});
        self.request(if new { "PUT" } else { "DELETE" }, "me/library", q, None, Req::SetLiked { track: t, new });
    }

    // ── Choosing what to play ────────────────────────────────────────────────
    pub fn selected(&self) -> Option<Rc<Track>> {
        let l = self.lists.get(&self.cur)?;
        l.tracks.get(l.sel).cloned()
    }

    pub fn play_selected(&mut self) {
        let can_play_here = self.engine_can_play();
        let Some(l) = self.lists.get(&self.cur) else { return };
        let t = l.tracks.get(l.sel).cloned();
        let (kind, ctx_uri) = (l.kind, l.context_uri.clone());
        if t.is_none() && !(kind == Kind::Playlist && !l.loading) {
            return;
        }
        if t.as_ref().is_some_and(|t| !t.playable) {
            self.flash("This track can't be played (local file or not available)", true);
            return;
        }
        let eng = self.engine.device_id.clone();
        // Whichever device Spotify says is active gets the song, paused or not, as in
        // Spotify's own apps. Our own player takes over only when none is.
        let remote = self.pb.as_ref().is_some_and(|p| !self.local && p.device_id.is_some() && p.device_id != eng);
        if !remote && can_play_here {
            self.play_here(PlayReq { key: self.cur.clone(), uri: t.map(|t| t.uri.clone()) });
            return;
        }
        let shuffle = self.pb.as_ref().is_some_and(|p| p.shuffle);
        let body = match (&t, kind) {
            (None, _) => json!({"context_uri": ctx_uri}),
            (Some(t), Kind::Playlist) => {
                // Start from the track itself, not its position: with shuffle on, Spotify
                // applies a position to the shuffled order. Only a track that's in the
                // playlist twice needs its position (which one you picked).
                let twice = l.tracks.iter().filter(|x| x.uri == t.uri).count() > 1;
                let off = if twice && !shuffle { json!({"position": t.pos}) } else { json!({"uri": t.uri}) };
                json!({"context_uri": ctx_uri, "offset": off})
            }
            (Some(t), Kind::Liked) if self.user_id.is_some() => {
                json!({"context_uri": format!("spotify:user:{}:collection", self.user_id.as_deref().unwrap_or("")), "offset": {"uri": t.uri}})
            }
            (Some(_), _) => {
                let uris: Vec<&str> = l.tracks[l.sel..].iter().filter(|x| x.playable).take(100).map(|x| x.uri.as_str()).collect();
                json!({"uris": uris})
            }
        };
        let now = Instant::now();
        if let Some(t) = &t {
            match &mut self.pb {
                Some(pb) => {
                    pb.track = Some(t.clone());
                    pb.progress_ms = 0;
                    pb.at = now;
                    pb.playing = true;
                }
                None => {
                    self.pb = Some(Playback { track: Some(t.clone()), playing: true, progress_ms: 0, at: now, device_id: None,
                                              device_name: String::new(), volume: None, shuffle: false, repeat: Repeat::Off, context_uri: None });
                }
            }
            self.track_changed(Some(t.clone()));
            self.remote_expect = Some(RemoteExpect { want: Some(t.uri.clone()), want_name: t.name.clone(), avoid: None, until: now + Duration::from_secs(8) });
        }
        self.seek_target = None;
        self.seek_deadline = None;
        self.command("PUT", "play", Map::new(), Some(body), Then::RemotePlay);
        self.inv_all();
    }

    /// Play on our own engine: one line down a pipe, no Web API call.
    fn play_here(&mut self, req: PlayReq) {
        if !self.engine.ready {
            let now = Instant::now();
            self.pending_play = Some((req, now + Duration::from_secs(20)));
            self.flash("Starting SpoTerm's player...", false);
            return;
        }
        let Some(l) = self.lists.get(&req.key) else { return };
        let t = req.uri.as_ref().and_then(|u| l.tracks.iter().find(|x| &x.uri == u).cloned());
        let now = Instant::now();
        let (shuffle, repeat) = self.pb.as_ref().map_or((false, Repeat::Off), |p| (p.shuffle, p.repeat));
        let load = if l.kind == Kind::Playlist {
            json!({"cmd": "play_context", "uri": l.context_uri, "track_uri": t.as_ref().map(|t| t.uri.clone())})
        } else {
            // Liked Songs and search results play as a track list, so the queue runs
            // through everything loaded and shuffle covers all of it.
            let uris: Vec<&str> = l.tracks.iter().filter(|x| x.playable).map(|x| x.uri.as_str()).collect();
            json!({"cmd": "play_tracks", "uris": uris, "start_uri": t.as_ref().map(|t| t.uri.clone())})
        };
        let retry = (l.kind == Kind::Playlist).then(|| l.context_uri.clone()).flatten();
        self.local = true;
        self.local_grace_until = Some(now + Duration::from_secs(8));
        let pb = self.local_pb(now);
        pb.shuffle = shuffle;
        pb.repeat = repeat;
        if let Some(t) = &t {
            pb.track = Some(t.clone());
            pb.progress_ms = 0;
            pb.at = now;
            pb.playing = true;
        }
        self.play_echo.clear();
        self.seek_echo.clear();
        self.seek_target = None;
        self.seek_deadline = None;
        self.held_track = None;
        self.held_state = None;
        self.expect_retry = None;
        if let Some(t) = &t {
            self.track_changed(Some(t.clone()));
            self.expect_uri = Some(t.uri.clone());
            self.expect_name = t.name.clone();
            self.expect_until = Some(now + Duration::from_secs(5));
            if let Some(ctx) = retry {
                self.expect_retry = Some((ctx, t.uri.clone()));
            }
        } else {
            self.expect_uri = None;
        }
        self.last_load = Some(load.clone());
        self.reload_needed = false;
        let mut cmd = load;
        cmd["shuffle"] = json!(shuffle);
        cmd["repeat"] = json!(repeat.as_str());
        if !self.send(cmd) {
            self.local = false;
            self.expect_uri = None;
            self.expect_retry = None;
            self.flash("SpoTerm's player isn't running", true);
        }
        self.inv_all();
    }

    // ── Devices ──────────────────────────────────────────────────────────────
    pub fn fetch_devices(&mut self) {
        self.devices_wanted = true;
        self.request("GET", "me/player/devices", json!({}), None, Req::Devices);
    }

    pub fn transfer(&mut self, idx: usize) {
        let Some(d) = self.devices.get(idx) else { return };
        let play = !self.pb.as_ref().is_some_and(|p| !p.playing);
        let (id, name) = (d.id.clone(), d.name.clone());
        self.cmd_seq += 1;
        self.flash(format!("{} on {name}", if play { "Playing" } else { "Switched" }), false);
        self.request("PUT", "me/player", json!({}), Some(json!({"device_ids": [id], "play": play})), Req::Cmd { then: Then::Transfer });
    }

    // ── Library ──────────────────────────────────────────────────────────────
    pub fn load_playlists(&mut self) {
        self.playlists_acc.clear();
        self.request("GET", "me/playlists", json!({"limit": sp::PAGE_SIZE}), None, Req::Playlists);
    }

    pub fn cur_list(&self) -> &TrackList {
        self.lists.get(&self.cur).or_else(|| self.lists.get("liked")).expect("liked always exists")
    }

    pub fn cur_list_mut(&mut self) -> &mut TrackList {
        let key = if self.lists.contains_key(&self.cur) { self.cur.clone() } else { "liked".into() };
        self.lists.get_mut(&key).expect("liked always exists")
    }

    pub fn open_playlist(&mut self, idx: usize) {
        let Some(p) = self.playlists.get(idx) else { return };
        let key = format!("pl:{}", p.id);
        if !self.lists.contains_key(&key) {
            let mut l = TrackList::new(&key, Kind::Playlist, &p.name);
            l.context_uri = Some(p.uri.clone());
            l.playlist_id = Some(p.id.clone());
            self.lists.insert(key.clone(), l);
        }
        self.open_list(&key);
    }

    pub fn open_list(&mut self, key: &str) {
        if !self.lists.contains_key(key) {
            return;
        }
        self.cur = key.to_string();
        self.typing = false;
        let l = self.cur_list_mut();
        if l.stale {
            l.reset();
        }
        if l.kind != Kind::Search || !l.query.is_empty() {
            self.load_more(key.to_string());
        }
        self.show_playlist();
        self.inv_pl();
    }

    pub fn show_playlist(&mut self) {
        if !self.ui.pl_open {
            crate::toggle_playlist(self);
        }
    }

    pub fn load_more(&mut self, key: String) {
        let Some(l) = self.lists.get_mut(&key) else { return };
        let Some(off) = l.next_offset else { return };
        if l.loading || l.retry_at.is_some_and(|t| Instant::now() < t) {
            return;
        }
        l.loading = true;
        let gen = l.gen;
        let (path, q) = match l.kind {
            Kind::Liked => ("me/tracks".to_string(), json!({"limit": sp::PAGE_SIZE, "offset": off, "market": "from_token"})),
            Kind::Playlist => (
                format!("playlists/{}/items", l.playlist_id.as_deref().unwrap_or("")),
                json!({"limit": sp::PLAYLIST_PAGE_SIZE, "offset": off, "fields": sp::playlist_fields(),
                       "additional_types": "track,episode", "market": "from_token"}),
            ),
            Kind::Search => ("search".to_string(), json!({"q": l.query, "type": "track", "limit": sp::SEARCH_PAGE_SIZE, "offset": off})),
        };
        self.request("GET", &path, q, None, Req::Page { key, gen });
    }

    fn on_page(&mut self, key: &str, gen: u32, res: Result<Value, ApiError>) {
        let Some(l) = self.lists.get_mut(key) else { return };
        if l.gen != gen {
            return;
        }
        l.loading = false;
        match res {
            Ok(v) => {
                let off = l.next_offset.unwrap_or(0);
                let page = match l.kind {
                    Kind::Liked => sp::page(&v, off, |i| &i["track"]),
                    Kind::Playlist => sp::page(&v, off, |i| if i["item"].is_object() { &i["item"] } else { &i["track"] }),
                    Kind::Search => sp::page(&v["tracks"], off, |i| i),
                };
                l.error.clear();
                l.retry_at = None;
                l.tracks.extend(page.tracks.into_iter().map(Rc::new));
                l.total = page.total;
                l.next_offset = page.next_offset;
            }
            Err(e) => {
                if l.kind == Kind::Playlist && e.status == 403 && l.tracks.is_empty() {
                    l.locked = true;
                    l.next_offset = None;
                } else if !l.tracks.is_empty() && matches!(e.status, 400 | 404) {
                    l.next_offset = None; // past what Spotify will page through (search cap)
                } else {
                    l.error = e.describe();
                    l.retry_at = Some(Instant::now() + Duration::from_secs(5));
                    if !l.tracks.is_empty() {
                        let msg = format!("Couldn't load more: {}", l.error);
                        self.flash(msg, true);
                    }
                }
            }
        }
        self.prefetch();
        self.inv_pl();
    }

    /// Load the next page when the view gets near the end of what's loaded.
    pub fn prefetch(&mut self) {
        let rows = crate::view::pl_rows(self);
        let l = self.cur_list();
        let near_end = l.sel + PREFETCH_ROWS >= l.tracks.len() || l.top + rows + PREFETCH_ROWS >= l.tracks.len();
        if l.next_offset.is_some() && near_end {
            let key = l.key.clone();
            self.load_more(key);
        }
    }

    pub fn reload(&mut self) {
        if !self.engine.running() {
            self.restarts.clear();
            self.start_engine();
            return;
        }
        if self.engine.state == "retrying" && self.engine.reconnect() {
            self.player_warned = false;
            self.flash("Retrying SpoTerm's player...", false);
        }
        self.load_playlists();
        let key = self.cur.clone();
        for (k, l) in self.lists.iter_mut() {
            if *k != key {
                l.stale = true;
            }
        }
        let l = self.cur_list_mut();
        if l.kind != Kind::Search || !l.query.is_empty() {
            let (sel, top) = (l.sel, l.top);
            l.reset();
            l.sel = sel;
            l.top = top;
            self.load_more(key);
        }
        self.poll_soon(0, Duration::from_millis(100));
        self.flash("Reloading", false);
    }

    pub fn start_search(&mut self) {
        self.cur = "search".into();
        self.eat_char = !self.typing;
        self.typing = true;
        self.query = self.cur_list().query.clone();
        self.show_playlist();
        self.inv_pl();
    }

    pub fn submit_search(&mut self) {
        self.typing = false;
        let q = self.query.trim().to_string();
        if q.is_empty() {
            self.inv_pl();
            return;
        }
        let l = self.lists.get_mut("search").expect("search always exists");
        l.query = q.clone();
        l.title = format!("Search: {q}");
        l.reset();
        self.load_more("search".into());
        self.inv_pl();
    }

    /// Move the playlist selection, keeping it in view.
    pub fn move_sel(&mut self, delta: i64) {
        let rows = crate::view::pl_rows(self).max(1);
        let l = self.cur_list_mut();
        if l.tracks.is_empty() {
            return;
        }
        let n = l.tracks.len() as i64;
        l.sel = (l.sel as i64 + delta).clamp(0, n - 1) as usize;
        if l.sel < l.top {
            l.top = l.sel;
        } else if l.sel >= l.top + rows {
            l.top = l.sel + 1 - rows;
        }
        self.prefetch();
        self.inv_pl();
    }

    pub fn scroll(&mut self, delta: i64) {
        let rows = crate::view::pl_rows(self).max(1);
        let l = self.cur_list_mut();
        let max_top = l.tracks.len().saturating_sub(rows) as i64;
        l.top = (l.top as i64 + delta).clamp(0, max_top.max(0)) as usize;
        self.prefetch();
        self.inv_pl();
    }

    // ── What the marquee says ────────────────────────────────────────────────
    pub fn marquee_text(&self) -> String {
        if !self.status.is_empty() {
            return self.status.clone();
        }
        if let Hold::Seek(pos) = self.hold {
            if let Some(t) = self.pb.as_ref().and_then(|p| p.track.as_ref()) {
                let pct = if t.duration_ms > 0 { pos as u64 * 100 / t.duration_ms as u64 } else { 0 };
                return format!("Seek to: {}/{} ({pct}%)", sp::fmt_time(pos), sp::fmt_time(t.duration_ms));
            }
        }
        match self.pb.as_ref().and_then(|p| p.track.as_ref()) {
            Some(t) => {
                let liked = if self.liked == Some(true) && self.liked_uri.as_deref() == Some(&t.uri) { " *" } else { "" };
                if t.duration_ms > 0 {
                    format!("{} ({}){liked}", t.label(), sp::fmt_time(t.duration_ms))
                } else {
                    format!("{}{liked}", t.label())
                }
            }
            None => match self.phase {
                Phase::Running if self.engine.ready => format!("{} ready", self.s.device_name),
                Phase::Running if !self.engine.alive => "Starting...".into(),
                Phase::Running if self.engine.state == "retrying" => "Player offline, retrying".into(),
                _ => "SpoTermAmp".into(),
            },
        }
    }

    pub fn save(&mut self) {
        crate::remember_positions(self);
        config::save_state(&self.ui);
    }
}

/// Invalidate a logical (1x) rectangle of a window.
pub fn invalidate_rect(h: HWND, x: i32, y: i32, w: i32, hgt: i32, scale: i32) {
    let r = RECT { left: x * scale, top: y * scale, right: (x + w) * scale, bottom: (y + hgt) * scale };
    unsafe { InvalidateRect(h, &r, 0) };
}
