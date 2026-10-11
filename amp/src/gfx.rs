//! A window's back buffer: a 32-bit DIB section at the window's real pixel size.
//!
//! Everything is laid out in the skin's own coordinates (logical pixels, 275x116 for the
//! main window) and drawn at the window's zoom (`pct`, 100 = 1:1). Sprites are scaled
//! with nearest-neighbour as they're copied, so they stay crisp at any size; text is
//! drawn with GDI at the real size, so it is sharp rather than blown up. Painting is
//! then a plain 1:1 copy of the dirty rectangle.
//!
//! Logical x maps to physical pixels [phys(x), phys(x + 1)), with phys(x) = ceil(x * pct
//! / 100); a physical pixel p belongs to logical floor(p * 100 / pct). Every blit uses the
//! same mapping, so tiles meet without seams at any zoom.

use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{COLORREF, RECT};
use windows_sys::Win32::Graphics::Gdi::*;

use crate::skin::Bitmap;

pub const MIN_PCT: i32 = 100;
pub const MAX_PCT: i32 = 300;

/// The first physical pixel of logical coordinate `v`.
pub fn phys(v: i32, pct: i32) -> i32 {
    -((-v * pct).div_euclid(100))
}

/// The logical coordinate a physical pixel belongs to.
pub fn logical(p: i32, pct: i32) -> i32 {
    (p * 100).div_euclid(pct)
}

pub fn phys_rect(r: &RECT, pct: i32) -> RECT {
    RECT { left: phys(r.left, pct), top: phys(r.top, pct), right: phys(r.right, pct), bottom: phys(r.bottom, pct) }
}

pub struct Surface {
    pub dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    pub w: i32,  // physical size
    pub h: i32,
    pub lw: i32, // logical size
    pub lh: i32,
    pub pct: i32,
    cols: Vec<i32>, // per-blit column map, kept to avoid allocating
}

impl Surface {
    pub fn new(lw: i32, lh: i32, pct: i32) -> Surface {
        let mut s = Surface::empty();
        s.dc = unsafe { CreateCompatibleDC(null_mut()) };
        unsafe {
            SetBkMode(s.dc, TRANSPARENT as _);
        }
        s.resize(lw, lh, pct);
        s
    }

    /// A placeholder with no GDI objects, swapped in while the real one is drawn.
    pub fn empty() -> Surface {
        Surface { dc: null_mut(), bmp: null_mut(), old: null_mut(), bits: null_mut(), w: 0, h: 0, lw: 0, lh: 0, pct: 100, cols: Vec::new() }
    }

    /// Whether the buffer matches this logical size and zoom.
    pub fn fits(&self, lw: i32, lh: i32, pct: i32) -> bool {
        !self.bmp.is_null() && self.lw == lw && self.lh == lh && self.pct == pct
    }

    pub fn resize(&mut self, lw: i32, lh: i32, pct: i32) {
        if self.dc.is_null() || self.fits(lw, lh, pct) {
            return;
        }
        let pct = pct.clamp(MIN_PCT, MAX_PCT);
        let (w, h) = (phys(lw.max(1), pct), phys(lh.max(1), pct));
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down
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
        }
        (self.w, self.h, self.lw, self.lh, self.pct) = (w, h, lw.max(1), lh.max(1), pct);
    }

    fn px(&mut self) -> &mut [u32] {
        if self.bits.is_null() {
            return &mut [];
        }
        unsafe {
            GdiFlush(); // finish any queued GDI text before touching pixels
            std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize)
        }
    }

    /// Copy logical `w`x`h` from `src` at (sx, sy) to (dx, dy), clipped on both sides and
    /// scaled to the zoom.
    pub fn blit(&mut self, src: Option<&Bitmap>, sx: i32, sy: i32, w: i32, h: i32, dx: i32, dy: i32) {
        let Some(src) = src else { return };
        let (mut sx, mut sy, mut dx, mut dy, mut w, mut h) = (sx, sy, dx, dy, w, h);
        if sx < 0 { w += sx; dx -= sx; sx = 0; }
        if sy < 0 { h += sy; dy -= sy; sy = 0; }
        if dx < 0 { w += dx; sx -= dx; dx = 0; }
        if dy < 0 { h += dy; sy -= dy; dy = 0; }
        w = w.min(src.w - sx).min(self.lw - dx);
        h = h.min(src.h - sy).min(self.lh - dy);
        if w <= 0 || h <= 0 || self.bits.is_null() {
            return;
        }
        let (pct, dw) = (self.pct, self.w as usize);
        if pct == 100 {
            let px = self.px();
            for row in 0..h {
                let s = ((sy + row) * src.w + sx) as usize;
                let d = (dy + row) as usize * dw + dx as usize;
                px[d..d + w as usize].copy_from_slice(&src.px[s..s + w as usize]);
            }
            return;
        }
        let (x0, x1) = (phys(dx, pct), phys(dx + w, pct).min(self.w));
        let (y0, y1) = (phys(dy, pct), phys(dy + h, pct).min(self.h));
        let mut cols = std::mem::take(&mut self.cols);
        cols.clear();
        cols.extend((x0..x1).map(|p| sx + (logical(p, pct) - dx).min(w - 1)));
        let px = self.px();
        for p in y0..y1 {
            let srow = ((sy + (logical(p, pct) - dy).min(h - 1)) * src.w) as usize;
            let d = p as usize * dw + x0 as usize;
            for (out, &c) in px[d..d + cols.len()].iter_mut().zip(&cols) {
                *out = src.px[srow + c as usize];
            }
        }
        self.cols = cols;
    }

    /// Tile a sprite across a logical rectangle (playlist edges).
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
        let pct = self.pct;
        let (x0, y0) = (phys(x.max(0), pct), phys(y.max(0), pct));
        let (x1, y1) = (phys(x + w, pct).min(self.w), phys(y + h, pct).min(self.h));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let dw = self.w;
        let px = self.px();
        for row in y0..y1 {
            px[(row * dw + x0) as usize..(row * dw + x1) as usize].fill(color);
        }
    }

    /// GDI text in the logical `rect`, clipped to it. `flags` are DrawText flags.
    pub fn text(&mut self, s: &[u16], rect: RECT, color: u32, flags: u32) {
        let mut r = phys_rect(&rect, self.pct);
        unsafe {
            SetTextColor(self.dc, rgb(color));
            DrawTextW(self.dc, s.as_ptr(), s.len() as i32, &mut r, flags | DT_SINGLELINE | DT_NOPREFIX);
        }
    }

    /// Text drawn from `x` (logical, may be left of the clip) and cut to the logical
    /// clip rectangle: for scrolling text.
    pub fn text_clipped(&mut self, s: &[u16], x: i32, clip: RECT, color: u32) {
        let pct = self.pct;
        let c = phys_rect(&clip, pct);
        unsafe {
            let rgn = CreateRectRgn(c.left, c.top, c.right, c.bottom);
            SelectClipRgn(self.dc, rgn);
            let mut r = RECT { left: phys(x, pct), top: c.top, right: phys(x, pct) + 30_000, bottom: c.bottom };
            SetTextColor(self.dc, rgb(color));
            DrawTextW(self.dc, s.as_ptr(), s.len() as i32, &mut r, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
            SelectClipRgn(self.dc, null_mut());
            DeleteObject(rgn);
        }
    }

    /// The logical width of some text in the selected font.
    pub fn text_width(&self, s: &[u16]) -> i32 {
        let mut size = windows_sys::Win32::Foundation::SIZE { cx: 0, cy: 0 };
        unsafe { GetTextExtentPoint32W(self.dc, s.as_ptr(), s.len() as i32, &mut size) };
        (size.cx * 100 + self.pct - 1) / self.pct
    }

    pub fn select_font(&self, font: HFONT) {
        unsafe { SelectObject(self.dc, font) };
    }

    /// Copy the physical rect `r` to the window, 1:1.
    pub fn present(&self, hdc: HDC, r: &RECT) {
        unsafe {
            GdiFlush();
            BitBlt(hdc, r.left, r.top, r.right - r.left, r.bottom - r.top, self.dc, r.left, r.top, SRCCOPY);
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

/// A font `height` physical pixels tall. At 1:1 it is drawn without smoothing, crisp
/// like Winamp's; zoomed, with ClearType, since there are pixels to spare.
pub fn make_font(face: &str, height: i32, smooth: bool) -> HFONT {
    let mut lf: LOGFONTW = unsafe { std::mem::zeroed() };
    lf.lfHeight = -height.max(6);
    lf.lfWeight = FW_NORMAL as i32;
    lf.lfCharSet = DEFAULT_CHARSET as u8;
    lf.lfQuality = if smooth { CLEARTYPE_QUALITY as u8 } else { NONANTIALIASED_QUALITY as u8 };
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

/// Draw `s` with the skin font at logical (x, y), starting `skip` pixels into the text
/// and clipped to `clip_w` pixels. Characters it can't draw show as spaces.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_is_consistent() {
        for pct in [100, 125, 150, 175, 200, 250, 300] {
            for x in 0..400 {
                // Every logical pixel gets at least one physical pixel, and they round-trip.
                assert!(phys(x + 1, pct) > phys(x, pct));
                assert_eq!(logical(phys(x, pct), pct), x);
                assert_eq!(logical(phys(x + 1, pct) - 1, pct), x);
            }
            assert_eq!(logical(phys(275, pct), pct), 275);
        }
        assert_eq!(phys(-3, 150), -4);
    }
}
