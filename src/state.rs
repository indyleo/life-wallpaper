//! Board persistence: one small file per output under
//! `$XDG_STATE_HOME/life-wallpaper/` (default `~/.local/state/life-wallpaper/`).
//! Format: a text header line, then `cols*rows` cell bytes, then as many age
//! bytes, then (newer files) as many colony bytes. Files without the colony
//! section still load.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use crate::life::{Grid, Life, Rule};

/// Square boards keep writing version 1 (older binaries can still read them);
/// other grids write version 2, which names the grid, so a hex board is never
/// loaded as a square one.
const MAGIC: &str = "life-wallpaper-state 1";
const MAGIC2: &str = "life-wallpaper-state 2";
const MAX_DIM: usize = 16_384;

pub struct Saved {
    pub cols: usize,
    pub rows: usize,
    pub rule: String,
    pub grid: Grid,
    pub cells: Vec<u8>,
    pub age: Vec<u8>,
    /// Empty for files written before colonies existed.
    pub colony: Vec<u8>,
}

pub fn state_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local").join("state")))?;
    Some(base.join("life-wallpaper"))
}

/// File of the scene called `name` in `dir` (empty = `<state dir>/scenes`).
/// Names are plain words, so a scene can never point outside its directory.
pub fn scene_path(dir: &str, name: &str) -> Result<PathBuf, String> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "scene name {name:?} must be 1-64 letters, digits, - or _"
        ));
    }
    let base = if dir.is_empty() {
        state_dir()
            .ok_or("no state directory (HOME is not set)")?
            .join("scenes")
    } else {
        PathBuf::from(dir)
    };
    Ok(base.join(format!("{name}.scene")))
}

/// Save a scene, creating its directory.
pub fn save_scene(dir: &str, name: &str, life: &Life, rule: Rule) -> Result<PathBuf, String> {
    let path = scene_path(dir, name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    save(&path, life, rule).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Read a scene.
pub fn load_scene(dir: &str, name: &str) -> Result<Saved, String> {
    let path = scene_path(dir, name)?;
    load(&path).ok_or_else(|| {
        format!(
            "no scene {name:?} ({} is missing or unreadable)",
            path.display()
        )
    })
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
/// `rule` is the rule the board lives under; it differs from `life.rule()`
/// while a temporary rule shift is running.
pub fn save(path: &Path, life: &Life, rule: Rule) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let head = match life.grid() {
        Grid::Square => format!("{MAGIC} {} {} {}\n", life.cols(), life.rows(), rule.name()),
        g => format!(
            "{MAGIC2} {} {} {} {}\n",
            life.cols(),
            life.rows(),
            g.name(),
            rule.name()
        ),
    };
    let mut buf = head.into_bytes();
    buf.extend_from_slice(life.cells());
    buf.extend_from_slice(life.age());
    buf.extend_from_slice(life.colony());
    let tmp = path.with_extension("tmp");
    fs::File::create(&tmp)?.write_all(&buf)?;
    fs::rename(&tmp, path)
}

/// `None` for a missing, truncated or otherwise invalid file.
pub fn load(path: &Path) -> Option<Saved> {
    let data = fs::read(path).ok()?;
    let nl = data.iter().position(|&b| b == b'\n')?;
    let text = std::str::from_utf8(&data[..nl]).ok()?;
    let (cols, rows, grid, rule): (usize, usize, Grid, String) =
        if let Some(h) = text.strip_prefix(MAGIC) {
            let mut it = h.trim().splitn(3, ' ');
            (
                it.next()?.parse().ok()?,
                it.next()?.parse().ok()?,
                Grid::Square,
                it.next()?.to_string(),
            )
        } else {
            let mut it = text.strip_prefix(MAGIC2)?.trim().splitn(4, ' ');
            (
                it.next()?.parse().ok()?,
                it.next()?.parse().ok()?,
                Grid::parse(it.next()?)?,
                it.next()?.to_string(),
            )
        };
    if cols == 0 || rows == 0 || cols > MAX_DIM || rows > MAX_DIM {
        return None;
    }
    let n = cols * rows;
    let body = &data[nl + 1..];
    if body.len() != 2 * n && body.len() != 3 * n {
        return None;
    }
    let (cells, rest) = body.split_at(n);
    let (age, colony) = rest.split_at(n);
    if cells.iter().any(|&c| c > 2) {
        return None;
    }
    Some(Saved {
        cols,
        rows,
        rule,
        grid,
        cells: cells.to_vec(),
        age: age.to_vec(),
        colony: colony.to_vec(),
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
        l.set_colonies(4, 0.0);
        let mut rng = rand::thread_rng();
        l.scatter_colonies(&mut rng);
        save(&p, &l, HIGHLIFE).unwrap();
        let s = load(&p).unwrap();
        let _ = fs::remove_file(&p);
        assert_eq!((s.cols, s.rows, s.rule.as_str()), (12, 7, "highlife"));
        assert_eq!(s.cells, l.cells());
        assert_eq!(s.age, l.age());
        assert_eq!(s.colony, l.colony());
    }

    #[test]
    fn saves_the_base_rule_not_a_temporary_one() {
        let mut l = Life::new(6, 6, 0, CONWAY);
        l.set_rule(HIGHLIFE); // pretend a rule shift is running
        let p = tmp("baserule");
        save(&p, &l, CONWAY).unwrap();
        let s = load(&p).unwrap();
        let _ = fs::remove_file(&p);
        assert_eq!(s.rule, "life");
    }

    #[test]
    fn old_files_without_colonies_still_load() {
        let p = tmp("oldformat");
        let mut data = format!("{MAGIC} 3 2 life\n").into_bytes();
        data.extend_from_slice(&[1, 0, 0, 0, 1, 0]); // cells
        data.extend_from_slice(&[5, 0, 0, 0, 9, 0]); // ages
        fs::write(&p, &data).unwrap();
        let s = load(&p).unwrap();
        let _ = fs::remove_file(&p);
        assert_eq!(s.cells, vec![1, 0, 0, 0, 1, 0]);
        assert!(s.colony.is_empty());
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        let p = tmp("garbage");
        fs::write(&p, b"nope\n123").unwrap();
        assert!(load(&p).is_none());
        let l = Life::new(5, 5, 0, CONWAY);
        save(&p, &l, CONWAY).unwrap();
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

    #[test]
    fn scene_names_are_plain_words_and_scenes_round_trip() {
        assert!(scene_path("/tmp/x", "../evil").is_err());
        assert!(scene_path("/tmp/x", "").is_err());
        assert!(scene_path("/tmp/x", "a b").is_err());
        assert_eq!(
            scene_path("/tmp/x", "night-1_a").unwrap(),
            PathBuf::from("/tmp/x/night-1_a.scene")
        );
        let dir = std::env::temp_dir().join(format!("lw-scenes-{}", std::process::id()));
        let mut l = Life::new(20, 10, 0, CONWAY);
        l.place(&Pattern::from_art(&["###"]), 3, 3, 0);
        let path = save_scene(dir.to_str().unwrap(), "t", &l, CONWAY).unwrap();
        assert!(path.ends_with("t.scene"));
        let back = load_scene(dir.to_str().unwrap(), "t").unwrap();
        assert_eq!(back.cells, l.cells());
        assert!(load_scene(dir.to_str().unwrap(), "missing").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn square_boards_keep_the_old_format_and_other_grids_cannot_be_mistaken_for_square() {
        let dir = std::env::temp_dir().join(format!("lw-grid-{}", std::process::id()));
        let mut l = Life::new(12, 8, 0, CONWAY);
        let p = dir.join("sq.state");
        save(&p, &l, CONWAY).unwrap();
        assert!(
            fs::read(&p)
                .unwrap()
                .starts_with(b"life-wallpaper-state 1 12 8"),
            "unchanged for square"
        );
        assert_eq!(load(&p).unwrap().grid, Grid::Square);
        l.set_grid(Grid::Hex);
        let p2 = dir.join("hex.state");
        save(&p2, &l, CONWAY).unwrap();
        assert!(fs::read(&p2)
            .unwrap()
            .starts_with(b"life-wallpaper-state 2 12 8 hex"));
        let back = load(&p2).unwrap();
        assert_eq!((back.grid, back.cols, back.rows), (Grid::Hex, 12, 8));
        // a corrupted grid name is rejected rather than guessed
        let mut bytes = fs::read(&p2).unwrap();
        let pos = bytes.windows(3).position(|w| w == b"hex").unwrap();
        bytes[pos..pos + 3].copy_from_slice(b"xyz");
        fs::write(&p2, bytes).unwrap();
        assert!(load(&p2).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
