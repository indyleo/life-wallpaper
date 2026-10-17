//! Policy layer, one `Sim` per output: owns the automaton, the code-rain layer
//! and every timer (optional soft reset, calm detection, gliders, specials,
//! stamps). A calm board gets new life dripped in over a few ticks; settled
//! shapes that clog the grid are pruned (whole shapes, over a few ticks, so
//! they fade out) instead of wiping the board.
//! Pure logic driven by `tick(now, ..)`; no Wayland in here.

use std::time::{Duration, Instant};

use rand::{rngs::ThreadRng, seq::SliceRandom, Rng};

use crate::config::{Config, RuleSel};
use crate::font::Font;
use crate::life::{Life, ROTATION};
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

pub const RAIN_LEVELS: u8 = 8;

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

pub struct Sim {
    pub life: Life,
    pub rain: Option<Rain>,
    pub gen: u64,
    pub heat: u8, // 0..=2, from CPU load
    rng: ThreadRng,
    rule_idx: usize,
    next_soft_reset: Instant,
    next_glider: Instant,
    next_special: Instant,
    next_stamp: Instant,
    history: [u64; HISTORY],
    hist_len: usize,
    hist_pos: usize,
    stagnant_since: Option<Instant>,
    calm_since: Option<Instant>,
    /// Cells still to be born / erased, drained a few per generation.
    spawn: Vec<usize>,
    spawn_ticks: u32,
    erase: Vec<usize>,
    erase_ticks: u32,
}

/// Take this generation's share of a queue: len / ticks_left, rounded up.
fn drain_share(queue: &mut Vec<usize>, ticks_left: &mut u32) -> Vec<usize> {
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

impl Sim {
    pub fn new(cols: usize, rows: usize, now: Instant, cfg: &Config) -> Self {
        let mut rng = rand::thread_rng();
        let rule = match cfg.rule {
            RuleSel::Fixed(r) => r,
            RuleSel::Rotate => ROTATION[0],
        };
        let mut life = Life::new(cols, rows, trail_len(cfg), rule);
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
            history: [0; HISTORY],
            hist_len: 0,
            hist_pos: 0,
            stagnant_since: None,
            calm_since: None,
            spawn: Vec::new(),
            spawn_ticks: 0,
            erase: Vec::new(),
            erase_ticks: 0,
        };
        s.reschedule(now, cfg);
        s
    }

    fn reschedule(&mut self, now: Instant, cfg: &Config) {
        self.next_soft_reset = now + Duration::from_secs(cfg.soft_reset_secs);
        self.next_glider =
            now + secs_between(&mut self.rng, cfg.glider_min_secs, cfg.glider_max_secs);
        self.next_special =
            now + secs_between(&mut self.rng, cfg.special_min_secs, cfg.special_max_secs);
        self.next_stamp = now + secs_between(&mut self.rng, cfg.stamp_min_secs, cfg.stamp_max_secs);
    }

    /// The wallpaper is visible again after a pause: restart every timer so
    /// gliders, stamps and the calm check don't all fire at once.
    pub fn resume(&mut self, now: Instant, cfg: &Config) {
        self.reschedule(now, cfg);
        self.reset_stagnation();
    }

    /// Config changed (SIGHUP): adopt rule/trail/timers.
    pub fn reconfigure(&mut self, now: Instant, cfg: &Config) {
        self.life.set_trail_len(trail_len(cfg));
        if let RuleSel::Fixed(r) = cfg.rule {
            if r != self.life.rule() {
                self.life.set_rule(r);
                self.reseed(cfg);
            }
        }
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
        self.life.resize(cols, rows);
        self.rain = None; // rebuilt lazily at the new size
        self.clear_queues();
        if significant {
            self.reseed(cfg);
        } else {
            self.reset_stagnation();
        }
    }

    /// One generation plus every timer.
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

        self.life.step();
        self.gen += 1;
        self.drip();

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
        if cfg.sysload
            && load > 0.5
            && self
                .rng
                .gen_bool(((load - 0.5) * 0.6).clamp(0.0, 1.0) as f64)
        {
            self.life.spark(&mut self.rng);
        }

        // 3. Gliders (or sparks on rules where gliders don't exist).
        if now >= self.next_glider {
            for _ in 0..self.rng.gen_range(1..=2) {
                if self.life.rule().supports_ships() {
                    self.life.place_ship_on_edge(&mut self.rng, true, false);
                } else {
                    self.life.spark(&mut self.rng);
                }
            }
            self.next_glider =
                now + secs_between(&mut self.rng, cfg.glider_min_secs, cfg.glider_max_secs);
        }

        // 4. Specials: LWSS, pulsar, Gosper gun.
        if now >= self.next_special {
            if cfg.patterns {
                self.inject_special();
            }
            self.next_special =
                now + secs_between(&mut self.rng, cfg.special_min_secs, cfg.special_max_secs);
        }

        // 5. Stamps: Tux, Arch logo, terminal text.
        if now >= self.next_stamp {
            if cfg.stamps {
                self.inject_stamp();
            }
            self.next_stamp =
                now + secs_between(&mut self.rng, cfg.stamp_min_secs, cfg.stamp_max_secs);
        }

        // 6. Clutter / calm census. When settled shapes (still lifes,
        //    oscillators) fill too much of the grid, some of them are pruned
        //    away; a merely calm board gets new life added on top of what it
        //    has. Neither wipes the board.
        if self.gen.is_multiple_of(CENSUS_EVERY) {
            let total = (self.life.cols() * self.life.rows()) as f64;
            let (live, settled) = self.life.census();
            if self.erase.is_empty()
                && cfg.clutter_pct > 0.0
                && settled as f64 * 100.0 >= total * cfg.clutter_pct
            {
                self.prune();
            }
            let active = (live - settled) as f64;
            if cfg.calm_pct > 0.0 && active * 100.0 < total * cfg.calm_pct {
                self.calm_since.get_or_insert(now);
            } else {
                self.calm_since = None;
            }
        }
        if let Some(since) = self.calm_since {
            if now.duration_since(since) >= Duration::from_secs(cfg.stagnation_secs) {
                self.inject_fresh();
                return;
            }
        }

        // 7. Whole-grid repeat: a state seen within the last HISTORY
        //    generations for `stagnation_secs` straight => inject new life.
        let h = self.life.hash();
        if self.history[..self.hist_len].contains(&h) {
            let since = *self.stagnant_since.get_or_insert(now);
            if now.duration_since(since) >= Duration::from_secs(cfg.stagnation_secs) {
                self.inject_fresh();
                return;
            }
        } else {
            self.stagnant_since = None;
        }
        self.history[self.hist_pos] = h;
        self.hist_pos = (self.hist_pos + 1) % HISTORY;
        self.hist_len = (self.hist_len + 1).min(HISTORY);
    }

    /// Add new life without clearing anything: a few soup patches in the
    /// emptiest parts of the board (their cells appear over the next few
    /// generations), plus gliders where they work.
    fn inject_fresh(&mut self) {
        let (cols, rows) = (self.life.cols(), self.life.rows());
        let rule = self.life.rule();
        let pw = (cols / 3).clamp(4, 32).min(cols);
        let ph = (rows / 3).clamp(4, 20).min(rows);
        let density = (0.35 * rule.seed_scale()).min(0.9);
        for _ in 0..self.rng.gen_range(2..=4) {
            let (x, y) = self.life.emptiest_spot(&mut self.rng, pw, ph, 24);
            let cells = self.life.soup_cells(&mut self.rng, x, y, pw, ph, density);
            self.spawn.extend(cells);
        }
        self.spawn.shuffle(&mut self.rng);
        self.spawn_ticks = DRIP_TICKS;
        if rule.supports_ships() {
            for _ in 0..self.rng.gen_range(1..=2) {
                self.life.place_ship_on_edge(&mut self.rng, true, false);
            }
        }
        self.reset_stagnation();
    }

    /// Birth / erase this generation's share of the queued cells.
    fn drip(&mut self) {
        if !self.spawn.is_empty() {
            for i in drain_share(&mut self.spawn, &mut self.spawn_ticks) {
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

    /// Adopt a saved board. False (nothing changed) if the board is empty or
    /// was saved under a rule the config doesn't allow.
    pub fn restore(&mut self, saved: &Saved, cfg: &Config) -> bool {
        let Some(rule) = crate::life::Rule::parse(&saved.rule) else {
            return false;
        };
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
        self.life.set_rule(rule);
        self.life.load(saved);
        self.clear_queues();
        self.reset_stagnation();
        true
    }

    fn inject_special(&mut self) {
        if !self.life.rule().supports_ships() {
            for _ in 0..3 {
                self.life.spark(&mut self.rng);
            }
            return;
        }
        match self.rng.gen_range(0..3) {
            0 => {
                self.life.place_ship_on_edge(&mut self.rng, false, true);
            }
            1 => {
                let p = patterns::pulsar();
                self.life.place_random(&mut self.rng, &p, 2, 1);
            }
            _ => {
                self.life.place_gun(&mut self.rng);
            }
        }
    }

    fn inject_stamp(&mut self) {
        let font = Font::build();
        let n = 2 + patterns::STAMP_TEXTS.len();
        let pat = match self.rng.gen_range(0..n) {
            0 => patterns::tux(),
            1 => patterns::arch(),
            i => patterns::text(&font, patterns::STAMP_TEXTS[i - 2]),
        };
        // Clear a 2-cell margin so the stamp is legible before the rules eat it.
        self.life.place_random(&mut self.rng, &pat, 3, 2);
    }

    /// Clear + random soup at the reseed density; rotate rules if asked to.
    pub fn reseed(&mut self, cfg: &Config) {
        let rule = match cfg.rule {
            RuleSel::Fixed(r) => r,
            RuleSel::Rotate => {
                self.rule_idx = (self.rule_idx + 1) % ROTATION.len();
                ROTATION[self.rule_idx]
            }
        };
        self.life.set_rule(rule);
        self.life.seed(
            &mut self.rng,
            (cfg.reseed_density * rule.seed_scale()).min(0.9),
        );
        self.gen = 0;
        self.clear_queues();
        self.reset_stagnation();
    }

    fn reset_stagnation(&mut self) {
        self.stagnant_since = None;
        self.calm_since = None;
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

    fn quiet_cfg() -> Config {
        Config {
            patterns: false,
            stamps: false,
            glider_min_secs: 100_000,
            glider_max_secs: 100_000,
            ..Config::default()
        }
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
            if s.life.population() > 20 {
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
        assert!(
            seen.len() >= 2 && seen[0] < 20,
            "population jumped: {seen:?}"
        );
    }

    #[test]
    fn no_timed_reset_by_default() {
        let cfg = Config {
            calm_pct: 0.0,
            clutter_pct: 0.0,
            stagnation_secs: 86_400,
            ..quiet_cfg()
        };
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
        let cfg = Config {
            clutter_pct: 5.0,
            ..quiet_cfg()
        };
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
        assert!(
            (settled as f64) * 100.0 / 1200.0 < 5.0,
            "still cluttered: {settled}"
        );
        assert_eq!(settled % 4, 0, "shapes should be removed whole");
    }

    #[test]
    fn pruned_cells_leave_death_trails() {
        let cfg = Config {
            clutter_pct: 5.0,
            trail: true,
            trail_length: 4,
            ..quiet_cfg()
        };
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
        let cfg = Config {
            clutter_pct: 5.0,
            calm_pct: 0.0,
            stagnation_secs: 86_400,
            ..quiet_cfg()
        };
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
        let saved = Saved {
            cols: 30,
            rows: 20,
            rule: a.life.rule().name(),
            cells: a.life.cells().to_vec(),
            age: a.life.age().to_vec(),
        };

        let mut b = Sim::new(30, 20, Instant::now(), &cfg);
        assert!(b.restore(&saved, &cfg));
        assert_eq!(b.life.cells(), a.life.cells());

        let other = Config {
            rule: RuleSel::Fixed(crate::life::SEEDS),
            ..Config::default()
        };
        let mut c = Sim::new(30, 20, Instant::now(), &other);
        assert!(!c.restore(&saved, &other)); // saved under life, config wants seeds

        let empty = Saved {
            cells: vec![0; 600],
            ..saved
        };
        assert!(!b.restore(&empty, &cfg));
    }

    #[test]
    fn rotate_cycles_rules_on_reseed() {
        let cfg = Config {
            rule: RuleSel::Rotate,
            ..Config::default()
        };
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
                s.inject_special();
                s.inject_stamp();
                s.inject_fresh();
            }
        }
    }
}
