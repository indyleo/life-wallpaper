//! The control socket (`ipc = true`): scripts talk to the running wallpaper
//! over a Unix socket. One command per line in, one reply line out
//! (`ok ...` or `err ...`), then the connection is closed. No dependencies,
//! no threads: the event loop accepts connections when they arrive, and
//! nothing exists at all while `ipc = false`.
//!
//! ```text
//! inject NAME [--at X Y] [--edge top|bottom|left|right]   e.g. inject glider --edge left
//! inject text "hello world" [--at X Y]
//! reseed [--rule RULE] [--density D]
//! stats                      the same line `--stats` prints
//! set KEY VALUE              any config key but audio/ipc ones; lasts until the next reload
//! pause | resume | quit
//! load scene NAME | save scene NAME
//! ```
//! The socket is `$XDG_RUNTIME_DIR/life-wallpaper-<output|all>.sock`, mode 0600.

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};

pub const HELP: &str = "ok commands: inject NAME [--at X Y] [--edge E] | inject text \"WORDS\" [--at X Y] | reseed [--rule R] [--density D] | stats | set KEY VALUE | pause | resume | quit | load scene NAME | save scene NAME";

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// `name` is a pattern name, or `text=WORDS` for text.
    Inject {
        name: String,
        at: Option<(usize, usize)>,
        edge: Option<String>,
    },
    Reseed {
        rule: Option<String>,
        density: Option<String>,
    },
    Stats,
    Set(String, String),
    Pause,
    Resume,
    Quit,
    LoadScene(String),
    SaveScene(String),
    Help,
}

/// Where the socket lives: `ipc_path` if set, else in the runtime directory,
/// named after the output (`all` when the wallpaper drives every output).
pub fn socket_path(ipc_path: &str, output: Option<&str>) -> Option<PathBuf> {
    if !ipc_path.is_empty() {
        return Some(PathBuf::from(ipc_path));
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
    let name: String = output
        .unwrap_or("all")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(Path::new(&dir).join(format!("life-wallpaper-{name}.sock")))
}

/// Create the listening socket (private, non-blocking). A stale socket file
/// from a crashed run is replaced; a live one is an error.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("{} is in use by another life-wallpaper", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    l.set_nonblocking(true)?;
    Ok(l)
}

/// Split a line into words; double quotes group, backslash escapes inside them.
pub fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            '\\' if quoted => {
                cur.push(chars.next().ok_or("dangling backslash")?);
            }
            c if c.is_whitespace() && !quoted => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if quoted {
        return Err("unterminated quote".into());
    }
    if in_word {
        out.push(cur);
    }
    Ok(out)
}

pub fn parse(line: &str) -> Result<Command, String> {
    let t = tokenize(line)?;
    let Some(verb) = t.first() else {
        return Err("empty command".into());
    };
    let rest = &t[1..];
    match verb.to_ascii_lowercase().as_str() {
        "stats" => Ok(Command::Stats),
        "pause" => Ok(Command::Pause),
        "resume" => Ok(Command::Resume),
        "quit" | "exit" => Ok(Command::Quit),
        "help" | "?" => Ok(Command::Help),
        "inject" => {
            let Some(name) = rest.first() else {
                return Err("inject needs a pattern name".into());
            };
            let mut i = 1;
            let mut name = name.clone();
            if name.eq_ignore_ascii_case("text") {
                let mut words = Vec::new();
                while i < rest.len() && !rest[i].starts_with("--") {
                    words.push(rest[i].as_str());
                    i += 1;
                }
                if words.is_empty() {
                    return Err("inject text needs the text".into());
                }
                name = format!("text={}", words.join(" "));
            }
            let (mut at, mut edge) = (None, None);
            while i < rest.len() {
                match rest[i].as_str() {
                    "--at" => {
                        let coord = |k: usize| rest.get(k).and_then(|v| v.parse::<usize>().ok());
                        match (coord(i + 1), coord(i + 2)) {
                            (Some(x), Some(y)) => at = Some((x, y)),
                            _ => return Err("--at needs two cell coordinates".into()),
                        }
                        i += 3;
                    }
                    "--edge" => {
                        edge = Some(
                            rest.get(i + 1)
                                .ok_or("--edge needs top, bottom, left or right")?
                                .clone(),
                        );
                        i += 2;
                    }
                    other => return Err(format!("unexpected {other:?} after inject")),
                }
            }
            Ok(Command::Inject { name, at, edge })
        }
        "reseed" => {
            let (mut rule, mut density) = (None, None);
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--rule" => rule = Some(rest.get(i + 1).ok_or("--rule needs a value")?.clone()),
                    "--density" => {
                        density = Some(rest.get(i + 1).ok_or("--density needs a value")?.clone())
                    }
                    other => return Err(format!("unexpected {other:?} after reseed")),
                }
                i += 2;
            }
            Ok(Command::Reseed { rule, density })
        }
        "set" => {
            if rest.len() < 2 {
                return Err("set needs a key and a value".into());
            }
            Ok(Command::Set(rest[0].clone(), rest[1..].join(" ")))
        }
        v @ ("load" | "save") => {
            match (rest.first().map(|s| s.to_ascii_lowercase()), rest.get(1)) {
                (Some(k), Some(name)) if k == "scene" => Ok(if v == "load" {
                    Command::LoadScene(name.clone())
                } else {
                    Command::SaveScene(name.clone())
                }),
                _ => Err(format!("usage: {v} scene NAME")),
            }
        }
        other => Err(format!("unknown command {other:?} (try help)")),
    }
}

/// Answer every connection that is waiting. `handler` gets the raw line and
/// the parsed command and returns the reply (without newline). A client that
/// connects and says nothing is dropped after 200 ms.
pub fn serve(listener: &UnixListener, handler: &mut dyn FnMut(&str, Command) -> String) {
    loop {
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
        let mut line = String::new();
        let got = BufReader::new((&stream).take(4096)).read_line(&mut line);
        let line = line.trim();
        let reply = match got {
            Ok(0) | Err(_) if line.is_empty() => continue,
            _ => match parse(line) {
                Ok(cmd) => handler(line, cmd),
                Err(e) => format!("err {e}"),
            },
        };
        let _ = writeln!(stream, "{reply}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_group_words_and_escapes_work() {
        assert_eq!(
            tokenize("inject text \"hello  world\" --at 3 4").unwrap(),
            ["inject", "text", "hello  world", "--at", "3", "4"]
        );
        assert_eq!(
            tokenize("a \"b \\\"c\\\"\" d").unwrap(),
            ["a", "b \"c\"", "d"]
        );
        assert_eq!(tokenize("   ").unwrap(), Vec::<String>::new());
        assert_eq!(tokenize("x \"\"").unwrap(), ["x", ""]);
        assert!(tokenize("oops \"open").is_err());
    }

    #[test]
    fn every_command_parses() {
        assert_eq!(parse("stats"), Ok(Command::Stats));
        assert_eq!(parse("PAUSE"), Ok(Command::Pause));
        assert_eq!(parse("resume"), Ok(Command::Resume));
        assert_eq!(parse("quit"), Ok(Command::Quit));
        assert_eq!(
            parse("inject glider"),
            Ok(Command::Inject {
                name: "glider".into(),
                at: None,
                edge: None
            })
        );
        assert_eq!(
            parse("inject lwss --at 10 20 --edge left"),
            Ok(Command::Inject {
                name: "lwss".into(),
                at: Some((10, 20)),
                edge: Some("left".into())
            })
        );
        assert_eq!(
            parse("inject text \"hi there\" --at 1 2"),
            Ok(Command::Inject {
                name: "text=hi there".into(),
                at: Some((1, 2)),
                edge: None
            })
        );
        assert_eq!(
            parse("inject text hello world"),
            Ok(Command::Inject {
                name: "text=hello world".into(),
                at: None,
                edge: None
            })
        );
        assert_eq!(
            parse("reseed --rule highlife --density 0.3"),
            Ok(Command::Reseed {
                rule: Some("highlife".into()),
                density: Some("0.3".into())
            })
        );
        assert_eq!(
            parse("reseed"),
            Ok(Command::Reseed {
                rule: None,
                density: None
            })
        );
        assert_eq!(
            parse("set glow 0.4"),
            Ok(Command::Set("glow".into(), "0.4".into()))
        );
        assert_eq!(
            parse("set audio_command pw-cat --raw"),
            Ok(Command::Set("audio_command".into(), "pw-cat --raw".into()))
        );
        assert_eq!(
            parse("load scene night"),
            Ok(Command::LoadScene("night".into()))
        );
        assert_eq!(
            parse("save scene night"),
            Ok(Command::SaveScene("night".into()))
        );
    }

    #[test]
    fn bad_commands_say_what_is_wrong() {
        for bad in [
            "",
            "dance",
            "inject",
            "inject text",
            "inject glider --at 1",
            "inject glider --at a b",
            "inject glider --bogus",
            "set glow",
            "load night",
            "load scene",
            "reseed --rule",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn socket_path_prefers_the_config_and_cleans_output_names() {
        assert_eq!(
            socket_path("/tmp/x.sock", Some("DP-1")),
            Some(PathBuf::from("/tmp/x.sock"))
        );
        if let Some(p) = socket_path("", Some("DP-1/odd name")) {
            assert!(
                p.to_string_lossy()
                    .ends_with("life-wallpaper-DP-1_odd_name.sock"),
                "{p:?}"
            );
        }
    }

    #[test]
    fn a_client_gets_one_reply_and_a_stale_socket_is_replaced() {
        let dir = std::env::temp_dir().join(format!("lw-ipc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.sock");
        let l = bind(&path).unwrap();
        assert!(bind(&path).is_err(), "a live socket is not stolen");
        let p2 = path.clone();
        let client = std::thread::spawn(move || {
            let ask = |msg: &str| {
                let mut s = UnixStream::connect(&p2).unwrap();
                writeln!(s, "{msg}").unwrap();
                let mut r = String::new();
                BufReader::new(s).read_line(&mut r).unwrap();
                r
            };
            (
                ask("stats"),
                ask("inject glider --edge left"),
                ask("nonsense"),
            )
        });
        let mut seen = Vec::new();
        for _ in 0..200 {
            serve(&l, &mut |line, cmd| {
                seen.push(line.to_string());
                match cmd {
                    Command::Stats => "ok gen=1".to_string(),
                    other => format!("ok {other:?}"),
                }
            });
            if seen.len() >= 2 && client.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let (a, b, c) = client.join().unwrap();
        assert_eq!(a.trim(), "ok gen=1");
        assert!(b.starts_with("ok Inject"), "{b}");
        assert!(c.starts_with("err unknown command"), "{c}");
        assert_eq!(
            seen,
            ["stats", "inject glider --edge left"],
            "the bad line never reaches the handler"
        );
        drop(l);
        let l2 = bind(&path).expect("stale socket file is replaced");
        drop(l2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
