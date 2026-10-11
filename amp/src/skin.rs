//! Classic Winamp skins (.wsz): a zip of BMP sprite sheets plus a few text files.
//!
//! Only what the windows draw is decoded, once, at startup, into 0x00RRGGBB pixels.
//! The zip reader and BMP decoder are small and self-contained so that loading a skin
//! pulls in nothing but an inflater.

use std::collections::HashMap;

/// The skin shipped inside the exe (the reference skin).
pub const DEFAULT: &[u8] = include_bytes!("../skins/Winamp5_Classified_v5.5.wsz");

const MAX_FILE: usize = 8 << 20; // a skin file bigger than this is refused

pub struct Bitmap {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>, // top-down rows, 0x00RRGGBB
}

impl Bitmap {
    pub fn at(&self, x: i32, y: i32) -> Option<u32> {
        (x >= 0 && y >= 0 && x < self.w && y < self.h).then(|| self.px[(y * self.w + x) as usize])
    }
}

/// Playlist editor colours (pledit.txt), as 0x00RRGGBB.
pub struct PlColors {
    pub normal: u32,
    pub current: u32,
    pub normal_bg: u32,
    pub selected_bg: u32,
    pub font: String,
}

pub struct Skin {
    pub main: Option<Bitmap>,
    pub titlebar: Option<Bitmap>,
    pub cbuttons: Option<Bitmap>,
    pub shufrep: Option<Bitmap>,
    pub posbar: Option<Bitmap>,
    pub volume: Option<Bitmap>,
    pub balance: Option<Bitmap>,
    pub numbers: Option<Bitmap>,
    /// nums_ex.bmp has a real minus sign; numbers.bmp draws it from a slice of the blank.
    pub nums_ex: bool,
    pub text: Option<Bitmap>,
    pub playpaus: Option<Bitmap>,
    pub monoster: Option<Bitmap>,
    pub pledit: Option<Bitmap>,
    pub pl: PlColors,
    /// region.txt [Normal]: polygons that shape the main window.
    pub region: Vec<Vec<(i32, i32)>>,
    /// The marquee's colours, read from text.bmp, for titles its bitmap font can't spell.
    pub text_fg: u32,
    pub text_bg: u32,
}

impl Skin {
    pub fn load(wsz: &[u8]) -> Result<Skin, String> {
        let files = unzip(wsz)?;
        let bmp = |name: &str| files.get(name).and_then(|d| decode_bmp(d));
        let txt = |name: &str| files.get(name).map(|d| String::from_utf8_lossy(d).into_owned());
        let (numbers, nums_ex) = match bmp("nums_ex.bmp") {
            Some(b) => (Some(b), true),
            None => (bmp("numbers.bmp"), false),
        };
        let text = bmp("text.bmp");
        let (text_fg, text_bg) = text.as_ref().map(text_colors).unwrap_or((0x00FF00, 0));
        let main = bmp("main.bmp");
        if main.is_none() {
            return Err("the skin has no main.bmp".into());
        }
        Ok(Skin {
            main,
            titlebar: bmp("titlebar.bmp"),
            cbuttons: bmp("cbuttons.bmp"),
            shufrep: bmp("shufrep.bmp"),
            posbar: bmp("posbar.bmp"),
            volume: bmp("volume.bmp"),
            balance: bmp("balance.bmp").or_else(|| bmp("volume.bmp")),
            numbers,
            nums_ex,
            text,
            playpaus: bmp("playpaus.bmp"),
            monoster: bmp("monoster.bmp"),
            pledit: bmp("pledit.bmp"),
            pl: txt("pledit.txt").map(|t| parse_pledit(&t)).unwrap_or_else(default_pl),
            region: txt("region.txt").map(|t| parse_region(&t)).unwrap_or_default(),
            text_fg,
            text_bg,
        })
    }
}

/// Foreground: the most common colour in the letter "A" that isn't the background.
/// Background: the pixel at the space glyph (column 30 of row 0).
fn text_colors(b: &Bitmap) -> (u32, u32) {
    let bg = b.at(152, 2).unwrap_or(0);
    let mut counts: HashMap<u32, u32> = HashMap::new();
    for y in 0..6 {
        for x in 0..5 {
            if let Some(c) = b.at(x, y) {
                if c != bg {
                    *counts.entry(c).or_default() += 1;
                }
            }
        }
    }
    let fg = counts.into_iter().max_by_key(|&(_, n)| n).map(|(c, _)| c).unwrap_or(0xFFFFFF);
    (fg, bg)
}

fn default_pl() -> PlColors {
    PlColors { normal: 0x00FF00, current: 0xFFFFFF, normal_bg: 0x000000, selected_bg: 0x0000C6, font: "Arial".into() }
}

fn parse_pledit(text: &str) -> PlColors {
    let mut pl = default_pl();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        let color = || {
            let hex = v.trim_start_matches('#');
            (hex.len() >= 6).then(|| u32::from_str_radix(&hex[..6], 16).ok()).flatten()
        };
        match k.as_str() {
            "normal" => pl.normal = color().unwrap_or(pl.normal),
            "current" => pl.current = color().unwrap_or(pl.current),
            "normalbg" => pl.normal_bg = color().unwrap_or(pl.normal_bg),
            "selectedbg" => pl.selected_bg = color().unwrap_or(pl.selected_bg),
            "font" if !v.is_empty() && v.len() < 32 => pl.font = v.to_string(),
            _ => {}
        }
    }
    pl
}

/// The [Normal] section: NumPoints=4,4,... and PointList=x,y x,y ...
fn parse_region(text: &str) -> Vec<Vec<(i32, i32)>> {
    let mut in_normal = false;
    let (mut counts, mut points) = (Vec::new(), Vec::new());
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_normal = line.eq_ignore_ascii_case("[normal]");
            continue;
        }
        if !in_normal {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let nums = || v.split(|c: char| c == ',' || c.is_whitespace()).filter_map(|s| s.trim().parse::<i32>().ok());
        if k.trim().eq_ignore_ascii_case("numpoints") {
            counts = nums().collect();
        } else if k.trim().eq_ignore_ascii_case("pointlist") {
            points = nums().collect();
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    for &n in &counts {
        let n = n.clamp(0, 1000) as usize;
        if n < 3 || i + n * 2 > points.len() {
            break;
        }
        out.push((0..n).map(|k| (points[i + k * 2].clamp(0, 2000), points[i + k * 2 + 1].clamp(0, 2000))).collect());
        i += n * 2;
    }
    out
}

// ── Zip ──────────────────────────────────────────────────────────────────────
fn u16le(d: &[u8], at: usize) -> Option<usize> {
    d.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
}

fn u32le(d: &[u8], at: usize) -> Option<usize> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

/// Every file in the archive, by lowercase base name (skins are sometimes zipped with
/// their folder). Only stored and deflated entries; anything else is skipped.
fn unzip(d: &[u8]) -> Result<HashMap<String, Vec<u8>>, String> {
    let eocd = (0..d.len().saturating_sub(21))
        .rev()
        .take(65557)
        .find(|&i| d[i..].starts_with(b"PK\x05\x06"))
        .ok_or("not a zip file")?;
    let entries = u16le(d, eocd + 10).ok_or("bad zip")?;
    let mut at = u32le(d, eocd + 16).ok_or("bad zip")?;
    let mut out = HashMap::new();
    for _ in 0..entries.min(1000) {
        if !d.get(at..).is_some_and(|s| s.starts_with(b"PK\x01\x02")) {
            break;
        }
        let bad = || "bad zip entry".to_string();
        let method = u16le(d, at + 10).ok_or_else(bad)?;
        let csize = u32le(d, at + 20).ok_or_else(bad)?;
        let usize_ = u32le(d, at + 24).ok_or_else(bad)?;
        let (nlen, xlen, clen) = (u16le(d, at + 28).ok_or_else(bad)?, u16le(d, at + 30).ok_or_else(bad)?, u16le(d, at + 32).ok_or_else(bad)?);
        let local = u32le(d, at + 42).ok_or_else(bad)?;
        let name = d.get(at + 46..at + 46 + nlen).ok_or_else(bad)?;
        at += 46 + nlen + xlen + clen;
        let name = String::from_utf8_lossy(name).replace('\\', "/");
        let base = name.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
        if base.is_empty() || usize_ > MAX_FILE || !d.get(local..).is_some_and(|s| s.starts_with(b"PK\x03\x04")) {
            continue;
        }
        let start = local + 30 + u16le(d, local + 26).ok_or_else(bad)? + u16le(d, local + 28).ok_or_else(bad)?;
        let Some(raw) = d.get(start..start + csize) else { continue };
        let data = match method {
            0 => raw.to_vec(),
            8 => match miniz_oxide::inflate::decompress_to_vec_with_limit(raw, MAX_FILE) {
                Ok(v) => v,
                Err(_) => continue,
            },
            _ => continue,
        };
        out.entry(base).or_insert(data);
    }
    Ok(out)
}

// ── BMP ──────────────────────────────────────────────────────────────────────
/// Uncompressed 1/4/8/16/24/32-bit and RLE4/RLE8 BMPs, which covers classic skins.
pub fn decode_bmp(d: &[u8]) -> Option<Bitmap> {
    if !d.starts_with(b"BM") {
        return None;
    }
    let off = u32le(d, 10)?;
    let hs = u32le(d, 14)?;
    if hs < 40 {
        return None;
    }
    let w = i32::from_le_bytes(d.get(18..22)?.try_into().ok()?);
    let h_raw = i32::from_le_bytes(d.get(22..26)?.try_into().ok()?);
    let bpp = u16le(d, 28)?;
    let comp = u32le(d, 30)?;
    let (h, bottom_up) = (h_raw.unsigned_abs() as i32, h_raw > 0);
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
        return None;
    }
    let mut ncol = u32le(d, 46)?;
    if bpp <= 8 && ncol == 0 {
        ncol = 1 << bpp;
    }
    let pal_at = 14 + hs;
    let palette: Vec<u32> = (0..ncol.min(256))
        .map(|i| {
            let p = pal_at + i * 4;
            d.get(p..p + 3).map(|c| (c[2] as u32) << 16 | (c[1] as u32) << 8 | c[0] as u32).unwrap_or(0)
        })
        .collect();
    let pal = |i: usize| palette.get(i).copied().unwrap_or(0);
    let mut px = vec![0u32; (w * h) as usize];
    let row_of = |y: i32| if bottom_up { h - 1 - y } else { y };

    match (comp, bpp) {
        (1, 8) | (2, 4) => {
            // RLE: decoded straight into the image (bottom-up by definition).
            let (mut x, mut y, mut i) = (0i32, 0i32, off);
            let mut put = |x: i32, y: i32, c: u32| {
                if x < w && y < h {
                    px[((h - 1 - y) * w + x) as usize] = c;
                }
            };
            while i + 1 < d.len() && y < h {
                let (n, v) = (d[i] as usize, d[i + 1] as usize);
                i += 2;
                if n > 0 {
                    for k in 0..n {
                        let idx = if bpp == 8 { v } else if k % 2 == 0 { v >> 4 } else { v & 15 };
                        put(x, y, pal(idx));
                        x += 1;
                    }
                    continue;
                }
                match v {
                    0 => { x = 0; y += 1; }
                    1 => break,
                    2 => {
                        x += *d.get(i)? as i32;
                        y += *d.get(i + 1)? as i32;
                        i += 2;
                    }
                    n => {
                        let bytes = if bpp == 8 { n } else { n.div_ceil(2) };
                        for k in 0..n {
                            let idx = if bpp == 8 { *d.get(i + k)? as usize } else {
                                let b = *d.get(i + k / 2)? as usize;
                                if k % 2 == 0 { b >> 4 } else { b & 15 }
                            };
                            put(x, y, pal(idx));
                            x += 1;
                        }
                        i += bytes.div_ceil(2) * 2; // runs are word-aligned
                    }
                }
            }
        }
        (0 | 3, 1 | 4 | 8 | 16 | 24 | 32) => {
            let stride = ((w as usize * bpp + 31) / 32) * 4;
            // 16/32-bit masks: BI_BITFIELDS stores them after the 40-byte header.
            let masks = if comp == 3 {
                Some((u32le(d, 54)? as u32, u32le(d, 58)? as u32, u32le(d, 62)? as u32))
            } else if bpp == 16 {
                Some((0x7C00, 0x03E0, 0x001F))
            } else {
                None
            };
            let chan = |v: u32, m: u32| -> u32 {
                if m == 0 { return 0; }
                let shift = m.trailing_zeros();
                let max = m >> shift;
                ((v & m) >> shift) * 255 / max.max(1)
            };
            for y in 0..h {
                let row = d.get(off + row_of(y) as usize * stride..)?;
                let out = &mut px[(y * w) as usize..((y + 1) * w) as usize];
                for x in 0..w as usize {
                    out[x] = match bpp {
                        1 => pal((*row.get(x / 8)? as usize >> (7 - x % 8)) & 1),
                        4 => pal((*row.get(x / 2)? as usize >> if x % 2 == 0 { 4 } else { 0 }) & 15),
                        8 => pal(*row.get(x)? as usize),
                        24 => {
                            let c = row.get(x * 3..x * 3 + 3)?;
                            (c[2] as u32) << 16 | (c[1] as u32) << 8 | c[0] as u32
                        }
                        16 | 32 => {
                            let v = if bpp == 16 {
                                u16le(row, x * 2)? as u32
                            } else {
                                u32le(row, x * 4)? as u32
                            };
                            match masks {
                                Some((r, g, b)) if bpp == 16 || comp == 3 => chan(v, r) << 16 | chan(v, g) << 8 | chan(v, b),
                                _ => v & 0xFFFFFF,
                            }
                        }
                        _ => 0,
                    };
                }
            }
        }
        _ => return None,
    }
    Some(Bitmap { w, h, px })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_skin_loads() {
        let s = Skin::load(DEFAULT).expect("default skin");
        let m = s.main.as_ref().unwrap();
        assert_eq!((m.w, m.h), (275, 116));
        assert!(s.pledit.is_some() && s.cbuttons.is_some() && s.text.is_some());
        assert_eq!(s.region.len(), 7);
        assert_eq!(s.pl.normal_bg, 0x1C3D7D);
        assert!(s.nums_ex);
    }
}
