//! Spotify Web API shapes, as small typed values (a port of spoterm/api.py).
//!
//! Nothing from Spotify is trusted: every field is type-checked, so malformed data shows
//! as blank or "Unknown", never a crash. Text is stripped of control characters.

use std::rc::Rc;
use std::time::Instant;

use serde_json::Value;

pub const PAGE_SIZE: u32 = 50;
pub const PLAYLIST_PAGE_SIZE: u32 = 100;
pub const SEARCH_PAGE_SIZE: u32 = 10; // Spotify rejects search limits above 10
const ITEM_FIELDS: &str = "uri,name,duration_ms,type,is_local,is_playable,linked_from(uri),artists(name),album(name),show(name)";

pub fn playlist_fields() -> String {
    format!("total,next,items(is_local,item({ITEM_FIELDS}),track({ITEM_FIELDS}))")
}

pub struct Track {
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    pub duration_ms: u32,
    pub pos: u32,
    pub playable: bool,
}

impl Track {
    pub fn id(&self) -> &str {
        self.uri.rsplit(':').next().unwrap_or("")
    }

    pub fn is_track(&self) -> bool {
        self.uri.starts_with("spotify:track:")
    }

    /// "Artist - Title", the way Winamp names a song.
    pub fn label(&self) -> String {
        if self.artists.is_empty() { self.name.clone() } else { format!("{} - {}", self.artists, self.name) }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    Off,
    Context,
    Track,
}

impl Repeat {
    pub fn parse(s: &str) -> Repeat {
        match s { "context" => Repeat::Context, "track" => Repeat::Track, _ => Repeat::Off }
    }

    pub fn as_str(self) -> &'static str {
        match self { Repeat::Off => "off", Repeat::Context => "context", Repeat::Track => "track" }
    }

    pub fn next(self) -> Repeat {
        match self { Repeat::Off => Repeat::Context, Repeat::Context => Repeat::Track, Repeat::Track => Repeat::Off }
    }
}

pub struct Playback {
    pub track: Option<Rc<Track>>,
    pub playing: bool,
    pub progress_ms: u32,
    pub at: Instant, // when progress_ms was true
    pub device_id: Option<String>,
    pub device_name: String,
    pub volume: Option<u8>,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub context_uri: Option<String>,
}

impl Playback {
    pub fn progress(&self, now: Instant) -> u32 {
        let mut ms = self.progress_ms as u64;
        if self.playing {
            ms += now.saturating_duration_since(self.at).as_millis() as u64;
        }
        let dur = self.track.as_ref().map_or(0, |t| t.duration_ms) as u64;
        (if dur > 0 { ms.min(dur) } else { ms }) as u32
    }

    /// Freeze the interpolated progress (before changing `playing`).
    pub fn rebase(&mut self, now: Instant) {
        self.progress_ms = self.progress(now);
        self.at = now;
    }
}

pub struct Playlist {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub total: u32,
}

pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub active: bool,
}

pub struct Page {
    pub tracks: Vec<Track>,
    pub total: u32,
    pub next_offset: Option<u32>,
}

/// Spotify's numbers, tolerating null, floats or strings.
pub fn num(v: &Value) -> u64 {
    match v {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)).unwrap_or(0),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// A string field with control characters removed (a title can't inject anything).
pub fn text(v: &Value) -> String {
    v.as_str().map(clean).unwrap_or_default()
}

pub fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// The maximum number of characters allowed in a search query.
pub const MAX_QUERY_CHARS: usize = 200;

/// Append `extra` to `dst`, keeping `dst` at no more than `max_chars` characters.
/// Counts chars (not bytes), so it can never split a UTF-8 sequence.
pub fn append_capped(dst: &mut String, extra: &str, max_chars: usize) {
    let room = max_chars.saturating_sub(dst.chars().count());
    dst.extend(extra.chars().take(room));
}

pub fn track(item: &Value, pos: u32, is_local: bool) -> Option<Track> {
    let uri = item.get("uri")?.as_str()?;
    // With a market, Spotify swaps in a regional copy of some tracks (relinking). Keep the
    // original URI: it's the one in the playlist and the one the player reports.
    let uri = item["linked_from"]["uri"].as_str().filter(|u| u.starts_with("spotify:")).unwrap_or(uri);
    let (artists, album) = if item["type"] == "episode" {
        (text(&item["show"]["name"]), String::new())
    } else {
        let names: Vec<String> = item["artists"].as_array().map(|a| a.iter().map(|x| text(&x["name"])).collect()).unwrap_or_default();
        (names.join(", "), text(&item["album"]["name"]))
    };
    let name = text(&item["name"]);
    Some(Track {
        uri: clean(uri),
        name: if name.is_empty() { "Unknown".into() } else { name },
        artists,
        album,
        duration_ms: num(&item["duration_ms"]).min(u32::MAX as u64) as u32,
        pos,
        playable: !(is_local || item["is_local"] == true || item["is_playable"] == false),
    })
}

/// One page of tracks; `get_item` picks the track out of each entry.
pub fn page(r: &Value, offset: u32, get_item: impl Fn(&Value) -> &Value) -> Page {
    let empty = Vec::new();
    let items = r["items"].as_array().unwrap_or(&empty);
    let tracks = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| track(get_item(it), offset + i as u32, it["is_local"] == true))
        .collect();
    let n = items.len() as u32;
    let total = (num(&r["total"]) as u32).max(offset + n);
    let next_offset = (r["next"].is_string() && n > 0).then_some(offset + n);
    Page { tracks, total, next_offset }
}

pub fn playlists(r: &Value, out: &mut Vec<Playlist>) {
    for p in r["items"].as_array().into_iter().flatten() {
        // The id goes into a request path, so only Spotify's base62 ids are taken.
        let Some(id) = p["id"].as_str().filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())) else { continue };
        let Some(uri) = p["uri"].as_str() else { continue };
        let counts = if p["tracks"].is_object() { &p["tracks"] } else { &p["items"] };
        let name = text(&p["name"]);
        out.push(Playlist {
            id: id.into(),
            uri: clean(uri),
            name: if name.is_empty() { "Untitled".into() } else { name },
            total: num(&counts["total"]) as u32,
        });
    }
}

pub fn devices(r: &Value) -> Vec<Device> {
    r["devices"].as_array().into_iter().flatten()
        .filter_map(|d| {
            let id = d["id"].as_str().filter(|s| !s.is_empty())?;
            let name = text(&d["name"]);
            Some(Device {
                id: clean(id),
                name: if name.is_empty() { "Unknown".into() } else { name },
                kind: text(&d["type"]),
                active: d["is_active"] == true,
            })
        })
        .collect()
}

pub fn playback(r: &Value, now: Instant) -> Option<Playback> {
    if !r.is_object() || r.as_object().is_some_and(|o| o.is_empty()) {
        return None;
    }
    let dev = &r["device"];
    let vol = &dev["volume_percent"];
    Some(Playback {
        track: track(&r["item"], 0, false).map(Rc::new),
        playing: r["is_playing"] == true,
        progress_ms: num(&r["progress_ms"]).min(u32::MAX as u64) as u32,
        at: now,
        device_id: dev["id"].as_str().map(clean),
        device_name: text(&dev["name"]),
        volume: (!vol.is_null() && dev["supports_volume"] != false).then(|| num(vol).min(100) as u8),
        shuffle: r["shuffle_state"] == true,
        repeat: Repeat::parse(r["repeat_state"].as_str().unwrap_or("off")),
        context_uri: r["context"]["uri"].as_str().map(clean),
    })
}

/// A failed call, from an `api` event.
pub struct ApiError {
    pub status: u16,
    pub msg: String,
    pub login: bool,
    pub retry_after: u64,
}

impl ApiError {
    pub fn from_event(ev: &Value) -> Option<ApiError> {
        let status = num(&ev["status"]).min(999) as u16;
        if ev.get("error").is_none() && (200..400).contains(&status) {
            return None;
        }
        Some(ApiError {
            status,
            msg: text(&ev["error"]),
            login: ev["body"]["login"] == true,
            retry_after: num(&ev["retry_after"]).min(86_400),
        })
    }

    pub fn no_device(&self) -> bool {
        self.status == 404 && self.msg.to_lowercase().contains("device")
    }

    /// A short, human-readable message (describe_error in api.py).
    pub fn describe(&self) -> String {
        if self.login {
            return "Spotify sign-in expired: choose Sign in again from the menu".into();
        }
        if self.status == 0 {
            return if self.msg.is_empty() { "Network error: can't reach Spotify".into() } else { self.msg.clone() };
        }
        let reason = self.msg.rsplit(':').next().unwrap_or("").trim();
        let reason = if reason.is_empty() { format!("HTTP {}", self.status) } else { reason.to_string() };
        match self.status {
            404 if self.no_device() => "No active device: pick one from the menu".into(),
            403 if self.msg.to_lowercase().contains("premium") => "Spotify Premium is required for playback".into(),
            403 => format!("Not allowed: {reason}"),
            429 => "Rate limited by Spotify, slowing down".into(),
            _ => reason,
        }
    }
}

pub fn fmt_time(ms: u32) -> String {
    let s = ms / 1000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn malformed_is_harmless() {
        assert!(track(&json!({"name": "x"}), 0, false).is_none());
        let t = track(&json!({"uri": "spotify:track:1", "name": "a\u{1b}[2Jb", "artists": [{"name": 3}, {"name": "Z"}],
                              "duration_ms": "12", "linked_from": {"uri": "spotify:track:0"}}), 4, false).unwrap();
        assert_eq!((t.uri.as_str(), t.name.as_str(), t.artists.as_str(), t.duration_ms, t.pos), ("spotify:track:0", "a[2Jb", ", Z", 12, 4));
        let p = page(&json!({"items": [{"track": {"uri": "spotify:track:2"}}, 5], "total": 1, "next": "x"}), 10, |i| &i["track"]);
        assert_eq!((p.tracks.len(), p.total, p.next_offset), (1, 12, Some(12)));
        assert!(playback(&json!({}), Instant::now()).is_none());
        let e = ApiError::from_event(&json!({"status": 404, "error": "Player command failed: No active device found"})).unwrap();
        assert!(e.no_device());
        assert!(ApiError::from_event(&json!({"status": 204, "body": null})).is_none());
    }

    // ── Step 0: prove the old logic panics ──────────────────────────────────
    /// Regression guard: the old `push_str` + `truncate(200)` panicked when byte
    /// 200 fell inside a multibyte character. This test reproduces that crash.
    #[test]
    #[should_panic(expected = "not on a char boundary")]
    fn old_truncate_panics_on_multibyte() {
        // 199 ASCII bytes + "😀" (4 bytes) → 203 bytes; byte index 200 is inside
        // the emoji, so `truncate(200)` must panic.
        let mut s = "a".repeat(199);
        s.push_str("😀");
        s.truncate(200);
    }

    // ── append_capped tests ─────────────────────────────────────────────────
    fn assert_valid(s: &str, max: usize) {
        assert!(s.chars().count() <= max, "char count {} exceeds {max}", s.chars().count());
        assert!(s.is_char_boundary(s.len()));
        assert_eq!(s, String::from_utf8(s.as_bytes().to_vec()).unwrap());
    }

    #[test]
    fn empty_paste() {
        let mut s = String::new();
        append_capped(&mut s, "", MAX_QUERY_CHARS);
        assert_eq!(s, "");
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn short_ascii() {
        let mut s = String::new();
        append_capped(&mut s, "abc", MAX_QUERY_CHARS);
        assert_eq!(s, "abc");
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn appends_to_existing() {
        let mut s = "ab".to_string();
        append_capped(&mut s, "cd", MAX_QUERY_CHARS);
        assert_eq!(s, "abcd");
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn exactly_at_cap() {
        let mut s = String::new();
        let extra: String = "x".repeat(200);
        append_capped(&mut s, &extra, MAX_QUERY_CHARS);
        assert_eq!(s.chars().count(), 200);
        assert_eq!(s, extra);
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn one_over_cap() {
        let mut s = String::new();
        let extra: String = "x".repeat(201);
        append_capped(&mut s, &extra, MAX_QUERY_CHARS);
        assert_eq!(s.chars().count(), 200);
        assert_eq!(s, "x".repeat(200));
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn far_over_cap() {
        let mut s = String::new();
        let extra: String = "x".repeat(5000);
        append_capped(&mut s, &extra, MAX_QUERY_CHARS);
        assert_eq!(s.chars().count(), 200);
        assert_eq!(s, "x".repeat(200));
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn two_byte_chars() {
        let mut s = String::new();
        let extra: String = "é".repeat(300);
        append_capped(&mut s, &extra, MAX_QUERY_CHARS);
        assert_eq!(s.chars().count(), 200);
        assert_eq!(s.len(), 400); // 2 bytes each
        assert_eq!(s, "é".repeat(200));
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn three_byte_chars_euro() {
        let mut s = String::new();
        let extra: String = "€".repeat(300);
        append_capped(&mut s, &extra, MAX_QUERY_CHARS);
        assert_eq!(s.chars().count(), 200);
        assert_eq!(s.len(), 600); // 3 bytes each
        assert_eq!(s, "€".repeat(200));
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn three_byte_chars_cjk() {
        let mut s = String::new();
        let extra: String = "日".repeat(300);
        append_capped(&mut s, &extra, MAX_QUERY_CHARS);
        assert_eq!(s.chars().count(), 200);
        assert_eq!(s.len(), 600); // 3 bytes each
        assert_eq!(s, "日".repeat(200));
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn four_byte_chars() {
        let mut s = String::new();
        let extra: String = "😀".repeat(300);
        append_capped(&mut s, &extra, MAX_QUERY_CHARS);
        assert_eq!(s.chars().count(), 200);
        assert_eq!(s.len(), 800); // 4 bytes each
        assert_eq!(s, "😀".repeat(200));
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn cap_lands_inside_multibyte_by_bytes() {
        // 199 ASCII + "😀😀" → by bytes that's 199 + 8 = 207, but by chars it's 201.
        // The function should keep exactly 200 chars: 199 + 1 emoji.
        let mut s = "a".repeat(199);
        append_capped(&mut s, "😀😀", MAX_QUERY_CHARS);
        assert_eq!(s.chars().count(), 200);
        let expected = format!("{}{}", "a".repeat(199), "😀");
        assert_eq!(s, expected);
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn mixed_widths() {
        let unit = "aé€😀"; // 4 chars, 10 bytes
        let extra: String = unit.repeat(250); // 1000 chars
        let mut s = String::new();
        append_capped(&mut s, &extra, MAX_QUERY_CHARS);
        assert_eq!(s.chars().count(), 200);
        let expected: String = extra.chars().take(200).collect();
        assert_eq!(s, expected);
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn existing_query_at_cap() {
        let mut s = "z".repeat(200);
        append_capped(&mut s, "x", MAX_QUERY_CHARS);
        assert_eq!(s, "z".repeat(200));
        assert_eq!(s.chars().count(), 200);
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn existing_query_multibyte_partially_full() {
        let mut s: String = "é".repeat(150);
        let extra: String = "😀".repeat(100);
        append_capped(&mut s, &extra, MAX_QUERY_CHARS);
        assert_eq!(s.chars().count(), 200);
        let expected = format!("{}{}", "é".repeat(150), "😀".repeat(50));
        assert_eq!(s, expected);
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn over_cap_existing_query_defensive() {
        // dst already has 250 chars (shouldn't happen, but must not panic).
        let mut s = "a".repeat(250);
        append_capped(&mut s, "x", MAX_QUERY_CHARS);
        assert_eq!(s, "a".repeat(250)); // unchanged, no panic
        assert_valid(&s, 250); // still valid UTF-8 (just over the intended cap)
    }

    #[test]
    fn max_chars_zero() {
        let mut s = String::new();
        append_capped(&mut s, "hello", 0);
        assert_eq!(s, "");
        assert_valid(&s, 0);

        let mut s2 = "existing".to_string();
        append_capped(&mut s2, "more", 0);
        assert_eq!(s2, "existing"); // unchanged
    }

    #[test]
    fn combined_with_clean() {
        let mut s = String::new();
        let raw = "a\u{1b}[2Jb\n\tc";
        append_capped(&mut s, &clean(raw), MAX_QUERY_CHARS);
        assert_eq!(s, "a[2Jbc");
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    #[test]
    fn combining_marks() {
        // "e\u{301}" is two chars (e + combining acute), not one grapheme cluster.
        // The cap counts chars, not grapheme clusters.
        let unit = "e\u{301}"; // 2 chars per unit
        let extra: String = unit.repeat(300); // 600 chars
        let mut s = String::new();
        append_capped(&mut s, &extra, MAX_QUERY_CHARS);
        assert_eq!(s.chars().count(), 200);
        let expected: String = extra.chars().take(200).collect();
        assert_eq!(s, expected);
        assert_valid(&s, MAX_QUERY_CHARS);
    }

    // ── Property-style loop test ────────────────────────────────────────────
    #[test]
    fn property_loop_never_panics_or_exceeds_cap() {
        let alphabets: &[&str] = &[
            "abcdefghij",            // ASCII
            "éèêëàâîïôù",           // 2-byte
            "日本語中文韓台港星嶼灣", // 3-byte CJK
            "😀😁😂🤣😃😄😅😆😇😈", // 4-byte emoji
        ];
        for alpha in alphabets {
            let pool: Vec<char> = alpha.chars().collect();
            for n in 0..=400 {
                let input: String = pool.iter().cycle().take(n).collect();
                let mut s = String::new();
                append_capped(&mut s, &input, MAX_QUERY_CHARS);
                let count = s.chars().count();
                assert!(count <= MAX_QUERY_CHARS,
                    "n={n}, alphabet starts with {:?}: got {count} chars",
                    pool.first());
                let expected_count = n.min(MAX_QUERY_CHARS);
                assert_eq!(count, expected_count,
                    "n={n}: expected {expected_count} chars, got {count}");
                let expected: String = input.chars().take(MAX_QUERY_CHARS).collect();
                assert_eq!(s, expected);
                assert!(s.is_char_boundary(s.len()));
            }
        }
    }
}
