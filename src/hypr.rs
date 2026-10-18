//! Hyprland event hooks. Hyprland publishes its events (workspace switches,
//! windows opening and closing, ...) as `name>>data` lines on a unix socket,
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`. We only
//! listen; the wallpaper reacts to a few of them. Without Hyprland (or with
//! `hypr_events = false`) none of this runs.

use std::{os::unix::net::UnixStream, path::PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HyprEvent {
    /// The active workspace changed.
    Workspace,
    OpenWindow,
    CloseWindow,
}

/// `$XDG_RUNTIME_DIR/hypr/<signature>/.socket2.sock`, if we are under Hyprland.
pub fn socket_path() -> Option<PathBuf> {
    let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").filter(|v| !v.is_empty())?;
    let run = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(run).join("hypr").join(sig).join(".socket2.sock"))
}

/// A non-blocking connection to the event socket.
pub fn connect() -> Option<UnixStream> {
    let stream = UnixStream::connect(socket_path()?).ok()?;
    stream.set_nonblocking(true).ok()?;
    Some(stream)
}

/// One `name>>data` line; `None` for events we do not care about.
pub fn parse_event(line: &str) -> Option<HyprEvent> {
    let name = line.split_once(">>").map(|(n, _)| n).unwrap_or(line).trim();
    match name {
        // `workspacev2` follows every `workspace`; only react to the old name.
        "workspace" => Some(HyprEvent::Workspace),
        "openwindow" => Some(HyprEvent::OpenWindow),
        "closewindow" => Some(HyprEvent::CloseWindow),
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
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<HyprEvent>) {
        self.buf.extend_from_slice(bytes);
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            if let Some(ev) = std::str::from_utf8(&line).ok().and_then(parse_event) {
                out.push(ev);
            }
        }
        if self.buf.len() > MAX_LINE {
            self.buf.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_events_we_use() {
        assert_eq!(parse_event("workspace>>3"), Some(HyprEvent::Workspace));
        assert_eq!(parse_event("openwindow>>80a6f50,2,kitty,~"), Some(HyprEvent::OpenWindow));
        assert_eq!(parse_event("closewindow>>80a6f50\n"), Some(HyprEvent::CloseWindow));
        assert_eq!(parse_event("workspacev2>>3,3"), None);
        assert_eq!(parse_event("activewindow>>kitty,~"), None);
        assert_eq!(parse_event(""), None);
    }

    #[test]
    fn line_buffer_handles_split_reads() {
        let mut lb = LineBuf::default();
        let mut out = Vec::new();
        lb.feed(b"workspace>>2\nopenwin", &mut out);
        assert_eq!(out, vec![HyprEvent::Workspace]);
        lb.feed(b"dow>>a,1,x,y\nfocusedmon>>m,1\nclosewindow>>a\n", &mut out);
        assert_eq!(out, vec![HyprEvent::Workspace, HyprEvent::OpenWindow, HyprEvent::CloseWindow]);
        lb.feed(&vec![b'x'; MAX_LINE + 10], &mut out);
        assert_eq!(out.len(), 3);
        lb.feed(b"workspace>>1\n", &mut out);
        assert_eq!(out.len(), 4, "recovers after an overlong line");
    }
}
