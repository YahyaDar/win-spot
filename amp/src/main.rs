//! SpoTermAmp: SpoTerm's second face, a classic Winamp-skinned window.
//!
//! The same `spoterm-engine` that the terminal UI drives plays the audio and makes the
//! Web API calls; this process is only a window. It is plain Win32 and GDI with no UI
//! framework: two popup windows drawn from the skin's bitmaps at the window's real pixel
//! size (any zoom, following each monitor's DPI), one timer that is only armed when
//! something is due, and an engine pipe read by one blocking thread.

#![windows_subsystem = "windows"]

mod app;
mod config;
mod crash;
mod engine;
mod gfx;
mod input;
mod skin;
mod spotify;
mod view;

use std::ptr::{null, null_mut};
use std::time::Instant;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForMonitor, GetDpiForSystem, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use app::{App, Hold, Phase};
use engine::{Engine, WM_ENGINE, WM_TASK};
use gfx::{logical, phys, wide_z, Surface, MAX_PCT, MIN_PCT};
use spotify::Repeat;
use view::*;

const MAIN_CLASS: &str = "SpoTermAmpMain";
const PL_CLASS: &str = "SpoTermAmpPlaylist";
const DASHBOARD: &str = "https://developer.spotify.com/dashboard";

// Menu commands.
const M_LIKED: usize = 1000;
const M_SEARCH: usize = 1001;
const M_RELOAD: usize = 1002;
const M_PLAYLIST: usize = 1100; // + index
const M_DEVICES: usize = 3000;
const M_DEVICE: usize = 3100; // + index
const M_LIKE: usize = 4000;
const M_SHUFFLE: usize = 4001;
const M_REPEAT: usize = 4010; // + 0 off, 1 all, 2 one
const M_PLWIN: usize = 4020;
const M_DOUBLE: usize = 4021;
const M_ONTOP: usize = 4022;
const M_SCROLL: usize = 4023;
const M_REMAINING: usize = 4024;
const M_SHADE: usize = 4025;
const M_RETRY: usize = 4030;
const M_SIGNIN: usize = 4031;
const M_SIGNIN_PLAYER: usize = 4032;
const M_ABOUT: usize = 4040;
const M_EXIT: usize = 4041;
const M_FOLDER: usize = 4050;
const M_PLAY: usize = 4060;
const M_PAUSE: usize = 4061;
const M_STOP: usize = 4062;
const M_NEXT: usize = 4063;
const M_PREV: usize = 4064;
const M_UNLIKE_SEL: usize = 4070;
const M_JUMP: usize = 4071;
const M_FIRST: usize = 4072;
const M_LAST: usize = 4073;
const M_INFO: usize = 4074;
const M_ZOOM_AUTO: usize = 4080;
const M_ZOOM: usize = 4081; // + index into ZOOMS
const M_ZOOM_IN: usize = 4090;
const M_ZOOM_OUT: usize = 4091;

/// The sizes offered, in percent of the skin's own.
const ZOOMS: [i32; 7] = [100, 125, 150, 175, 200, 250, 300];

// The app lives on the UI thread for the whole run; window procedures reach it here.
static mut APP: *mut App = null_mut();

fn app() -> Option<&'static mut App> {
    unsafe { APP.as_mut() }
}

fn main() {
    crash::install();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Hidden flag for automated verification of panic hook / crash log recording.
    if args.first().map(String::as_str) == Some("--crash-test") {
        panic!("crash test (deliberate)");
    }
    if args.first().map(String::as_str) == Some("--snapshot") {
        std::process::exit(snapshot(args.get(1).map_or(".", String::as_str)));
    }
    if args.first().is_some_and(|a| a == "--version" || a == "-V") {
        message(&format!("SpoTermAmp {}", env!("CARGO_PKG_VERSION")), false);
        return;
    }
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // One window at a time: a second launch brings the first one forward.
        CreateMutexW(null(), 0, wide_z("SpoTermAmp.single").as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let h = FindWindowW(wide_z(MAIN_CLASS).as_ptr(), null());
            if !h.is_null() {
                ShowWindow(h, SW_RESTORE);
                SetForegroundWindow(h);
            }
            return;
        }
    }

    let mut s = config::load();
    if !config::valid_client_id(&s.client_id) {
        match ask_client_id() {
            Some(cid) => {
                if let Err(e) = config::save_client_id(&cid) {
                    message(&format!("Couldn't save the Client ID: {e}"), true);
                    return;
                }
                s = config::load();
                if s.client_id != cid {
                    s.client_id = cid; // an invalid one elsewhere still wins the lookup
                }
            }
            None => return,
        }
    }
    let Some(bin) = config::find_engine(&s.engine_bin) else {
        message(&format!("{} is missing. Keep it next to SpoTermAmp.exe (or build it: cd engine && cargo build --release).", config::ENGINE_EXE), true);
        return;
    };
    let skin = match skin::Skin::load(skin::DEFAULT) {
        Ok(sk) => sk,
        Err(e) => {
            message(&format!("The skin couldn't be loaded: {e}"), true);
            return;
        }
    };

    let ui = config::load_state();
    let at = if ui.x == i32::MIN {
        POINT { x: 0, y: 0 }
    } else {
        POINT {
            x: ui.x + 10,
            y: ui.y + 10,
        }
    };
    let auto = auto_pct(monitor_dpi(at));
    let pct = if ui.scale > 0 { ui.scale } else { auto };
    let a = Box::new(App::new(s, ui, skin, Engine::new(bin), pct, auto));
    unsafe { APP = Box::into_raw(a) };

    if !create_windows() {
        message("Couldn't create the window.", true);
        return;
    }
    if let Some(a) = app() {
        a.begin();
        a.reschedule();
    }

    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if let Some(a) = app() {
            a.engine.cancel_sign_in();
            a.engine.stop();
        }
        // The engine stops with its pipe; everything else goes with the process.
    }
}

fn message(text: &str, error: bool) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide_z(text).as_ptr(),
            wide_z("SpoTermAmp").as_ptr(),
            if error {
                MB_ICONERROR
            } else {
                MB_ICONINFORMATION
            } | MB_OK,
        );
    }
}

// ── Windows ──────────────────────────────────────────────────────────────────
fn create_windows() -> bool {
    let Some(a) = app() else { return false };
    unsafe {
        let hinst = GetModuleHandleW(null());
        let icon = make_icon(a);
        let procs: [(&str, WNDPROC); 2] =
            [(MAIN_CLASS, Some(main_proc)), (PL_CLASS, Some(pl_proc))];
        for (name, proc_) in procs {
            let cls = wide_z(name);
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_DBLCLKS,
                lpfnWndProc: proc_,
                hInstance: hinst,
                hIcon: icon,
                hIconSm: icon,
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                lpszClassName: cls.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&wc);
        }
        let pct = a.pct;
        let (mut x, mut y) = (a.ui.x, a.ui.y);
        if x == i32::MIN || !on_screen(x, y) {
            x = (GetSystemMetrics(SM_CXSCREEN) - phys(MAIN_W, pct)) / 2;
            y = (GetSystemMetrics(SM_CYSCREEN) - phys(main_h(a) + a.ui.pl_h, pct)) / 3;
        }
        let ex = if a.ui.on_top { WS_EX_TOPMOST } else { 0 } | WS_EX_APPWINDOW;
        let hmain = CreateWindowExW(
            ex,
            wide_z(MAIN_CLASS).as_ptr(),
            wide_z("SpoTermAmp").as_ptr(),
            WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX,
            x,
            y,
            phys(MAIN_W, pct),
            phys(main_h(a), pct),
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        if hmain.is_null() {
            return false;
        }
        a.hmain = hmain;
        apply_region(a);
        let (px, py) = if a.ui.docked || !on_screen(a.ui.pl_x, a.ui.pl_y) {
            (x, y + phys(main_h(a), pct))
        } else {
            (a.ui.pl_x, a.ui.pl_y)
        };
        // Owned by the main window: no taskbar button of its own, minimised with it.
        let hpl = CreateWindowExW(
            if a.ui.on_top { WS_EX_TOPMOST } else { 0 },
            wide_z(PL_CLASS).as_ptr(),
            wide_z("SpoTermAmp Playlist").as_ptr(),
            WS_POPUP,
            px,
            py,
            phys(a.ui.pl_w, pct),
            phys(a.ui.pl_h, pct),
            hmain,
            null_mut(),
            hinst,
            null(),
        );
        a.hpl = hpl;
        ShowWindow(hmain, SW_SHOW);
        if a.ui.pl_open && !hpl.is_null() {
            ShowWindow(hpl, SW_SHOWNOACTIVATE);
        }
        SetForegroundWindow(hmain);
    }
    true
}

fn on_screen(x: i32, y: i32) -> bool {
    unsafe {
        let (vx, vy) = (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        );
        let (vw, vh) = (
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        );
        x >= vx - 100 && y >= vy - 10 && x < vx + vw - 50 && y < vy + vh - 30
    }
}

/// The window icon, cut from the skin (the logo in the bottom-right corner of main.bmp).
fn make_icon(a: &App) -> HICON {
    let Some(m) = a.skin.main.as_ref() else {
        return null_mut();
    };
    let (sx, sy) = (250, 89);
    let mut px = vec![0u32; 32 * 32];
    for y in 0..32 {
        for x in 0..32 {
            px[(y * 32 + x) as usize] = m.at(sx + x / 2, sy + y / 2).unwrap_or(0) | 0xFF00_0000;
        }
    }
    unsafe {
        let color = CreateBitmap(32, 32, 1, 32, px.as_ptr() as _);
        let mask = CreateBitmap(32, 32, 1, 1, vec![0u8; 32 * 4].as_ptr() as _);
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}

/// Shape the main window by the skin's region.txt (rounded corners).
fn apply_region(a: &App) {
    if a.skin.region.is_empty() || a.ui.shade {
        unsafe { SetWindowRgn(a.hmain, null_mut(), 1) };
        return;
    }
    let pct = a.pct;
    let mut pts = Vec::new();
    let mut counts = Vec::new();
    for poly in &a.skin.region {
        counts.push(poly.len() as i32);
        pts.extend(poly.iter().map(|&(x, y)| POINT {
            x: phys(x, pct),
            y: phys(y, pct),
        }));
    }
    unsafe {
        let rgn = CreatePolyPolygonRgn(pts.as_ptr(), counts.as_ptr(), counts.len() as i32, WINDING);
        if !rgn.is_null() && SetWindowRgn(a.hmain, rgn, 1) == 0 {
            DeleteObject(rgn);
        }
    }
}

fn window_rect(h: HWND) -> RECT {
    let mut r: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetWindowRect(h, &mut r) };
    r
}

/// Where the playlist sits when docked under the main window.
fn dock_pos(a: &App) -> (i32, i32) {
    let r = window_rect(a.hmain);
    (r.left, r.top + phys(main_h(a), a.pct))
}

pub fn remember_positions(a: &mut App) {
    unsafe {
        if IsIconic(a.hmain) != 0 {
            return;
        }
    }
    let r = window_rect(a.hmain);
    a.ui.x = r.left;
    a.ui.y = r.top;
    if !a.hpl.is_null() {
        let p = window_rect(a.hpl);
        a.ui.pl_x = p.left;
        a.ui.pl_y = p.top;
    }
}

pub fn toggle_playlist(a: &mut App) {
    a.ui.pl_open = !a.ui.pl_open;
    if a.hpl.is_null() {
        return;
    }
    unsafe {
        if a.ui.pl_open {
            if a.ui.docked {
                let (x, y) = dock_pos(a);
                SetWindowPos(
                    a.hpl,
                    null_mut(),
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            ShowWindow(a.hpl, SW_SHOWNOACTIVATE);
        } else {
            ShowWindow(a.hpl, SW_HIDE);
        }
    }
    a.inv_main();
}

/// What a monitor's DPI calls normal size, rounded to the nearest offered step.
fn auto_pct(dpi: u32) -> i32 {
    ((dpi as i32 * 100 / 96 + 12) / 25 * 25).clamp(MIN_PCT, MAX_PCT)
}

fn monitor_dpi(pt: POINT) -> u32 {
    unsafe {
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let (mut x, mut y) = (0u32, 0u32);
        if !mon.is_null() && GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y) == 0 && x > 0
        {
            x
        } else {
            GetDpiForSystem()
        }
    }
}

/// The size the user picked: a percentage, or 0 to follow the screen.
fn choose_zoom(a: &mut App, choice: i32) {
    a.ui.scale = if choice == 0 {
        0
    } else {
        choice.clamp(MIN_PCT, MAX_PCT)
    };
    let pct = if a.ui.scale == 0 {
        a.auto_pct
    } else {
        a.ui.scale
    };
    set_pct(a, pct);
    a.flash(format!("Size: {pct}%"), false);
}

/// One step larger or smaller.
fn zoom_step(a: &mut App, larger: bool) {
    let next = if larger {
        ZOOMS.iter().copied().find(|&z| z > a.pct)
    } else {
        ZOOMS.iter().rev().copied().find(|&z| z < a.pct)
    };
    if let Some(z) = next {
        choose_zoom(a, if z == a.auto_pct { 0 } else { z });
    }
}

/// Winamp's double size, relative to what the screen calls normal.
fn toggle_double(a: &mut App) {
    if a.doubled() {
        choose_zoom(a, 0);
    } else {
        choose_zoom(a, (a.auto_pct * 2).min(MAX_PCT));
    }
}

fn set_pct(a: &mut App, pct: i32) {
    let pct = pct.clamp(MIN_PCT, MAX_PCT);
    if pct == a.pct {
        return;
    }
    a.pct = pct;
    a.set_fonts();
    unsafe {
        SetWindowPos(
            a.hmain,
            null_mut(),
            0,
            0,
            phys(MAIN_W, pct),
            phys(main_h(a), pct),
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        apply_region(a);
        if !a.hpl.is_null() {
            let (w, h) = (phys(a.ui.pl_w, pct), phys(a.ui.pl_h, pct));
            if a.ui.docked {
                let (x, y) = dock_pos(a);
                SetWindowPos(a.hpl, null_mut(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            } else {
                SetWindowPos(
                    a.hpl,
                    null_mut(),
                    0,
                    0,
                    w,
                    h,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }
    a.inv_all();
}

/// Winamp's shade mode: the main window folds into a 14-pixel strip.
fn toggle_shade(a: &mut App) {
    a.ui.shade = !a.ui.shade;
    unsafe {
        SetWindowPos(
            a.hmain,
            null_mut(),
            0,
            0,
            phys(MAIN_W, a.pct),
            phys(main_h(a), a.pct),
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    apply_region(a);
    if a.ui.docked && a.ui.pl_open && !a.hpl.is_null() {
        let (x, y) = dock_pos(a);
        unsafe {
            SetWindowPos(
                a.hpl,
                null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }
    a.inv_main();
}

fn set_on_top(a: &mut App, on: bool) {
    a.ui.on_top = on;
    let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
    unsafe {
        SetWindowPos(
            a.hmain,
            after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        if !a.hpl.is_null() {
            SetWindowPos(
                a.hpl,
                after,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
    a.inv_main();
}

/// The taskbar shows the song, as Winamp's does.
fn update_title(a: &App) {
    thread_local!(static LAST: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) });
    let title = match a.pb.as_ref().and_then(|p| p.track.as_ref()) {
        Some(t) => format!("{} - SpoTermAmp", t.label()),
        None => "SpoTermAmp".into(),
    };
    LAST.with(|l| {
        if *l.borrow() != title {
            unsafe { SetWindowTextW(a.hmain, wide_z(&title).as_ptr()) };
            *l.borrow_mut() = title;
        }
    });
}

/// A mouse message's client position, in the skin's coordinates.
fn mouse(lp: LPARAM, pct: i32) -> (i32, i32) {
    let x = (lp & 0xFFFF) as i16 as i32;
    let y = ((lp >> 16) & 0xFFFF) as i16 as i32;
    (logical(x, pct), logical(y, pct))
}

fn screen_point(lp: LPARAM) -> POINT {
    POINT {
        x: (lp & 0xFFFF) as i16 as i32,
        y: ((lp >> 16) & 0xFFFF) as i16 as i32,
    }
}

fn screen_to_logical(h: HWND, lp: LPARAM, pct: i32) -> (i32, i32) {
    let mut p = screen_point(lp);
    unsafe { ScreenToClient(h, &mut p) };
    (logical(p.x, pct), logical(p.y, pct))
}

/// The mouse wheel goes to the window under the pointer (Windows sends it to the focused
/// one). Notches are accumulated, so touchpads' small steps add up instead of vanishing.
fn wheel(wp: WPARAM, lp: LPARAM) {
    thread_local!(static ACC: std::cell::Cell<(i32, i32, i32)> = const { std::cell::Cell::new((0, 0, 0)) });
    let Some(a) = app() else { return };
    let delta = ((wp >> 16) & 0xFFFF) as i16 as i32;
    let under = unsafe { GetAncestor(WindowFromPoint(screen_point(lp)), GA_ROOT) };
    let (mut zoom, mut vol, mut rows) = ACC.with(|c| c.get());
    if down(VK_CONTROL) {
        zoom += delta;
        while zoom.abs() >= 120 {
            zoom_step(a, zoom > 0);
            zoom -= 120 * zoom.signum();
        }
    } else if under == a.hpl && !a.hpl.is_null() {
        rows += delta;
        let n = rows / 40; // a notch is three rows
        rows -= n * 40;
        if n != 0 {
            a.scroll(-n as i64);
        }
    } else {
        vol += delta;
        let n = vol / 60; // a notch is 2%
        vol -= n * 60;
        if n != 0 {
            a.change_volume(n);
        }
    }
    ACC.with(|c| c.set((zoom, vol, rows)));
    if let Some(a) = app() {
        a.reschedule();
    }
}

fn paint(h: HWND, main: bool) {
    let Some(a) = app() else { return };
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(h, &mut ps);
        let (pct, r) = (a.pct, ps.rcPaint);
        if main {
            let mut sf = std::mem::replace(&mut a.main_sf, Surface::empty());
            sf.resize(MAIN_W, main_h(a), pct);
            draw_main(a, &mut sf);
            sf.present(hdc, &r);
            a.main_sf = sf;
            update_title(a);
        } else {
            let mut sf = std::mem::replace(&mut a.pl_sf, Surface::empty());
            if !sf.fits(a.ui.pl_w, a.ui.pl_h, pct) || a.pl_full.replace(false) {
                sf.resize(a.ui.pl_w, a.ui.pl_h, pct);
                draw_pl(a, &mut sf);
            } else {
                draw_pl_clock(a, &mut sf);
            }
            sf.present(hdc, &r);
            a.pl_sf = sf;
        }
        EndPaint(h, &ps);
    }
}

// ── Main window ──────────────────────────────────────────────────────────────
unsafe extern "system" fn main_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(a) = app() else {
        return DefWindowProcW(h, msg, wp, lp);
    };
    if a.hmain.is_null() {
        return DefWindowProcW(h, msg, wp, lp);
    }
    match msg {
        WM_PAINT => {
            paint(h, true);
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_ENGINE => {
            a.on_engine_events();
            after_events();
            return 0;
        }
        WM_TASK => {
            a.on_sign_in(wp == 1, lp != 0);
            a.reschedule();
            unsafe { SetForegroundWindow(h) };
            return 0;
        }
        WM_TIMER if wp == app::TIMER_ID => {
            a.on_timer();
            return 0;
        }
        WM_NCHITTEST => {
            let (x, y) = screen_to_logical(h, lp, a.pct);
            return if matches!(main_hit(a, x, y), Hit::Drag) {
                HTCAPTION as LRESULT
            } else {
                HTCLIENT as LRESULT
            };
        }
        WM_DPICHANGED => {
            // Moved to a monitor with another DPI: keep the place Windows suggests and, when
            // following the screen, take its size.
            let dpi = (wp & 0xFFFF) as u32;
            a.auto_pct = auto_pct(dpi);
            let r = &*(lp as *const RECT);
            SetWindowPos(
                h,
                null_mut(),
                r.left,
                r.top,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            if a.ui.scale == 0 {
                set_pct(a, a.auto_pct);
            }
            return 0;
        }
        WM_NCLBUTTONDBLCLK => {
            // Double-clicking the title bar folds the window, as in Winamp (never maximises).
            let (_, y) = screen_to_logical(h, lp, a.pct);
            if y < 14 {
                toggle_shade(a);
            }
            return 0;
        }
        WM_ACTIVATE => {
            a.main_active = (wp & 0xFFFF) as u32 != WA_INACTIVE;
            a.inv_main();
        }
        WM_MOVE => {
            if a.ui.docked && a.ui.pl_open && !a.hpl.is_null() {
                let (x, y) = dock_pos(a);
                SetWindowPos(
                    a.hpl,
                    null_mut(),
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        WM_SIZE => {
            a.reschedule(); // minimised: no clock ticks or marquee steps
            if wp as u32 == SIZE_RESTORED {
                a.inv_all();
            }
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let (x, y) = mouse(lp, a.pct);
            match main_hit(a, x, y) {
                Hit::Button(id) => {
                    a.hold = Hold::Button(id);
                    a.hold_inside = true;
                }
                Hit::Volume => {
                    // Grabbing the thumb keeps it under the pointer; elsewhere it jumps there.
                    let tx = vol_thumb_x(a);
                    a.grab = if (tx..tx + 14).contains(&x) {
                        x - tx
                    } else {
                        7
                    };
                    a.hold = Hold::Volume;
                    let v = volume_at(x - a.grab + 7);
                    if a.volume() != Some(v) {
                        a.set_volume(v);
                    }
                }
                Hit::Seek => {
                    if let Some(d) =
                        a.pb.as_ref()
                            .and_then(|p| p.track.as_ref())
                            .map(|t| t.duration_ms)
                    {
                        a.grab = match seek_thumb_x(a) {
                            _ if a.ui.shade => 1,
                            Some(tx) if (tx..tx + 29).contains(&x) => x - tx,
                            _ => 14,
                        };
                        let at = if a.ui.shade { x } else { x - a.grab + 14 };
                        a.hold = Hold::Seek(seek_at(a, at, d));
                    }
                }
                _ => {}
            }
            if a.hold != Hold::None {
                SetCapture(h);
                a.inv_main();
            }
            return 0;
        }
        WM_MOUSEMOVE => {
            let (x, y) = mouse(lp, a.pct);
            match a.hold {
                Hold::Button(id) => {
                    // A held button looks pressed only while the pointer is on it.
                    let inside = matches!(main_hit(a, x, y), Hit::Button(b) if b == id);
                    if inside != a.hold_inside {
                        a.hold_inside = inside;
                        a.inv_main();
                    }
                }
                Hold::Volume => {
                    let v = volume_at(x - a.grab + 7);
                    if a.volume() != Some(v) {
                        a.set_volume(v);
                        a.reschedule();
                    }
                }
                Hold::Seek(old) => {
                    if let Some(d) =
                        a.pb.as_ref()
                            .and_then(|p| p.track.as_ref())
                            .map(|t| t.duration_ms)
                    {
                        let at = if a.ui.shade { x } else { x - a.grab + 14 };
                        let pos = seek_at(a, at, d);
                        if pos != old {
                            a.hold = Hold::Seek(pos);
                            a.inv_all();
                        }
                    }
                }
                _ => {}
            }
            return 0;
        }
        WM_LBUTTONUP => {
            let (x, y) = mouse(lp, a.pct);
            let hold = std::mem::replace(&mut a.hold, Hold::None);
            ReleaseCapture();
            match hold {
                Hold::Button(id) if matches!(main_hit(a, x, y), Hit::Button(b) if b == id) => {
                    main_button(id)
                }
                Hold::Seek(pos) => a.seek_to(pos),
                _ => {}
            }
            if let Some(a) = app() {
                a.inv_all();
                a.reschedule();
            }
            return 0;
        }
        WM_CAPTURECHANGED => {
            if matches!(a.hold, Hold::Button(_) | Hold::Volume | Hold::Seek(_)) {
                a.hold = Hold::None;
                a.inv_main();
            }
        }
        WM_RBUTTONUP | WM_NCRBUTTONUP => {
            main_menu();
            return 0;
        }
        WM_CONTEXTMENU => return 0,
        WM_MOUSEWHEEL => {
            wheel(wp, lp);
            return 0;
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if key(wp as u16, false, msg == WM_SYSKEYDOWN) {
                return 0;
            }
        }
        WM_CHAR => {
            char_in(wp as u32);
            return 0;
        }
        WM_CLOSE => {
            a.save();
            DestroyWindow(h);
            return 0;
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(h, msg, wp, lp)
}

/// Work that needs a menu or a message box, after the state has settled.
fn after_events() {
    let Some(a) = app() else { return };
    update_title(a);
    if a.devices_wanted && !a.devices.is_empty() {
        a.devices_wanted = false;
        devices_menu();
    }
}

fn main_button(id: u32) {
    let Some(a) = app() else { return };
    match id {
        B_PREV => a.skip(false),
        B_PLAY => a.play(),
        B_PAUSE => a.toggle_play(),
        B_STOP => a.stop(),
        B_NEXT => a.skip(true),
        B_EJECT => library_menu(),
        B_SHUFFLE => a.toggle_shuffle(),
        B_REPEAT => a.cycle_repeat(),
        B_EQ => a.flash("No equalizer: Spotify's player has none", false),
        B_PL => toggle_playlist(a),
        B_OPTIONS | B_CL_O => main_menu(),
        B_MIN => unsafe {
            ShowWindow(a.hmain, SW_MINIMIZE);
        },
        B_SHADE => toggle_shade(a),
        B_CLOSE => unsafe {
            PostMessageW(a.hmain, WM_CLOSE, 0, 0);
        },
        B_CL_A => set_on_top(a, !a.ui.on_top),
        B_CL_I => song_info(),
        B_CL_D => toggle_double(a),
        B_CL_V => a.flash("No visualizer: the engine doesn't share audio", false),
        B_TIME => {
            a.ui.remaining = !a.ui.remaining;
            a.inv_all();
        }
        B_LOGO => about(),
        _ => {}
    }
}

// ── Playlist window ──────────────────────────────────────────────────────────
unsafe extern "system" fn pl_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(a) = app() else {
        return DefWindowProcW(h, msg, wp, lp);
    };
    if a.hpl.is_null() {
        return DefWindowProcW(h, msg, wp, lp);
    }
    let pct = a.pct;
    match msg {
        WM_PAINT => {
            paint(h, false);
            return 0;
        }
        WM_DPICHANGED => return 0, // the main window decides the size
        WM_ERASEBKGND => return 1,
        WM_NCHITTEST => {
            let (x, y) = screen_to_logical(h, lp, pct);
            return match pl_hit(a, x, y) {
                Hit::Drag => HTCAPTION as LRESULT,
                Hit::Resize => HTBOTTOMRIGHT as LRESULT,
                _ => HTCLIENT as LRESULT,
            };
        }
        WM_NCLBUTTONDBLCLK => return 0,
        WM_ACTIVATE => {
            a.pl_active = (wp & 0xFFFF) as u32 != WA_INACTIVE;
            a.inv_pl();
        }
        WM_GETMINMAXINFO => {
            let mm = &mut *(lp as *mut MINMAXINFO);
            mm.ptMinTrackSize = POINT {
                x: phys(275, pct),
                y: phys(116, pct),
            };
            return 0;
        }
        WM_SIZING => {
            // Playlists grow in steps of 25x29, the size of the skin's tiles.
            let r = &mut *(lp as *mut RECT);
            let w = (logical(r.right - r.left, pct) - 275 + 12)
                .div_euclid(25)
                .max(0)
                * 25
                + 275;
            let hgt = (logical(r.bottom - r.top, pct) - 116 + 14)
                .div_euclid(29)
                .max(0)
                * 29
                + 116;
            r.right = r.left + phys(w, pct);
            r.bottom = r.top + phys(hgt, pct);
            return 1;
        }
        WM_SIZE => {
            let (w, hgt) = ((lp & 0xFFFF) as i32, ((lp >> 16) & 0xFFFF) as i32);
            if w > 0 && hgt > 0 {
                a.ui.pl_w = logical(w, pct).max(275);
                a.ui.pl_h = logical(hgt, pct).max(116);
                a.scroll(0);
                a.prefetch();
                a.reschedule();
                a.inv_pl();
            }
        }
        WM_MOVING => {
            // Snap to the bottom of the main window within 10 pixels, and dock there.
            let r = &mut *(lp as *mut RECT);
            let (dx, dy) = dock_pos(a);
            let snap = phys(10, pct);
            if (r.left - dx).abs() < snap && (r.top - dy).abs() < snap {
                let (w, hgt) = (r.right - r.left, r.bottom - r.top);
                *r = RECT {
                    left: dx,
                    top: dy,
                    right: dx + w,
                    bottom: dy + hgt,
                };
            }
            return 1;
        }
        WM_EXITSIZEMOVE => {
            let r = window_rect(h);
            let (dx, dy) = dock_pos(a);
            a.ui.docked = r.left == dx && r.top == dy;
        }
        WM_LBUTTONDOWN => {
            let (x, y) = mouse(lp, pct);
            match pl_hit(a, x, y) {
                Hit::Button(id) => {
                    a.hold = Hold::PlButton(id);
                    a.hold_inside = true;
                    SetCapture(h);
                }
                Hit::Row(i) => {
                    let l = a.cur_list_mut();
                    if i < l.tracks.len() {
                        l.sel = i;
                    }
                    a.prefetch();
                }
                Hit::Scroll => {
                    let (hy, travel) = scroll_geometry(a);
                    if y >= hy && y < hy + 18 {
                        a.hold = Hold::PlScroll(y - hy);
                        SetCapture(h);
                    } else if travel > 0 {
                        let rows = pl_rows(a) as i64;
                        a.scroll(if y < hy { -rows } else { rows });
                    }
                }
                _ => {}
            }
            a.inv_pl();
            a.reschedule();
            return 0;
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = mouse(lp, pct);
            match pl_hit(a, x, y) {
                Hit::Row(i) if i < a.cur_list().tracks.len() => {
                    a.cur_list_mut().sel = i;
                    a.play_selected();
                }
                Hit::Row(_) if a.cur_list().locked => a.play_selected(),
                Hit::Button(_) => return pl_proc(h, WM_LBUTTONDOWN, wp, lp),
                _ => {}
            }
            a.reschedule();
            return 0;
        }
        WM_MOUSEMOVE => {
            if let Hold::PlButton(id) = a.hold {
                let (x, y) = mouse(lp, pct);
                let inside = matches!(pl_hit(a, x, y), Hit::Button(b) if b == id);
                if inside != a.hold_inside {
                    a.hold_inside = inside;
                    a.inv_pl();
                }
            }
            if let Hold::PlScroll(grab) = a.hold {
                let (_, y) = mouse(lp, pct);
                let (_, travel) = scroll_geometry(a);
                let rows = pl_rows(a);
                let l = a.cur_list_mut();
                let max_top = l.tracks.len().saturating_sub(rows);
                if travel > 0 {
                    l.top = ((y - grab - 20).clamp(0, travel) as i64 * max_top as i64
                        / travel as i64) as usize;
                }
                a.prefetch();
                a.inv_pl();
                a.reschedule();
            }
            return 0;
        }
        WM_LBUTTONUP => {
            let (x, y) = mouse(lp, pct);
            let hold = std::mem::replace(&mut a.hold, Hold::None);
            ReleaseCapture();
            if let Hold::PlButton(id) = hold {
                if matches!(pl_hit(a, x, y), Hit::Button(b) if b == id) {
                    pl_button(id);
                }
            }
            if let Some(a) = app() {
                a.inv_all();
                a.reschedule();
            }
            return 0;
        }
        WM_CAPTURECHANGED => {
            if matches!(a.hold, Hold::PlButton(_) | Hold::PlScroll(_)) {
                a.hold = Hold::None;
                a.inv_pl();
            }
        }
        WM_MOUSEWHEEL => {
            wheel(wp, lp);
            return 0;
        }
        WM_RBUTTONUP | WM_NCRBUTTONUP => {
            library_menu();
            return 0;
        }
        WM_CONTEXTMENU => return 0,
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if key(wp as u16, true, msg == WM_SYSKEYDOWN) {
                return 0;
            }
        }
        WM_CHAR => {
            char_in(wp as u32);
            return 0;
        }
        WM_CLOSE => {
            toggle_playlist(a);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(h, msg, wp, lp)
}

fn pl_button(id: u32) {
    let Some(a) = app() else { return };
    match id {
        P_CLOSE => toggle_playlist(a),
        P_SHADE => a.flash("Shade mode isn't in this version yet", false),
        P_LIST | P_EJECT => library_menu(),
        P_ADD => {
            let cmd = popup(|a, m| {
                let has =
                    a.pb.as_ref()
                        .and_then(|p| p.track.as_ref())
                        .is_some_and(|t| t.is_track());
                add(m, M_SEARCH, "&Search Spotify...\tJ", false, true);
                add(
                    m,
                    M_LIKE,
                    "&Add the playing song to Liked Songs\tF",
                    false,
                    has && a.liked == Some(false),
                );
                sep(m);
                library_items(a, m);
            });
            run_command(cmd);
        }
        P_REM => {
            let cmd = popup(|a, m| {
                let sel = a.cur == "liked" && a.selected().is_some();
                add(
                    m,
                    M_UNLIKE_SEL,
                    "Remove the &selected song from Liked Songs",
                    false,
                    sel,
                );
                add(
                    m,
                    M_LIKE,
                    "Remove the &playing song from Liked Songs\tF",
                    false,
                    a.liked == Some(true),
                );
            });
            run_command(cmd);
        }
        P_SEL => {
            let cmd = popup(|a, m| {
                add(
                    m,
                    M_JUMP,
                    "&Jump to the playing song",
                    false,
                    a.pb.as_ref().is_some_and(|p| p.track.is_some()),
                );
                add(m, M_FIRST, "&First song\tHome", false, true);
                add(m, M_LAST, "&Last song\tEnd", false, true);
            });
            run_command(cmd);
        }
        P_MISC => {
            let cmd = popup(|_, m| {
                add(m, M_RELOAD, "&Reload\tF5", false, true);
                add(m, M_INFO, "Song &info", false, true);
            });
            run_command(cmd);
        }
        P_PREV => a.skip(false),
        P_PLAY => a.play(),
        P_PAUSE => a.toggle_play(),
        P_STOP => a.stop(),
        P_NEXT => a.skip(true),
        _ => {}
    }
}

// ── Keyboard ─────────────────────────────────────────────────────────────────
fn down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk as i32) < 0 }
}

/// A key press in either window. Winamp's own keys (z x c v b, arrows) plus SpoTerm's.
fn key(vk: u16, in_pl: bool, alt: bool) -> bool {
    let Some(a) = app() else { return false };
    let ctrl = down(VK_CONTROL);
    if a.typing {
        match input::typing_key_action(vk, ctrl) {
            input::TypingKeyAction::Submit => {
                reset_input_decoder();
                a.submit_search();
            }
            input::TypingKeyAction::Cancel => {
                reset_input_decoder();
                a.typing = false;
                a.inv_pl();
            }
            input::TypingKeyAction::Backspace => {
                reset_input_decoder();
                a.query.pop();
                a.inv_pl();
            }
            input::TypingKeyAction::Clear => {
                reset_input_decoder();
                a.query.clear();
                a.inv_pl();
            }
            input::TypingKeyAction::Paste => {
                reset_input_decoder();
                if let Some(t) = clipboard_text() {
                    spotify::append_capped(
                        &mut a.query,
                        &spotify::clean(&t),
                        spotify::MAX_QUERY_CHARS,
                    );
                    a.inv_pl();
                }
            }
            input::TypingKeyAction::Char => return false,
        }
        a.reschedule();
        return true;
    }
    if alt {
        if vk == b'E' as u16 {
            toggle_playlist(a);
            return true;
        }
        return false;
    }
    let rows = pl_rows(a) as i64;
    match vk {
        _ if ctrl && vk == b'D' as u16 => toggle_double(a),
        _ if ctrl && matches!(vk, VK_OEM_PLUS | VK_ADD) => zoom_step(a, true),
        _ if ctrl && matches!(vk, VK_OEM_MINUS | VK_SUBTRACT) => zoom_step(a, false),
        _ if ctrl && matches!(vk, 0x30 | VK_NUMPAD0) => choose_zoom(a, 0),
        _ if ctrl && vk == b'T' as u16 => set_on_top(a, !a.ui.on_top),
        _ if ctrl && vk == b'W' as u16 => toggle_shade(a),
        _ if ctrl && vk == b'F' as u16 => {
            reset_input_decoder();
            a.start_search();
        }
        _ if ctrl => return false,
        0x5A => a.skip(false),              // Z
        0x58 => a.play(),                   // X
        0x43 | VK_SPACE => a.toggle_play(), // C (Winamp's pause), space
        0x56 => a.stop(),                   // V
        0x42 => a.skip(true),               // B
        0x53 => a.toggle_shuffle(),         // S
        0x52 => a.cycle_repeat(),           // R
        0x46 => a.toggle_like(),            // F
        0x44 => a.fetch_devices(),          // D
        0x4C => library_menu(),             // L
        0x4A => {
            reset_input_decoder();
            a.start_search();
        } // J (Winamp's jump to file)
        VK_F5 => a.reload(),
        VK_LEFT => a.seek_by(-app::SEEK_STEP_MS),
        VK_RIGHT => a.seek_by(app::SEEK_STEP_MS),
        VK_UP if !in_pl => a.change_volume(app::VOLUME_STEP),
        VK_DOWN if !in_pl => a.change_volume(-app::VOLUME_STEP),
        VK_UP => a.move_sel(-1),
        VK_DOWN => a.move_sel(1),
        VK_PRIOR => a.move_sel(-rows),
        VK_NEXT => a.move_sel(rows),
        VK_HOME => a.move_sel(-1_000_000),
        VK_END => a.move_sel(1_000_000),
        VK_RETURN => a.play_selected(),
        VK_ADD | VK_OEM_PLUS => a.change_volume(app::VOLUME_STEP),
        VK_SUBTRACT | VK_OEM_MINUS => a.change_volume(-app::VOLUME_STEP),
        _ => return false,
    }
    if let Some(a) = app() {
        a.reschedule();
    }
    true
}

thread_local! {
    static DECODER: std::cell::RefCell<input::Utf16Decoder> =
        std::cell::RefCell::new(input::Utf16Decoder::default());
}

fn reset_input_decoder() {
    DECODER.with(|d| d.borrow_mut().reset());
}

fn char_in(c: u32) {
    let Some(a) = app() else { return };
    let Some(ch) = DECODER.with(|d| d.borrow_mut().push(c)) else {
        return;
    };
    if std::mem::take(&mut a.eat_char) && a.typing && ch.eq_ignore_ascii_case(&'j') {
        return;
    }
    if a.typing {
        if !ch.is_control() && a.query.chars().count() < spotify::MAX_QUERY_CHARS {
            a.query.push(ch);
            a.inv_pl();
        }
        return;
    }
    if ch == '/' {
        reset_input_decoder();
        a.start_search();
        a.eat_char = false; // this character opened it; nothing else follows
    }
}

fn clipboard_text() -> Option<String> {
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        if OpenClipboard(null_mut()) == 0 {
            return None;
        }
        let h = GetClipboardData(CF_UNICODETEXT);
        let mut out = None;
        if !h.is_null() {
            let p = GlobalLock(h) as *const u16;
            if !p.is_null() {
                let mut n = 0;
                while n < 4096 && *p.add(n) != 0 {
                    n += 1;
                }
                out = Some(
                    String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
                        .replace(['\r', '\n'], " "),
                );
                GlobalUnlock(h);
            }
        }
        CloseClipboard();
        out
    }
}

// ── Menus ────────────────────────────────────────────────────────────────────
fn add(m: HMENU, id: usize, text: &str, checked: bool, enabled: bool) {
    let flags =
        MF_STRING | if checked { MF_CHECKED } else { 0 } | if enabled { 0 } else { MF_GRAYED };
    unsafe { AppendMenuW(m, flags, id, wide_z(text).as_ptr()) };
}

fn sep(m: HMENU) {
    unsafe { AppendMenuW(m, MF_SEPARATOR, 0, null()) };
}

fn library_items(a: &App, m: HMENU) {
    add(m, M_LIKED, "&Liked Songs", a.cur == "liked", true);
    add(m, M_SEARCH, "&Search...\tJ, /", a.cur == "search", true);
    sep(m);
    if a.playlists.is_empty() {
        add(
            m,
            0,
            if a.playlists_loaded {
                "No playlists"
            } else {
                "Loading playlists..."
            },
            false,
            false,
        );
    }
    for (i, p) in a.playlists.iter().enumerate().take(900) {
        let name = p.name.replace('&', "&&");
        add(
            m,
            M_PLAYLIST + i,
            &name,
            a.cur == format!("pl:{}", p.id),
            true,
        );
    }
    sep(m);
    add(m, M_RELOAD, "&Reload\tF5", false, true);
}

/// Show a popup menu at the cursor and return the chosen command (0: none). No app
/// reference is held while it is open: the menu's modal loop keeps dispatching messages.
fn popup(build: impl FnOnce(&App, HMENU)) -> usize {
    let Some(a) = app() else { return 0 };
    let owner = a.hmain;
    unsafe {
        let m = CreatePopupMenu();
        build(a, m);
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(owner);
        let cmd = TrackPopupMenu(
            m,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            owner,
            null(),
        );
        DestroyMenu(m);
        cmd as usize
    }
}

fn library_menu() {
    let cmd = popup(library_items);
    run_command(cmd);
}

fn main_menu() {
    let cmd = popup(|a, m| unsafe {
        let lib = CreatePopupMenu();
        library_items(a, lib);
        AppendMenuW(
            m,
            MF_POPUP | MF_STRING,
            lib as usize,
            wide_z("&Library").as_ptr(),
        );
        add(m, M_DEVICES, "Play on &device...\tD", false, a.engine.alive);
        sep(m);
        let has_track = a.pb.as_ref().is_some_and(|p| p.track.is_some());
        add(m, M_PLAY, "&Play\tX", false, true);
        add(m, M_PAUSE, "P&ause\tC", false, has_track);
        add(m, M_STOP, "S&top\tV", false, has_track);
        add(m, M_PREV, "Pre&vious\tZ", false, has_track);
        add(m, M_NEXT, "&Next\tB", false, has_track);
        sep(m);
        let liked = a.liked == Some(true)
            && a.pb
                .as_ref()
                .and_then(|p| p.track.as_ref())
                .is_some_and(|t| a.liked_uri.as_deref() == Some(&t.uri));
        add(m, M_LIKE, "&Like this song\tF", liked, has_track);
        let (shuffle, repeat) = shuffle_repeat(a);
        add(m, M_SHUFFLE, "S&huffle\tS", shuffle, true);
        let rep = CreatePopupMenu();
        for (i, (r, label)) in [
            (Repeat::Off, "Off"),
            (Repeat::Context, "All"),
            (Repeat::Track, "One"),
        ]
        .into_iter()
        .enumerate()
        {
            add(rep, M_REPEAT + i, label, repeat == r, true);
        }
        AppendMenuW(
            m,
            MF_POPUP | MF_STRING,
            rep as usize,
            wide_z("&Repeat\tR").as_ptr(),
        );
        sep(m);
        add(m, M_PLWIN, "Playlist &editor\tAlt+E", a.ui.pl_open, true);
        let size = CreatePopupMenu();
        add(
            size,
            M_ZOOM_AUTO,
            &format!("&Match the screen ({}%)\tCtrl+0", a.auto_pct),
            a.ui.scale == 0,
            true,
        );
        sep(size);
        for (i, z) in ZOOMS.iter().enumerate() {
            add(size, M_ZOOM + i, &format!("{z}%"), a.ui.scale == *z, true);
        }
        sep(size);
        add(size, M_ZOOM_IN, "&Larger\tCtrl++", false, a.pct < MAX_PCT);
        add(size, M_ZOOM_OUT, "&Smaller\tCtrl+-", false, a.pct > MIN_PCT);
        AppendMenuW(
            m,
            MF_POPUP | MF_STRING,
            size as usize,
            wide_z(&format!("Si&ze ({}%)", a.pct)).as_ptr(),
        );
        add(m, M_DOUBLE, "Dou&ble size\tCtrl+D", a.doubled(), true);
        add(m, M_ONTOP, "Always on &top\tCtrl+T", a.ui.on_top, true);
        add(m, M_SHADE, "&Shade mode\tCtrl+W", a.ui.shade, true);
        add(m, M_SCROLL, "Scroll song t&itle", a.ui.scroll, true);
        add(m, M_REMAINING, "Show time re&maining", a.ui.remaining, true);
        sep(m);
        add(
            m,
            M_RETRY,
            "Retry the pla&yer",
            false,
            a.phase == Phase::Running,
        );
        add(m, M_SIGNIN, "Sign &in to Spotify again", false, true);
        add(
            m,
            M_SIGNIN_PLAYER,
            "Sign in the player a&gain",
            false,
            a.s.engine,
        );
        add(m, M_FOLDER, "Open settings &folder", false, true);
        add(m, M_ABOUT, "About Spo&TermAmp", false, true);
        sep(m);
        add(m, M_EXIT, "E&xit", false, true);
    });
    run_command(cmd);
}

fn devices_menu() {
    let cmd = popup(|a, m| {
        let eng = a.engine.device_id.clone();
        for (i, d) in a.devices.iter().enumerate().take(50) {
            let me = if Some(&d.id) == eng.as_ref() {
                " (this PC)"
            } else {
                ""
            };
            let label = format!(
                "{}{me}\t{}",
                d.name.replace('&', "&&"),
                d.kind.to_lowercase()
            );
            add(m, M_DEVICE + i, &label, d.active, true);
        }
    });
    run_command(cmd);
}

fn run_command(cmd: usize) {
    let Some(a) = app() else { return };
    if a.typing {
        reset_input_decoder();
    }
    match cmd {
        0 => {}
        M_LIKED => a.open_list("liked"),
        M_SEARCH => {
            reset_input_decoder();
            a.start_search();
        }
        M_RELOAD => a.reload(),
        M_DEVICES => a.fetch_devices(),
        M_LIKE => a.toggle_like(),
        M_SHUFFLE => a.toggle_shuffle(),
        M_PLAY => a.play(),
        M_PAUSE => a.toggle_play(),
        M_STOP => a.stop(),
        M_NEXT => a.skip(true),
        M_PREV => a.skip(false),
        M_PLWIN => toggle_playlist(a),
        M_DOUBLE => toggle_double(a),
        M_ZOOM_AUTO => choose_zoom(a, 0),
        M_ZOOM_IN => zoom_step(a, true),
        M_ZOOM_OUT => zoom_step(a, false),
        M_UNLIKE_SEL => a.unlike_selected(),
        M_JUMP => a.jump_to_playing(),
        M_FIRST => a.move_sel(-1_000_000),
        M_LAST => a.move_sel(1_000_000),
        M_INFO => song_info(),
        M_ONTOP => set_on_top(a, !a.ui.on_top),
        M_SHADE => toggle_shade(a),
        M_SCROLL => {
            a.ui.scroll = !a.ui.scroll;
            a.marquee_px = 0;
            a.inv_main();
        }
        M_REMAINING => {
            a.ui.remaining = !a.ui.remaining;
            a.inv_all();
        }
        M_RETRY => a.reload(),
        M_SIGNIN => a.sign_in_again(),
        M_SIGNIN_PLAYER => {
            a.engine.stop();
            a.phase = Phase::PlayerSignIn;
            a.flash("Sign in SpoTerm's player in your browser", false);
            a.engine.sign_in(&a.s, true, a.hmain as isize);
        }
        M_FOLDER => unsafe {
            let dir = config::config_dir();
            let _ = std::fs::create_dir_all(&dir);
            ShellExecuteW(
                null_mut(),
                wide_z("open").as_ptr(),
                wide_z(&dir.to_string_lossy()).as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL,
            );
        },
        M_ABOUT => about(),
        M_EXIT => unsafe {
            PostMessageW(a.hmain, WM_CLOSE, 0, 0);
        },
        c if (M_ZOOM..M_ZOOM + ZOOMS.len()).contains(&c) => choose_zoom(a, ZOOMS[c - M_ZOOM]),
        c if (M_REPEAT..M_REPEAT + 3).contains(&c) => {
            a.set_repeat([Repeat::Off, Repeat::Context, Repeat::Track][c - M_REPEAT])
        }
        c if (M_PLAYLIST..M_PLAYLIST + 900).contains(&c) => a.open_playlist(c - M_PLAYLIST),
        c if (M_DEVICE..M_DEVICE + 50).contains(&c) => a.transfer(c - M_DEVICE),
        _ => {}
    }
    if let Some(a) = app() {
        a.reschedule();
    }
}

fn about() {
    let owner = app().map_or(null_mut(), |a| a.hmain);
    let text = format!(
        "SpoTermAmp {}\n\nA classic-skinned window for SpoTerm's Spotify engine.\n\
         Skin: Winamp5 Classified v5.5 by Sven Kistner, Zarko Jovic, GuidoD and Wildrose-Wally.\n\n\
         Keys: Z X C V B (prev, play, pause, stop, next), arrows seek and change volume,\n\
         S shuffle, R repeat, F like, D devices, L library, J or / search, Ctrl+D double size,\n\
         Ctrl +/- or Ctrl+wheel resize, Ctrl+0 match the screen, Ctrl+T always on top,\n\
         Ctrl+W shade, Alt+E playlist, F5 reload.",
        env!("CARGO_PKG_VERSION")
    );
    unsafe {
        MessageBoxW(
            owner,
            wide_z(&text).as_ptr(),
            wide_z("About SpoTermAmp").as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        )
    };
}

fn song_info() {
    let Some(a) = app() else { return };
    let owner = a.hmain;
    let text = match a.pb.as_ref().and_then(|p| p.track.as_ref()) {
        Some(t) => {
            let dev =
                a.pb.as_ref()
                    .map_or(String::new(), |p| p.device_name.clone());
            format!(
                "{}\n{}\n{}\n\n{}  ·  playing on {}",
                t.name,
                t.artists,
                t.album,
                spotify::fmt_time(t.duration_ms),
                dev
            )
        }
        None => "Nothing is playing.".into(),
    };
    unsafe {
        MessageBoxW(
            owner,
            wide_z(&text).as_ptr(),
            wide_z("Song info").as_ptr(),
            MB_OK,
        )
    };
}

// ── First run: the Client ID ─────────────────────────────────────────────────
/// A small window asking for the Spotify app's Client ID. None if cancelled.
fn ask_client_id() -> Option<String> {
    const ID_EDIT: isize = 10;
    const ID_LINK: isize = 11;
    static mut RESULT: Option<String> = None;
    static mut DONE: bool = false;

    unsafe extern "system" fn proc_(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_COMMAND => {
                let id = (wp & 0xFFFF) as i32;
                if id == IDOK {
                    let edit = GetDlgItem(h, ID_EDIT as i32);
                    let mut buf = [0u16; 64];
                    let n = GetWindowTextW(edit, buf.as_mut_ptr(), 64);
                    let cid = String::from_utf16_lossy(&buf[..n.max(0) as usize])
                        .trim()
                        .to_string();
                    if config::valid_client_id(&cid) {
                        RESULT = Some(cid);
                        DestroyWindow(h);
                    } else {
                        MessageBoxW(
                            h,
                            wide_z("That doesn't look like a Client ID (32 letters and digits).")
                                .as_ptr(),
                            wide_z("SpoTermAmp").as_ptr(),
                            MB_OK | MB_ICONWARNING,
                        );
                        SetFocus(edit);
                    }
                    return 0;
                }
                if id == IDCANCEL {
                    DestroyWindow(h);
                    return 0;
                }
                if id == ID_LINK as i32 {
                    ShellExecuteW(
                        null_mut(),
                        wide_z("open").as_ptr(),
                        wide_z(DASHBOARD).as_ptr(),
                        null(),
                        null(),
                        SW_SHOWNORMAL,
                    );
                    return 0;
                }
            }
            WM_CLOSE => {
                DestroyWindow(h);
                return 0;
            }
            WM_DESTROY => {
                DONE = true;
                return 0;
            }
            _ => {}
        }
        DefWindowProcW(h, msg, wp, lp)
    }

    unsafe {
        let hinst = GetModuleHandleW(null());
        let cls = wide_z("SpoTermAmpSetup");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            hInstance: hinst,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_BTNFACE + 1) as usize as HBRUSH,
            lpszClassName: cls.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
        let dpi = GetDpiForSystem() as i32;
        let px = |v: i32| v * dpi / 96;
        let (w, h) = (px(460), px(250));
        let x = (GetSystemMetrics(SM_CXSCREEN) - w) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - h) / 3;
        let win = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            cls.as_ptr(),
            wide_z("SpoTermAmp setup").as_ptr(),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            x,
            y,
            w,
            h,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        if win.is_null() {
            return None;
        }
        let font = gfx::make_font("Segoe UI", px(15), false);
        let child =
            |class: &str, text: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: isize| {
                let c = CreateWindowExW(
                    if class == "EDIT" { WS_EX_CLIENTEDGE } else { 0 },
                    wide_z(class).as_ptr(),
                    wide_z(text).as_ptr(),
                    WS_CHILD | WS_VISIBLE | style,
                    px(x),
                    px(y),
                    px(w),
                    px(h),
                    win,
                    id as HMENU,
                    hinst,
                    null(),
                );
                SendMessageW(c, WM_SETFONT, font as usize, 1);
                c
            };
        child("STATIC", "SpoTermAmp needs the Client ID of your own Spotify app (one-time setup):\n\n\
                         1. Create an app at developer.spotify.com/dashboard.\n\
                         2. Add the redirect URI  http://127.0.0.1:8888/callback  and tick Web API.\n\
                         3. Paste the app's Client ID here (no secret needed).",
              0, 14, 10, 430, 110, 0);
        let edit = child(
            "EDIT",
            "",
            WS_TABSTOP | ES_AUTOHSCROLL as u32,
            14,
            128,
            430,
            26,
            ID_EDIT,
        );
        child(
            "BUTTON",
            "Open dashboard",
            WS_TABSTOP,
            14,
            172,
            130,
            30,
            ID_LINK,
        );
        child(
            "BUTTON",
            "OK",
            WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            254,
            172,
            90,
            30,
            IDOK as isize,
        );
        child(
            "BUTTON",
            "Cancel",
            WS_TABSTOP,
            354,
            172,
            90,
            30,
            IDCANCEL as isize,
        );
        ShowWindow(win, SW_SHOW);
        SetForegroundWindow(win);
        SetFocus(edit);
        let mut msg: MSG = std::mem::zeroed();
        while !DONE && GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(win, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        DeleteObject(font);
        (*std::ptr::addr_of_mut!(RESULT)).take()
    }
}

// ── Self-check ───────────────────────────────────────────────────────────────
/// `--snapshot DIR`: draw both windows with sample data into DIR/main.bmp and DIR/pl.bmp,
/// without opening any window. Used to check the drawing against the skin.
fn snapshot(dir: &str) -> i32 {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("snapshot dir: {e}");
        return 1;
    }
    use std::rc::Rc;
    let sk = match skin::Skin::load(skin::DEFAULT) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("skin: {e}");
            return 1;
        }
    };
    let s = config::load();
    let ui = config::UiState {
        pl_w: 275,
        pl_h: 232,
        ..Default::default()
    };
    let mut a = App::new(s, ui, sk, Engine::new(Default::default()), 100, 100);
    a.local = true;
    let tracks: Vec<Rc<spotify::Track>> = (0..30)
        .map(|i| {
            Rc::new(spotify::Track {
                uri: format!("spotify:track:{i}"),
                name: if i == 3 {
                    "Lyin' Eyes".into()
                } else {
                    format!("Song {i}")
                },
                artists: if i == 3 {
                    "Eagles".into()
                } else {
                    "Artist".into()
                },
                album: "Album".into(),
                duration_ms: 180_000 + i * 1000,
                pos: i,
                playable: i != 6,
            })
        })
        .collect();
    {
        let l = a.lists.get_mut("liked").expect("liked");
        l.tracks = tracks.clone();
        l.total = 30;
        l.sel = 5;
        l.next_offset = None;
    }
    a.pb = Some(spotify::Playback {
        track: Some(tracks[3].clone()),
        playing: true,
        progress_ms: 61_000,
        at: Instant::now(),
        device_id: Some("x".into()),
        device_name: "SpoTerm".into(),
        volume: Some(70),
        shuffle: true,
        repeat: Repeat::Off,
        context_uri: None,
    });
    let mut ok = true;
    for pct in [100, 125, 200] {
        a.pct = pct;
        a.set_fonts();
        a.ui.shade = false;
        let mut main_sf = Surface::new(MAIN_W, MAIN_H, pct);
        draw_main(&mut a, &mut main_sf);
        let mut pl_sf = Surface::new(275, 232, pct);
        draw_pl(&mut a, &mut pl_sf);
        a.ui.shade = true;
        let mut shade_sf = Surface::new(MAIN_W, SHADE_H, pct);
        draw_main(&mut a, &mut shade_sf);
        let name = |n: &str| {
            if pct == 100 {
                format!("{dir}/{n}.bmp")
            } else {
                format!("{dir}/{n}_{pct}.bmp")
            }
        };
        ok &= write_bmp(&name("main"), main_sf.w, main_sf.h, &main_sf.pixels())
            && write_bmp(&name("pl"), pl_sf.w, pl_sf.h, &pl_sf.pixels())
            && write_bmp(&name("shade"), shade_sf.w, shade_sf.h, &shade_sf.pixels());
    }
    std::mem::forget(a); // no engine, no windows: nothing to clean up
    if ok {
        0
    } else {
        1
    }
}

fn write_bmp(path: &str, w: i32, h: i32, px: &[u32]) -> bool {
    let mut d = Vec::with_capacity(54 + px.len() * 4);
    let size = 54 + px.len() as u32 * 4;
    d.extend_from_slice(b"BM");
    d.extend_from_slice(&size.to_le_bytes());
    d.extend_from_slice(&[0; 4]);
    d.extend_from_slice(&54u32.to_le_bytes());
    d.extend_from_slice(&40u32.to_le_bytes());
    d.extend_from_slice(&w.to_le_bytes());
    d.extend_from_slice(&(-h).to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&32u16.to_le_bytes());
    d.extend_from_slice(&[0; 24]);
    for p in px {
        d.extend_from_slice(&p.to_le_bytes());
    }
    std::fs::write(path, d).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_creates_directory() {
        let dir = std::env::temp_dir().join(format!(
            "amp_snap_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!dir.exists());
        let rc = snapshot(dir.to_str().unwrap());
        assert_eq!(rc, 0);
        assert!(dir.join("main.bmp").exists());
        assert!(dir.join("pl.bmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
