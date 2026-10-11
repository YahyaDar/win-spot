//! Drawing and hit-testing for the two windows, in the skin's own 1x coordinates.
//!
//! Sprite positions are those of the classic skin format, the layout every .wsz shares:
//! the main window is 275x116, the playlist editor is built from tiles of 25x29.

use std::time::Instant;

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, DT_VCENTER};

use crate::app::{App, Hold, Kind};
use crate::gfx::{bitmap_font_can, bitmap_text, wide, Surface};
use crate::spotify::{fmt_time, Repeat};

pub const MAIN_W: i32 = 275;
pub const MAIN_H: i32 = 116;
pub const SHADE_H: i32 = 14;
pub const ROW_H: i32 = 13;

// Main window buttons.
pub const B_PREV: u32 = 1;
pub const B_PLAY: u32 = 2;
pub const B_PAUSE: u32 = 3;
pub const B_STOP: u32 = 4;
pub const B_NEXT: u32 = 5;
pub const B_EJECT: u32 = 6;
pub const B_SHUFFLE: u32 = 7;
pub const B_REPEAT: u32 = 8;
pub const B_EQ: u32 = 9;
pub const B_PL: u32 = 10;
pub const B_OPTIONS: u32 = 11;
pub const B_MIN: u32 = 12;
pub const B_SHADE: u32 = 13;
pub const B_CLOSE: u32 = 14;
pub const B_CL_O: u32 = 15;
pub const B_CL_A: u32 = 16;
pub const B_CL_I: u32 = 17;
pub const B_CL_D: u32 = 18;
pub const B_CL_V: u32 = 19;
pub const B_TIME: u32 = 20;
pub const B_LOGO: u32 = 21;

// Playlist window buttons.
pub const P_CLOSE: u32 = 101;
pub const P_SHADE: u32 = 102;
pub const P_ADD: u32 = 103;
pub const P_REM: u32 = 104;
pub const P_SEL: u32 = 105;
pub const P_MISC: u32 = 106;
pub const P_LIST: u32 = 107;
pub const P_PREV: u32 = 108;
pub const P_PLAY: u32 = 109;
pub const P_PAUSE: u32 = 110;
pub const P_STOP: u32 = 111;
pub const P_NEXT: u32 = 112;
pub const P_EJECT: u32 = 113;

pub enum Hit {
    Button(u32),
    Volume,
    Seek,
    Drag,      // anywhere else on the main window moves it, as in Winamp
    Row(usize),
    Scroll,
    Resize,
    Nothing,
}

struct Btn {
    id: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

const MAIN_BUTTONS: &[Btn] = &[
    Btn { id: B_OPTIONS, x: 6, y: 3, w: 9, h: 9 },
    Btn { id: B_MIN, x: 244, y: 3, w: 9, h: 9 },
    Btn { id: B_SHADE, x: 254, y: 3, w: 9, h: 9 },
    Btn { id: B_CLOSE, x: 264, y: 3, w: 9, h: 9 },
    Btn { id: B_CL_O, x: 10, y: 25, w: 8, h: 8 },
    Btn { id: B_CL_A, x: 10, y: 33, w: 8, h: 7 },
    Btn { id: B_CL_I, x: 10, y: 40, w: 8, h: 7 },
    Btn { id: B_CL_D, x: 10, y: 47, w: 8, h: 8 },
    Btn { id: B_CL_V, x: 10, y: 55, w: 8, h: 7 },
    Btn { id: B_TIME, x: 36, y: 26, w: 63, h: 13 },
    Btn { id: B_PREV, x: 16, y: 88, w: 23, h: 18 },
    Btn { id: B_PLAY, x: 39, y: 88, w: 23, h: 18 },
    Btn { id: B_PAUSE, x: 62, y: 88, w: 23, h: 18 },
    Btn { id: B_STOP, x: 85, y: 88, w: 23, h: 18 },
    Btn { id: B_NEXT, x: 108, y: 88, w: 22, h: 18 },
    Btn { id: B_EJECT, x: 136, y: 89, w: 22, h: 16 },
    Btn { id: B_SHUFFLE, x: 164, y: 89, w: 47, h: 15 },
    Btn { id: B_REPEAT, x: 210, y: 89, w: 28, h: 15 },
    Btn { id: B_EQ, x: 219, y: 58, w: 23, h: 12 },
    Btn { id: B_PL, x: 242, y: 58, w: 23, h: 12 },
    Btn { id: B_LOGO, x: 253, y: 91, w: 13, h: 15 },
];

fn inside(x: i32, y: i32, bx: i32, by: i32, w: i32, h: i32) -> bool {
    x >= bx && y >= by && x < bx + w && y < by + h
}

/// The main window's height: full, or the 14-pixel shade strip.
pub fn main_h(app: &App) -> i32 {
    if app.ui.shade { SHADE_H } else { MAIN_H }
}

const SHADE_BUTTONS: &[Btn] = &[
    Btn { id: B_OPTIONS, x: 6, y: 3, w: 9, h: 9 },
    Btn { id: B_MIN, x: 244, y: 3, w: 9, h: 9 },
    Btn { id: B_SHADE, x: 254, y: 3, w: 9, h: 9 },
    Btn { id: B_CLOSE, x: 264, y: 3, w: 9, h: 9 },
    Btn { id: B_TIME, x: 125, y: 3, w: 30, h: 8 },
    Btn { id: B_PREV, x: 169, y: 2, w: 7, h: 10 },
    Btn { id: B_PLAY, x: 176, y: 2, w: 10, h: 10 },
    Btn { id: B_PAUSE, x: 186, y: 2, w: 10, h: 10 },
    Btn { id: B_STOP, x: 196, y: 2, w: 9, h: 10 },
    Btn { id: B_NEXT, x: 205, y: 2, w: 10, h: 10 },
    Btn { id: B_EJECT, x: 215, y: 2, w: 9, h: 10 },
];

pub fn main_hit(app: &App, x: i32, y: i32) -> Hit {
    if app.ui.shade {
        if let Some(b) = SHADE_BUTTONS.iter().find(|b| inside(x, y, b.x, b.y, b.w, b.h)) {
            return Hit::Button(b.id);
        }
        return if inside(x, y, 226, 3, 17, 9) { Hit::Seek } else { Hit::Drag };
    }
    if let Some(b) = MAIN_BUTTONS.iter().find(|b| inside(x, y, b.x, b.y, b.w, b.h)) {
        return Hit::Button(b.id);
    }
    if inside(x, y, 107, 57, 68, 13) {
        return Hit::Volume;
    }
    if inside(x, y, 16, 72, 248, 10) {
        return Hit::Seek;
    }
    if inside(x, y, 177, 57, 38, 13) {
        return Hit::Nothing;
    }
    Hit::Drag
}

/// The volume thumb's left edge.
pub fn vol_thumb_x(app: &App) -> i32 {
    107 + app.volume().unwrap_or(0) as i32 * 54 / 100
}

/// The position thumb's left edge, while something is loaded.
pub fn seek_thumb_x(app: &App) -> Option<i32> {
    let pb = app.pb.as_ref()?;
    let t = pb.track.as_ref()?;
    let pos = match app.hold { Hold::Seek(p) => p, _ => pb.progress(Instant::now()) };
    let x = if t.duration_ms > 0 { (pos as u64 * 219 / t.duration_ms as u64) as i32 } else { 0 };
    Some(16 + x.min(219))
}

/// Shuffle and repeat as shown: the player's, or what the first play will use.
pub fn shuffle_repeat(app: &App) -> (bool, Repeat) {
    app.pb.as_ref().map_or((app.pref_shuffle, app.pref_repeat), |p| (p.shuffle, p.repeat))
}

/// The volume a click or drag at `x` means.
pub fn volume_at(x: i32) -> u8 {
    ((x - 107 - 7).clamp(0, 54) * 100 / 54) as u8
}

/// The position a click or drag at `x` means, for a track of `dur` ms.
pub fn seek_at(app: &App, x: i32, dur: u32) -> u32 {
    if app.ui.shade {
        return ((x - 226 - 1).clamp(0, 14) as u64 * dur as u64 / 14) as u32;
    }
    ((x - 16 - 14).clamp(0, 219) as u64 * dur as u64 / 219) as u32
}

/// The 275x14 shade strip: title bar, clock, mini transport and position bar.
fn draw_shade(app: &mut App, sf: &mut Surface, now: Instant) {
    let sk = &app.skin;
    let tb = sk.titlebar.as_ref();
    let pressed = |id: u32| app.hold == Hold::Button(id) && app.hold_inside;
    sf.blit(tb, 27, if app.main_active { 29 } else { 42 }, MAIN_W, SHADE_H, 0, 0);
    if pressed(B_OPTIONS) { sf.blit(tb, 0, 9, 9, 9, 6, 3); }
    if pressed(B_MIN) { sf.blit(tb, 9, 9, 9, 9, 244, 3); }
    sf.blit(tb, if pressed(B_SHADE) { 9 } else { 0 }, 27, 9, 9, 254, 3);
    if pressed(B_CLOSE) { sf.blit(tb, 18, 9, 9, 9, 264, 3); }
    let pb = app.pb.as_ref();
    let Some(t) = pb.and_then(|p| p.track.as_ref()) else { return };
    let pos = match app.hold { Hold::Seek(p) => p, _ => pb.map_or(0, |p| p.progress(now)) };
    let remaining = app.ui.remaining && t.duration_ms > 0;
    let shown = if remaining { t.duration_ms.saturating_sub(pos) } else { pos } / 1000;
    let sign = if remaining { "-" } else { " " };
    let clock = format!("{sign}{:02}:{:02}", (shown / 60) % 100, shown % 60);
    bitmap_text(sf, sk.text.as_ref(), &clock, 125, 4, 0, 30);
    sf.blit(tb, 0, 36, 17, 7, 226, 4);
    let x = if t.duration_ms > 0 { (pos as u64 * 14 / t.duration_ms as u64) as i32 } else { 0 };
    let sx = if x <= 3 { 17 } else if x >= 11 { 23 } else { 20 };
    sf.blit(tb, sx, 36, 3, 7, 226 + x.min(14), 4);
}

// ── Main window ──────────────────────────────────────────────────────────────
pub fn draw_main(app: &mut App, sf: &mut Surface) {
    let now = Instant::now();
    let sk = &app.skin;
    if app.ui.shade {
        return draw_shade(app, sf, now);
    }
    let pressed = |id: u32| app.hold == Hold::Button(id) && app.hold_inside;
    sf.blit(sk.main.as_ref(), 0, 0, MAIN_W, MAIN_H, 0, 0);

    // Title bar and its buttons.
    let tb = sk.titlebar.as_ref();
    sf.blit(tb, 27, if app.main_active { 0 } else { 15 }, MAIN_W, 14, 0, 0);
    if pressed(B_OPTIONS) { sf.blit(tb, 0, 9, 9, 9, 6, 3); }
    if pressed(B_MIN) { sf.blit(tb, 9, 9, 9, 9, 244, 3); }
    if pressed(B_SHADE) { sf.blit(tb, 9, 18, 9, 9, 254, 3); }
    if pressed(B_CLOSE) { sf.blit(tb, 18, 9, 9, 9, 264, 3); }

    // Clutter bar: O(ptions) A(lways on top) I(nfo) D(ouble size) V(is).
    sf.blit(tb, 304, 0, 8, 43, 10, 22);
    if pressed(B_CL_O) { sf.blit(tb, 304, 47, 8, 8, 10, 25); }
    if app.ui.on_top || pressed(B_CL_A) { sf.blit(tb, 312, 55, 8, 7, 10, 33); }
    if pressed(B_CL_I) { sf.blit(tb, 320, 62, 8, 7, 10, 40); }
    if app.doubled() || pressed(B_CL_D) { sf.blit(tb, 328, 69, 8, 8, 10, 47); }

    let pb = app.pb.as_ref();
    let track = pb.and_then(|p| p.track.as_ref());
    let playing = pb.is_some_and(|p| p.playing);

    // Play state indicator.
    let pp = sk.playpaus.as_ref();
    if track.is_some() {
        if playing {
            sf.blit(pp, 0, 0, 9, 9, 26, 28);
        } else {
            sf.blit(pp, 9, 0, 9, 9, 26, 28);
        }
    } else {
        sf.blit(pp, 18, 0, 9, 9, 26, 28);
    }

    // Time: mm:ss elapsed, or -mm:ss remaining (click to switch).
    if let Some(t) = track {
        let pos = match app.hold { Hold::Seek(p) => p, _ => pb.map_or(0, |p| p.progress(now)) };
        let remaining = app.ui.remaining && t.duration_ms > 0;
        let shown = if remaining { t.duration_ms.saturating_sub(pos) } else { pos } / 1000;
        let (m, s) = ((shown / 60) % 100, shown % 60);
        let nums = sk.numbers.as_ref();
        let digit = |sf: &mut Surface, d: u32, x: i32| sf.blit(nums, d as i32 * 9, 0, 9, 13, x, 26);
        if remaining {
            if sk.nums_ex {
                sf.blit(nums, 99, 0, 9, 13, 36, 26);
            } else {
                sf.blit(nums, 20, 6, 5, 1, 38, 32);
            }
        }
        digit(sf, m / 10, 48);
        digit(sf, m % 10, 60);
        digit(sf, s / 10, 78);
        digit(sf, s % 10, 90);

        // kbps / kHz and stereo, for our own player.
        if app.local {
            bitmap_text(sf, sk.text.as_ref(), &format!("{:>3}", app.s.bitrate), 111, 43, 0, 15);
            bitmap_text(sf, sk.text.as_ref(), "44", 156, 43, 0, 10);
        }
        sf.blit(sk.monoster.as_ref(), 0, 0, 29, 12, 239, 41);
        sf.blit(sk.monoster.as_ref(), 29, 12, 27, 12, 212, 41);
    } else {
        sf.blit(sk.monoster.as_ref(), 0, 12, 29, 12, 239, 41);
        sf.blit(sk.monoster.as_ref(), 29, 12, 27, 12, 212, 41);
    }

    draw_marquee(app, sf);
    let sk = &app.skin;
    let pb = app.pb.as_ref();
    let track = pb.and_then(|p| p.track.as_ref());
    let pressed = |id: u32| app.hold == Hold::Button(id) && app.hold_inside;

    // Volume and balance.
    let vol = app.volume().unwrap_or(0) as i32;
    let frame = (vol * 27 + 50) / 100;
    sf.blit(sk.volume.as_ref(), 0, frame * 15, 68, 13, 107, 57);
    let vthumb = if app.hold == Hold::Volume { 0 } else { 15 };
    sf.blit(sk.volume.as_ref(), vthumb, 422, 14, 11, vol_thumb_x(app), 58);
    sf.blit(sk.balance.as_ref(), 9, 0, 38, 13, 177, 57);
    sf.blit(sk.balance.as_ref(), 15, 422, 14, 11, 177 + 12, 58);

    // Position bar: only while something is loaded.
    if let (Some(_), Some(x)) = (track, seek_thumb_x(app)) {
        sf.blit(sk.posbar.as_ref(), 0, 0, 248, 10, 16, 72);
        let sx = if matches!(app.hold, Hold::Seek(_)) { 278 } else { 248 };
        sf.blit(sk.posbar.as_ref(), sx, 0, 29, 10, x, 72);
    }

    // Transport buttons.
    let cb = sk.cbuttons.as_ref();
    for (id, sx, w, h, x, y) in [(B_PREV, 0, 23, 18, 16, 88), (B_PLAY, 23, 23, 18, 39, 88), (B_PAUSE, 46, 23, 18, 62, 88),
                                 (B_STOP, 69, 23, 18, 85, 88), (B_NEXT, 92, 22, 18, 108, 88), (B_EJECT, 114, 22, 16, 136, 89)] {
        sf.blit(cb, sx, if pressed(id) { h } else { 0 }, w, h, x, y);
    }

    // Shuffle, repeat, EQ and PL toggles.
    let sr = sk.shufrep.as_ref();
    let (shuffle, repeat) = shuffle_repeat(app);
    sf.blit(sr, 28, (shuffle as i32) * 30 + (pressed(B_SHUFFLE) as i32) * 15, 47, 15, 164, 89);
    sf.blit(sr, 0, ((repeat != Repeat::Off) as i32) * 30 + (pressed(B_REPEAT) as i32) * 15, 28, 15, 210, 89);
    sf.blit(sr, if pressed(B_EQ) { 46 } else { 0 }, 61, 23, 12, 219, 58);
    sf.blit(sr, if pressed(B_PL) { 69 } else { 23 }, if app.ui.pl_open { 73 } else { 61 }, 23, 12, 242, 58);
}

/// The scrolling song title (111, 27), 154 pixels wide.
fn draw_marquee(app: &mut App, sf: &mut Surface) {
    const X: i32 = 111;
    const W: i32 = 154;
    let text = app.marquee_text();
    if text != app.marquee_key {
        app.marquee_key = text.clone();
        app.marquee_px = 0;
    }
    if bitmap_font_can(&text) {
        let tw = text.chars().count() as i32 * 5;
        app.marquee_scrolls = tw > W;
        if !app.marquee_scrolls {
            bitmap_text(sf, app.skin.text.as_ref(), &text, X, 27, 0, W);
            return;
        }
        let looped = format!("{text}  ***  ");
        let cycle = looped.chars().count() as i32 * 5;
        let off = app.marquee_px.rem_euclid(cycle);
        app.marquee_px = off;
        bitmap_text(sf, app.skin.text.as_ref(), &looped, X, 27, off, W);
        if cycle - off < W {
            sf_clip_text(sf, app, &looped, X + cycle - off, W - (cycle - off));
        }
    } else {
        // Titles the skin's font can't spell (CJK, Cyrillic, ...): the system font, in
        // the skin's text colours.
        sf.select_font(app.small_font);
        sf.fill(X, 25, W, 10, app.skin.text_bg);
        let w16 = wide(&text);
        let tw = sf.text_width(&w16);
        app.marquee_scrolls = tw > W;
        let clip = RECT { left: X, top: 24, right: X + W, bottom: 36 };
        if !app.marquee_scrolls {
            sf.text_clipped(&w16, X, clip, app.skin.text_fg);
            return;
        }
        let looped = wide(&format!("{text}   ***   "));
        let cycle = sf.text_width(&looped).max(1);
        let off = app.marquee_px.rem_euclid(cycle);
        app.marquee_px = off;
        sf.text_clipped(&looped, X - off, clip, app.skin.text_fg);
        if cycle - off < W {
            sf.text_clipped(&looped, X + cycle - off, clip, app.skin.text_fg);
        }
    }
}

fn sf_clip_text(sf: &mut Surface, app: &App, s: &str, x: i32, w: i32) {
    bitmap_text(sf, app.skin.text.as_ref(), s, x, 27, 0, w);
}

// ── Playlist window ──────────────────────────────────────────────────────────
/// Track rows that fit, below the header row.
pub fn pl_rows(app: &App) -> usize {
    ((app.ui.pl_h - 20 - 38 - ROW_H) / ROW_H).max(1) as usize
}

pub fn pl_hit(app: &App, x: i32, y: i32) -> Hit {
    let (w, h) = (app.ui.pl_w, app.ui.pl_h);
    if inside(x, y, w - 20, h - 20, 20, 20) {
        return Hit::Resize;
    }
    if y < 20 {
        if inside(x, y, w - 11, 3, 9, 9) { return Hit::Button(P_CLOSE); }
        if inside(x, y, w - 20, 3, 9, 9) { return Hit::Button(P_SHADE); }
        return Hit::Drag;
    }
    if y >= h - 38 {
        let (rx, ry) = (w - 150, h - 38);
        for (id, bx, bw) in [(P_ADD, 14, 25), (P_REM, 43, 25), (P_SEL, 72, 25), (P_MISC, 101, 25)] {
            if inside(x, y, bx, h - 30, bw, 18) { return Hit::Button(id); }
        }
        if inside(x, y, w - 44, h - 30, 25, 18) { return Hit::Button(P_LIST); }
        for (id, bx, bw) in [(P_PREV, 6, 8), (P_PLAY, 14, 8), (P_PAUSE, 22, 10), (P_STOP, 32, 9), (P_NEXT, 41, 8), (P_EJECT, 49, 9)] {
            if inside(x, y, rx + bx, ry + 22, bw, 8) { return Hit::Button(id); }
        }
        return Hit::Drag;
    }
    if x >= w - 20 {
        return Hit::Scroll;
    }
    if x >= 12 && y >= 20 + ROW_H {
        let row = ((y - 20 - ROW_H) / ROW_H) as usize;
        if row < pl_rows(app) {
            return Hit::Row(app.cur_list().top + row);
        }
    }
    Hit::Nothing
}

/// The scroll handle's top, and the travel it has.
pub fn scroll_geometry(app: &App) -> (i32, i32) {
    let travel = (app.ui.pl_h - 20 - 38 - 18).max(0);
    let l = app.cur_list();
    let max_top = l.tracks.len().saturating_sub(pl_rows(app));
    let y = if max_top == 0 { 0 } else { (l.top.min(max_top) as i64 * travel as i64 / max_top as i64) as i32 };
    (20 + y, travel)
}

pub fn draw_pl(app: &mut App, sf: &mut Surface) {
    let (w, h) = (app.ui.pl_w, app.ui.pl_h);
    let sk = &app.skin;
    let pe = sk.pledit.as_ref();
    let pressed = |id: u32| app.hold == Hold::PlButton(id) && app.hold_inside;
    let ty = if app.pl_active { 0 } else { 21 };

    // Frame: top row, sides, bottom row.
    sf.blit(pe, 0, ty, 25, 20, 0, 0);
    let title_x = (w - 100) / 2;
    sf.tile(pe, 127, ty, 25, 20, 25, 0, title_x - 25, 20);
    sf.blit(pe, 26, ty, 100, 20, title_x, 0);
    sf.tile(pe, 127, ty, 25, 20, title_x + 100, 0, w - 25 - title_x - 100, 20);
    sf.blit(pe, 153, ty, 25, 20, w - 25, 0);
    if pressed(P_CLOSE) { sf.blit(pe, 52, 42, 9, 9, w - 11, 3); }
    if pressed(P_SHADE) { sf.blit(pe, 62, 42, 9, 9, w - 20, 3); }
    sf.tile(pe, 0, 42, 12, 29, 0, 20, 12, h - 58);
    sf.tile(pe, 31, 42, 20, 29, w - 20, 20, 20, h - 58);
    sf.blit(pe, 0, 72, 125, 38, 0, h - 38);
    sf.tile(pe, 179, 0, 25, 38, 125, h - 38, w - 275, 38);
    sf.blit(pe, 126, 72, 150, 38, w - 150, h - 38);


    // List area.
    let pl = &sk.pl;
    let (lx, ly, lw) = (12, 20, w - 32);
    sf.fill(lx, ly, lw, h - 58, pl.normal_bg);
    let (hy, _) = scroll_geometry(app);
    sf.blit(pe, if app.hold != Hold::None && matches!(app.hold, Hold::PlScroll(_)) { 61 } else { 52 }, 53, 8, 18, w - 15, hy);

    // Mini displays in the bottom-right corner: position in the list and elapsed time.
    let (rx, ry) = (w - 150, h - 38);
    let l = app.cur_list();
    let count = if l.tracks.is_empty() { "0/0".to_string() } else { format!("{}/{}", l.sel + 1, l.total.max(l.tracks.len() as u32)) };
    bitmap_text(sf, sk.text.as_ref(), &count, rx + 7, ry + 10, 0, 90);
    draw_pl_clock(app, sf);

    // Rows: a header (the list's name, or the search being typed), then tracks.
    sf.select_font(app.pl_font);
    let row_rect = |i: i32| RECT { left: lx + 2, top: ly + i * ROW_H, right: lx + lw - 2, bottom: ly + (i + 1) * ROW_H };
    let header = if app.typing {
        format!("Search: {}_", app.query)
    } else if l.locked {
        format!("{} (Spotify hides this list: press enter to play it)", l.title)
    } else if l.loading && l.tracks.is_empty() {
        format!("{} (loading...)", l.title)
    } else if !l.error.is_empty() && l.tracks.is_empty() {
        format!("{}: {}", l.title, l.error)
    } else if l.kind == Kind::Search && l.query.is_empty() {
        "Search: press / or Ctrl+F to type".to_string()
    } else {
        format!("{} ({} {})", l.title, l.total, if l.total == 1 { "song" } else { "songs" })
    };
    sf.fill(lx, ly, lw, ROW_H, if app.typing { pl.selected_bg } else { mix(pl.normal_bg, pl.selected_bg) });
    sf.text(&wide(&header), row_rect(0), pl.current, DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS);

    let playing_uri = app.pb.as_ref().and_then(|p| p.track.as_ref()).map(|t| t.uri.as_str());
    let rows = pl_rows(app);
    let num_w = sf.text_width(&wide(&format!("{}.", l.tracks.len().max(1)))) + 4;
    for (i, t) in l.tracks.iter().enumerate().skip(l.top).take(rows) {
        let r = row_rect((i - l.top) as i32 + 1);
        if i == l.sel {
            sf.fill(lx, r.top, lw, ROW_H, pl.selected_bg);
        }
        let color = if Some(t.uri.as_str()) == playing_uri { pl.current } else { pl.normal };
        let dur = if t.duration_ms > 0 { fmt_time(t.duration_ms) } else { String::new() };
        let dw = sf.text_width(&wide(&dur));
        sf.text(&wide(&format!("{}.", i + 1)), RECT { right: r.left + num_w - 4, ..r }, color, DT_RIGHT | DT_VCENTER);
        let label = if t.playable { t.label() } else { format!("{} (unavailable)", t.label()) };
        sf.text(&wide(&label), RECT { left: r.left + num_w, right: r.right - dw - 6, ..r }, color, DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS);
        sf.text(&wide(&dur), r, color, DT_RIGHT | DT_VCENTER);
    }
}

/// The playlist's mini clock alone: what the once-a-second tick repaints.
pub fn draw_pl_clock(app: &App, sf: &mut Surface) {
    let (rx, ry) = (app.ui.pl_w - 150, app.ui.pl_h - 38);
    sf.blit(app.skin.pledit.as_ref(), 126 + 66, 72 + 23, 25, 6, rx + 66, ry + 23);
    if let Some(p) = app.pb.as_ref().filter(|p| p.track.is_some()) {
        let t = fmt_time(match app.hold { Hold::Seek(x) => x, _ => p.progress(Instant::now()) });
        bitmap_text(sf, app.skin.text.as_ref(), &format!("{t:>5}"), rx + 66, ry + 23, 0, 25);
    }
}

/// Halfway between two colours (the header row's band).
fn mix(a: u32, b: u32) -> u32 {
    let ch = |s: u32| (((a >> s) & 0xFF) + ((b >> s) & 0xFF)) / 2;
    ch(16) << 16 | ch(8) << 8 | ch(0)
}
