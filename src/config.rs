//! Configuration: defaults < `config.toml` (flat TOML subset) < CLI flags.
//! The same `Config::set` validates keys from both sources.

use std::path::{Path, PathBuf};

use crate::life::Rule;

pub type Rgb = [u8; 3];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RuleSel {
    Fixed(Rule),
    Rotate,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColorMode {
    Age,
    Fixed(Rgb),
}

#[derive(Clone, Debug)]
pub struct Palette {
    pub bg: Rgb,
    pub newborn: Rgb,
    pub mature: Rgb,
    pub old: Rgb,
    pub ancient: Rgb,
    pub dying: Rgb,
    pub rain: Rgb,
    pub overlay: Rgb,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub cell_size: usize,
    /// Pixels (logical) left empty between neighbouring cells.
    pub cell_gap: usize,
    /// Round the corners of each cell.
    pub cell_round: bool,
    pub fps: f64,
    pub density: f64,
    pub reseed_density: f64,
    pub rule: RuleSel,
    pub color: ColorMode,
    pub trail: bool,
    pub trail_length: u8,
    pub rain: bool,
    pub rain_density: f64,
    pub overlay: bool,
    pub sysload: bool,
    pub patterns: bool,
    pub stamps: bool,
    /// 0 = never do a timed full reset (default).
    pub soft_reset_secs: u64,
    /// How long the board must stay calm before new life is injected.
    pub stagnation_secs: u64,
    /// Percent of the grid taken up by settled shapes (still lifes,
    /// oscillators) at which some of them are pruned. 0 = never.
    pub clutter_pct: f64,
    /// Board counts as calm while moving/chaotic cells are below this
    /// percent of the grid. 0 = only a fully repeating board is calm.
    pub calm_pct: f64,
    /// Crossfade cells between generations (renders at `render_fps`).
    pub smooth: bool,
    pub render_fps: f64,
    /// Save the board on exit (and every `save_secs`) and restore it at start.
    pub persist: bool,
    /// Stop simulating and drawing while the compositor isn't showing the
    /// wallpaper (fullscreen window on top, screen off).
    pub pause_hidden: bool,
    pub save_secs: u64,
    pub glider_min_secs: u64,
    pub glider_max_secs: u64,
    pub special_min_secs: u64,
    pub special_max_secs: u64,
    pub stamp_min_secs: u64,
    pub stamp_max_secs: u64,
    pub output: Option<String>,
    pub palette: Palette,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cell_size: 8,
            cell_gap: 1,
            cell_round: true,
            fps: 4.0,
            density: 0.25,
            reseed_density: 0.30,
            rule: RuleSel::Fixed(crate::life::CONWAY),
            color: ColorMode::Age,
            trail: true,
            trail_length: 4,
            rain: false,
            rain_density: 1.0,
            overlay: false,
            sysload: false,
            patterns: true,
            stamps: true,
            soft_reset_secs: 0,
            stagnation_secs: 60,
            clutter_pct: 8.0,
            calm_pct: 0.3,
            smooth: true,
            render_fps: 30.0,
            persist: true,
            pause_hidden: true,
            save_secs: 300,
            glider_min_secs: 60,
            glider_max_secs: 90,
            special_min_secs: 90,
            special_max_secs: 180,
            stamp_min_secs: 150,
            stamp_max_secs: 300,
            output: None,
            palette: Palette {
                bg: [0x28, 0x28, 0x28],
                newborn: [0xfa, 0xbd, 0x2f],
                mature: [0xb8, 0xbb, 0x26],
                old: [0xfe, 0x80, 0x19],
                ancient: [0xfb, 0x49, 0x34],
                dying: [0x45, 0x85, 0x88],
                rain: [0x68, 0x9d, 0x6a],
                overlay: [0xa8, 0x99, 0x84],
            },
        }
    }
}

pub const BOOL_KEYS: &[&str] = &[
    "trail",
    "rain",
    "overlay",
    "sysload",
    "patterns",
    "stamps",
    "smooth",
    "persist",
    "pause_hidden",
    "cell_round",
];

pub fn parse_color(s: &str) -> Result<Rgb, String> {
    let named = match s.to_ascii_lowercase().as_str() {
        "green" => Some([0xb8, 0xbb, 0x26]),
        "yellow" => Some([0xfa, 0xbd, 0x2f]),
        "orange" => Some([0xfe, 0x80, 0x19]),
        "red" => Some([0xfb, 0x49, 0x34]),
        "aqua" => Some([0x8e, 0xc0, 0x7c]),
        "blue" => Some([0x83, 0xa5, 0x98]),
        "purple" => Some([0xd3, 0x86, 0x9b]),
        "fg" => Some([0xeb, 0xdb, 0xb2]),
        "gray" | "grey" => Some([0x92, 0x83, 0x74]),
        _ => None,
    };
    if let Some(c) = named {
        return Ok(c);
    }
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()) {
        let v = u32::from_str_radix(h, 16).map_err(|e| e.to_string())?;
        return Ok([(v >> 16) as u8, (v >> 8) as u8, v as u8]);
    }
    Err(format!("bad color {s:?} (use a gruvbox name like green/yellow/orange/red/aqua/blue/purple or #rrggbb)"))
}

fn parse_bool(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        _ => Err(format!("expected true/false, got {s:?}")),
    }
}

fn num<T: std::str::FromStr + PartialOrd + std::fmt::Display>(
    s: &str,
    lo: T,
    hi: T,
) -> Result<T, String> {
    let v: T = s.parse().map_err(|_| format!("bad number {s:?}"))?;
    // `partial_cmp` is None for NaN, which `<` / `>` would let straight through.
    if v.partial_cmp(&lo).is_none() || v < lo || v > hi {
        return Err(format!("{s} out of range {lo}..={hi}"));
    }
    Ok(v)
}

impl Config {
    pub fn set(&mut self, key: &str, val: &str) -> Result<(), String> {
        let key = key.replace('-', "_");
        match key.as_str() {
            "cell_size" => self.cell_size = num(val, 2, 64)?,
            "cell_gap" => self.cell_gap = num(val, 0, 8)?,
            "cell_round" => self.cell_round = parse_bool(val)?,
            "pause_hidden" => self.pause_hidden = parse_bool(val)?,
            "fps" | "tick_hz" => self.fps = num(val, 0.1, 60.0)?,
            "density" => {
                let d = num(val, 0.0, 1.0)?;
                self.density = d;
                self.reseed_density = d;
            }
            "reseed_density" => self.reseed_density = num(val, 0.0, 1.0)?,
            "rule" => {
                self.rule = if val.eq_ignore_ascii_case("rotate") {
                    RuleSel::Rotate
                } else {
                    RuleSel::Fixed(Rule::parse(val).ok_or_else(|| {
                        format!("unknown rule {val:?} (life, highlife, daynight, seeds, brain, rotate, or B3/S23 notation)")
                    })?)
                }
            }
            "color" => {
                self.color = if val.eq_ignore_ascii_case("age") {
                    ColorMode::Age
                } else {
                    ColorMode::Fixed(parse_color(val)?)
                }
            }
            "trail" => self.trail = parse_bool(val)?,
            "trail_length" => self.trail_length = num(val, 1, 32)?,
            "rain" => self.rain = parse_bool(val)?,
            "rain_density" => self.rain_density = num(val, 0.1, 10.0)?,
            "overlay" => self.overlay = parse_bool(val)?,
            "sysload" => self.sysload = parse_bool(val)?,
            "patterns" => self.patterns = parse_bool(val)?,
            "stamps" => self.stamps = parse_bool(val)?,
            "soft_reset_secs" => self.soft_reset_secs = num(val, 0, 86_400)?,
            "clutter_pct" => self.clutter_pct = num(val, 0.0, 100.0)?,
            "calm_pct" => self.calm_pct = num(val, 0.0, 100.0)?,
            "smooth" => self.smooth = parse_bool(val)?,
            "render_fps" => self.render_fps = num(val, 1.0, 60.0)?,
            "persist" => self.persist = parse_bool(val)?,
            "save_secs" => self.save_secs = num(val, 0, 86_400)?,
            "stagnation_secs" => self.stagnation_secs = num(val, 1, 86_400)?,
            "glider_min_secs" => self.glider_min_secs = num(val, 1, 86_400)?,
            "glider_max_secs" => self.glider_max_secs = num(val, 1, 86_400)?,
            "special_min_secs" => self.special_min_secs = num(val, 1, 86_400)?,
            "special_max_secs" => self.special_max_secs = num(val, 1, 86_400)?,
            "stamp_min_secs" => self.stamp_min_secs = num(val, 1, 86_400)?,
            "stamp_max_secs" => self.stamp_max_secs = num(val, 1, 86_400)?,
            "output" => self.output = Some(val.to_string()),
            "bg" => self.palette.bg = parse_color(val)?,
            "newborn" => self.palette.newborn = parse_color(val)?,
            "mature" => self.palette.mature = parse_color(val)?,
            "old" => self.palette.old = parse_color(val)?,
            "ancient" => self.palette.ancient = parse_color(val)?,
            "dying" => self.palette.dying = parse_color(val)?,
            "rain_color" => self.palette.rain = parse_color(val)?,
            "overlay_color" => self.palette.overlay = parse_color(val)?,
            _ => return Err(format!("unknown option {key:?}")),
        }
        Ok(())
    }

    /// Apply a flat `key = value` TOML subset (comments, quoted strings, `[sections]` ignored).
    pub fn apply_toml(&mut self, text: &str) -> Result<(), String> {
        for (n, raw) in text.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() || line.starts_with('[') {
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| format!("line {}: expected key = value", n + 1))?;
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .unwrap_or(v);
            self.set(k.trim(), v)
                .map_err(|e| format!("line {}: {e}", n + 1))?;
        }
        if self.glider_min_secs > self.glider_max_secs {
            return Err("glider_min_secs > glider_max_secs".into());
        }
        Ok(())
    }
}

fn strip_comment(line: &str) -> &str {
    let mut in_q = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_q = !in_q,
            '#' if !in_q => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Where a config comes from; kept so SIGHUP can re-read it.
#[derive(Clone, Debug, Default)]
pub struct ConfigSource {
    pub path: Option<PathBuf>, // explicit --config path
    pub preset: Option<String>,
    pub cli: Vec<(String, String)>,
}

impl ConfigSource {
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
        Some(base.join("life-wallpaper").join("config.toml"))
    }

    pub fn load(&self) -> Result<Config, String> {
        let mut cfg = Config::default();
        if let Some(name) = &self.preset {
            let text = match name.as_str() {
                "default" => include_str!("../config_example/config.default.toml"),
                "ambient" => include_str!("../config_example/ambient.toml"),
                "matrix-rain" => include_str!("../config_example/matrix-rain.toml"),
                "neon-brain" => include_str!("../config_example/neon-brain.toml"),
                "minimal" => include_str!("../config_example/minimal.toml"),
                _ => return Err(format!("unknown preset {name:?} (choose default, ambient, matrix-rain, neon-brain, or minimal)")),
            };
            cfg.apply_toml(text)
                .map_err(|e| format!("preset {name}: {e}"))?;
        }
        let (path, explicit) = match &self.path {
            Some(p) => (Some(p.clone()), true),
            None => (Self::default_path(), false),
        };
        if let Some(p) = path {
            match std::fs::read_to_string(&p) {
                Ok(text) if self.preset.is_none() || explicit => cfg
                    .apply_toml(&text)
                    .map_err(|e| format!("{}: {e}", p.display()))?,
                Ok(_) => {}
                Err(e) if !explicit && e.kind() == std::io::ErrorKind::NotFound => {
                    // Ship the template in the binary so first-run setup also works
                    // when the executable is launched outside the source directory.
                    let template = include_str!("../config_example/config.default.toml");
                    let create_result = p
                        .parent()
                        .map(std::fs::create_dir_all)
                        .transpose()
                        .and_then(|_| std::fs::write(&p, template));
                    match create_result {
                        Ok(()) => {
                            eprintln!("life-wallpaper: created default config at {}", p.display());
                            if self.preset.is_none() {
                                cfg.apply_toml(template)
                                    .map_err(|e| format!("embedded default config: {e}"))?;
                            }
                        }
                        Err(create_err) => {
                            eprintln!(
                                "life-wallpaper: could not create config at {} ({create_err}); using built-in defaults",
                                p.display()
                            );
                        }
                    }
                }
                Err(e) if explicit || e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(format!("{}: {e}", p.display()))
                }
                Err(_) => {}
            }
        }
        for (k, v) in &self.cli {
            cfg.set(k, v)
                .map_err(|e| format!("--{}: {e}", k.replace('_', "-")))?;
        }
        if cfg.glider_min_secs > cfg.glider_max_secs {
            return Err("glider_min_secs > glider_max_secs".into());
        }
        Ok(cfg)
    }
}

pub struct Cli {
    pub source: ConfigSource,
    pub list_outputs: bool,
}

pub const USAGE: &str = "\
life-wallpaper - Conway's Game of Life on the Wayland background layer

usage: life-wallpaper [options]

  -o, --output NAME       only this output (default: every output, hotplug aware)
      --list-outputs      print output names and exit
      --config PATH       config file (default: $XDG_CONFIG_HOME/life-wallpaper/config.toml)
      --preset NAME       default | ambient | matrix-rain | neon-brain | minimal
                          a preset replaces the default config file (an explicit --config
                          still applies on top of it); CLI flags override both
      --cell-size PX      cell size in logical pixels (default 8)
      --fps HZ            generations per second (default 4)
      --density 0..1      soup density for startup and reseeds (default 0.25 / 0.30)
      --rule RULE         life | highlife | daynight | seeds | brain | rotate | B3/S23
      --color COLOR       age | green | yellow | orange | red | aqua | blue | purple | #rrggbb
      --[no-]trail        death trails
      --[no-]rain         faint hex code-rain behind the cells
      --[no-]overlay      generation/population status line
      --[no-]sysload      let CPU load drive speed, sparks and heat
      --[no-]patterns     periodic LWSS / pulsar / Gosper gun injections
      --[no-]stamps       periodic Tux / Arch / text stamps
      --[no-]smooth       crossfade cells between generations (default on)
      --render-fps HZ     frames per second while crossfading (default 30)
      --[no-]persist      save the board on exit and restore it on start (default on)
      --[no-]pause-hidden stop simulating/drawing while the wallpaper is covered (default on)
      --cell-gap PX       empty pixels between cells (0-8, default 1)
      --[no-]cell-round   round cell corners (default on)
  -h, --help

SIGHUP reloads the config file. Any option can also be set in the config as key = value
(use underscores: cell_size = 10). See the README comments in config.rs for all keys.";

pub fn parse_cli(args: impl Iterator<Item = String>) -> Result<Cli, String> {
    let mut cli = Cli {
        source: ConfigSource::default(),
        list_outputs: false,
    };
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        if a == "-h" || a == "--help" {
            println!("{USAGE}");
            std::process::exit(0);
        }
        if a == "--list-outputs" {
            cli.list_outputs = true;
            continue;
        }
        let a = if a == "-o" { "--output".to_string() } else { a };
        let Some(flag) = a.strip_prefix("--") else {
            return Err(format!("unexpected argument {a:?}"));
        };
        let (name, inline) = match flag.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (flag.to_string(), None),
        };
        let key = name.replace('-', "_");
        if let Some(b) = key.strip_prefix("no_") {
            if BOOL_KEYS.contains(&b) {
                cli.source.cli.push((b.to_string(), "false".into()));
                continue;
            }
        }
        if BOOL_KEYS.contains(&key.as_str()) && inline.is_none() {
            cli.source.cli.push((key, "true".into()));
            continue;
        }
        let val = match inline {
            Some(v) => v,
            None => args
                .next()
                .ok_or_else(|| format!("--{name} requires a value"))?,
        };
        if key == "config" {
            cli.source.path = Some(PathBuf::from(val));
        } else if key == "preset" {
            let valid = ["default", "ambient", "matrix-rain", "neon-brain", "minimal"];
            if !valid.contains(&val.as_str()) {
                return Err(format!(
                    "unknown preset {val:?} (choose {})",
                    valid.join(", ")
                ));
            }
            cli.source.preset = Some(val);
        } else {
            cli.source.cli.push((key, val));
        }
    }
    Ok(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_subset() {
        let mut c = Config::default();
        c.apply_toml("# comment\ncell_size = 10\nrule = \"highlife\" # inline\nrain = true\nbg = \"#1d2021\"\n[ignored]\n").unwrap();
        assert_eq!(c.cell_size, 10);
        assert!(c.rain);
        assert_eq!(c.palette.bg, [0x1d, 0x20, 0x21]);
        assert!(c.apply_toml("bogus = 1").is_err());
        assert!(c.apply_toml("cell_size = 1000").is_err());
        assert!(c.apply_toml("fps = nan").is_err());
        assert!(c.apply_toml("fps = inf").is_err());
    }

    #[test]
    fn cli_preset() {
        let cli = parse_cli(["--preset", "ambient"].iter().map(|s| s.to_string())).unwrap();
        assert_eq!(cli.source.preset.as_deref(), Some("ambient"));
        assert!(parse_cli(["--preset", "unknown"].iter().map(|s| s.to_string())).is_err());
    }

    #[test]
    fn cli_flags() {
        let args = [
            "--cell-size",
            "12",
            "--no-trail",
            "--rain",
            "--rule=seeds",
            "-o",
            "DP-1",
            "--color",
            "yellow",
        ];
        let cli = parse_cli(args.iter().map(|s| s.to_string())).unwrap();
        let mut cfg = Config::default();
        for (k, v) in &cli.source.cli {
            cfg.set(k, v).unwrap();
        }
        assert_eq!(cfg.cell_size, 12);
        assert!(!cfg.trail && cfg.rain);
        assert_eq!(cfg.output.as_deref(), Some("DP-1"));
        assert_eq!(cfg.color, ColorMode::Fixed([0xfa, 0xbd, 0x2f]));
    }
}
