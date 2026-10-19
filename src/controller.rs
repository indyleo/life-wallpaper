//! Policy layer, one `Sim` per output: owns the automaton, the code-rain layer
//! and every timer. Nothing here ever wipes the board on a timer; it keeps
//! things lively by *adding* and *thinning*:
//!
//! * **Local calm**: the board is cut into tiles; a tile with nothing moving in
//!   it for `local_calm_secs` gets a soup patch, an aimed glider or a methuselah.
//! * **Global calm / clutter**: the older whole-board checks (stagnation, pruning
//!   of settled shapes) still run as a safety net.
//! * **Decay and cosmic rays**: cells stuck in still lifes / oscillators slowly
//!   die off, and a couple of random cells are disturbed every second, so
//!   nothing stays frozen for ever.
//! * **Rule shifts**: now and then the rule is swapped for a few generations
//!   and back, which shakes settled structures loose. By default as a ripple:
//!   tiles switch over ring by ring from a random tile, so it travels across
//!   the board instead of flipping all of it at once.
//! * **Fast-forward** through calm stretches, convoys of spaceships, short-lived
//!   Gosper guns, a clock stamp, beats from the audio meter, and reactions to
//!   Hyprland events (at the window's own spot when it is known).
//!
//! Pure logic driven by `tick(now, ..)`; no Wayland in here.

use std::time::{Duration, Instant};

use rand::{rngs::StdRng, seq::SliceRandom, Rng, SeedableRng};

use crate::config::{ColorMode, Config, RuleSel, ShiftMode};
use crate::font::Font;
use crate::hypr::HyprEvent;
use crate::life::{Life, Rule, Sym, ROTATION, SYMMETRIES};
use crate::patterns;
use crate::state::Saved;

/// How many recent generation hashes count as "already seen".
/// 1  = literal "grid unchanged since last tick" (still lifes only).
/// 16 = also catches oscillators up to period 16 (blinkers, pulsars, ...),
///      which otherwise keep the grid "changing" forever.
const HISTORY: usize = 16;

/// Run the settled-shape census every this many generations.
const CENSUS_EVERY: u64 = 8;

/// Soup patches and prunes are spread over this many generations.
const DRIP_TICKS: u32 = 8;

/// Share of the settled cells a prune removes (in whole shapes).
const PRUNE_FRACTION: f64 = 0.5;

/// The board must have been calm this long before fast-forward kicks in.
const FF_AFTER: Duration = Duration::from_secs(2);

/// Hyprland events closer together than this are ignored (workspace flicking).
const EVENT_COOLDOWN: Duration = Duration::from_millis(600);

/// Local injections are skipped while more than this share of the board is alive.
const LOCAL_MAX_FILL: f64 = 0.30;

/// Located Hyprland events (a window opening or closing at a known spot) may
/// follow each other this closely; unlocated ones use `EVENT_COOLDOWN`.
const LOCATED_COOLDOWN: Duration = Duration::from_millis(150);

/// Event soup is not queued while this many cells are already waiting.
const MAX_QUEUED: usize = 6000;

/// Beats closer together than this are ignored (a drum roll is not a flood).
const BEAT_COOLDOWN: Duration = Duration::from_millis(250);

/// Beats stop adding gliders while more than this share of the board is alive.
const BEAT_MAX_FILL: f64 = 0.22;

/// A window's halo reaches this many cells beyond its edge.
const HALO: usize = 3;

/// The bloom of a closed window covers at most this much (cells).
const BLOOM_MAX: (usize, usize) = (48, 28);

/// Rules a shift can swap in. All are Life plus extra survival or birth, so a
/// short burst loosens still lifes and oscillators and wakes the board up
/// without wiping it (measured: a few dozen generations roughly double the
/// activity and leave fewer settled cells behind; longer bursts flood the grid).
const SHIFT_RULES: [&str; 3] = ["b34/s23", "b3/s234", "b3/s2345"];

pub const RAIN_LEVELS: u8 = 8;

/// A rectangle of grid cells (a window's footprint on this output).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Spot {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Spot {
    /// A rectangle in logical pixels, relative to the output's corner, as the
    /// cells it touches, clipped to the grid. `None` if nothing is left (the
    /// window is on another output, or entirely off this one).
    pub fn from_px(x: i32, y: i32, w: i32, h: i32, cell_px: usize, cols: usize, rows: usize) -> Option<Spot> {
        if w <= 0 || h <= 0 || cell_px == 0 {
            return None;
        }
        let c = cell_px as i64;
        let cell0 = |v: i32| (v as i64).div_euclid(c).max(0);
        let cell1 = |v: i64| ((v + c - 1).div_euclid(c)).min(cols as i64);
        let (x0, y0) = (cell0(x), cell0(y));
        let x1 = cell1(x as i64 + w as i64);
        let y1 = ((y as i64 + h as i64 + c - 1).div_euclid(c)).min(rows as i64);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Spot { x: x0 as usize, y: y0 as usize, w: (x1 - x0) as usize, h: (y1 - y0) as usize })
    }
}

// ---------------------------------------------------------------- code rain

pub struct Rain {
    level: Vec<u8>,
    glyph: Vec<u8>,
    head: Vec<i32>, // per column, -1 = idle
    cols: usize,
    rows: usize,
}

impl Rain {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            level: vec![0; cols * rows],
            glyph: vec![b'0'; cols * rows],
            head: vec![-1; cols],
            cols,
            rows,
        }
    }

    pub fn level(&self) -> &[u8] {
        &self.level
    }
    pub fn glyph(&self) -> &[u8] {
        &self.glyph
    }

    pub fn tick<R: Rng>(&mut self, rng: &mut R, density: f64) {
        for l in self.level.iter_mut() {
            *l = l.saturating_sub(1);
        }
        let start_p = (density / 400.0).clamp(0.0, 1.0);
        for c in 0..self.cols {
            if self.head[c] < 0 {
                if rng.gen_bool(start_p) {
                    self.head[c] = 0;
                }
                continue;
            }
            let i = self.head[c] as usize * self.cols + c;
            self.level[i] = RAIN_LEVELS;
            self.glyph[i] = hex_digit(rng);
            self.head[c] += 1;
            if self.head[c] as usize >= self.rows {
                self.head[c] = -1;
            }
        }
        // a little flicker in already-lit cells
        for _ in 0..(self.cols / 8).max(1) {
            let i = rng.gen_range(0..self.level.len());
            if self.level[i] > 0 {
                self.glyph[i] = hex_digit(rng);
            }
        }
    }
}

fn hex_digit<R: Rng>(rng: &mut R) -> u8 {
    b"0123456789ABCDEF"[rng.gen_range(0..16)]
}

// ---------------------------------------------------------------------- sim

/// A temporary rule: the board really lives under `restore`.
#[derive(Clone, Copy)]
struct Shift {
    restore: Rule,
    /// Generation number at which the shift ends.
    until_gen: u64,
}

/// A rule ripple: tiles switch to `alt` ring by ring and back again, each for
/// `hold` generations, starting at a random tile. The board's own rule never
/// changes; `Life` applies `alt` to the tiles marked at the moment.
struct Ripple {
    /// Ring (distance in tiles from the origin tile) of every tile.
    ring: Vec<u32>,
    /// Whether each tile currently follows the alternative rule.
    on: Vec<bool>,
    start: u64,
    hold: u64,
    stagger: u64,
    /// Generation at which the last tile switches back.
    end: u64,
}

/// A Gosper gun and where it sits, so it can be taken down again.
struct Gun {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    expire: Instant,
}

pub struct Sim {
    pub life: Life,
    pub rain: Option<Rain>,
    pub gen: u64,
    pub heat: u8, // 0..=2, from CPU load
    rng: StdRng,
    rule_idx: usize,
    next_soft_reset: Instant,
    next_glider: Instant,
    next_special: Instant,
    next_stamp: Instant,
    next_traffic: Instant,
    next_shift: Instant,
    next_local: Instant,
    last_tick: Instant,
    last_event: Option<Instant>,
    history: [u64; HISTORY],
    hist_len: usize,
    hist_pos: usize,
    stagnant_since: Option<Instant>,
    calm_since: Option<Instant>,
    census_acc: u64,
    /// Fast-forwarding right now, and how many generations the last tick ran.
    ff: bool,
    steps: u32,
    noise_acc: f64,
    shift: Option<Shift>,
    ripple: Option<Ripple>,
    next_clock_stamp: Instant,
    clock_min: Option<u32>,
    last_beat: Option<Instant>,
    guns: Vec<Gun>,
    /// Per-tile "quiet since" clocks for local calm detection.
    tile: usize,
    tcols: usize,
    tiles: Vec<Option<Instant>>,
    tact: Vec<u32>,
    /// Cells still to be born (with their colony) / erased, drained a few per generation.
    spawn: Vec<(usize, u8)>,
    spawn_ticks: u32,
    erase: Vec<usize>,
    erase_ticks: u32,
}

/// Take this generation's share of a queue: len / ticks_left, rounded up.
fn drain_share<T>(queue: &mut Vec<T>, ticks_left: &mut u32) -> Vec<T> {
    let n = queue.len().div_ceil((*ticks_left).max(1) as usize);
    *ticks_left = ticks_left.saturating_sub(1);
    queue.split_off(queue.len() - n)
}

fn secs_between<R: Rng>(rng: &mut R, lo: u64, hi: u64) -> Duration {
    Duration::from_secs(rng.gen_range(lo..=hi.max(lo)))
}

fn trail_len(cfg: &Config) -> u8 {
    if cfg.trail {
        cfg.trail_length
    } else {
        0
    }
}

/// How many colonies the config asks for (0 = colour tracking off).
fn colony_count(cfg: &Config) -> usize {
    if cfg.colonies && matches!(cfg.color, ColorMode::Age) {
        cfg.palette.colonies.len()
    } else {
        0
    }
}

impl Sim {
    pub fn new(cols: usize, rows: usize, now: Instant, cfg: &Config) -> Self {
        Self::with_rng(cols, rows, now, cfg, StdRng::from_entropy())
    }

    /// Like `new`, but every random choice follows from `seed`, so a run can
    /// be replayed exactly (the long-run health test relies on this).
    #[cfg(test)]
    pub fn seeded(cols: usize, rows: usize, now: Instant, cfg: &Config, seed: u64) -> Self {
        Self::with_rng(cols, rows, now, cfg, StdRng::seed_from_u64(seed))
    }

    fn with_rng(cols: usize, rows: usize, now: Instant, cfg: &Config, mut rng: StdRng) -> Self {
        let rule = match cfg.rule {
            RuleSel::Fixed(r) => r,
            RuleSel::Rotate => ROTATION[0],
        };
        let mut life = Life::new(cols, rows, trail_len(cfg), rule);
        life.set_colonies(colony_count(cfg), cfg.colony_mutation);
        life.set_decay(cfg.decay_after, cfg.decay_chance);
        life.seed_rng(rng.gen());
        life.seed(&mut rng, (cfg.density * rule.seed_scale()).min(0.9));
        let mut s = Self {
            life,
            rain: None,
            gen: 0,
            heat: 0,
            rng,
            rule_idx: 0,
            next_soft_reset: now,
            next_glider: now,
            next_special: now,
            next_stamp: now,
            next_traffic: now,
            next_shift: now,
            next_local: now,
            last_tick: now,
            last_event: None,
            history: [0; HISTORY],
            hist_len: 0,
            hist_pos: 0,
            stagnant_since: None,
            calm_since: None,
            census_acc: 0,
            ff: false,
            steps: 1,
            noise_acc: 0.0,
            shift: None,
            ripple: None,
            next_clock_stamp: now,
            clock_min: None,
            last_beat: None,
            guns: Vec::new(),
            tile: 16,
            tcols: 1,
            tiles: Vec::new(),
            tact: Vec::new(),
            spawn: Vec::new(),
            spawn_ticks: 0,
            erase: Vec::new(),
            erase_ticks: 0,
        };
        s.rebuild_tiles(cfg);
        s.reschedule(now, cfg);
        s
    }

    fn reschedule(&mut self, now: Instant, cfg: &Config) {
        self.next_soft_reset = now + Duration::from_secs(cfg.soft_reset_secs);
        self.next_glider = now + secs_between(&mut self.rng, cfg.glider_min_secs, cfg.glider_max_secs);
        self.next_special = now + secs_between(&mut self.rng, cfg.special_min_secs, cfg.special_max_secs);
        self.next_stamp = now + secs_between(&mut self.rng, cfg.stamp_min_secs, cfg.stamp_max_secs);
        self.next_traffic = now + secs_between(&mut self.rng, cfg.traffic_min_secs, cfg.traffic_max_secs);
        self.next_shift = now + self.shift_gap(cfg);
        self.next_clock_stamp = now + self.clock_gap(cfg);
        self.next_local = now;
        self.last_tick = now;
        for t in self.tiles.iter_mut() {
            *t = None;
        }
    }

    /// Jittered gap to the next clock stamp (an hour when the stamp is off).
    fn clock_gap(&mut self, cfg: &Config) -> Duration {
        match cfg.clock_stamp_secs {
            0 => Duration::from_secs(3600),
            s => secs_between(&mut self.rng, s * 3 / 4, s * 5 / 4),
        }
    }

    /// Local time of day in minutes since midnight, for the clock stamp.
    pub fn set_clock(&mut self, minutes: Option<u32>) {
        self.clock_min = minutes;
    }

    fn shift_gap(&mut self, cfg: &Config) -> Duration {
        let s = cfg.rule_shift_secs.max(1);
        secs_between(&mut self.rng, s * 3 / 4, s * 5 / 4)
    }

    fn rebuild_tiles(&mut self, cfg: &Config) {
        let (cols, rows) = (self.life.cols(), self.life.rows());
        self.tile = cfg.tile_size.min(cols).min(rows).max(1);
        self.tcols = cols.div_ceil(self.tile);
        let trows = rows.div_ceil(self.tile);
        self.tiles = vec![None; self.tcols * trows];
        self.tact = vec![0; self.tcols * trows];
    }

    /// The wallpaper is visible again after a pause: restart every timer so
    /// gliders, stamps and the calm check don't all fire at once.
    pub fn resume(&mut self, now: Instant, cfg: &Config) {
        self.reschedule(now, cfg);
        self.reset_stagnation();
    }

    /// Config changed (SIGHUP): adopt rule/trail/colonies/timers.
    pub fn reconfigure(&mut self, now: Instant, cfg: &Config) {
        self.end_shift();
        self.life.set_trail_len(trail_len(cfg));
        if self.life.set_colonies(colony_count(cfg), cfg.colony_mutation) {
            self.life.scatter_colonies(&mut self.rng);
        }
        self.life.set_decay(cfg.decay_after, cfg.decay_chance);
        if let RuleSel::Fixed(r) = cfg.rule {
            if r != self.life.rule() {
                self.life.set_rule(r);
                self.reseed(cfg);
            }
        }
        self.rebuild_tiles(cfg);
        self.reschedule(now, cfg);
    }

    /// Resize the grid. Small changes keep the overlapping cells; a change of
    /// more than 10% in either dimension reseeds.
    pub fn resize(&mut self, cols: usize, rows: usize, cfg: &Config) {
        let (oc, or) = (self.life.cols(), self.life.rows());
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (oc, or) {
            return;
        }
        let significant = oc.abs_diff(cols) * 10 > oc || or.abs_diff(rows) * 10 > or;
        self.end_shift(); // a ripple's tiles no longer fit
        self.life.resize(cols, rows);
        self.rain = None; // rebuilt lazily at the new size
        self.guns.clear();
        self.clear_queues();
        self.rebuild_tiles(cfg);
        if significant {
            self.reseed(cfg);
        } else {
            self.reset_stagnation();
        }
    }

    /// Generations the last `tick` ran (more than 1 while fast-forwarding).
    pub fn steps(&self) -> u32 {
        self.steps
    }

    /// The rule the board really lives under, ignoring a running rule shift.
    pub fn base_rule(&self) -> Rule {
        self.shift.map_or(self.life.rule(), |s| s.restore)
    }

    fn end_shift(&mut self) {
        if let Some(s) = self.shift.take() {
            self.life.set_rule(s.restore);
        }
        if self.ripple.take().is_some() {
            self.life.end_alt();
        }
    }

    fn fresh_colony(&mut self) -> u8 {
        match self.life.ncolonies() {
            0 | 1 => 0,
            n => self.rng.gen_range(0..n) as u8,
        }
    }

    fn pick_sym(&mut self, cfg: &Config) -> Sym {
        if cfg.symmetry_chance > 0.0 && self.rng.gen_bool(cfg.symmetry_chance) {
            *SYMMETRIES.choose(&mut self.rng).unwrap_or(&Sym::Mirror)
        } else {
            Sym::None
        }
    }

    /// One generation (or several, while fast-forwarding) plus every timer.
    pub fn tick(&mut self, now: Instant, cfg: &Config, load: f32) {
        self.heat = if !cfg.sysload {
            0
        } else if load > 0.85 {
            2
        } else if load > 0.66 {
            1
        } else {
            0
        };
        let dt = now.saturating_duration_since(self.last_tick).as_secs_f64().min(1.0);
        self.last_tick = now;

        self.update_rule_shift(now, cfg);

        let steps = if cfg.fastforward && self.ff { cfg.fastforward_speed.max(1) } else { 1 };
        self.steps = steps;
        for _ in 0..steps {
            self.advance_ripple(now, cfg);
            self.life.step();
            self.gen += 1;
            self.drip();
        }
        self.census_acc += steps as u64;

        match (cfg.rain, self.rain.is_some()) {
            (true, false) => self.rain = Some(Rain::new(self.life.cols(), self.life.rows())),
            (false, true) => self.rain = None,
            _ => {}
        }
        if let Some(r) = self.rain.as_mut() {
            r.tick(&mut self.rng, cfg.rain_density);
        }

        // 1. Optional timed full reset (soft_reset_secs = 0 means never).
        if cfg.soft_reset_secs > 0 && now >= self.next_soft_reset {
            self.reseed(cfg);
            self.next_soft_reset = now + Duration::from_secs(cfg.soft_reset_secs);
            return;
        }

        // 2. System-driven sparks: the busier the CPU, the more often.
        if cfg.sysload && load > 0.5 && self.rng.gen_bool(((load - 0.5) * 0.6).clamp(0.0, 1.0) as f64) {
            self.life.spark(&mut self.rng);
        }

        // 2b. Cosmic rays: a few random single-cell disturbances per second.
        if cfg.noise_per_sec > 0.0 {
            self.noise_acc = (self.noise_acc + cfg.noise_per_sec * dt).min(20.0);
            while self.noise_acc >= 1.0 {
                self.noise_acc -= 1.0;
                self.life.cosmic_ray(&mut self.rng);
            }
        }

        // 2c. Guns do not live for ever.
        self.expire_guns(now);

        // 3. Gliders (or sparks on rules where gliders don't exist).
        if now >= self.next_glider {
            for _ in 0..self.rng.gen_range(1..=2) {
                if self.life.rule().supports_ships() {
                    let c = self.fresh_colony();
                    self.life.set_brush(c);
                    self.life.place_ship_on_edge(&mut self.rng, true, false);
                } else {
                    self.life.spark(&mut self.rng);
                }
            }
            self.next_glider = now + secs_between(&mut self.rng, cfg.glider_min_secs, cfg.glider_max_secs);
        }

        // 4. Specials: ships, pulsar, Gosper gun, methuselah.
        if now >= self.next_special {
            if cfg.patterns {
                self.inject_special(now, cfg);
            }
            self.next_special = now + secs_between(&mut self.rng, cfg.special_min_secs, cfg.special_max_secs);
        }

        // 4b. Traffic: a convoy of spaceships crossing the board.
        if now >= self.next_traffic {
            if cfg.traffic {
                self.inject_convoy();
            }
            self.next_traffic = now + secs_between(&mut self.rng, cfg.traffic_min_secs, cfg.traffic_max_secs);
        }

        // 5. Stamps: Tux, Arch logo, terminal text.
        if now >= self.next_stamp {
            if cfg.stamps {
                self.inject_stamp();
            }
            self.next_stamp = now + secs_between(&mut self.rng, cfg.stamp_min_secs, cfg.stamp_max_secs);
        }

        // 5b. The clock: a stamp that spells the time.
        if now >= self.next_clock_stamp {
            if cfg.stamps && cfg.clock_stamp_secs > 0 {
                self.inject_clock_stamp();
            }
            self.next_clock_stamp = now + self.clock_gap(cfg);
        }

        // 6. Clutter / calm census. When settled shapes (still lifes,
        //    oscillators) fill too much of the grid, some of them are pruned
        //    away; a merely calm board gets new life added on top of what it
        //    has; tiles that went quiet get their own injection. None of
        //    these wipe the board.
        if self.census_acc >= CENSUS_EVERY {
            self.census_acc = 0;
            let total = (self.life.cols() * self.life.rows()) as f64;
            let (live, settled) = self.life.census();
            if self.erase.is_empty() && cfg.clutter_pct > 0.0 && settled as f64 * 100.0 >= total * cfg.clutter_pct {
                self.prune();
            }
            let active = (live - settled) as f64;
            if cfg.calm_pct > 0.0 && active * 100.0 < total * cfg.calm_pct {
                self.calm_since.get_or_insert(now);
            } else {
                self.calm_since = None;
            }
            self.ff = cfg.fastforward && self.calm_since.is_some_and(|s| now.duration_since(s) >= FF_AFTER);
            self.local_calm(now, cfg);
        }
        if let Some(since) = self.calm_since {
            if now.duration_since(since) >= Duration::from_secs(cfg.stagnation_secs) {
                self.inject_fresh(cfg);
                return;
            }
        }

        // 7. Whole-grid repeat: a state seen within the last HISTORY
        //    generations for `stagnation_secs` straight => inject new life.
        let h = self.life.hash();
        if self.history[..self.hist_len].contains(&h) {
            let since = *self.stagnant_since.get_or_insert(now);
            if now.duration_since(since) >= Duration::from_secs(cfg.stagnation_secs) {
                self.inject_fresh(cfg);
                return;
            }
        } else {
            self.stagnant_since = None;
        }
        self.history[self.hist_pos] = h;
        self.hist_pos = (self.hist_pos + 1) % HISTORY;
        self.hist_len = (self.hist_len + 1).min(HISTORY);
    }

    /// Swap the rule for `rule_shift_gens` generations every `rule_shift_secs`
    /// seconds, then swap back. Only Conway-like boards are shifted: the
    /// replacement rules are Life with extra survival / birth. In `ripple`
    /// mode (the default) the swap travels over the tiles instead of hitting
    /// the whole board at once; a board too small to have several tiles falls
    /// back to the all-at-once shift.
    fn update_rule_shift(&mut self, now: Instant, cfg: &Config) {
        if let Some(s) = self.shift {
            if self.gen >= s.until_gen {
                self.end_shift();
                self.next_shift = now + self.shift_gap(cfg);
            }
            return;
        }
        if self.ripple.is_some() || cfg.rule_shift_secs == 0 || now < self.next_shift {
            return; // (a running ripple ends itself in `advance_ripple`)
        }
        self.next_shift = now + self.shift_gap(cfg);
        let base = self.life.rule();
        if !base.supports_ships() {
            return;
        }
        let alts: Vec<Rule> = SHIFT_RULES.iter().filter_map(|r| Rule::parse(r)).filter(|&r| r != base).collect();
        let Some(&alt) = alts.choose(&mut self.rng) else { return };
        if cfg.rule_shift_mode == ShiftMode::Ripple && self.begin_ripple(alt, cfg) {
            return;
        }
        self.life.set_rule(alt);
        self.shift = Some(Shift { restore: base, until_gen: self.gen + cfg.rule_shift_gens as u64 });
    }

    /// Start a ripple from a random tile. False if the board has a single tile.
    fn begin_ripple(&mut self, alt: Rule, cfg: &Config) -> bool {
        let n = self.tiles.len();
        if n <= 1 || self.tcols == 0 || !self.life.begin_alt(alt) {
            return false;
        }
        let trows = n / self.tcols;
        let (ox, oy) = (self.rng.gen_range(0..self.tcols), self.rng.gen_range(0..trows));
        // distance in tiles, the short way round (the board wraps)
        let wrap = |a: usize, b: usize, len: usize| a.abs_diff(b).min(len - a.abs_diff(b));
        let ring: Vec<u32> = (0..n)
            .map(|t| {
                let (dx, dy) = (wrap(t % self.tcols, ox, self.tcols), wrap(t / self.tcols, oy, trows));
                ((dx * dx + dy * dy) as f64).sqrt().round() as u32
            })
            .collect();
        let (hold, stagger) = (cfg.rule_shift_gens as u64, cfg.rule_shift_stagger as u64);
        let last = ring.iter().copied().max().unwrap_or(0) as u64;
        self.ripple = Some(Ripple { ring, on: vec![false; n], start: self.gen, hold, stagger, end: self.gen + last * stagger + hold });
        true
    }

    /// Before each generation: switch tiles over to / back from the
    /// alternative rule according to how far the ripple has travelled.
    fn advance_ripple(&mut self, now: Instant, cfg: &Config) {
        let Some(mut r) = self.ripple.take() else { return };
        if self.gen >= r.end {
            self.life.end_alt();
            self.next_shift = now + self.shift_gap(cfg);
            return;
        }
        for t in 0..r.ring.len() {
            let from = r.start + r.ring[t] as u64 * r.stagger;
            let want = self.gen >= from && self.gen < from + r.hold;
            if want != r.on[t] {
                r.on[t] = want;
                let (x, y) = ((t % self.tcols) * self.tile, (t / self.tcols) * self.tile);
                self.life.alt_rect(x, y, self.tile, self.tile, want);
            }
        }
        self.ripple = Some(r);
    }

    /// Per-tile calm detection. A tile that has had (almost) nothing moving in
    /// it for `local_calm_secs` gets new life; one tile at a time, and only
    /// while the board is not already crowded.
    fn local_calm(&mut self, now: Instant, cfg: &Config) {
        if cfg.local_calm_secs == 0 || self.tiles.is_empty() {
            return;
        }
        self.life.tile_activity(self.tile, self.tcols, &mut self.tact);
        let allow = ((self.tile * self.tile) / 128).max(1) as u32;
        let calm_for = Duration::from_secs(cfg.local_calm_secs);
        let mut eligible = Vec::new();
        for t in 0..self.tiles.len() {
            if self.tact[t] <= allow {
                let since = *self.tiles[t].get_or_insert(now);
                if now.duration_since(since) >= calm_for {
                    eligible.push(t);
                }
            } else {
                self.tiles[t] = None;
            }
        }
        if eligible.is_empty() || now < self.next_local {
            return;
        }
        let total = (self.life.cols() * self.life.rows()) as f64;
        if self.life.population() as f64 > total * LOCAL_MAX_FILL {
            return;
        }
        let t = eligible[self.rng.gen_range(0..eligible.len())];
        self.inject_in_tile(t, cfg);
        self.tiles[t] = None; // its clock starts over
        self.next_local = now + Duration::from_secs_f64((cfg.local_calm_secs as f64 / 3.0).max(1.0));
    }

    /// Something new in tile `t`: mostly a soup patch (sometimes symmetric), or
    /// a glider flying through it, or a methuselah.
    fn inject_in_tile(&mut self, t: usize, cfg: &Config) {
        let (cols, rows) = (self.life.cols(), self.life.rows());
        let (tx, ty) = (t % self.tcols, t / self.tcols);
        let (x0, y0) = (tx * self.tile, ty * self.tile);
        let (cx, cy) = ((x0 + self.tile / 2).min(cols - 1), (y0 + self.tile / 2).min(rows - 1));
        let ships = self.life.rule().supports_ships();
        let roll = self.rng.gen_range(0..100);
        if ships && roll >= 85 && self.inject_methuselah(cx, cy) {
            return;
        }
        if ships && roll >= 60 {
            let c = self.fresh_colony();
            self.life.set_brush(c);
            if self.life.place_glider_toward(&mut self.rng, cx, cy) {
                return;
            }
        }
        // soup patch: the tile plus a little overlap into its neighbours
        let pad = self.tile / 4;
        let (px, py) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
        let (pw, ph) = (self.tile + 2 * pad, self.tile + 2 * pad);
        let density = (self.rng.gen_range(0.28..0.42) * self.life.rule().seed_scale()).min(0.9);
        let sym = self.pick_sym(cfg);
        let colony = self.fresh_colony();
        self.queue_soup(px, py, pw, ph, density, sym, colony);
    }

    fn queue_soup(&mut self, x: usize, y: usize, w: usize, h: usize, density: f64, sym: Sym, colony: u8) {
        let cells = self.life.soup_cells_sym(&mut self.rng, x, y, w, h, density, sym);
        self.spawn.extend(cells.into_iter().map(|i| (i, colony)));
        self.spawn.shuffle(&mut self.rng);
        self.spawn_ticks = self.spawn_ticks.max(DRIP_TICKS);
    }

    /// An R-pentomino or acorn (random orientation) centred near (cx, cy).
    fn inject_methuselah(&mut self, cx: usize, cy: usize) -> bool {
        if !self.life.rule().supports_ships() {
            return false;
        }
        let base = if self.rng.gen_bool(0.6) { patterns::r_pentomino() } else { patterns::acorn() };
        let pat = base.transformed(self.rng.gen_bool(0.5), self.rng.gen_bool(0.5), self.rng.gen_bool(0.5));
        let (cols, rows) = (self.life.cols(), self.life.rows());
        if pat.w + 4 > cols || pat.h + 4 > rows {
            return false;
        }
        let x = cx.saturating_sub(pat.w / 2).min(cols - pat.w);
        let y = cy.saturating_sub(pat.h / 2).min(rows - pat.h);
        let c = self.fresh_colony();
        self.life.set_brush(c);
        self.life.place(&pat, x, y, 2);
        true
    }

    /// Add new life without clearing anything: a few soup patches in the
    /// emptiest parts of the board (their cells appear over the next few
    /// generations), plus gliders and now and then a methuselah.
    fn inject_fresh(&mut self, cfg: &Config) {
        let (cols, rows) = (self.life.cols(), self.life.rows());
        let rule = self.life.rule();
        let pw = (cols / 3).clamp(4, 32).min(cols);
        let ph = (rows / 3).clamp(4, 20).min(rows);
        let density = (0.35 * rule.seed_scale()).min(0.9);
        for _ in 0..self.rng.gen_range(2..=4) {
            let (x, y) = self.life.emptiest_spot(&mut self.rng, pw, ph, 24);
            let sym = self.pick_sym(cfg);
            let colony = self.fresh_colony();
            self.queue_soup(x, y, pw, ph, density, sym, colony);
        }
        if rule.supports_ships() {
            for _ in 0..self.rng.gen_range(1..=2) {
                let c = self.fresh_colony();
                self.life.set_brush(c);
                self.life.place_ship_on_edge(&mut self.rng, true, false);
            }
            if self.rng.gen_bool(0.4) {
                let (x, y) = self.life.emptiest_spot(&mut self.rng, 8, 8, 24);
                self.inject_methuselah(x + 4, y + 4);
            }
        }
        self.reset_stagnation();
    }

    /// Birth / erase this generation's share of the queued cells.
    fn drip(&mut self) {
        if !self.spawn.is_empty() {
            for (i, c) in drain_share(&mut self.spawn, &mut self.spawn_ticks) {
                self.life.set_brush(c);
                self.life.set_alive(i);
            }
        }
        if !self.erase.is_empty() {
            for i in drain_share(&mut self.erase, &mut self.erase_ticks) {
                self.life.kill(i);
            }
        }
    }

    /// Queue whole settled shapes for removal (about `PRUNE_FRACTION` of all
    /// settled cells), erased over the next few generations.
    fn prune(&mut self) {
        let mut clusters = self.life.settled_clusters();
        clusters.shuffle(&mut self.rng);
        let total: usize = clusters.iter().map(Vec::len).sum();
        let goal = ((total as f64 * PRUNE_FRACTION).ceil() as usize).max(1);
        let mut cells = Vec::new();
        for c in clusters {
            if cells.len() >= goal {
                break;
            }
            cells.extend(c);
        }
        self.erase = cells;
        self.erase_ticks = DRIP_TICKS;
    }

    fn clear_queues(&mut self) {
        self.spawn.clear();
        self.erase.clear();
    }

    fn expire_guns(&mut self, now: Instant) {
        let mut i = 0;
        while i < self.guns.len() {
            if now >= self.guns[i].expire {
                let g = self.guns.swap_remove(i);
                let cells = self.life.live_in_rect(g.x, g.y, g.w, g.h);
                self.erase.extend(cells);
                self.erase_ticks = self.erase_ticks.max(DRIP_TICKS);
            } else {
                i += 1;
            }
        }
    }

    /// Adopt a saved board. False (nothing changed) if the board is empty or
    /// was saved under a rule the config doesn't allow.
    pub fn restore(&mut self, saved: &Saved, cfg: &Config) -> bool {
        let Some(rule) = crate::life::Rule::parse(&saved.rule) else { return false };
        if saved.cells.iter().all(|&c| c == 0) {
            return false;
        }
        match cfg.rule {
            RuleSel::Fixed(r) if r != rule => return false,
            RuleSel::Rotate => match ROTATION.iter().position(|&r| r == rule) {
                Some(i) => self.rule_idx = i,
                None => return false,
            },
            _ => {}
        }
        self.end_shift();
        self.life.set_rule(rule);
        self.life.load(saved);
        if self.life.colonies_on() && saved.colony.is_empty() {
            self.life.scatter_colonies(&mut self.rng); // older save without colours
        }
        self.guns.clear();
        self.clear_queues();
        self.reset_stagnation();
        true
    }

    fn inject_special(&mut self, now: Instant, cfg: &Config) {
        if !self.life.rule().supports_ships() {
            for _ in 0..3 {
                self.life.spark(&mut self.rng);
            }
            return;
        }
        let c = self.fresh_colony();
        self.life.set_brush(c);
        match self.rng.gen_range(0..4) {
            0 => {
                let ship = match self.rng.gen_range(0..3) {
                    0 => patterns::lwss(),
                    1 => patterns::mwss(),
                    _ => patterns::hwss(),
                };
                self.life.place_east_ship_on_edge(&mut self.rng, &ship);
            }
            1 => {
                let p = patterns::pulsar();
                self.life.place_random(&mut self.rng, &p, 2, 1);
            }
            2 => {
                if let Some((x, y, w, h)) = self.life.place_gun(&mut self.rng) {
                    if cfg.gun_secs > 0 {
                        let expire = now + Duration::from_secs(cfg.gun_secs);
                        self.guns.push(Gun { x, y, w, h, expire });
                    }
                }
            }
            _ => {
                let (cols, rows) = (self.life.cols(), self.life.rows());
                let (cx, cy) = (self.rng.gen_range(0..cols), self.rng.gen_range(0..rows));
                self.inject_methuselah(cx, cy);
            }
        }
    }

    /// A convoy of two to four identical spaceships entering from one edge.
    fn inject_convoy(&mut self) {
        if !self.life.rule().supports_ships() {
            return;
        }
        let ship = match self.rng.gen_range(0..3) {
            0 => patterns::lwss(),
            1 => patterns::mwss(),
            _ => patterns::hwss(),
        };
        let n = self.rng.gen_range(2..=4);
        let c = self.fresh_colony();
        self.life.set_brush(c);
        self.life.place_convoy(&mut self.rng, &ship, n);
    }

    fn inject_stamp(&mut self) {
        let font = Font::build();
        let n = 2 + patterns::STAMP_TEXTS.len();
        let pat = match self.rng.gen_range(0..n) {
            0 => patterns::tux(),
            1 => patterns::arch(),
            i => patterns::text(&font, patterns::STAMP_TEXTS[i - 2]),
        };
        let c = self.fresh_colony();
        self.life.set_brush(c);
        // Clear a 2-cell margin so the stamp is legible before the rules eat it.
        self.life.place_random(&mut self.rng, &pat, 3, 2);
    }

    /// A stamp that spells the local time as HH:MM (24 hour). False if the
    /// time is unknown or the board is too small for it.
    fn inject_clock_stamp(&mut self) -> bool {
        let Some(m) = self.clock_min else { return false };
        let pat = patterns::text(&Font::build(), &format!("{:02}:{:02}", m / 60 % 24, m % 60));
        let c = self.fresh_colony();
        self.life.set_brush(c);
        self.life.place_random(&mut self.rng, &pat, 3, 2)
    }

    /// A beat from the audio meter: a glider (two on a hard beat) from an
    /// edge. Rate limited, and held back while the board is crowded.
    pub fn on_beat(&mut self, strength: f32, now: Instant) {
        if self.last_beat.is_some_and(|t| now.duration_since(t) < BEAT_COOLDOWN) {
            return;
        }
        let total = (self.life.cols() * self.life.rows()) as f64;
        if self.life.population() as f64 > total * BEAT_MAX_FILL {
            return;
        }
        self.last_beat = Some(now);
        for _ in 0..if strength > 0.6 { 2 } else { 1 } {
            let c = self.fresh_colony();
            self.life.set_brush(c);
            if self.life.rule().supports_ships() {
                self.life.place_ship_on_edge(&mut self.rng, true, false);
            } else {
                self.life.spark(&mut self.rng);
            }
        }
    }

    /// React to a Hyprland event: a workspace switch sends gliders across, a
    /// new window sparks a soup patch, a closed window leaves a symmetric bloom.
    pub fn on_event(&mut self, ev: HyprEvent, now: Instant, cfg: &Config) {
        if self.last_event.is_some_and(|t| now.duration_since(t) < EVENT_COOLDOWN) || self.spawn.len() > MAX_QUEUED {
            return;
        }
        self.last_event = Some(now);
        let (cols, rows) = (self.life.cols(), self.life.rows());
        match ev {
            HyprEvent::Workspace => {
                for _ in 0..self.rng.gen_range(2..=4) {
                    let c = self.fresh_colony();
                    self.life.set_brush(c);
                    if self.life.rule().supports_ships() {
                        self.life.place_ship_on_edge(&mut self.rng, true, false);
                    } else {
                        self.life.spark(&mut self.rng);
                    }
                }
            }
            HyprEvent::OpenWindow | HyprEvent::CloseWindow => {
                let (pw, ph) = ((cols / 6).clamp(4, 24).min(cols), (rows / 6).clamp(4, 16).min(rows));
                let (x, y) = self.life.emptiest_spot(&mut self.rng, pw, ph, 24);
                let (sym, density) = if ev == HyprEvent::CloseWindow {
                    (if self.rng.gen_bool(0.5) { Sym::Both } else { Sym::Rot }, 0.38)
                } else {
                    (self.pick_sym(cfg), 0.34)
                };
                let colony = self.fresh_colony();
                let density = (density * self.life.rule().seed_scale()).min(0.9);
                self.queue_soup(x, y, pw, ph, density, sym, colony);
            }
        }
    }

    /// A window event at a known spot. A new window gets a halo of soup just
    /// outside its edge (the window itself covers the wallpaper, so only the
    /// surroundings can be seen); a closed window blooms in the space it
    /// leaves, symmetric, up to `BLOOM_MAX` cells across.
    pub fn on_event_at(&mut self, ev: HyprEvent, spot: Spot, now: Instant, cfg: &Config) {
        if self.last_event.is_some_and(|t| now.duration_since(t) < LOCATED_COOLDOWN) || self.spawn.len() > MAX_QUEUED {
            return;
        }
        let (cols, rows) = (self.life.cols(), self.life.rows());
        let Spot { x, y, w, h } = spot;
        if w == 0 || h == 0 || x >= cols || y >= rows {
            return;
        }
        self.last_event = Some(now);
        let scale = self.life.rule().seed_scale();
        match ev {
            // not a window event; nothing to place
            HyprEvent::Workspace => {}
            HyprEvent::OpenWindow => {
                let (x0, y0) = (x.saturating_sub(HALO), y.saturating_sub(HALO));
                let (x1, y1) = ((x + w + HALO).min(cols), (y + h + HALO).min(rows));
                let sym = self.pick_sym(cfg);
                let colony = self.fresh_colony();
                let cells = self.life.soup_cells_sym(&mut self.rng, x0, y0, x1 - x0, y1 - y0, (0.45 * scale).min(0.9), sym);
                let outside = |i: usize| {
                    let (cx, cy) = (i % cols, i / cols);
                    !(cx >= x && cx < x + w && cy >= y && cy < y + h)
                };
                self.spawn.extend(cells.into_iter().filter(|&i| outside(i)).map(|i| (i, colony)));
                self.spawn.shuffle(&mut self.rng);
                self.spawn_ticks = self.spawn_ticks.max(DRIP_TICKS);
            }
            HyprEvent::CloseWindow => {
                // centred in the window, at most BLOOM_MAX, at least a small patch
                let (bw, bh) = (w.clamp(8.min(cols), BLOOM_MAX.0.min(cols)), h.clamp(8.min(rows), BLOOM_MAX.1.min(rows)));
                let bx = (x + w / 2).saturating_sub(bw / 2).min(cols - bw);
                let by = (y + h / 2).saturating_sub(bh / 2).min(rows - bh);
                let sym = if self.rng.gen_bool(0.5) { Sym::Both } else { Sym::Rot };
                let colony = self.fresh_colony();
                self.queue_soup(bx, by, bw, bh, (0.38 * scale).min(0.9), sym, colony);
            }
        }
    }

    /// Clear + random soup at the reseed density; rotate rules if asked to.
    pub fn reseed(&mut self, cfg: &Config) {
        self.end_shift();
        let rule = match cfg.rule {
            RuleSel::Fixed(r) => r,
            RuleSel::Rotate => {
                self.rule_idx = (self.rule_idx + 1) % ROTATION.len();
                ROTATION[self.rule_idx]
            }
        };
        self.life.set_rule(rule);
        self.life.seed(&mut self.rng, (cfg.reseed_density * rule.seed_scale()).min(0.9));
        self.gen = 0;
        self.guns.clear();
        self.clear_queues();
        self.reset_stagnation();
    }

    fn reset_stagnation(&mut self) {
        self.stagnant_since = None;
        self.calm_since = None;
        self.ff = false;
        self.hist_len = 0;
        self.hist_pos = 0;
    }

    #[cfg(test)]
    pub fn rule(&self) -> crate::life::Rule {
        self.life.rule()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::Pattern;

    /// Everything that adds, removes or speeds up life on its own is off, so
    /// a test can look at exactly one mechanism.
    fn quiet_cfg() -> Config {
        Config {
            patterns: false,
            stamps: false,
            glider_min_secs: 100_000,
            glider_max_secs: 100_000,
            local_calm_secs: 0,
            noise_per_sec: 0.0,
            rule_shift_secs: 0,
            decay_after: 0,
            traffic: false,
            fastforward: false,
            colonies: false,
            ..Config::default()
        }
    }

    fn run(s: &mut Sim, cfg: &Config, t0: Instant, ticks: usize) -> Instant {
        let mut now = t0;
        for _ in 0..ticks {
            now += Duration::from_millis(250);
            s.tick(now, cfg, 0.0);
        }
        now
    }

    #[test]
    fn empty_grid_gets_new_life_after_stagnation_limit() {
        let cfg = quiet_cfg();
        let t0 = Instant::now();
        let mut s = Sim::new(40, 30, t0, &cfg);
        s.life.clear();
        let mut now = t0;
        let mut revived = false;
        for _ in 0..(70 * 4) {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
            // sparse soup cells die as fast as the drip adds them, so "more than a
            // handful" is the reliable bar (the old `> 20` failed ~7% of runs)
            if s.life.population() > 5 {
                revived = true;
                break;
            }
        }
        assert!(revived);
    }

    #[test]
    fn calm_board_gets_new_life_dripped_in_without_a_reset() {
        let cfg = quiet_cfg();
        let t0 = Instant::now();
        let mut s = Sim::new(40, 30, t0, &cfg);
        s.life.clear();
        s.life.place(&Pattern::from_art(&["##", "##"]), 5, 5, 0);
        let mut now = t0;
        let mut ticks = 0u64;
        let mut seen = Vec::new();
        loop {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
            ticks += 1;
            if ticks > 240 {
                seen.push(s.life.population());
            }
            if s.life.population() > 20 || ticks > 400 {
                break;
            }
        }
        assert!(s.life.population() > 20, "never injected");
        assert_eq!(s.gen, ticks, "board was reset instead of added to");
        // the patch arrives over several generations, not in one pop
        assert!(seen.len() >= 2 && seen[0] < 20, "population jumped: {seen:?}");
    }

    #[test]
    fn no_timed_reset_by_default() {
        let cfg = Config { calm_pct: 0.0, clutter_pct: 0.0, stagnation_secs: 86_400, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(40, 30, t0, &cfg);
        let mut now = t0;
        for _ in 0..(3600 * 4) {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
        }
        assert_eq!(s.gen, 3600 * 4, "gen counter only resets on a full reseed");
    }

    fn block_lattice(s: &mut Sim) {
        let block = Pattern::from_art(&["##", "##"]);
        for y in (0..30).step_by(5) {
            for x in (0..40).step_by(5) {
                s.life.place(&block, x, y, 0);
            }
        }
    }

    #[test]
    fn clutter_prunes_settled_shapes_without_resetting() {
        let cfg = Config { clutter_pct: 5.0, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(40, 30, t0, &cfg);
        s.life.clear();
        block_lattice(&mut s); // 48 blocks = 16% of the grid
        let mut now = t0;
        for _ in 0..200 {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
        }
        assert_eq!(s.gen, 200, "board was reset");
        let (_, settled) = s.life.census();
        assert!(settled > 0, "everything was wiped");
        assert!((settled as f64) * 100.0 / 1200.0 < 5.0, "still cluttered: {settled}");
        assert_eq!(settled % 4, 0, "shapes should be removed whole");
    }

    #[test]
    fn pruned_cells_leave_death_trails() {
        let cfg = Config { clutter_pct: 5.0, trail: true, trail_length: 4, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(40, 30, t0, &cfg);
        s.life.clear();
        block_lattice(&mut s);
        let mut now = t0;
        let mut trailing = false;
        for _ in 0..120 {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
            trailing |= s.life.trail().iter().any(|&t| t > 0);
        }
        assert!(trailing);
    }

    #[test]
    fn sparse_settled_shapes_are_left_alone() {
        let cfg = Config { clutter_pct: 5.0, calm_pct: 0.0, stagnation_secs: 86_400, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(40, 30, t0, &cfg);
        s.life.clear();
        let block = Pattern::from_art(&["##", "##"]);
        for (x, y) in [(3, 3), (20, 10), (30, 22)] {
            s.life.place(&block, x, y, 0);
        }
        let mut now = t0;
        for _ in 0..300 {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
        }
        assert_eq!(s.gen, 300);
        assert_eq!(s.life.population(), 12);
    }

    #[test]
    fn restore_brings_back_a_saved_board_if_the_rule_matches() {
        let cfg = Config::default();
        let mut a = Sim::new(30, 20, Instant::now(), &cfg);
        a.life.clear();
        a.life.place(&Pattern::from_art(&["###"]), 4, 4, 0);
        let saved = Saved { cols: 30, rows: 20, rule: a.life.rule().name(), cells: a.life.cells().to_vec(), age: a.life.age().to_vec(), colony: a.life.colony().to_vec() };

        let mut b = Sim::new(30, 20, Instant::now(), &cfg);
        assert!(b.restore(&saved, &cfg));
        assert_eq!(b.life.cells(), a.life.cells());

        let other = Config { rule: RuleSel::Fixed(crate::life::SEEDS), ..Config::default() };
        let mut c = Sim::new(30, 20, Instant::now(), &other);
        assert!(!c.restore(&saved, &other)); // saved under life, config wants seeds

        let empty = Saved { cells: vec![0; 600], ..saved };
        assert!(!b.restore(&empty, &cfg));
    }

    #[test]
    fn rotate_cycles_rules_on_reseed() {
        let cfg = Config { rule: RuleSel::Rotate, ..Config::default() };
        let mut s = Sim::new(30, 30, Instant::now(), &cfg);
        let mut seen = vec![s.rule()];
        for _ in 0..4 {
            s.reseed(&cfg);
            seen.push(s.rule());
        }
        seen.dedup();
        assert_eq!(seen.len(), 5);
    }

    #[test]
    fn rain_stays_in_bounds() {
        let mut r = Rain::new(20, 10);
        let mut rng = rand::thread_rng();
        for _ in 0..500 {
            r.tick(&mut rng, 10.0);
        }
        assert!(r.level().iter().any(|&l| l > 0));
    }

    #[test]
    fn stamps_and_specials_never_panic_on_small_grids() {
        let cfg = Config::default();
        for (c, r) in [(3, 3), (12, 12), (40, 20), (100, 60)] {
            let mut s = Sim::new(c, r, Instant::now(), &cfg);
            for _ in 0..50 {
                s.inject_special(Instant::now(), &cfg);
                s.inject_stamp();
                s.inject_fresh(&cfg);
                s.inject_convoy();
                s.inject_methuselah(c / 2, r / 2);
                s.on_event(HyprEvent::OpenWindow, Instant::now(), &cfg);
            }
        }
    }

    // ---------------------------------------------------------------- new behaviour

    fn bbox(life: &Life) -> Option<(usize, usize)> {
        let (cols, rows) = (life.cols(), life.rows());
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for i in 0..cols * rows {
            if life.cells()[i] != 0 {
                let (x, y) = (i % cols, i / cols);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        (x0 != usize::MAX).then_some((x1 - x0 + 1, y1 - y0 + 1))
    }

    #[test]
    fn a_quiet_tile_gets_a_local_patch_without_a_reset() {
        let cfg = Config { local_calm_secs: 4, calm_pct: 0.0, stagnation_secs: 86_400, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(96, 64, t0, &cfg);
        s.life.clear();
        let mut now = t0;
        let mut seen = None;
        for tick in 1..=400u64 {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
            assert_eq!(s.gen, tick, "board must not be reset");
            if s.life.population() > 0 {
                seen = bbox(&s.life);
                break;
            }
        }
        let (w, h) = seen.expect("an empty board must get local life within 100 s");
        // a soup patch is one tile plus padding (24 cells); gliders and methuselahs are smaller
        assert!(w <= 24 && h <= 24, "first injection is local, got {w}x{h}");
        // ... and it was not injected before the tile had been quiet for local_calm_secs
        assert!(s.gen >= 16, "too early: gen {}", s.gen);
    }

    #[test]
    fn a_busy_tile_is_left_alone_while_quiet_ones_get_help() {
        let cfg = Config { local_calm_secs: 3, calm_pct: 0.0, stagnation_secs: 86_400, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(64, 64, t0, &cfg);
        s.life.clear();
        // a Gosper gun in tile (0,0) keeps that tile busy with fresh gliders for a long time
        s.life.place(&patterns::gosper_gun(), 2, 2, 0);
        let now = run(&mut s, &cfg, t0, 16);
        s.life.tile_activity(s.tile, s.tcols, &mut s.tact);
        assert!(s.tact[0] > 2, "the gun tile is active: {}", s.tact[0]);
        // quiet tiles far from the gun are on their own clocks and eventually get life
        let before = s.life.count_in(0, 40, 64, 24);
        run(&mut s, &cfg, now, 200);
        assert!(s.life.count_in(0, 40, 64, 24) > before || s.life.population() > 100, "quiet tiles never got life");
    }

    #[test]
    fn cosmic_rays_keep_disturbing_a_frozen_board() {
        let cfg = Config { noise_per_sec: 40.0, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(40, 30, t0, &cfg);
        s.life.clear();
        block_lattice(&mut s);
        let pop0 = s.life.population();
        run(&mut s, &cfg, t0, 40);
        assert_ne!(s.life.population(), pop0, "40 rays per second must change a frozen lattice");
    }

    #[test]
    fn rule_shift_swaps_for_a_while_and_always_returns() {
        let cfg = Config { rule_shift_secs: 2, rule_shift_gens: 5, rule_shift_mode: ShiftMode::Global, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(40, 30, t0, &cfg);
        let base = s.rule();
        let (mut shifted, mut back_after) = (false, false);
        let mut now = t0;
        for _ in 0..200 {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
            assert_eq!(s.base_rule(), base, "the board's real rule never changes");
            if s.rule() != base {
                shifted = true;
            } else if shifted {
                back_after = true;
            }
        }
        assert!(shifted, "no shift in 50 s");
        assert!(back_after, "never swapped back");
        assert_eq!(s.gen, 200, "a shift is not a reset");
    }

    #[test]
    fn rule_shift_is_not_used_on_rules_without_ships() {
        let cfg = Config { rule_shift_secs: 1, rule: RuleSel::Fixed(crate::life::SEEDS), ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(40, 30, t0, &cfg);
        let mut now = t0;
        for _ in 0..80 {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
            assert_eq!(s.rule(), crate::life::SEEDS);
        }
    }

    #[test]
    fn calm_board_fast_forwards_and_slows_down_when_life_returns() {
        let cfg = Config { fastforward: true, fastforward_speed: 4, calm_pct: 0.3, stagnation_secs: 86_400, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(60, 40, t0, &cfg);
        s.life.clear();
        s.life.place(&Pattern::from_art(&["##", "##"]), 5, 5, 0);
        let mut now = run(&mut s, &cfg, t0, 80);
        assert_eq!(s.steps(), 4, "a calm board should be fast-forwarding");
        let g = s.gen;
        now += Duration::from_millis(250);
        s.tick(now, &cfg, 0.0);
        assert_eq!(s.gen, g + 4, "four generations in one tick");
        // lots of new activity: back to normal speed within a couple of censuses
        let mut rng = rand::thread_rng();
        s.life.seed(&mut rng, 0.3);
        run(&mut s, &cfg, now, 12);
        assert_eq!(s.steps(), 1, "busy board must not fast-forward");
    }

    #[test]
    fn fast_forward_off_means_one_generation_per_tick() {
        let cfg = Config { fastforward: false, stagnation_secs: 86_400, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(60, 40, t0, &cfg);
        s.life.clear();
        run(&mut s, &cfg, t0, 80);
        assert_eq!((s.steps(), s.gen), (1, 80));
    }

    #[test]
    fn guns_are_taken_down_after_gun_secs() {
        let cfg = Config { gun_secs: 2, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(120, 80, t0, &cfg);
        s.life.clear();
        let mut tries = 0;
        while s.guns.is_empty() {
            s.inject_special(t0, &cfg);
            tries += 1;
            assert!(tries < 300, "never rolled a gun");
        }
        assert_eq!(s.guns.len(), 1);
        let n_before = s.life.population();
        let mut now = t0;
        let mut queued = false;
        for _ in 0..40 {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
            queued |= !s.erase.is_empty();
        }
        assert!(queued, "expiry must queue the gun's cells for erasure");
        assert!(s.guns.is_empty());
        assert!(n_before > 0);
    }

    #[test]
    fn permanent_guns_when_gun_secs_is_zero() {
        let cfg = Config { gun_secs: 0, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(120, 80, t0, &cfg);
        for _ in 0..300 {
            s.inject_special(t0, &cfg);
        }
        assert!(s.guns.is_empty());
    }

    #[test]
    fn hypr_events_add_life_and_respect_the_cooldown() {
        let cfg = quiet_cfg();
        let t0 = Instant::now();
        let mut s = Sim::new(120, 80, t0, &cfg);
        s.life.clear();
        s.on_event(HyprEvent::Workspace, t0, &cfg);
        assert!(s.life.population() >= 5, "at least one glider: {}", s.life.population());
        let pop = s.life.population();
        s.on_event(HyprEvent::Workspace, t0 + Duration::from_millis(100), &cfg);
        assert_eq!(s.life.population(), pop, "second event inside the cooldown is ignored");
        s.on_event(HyprEvent::OpenWindow, t0 + Duration::from_secs(1), &cfg);
        assert!(!s.spawn.is_empty(), "a new window queues a soup patch");
        s.spawn.clear();
        s.on_event(HyprEvent::CloseWindow, t0 + Duration::from_secs(3), &cfg);
        assert!(!s.spawn.is_empty(), "a closed window blooms too");
    }

    #[test]
    fn convoys_and_methuselahs_need_a_ship_rule() {
        let cfg = Config { rule: RuleSel::Fixed(crate::life::SEEDS), ..quiet_cfg() };
        let mut s = Sim::new(120, 80, Instant::now(), &cfg);
        s.life.clear();
        s.inject_convoy();
        assert_eq!(s.life.population(), 0);
        assert!(!s.inject_methuselah(10, 10));
        let cfg = quiet_cfg();
        let mut s = Sim::new(120, 80, Instant::now(), &cfg);
        s.life.clear();
        s.inject_convoy();
        assert!(s.life.population() >= 18, "at least two ships, got {}", s.life.population());
    }

    #[test]
    fn colonies_spread_and_mix_on_a_running_board() {
        let cfg = Config { colonies: true, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(120, 80, t0, &cfg);
        assert!(s.life.colonies_on());
        run(&mut s, &cfg, t0, 120);
        let mut seen = [0usize; 8];
        for (i, &c) in s.life.cells().iter().enumerate() {
            if c == 1 {
                seen[s.life.colony()[i] as usize] += 1;
            }
        }
        assert!(seen.iter().filter(|&&n| n > 20).count() >= 2, "several colonies alive: {seen:?}");
        assert!(seen[s.life.ncolonies()..].iter().all(|&n| n == 0), "no colony id out of range");
    }

    #[test]
    fn fixed_colour_turns_colonies_off() {
        let cfg = Config { colonies: true, color: ColorMode::Fixed([1, 2, 3]), ..quiet_cfg() };
        let s = Sim::new(30, 30, Instant::now(), &cfg);
        assert!(!s.life.colonies_on());
    }

    #[test]
    fn decay_clears_a_frozen_board_but_not_a_busy_one() {
        let cfg = Config { decay_after: 20, decay_chance: 0.2, clutter_pct: 0.0, calm_pct: 0.0, stagnation_secs: 86_400, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(40, 30, t0, &cfg);
        s.life.clear();
        block_lattice(&mut s);
        run(&mut s, &cfg, t0, 400);
        // 48 blocks = 192 cells. Half-decayed blocks can regrow into new shapes, so
        // the bar is "well under half", not "zero".
        assert!(s.life.population() < 96, "frozen blocks must decay, {} cells left", s.life.population());
        assert_eq!(s.gen, 400);
    }

    #[test]
    fn rotate_rule_never_gets_stuck_in_a_shift_after_reseed() {
        let cfg = Config { rule: RuleSel::Rotate, rule_shift_secs: 1, rule_shift_gens: 50, rule_shift_mode: ShiftMode::Global, ..quiet_cfg() };
        let t0 = Instant::now();
        let mut s = Sim::new(40, 30, t0, &cfg);
        let mut now = t0;
        for i in 0..160 {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
            if i % 40 == 39 {
                s.reseed(&cfg);
                assert_eq!(s.life.rule(), s.base_rule(), "a reseed ends any running shift");
            }
        }
    }

    // ------------------------------------------------------------ ripple

    fn ripple_cfg() -> Config {
        Config { rule_shift_secs: 2, rule_shift_gens: 5, rule_shift_stagger: 3, tile_size: 8, ..quiet_cfg() }
    }

    /// 20 x 12 tiles of 8 x 8 cells.
    const RIPPLE_GRID: (usize, usize) = (160, 96);

    #[test]
    fn a_ripple_travels_over_the_tiles_and_never_flips_the_whole_board() {
        let cfg = ripple_cfg();
        assert_eq!(cfg.rule_shift_mode, ShiftMode::Ripple);
        let t0 = Instant::now();
        let mut s = Sim::new(RIPPLE_GRID.0, RIPPLE_GRID.1, t0, &cfg);
        let base = s.rule();
        let total = RIPPLE_GRID.0 * RIPPLE_GRID.1;
        let (mut peak, mut ripples, mut was_on) = (0usize, 0, false);
        let mut first_sizes = Vec::new();
        let mut now = t0;
        for _ in 0..240 {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
            assert_eq!(s.rule(), base, "a ripple never changes the board's own rule");
            let on = s.life.alt_cells();
            peak = peak.max(on);
            if on > 0 && !was_on {
                ripples += 1;
            }
            if ripples == 1 && on > 0 {
                first_sizes.push(on);
            }
            was_on = on > 0;
        }
        assert!(ripples >= 3, "ripples keep coming, got {ripples}");
        assert!(peak > 64, "more than one tile is on at once ({peak} cells)");
        assert!(peak < total / 2, "but never close to the whole board: {peak} of {total}");
        assert!(peak > total / 20, "and a real band of tiles, not a speck: {peak} of {total}");
        // the first ripple starts at a single tile (ring 0) and then grows
        assert_eq!(first_sizes[0], 64, "starts at one 8x8 tile, got {}", first_sizes[0]);
        assert!(first_sizes.iter().any(|&n| n > 64), "and spreads: {first_sizes:?}");
        assert_eq!(s.gen, 240, "a ripple is not a reset");
    }

    #[test]
    fn a_ripple_ends_with_every_tile_back_on_the_normal_rule() {
        let cfg = ripple_cfg();
        let t0 = Instant::now();
        let mut s = Sim::new(RIPPLE_GRID.0, RIPPLE_GRID.1, t0, &cfg);
        let mut now = t0;
        let mut seen_on = false;
        for _ in 0..400 {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
            seen_on |= s.life.alt_cells() > 0;
            if seen_on && s.ripple.is_none() {
                assert_eq!(s.life.alt_cells(), 0, "the ripple is over: no tile may stay switched");
                return;
            }
        }
        panic!("no ripple finished");
    }

    #[test]
    fn a_marked_tile_really_follows_the_other_rule() {
        // a block is still under Life; under B3/S (no survival) it dies. Two blocks,
        // only the left one in a marked tile.
        let mut l = Life::new(32, 16, 0, crate::life::CONWAY);
        let block = Pattern::from_art(&["##", "##"]);
        l.place(&block, 2, 2, 0);
        l.place(&block, 20, 2, 0);
        assert!(l.begin_alt(Rule::parse("b3/s").unwrap()));
        l.alt_rect(0, 0, 16, 16, true);
        l.step();
        assert_eq!(l.count_in(0, 0, 16, 16), 0, "marked block dies");
        assert_eq!(l.count_in(16, 0, 16, 16), 4, "unmarked block lives on");
        l.end_alt();
        assert_eq!(l.alt_cells(), 0);
    }

    #[test]
    fn an_all_clear_mask_changes_nothing_and_brain_refuses_a_ripple() {
        let mut a = Life::new(40, 30, 0, crate::life::CONWAY);
        let mut b = Life::new(40, 30, 0, crate::life::CONWAY);
        let mut rng = rand::rngs::StdRng::seed_from_u64(5);
        a.seed(&mut rng, 0.3);
        b.seed(&mut rng.clone(), 0.3);
        a.seed_rng(9);
        b.seed_rng(9);
        // same board in both
        let cells = a.cells().to_vec();
        b.load(&Saved { cols: 40, rows: 30, rule: "life".into(), cells: cells.clone(), age: vec![0; 1200], colony: Vec::new() });
        assert!(b.begin_alt(Rule::parse("b36/s23").unwrap())); // mask stays all zero
        for _ in 0..30 {
            a.step();
            b.step();
        }
        assert_eq!(a.cells(), b.cells());
        let mut brain = Life::new(10, 10, 0, Rule::Brain);
        assert!(!brain.begin_alt(crate::life::CONWAY));
        assert!(!a.begin_alt(Rule::Brain));
    }

    #[test]
    fn reseed_and_resize_drop_a_running_ripple() {
        let cfg = ripple_cfg();
        let t0 = Instant::now();
        let mut s = Sim::new(RIPPLE_GRID.0, RIPPLE_GRID.1, t0, &cfg);
        let mut now = t0;
        while s.ripple.is_none() {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
            assert!(now.duration_since(t0) < Duration::from_secs(60));
        }
        s.reseed(&cfg);
        assert!(s.ripple.is_none());
        assert_eq!(s.life.alt_cells(), 0);
        while s.ripple.is_none() {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
        }
        s.resize(200, 120, &cfg);
        assert!(s.ripple.is_none());
        assert_eq!(s.life.alt_cells(), 0);
        run(&mut s, &cfg, now, 20); // and it keeps running fine
    }

    #[test]
    fn a_board_with_one_tile_falls_back_to_the_whole_board_shift() {
        let cfg = Config { rule_shift_secs: 1, rule_shift_gens: 5, ..quiet_cfg() }; // tile_size 16 > 12
        let t0 = Instant::now();
        let mut s = Sim::new(12, 12, t0, &cfg);
        let base = s.rule();
        let mut now = t0;
        let mut shifted = false;
        for _ in 0..80 {
            now += Duration::from_millis(250);
            s.tick(now, &cfg, 0.0);
            shifted |= s.rule() != base;
            assert!(s.ripple.is_none());
        }
        assert!(shifted, "a one-tile board uses the global shift");
    }

    // ----------------------------------------------------- window spots

    #[test]
    fn spots_are_cells_clipped_to_the_grid() {
        // 8 px cells on a 100 x 50 grid
        let sp = |x, y, w, h| Spot::from_px(x, y, w, h, 8, 100, 50);
        assert_eq!(sp(16, 24, 80, 40), Some(Spot { x: 2, y: 3, w: 10, h: 5 }));
        assert_eq!(sp(17, 25, 80, 40), Some(Spot { x: 2, y: 3, w: 11, h: 6 }), "partly covered cells count");
        assert_eq!(sp(-40, -16, 100, 100), Some(Spot { x: 0, y: 0, w: 8, h: 11 }), "clipped at the top-left");
        assert_eq!(sp(700, 380, 400, 400), Some(Spot { x: 87, y: 47, w: 13, h: 3 }), "clipped at the bottom-right");
        assert_eq!(sp(900, 0, 50, 50), None, "entirely to the right of this output");
        assert_eq!(sp(-200, 0, 100, 50), None, "entirely to the left");
        assert_eq!(sp(0, 0, 0, 10), None);
        assert_eq!(Spot::from_px(0, 0, 10, 10, 0, 100, 50), None);
    }

    fn cells_of(s: &Sim) -> Vec<(usize, usize)> {
        let cols = s.life.cols();
        s.spawn.iter().map(|&(i, _)| (i % cols, i / cols)).collect()
    }

    #[test]
    fn a_closed_window_blooms_where_it_was() {
        let cfg = quiet_cfg();
        let t0 = Instant::now();
        let mut s = Sim::new(200, 120, t0, &cfg);
        s.life.clear();
        let spot = Spot { x: 100, y: 60, w: 24, h: 16 };
        s.on_event_at(HyprEvent::CloseWindow, spot, t0, &cfg);
        let cells = cells_of(&s);
        assert!(cells.len() > 40, "a bloom, got {}", cells.len());
        for (x, y) in cells {
            assert!((100..124).contains(&x) && (60..76).contains(&y), "({x},{y}) is outside the window's footprint");
        }
    }

    #[test]
    fn a_huge_or_tiny_window_still_gets_a_sensible_bloom() {
        let cfg = quiet_cfg();
        let t0 = Instant::now();
        let mut s = Sim::new(200, 120, t0, &cfg);
        s.life.clear();
        s.on_event_at(HyprEvent::CloseWindow, Spot { x: 0, y: 0, w: 200, h: 120 }, t0, &cfg);
        let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0, usize::MAX, 0);
        for (x, y) in cells_of(&s) {
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        assert!(x1 - x0 < BLOOM_MAX.0 && y1 - y0 < BLOOM_MAX.1, "capped: {}x{}", x1 - x0 + 1, y1 - y0 + 1);
        assert!((60..140).contains(&x0) && (30..90).contains(&y0), "and centred in the window");
        s.spawn.clear();
        s.on_event_at(HyprEvent::CloseWindow, Spot { x: 50, y: 50, w: 1, h: 1 }, t0 + Duration::from_secs(1), &cfg);
        assert!(!s.spawn.is_empty(), "a one-cell window still blooms (at least a small patch)");
    }

    #[test]
    fn a_new_window_gets_a_halo_but_nothing_under_it() {
        let cfg = quiet_cfg();
        let t0 = Instant::now();
        let mut s = Sim::new(200, 120, t0, &cfg);
        s.life.clear();
        let spot = Spot { x: 60, y: 40, w: 40, h: 30 };
        s.on_event_at(HyprEvent::OpenWindow, spot, t0, &cfg);
        let cells = cells_of(&s);
        assert!(!cells.is_empty());
        for (x, y) in cells {
            let inside = (60..100).contains(&x) && (40..70).contains(&y);
            assert!(!inside, "({x},{y}) is under the window");
            assert!((57..103).contains(&x) && (37..73).contains(&y), "({x},{y}) is beyond the halo");
        }
        // a window that fills the whole output leaves no halo to draw
        let mut s = Sim::new(200, 120, t0, &cfg);
        s.life.clear();
        s.on_event_at(HyprEvent::OpenWindow, Spot { x: 0, y: 0, w: 200, h: 120 }, t0, &cfg);
        assert!(s.spawn.is_empty());
    }

    #[test]
    fn window_spots_at_the_edges_never_panic_and_respect_the_cooldown() {
        let cfg = quiet_cfg();
        let t0 = Instant::now();
        for (c, r) in [(3, 3), (12, 12), (40, 20), (200, 120)] {
            let mut s = Sim::new(c, r, t0, &cfg);
            let mut now = t0;
            for spot in [
                Spot { x: 0, y: 0, w: 1, h: 1 },
                Spot { x: c - 1, y: r - 1, w: 1, h: 1 },
                Spot { x: 0, y: 0, w: c, h: r },
                Spot { x: c / 2, y: r / 2, w: 50, h: 50 },
                Spot { x: c + 5, y: 0, w: 4, h: 4 },
            ] {
                for ev in [HyprEvent::OpenWindow, HyprEvent::CloseWindow, HyprEvent::Workspace] {
                    now += Duration::from_secs(1);
                    s.on_event_at(ev, spot, now, &cfg);
                }
            }
            let n = s.spawn.len();
            s.on_event_at(HyprEvent::CloseWindow, Spot { x: 0, y: 0, w: 4, h: 4 }, now + Duration::from_secs(1), &cfg);
            let grown = s.spawn.len();
            s.on_event_at(HyprEvent::CloseWindow, Spot { x: 0, y: 0, w: 4, h: 4 }, now + Duration::from_millis(1100), &cfg);
            assert_eq!(s.spawn.len(), grown, "second located event inside the cooldown is dropped ({c}x{r})");
            assert!(grown >= n);
        }
    }

    // -------------------------------------------------------------- beats

    #[test]
    fn a_beat_sends_a_glider_and_beats_are_rate_limited() {
        let cfg = quiet_cfg();
        let t0 = Instant::now();
        let mut s = Sim::new(120, 80, t0, &cfg);
        s.life.clear();
        s.on_beat(0.3, t0);
        assert_eq!(s.life.population(), 5, "one glider");
        s.on_beat(0.3, t0 + Duration::from_millis(100));
        assert_eq!(s.life.population(), 5, "inside the cooldown");
        s.on_beat(0.9, t0 + Duration::from_millis(400));
        assert_eq!(s.life.population(), 15, "a hard beat sends two");
    }

    #[test]
    fn beats_do_not_flood_a_crowded_board_and_fall_back_to_sparks_without_ships() {
        let cfg = quiet_cfg();
        let t0 = Instant::now();
        let mut s = Sim::new(40, 30, t0, &cfg);
        let mut rng = rand::thread_rng();
        s.life.seed(&mut rng, 0.5);
        let pop = s.life.population();
        s.on_beat(1.0, t0);
        assert_eq!(s.life.population(), pop, "a crowded board gets no more gliders");
        let seeds = Config { rule: RuleSel::Fixed(crate::life::SEEDS), ..quiet_cfg() };
        let mut s = Sim::new(60, 40, t0, &seeds);
        s.life.clear();
        s.on_beat(0.2, t0);
        assert!(s.life.population() > 0 && s.life.population() <= 9, "a spark, not a glider");
    }

    // -------------------------------------------------------------- clock

    #[test]
    fn the_clock_stamp_spells_the_time() {
        let cfg = quiet_cfg();
        let t0 = Instant::now();
        let mut s = Sim::new(120, 80, t0, &cfg);
        s.life.clear();
        assert!(!s.inject_clock_stamp(), "no time known, no stamp");
        s.set_clock(Some(14 * 60 + 7));
        assert!(s.inject_clock_stamp());
        let (w, h) = bbox(&s.life).unwrap();
        assert!((20..=29).contains(&w) && (6..=7).contains(&h), "five 5x7 glyphs, got {w}x{h}");
        // the same pixels the font gives for "14:07"
        let pat = patterns::text(&Font::build(), "14:07");
        assert_eq!(s.life.population(), pat.cells.len());
        // midnight and the last minute of the day
        for m in [0, 23 * 60 + 59] {
            s.set_clock(Some(m));
            assert!(s.inject_clock_stamp());
        }
        // too small a board is fine, it just does not fit
        let mut tiny = Sim::new(20, 10, t0, &cfg);
        tiny.set_clock(Some(600));
        assert!(!tiny.inject_clock_stamp());
    }

    #[test]
    fn the_clock_stamp_arrives_on_its_own_timer_and_only_when_asked_for() {
        let t0 = Instant::now();
        let base = Config { stamps: true, stamp_min_secs: 100_000, stamp_max_secs: 100_000, clock_stamp_secs: 2, local_calm_secs: 0, stagnation_secs: 86_400, ..quiet_cfg() };
        let mut s = Sim::new(120, 80, t0, &base);
        s.life.clear();
        s.set_clock(Some(9 * 60 + 41));
        run(&mut s, &base, t0, 24); // 6 s, the stamp is due after 1.5 to 2.5 s
        assert!(s.life.population() > 0, "the stamp never appeared");

        let off = Config { clock_stamp_secs: 0, ..base.clone() };
        let mut s = Sim::new(120, 80, t0, &off);
        s.life.clear();
        s.set_clock(Some(9 * 60 + 41));
        run(&mut s, &off, t0, 400);
        assert_eq!(s.life.population(), 0, "clock_stamp_secs = 0 means off");

        let no_stamps = Config { stamps: false, ..base };
        let mut s = Sim::new(120, 80, t0, &no_stamps);
        s.life.clear();
        s.set_clock(Some(9 * 60 + 41));
        run(&mut s, &no_stamps, t0, 80);
        assert_eq!(s.life.population(), 0, "stamps = false switches the clock off too");
    }

    // ---------------------------------------------------- replay and health

    #[test]
    fn a_seeded_run_replays_exactly() {
        let cfg = Config::default();
        let play = |seed| {
            let t0 = Instant::now();
            let mut s = Sim::seeded(80, 50, t0, &cfg, seed);
            run(&mut s, &cfg, t0, 600);
            (s.life.hash(), s.gen)
        };
        assert_eq!(play(3), play(3));
        assert_ne!(play(3).0, play(4).0);
    }

    /// What a long simulated run looked like, in % of the grid unless noted.
    #[derive(Debug)]
    struct Health {
        min_pop: f64,
        max_pop: f64,
        mean_pop: f64,
        mean_active: f64,
        max_settled: f64,
        mean_settled: f64,
        ff_share: f64,
        /// Fewest colonies holding at least 1% of the live cells at any sample.
        min_colonies: usize,
        /// The generation counter never went backwards (a reset would zero it).
        no_reset: bool,
    }

    /// Run `secs` simulated seconds at 8 generations per second and measure
    /// everything after the first minute (the opening soup is not typical).
    fn long_run(cfg: &Config, (cols, rows): (usize, usize), secs: u64, seed: u64, verbose: bool, prep: &dyn Fn(&mut Sim)) -> Health {
        let t0 = Instant::now();
        let mut s = Sim::seeded(cols, rows, t0, cfg, seed);
        prep(&mut s);
        let total = (cols * rows) as f64;
        let mut h = Health { min_pop: f64::MAX, max_pop: 0.0, mean_pop: 0.0, mean_active: 0.0, max_settled: 0.0, mean_settled: 0.0, ff_share: 0.0, min_colonies: usize::MAX, no_reset: true };
        let (mut n, mut ff, mut last_gen) = (0u64, 0u64, 0u64);
        let mut now = t0;
        if verbose {
            println!("{:>5} {:>6} {:>6} {:>7} {:>5}  colony shares (%)", "sec", "pop%", "active", "settled", "gen");
        }
        for tick in 1..=(secs * 8) {
            now += Duration::from_millis(125);
            s.set_clock(Some(12 * 60 + 34));
            s.tick(now, cfg, 0.0);
            h.no_reset &= s.gen >= last_gen;
            last_gen = s.gen;
            let sec = tick as f64 / 8.0;
            if sec <= 60.0 || tick % 4 != 0 {
                continue;
            }
            let (live, settled) = s.life.census();
            let pop = s.life.population() as f64 * 100.0 / total;
            let (settled_pct, active_pct) = (settled as f64 * 100.0 / total, (live - settled) as f64 * 100.0 / total);
            h.min_pop = h.min_pop.min(pop);
            h.max_pop = h.max_pop.max(pop);
            h.max_settled = h.max_settled.max(settled_pct);
            h.mean_pop += pop;
            h.mean_active += active_pct;
            h.mean_settled += settled_pct;
            ff += s.ff as u64;
            n += 1;
            if tick % (8 * 30) == 0 {
                let mut share = [0usize; 8];
                for (i, &c) in s.life.cells().iter().enumerate() {
                    if c == 1 {
                        share[s.life.colony()[i] as usize] += 1;
                    }
                }
                let big = share[..s.life.ncolonies()].iter().filter(|&&c| c * 100 >= live.max(1)).count();
                h.min_colonies = h.min_colonies.min(big);
                if verbose {
                    let sh: Vec<String> = share[..s.life.ncolonies()].iter().map(|&c| format!("{:.0}", c as f64 * 100.0 / live.max(1) as f64)).collect();
                    println!("{:>5.0} {:>6.1} {:>6.1} {:>7.1} {:>5}  {}", sec, pop, active_pct, settled_pct, s.gen, sh.join(" "));
                }
            }
        }
        let n = n.max(1) as f64;
        h.mean_pop /= n;
        h.mean_active /= n;
        h.mean_settled /= n;
        h.ff_share = ff as f64 / n;
        h
    }

    /// Grid for the health runs: a 1280x720 screen at 8 px cells.
    const HEALTH_GRID: (usize, usize) = (160, 90);

    /// The default board, ten simulated minutes, must stay alive without
    /// filling up or freezing, and must never be reset. If a tweak to the
    /// injections, decay, ripples or pruning breaks that, this fails; the
    /// bounds are loose enough for any seed but tight enough to notice.
    fn assert_healthy(seed: u64) {
        let cfg = Config::default();
        let h = long_run(&cfg, HEALTH_GRID, 600, seed, false, &|_| {});
        let ctx = format!("seed {seed}: {h:?}");
        assert!(h.no_reset, "the board was reset. {ctx}");
        assert!(h.min_pop >= 1.0, "the board went (nearly) dead. {ctx}");
        assert!(h.max_pop <= 30.0, "the board filled up. {ctx}");
        assert!((2.5..=16.0).contains(&h.mean_pop), "typical population is off. {ctx}");
        assert!(h.mean_active >= 1.5, "too little is moving. {ctx}");
        assert!(h.mean_settled <= 3.0, "too many frozen shapes on average. {ctx}");
        assert!(h.max_settled <= cfg.clutter_pct + 3.0, "pruning lets clutter through. {ctx}");
        assert!(h.ff_share <= 0.35, "the board is calm so often that it fast-forwards. {ctx}");
        assert!(h.min_colonies >= 2, "one colour took over. {ctx}");
    }

    #[test]
    fn the_default_board_stays_healthy_over_ten_minutes() {
        assert_healthy(11);
    }

    /// Several more seeds: `cargo test --release health_sweep -- --ignored`
    #[test]
    #[ignore]
    fn health_sweep() {
        for seed in 1..=12 {
            assert_healthy(seed);
        }
    }

    #[test]
    fn the_health_bounds_notice_a_board_that_goes_dead_or_clogs() {
        // Dead: every injection off and a board that starts empty.
        let dead = Config {
            local_calm_secs: 0,
            noise_per_sec: 0.0,
            traffic: false,
            patterns: false,
            stamps: false,
            glider_min_secs: 100_000,
            glider_max_secs: 100_000,
            stagnation_secs: 86_400,
            rule_shift_secs: 0,
            density: 0.0,
            ..Config::default()
        };
        let h = long_run(&dead, (80, 50), 300, 1, false, &|_| {});
        assert!(h.min_pop < 1.0, "a board with nothing feeding it must register as dead: {h:?}");
        // Clogged: no decay, no pruning, no help, and a board full of blocks.
        let clogged = Config {
            decay_after: 0,
            clutter_pct: 0.0,
            calm_pct: 0.0,
            local_calm_secs: 0,
            noise_per_sec: 0.0,
            rule_shift_secs: 0,
            stagnation_secs: 86_400,
            traffic: false,
            patterns: false,
            stamps: false,
            glider_min_secs: 100_000,
            glider_max_secs: 100_000,
            ..Config::default()
        };
        let lattice = |s: &mut Sim| {
            s.life.clear();
            let block = Pattern::from_art(&["##", "##"]);
            for y in (0..50).step_by(5) {
                for x in (0..80).step_by(5) {
                    s.life.place(&block, x, y, 0);
                }
            }
        };
        let h = long_run(&clogged, (80, 50), 300, 1, false, &lattice);
        assert!(h.mean_settled > 3.0, "a board that is never thinned must register as clogged: {h:?}");
    }

    /// Not a test: `cargo test --release long_run -- --ignored --nocapture`
    /// prints how a default board behaves over ten simulated minutes. Set
    /// `LW_OFF=shift,local,noise,decay,traffic` to switch mechanisms off for
    /// comparison, `LW_GENS` for the length of a rule shift and `LW_SEED`.
    #[test]
    #[ignore]
    fn long_run_report() {
        let mut cfg = Config::default();
        let off = std::env::var("LW_OFF").unwrap_or_default();
        if off.contains("shift") {
            cfg.rule_shift_secs = 0;
        }
        if off.contains("local") {
            cfg.local_calm_secs = 0;
        }
        if off.contains("noise") {
            cfg.noise_per_sec = 0.0;
        }
        if off.contains("decay") {
            cfg.decay_after = 0;
        }
        if off.contains("traffic") {
            cfg.traffic = false;
            cfg.patterns = false;
        }
        if let Some(g) = std::env::var("LW_GENS").ok().and_then(|v| v.parse().ok()) {
            cfg.rule_shift_gens = g;
        }
        let seed = std::env::var("LW_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        let h = long_run(&cfg, (240, 135), 600, seed, true, &|_| {});
        println!("{h:#?}");
    }
}
