//! A window's back buffer: a 32-bit DIB section at the skin's own (1x) resolution.
//!
//! Sprites are copied straight into its memory, text is drawn with GDI into the same
//! bitmap, and painting stretches the dirty part to the window with nearest-neighbour
//! scaling (double size), as Winamp does. Nothing is allocated per frame.

use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{COLORREF, RECT};
use windows_sys::Win32::Graphics::Gdi::*;

use crate::skin::Bitmap;

pub struct Surface {
    pub dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    pub w: i32,
    pub h: i32,
}

impl Surface {
    pub fn new(w: i32, h: i32) -> Surface {
        let mut s = Surface { dc: unsafe { CreateCompatibleDC(null_mut()) }, bmp: null_mut(), old: null_mut(), bits: null_mut(), w: 0, h: 0 };
        unsafe {
            SetBkMode(s.dc, TRANSPARENT as _);
        }
        s.resize(w, h);
        s
    }

    /// A placeholder with no GDI objects, swapped in while the real one is drawn.
    pub fn empty() -> Surface {
        Surface { dc: null_mut(), bmp: null_mut(), old: null_mut(), bits: null_mut(), w: 0, h: 0 }
    }

    pub fn resize(&mut self, w: i32, h: i32) {
        if self.dc.is_null() || (w == self.w && h == self.h && !self.bmp.is_null()) {
            return;
        }
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w.max(1),
                    biHeight: -h.max(1), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                },
                bmiColors: [std::mem::zeroed()],
            };
            let mut bits: *mut core::ffi::c_void = null_mut();
            let bmp = CreateDIBSection(self.dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bmp.is_null() || bits.is_null() {
                return; // out of GDI memory: keep the old buffer
            }
            let prev = SelectObject(self.dc, bmp);
            if self.bmp.is_null() {
                self.old = prev;
            } else {
                DeleteObject(self.bmp);
            }
            self.bmp = bmp;
            self.bits = bits as *mut u32;
            self.w = w.max(1);
            self.h = h.max(1);
        }
    }

    fn px(&mut self) -> &mut [u32] {
        unsafe {
            GdiFlush(); // finish any queued GDI text before touching pixels
            std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize)
        }
    }

    /// Copy `w`x`h` from `src` at (sx, sy) to (dx, dy), clipped on both sides.
    pub fn blit(&mut self, src: Option<&Bitmap>, sx: i32, sy: i32, w: i32, h: i32, dx: i32, dy: i32) {
        let Some(src) = src else { return };
        let (mut sx, mut sy, mut dx, mut dy, mut w, mut h) = (sx, sy, dx, dy, w, h);
        if sx < 0 { w += sx; dx -= sx; sx = 0; }
        if sy < 0 { h += sy; dy -= sy; sy = 0; }
        if dx < 0 { w += dx; sx -= dx; dx = 0; }
        if dy < 0 { h += dy; sy -= dy; dy = 0; }
        w = w.min(src.w - sx).min(self.w - dx);
        h = h.min(src.h - sy).min(self.h - dy);
        if w <= 0 || h <= 0 {
            return;
        }
        let dw = self.w;
        let px = self.px();
        for row in 0..h {
            let s = ((sy + row) * src.w + sx) as usize;
            let d = ((dy + row) * dw + dx) as usize;
            px[d..d + w as usize].copy_from_slice(&src.px[s..s + w as usize]);
        }
    }

    /// Tile a sprite across a rectangle (playlist edges).
    pub fn tile(&mut self, src: Option<&Bitmap>, sx: i32, sy: i32, sw: i32, sh: i32, dx: i32, dy: i32, w: i32, h: i32) {
        let mut y = 0;
        while y < h {
            let mut x = 0;
            while x < w {
                self.blit(src, sx, sy, sw.min(w - x), sh.min(h - y), dx + x, dy + y);
                x += sw;
            }
            y += sh;
        }
    }

    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let dw = self.w;
        let px = self.px();
        for row in y0..y1 {
            px[(row * dw + x0) as usize..(row * dw + x1) as usize].fill(color);
        }
    }

    /// GDI text in `rect`, clipped. `flags` are DrawText flags.
    pub fn text(&mut self, s: &[u16], rect: RECT, color: u32, flags: u32) {
        let mut r = rect;
        unsafe {
            SetTextColor(self.dc, rgb(color));
            DrawTextW(self.dc, s.as_ptr(), s.len() as i32, &mut r, flags | DT_SINGLELINE | DT_NOPREFIX);
        }
    }

    pub fn text_width(&self, s: &[u16]) -> i32 {
        let mut size = windows_sys::Win32::Foundation::SIZE { cx: 0, cy: 0 };
        unsafe { GetTextExtentPoint32W(self.dc, s.as_ptr(), s.len() as i32, &mut size) };
        size.cx
    }

    pub fn select_font(&self, font: HFONT) {
        unsafe { SelectObject(self.dc, font) };
    }

    /// Stretch the logical rect `r` (1x coordinates) to the window at `scale`.
    pub fn present(&self, hdc: HDC, r: &RECT, scale: i32) {
        unsafe {
            GdiFlush();
            if scale == 1 {
                BitBlt(hdc, r.left, r.top, r.right - r.left, r.bottom - r.top, self.dc, r.left, r.top, SRCCOPY);
            } else {
                SetStretchBltMode(hdc, COLORONCOLOR as _);
                StretchBlt(hdc, r.left * scale, r.top * scale, (r.right - r.left) * scale, (r.bottom - r.top) * scale,
                           self.dc, r.left, r.top, r.right - r.left, r.bottom - r.top, SRCCOPY);
            }
        }
    }

    /// The pixels, for the --snapshot self-check.
    pub fn pixels(&mut self) -> Vec<u32> {
        self.px().to_vec()
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        if self.dc.is_null() {
            return;
        }
        unsafe {
            if !self.old.is_null() {
                SelectObject(self.dc, self.old);
            }
            if !self.bmp.is_null() {
                DeleteObject(self.bmp);
            }
            DeleteDC(self.dc);
        }
    }
}

/// 0x00RRGGBB to GDI's 0x00BBGGRR.
pub fn rgb(c: u32) -> COLORREF {
    (c & 0xFF) << 16 | (c & 0xFF00) | (c >> 16) & 0xFF
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub fn wide_z(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn make_font(face: &str, height: i32, bold: bool) -> HFONT {
    let mut lf: LOGFONTW = unsafe { std::mem::zeroed() };
    lf.lfHeight = -height;
    lf.lfWeight = if bold { FW_BOLD as i32 } else { FW_NORMAL as i32 };
    lf.lfCharSet = DEFAULT_CHARSET as u8;
    lf.lfQuality = NONANTIALIASED_QUALITY as u8; // crisp at 1x and when doubled, like Winamp
    for (i, c) in face.encode_utf16().take(31).enumerate() {
        lf.lfFaceName[i] = c;
    }
    unsafe { CreateFontIndirectW(&lf) }
}

// ── The skin's bitmap font (text.bmp, 5x6 glyphs) ────────────────────────────
/// Where a character's glyph is in text.bmp, or None if the font can't draw it.
pub fn glyph(c: char) -> Option<(i32, i32)> {
    let c = fold(c);
    let row0 = "abcdefghijklmnopqrstuvwxyz\"@";
    let row1 = "0123456789\u{2026}.:()-'!_+\\/[]^&%,=$#";
    let row2 = "\u{c5}\u{d6}\u{c4}?*";
    let lc = c.to_ascii_lowercase();
    if c == ' ' {
        return Some((30 * 5, 0));
    }
    if let Some(i) = row0.chars().position(|x| x == lc) {
        return Some((i as i32 * 5, 0));
    }
    if let Some(i) = row1.chars().position(|x| x == c) {
        return Some((i as i32 * 5, 6));
    }
    if let Some(i) = row2.chars().position(|x| x == c.to_uppercase().next().unwrap_or(c)) {
        return Some((i as i32 * 5, 12));
    }
    match c {
        '<' | '{' => glyph('('),
        '>' | '}' => glyph(')'),
        ';' => glyph(':'),
        '|' => glyph('!'),
        '~' => glyph('-'),
        '`' | '\u{2019}' | '\u{2018}' => glyph('\''),
        '\u{201c}' | '\u{201d}' => glyph('"'),
        '\u{2013}' | '\u{2014}' => glyph('-'),
        _ => None,
    }
}

/// Accented Latin letters to their plain form, so most European titles still use the
/// skin's font.
fn fold(c: char) -> char {
    const FROM: &str = "àáâãāăąçćčďèéêëēėęěìíîïīįłñńňòóôõøōőŕřśšşťùúûüūůűųýÿźżžÀÁÂÃĀĂĄÇĆČĎÈÉÊËĒĖĘĚÌÍÎÏĪĮŁÑŃŇÒÓÔÕØŌŐŔŘŚŠŞŤÙÚÛÜŪŮŰŲÝŸŹŻŽß";
    const TO: &str = "aaaaaaacccdeeeeeeeeiiiiiilnnnoooooooorrsssstuuuuuuuuyyzzzAAAAAAACCCDEEEEEEEEIIIIIILNNNOOOOOOOORRSSSTUUUUUUUUYYZZZs";
    if c.is_ascii() || matches!(c, 'å' | 'ö' | 'ä' | 'Å' | 'Ö' | 'Ä' | '\u{2026}') {
        return c;
    }
    FROM.chars().position(|x| x == c).and_then(|i| TO.chars().nth(i)).unwrap_or(c)
}

/// Whether the skin's font can draw all of `s`.
pub fn bitmap_font_can(s: &str) -> bool {
    s.chars().all(|c| glyph(c).is_some())
}

/// Draw `s` with the skin font at (x, y), starting `skip` pixels into the text and
/// clipped to `clip_w` pixels. Characters it can't draw show as spaces.
pub fn bitmap_text(sf: &mut Surface, font: Option<&Bitmap>, s: &str, x: i32, y: i32, skip: i32, clip_w: i32) {
    let Some(font) = font else { return };
    let mut px = -skip;
    for c in s.chars() {
        if px >= clip_w {
            break;
        }
        if px + 5 > 0 {
            let (gx, gy) = glyph(c).unwrap_or((150, 0));
            let (from, to) = ((-px).max(0), (clip_w - px).min(5));
            if to > from {
                sf.blit(Some(font), gx + from, gy, to - from, 6, x + px + from, y);
            }
        }
        px += 5;
    }
}
