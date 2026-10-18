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
    /// Colours of the colonies (2..=8) used when `colonies` is on.
    pub colonies: Vec<Rgb>,
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
    /// oscillators) at which a full reset happens. 0 = never.
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
    // ---- liveliness: keep things happening without wiping the board ----
    /// A tile of the board with nothing moving in it for this long gets new
    /// life (soup, a glider, a symmetric patch). 0 = off.
    pub local_calm_secs: u64,
    /// Tile edge, in cells, for local calm detection.
    pub tile_size: usize,
    /// Generations a cell must have been stuck in a still life / oscillator
    /// before it may decay. 0 = off.
    pub decay_after: u32,
    /// Per-generation chance that such a cell dies.
    pub decay_chance: f64,
    /// Random single-cell disturbances per second ("cosmic rays").
    pub noise_per_sec: f64,
    /// Every this many seconds the rule is swapped for a short while. 0 = off.
    pub rule_shift_secs: u64,
    /// How many generations each swap lasts (generations, so it does not
    /// depend on `fps`).
    pub rule_shift_gens: u32,
    /// Run several generations per tick while the board is calm.
    pub fastforward: bool,
    pub fastforward_speed: u32,
    /// Births inherit the majority colour of their parents (needs `color = age`).
    pub colonies: bool,
    pub colony_mutation: f64,
    /// Convoys of LWSS / MWSS / HWSS crossing the board.
    pub traffic: bool,
    pub traffic_min_secs: u64,
    pub traffic_max_secs: u64,
    /// How long a Gosper gun lives before it is removed. 0 = forever.
    pub gun_secs: u64,
    /// Chance that an injected soup is mirror / rotation symmetric.
    pub symmetry_chance: f64,
    /// React to Hyprland events (workspace switch, window open / close).
    pub hypr_events: bool,
    // ---- look ----
    /// Generation rate swings by +-this fraction over `breathe_secs`. 0 = off.
    pub breathe: f64,
    pub breathe_secs: u64,
    /// Soft halo around live cells, 0 = off.
    pub glow: f64,
    /// Dim the cells towards the edges of the screen, 0 = off.
    pub vignette: f64,
    /// Palette hue wanders +-this many degrees over `hue_drift_secs`. 0 = off.
    pub hue_drift: f64,
    pub hue_drift_secs: u64,
    /// Fraction of brightness removed at night, 0 = off.
    pub night_dim: f64,
    pub night_start: u32,
    pub night_end: u32,
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
            local_calm_secs: 12,
            tile_size: 16,
            decay_after: 120,
            decay_chance: 0.03,
            noise_per_sec: 1.5,
            rule_shift_secs: 45,
            rule_shift_gens: 10,
            fastforward: true,
            fastforward_speed: 4,
            colonies: true,
            colony_mutation: 0.01,
            traffic: true,
            traffic_min_secs: 100,
            traffic_max_secs: 200,
            gun_secs: 60,
            symmetry_chance: 0.25,
            hypr_events: true,
            breathe: 0.3,
            breathe_secs: 90,
            glow: 0.25,
            vignette: 0.2,
            hue_drift: 0.0,
            hue_drift_secs: 900,
            night_dim: 0.0,
            night_start: 22,
            night_end: 7,
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
                colonies: default_colonies(),
            },
        }
    }
}

/// Gruvbox bright: yellow, green, orange, red, aqua, blue, purple.
pub fn default_colonies() -> Vec<Rgb> {
    vec![
        [0xfa, 0xbd, 0x2f],
        [0xb8, 0xbb, 0x26],
        [0xfe, 0x80, 0x19],
        [0xfb, 0x49, 0x34],
        [0x8e, 0xc0, 0x7c],
        [0x83, 0xa5, 0x98],
        [0xd3, 0x86, 0x9b],
    ]
}

pub const BOOL_KEYS: &[&str] = &[
    "trail", "rain", "overlay", "sysload", "patterns", "stamps", "smooth", "persist", "pause_hidden", "cell_round",
    "fastforward", "colonies", "traffic", "hypr_events",
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

fn parse_color_list(s: &str) -> Result<Vec<Rgb>, String> {
    let list = s.split(',').map(|c| parse_color(c.trim())).collect::<Result<Vec<_>, _>>()?;
    if !(2..=8).contains(&list.len()) {
        return Err(format!("colony_colors needs 2 to 8 colours, got {}", list.len()));
    }
    Ok(list)
}

fn parse_bool(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        _ => Err(format!("expected true/false, got {s:?}")),
    }
}

fn num<T: std::str::FromStr + PartialOrd + std::fmt::Display>(s: &str, lo: T, hi: T) -> Result<T, String> {
    let v: T = s.parse().map_err(|_| format!("bad number {s:?}"))?;
    if v < lo || v > hi {
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
                self.color = if val.eq_ignore_ascii_case("age") { ColorMode::Age } else { ColorMode::Fixed(parse_color(val)?) }
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
            "local_calm_secs" => self.local_calm_secs = num(val, 0, 86_400)?,
            "tile_size" => self.tile_size = num(val, 4, 64)?,
            "decay_after" => self.decay_after = num(val, 0, 250)?,
            "decay_chance" => self.decay_chance = num(val, 0.0, 1.0)?,
            "noise_per_sec" => self.noise_per_sec = num(val, 0.0, 200.0)?,
            "rule_shift_secs" => self.rule_shift_secs = num(val, 0, 86_400)?,
            "rule_shift_gens" => self.rule_shift_gens = num(val, 2, 200)?,
            "fastforward" => self.fastforward = parse_bool(val)?,
            "fastforward_speed" => self.fastforward_speed = num(val, 2, 12)?,
            "colonies" => self.colonies = parse_bool(val)?,
            "colony_mutation" => self.colony_mutation = num(val, 0.0, 0.5)?,
            "colony_colors" => self.palette.colonies = parse_color_list(val)?,
            "traffic" => self.traffic = parse_bool(val)?,
            "traffic_min_secs" => self.traffic_min_secs = num(val, 1, 86_400)?,
            "traffic_max_secs" => self.traffic_max_secs = num(val, 1, 86_400)?,
            "gun_secs" => self.gun_secs = num(val, 0, 86_400)?,
            "symmetry_chance" => self.symmetry_chance = num(val, 0.0, 1.0)?,
            "hypr_events" => self.hypr_events = parse_bool(val)?,
            "breathe" => self.breathe = num(val, 0.0, 0.8)?,
            "breathe_secs" => self.breathe_secs = num(val, 5, 86_400)?,
            "glow" => self.glow = num(val, 0.0, 1.0)?,
            "vignette" => self.vignette = num(val, 0.0, 1.0)?,
            "hue_drift" => self.hue_drift = num(val, 0.0, 180.0)?,
            "hue_drift_secs" => self.hue_drift_secs = num(val, 10, 86_400)?,
            "night_dim" => self.night_dim = num(val, 0.0, 0.8)?,
            "night_start" => self.night_start = num(val, 0, 23)?,
            "night_end" => self.night_end = num(val, 0, 23)?,
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
            let (k, v) = line.split_once('=').ok_or_else(|| format!("line {}: expected key = value", n + 1))?;
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(v);
            self.set(k.trim(), v).map_err(|e| format!("line {}: {e}", n + 1))?;
        }
        self.validate()
    }

    fn validate(&self) -> Result<(), String> {
        if self.glider_min_secs > self.glider_max_secs {
            return Err("glider_min_secs > glider_max_secs".into());
        }
        if self.traffic_min_secs > self.traffic_max_secs {
            return Err("traffic_min_secs > traffic_max_secs".into());
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
            cfg.apply_toml(text).map_err(|e| format!("preset {name}: {e}"))?;
        }
        let (path, explicit) = match &self.path {
            Some(p) => (Some(p.clone()), true),
            None => (Self::default_path(), false),
        };
        if let Some(p) = path {
            match std::fs::read_to_string(&p) {
                Ok(text) if self.preset.is_none() || explicit => {
                    cfg.apply_toml(&text).map_err(|e| format!("{}: {e}", p.display()))?
                }
                Ok(_) => {},
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Ship the template in the binary so first-run setup also works
                    // when the executable is launched outside the source directory.
                    let template = include_str!("../config_example/config.default.toml");
                    let create_result = p.parent()
                        .map(std::fs::create_dir_all)
                        .transpose()
                        .and_then(|_| std::fs::write(&p, template));
                    match create_result {
                        Ok(()) => {
                            eprintln!("life-wallpaper: created default config at {}", p.display());
                            if self.preset.is_none() {
                                cfg.apply_toml(template).map_err(|e| format!("embedded default config: {e}"))?;
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
            cfg.set(k, v).map_err(|e| format!("--{}: {e}", k.replace('_', "-")))?;
        }
        cfg.validate()?;
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
                          preset values are overridden by config file, then CLI flags
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

  liveliness (all also settable in the config file):
      --local-calm-secs S     seed a tile that has been quiet this long (0 = off, default 12)
      --tile-size CELLS       tile edge for local calm detection (default 16)
      --decay-after GENS      stuck cells may decay after this many generations (0 = off, default 120)
      --decay-chance P        per-generation chance such a cell dies (default 0.03)
      --noise-per-sec N       random single-cell disturbances per second (default 1.5)
      --rule-shift-secs S     briefly swap the rule every S seconds (0 = off, default 45)
      --rule-shift-gens N     how many generations each swap lasts (default 10)
      --[no-]fastforward      run extra generations per tick while the board is calm
      --fastforward-speed N   generations per tick while fast-forwarding (default 4)
      --[no-]colonies         births inherit their parents' colour (needs --color age)
      --colony-mutation P     chance a birth picks a random colony (default 0.01)
      --colony-colors LIST    comma separated colours, 2 to 8 (default gruvbox brights)
      --[no-]traffic          convoys of LWSS / MWSS / HWSS crossing the board
      --gun-secs S            lifetime of a Gosper gun (0 = forever, default 60)
      --symmetry-chance P     chance an injected soup is symmetric (default 0.25)
      --[no-]hypr-events      react to Hyprland workspace / window events

  look:
      --breathe DEPTH         generation rate swings by +-DEPTH over --breathe-secs (0 = off, default 0.3)
      --glow AMOUNT           halo around live cells, 0..1 (0 = off, default 0.25)
      --vignette AMOUNT       dim towards the screen edges, 0..1 (0 = off, default 0.2)
      --hue-drift DEGREES     palette hue wanders +-DEGREES over --hue-drift-secs (0 = off)
      --night-dim AMOUNT      fraction of brightness removed between --night-start and --night-end hours
  -h, --help

SIGHUP reloads the config file. Any option can also be set in the config as key = value
(use underscores: cell_size = 10). See the README comments in config.rs for all keys.";

pub fn parse_cli(args: impl Iterator<Item = String>) -> Result<Cli, String> {
    let mut cli = Cli { source: ConfigSource::default(), list_outputs: false };
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
            None => args.next().ok_or_else(|| format!("--{name} requires a value"))?,
        };
        if key == "config" {
            cli.source.path = Some(PathBuf::from(val));
        } else if key == "preset" {
            let valid = ["default", "ambient", "matrix-rain", "neon-brain", "minimal"];
            if !valid.contains(&val.as_str()) {
                return Err(format!("unknown preset {val:?} (choose {})", valid.join(", ")));
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
    }

    #[test]
    fn liveliness_and_look_keys() {
        let mut c = Config::default();
        c.apply_toml("local_calm_secs = 0\ntile_size = 24\ndecay_after = 90\ncolonies = false\nglow = 0.4\nnight_dim = 0.3\ncolony_colors = \"yellow, #112233, purple\"\n").unwrap();
        assert_eq!((c.local_calm_secs, c.tile_size, c.decay_after), (0, 24, 90));
        assert!(!c.colonies);
        assert_eq!(c.palette.colonies, vec![[0xfa, 0xbd, 0x2f], [0x11, 0x22, 0x33], [0xd3, 0x86, 0x9b]]);
        assert!(c.apply_toml("colony_colors = \"red\"").is_err()); // needs at least two
        assert!(c.apply_toml("decay_after = 900").is_err());
        assert!(c.apply_toml("breathe = 2").is_err());
        assert!(c.apply_toml("traffic_min_secs = 500\ntraffic_max_secs = 100").is_err());
        let args = ["--no-colonies", "--glow", "0.5", "--fastforward-speed=6", "--no-hypr-events"];
        let cli = parse_cli(args.iter().map(|s| s.to_string())).unwrap();
        let mut cfg = Config::default();
        for (k, v) in &cli.source.cli {
            cfg.set(k, v).unwrap();
        }
        assert!(!cfg.colonies && !cfg.hypr_events);
        assert_eq!((cfg.glow, cfg.fastforward_speed), (0.5, 6));
    }

    #[test]
    fn cli_preset() {
        let cli = parse_cli(["--preset", "ambient"].iter().map(|s| s.to_string())).unwrap();
        assert_eq!(cli.source.preset.as_deref(), Some("ambient"));
        assert!(parse_cli(["--preset", "unknown"].iter().map(|s| s.to_string())).is_err());
    }

    #[test]
    fn cli_flags() {
        let args = ["--cell-size", "12", "--no-trail", "--rain", "--rule=seeds", "-o", "DP-1", "--color", "yellow"];
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

    #[test]
    fn shipped_example_configs_all_parse() {
        for text in [
            include_str!("../config_example/config.default.toml"),
            include_str!("../config_example/ambient.toml"),
            include_str!("../config_example/matrix-rain.toml"),
            include_str!("../config_example/minimal.toml"),
            include_str!("../config_example/neon-brain.toml"),
        ] {
            let mut c = Config::default();
            c.apply_toml(text).unwrap();
        }
    }
}
