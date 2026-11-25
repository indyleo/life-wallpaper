//! `life-wallpaper-ctl`: a thin client for the control socket (`ipc = true`).
//!
//!     life-wallpaper-ctl stats
//!     life-wallpaper-ctl inject glider --edge left
//!     life-wallpaper-ctl inject text "hello world" --at 10 5
//!     life-wallpaper-ctl set glow 0.5
//!     life-wallpaper-ctl -o DP-1 pause
//!
//! Prints the reply and exits 0 for `ok ...`, 1 for `err ...`, 2 if it could
//! not reach the wallpaper. Standard library only.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};

const USAGE: &str = "usage: life-wallpaper-ctl [--socket PATH | -o OUTPUT] COMMAND [ARGS...]

commands (see `help`): inject NAME [--at X Y] [--edge E] | inject text WORDS | reseed [--rule R] [--density D]
                       stats | set KEY VALUE | pause | resume | quit | load scene NAME | save scene NAME
The wallpaper must run with `ipc = true`. The socket is $XDG_RUNTIME_DIR/life-wallpaper-<output|all>.sock
(or $LIFE_WALLPAPER_SOCK).";

fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn find_socket(explicit: Option<PathBuf>, output: Option<&str>) -> Result<PathBuf, String> {
    if let Some(p) = explicit.or_else(|| {
        std::env::var_os("LIFE_WALLPAPER_SOCK")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }) {
        return Ok(p);
    }
    let dir = runtime_dir().ok_or("XDG_RUNTIME_DIR is not set; pass --socket PATH")?;
    if let Some(o) = output {
        let name: String = o
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        return Ok(dir.join(format!("life-wallpaper-{name}.sock")));
    }
    let all = dir.join("life-wallpaper-all.sock");
    if all.exists() {
        return Ok(all);
    }
    // one wallpaper started with `--output NAME`? use it
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("life-wallpaper-") && n.ends_with(".sock"))
                })
                .collect()
        })
        .unwrap_or_default();
    match found.len() {
        0 => Err(format!(
            "no control socket in {} (is life-wallpaper running with ipc = true?)",
            dir.display()
        )),
        1 => Ok(found.remove(0)),
        _ => Err("several sockets found; choose one with -o OUTPUT or --socket PATH".into()),
    }
}

/// Re-quote an argument the shell already unquoted, so the server sees one word.
fn quote(a: &str) -> String {
    if !a.is_empty()
        && !a
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\\')
    {
        return a.to_string();
    }
    format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\""))
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    let (mut socket, mut output) = (None, None);
    loop {
        match args.peek().map(String::as_str) {
            Some("-h") | Some("--help") => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            Some("--socket") => {
                args.next();
                socket = args.next().map(PathBuf::from);
            }
            Some("-o") | Some("--output") => {
                args.next();
                output = args.next();
            }
            _ => break,
        }
    }
    let words: Vec<String> = args.collect();
    if words.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let path = match find_socket(socket, output.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("life-wallpaper-ctl: {e}");
            return ExitCode::from(2);
        }
    };
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "life-wallpaper-ctl: cannot connect to {}: {e}",
                path.display()
            );
            return ExitCode::from(2);
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let line = words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ");
    if let Err(e) = writeln!(stream, "{line}") {
        eprintln!("life-wallpaper-ctl: {e}");
        return ExitCode::from(2);
    }
    let mut reply = String::new();
    if BufReader::new(stream).read_line(&mut reply).is_err() || reply.is_empty() {
        eprintln!("life-wallpaper-ctl: no reply");
        return ExitCode::from(2);
    }
    let reply = reply.trim_end();
    println!("{reply}");
    if reply.starts_with("ok") {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
