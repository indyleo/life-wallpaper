//! Board persistence: one small file per output under
//! `$XDG_STATE_HOME/life-wallpaper/` (default `~/.local/state/life-wallpaper/`).
//! Format: a text header line, then `cols*rows` cell bytes, then as many age bytes.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use crate::life::Life;

const MAGIC: &str = "life-wallpaper-state 1";
const MAX_DIM: usize = 16_384;

pub struct Saved {
    pub cols: usize,
    pub rows: usize,
    pub rule: String,
    pub cells: Vec<u8>,
    pub age: Vec<u8>,
}

pub fn state_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local").join("state")))?;
    Some(base.join("life-wallpaper"))
}

/// State file for an output name (`None` = unknown name).
pub fn path_for(output: Option<&str>) -> Option<PathBuf> {
    let name: String = output
        .unwrap_or("default")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(state_dir()?.join(format!("{name}.board")))
}

/// Write atomically (temp file + rename) so a crash never leaves half a board.
pub fn save(path: &Path, life: &Life) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut buf = format!(
        "{MAGIC} {} {} {}\n",
        life.cols(),
        life.rows(),
        life.rule().name()
    )
    .into_bytes();
    buf.extend_from_slice(life.cells());
    buf.extend_from_slice(life.age());
    let tmp = path.with_extension("tmp");
    fs::File::create(&tmp)?.write_all(&buf)?;
    fs::rename(&tmp, path)
}

/// `None` for a missing, truncated or otherwise invalid file.
pub fn load(path: &Path) -> Option<Saved> {
    let data = fs::read(path).ok()?;
    let nl = data.iter().position(|&b| b == b'\n')?;
    let head = std::str::from_utf8(&data[..nl])
        .ok()?
        .strip_prefix(MAGIC)?
        .trim();
    let mut it = head.splitn(3, ' ');
    let cols: usize = it.next()?.parse().ok()?;
    let rows: usize = it.next()?.parse().ok()?;
    let rule = it.next()?.to_string();
    if cols == 0 || rows == 0 || cols > MAX_DIM || rows > MAX_DIM {
        return None;
    }
    let n = cols * rows;
    let body = &data[nl + 1..];
    if body.len() != 2 * n {
        return None;
    }
    let (cells, age) = body.split_at(n);
    if cells.iter().any(|&c| c > 2) {
        return None;
    }
    Some(Saved {
        cols,
        rows,
        rule,
        cells: cells.to_vec(),
        age: age.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::life::{CONWAY, HIGHLIFE};
    use crate::patterns::Pattern;

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lw-state-test-{}-{tag}.board", std::process::id()))
    }

    #[test]
    fn roundtrip() {
        let mut l = Life::new(12, 7, 0, HIGHLIFE);
        l.place(&Pattern::from_art(&["##", "##"]), 3, 3, 0);
        l.step();
        l.step();
        let p = tmp("roundtrip");
        save(&p, &l).unwrap();
        let s = load(&p).unwrap();
        let _ = fs::remove_file(&p);
        assert_eq!((s.cols, s.rows, s.rule.as_str()), (12, 7, "highlife"));
        assert_eq!(s.cells, l.cells());
        assert_eq!(s.age, l.age());
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        let p = tmp("garbage");
        fs::write(&p, b"nope\n123").unwrap();
        assert!(load(&p).is_none());
        let l = Life::new(5, 5, 0, CONWAY);
        save(&p, &l).unwrap();
        let mut data = fs::read(&p).unwrap();
        data.pop();
        fs::write(&p, &data).unwrap();
        assert!(load(&p).is_none());
        let _ = fs::remove_file(&p);
        assert!(load(&tmp("missing")).is_none());
    }

    #[test]
    fn output_names_are_sanitised() {
        let p = path_for(Some("DP-1/../x y")).unwrap();
        assert!(p.file_name().unwrap().to_str().unwrap() == "DP-1____x_y.board");
    }
}
