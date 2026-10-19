//! Hyprland hooks. Hyprland publishes its events (workspace switches, windows
//! opening and closing, ...) as `name>>data` lines on a unix socket,
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`, and
//! answers commands (`j/clients`, `j/monitors`) on `.socket.sock` next to it.
//!
//! The event socket is only listened to. The command socket is used to find
//! out *where* a window is, so the wallpaper can react at that spot: a closed
//! window blooms where it was, a new window gets a halo around it. Window
//! geometry is cached from `j/clients` (refreshed shortly after events that
//! can move windows), because by the time `closewindow` arrives the window is
//! already gone. Without Hyprland (or with `hypr_events = false`) none of
//! this runs.

use std::{
    collections::HashMap,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HyprEvent {
    /// The active workspace changed.
    Workspace,
    OpenWindow,
    CloseWindow,
}

/// One interesting line from the event socket.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Raw {
    /// An event the wallpaper reacts to, with the window address if it has one.
    Event(HyprEvent, Option<String>),
    /// Nothing to react to, but window geometry may have changed.
    Refresh,
}

fn socket_dir() -> Option<PathBuf> {
    let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").filter(|v| !v.is_empty())?;
    let run = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(run).join("hypr").join(sig))
}

/// `$XDG_RUNTIME_DIR/hypr/<signature>/.socket2.sock`, if we are under Hyprland.
pub fn socket_path() -> Option<PathBuf> {
    Some(socket_dir()?.join(".socket2.sock"))
}

/// A non-blocking connection to the event socket.
pub fn connect() -> Option<UnixStream> {
    let stream = UnixStream::connect(socket_path()?).ok()?;
    stream.set_nonblocking(true).ok()?;
    Some(stream)
}

/// Ask Hyprland something on the command socket (`j/clients`, `j/monitors`).
/// Blocking, but Hyprland answers in well under a millisecond; the timeouts
/// only matter if it is wedged. `None` on any failure.
pub fn query(cmd: &str) -> Option<String> {
    let mut s = UnixStream::connect(socket_dir()?.join(".socket.sock")).ok()?;
    let t = Some(Duration::from_millis(300));
    s.set_read_timeout(t).ok()?;
    s.set_write_timeout(t).ok()?;
    s.write_all(cmd.as_bytes()).ok()?;
    let mut out = String::new();
    s.take(4 << 20).read_to_string(&mut out).ok()?;
    Some(out)
}

/// Window address as Hyprland prints it in events (no prefix) or in JSON
/// (`0x...`), normalised so the two compare equal.
fn norm_addr(a: &str) -> String {
    let a = a.trim();
    a.strip_prefix("0x").unwrap_or(a).to_ascii_lowercase()
}

/// One `name>>data` line; `None` for events we do not care about.
pub fn parse_line(line: &str) -> Option<Raw> {
    let (name, data) = line.split_once(">>").unwrap_or((line, ""));
    let addr = |d: &str| d.split(',').next().map(norm_addr).filter(|a| !a.is_empty());
    match name.trim() {
        // `workspacev2` follows every `workspace`; only react to the old name.
        "workspace" => Some(Raw::Event(HyprEvent::Workspace, None)),
        "openwindow" => Some(Raw::Event(HyprEvent::OpenWindow, addr(data))),
        "closewindow" => Some(Raw::Event(HyprEvent::CloseWindow, addr(data.trim()))),
        "activewindowv2" | "movewindowv2" | "fullscreen" | "changefloatingmode" | "monitoraddedv2"
        | "monitorremovedv2" | "monitoradded" | "monitorremoved" | "configreloaded" => Some(Raw::Refresh),
        _ => None,
    }
}

/// Collects bytes from the socket and yields complete lines as events.
#[derive(Default)]
pub struct LineBuf {
    buf: Vec<u8>,
}

/// A runaway line without a newline must not grow the buffer forever.
const MAX_LINE: usize = 16 * 1024;

impl LineBuf {
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Raw>) {
        self.buf.extend_from_slice(bytes);
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            if let Some(raw) = std::str::from_utf8(&line).ok().and_then(parse_line) {
                out.push(raw);
            }
        }
        if self.buf.len() > MAX_LINE {
            self.buf.clear();
        }
    }
}

// ------------------------------------------------------------------- json

/// Just enough JSON to read `j/clients` and `j/monitors` without a serde
/// dependency. Window titles are arbitrary text, so a real parser (strings
/// with escapes, nesting) is needed rather than scanning for braces.
#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn num(&self) -> Option<f64> {
        match self {
            J::Num(n) => Some(*n),
            _ => None,
        }
    }
    fn str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }
    fn bool(&self) -> Option<bool> {
        match self {
            J::Bool(b) => Some(*b),
            _ => None,
        }
    }
    fn arr(&self) -> Option<&[J]> {
        match self {
            J::Arr(a) => Some(a),
            _ => None,
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

const MAX_DEPTH: u32 = 24;

impl Parser<'_> {
    fn ws(&mut self) {
        while self.b.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        self.ws();
        (self.b.get(self.i) == Some(&c)).then(|| self.i += 1)
    }

    fn lit(&mut self, word: &str, v: J) -> Option<J> {
        self.b[self.i..].starts_with(word.as_bytes()).then(|| {
            self.i += word.len();
            v
        })
    }

    fn value(&mut self, depth: u32) -> Option<J> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.ws();
        match *self.b.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut kv = Vec::new();
                if self.eat(b'}').is_some() {
                    return Some(J::Obj(kv));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.eat(b':')?;
                    kv.push((k, self.value(depth + 1)?));
                    if self.eat(b',').is_none() {
                        self.eat(b'}')?;
                        return Some(J::Obj(kv));
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                if self.eat(b']').is_some() {
                    return Some(J::Arr(v));
                }
                loop {
                    v.push(self.value(depth + 1)?);
                    if self.eat(b',').is_none() {
                        self.eat(b']')?;
                        return Some(J::Arr(v));
                    }
                }
            }
            b'"' => self.string().map(J::Str),
            b't' => self.lit("true", J::Bool(true)),
            b'f' => self.lit("false", J::Bool(false)),
            b'n' => self.lit("null", J::Null),
            _ => {
                let start = self.i;
                while self.b.get(self.i).is_some_and(|c| matches!(c, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[start..self.i]).ok()?.parse().ok().map(J::Num)
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(h, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        if self.b.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.b[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + lo.wrapping_sub(0xDC00);
                            }
                            char::from_u32(cp).unwrap_or('\u{fffd}')
                        }
                        other => other as char, // \" \\ \/
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(c),
            }
        }
    }
}

pub fn parse_json(text: &str) -> Option<J> {
    let mut p = Parser { b: text.as_bytes(), i: 0 };
    let v = p.value(0)?;
    p.ws();
    (p.i == p.b.len()).then_some(v)
}

// ---------------------------------------------------------------- tracker

/// A window in the compositor's layout space (logical pixels).
#[derive(Clone, Debug, PartialEq)]
struct Win {
    monitor: i64,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

#[derive(Clone, Debug, PartialEq)]
struct Mon {
    name: String,
    x: i32,
    y: i32,
}

/// Where a window is, in logical pixels relative to the top-left corner of
/// the output named `monitor`.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub monitor: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// An event ready to be delivered. `place: None` means "no idea where":
/// every output reacts, at a random spot.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub ev: HyprEvent,
    pub place: Option<Place>,
}

/// Layout moves a moment after the window is mapped, so geometry is read
/// this long after `openwindow`.
const SETTLE: Duration = Duration::from_millis(150);

/// Most open events waiting for geometry at once (a login autostart burst).
const MAX_WAITING: usize = 16;

fn px(j: &J, key: &str, idx: usize) -> Option<i32> {
    Some(j.get(key)?.arr()?.get(idx)?.num()?.round() as i32)
}

fn parse_windows(text: &str) -> Option<HashMap<String, Win>> {
    let j = parse_json(text)?;
    let mut out = HashMap::new();
    for w in j.arr()? {
        if w.get("mapped").and_then(J::bool) == Some(false) || w.get("hidden").and_then(J::bool) == Some(true) {
            continue;
        }
        let (Some(addr), Some(x), Some(y), Some(ww), Some(hh)) =
            (w.get("address").and_then(J::str), px(w, "at", 0), px(w, "at", 1), px(w, "size", 0), px(w, "size", 1))
        else {
            continue;
        };
        let monitor = w.get("monitor").and_then(J::num).map_or(-1, |m| m as i64);
        out.insert(norm_addr(addr), Win { monitor, x, y, w: ww, h: hh });
    }
    Some(out)
}

fn parse_monitors(text: &str) -> Option<HashMap<i64, Mon>> {
    let j = parse_json(text)?;
    let mut out = HashMap::new();
    for m in j.arr()? {
        let (Some(id), Some(name)) = (m.get("id").and_then(J::num), m.get("name").and_then(J::str)) else { continue };
        let at = |k: &str| m.get(k).and_then(J::num).map_or(0, |v| v.round() as i32);
        out.insert(id as i64, Mon { name: name.to_string(), x: at("x"), y: at("y") });
    }
    Some(out)
}

/// Remembers where windows are and turns raw events into placed ones.
/// Pure state machine: the caller does the I/O (`refresh_due`, `refresh`).
pub struct Tracker {
    locate: bool,
    windows: HashMap<String, Win>,
    monitors: HashMap<i64, Mon>,
    refresh_due: Option<Instant>,
    /// Open events waiting for fresh geometry: (address, when to deliver).
    waiting: Vec<(Option<String>, Instant)>,
}

impl Tracker {
    /// `locate` off: events pass straight through without a place.
    /// `prime`: read the current windows right away (Hyprland is running).
    pub fn new(locate: bool, prime: bool, now: Instant) -> Self {
        Self {
            locate,
            windows: HashMap::new(),
            monitors: HashMap::new(),
            refresh_due: (locate && prime).then_some(now),
            waiting: Vec::new(),
        }
    }

    /// Windows we know about (for tests and diagnostics).
    #[cfg(test)]
    pub fn known(&self) -> usize {
        self.windows.len()
    }

    fn schedule(&mut self, at: Instant) {
        if self.locate && self.refresh_due.is_none() {
            self.refresh_due = Some(at);
        }
    }

    fn place_of(&self, addr: Option<&str>) -> Option<Place> {
        let w = self.windows.get(addr?)?;
        let m = self.monitors.get(&w.monitor)?;
        Some(Place { monitor: m.name.clone(), x: w.x - m.x, y: w.y - m.y, w: w.w, h: w.h })
    }

    /// Feed one line of the event socket. Events that can be answered now go
    /// to `out`; opens wait for geometry (see `ready`).
    pub fn on_raw(&mut self, raw: Raw, now: Instant, out: &mut Vec<Placed>) {
        if !self.locate {
            if let Raw::Event(ev, _) = raw {
                out.push(Placed { ev, place: None });
            }
            return;
        }
        match raw {
            Raw::Refresh => self.schedule(now + SETTLE),
            Raw::Event(HyprEvent::Workspace, _) => {
                self.schedule(now + SETTLE);
                out.push(Placed { ev: HyprEvent::Workspace, place: None });
            }
            Raw::Event(HyprEvent::OpenWindow, addr) => {
                self.schedule(now + SETTLE);
                if self.waiting.len() < MAX_WAITING {
                    self.waiting.push((addr, now + SETTLE));
                }
            }
            Raw::Event(HyprEvent::CloseWindow, addr) => {
                // Gone already: answer from the cache, then forget it.
                let place = self.place_of(addr.as_deref());
                if let Some(a) = &addr {
                    self.windows.remove(a);
                }
                self.schedule(now + SETTLE);
                out.push(Placed { ev: HyprEvent::CloseWindow, place });
            }
        }
    }

    /// Time to run `j/clients` and `j/monitors`?
    pub fn refresh_due(&self, now: Instant) -> bool {
        self.refresh_due.is_some_and(|t| now >= t)
    }

    /// Adopt fresh query results. A failed or garbled answer keeps the old
    /// cache (and does not retry in a loop).
    pub fn refresh(&mut self, clients: Option<&str>, monitors: Option<&str>) {
        self.refresh_due = None;
        if let Some(m) = monitors.and_then(parse_monitors) {
            self.monitors = m;
        }
        if let Some(w) = clients.and_then(parse_windows) {
            self.windows = w;
        }
    }

    /// Open events whose geometry has had time to settle.
    pub fn ready(&mut self, now: Instant, out: &mut Vec<Placed>) {
        if self.refresh_due.is_some_and(|t| t <= now) {
            return; // the refresh that settles them has not run yet
        }
        let mut i = 0;
        while i < self.waiting.len() {
            if self.waiting[i].1 <= now {
                let (addr, _) = self.waiting.remove(i);
                let place = self.place_of(addr.as_deref());
                out.push(Placed { ev: HyprEvent::OpenWindow, place });
            } else {
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: &str = r#"[
        {"id": 0, "name": "eDP-1", "width": 1920, "height": 1080, "x": 0, "y": 0, "scale": 1.00},
        {"id": 1, "name": "DP-2", "width": 2560, "height": 1440, "x": 1920, "y": -200, "scale": 1.25}
    ]"#;

    const CLIENTS: &str = r#"[
        {"address": "0x80a6f50", "mapped": true, "hidden": false, "at": [10, 20], "size": [800, 600],
         "workspace": {"id": 1, "name": "1"}, "monitor": 0, "class": "kitty",
         "title": "he said \"{[}\" \u00e9 \ud83d\ude00 and left"},
        {"address": "0x80b1a00", "mapped": true, "hidden": false, "at": [2000, 100], "size": [500, 400],
         "workspace": {"id": 2, "name": "2"}, "monitor": 1, "class": "firefox", "title": "tab"},
        {"address": "0x1111111", "mapped": false, "hidden": false, "at": [0, 0], "size": [1, 1], "monitor": 0},
        {"address": "0x2222222", "mapped": true, "hidden": true, "at": [0, 0], "size": [5, 5], "monitor": 0}
    ]"#;

    #[test]
    fn parses_the_events_we_use() {
        assert_eq!(parse_line("workspace>>3"), Some(Raw::Event(HyprEvent::Workspace, None)));
        assert_eq!(
            parse_line("openwindow>>80A6F50,2,kitty,~"),
            Some(Raw::Event(HyprEvent::OpenWindow, Some("80a6f50".into())))
        );
        assert_eq!(
            parse_line("closewindow>>80a6f50\n"),
            Some(Raw::Event(HyprEvent::CloseWindow, Some("80a6f50".into())))
        );
        assert_eq!(parse_line("activewindowv2>>80a6f50"), Some(Raw::Refresh));
        assert_eq!(parse_line("workspacev2>>3,3"), None);
        assert_eq!(parse_line("activewindow>>kitty,~"), None);
        assert_eq!(parse_line(""), None);
    }

    #[test]
    fn line_buffer_handles_split_reads() {
        let mut lb = LineBuf::default();
        let mut out = Vec::new();
        lb.feed(b"workspace>>2\nopenwin", &mut out);
        assert_eq!(out, vec![Raw::Event(HyprEvent::Workspace, None)]);
        lb.feed(b"dow>>a,1,x,y\nfocusedmon>>m,1\nclosewindow>>a\n", &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2], Raw::Event(HyprEvent::CloseWindow, Some("a".into())));
        lb.feed(&vec![b'x'; MAX_LINE + 10], &mut out);
        assert_eq!(out.len(), 3);
        lb.feed(b"workspace>>1\n", &mut out);
        assert_eq!(out.len(), 4, "recovers after an overlong line");
    }

    #[test]
    fn json_reader_copes_with_hostile_titles_and_garbage() {
        let j = parse_json(CLIENTS).unwrap();
        let title = j.arr().unwrap()[0].get("title").and_then(J::str).unwrap();
        assert_eq!(title, "he said \"{[}\" é 😀 and left");
        assert_eq!(parse_json("[1, 2.5, -3e2, true, null, \"a\\nb\", {\"k\": []}]").unwrap().arr().unwrap().len(), 7);
        for bad in ["", "[", "{\"a\":}", "[1,]x", "{\"a\" 1}", "\"unterminated", "[1] 2"] {
            assert!(parse_json(bad).is_none(), "{bad:?}");
        }
        let deep = "[".repeat(200) + &"]".repeat(200);
        assert!(parse_json(&deep).is_none(), "nesting is bounded");
    }

    #[test]
    fn windows_and_monitors_are_read_from_the_query_output() {
        let w = parse_windows(CLIENTS).unwrap();
        assert_eq!(w.len(), 2, "unmapped and hidden windows are skipped");
        assert_eq!(w["80a6f50"], Win { monitor: 0, x: 10, y: 20, w: 800, h: 600 });
        let m = parse_monitors(MONITORS).unwrap();
        assert_eq!(m[&1], Mon { name: "DP-2".into(), x: 1920, y: -200 });
    }

    fn primed(now: Instant) -> Tracker {
        let mut t = Tracker::new(true, true, now);
        assert!(t.refresh_due(now), "primed at start");
        t.refresh(Some(CLIENTS), Some(MONITORS));
        assert!(!t.refresh_due(now) && t.known() == 2);
        t
    }

    #[test]
    fn close_is_placed_from_the_cache_in_monitor_local_pixels() {
        let t0 = Instant::now();
        let mut t = primed(t0);
        let mut out = Vec::new();
        t.on_raw(Raw::Event(HyprEvent::CloseWindow, Some("80b1a00".into())), t0, &mut out);
        assert_eq!(
            out,
            vec![Placed {
                ev: HyprEvent::CloseWindow,
                // layout (2000, 100) on a monitor at (1920, -200)
                place: Some(Place { monitor: "DP-2".into(), x: 80, y: 300, w: 500, h: 400 }),
            }]
        );
        assert_eq!(t.known(), 1, "a closed window is forgotten");
        out.clear();
        t.on_raw(Raw::Event(HyprEvent::CloseWindow, Some("nosuch".into())), t0, &mut out);
        assert_eq!(out[0].place, None, "unknown window: no place, random spot");
    }

    #[test]
    fn open_waits_for_the_layout_to_settle_then_uses_fresh_geometry() {
        let t0 = Instant::now();
        let mut t = primed(t0);
        let mut out = Vec::new();
        t.on_raw(Raw::Event(HyprEvent::OpenWindow, Some("90ffff0".into())), t0, &mut out);
        assert!(out.is_empty(), "nothing yet");
        t.ready(t0 + Duration::from_millis(50), &mut out);
        assert!(out.is_empty());
        let later = t0 + SETTLE + Duration::from_millis(5);
        assert!(t.refresh_due(later));
        t.ready(later, &mut out);
        assert!(out.is_empty(), "must not deliver before the refresh has run");
        let fresh = CLIENTS.replace("0x80a6f50", "0x90ffff0");
        t.refresh(Some(&fresh), Some(MONITORS));
        t.ready(later, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].place.as_ref().map(|p| (p.monitor.as_str(), p.x, p.y)), Some(("eDP-1", 10, 20)));
    }

    #[test]
    fn failed_queries_keep_the_cache_and_do_not_retry_in_a_loop() {
        let t0 = Instant::now();
        let mut t = primed(t0);
        t.on_raw(Raw::Refresh, t0, &mut Vec::new());
        assert!(t.refresh_due(t0 + SETTLE));
        t.refresh(None, Some("not json"));
        assert!(!t.refresh_due(t0 + SETTLE * 10));
        assert_eq!(t.known(), 2);
    }

    #[test]
    fn locate_off_passes_events_through_unplaced() {
        let t0 = Instant::now();
        let mut t = Tracker::new(false, true, t0);
        assert!(!t.refresh_due(t0 + Duration::from_secs(9)), "never queries");
        let mut out = Vec::new();
        t.on_raw(Raw::Event(HyprEvent::OpenWindow, Some("a".into())), t0, &mut out);
        t.on_raw(Raw::Refresh, t0, &mut out);
        t.on_raw(Raw::Event(HyprEvent::CloseWindow, Some("a".into())), t0, &mut out);
        assert_eq!(out.iter().map(|p| (p.ev, p.place.is_none())).collect::<Vec<_>>(), vec![(HyprEvent::OpenWindow, true), (HyprEvent::CloseWindow, true)]);
    }

    #[test]
    fn a_burst_of_opens_is_capped() {
        let t0 = Instant::now();
        let mut t = primed(t0);
        let mut out = Vec::new();
        for i in 0..100 {
            t.on_raw(Raw::Event(HyprEvent::OpenWindow, Some(format!("{i:x}"))), t0, &mut out);
        }
        t.refresh(Some("[]"), Some(MONITORS));
        t.ready(t0 + SETTLE, &mut out);
        assert_eq!(out.len(), MAX_WAITING);
    }
}
