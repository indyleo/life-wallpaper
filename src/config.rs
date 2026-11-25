//! Configuration: defaults < `config.toml` (flat TOML subset) < CLI flags.
//! The same `Config::set` validates keys from both sources.

use std::path::{Path, PathBuf};

use crate::life::{Grid, Rule};

pub type Rgb = [u8; 3];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RuleSel {
    Fixed(Rule),
    Rotate,
}

/// How a rule shift is applied to the board.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShiftMode {
    /// The whole board changes rule at once and back again (a pulse).
    Global,
    /// Tiles change rule one ring at a time, starting at a random tile (a ripple).
    Ripple,
}

/// Shape of one live cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CellShape {
    Square,
    Circle,
    Hex,
    Rounded,
}

impl CellShape {
    pub fn name(self) -> &'static str {
        match self {
            CellShape::Square => "square",
            CellShape::Circle => "circle",
            CellShape::Hex => "hex",
            CellShape::Rounded => "rounded",
        }
    }
}

/// What the pointer leaves behind (`cursor_mode`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorMode {
    /// A line of live cells along the path.
    Trail,
    /// A few random cells around the pointer.
    Sparkle,
    /// A small soup under the pointer.
    Soup,
}

impl CursorMode {
    pub fn name(self) -> &'static str {
        match self {
            CursorMode::Trail => "trail",
            CursorMode::Sparkle => "sparkle",
            CursorMode::Soup => "soup",
        }
    }
}

/// How the age of a live cell maps to a colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgeShading {
    /// Four flat classes (newborn, mature, old, ancient). The original look.
    Steps,
    /// A continuous gradient through the same colours.
    Smooth,
}

/// Features that low-power mode switches off (`battery_disable`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Disable {
    pub traffic: bool,
    pub guns: bool,
    pub stamps: bool,
    pub audio: bool,
    pub patterns: bool,
}

impl Disable {
    pub const ALL: Disable = Disable {
        traffic: true,
        guns: true,
        stamps: true,
        audio: true,
        patterns: true,
    };

    pub fn parse(s: &str) -> Result<Self, String> {
        let mut d = Disable::default();
        for item in s
            .split(',')
            .map(|x| x.trim().trim_matches('"'))
            .filter(|x| !x.is_empty())
        {
            match item.to_ascii_lowercase().as_str() {
                "traffic" => d.traffic = true,
                "guns" => d.guns = true,
                "stamps" => d.stamps = true,
                "audio" => d.audio = true,
                "patterns" => d.patterns = true,
                "none" => {}
                _ => return Err(format!("unknown feature {item:?} in battery_disable (traffic, guns, stamps, audio, patterns)")),
            }
        }
        Ok(d)
    }

    pub fn list(&self) -> String {
        let mut v = Vec::new();
        for (on, name) in [
            (self.traffic, "traffic"),
            (self.guns, "guns"),
            (self.stamps, "stamps"),
            (self.audio, "audio"),
            (self.patterns, "patterns"),
        ] {
            if on {
                v.push(name);
            }
        }
        if v.is_empty() {
            "none".into()
        } else {
            v.join(",")
        }
    }
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
    /// Shape of a live cell (`cell_round` is the old two-way alias).
    pub cell_shape: CellShape,
    pub fps: f64,
    pub density: f64,
    pub reseed_density: f64,
    pub rule: RuleSel,
    pub color: ColorMode,
    /// `steps` (four flat classes) or `smooth` (continuous gradient by age).
    pub color_mode: AgeShading,
    /// Palette entries the smooth gradient runs through, as indices into
    /// newborn/mature/old/ancient (0..=3), at least two.
    pub color_smooth_stops: Vec<u8>,
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
    /// Rules a shift may swap in (one is picked at random each time).
    pub rule_shift_rules: Vec<Rule>,
    /// How many generations each swap lasts (generations, so it does not
    /// depend on `fps`).
    pub rule_shift_gens: u32,
    /// `ripple` (tile by tile) or `global` (whole board at once).
    pub rule_shift_mode: ShiftMode,
    /// Ripple only: generations between one ring of tiles and the next.
    pub rule_shift_stagger: u32,
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
    /// Ask Hyprland where a window is, so a closed window blooms where it was
    /// and a new one gets a halo around it. Falls back to random spots.
    pub hypr_locate: bool,
    /// Every this many seconds a stamp spells the time (HH:MM). 0 = off.
    pub clock_stamp_secs: u64,
    // ---- audio ----
    /// Gliders on beats, from a PipeWire capture of the default output. Off by default.
    pub audio: bool,
    /// Beat detector sensitivity: higher reacts to softer beats.
    pub audio_sensitivity: f64,
    /// Command that writes raw mono s16le audio at 11025 Hz to stdout.
    pub audio_command: String,
    /// Beats drop gliders (the original behaviour). Needs `audio`.
    pub audio_gliders: bool,
    /// The board lights up with the music; no cells are added. Needs `audio`.
    pub audio_react: bool,
    /// Multiplier on the measured level before it drives the light.
    pub audio_gain: f64,
    /// Brightness lift of live cells at full level (mid band).
    pub audio_brighten: f64,
    /// Extra halo at full level (bass band).
    pub audio_glow: f64,
    /// Background pulse at full level (treble band).
    pub audio_bg: f64,
    /// Strength of the flash on a beat.
    pub audio_flash: f64,
    pub audio_flash_ms: u64,
    pub audio_attack_ms: u64,
    pub audio_release_ms: u64,
    /// Levels below this (of full scale, after gain) light nothing.
    pub audio_floor: f64,
    /// Per-band beat sensitivity; `None` = follow `audio_sensitivity`.
    pub audio_band_bass_sensitivity: Option<f64>,
    pub audio_band_mid_sensitivity: Option<f64>,
    pub audio_band_treble_sensitivity: Option<f64>,
    /// Gliders follow the bass band instead of the full-range detector.
    pub audio_band_bass_gliders: bool,
    /// Mid band beats drop sparks.
    pub audio_band_mid_sparks: bool,
    /// Treble band beats add cosmic rays.
    pub audio_band_treble_noise: bool,
    /// Restart the audio helper (with backoff) when it exits.
    pub audio_restart_backoff: bool,
    pub audio_restart_max_secs: u64,
    // ---- control socket ----
    pub ipc: bool,
    /// Socket path; empty = `$XDG_RUNTIME_DIR/life-wallpaper-<output|all>.sock`.
    pub ipc_path: String,
    // ---- power ----
    pub power: bool,
    /// On battery, low-power mode starts at or below this charge (percent).
    pub battery_threshold: u32,
    pub battery_dim: f64,
    pub battery_fps_scale: f64,
    pub battery_disable: Disable,
    // ---- idle ----
    pub idle: bool,
    pub idle_secs: u64,
    pub idle_fps_scale: f64,
    pub idle_fastforward: bool,
    pub idle_dim: f64,
    pub idle_breathe: f64,
    // ---- media player ----
    pub mpris: bool,
    pub mpris_player: String,
    pub mpris_artist_colors: bool,
    // ---- more inputs ----
    /// Inject at the pointer (Hyprland only).
    pub cursor: bool,
    pub cursor_mode: CursorMode,
    pub cursor_radius: u32,
    pub cursor_interval_ms: u64,
    /// A workspace switch sends a quick ripple of the alternative rule
    /// across the board instead of being ignored by the cooldown.
    pub workspace_ripple: bool,
    /// Sustained loud bass blooms a pulsar (needs `audio`).
    pub audio_band_bass_bloom: bool,
    /// A command whose stdout lines are events (see README, `command`).
    pub command: String,
    /// 0 = a long-running process; N = run it every N seconds.
    pub command_interval_secs: u64,
    pub weather: bool,
    pub weather_command: String,
    pub weather_interval_secs: u64,
    /// Tint the palette by the weather (hue shift).
    pub weather_palette: bool,
    /// Board topology.
    pub grid: Grid,
    /// Population sparkline above the overlay text.
    pub overlay_sparkline: bool,
    // ---- diagnostics / scenes ----
    /// One status line per second on stderr.
    pub stats: bool,
    /// Extra diagnostics (audio levels, IPC commands, track changes).
    pub verbose: bool,
    /// Where named scenes live; empty = `<state dir>/scenes`.
    pub scenes_dir: String,
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
            cell_shape: CellShape::Rounded,
            fps: 4.0,
            density: 0.25,
            reseed_density: 0.30,
            rule: RuleSel::Fixed(crate::life::CONWAY),
            color: ColorMode::Age,
            color_mode: AgeShading::Steps,
            color_smooth_stops: vec![0, 1, 2, 3],
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
            rule_shift_rules: default_shift_rules(),
            rule_shift_gens: 10,
            rule_shift_mode: ShiftMode::Ripple,
            rule_shift_stagger: 3,
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
            hypr_locate: true,
            clock_stamp_secs: 600,
            audio: false,
            audio_sensitivity: 1.0,
            audio_command: DEFAULT_AUDIO_COMMAND.to_string(),
            audio_gliders: true,
            audio_react: true,
            audio_gain: 2.0,
            audio_brighten: 0.35,
            audio_glow: 0.35,
            audio_bg: 0.08,
            audio_flash: 0.5,
            audio_flash_ms: 150,
            audio_attack_ms: 30,
            audio_release_ms: 300,
            audio_floor: 0.02,
            audio_band_bass_sensitivity: None,
            audio_band_mid_sensitivity: None,
            audio_band_treble_sensitivity: None,
            audio_band_bass_gliders: false,
            audio_band_mid_sparks: false,
            audio_band_treble_noise: false,
            audio_restart_backoff: true,
            audio_restart_max_secs: 30,
            ipc: false,
            ipc_path: String::new(),
            power: false,
            battery_threshold: 40,
            battery_dim: 0.15,
            battery_fps_scale: 0.5,
            battery_disable: Disable::ALL,
            idle: false,
            idle_secs: 300,
            idle_fps_scale: 0.5,
            idle_fastforward: true,
            idle_dim: 0.2,
            idle_breathe: 0.6,
            mpris: false,
            mpris_player: String::new(),
            mpris_artist_colors: true,
            cursor: false,
            cursor_mode: CursorMode::Trail,
            cursor_radius: 2,
            cursor_interval_ms: 100,
            workspace_ripple: false,
            audio_band_bass_bloom: false,
            command: String::new(),
            command_interval_secs: 0,
            weather: false,
            weather_command: "curl -s -m 10 wttr.in/?format=j1".to_string(),
            weather_interval_secs: 900,
            weather_palette: true,
            grid: Grid::Square,
            overlay_sparkline: false,
            stats: false,
            verbose: false,
            scenes_dir: String::new(),
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

/// Rules a shift can swap in. All are Life plus extra survival or birth, so a
/// short burst loosens still lifes and oscillators and wakes the board up
/// without wiping it (measured: a few dozen generations roughly double the
/// activity and leave fewer settled cells behind; longer bursts flood the grid).
pub const DEFAULT_SHIFT_RULES: [&str; 3] = ["b34/s23", "b3/s234", "b3/s2345"];

pub fn default_shift_rules() -> Vec<Rule> {
    DEFAULT_SHIFT_RULES
        .iter()
        .filter_map(|r| Rule::parse(r))
        .collect()
}

fn parse_rule_list(s: &str) -> Result<Vec<Rule>, String> {
    let s = s.trim().trim_start_matches('[').trim_end_matches(']');
    let mut out = Vec::new();
    for item in s
        .split(',')
        .map(|x| x.trim().trim_matches('"').trim_matches('\''))
        .filter(|x| !x.is_empty())
    {
        out.push(
            Rule::parse(item)
                .ok_or_else(|| format!("unknown rule {item:?} in rule_shift_rules"))?,
        );
    }
    if out.is_empty() {
        return Err("rule_shift_rules needs at least one rule".into());
    }
    Ok(out)
}

const STOP_NAMES: [&str; 4] = ["newborn", "mature", "old", "ancient"];

fn parse_stops(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for item in s
        .split(',')
        .map(|x| x.trim().trim_matches('"'))
        .filter(|x| !x.is_empty())
    {
        let i = STOP_NAMES
            .iter()
            .position(|n| n.eq_ignore_ascii_case(item))
            .ok_or_else(|| format!("unknown stop {item:?} (newborn, mature, old, ancient)"))?;
        out.push(i as u8);
    }
    if out.len() < 2 {
        return Err("color_smooth_stops needs at least two stops".into());
    }
    Ok(out)
}

/// PipeWire's own recorder, pointed at the monitor of the default output
/// (whatever is playing), raw mono 16-bit at 11025 Hz on stdout (the rate the beat detector assumes).
pub const DEFAULT_AUDIO_COMMAND: &str =
    "pw-cat --record --format s16 --rate 11025 --channels 1 -P {stream.capture.sink=true} -";

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
    "fastforward",
    "colonies",
    "traffic",
    "hypr_events",
    "hypr_locate",
    "audio",
    "audio_react",
    "audio_gliders",
    "audio_band_bass_gliders",
    "audio_band_mid_sparks",
    "audio_band_treble_noise",
    "audio_restart_backoff",
    "ipc",
    "power",
    "idle",
    "idle_fastforward",
    "mpris",
    "mpris_artist_colors",
    "stats",
    "verbose",
    "cursor",
    "workspace_ripple",
    "audio_band_bass_bloom",
    "weather",
    "weather_palette",
    "overlay_sparkline",
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
    let list = s
        .split(',')
        .map(|c| parse_color(c.trim()))
        .collect::<Result<Vec<_>, _>>()?;
    if !(2..=8).contains(&list.len()) {
        return Err(format!(
            "colony_colors needs 2 to 8 colours, got {}",
            list.len()
        ));
    }
    Ok(list)
}

/// `master` / `auto` / empty = follow the master sensitivity, else 0.2..=5.
fn opt_sens(s: &str) -> Result<Option<f64>, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "master" | "auto" => Ok(None),
        _ => num(s, 0.2, 5.0).map(Some),
    }
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
            // old alias: true = rounded, false = square (the later of the two keys wins)
            "cell_round" => {
                self.cell_shape = if parse_bool(val)? {
                    CellShape::Rounded
                } else {
                    CellShape::Square
                }
            }
            "cell_shape" => {
                self.cell_shape = match val.to_ascii_lowercase().as_str() {
                    "square" => CellShape::Square,
                    "circle" => CellShape::Circle,
                    "hex" => CellShape::Hex,
                    "rounded" => CellShape::Rounded,
                    _ => {
                        return Err(format!(
                            "unknown cell_shape {val:?} (square, circle, hex, rounded)"
                        ))
                    }
                }
            }
            "color_mode" => {
                self.color_mode = match val.to_ascii_lowercase().as_str() {
                    "steps" => AgeShading::Steps,
                    "smooth" => AgeShading::Smooth,
                    _ => return Err(format!("unknown color_mode {val:?} (steps or smooth)")),
                }
            }
            "color_smooth_stops" => self.color_smooth_stops = parse_stops(val)?,
            "rule_shift_rules" => self.rule_shift_rules = parse_rule_list(val)?,
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
            "local_calm_secs" => self.local_calm_secs = num(val, 0, 86_400)?,
            "tile_size" => self.tile_size = num(val, 4, 64)?,
            "decay_after" => self.decay_after = num(val, 0, 250)?,
            "decay_chance" => self.decay_chance = num(val, 0.0, 1.0)?,
            "noise_per_sec" => self.noise_per_sec = num(val, 0.0, 200.0)?,
            "rule_shift_secs" => self.rule_shift_secs = num(val, 0, 86_400)?,
            "rule_shift_gens" => self.rule_shift_gens = num(val, 2, 200)?,
            "rule_shift_mode" => {
                self.rule_shift_mode = match val.to_ascii_lowercase().as_str() {
                    "ripple" => ShiftMode::Ripple,
                    "global" => ShiftMode::Global,
                    _ => {
                        return Err(format!(
                            "unknown rule_shift_mode {val:?} (ripple or global)"
                        ))
                    }
                }
            }
            "rule_shift_stagger" => self.rule_shift_stagger = num(val, 0, 50)?,
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
            "hypr_locate" => self.hypr_locate = parse_bool(val)?,
            "clock_stamp_secs" => self.clock_stamp_secs = num(val, 0, 86_400)?,
            "audio" => self.audio = parse_bool(val)?,
            "audio_sensitivity" => self.audio_sensitivity = num(val, 0.2, 5.0)?,
            "audio_command" => {
                if val.split_whitespace().next().is_none() {
                    return Err("audio_command is empty".into());
                }
                self.audio_command = val.to_string()
            }
            "audio_gliders" => self.audio_gliders = parse_bool(val)?,
            "audio_react" => self.audio_react = parse_bool(val)?,
            "audio_gain" => self.audio_gain = num(val, 0.1, 20.0)?,
            "audio_brighten" => self.audio_brighten = num(val, 0.0, 1.0)?,
            "audio_glow" => self.audio_glow = num(val, 0.0, 1.0)?,
            "audio_bg" => self.audio_bg = num(val, 0.0, 0.5)?,
            "audio_flash" => self.audio_flash = num(val, 0.0, 1.0)?,
            "audio_flash_ms" => self.audio_flash_ms = num(val, 20, 2000)?,
            "audio_attack_ms" => self.audio_attack_ms = num(val, 1, 2000)?,
            "audio_release_ms" => self.audio_release_ms = num(val, 1, 10_000)?,
            "audio_floor" => self.audio_floor = num(val, 0.0, 0.5)?,
            "audio_band_bass_sensitivity" => self.audio_band_bass_sensitivity = opt_sens(val)?,
            "audio_band_mid_sensitivity" => self.audio_band_mid_sensitivity = opt_sens(val)?,
            "audio_band_treble_sensitivity" => self.audio_band_treble_sensitivity = opt_sens(val)?,
            "audio_band_bass_gliders" => self.audio_band_bass_gliders = parse_bool(val)?,
            "audio_band_mid_sparks" => self.audio_band_mid_sparks = parse_bool(val)?,
            "audio_band_treble_noise" => self.audio_band_treble_noise = parse_bool(val)?,
            "audio_restart_backoff" => self.audio_restart_backoff = parse_bool(val)?,
            "audio_restart_max_secs" => self.audio_restart_max_secs = num(val, 1, 3600)?,
            "ipc" => self.ipc = parse_bool(val)?,
            "ipc_path" => self.ipc_path = val.to_string(),
            "power" => self.power = parse_bool(val)?,
            "battery_threshold" => self.battery_threshold = num(val, 0, 100)?,
            "battery_dim" => self.battery_dim = num(val, 0.0, 0.8)?,
            "battery_fps_scale" => self.battery_fps_scale = num(val, 0.1, 1.0)?,
            "battery_disable" => self.battery_disable = Disable::parse(val)?,
            "idle" => self.idle = parse_bool(val)?,
            "idle_secs" => self.idle_secs = num(val, 5, 86_400)?,
            "idle_fps_scale" => self.idle_fps_scale = num(val, 0.1, 1.0)?,
            "idle_fastforward" => self.idle_fastforward = parse_bool(val)?,
            "idle_dim" => self.idle_dim = num(val, 0.0, 0.8)?,
            "idle_breathe" => self.idle_breathe = num(val, 0.0, 0.8)?,
            "mpris" => self.mpris = parse_bool(val)?,
            "mpris_player" => self.mpris_player = val.to_string(),
            "mpris_artist_colors" => self.mpris_artist_colors = parse_bool(val)?,
            "cursor" => self.cursor = parse_bool(val)?,
            "cursor_mode" => {
                self.cursor_mode = match val.to_ascii_lowercase().as_str() {
                    "trail" => CursorMode::Trail,
                    "sparkle" => CursorMode::Sparkle,
                    "soup" => CursorMode::Soup,
                    _ => {
                        return Err(format!(
                            "unknown cursor_mode {val:?} (trail, sparkle, soup)"
                        ))
                    }
                }
            }
            "cursor_radius" => self.cursor_radius = num(val, 1, 16)?,
            "cursor_interval_ms" => self.cursor_interval_ms = num(val, 20, 2000)?,
            "workspace_ripple" => self.workspace_ripple = parse_bool(val)?,
            "audio_band_bass_bloom" => self.audio_band_bass_bloom = parse_bool(val)?,
            "command" => self.command = val.to_string(),
            "command_interval_secs" => self.command_interval_secs = num(val, 0, 86_400)?,
            "weather" => self.weather = parse_bool(val)?,
            "weather_command" => self.weather_command = val.to_string(),
            "weather_interval_secs" => self.weather_interval_secs = num(val, 60, 86_400)?,
            "weather_palette" => self.weather_palette = parse_bool(val)?,
            "grid" => {
                self.grid = Grid::parse(val)
                    .ok_or_else(|| format!("unknown grid {val:?} (square, hex, von-neumann)"))?
            }
            "overlay_sparkline" => self.overlay_sparkline = parse_bool(val)?,
            "stats" => self.stats = parse_bool(val)?,
            "verbose" => self.verbose = parse_bool(val)?,
            "scenes_dir" => self.scenes_dir = val.to_string(),
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
        self.validate()
    }

    /// Every key with its resolved value, as a config file that parses back
    /// into the same `Config` (`--dump-config`).
    pub fn dump(&self) -> String {
        struct Out(String);
        impl Out {
            fn head(&mut self, t: &str) {
                if !self.0.is_empty() {
                    self.0.push('\n');
                }
                self.0.push_str("# ");
                self.0.push_str(t);
                self.0.push('\n');
            }
            fn kv(&mut self, k: &str, v: impl std::fmt::Display) {
                self.0.push_str(&format!("{k} = {v}\n"));
            }
            fn q(&mut self, k: &str, v: &str) {
                self.0.push_str(&format!("{k} = \"{v}\"\n"));
            }
        }
        let hex = |c: Rgb| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        let mut o = Out(String::new());
        o.head("grid and look");
        o.kv("cell_size", self.cell_size);
        o.kv("cell_gap", self.cell_gap);
        o.q("cell_shape", self.cell_shape.name());
        o.kv("fps", self.fps);
        o.kv("density", self.density);
        o.kv("reseed_density", self.reseed_density);
        match self.rule {
            RuleSel::Fixed(r) => o.q("rule", &r.name()),
            RuleSel::Rotate => o.q("rule", "rotate"),
        }
        match self.color {
            ColorMode::Age => o.q("color", "age"),
            ColorMode::Fixed(c) => o.q("color", &hex(c)),
        }
        o.q(
            "color_mode",
            if self.color_mode == AgeShading::Smooth {
                "smooth"
            } else {
                "steps"
            },
        );
        let stops: Vec<&str> = self
            .color_smooth_stops
            .iter()
            .map(|&i| STOP_NAMES[(i as usize).min(3)])
            .collect();
        o.q("color_smooth_stops", &stops.join(","));
        o.kv("trail", self.trail);
        o.kv("trail_length", self.trail_length);
        o.kv("rain", self.rain);
        o.kv("rain_density", self.rain_density);
        o.kv("overlay", self.overlay);
        o.kv("overlay_sparkline", self.overlay_sparkline);
        o.q("grid", self.grid.name());
        o.kv("sysload", self.sysload);
        o.kv("patterns", self.patterns);
        o.kv("stamps", self.stamps);
        o.kv("smooth", self.smooth);
        o.kv("render_fps", self.render_fps);
        o.kv("persist", self.persist);
        o.kv("save_secs", self.save_secs);
        o.kv("pause_hidden", self.pause_hidden);
        o.kv("breathe", self.breathe);
        o.kv("breathe_secs", self.breathe_secs);
        o.kv("glow", self.glow);
        o.kv("vignette", self.vignette);
        o.kv("hue_drift", self.hue_drift);
        o.kv("hue_drift_secs", self.hue_drift_secs);
        o.kv("night_dim", self.night_dim);
        o.kv("night_start", self.night_start);
        o.kv("night_end", self.night_end);
        o.head("liveliness");
        o.kv("soft_reset_secs", self.soft_reset_secs);
        o.kv("stagnation_secs", self.stagnation_secs);
        o.kv("clutter_pct", self.clutter_pct);
        o.kv("calm_pct", self.calm_pct);
        o.kv("local_calm_secs", self.local_calm_secs);
        o.kv("tile_size", self.tile_size);
        o.kv("decay_after", self.decay_after);
        o.kv("decay_chance", self.decay_chance);
        o.kv("noise_per_sec", self.noise_per_sec);
        o.kv("rule_shift_secs", self.rule_shift_secs);
        let rules: Vec<String> = self.rule_shift_rules.iter().map(|r| r.name()).collect();
        o.q("rule_shift_rules", &rules.join(","));
        o.kv("rule_shift_gens", self.rule_shift_gens);
        o.q(
            "rule_shift_mode",
            if self.rule_shift_mode == ShiftMode::Ripple {
                "ripple"
            } else {
                "global"
            },
        );
        o.kv("rule_shift_stagger", self.rule_shift_stagger);
        o.kv("fastforward", self.fastforward);
        o.kv("fastforward_speed", self.fastforward_speed);
        o.kv("colonies", self.colonies);
        o.kv("colony_mutation", self.colony_mutation);
        let cols: Vec<String> = self.palette.colonies.iter().map(|&c| hex(c)).collect();
        o.q("colony_colors", &cols.join(","));
        o.kv("traffic", self.traffic);
        o.kv("traffic_min_secs", self.traffic_min_secs);
        o.kv("traffic_max_secs", self.traffic_max_secs);
        o.kv("gun_secs", self.gun_secs);
        o.kv("symmetry_chance", self.symmetry_chance);
        o.kv("glider_min_secs", self.glider_min_secs);
        o.kv("glider_max_secs", self.glider_max_secs);
        o.kv("special_min_secs", self.special_min_secs);
        o.kv("special_max_secs", self.special_max_secs);
        o.kv("stamp_min_secs", self.stamp_min_secs);
        o.kv("stamp_max_secs", self.stamp_max_secs);
        o.kv("clock_stamp_secs", self.clock_stamp_secs);
        o.kv("hypr_events", self.hypr_events);
        o.kv("hypr_locate", self.hypr_locate);
        o.head("audio");
        o.kv("audio", self.audio);
        o.kv("audio_sensitivity", self.audio_sensitivity);
        o.q("audio_command", &self.audio_command);
        o.kv("audio_gliders", self.audio_gliders);
        o.kv("audio_react", self.audio_react);
        o.kv("audio_gain", self.audio_gain);
        o.kv("audio_brighten", self.audio_brighten);
        o.kv("audio_glow", self.audio_glow);
        o.kv("audio_bg", self.audio_bg);
        o.kv("audio_flash", self.audio_flash);
        o.kv("audio_flash_ms", self.audio_flash_ms);
        o.kv("audio_attack_ms", self.audio_attack_ms);
        o.kv("audio_release_ms", self.audio_release_ms);
        o.kv("audio_floor", self.audio_floor);
        for (k, v) in [
            (
                "audio_band_bass_sensitivity",
                self.audio_band_bass_sensitivity,
            ),
            (
                "audio_band_mid_sensitivity",
                self.audio_band_mid_sensitivity,
            ),
            (
                "audio_band_treble_sensitivity",
                self.audio_band_treble_sensitivity,
            ),
        ] {
            match v {
                Some(x) => o.kv(k, x),
                None => o.q(k, "master"),
            }
        }
        o.kv("audio_band_bass_gliders", self.audio_band_bass_gliders);
        o.kv("audio_band_mid_sparks", self.audio_band_mid_sparks);
        o.kv("audio_band_treble_noise", self.audio_band_treble_noise);
        o.kv("audio_restart_backoff", self.audio_restart_backoff);
        o.kv("audio_restart_max_secs", self.audio_restart_max_secs);
        o.kv("audio_band_bass_bloom", self.audio_band_bass_bloom);
        o.head("control socket, power, idle, media player");
        o.kv("ipc", self.ipc);
        o.q("ipc_path", &self.ipc_path);
        o.kv("power", self.power);
        o.kv("battery_threshold", self.battery_threshold);
        o.kv("battery_dim", self.battery_dim);
        o.kv("battery_fps_scale", self.battery_fps_scale);
        o.q("battery_disable", &self.battery_disable.list());
        o.kv("idle", self.idle);
        o.kv("idle_secs", self.idle_secs);
        o.kv("idle_fps_scale", self.idle_fps_scale);
        o.kv("idle_fastforward", self.idle_fastforward);
        o.kv("idle_dim", self.idle_dim);
        o.kv("idle_breathe", self.idle_breathe);
        o.kv("mpris", self.mpris);
        o.q("mpris_player", &self.mpris_player);
        o.kv("mpris_artist_colors", self.mpris_artist_colors);
        o.head("pointer, workspace, command, weather");
        o.kv("cursor", self.cursor);
        o.q("cursor_mode", self.cursor_mode.name());
        o.kv("cursor_radius", self.cursor_radius);
        o.kv("cursor_interval_ms", self.cursor_interval_ms);
        o.kv("workspace_ripple", self.workspace_ripple);
        o.q("command", &self.command);
        o.kv("command_interval_secs", self.command_interval_secs);
        o.kv("weather", self.weather);
        o.q("weather_command", &self.weather_command);
        o.kv("weather_interval_secs", self.weather_interval_secs);
        o.kv("weather_palette", self.weather_palette);
        o.head("diagnostics and scenes");
        o.kv("stats", self.stats);
        o.kv("verbose", self.verbose);
        o.q("scenes_dir", &self.scenes_dir);
        o.head("palette");
        let p = &self.palette;
        for (k, c) in [
            ("bg", p.bg),
            ("newborn", p.newborn),
            ("mature", p.mature),
            ("old", p.old),
            ("ancient", p.ancient),
            ("dying", p.dying),
            ("rain_color", p.rain),
            ("overlay_color", p.overlay),
        ] {
            o.q(k, &hex(c));
        }
        if let Some(out) = &self.output {
            o.head("output");
            o.q("output", out);
        }
        o.0
    }

    pub fn validate(&self) -> Result<(), String> {
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
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
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
        cfg.validate()?;
        Ok(cfg)
    }
}

/// One `--inject NAME [--at X Y] [--edge E]` request.
#[derive(Clone, Debug, PartialEq)]
pub struct InjectReq {
    pub name: String,
    pub at: Option<(usize, usize)>,
    pub edge: Option<String>,
}

pub struct Cli {
    pub source: ConfigSource,
    pub list_outputs: bool,
    pub version: bool,
    pub dump_config: bool,
    /// `--validate-config PATH`: parse the file, report, exit.
    pub validate: Option<PathBuf>,
    /// `--once`: render headless to the PNG named by `--output`, then exit.
    pub once: bool,
    pub once_size: (usize, usize),
    pub once_gens: u32,
    pub inject: Vec<InjectReq>,
    pub load_scene: Option<String>,
    pub save_scene: Option<String>,
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
      --cell-shape SHAPE  square | circle | hex | rounded (default rounded)
      --[no-]cell-round   old alias: on = rounded, off = square
      --color-mode MODE   steps (four flat age classes, default) | smooth (continuous gradient)
      --color-smooth-stops LIST   smooth: palette entries to run through (default newborn,mature,old,ancient)

  liveliness (all also settable in the config file):
      --local-calm-secs S     seed a tile that has been quiet this long (0 = off, default 12)
      --tile-size CELLS       tile edge for local calm detection (default 16)
      --decay-after GENS      stuck cells may decay after this many generations (0 = off, default 120)
      --decay-chance P        per-generation chance such a cell dies (default 0.03)
      --noise-per-sec N       random single-cell disturbances per second (default 1.5)
      --rule-shift-secs S     briefly swap the rule every S seconds (0 = off, default 45)
      --rule-shift-gens N     how many generations each swap lasts (default 10)
      --rule-shift-mode MODE  ripple (tile by tile, default) | global (whole board at once)
      --rule-shift-stagger N  ripple: generations between one ring of tiles and the next (default 3)
      --[no-]fastforward      run extra generations per tick while the board is calm
      --fastforward-speed N   generations per tick while fast-forwarding (default 4)
      --[no-]colonies         births inherit their parents' colour (needs --color age)
      --colony-mutation P     chance a birth picks a random colony (default 0.01)
      --colony-colors LIST    comma separated colours, 2 to 8 (default gruvbox brights)
      --[no-]traffic          convoys of LWSS / MWSS / HWSS crossing the board
      --gun-secs S            lifetime of a Gosper gun (0 = forever, default 60)
      --symmetry-chance P     chance an injected soup is symmetric (default 0.25)
      --[no-]hypr-events      react to Hyprland workspace / window events
      --[no-]hypr-locate      ask Hyprland where windows are, so life appears there (default on)
      --clock-stamp-secs S    stamp the time (HH:MM) every ~S seconds (0 = off, default 600)

  audio (pw-cat, falling back to parec / pacat; on/off and command are read at start-up):
      --[no-]audio            react to whatever is playing (default off)
      --[no-]audio-gliders    beats drop gliders (default on, needs --audio)
      --[no-]audio-react      the board lights up with the music, no cells added (default on, needs --audio)
      --audio-sensitivity X   0.2..5, higher reacts to softer beats (default 1)
      --audio-command CMD     raw mono s16le @ 11025 Hz on stdout (default: pw-cat, parec or pacat on the sink monitor)
      --audio-gain X --audio-brighten X --audio-glow X --audio-bg X --audio-flash X
      --audio-flash-ms MS --audio-attack-ms MS --audio-release-ms MS --audio-floor X     light tuning
      --audio-band-{bass,mid,treble}-sensitivity X|master     per-band beat sensitivity
      --[no-]audio-band-bass-gliders  --[no-]audio-band-mid-sparks  --[no-]audio-band-treble-noise    band routing
      --[no-]audio-restart-backoff    restart the helper when it exits (default on)
      --audio-restart-max-secs S      longest wait between restarts (default 30)

  control, power, idle, media (all off by default):
      --[no-]ipc              control socket (see life-wallpaper-ctl); --ipc-path PATH
      --[no-]power            low-power mode on battery: --battery-threshold PCT --battery-dim X
                              --battery-fps-scale X --battery-disable LIST
      --[no-]idle             calmer board after --idle-secs S without pointer or window activity
      --[no-]mpris            react to the active media player (needs playerctl); --mpris-player NAME

  more (all off by default):
      --grid G                square | hex | von-neumann (board topology; hex wants an even row count and is drawn offset)
      --[no-]overlay-sparkline    population / settled sparkline above the overlay text (needs --overlay)
      --[no-]cursor           inject at the pointer (Hyprland): --cursor-mode trail|sparkle|soup --cursor-radius N --cursor-interval-ms MS
      --[no-]workspace-ripple a workspace switch sends a quick ripple of the alternative rule across the board
      --[no-]audio-band-bass-bloom    sustained loud bass blooms a pulsar (needs --audio)
      --command CMD [--command-interval-secs S]   stdout lines of CMD are events (JSON object or ctl syntax)
      --[no-]weather          react to the weather: --weather-command CMD --weather-interval-secs S --[no-]weather-palette

  tools:
      --version               print the version and exit
      --dump-config           print the resolved config (defaults + preset + file + flags) and exit
      --validate-config PATH  parse a config file, report, and exit (non-zero on error)
      --once                  render one frame headless to the PNG named by --output FILE.png, then exit
      --once-size WxH         size of that frame in pixels (default 1920x1080); --once-gens N (default 200)
      --inject NAME [--at X Y] [--edge E]   place a pattern at start-up (or in --once); text=HELLO spells text
      --load-scene NAME / --save-scene NAME   board scenes (--save-scene saves on exit and after --once)
      --[no-]stats            one status line per second on stderr
      --[no-]verbose          audio levels, IPC commands, track changes on stderr

  look:
      --breathe DEPTH         generation rate swings by +-DEPTH over --breathe-secs (0 = off, default 0.3)
      --glow AMOUNT           halo around live cells, 0..1 (0 = off, default 0.25)
      --vignette AMOUNT       dim towards the screen edges, 0..1 (0 = off, default 0.2)
      --hue-drift DEGREES     palette hue wanders +-DEGREES over --hue-drift-secs (0 = off)
      --night-dim AMOUNT      fraction of brightness removed between --night-start and --night-end hours
  -h, --help

SIGHUP reloads the config file (edits to it are also picked up automatically). Any option can
also be set in the config as key = value (use underscores: cell_size = 10). Run
`life-wallpaper --dump-config` to see every key with its current value.";

pub fn parse_cli(args: impl Iterator<Item = String>) -> Result<Cli, String> {
    let mut cli = Cli {
        source: ConfigSource::default(),
        list_outputs: false,
        version: false,
        dump_config: false,
        validate: None,
        once: false,
        once_size: (1920, 1080),
        once_gens: 200,
        inject: Vec::new(),
        load_scene: None,
        save_scene: None,
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
        if a == "--version" || a == "-V" {
            cli.version = true;
            continue;
        }
        if a == "--dump-config" {
            cli.dump_config = true;
            continue;
        }
        if a == "--once" {
            cli.once = true;
            continue;
        }
        if a == "--at" {
            let (x, y) = (args.next(), args.next());
            let parse = |v: Option<String>| v.and_then(|v| v.parse::<usize>().ok());
            let (Some(x), Some(y)) = (parse(x), parse(y)) else {
                return Err("--at needs two cell coordinates: --at X Y".into());
            };
            match cli.inject.last_mut() {
                Some(r) => r.at = Some((x, y)),
                None => return Err("--at must follow --inject NAME".into()),
            }
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
        } else if key == "validate_config" {
            cli.validate = Some(PathBuf::from(val));
        } else if key == "once_size" {
            let (w, h) = val
                .split_once(['x', 'X'])
                .ok_or("--once-size wants WIDTHxHEIGHT, e.g. 1920x1080")?;
            let (w, h): (usize, usize) = (
                w.parse().map_err(|_| "bad --once-size width")?,
                h.parse().map_err(|_| "bad --once-size height")?,
            );
            if !(16..=16_384).contains(&w) || !(16..=16_384).contains(&h) {
                return Err("--once-size must be between 16 and 16384 pixels each way".into());
            }
            cli.once_size = (w, h);
        } else if key == "once_gens" {
            cli.once_gens = val
                .parse()
                .map_err(|_| format!("bad --once-gens {val:?}"))?;
        } else if key == "inject" {
            cli.inject.push(InjectReq {
                name: val,
                at: None,
                edge: None,
            });
        } else if key == "edge" {
            match cli.inject.last_mut() {
                Some(r) => r.edge = Some(val),
                None => return Err("--edge must follow --inject NAME".into()),
            }
        } else if key == "load_scene" {
            cli.load_scene = Some(val);
        } else if key == "save_scene" {
            cli.save_scene = Some(val);
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
    }

    #[test]
    fn liveliness_and_look_keys() {
        let mut c = Config::default();
        c.apply_toml("local_calm_secs = 0\ntile_size = 24\ndecay_after = 90\ncolonies = false\nglow = 0.4\nnight_dim = 0.3\ncolony_colors = \"yellow, #112233, purple\"\n").unwrap();
        assert_eq!((c.local_calm_secs, c.tile_size, c.decay_after), (0, 24, 90));
        assert!(!c.colonies);
        assert_eq!(
            c.palette.colonies,
            vec![[0xfa, 0xbd, 0x2f], [0x11, 0x22, 0x33], [0xd3, 0x86, 0x9b]]
        );
        assert!(c.apply_toml("colony_colors = \"red\"").is_err()); // needs at least two
        assert!(c.apply_toml("decay_after = 900").is_err());
        assert!(c.apply_toml("breathe = 2").is_err());
        assert!(c
            .apply_toml("traffic_min_secs = 500\ntraffic_max_secs = 100")
            .is_err());
        let args = [
            "--no-colonies",
            "--glow",
            "0.5",
            "--fastforward-speed=6",
            "--no-hypr-events",
        ];
        let cli = parse_cli(args.iter().map(|s| s.to_string())).unwrap();
        let mut cfg = Config::default();
        for (k, v) in &cli.source.cli {
            cfg.set(k, v).unwrap();
        }
        assert!(!cfg.colonies && !cfg.hypr_events);
        assert_eq!((cfg.glow, cfg.fastforward_speed), (0.5, 6));
    }

    #[test]
    fn ripple_window_clock_and_audio_keys() {
        let mut c = Config::default();
        assert_eq!(c.rule_shift_mode, ShiftMode::Ripple);
        assert!(!c.audio && c.hypr_locate);
        c.apply_toml("rule_shift_mode = \"global\"\nrule_shift_stagger = 0\nclock_stamp_secs = 0\naudio = true\naudio_sensitivity = 2\nhypr_locate = false\n").unwrap();
        assert_eq!(c.rule_shift_mode, ShiftMode::Global);
        assert_eq!((c.rule_shift_stagger, c.clock_stamp_secs), (0, 0));
        assert!(c.audio && !c.hypr_locate);
        assert!(c.apply_toml("rule_shift_mode = \"wave\"").is_err());
        assert!(c.apply_toml("audio_sensitivity = 9").is_err());
        assert!(c.apply_toml("audio_command = \"\"").is_err());
        c.apply_toml("audio_command = \"parec --format=s16le --rate=11025 --channels=1\"")
            .unwrap();
        assert!(c.audio_command.starts_with("parec"));
        let cli = parse_cli(
            ["--audio", "--no-hypr-locate", "--rule-shift-mode=global"]
                .iter()
                .map(|s| s.to_string()),
        )
        .unwrap();
        let mut cfg = Config::default();
        for (k, v) in &cli.source.cli {
            cfg.set(k, v).unwrap();
        }
        assert!(cfg.audio && !cfg.hypr_locate && cfg.rule_shift_mode == ShiftMode::Global);
    }

    #[test]
    fn dump_config_parses_back_to_the_same_config() {
        let mut c = Config::default();
        c.apply_toml("cell_shape = \"circle\"\ncolor_mode = \"smooth\"\ncolor_smooth_stops = \"newborn,ancient\"\naudio_band_mid_sensitivity = 1.5\nrule_shift_rules = \"b3/s234,b36/s23\"\nbattery_disable = \"audio,guns\"\nrule = \"highlife\"\ncolor = \"#112233\"\ngrid = \"hex\"\ncursor_mode = \"soup\"\ncommand = \"echo hi\"\n").unwrap();
        let d = c.dump();
        let mut back = Config::default();
        back.apply_toml(&d).unwrap();
        assert_eq!(back.dump(), d);
        assert_eq!(back.cell_shape, CellShape::Circle);
        assert_eq!(back.grid, Grid::Hex);
        assert_eq!(back.cursor_mode, CursorMode::Soup);
        assert_eq!(back.command, "echo hi");
        assert_eq!(back.color_smooth_stops, vec![0, 3]);
        assert_eq!(back.audio_band_mid_sensitivity, Some(1.5));
        assert_eq!(back.audio_band_bass_sensitivity, None);
        assert_eq!(back.rule_shift_rules.len(), 2);
        assert!(
            back.battery_disable.audio
                && back.battery_disable.guns
                && !back.battery_disable.traffic
        );
        assert_eq!(
            Config::default()
                .dump()
                .lines()
                .filter(|l| l.contains(" = "))
                .count()
                > 100,
            true
        );
    }

    #[test]
    fn cell_round_is_an_alias_for_cell_shape() {
        let mut c = Config::default();
        assert_eq!(
            c.cell_shape,
            CellShape::Rounded,
            "default look is unchanged"
        );
        c.apply_toml("cell_round = false").unwrap();
        assert_eq!(c.cell_shape, CellShape::Square);
        c.apply_toml("cell_round = true").unwrap();
        assert_eq!(c.cell_shape, CellShape::Rounded);
        assert!(c.apply_toml("cell_shape = \"star\"").is_err());
    }

    #[test]
    fn new_audio_and_tool_flags_parse() {
        let args = [
            "--once",
            "--output",
            "out.png",
            "--once-size",
            "800x600",
            "--inject",
            "glider",
            "--at",
            "5",
            "7",
            "--no-audio-react",
            "--stats",
            "--verbose",
            "--validate-config",
            "/tmp/x.toml",
            "--version",
        ];
        let cli = parse_cli(args.iter().map(|s| s.to_string())).unwrap();
        assert!(cli.once && cli.version);
        assert_eq!(cli.once_size, (800, 600));
        assert_eq!(
            cli.inject,
            vec![InjectReq {
                name: "glider".into(),
                at: Some((5, 7)),
                edge: None
            }]
        );
        assert_eq!(cli.validate.as_deref(), Some(Path::new("/tmp/x.toml")));
        let mut cfg = Config::default();
        for (k, v) in &cli.source.cli {
            cfg.set(k, v).unwrap();
        }
        assert!(!cfg.audio_react && cfg.stats && cfg.verbose);
        assert_eq!(cfg.output.as_deref(), Some("out.png"));
        assert!(
            parse_cli(["--at", "1", "2"].iter().map(|s| s.to_string())).is_err(),
            "--at needs an --inject"
        );
        assert!(parse_cli(["--once-size", "10x10"].iter().map(|s| s.to_string())).is_err());
    }

    #[test]
    fn rule_list_stops_and_ranges_are_validated() {
        let mut c = Config::default();
        assert_eq!(c.rule_shift_rules.len(), 3);
        c.apply_toml("rule_shift_rules = \"b34/s23\"").unwrap();
        assert_eq!(c.rule_shift_rules.len(), 1);
        assert!(c.apply_toml("rule_shift_rules = \"nonsense\"").is_err());
        assert!(c.apply_toml("rule_shift_rules = \"\"").is_err());
        assert!(c.apply_toml("color_smooth_stops = \"newborn\"").is_err());
        assert!(c
            .apply_toml("color_smooth_stops = \"newborn,purple\"")
            .is_err());
        assert!(c.apply_toml("audio_flash_ms = 5").is_err());
        assert!(c.apply_toml("audio_band_bass_sensitivity = 9").is_err());
        c.apply_toml("audio_band_bass_sensitivity = \"master\"")
            .unwrap();
        assert!(c.apply_toml("battery_disable = \"wifi\"").is_err());
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
