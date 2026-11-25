//! Media player awareness (`mpris = true`): `playerctl --follow` prints a line
//! whenever the active player's status or track changes; this module parses
//! those lines. The process is supervised like the audio helper. With no
//! player running nothing arrives and nothing happens.
//!
//! Wire format of one line: `Status|artist|title` (title last, it may contain `|`).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Playing,
    Paused,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub status: Status,
    pub artist: String,
    pub title: String,
}

/// The command line (split on whitespace by `audio::spawn`, so the format
/// string must stay free of spaces).
pub fn command(player: &str) -> String {
    let who: String = player
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c))
        .collect();
    if who.is_empty() {
        "playerctl --follow metadata --format {{status}}|{{artist}}|{{title}}".to_string()
    } else {
        format!("playerctl --player={who} --follow metadata --format {{{{status}}}}|{{{{artist}}}}|{{{{title}}}}")
    }
}

pub fn parse_line(line: &str) -> Option<Track> {
    let mut it = line.trim().splitn(3, '|');
    let status = match it.next()?.trim().to_ascii_lowercase().as_str() {
        "playing" => Status::Playing,
        "paused" => Status::Paused,
        "stopped" => Status::Stopped,
        _ => return None,
    };
    Some(Track {
        status,
        artist: it.next()?.trim().to_string(),
        title: it.next()?.trim().to_string(),
    })
}

/// A stable bucket 0..n for an artist (FNV-1a), so the same artist always
/// gets the same colony colour.
pub fn artist_bucket(artist: &str, n: usize) -> usize {
    let mut h: u32 = 0x811c_9dc5;
    for b in artist.trim().to_lowercase().bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    (h as usize) % n.max(1)
}

/// Splits the helper's output into lines.
#[derive(Default)]
pub struct Lines {
    buf: Vec<u8>,
}

impl Lines {
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Track>) {
        self.buf.extend_from_slice(bytes);
        while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            if let Some(t) = parse_line(&String::from_utf8_lossy(&line)) {
                out.push(t);
            }
        }
        if self.buf.len() > 8192 {
            self.buf.clear(); // no newline in sight: garbage, start over
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_parse_and_titles_may_contain_the_separator() {
        let t = parse_line("Playing|Boards of Canada|Roygbiv | live\n").unwrap();
        assert_eq!(
            t,
            Track {
                status: Status::Playing,
                artist: "Boards of Canada".into(),
                title: "Roygbiv | live".into()
            }
        );
        assert_eq!(parse_line("Paused||").unwrap().status, Status::Paused);
        assert!(parse_line("garbage").is_none());
        assert!(parse_line("Dancing|a|b").is_none());
        assert!(parse_line("").is_none());
    }

    #[test]
    fn partial_reads_are_joined_and_noise_is_dropped() {
        let mut l = Lines::default();
        let mut out = Vec::new();
        l.feed(b"Playing|A|on", &mut out);
        assert!(out.is_empty());
        l.feed(b"e\nnot a track\nStopped||\n", &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "one");
        assert_eq!(out[1].status, Status::Stopped);
        l.feed(&vec![b'x'; 10_000], &mut out);
        l.feed(b"\nPlaying|B|t\n", &mut out);
        assert_eq!(
            out.last().unwrap().artist,
            "B",
            "recovers after an oversized line"
        );
    }

    #[test]
    fn the_same_artist_always_gets_the_same_bucket() {
        assert_eq!(
            artist_bucket("Aphex Twin", 6),
            artist_bucket("  aphex twin ", 6)
        );
        assert!(artist_bucket("anyone", 6) < 6);
        assert_eq!(artist_bucket("x", 0), 0);
    }

    #[test]
    fn the_command_is_safe_to_split_on_spaces() {
        let c = command("");
        assert!(c.starts_with("playerctl --follow metadata --format "));
        assert!(
            !command("spotify; rm -rf").contains(';'),
            "only name characters survive"
        );
        assert!(command("vlc").contains("--player=vlc"));
        assert!(command("vlc").ends_with("{{status}}|{{artist}}|{{title}}"));
    }
}
