//! Pure cellular-automaton core on flat, toroidal `Vec<u8>` grids.
//! Supports life-like B/S rules and Brian's Brain. No Wayland, no timing,
//! and `step` never allocates.

use rand::Rng;

use crate::patterns::{self, Pattern};

/// Cell states: 0 = dead, 1 = alive, 2 = dying (Brian's Brain only).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rule {
    /// Bit n of `birth`/`survive` set => n live neighbours triggers it.
    LifeLike { birth: u16, survive: u16 },
    Brain,
}

pub const CONWAY: Rule = Rule::LifeLike { birth: 1 << 3, survive: (1 << 2) | (1 << 3) };
pub const HIGHLIFE: Rule = Rule::LifeLike { birth: (1 << 3) | (1 << 6), survive: (1 << 2) | (1 << 3) };
pub const DAY_NIGHT: Rule = Rule::LifeLike {
    birth: (1 << 3) | (1 << 6) | (1 << 7) | (1 << 8),
    survive: (1 << 3) | (1 << 4) | (1 << 6) | (1 << 7) | (1 << 8),
};
pub const SEEDS: Rule = Rule::LifeLike { birth: 1 << 2, survive: 0 };

/// Order used by `rule = "rotate"`.
pub const ROTATION: [Rule; 5] = [CONWAY, HIGHLIFE, DAY_NIGHT, SEEDS, Rule::Brain];

impl Rule {
    pub fn parse(s: &str) -> Option<Rule> {
        let s = s.trim().to_ascii_lowercase();
        match s.as_str() {
            "life" | "conway" => return Some(CONWAY),
            "highlife" => return Some(HIGHLIFE),
            "daynight" | "day-night" | "day&night" | "dayandnight" => return Some(DAY_NIGHT),
            "seeds" => return Some(SEEDS),
            "brain" | "brians-brain" | "briansbrain" => return Some(Rule::Brain),
            _ => {}
        }
        // Generic "B36/S23" notation.
        let (b, sv) = s.split_once('/')?;
        let b = b.strip_prefix('b')?;
        let sv = sv.strip_prefix('s')?;
        let mask = |digits: &str| -> Option<u16> {
            let mut m = 0u16;
            for c in digits.chars() {
                let d = c.to_digit(10)?;
                if d > 8 {
                    return None;
                }
                m |= 1 << d;
            }
            Some(m)
        };
        Some(Rule::LifeLike { birth: mask(b)?, survive: mask(sv)? })
    }

    pub fn name(&self) -> String {
        match *self {
            r if r == CONWAY => "life".into(),
            r if r == HIGHLIFE => "highlife".into(),
            r if r == DAY_NIGHT => "daynight".into(),
            r if r == SEEDS => "seeds".into(),
            Rule::Brain => "brain".into(),
            Rule::LifeLike { birth, survive } => {
                let digits = |m: u16| (0..=8).filter(|n| m >> n & 1 == 1).map(|n| n.to_string()).collect::<String>();
                format!("b{}/s{}", digits(birth), digits(survive))
            }
        }
    }

    /// Rules where Conway's gliders/spaceships/guns actually work.
    pub fn supports_ships(&self) -> bool {
        *self == CONWAY || *self == HIGHLIFE
    }

    /// Soup density multiplier: explosive rules need sparser soups.
    pub fn seed_scale(&self) -> f64 {
        match *self {
            r if r == SEEDS => 0.2,
            Rule::Brain => 0.5,
            r if r == DAY_NIGHT => 1.5,
            _ => 1.0,
        }
    }
}

pub struct Life {
    cols: usize,
    rows: usize,
    rule: Rule,
    trail_len: u8,
    cur: Vec<u8>,
    next: Vec<u8>,
    age: Vec<u8>,
    trail: Vec<u8>,
    /// Per-cell occupancy over the last 64 generations (bit 0 = newest).
    /// Used by `census` to spot cells stuck in a still life / oscillator.
    hist: Vec<u64>,
}

/// Longest oscillator period `census` recognises. The 64-bit history leaves
/// at least 34 bits to compare at this period, so a match is not a fluke.
const MAX_PERIOD: u32 = 30;

/// True if the occupancy history repeats with some period 1..=MAX_PERIOD.
fn is_periodic(h: u64) -> bool {
    (1..=MAX_PERIOD).any(|p| (h ^ (h >> p)) & ((1u64 << (64 - p)) - 1) == 0)
}

impl Life {
    pub fn new(cols: usize, rows: usize, trail_len: u8, rule: Rule) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let n = cols * rows;
        Self {
            cols,
            rows,
            rule,
            trail_len,
            cur: vec![0; n],
            next: vec![0; n],
            age: vec![0; n],
            trail: vec![0; n],
            hist: vec![0; n],
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }
    pub fn rows(&self) -> usize {
        self.rows
    }
    pub fn rule(&self) -> Rule {
        self.rule
    }
    pub fn cells(&self) -> &[u8] {
        &self.cur
    }
    pub fn age(&self) -> &[u8] {
        &self.age
    }
    pub fn trail(&self) -> &[u8] {
        &self.trail
    }
    pub fn trail_len(&self) -> u8 {
        self.trail_len
    }

    pub fn set_trail_len(&mut self, n: u8) {
        self.trail_len = n;
        if n == 0 {
            self.trail.fill(0);
        }
    }

    pub fn set_rule(&mut self, rule: Rule) {
        self.rule = rule;
        // Leave no stray "dying" cells behind when leaving Brian's Brain.
        for c in self.cur.iter_mut() {
            if *c > 1 {
                *c = 0;
            }
        }
    }

    pub fn clear(&mut self) {
        self.cur.fill(0);
        self.age.fill(0);
        self.trail.fill(0);
        self.hist.fill(0);
    }

    /// Clear, then make each cell alive with probability `density`.
    pub fn seed<R: Rng>(&mut self, rng: &mut R, density: f64) {
        self.clear();
        let d = density.clamp(0.0, 1.0);
        for c in self.cur.iter_mut() {
            *c = rng.gen_bool(d) as u8;
        }
    }

    /// Change grid dimensions, keeping whatever overlaps. (Allocates.)
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        let copy = |old: &Vec<u8>, (oc, or): (usize, usize)| {
            let mut new = vec![0u8; cols * rows];
            let cc = cols.min(oc);
            for y in 0..rows.min(or) {
                new[y * cols..y * cols + cc].copy_from_slice(&old[y * oc..y * oc + cc]);
            }
            new
        };
        let old = (self.cols, self.rows);
        self.cur = copy(&self.cur, old);
        self.age = copy(&self.age, old);
        self.trail = copy(&self.trail, old);
        self.next = vec![0; cols * rows];
        self.hist = vec![0; cols * rows];
        self.cols = cols;
        self.rows = rows;
    }

    /// Advance one generation (edges wrap). Returns `true` if any cell changed.
    pub fn step(&mut self) -> bool {
        let changed = match self.rule {
            Rule::LifeLike { birth, survive } => self.step_lifelike(birth, survive),
            Rule::Brain => self.step_brain(),
        };
        // Age + death-trail bookkeeping, one pass.
        let tl = self.trail_len;
        for i in 0..self.cur.len() {
            let (was, now) = (self.cur[i], self.next[i]);
            self.age[i] = match (was, now) {
                (1, 1) => self.age[i].saturating_add(1),
                _ => 0,
            };
            self.trail[i] = if now != 0 {
                0
            } else if was != 0 {
                tl
            } else {
                self.trail[i].saturating_sub(1)
            };
            self.hist[i] = (self.hist[i] << 1) | (now != 0) as u64;
        }
        std::mem::swap(&mut self.cur, &mut self.next);
        changed
    }

    fn step_lifelike(&mut self, birth: u16, survive: u16) -> bool {
        let (cols, rows) = (self.cols, self.rows);
        let (cur, next) = (&self.cur, &mut self.next);
        let mut changed = false;
        for y in 0..rows {
            let row = y * cols;
            let up = if y == 0 { rows - 1 } else { y - 1 } * cols;
            let down = if y + 1 == rows { 0 } else { y + 1 } * cols;
            for x in 0..cols {
                let l = if x == 0 { cols - 1 } else { x - 1 };
                let r = if x + 1 == cols { 0 } else { x + 1 };
                let n = cur[up + l]
                    + cur[up + x]
                    + cur[up + r]
                    + cur[row + l]
                    + cur[row + r]
                    + cur[down + l]
                    + cur[down + x]
                    + cur[down + r];
                let alive = cur[row + x];
                let mask = if alive == 1 { survive } else { birth };
                let v = (mask >> n & 1) as u8;
                next[row + x] = v;
                changed |= v != alive;
            }
        }
        changed
    }

    fn step_brain(&mut self) -> bool {
        let (cols, rows) = (self.cols, self.rows);
        let (cur, next) = (&self.cur, &mut self.next);
        let mut changed = false;
        for y in 0..rows {
            let row = y * cols;
            let up = if y == 0 { rows - 1 } else { y - 1 } * cols;
            let down = if y + 1 == rows { 0 } else { y + 1 } * cols;
            for x in 0..cols {
                let l = if x == 0 { cols - 1 } else { x - 1 };
                let r = if x + 1 == cols { 0 } else { x + 1 };
                let on = |i: usize| (cur[i] == 1) as u8;
                let n = on(up + l) + on(up + x) + on(up + r) + on(row + l) + on(row + r) + on(down + l) + on(down + x) + on(down + r);
                let c = cur[row + x];
                let v = match c {
                    0 => (n == 2) as u8,
                    1 => 2,
                    _ => 0,
                };
                next[row + x] = v;
                changed |= v != c;
            }
        }
        changed
    }

    /// FNV-1a over the current generation.
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &c in &self.cur {
            h ^= c as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    pub fn population(&self) -> usize {
        self.cur.iter().filter(|&&c| c == 1).count()
    }

    /// `(live, settled)`: occupied cells, and how many of those have been
    /// repeating with a period of at most 30 generations for the last 64
    /// (still lifes, oscillators, gun bodies). Gliders and chaos don't count.
    pub fn census(&self) -> (usize, usize) {
        let mut live = 0;
        let mut settled = 0;
        for (&c, &h) in self.cur.iter().zip(&self.hist) {
            if c != 0 {
                live += 1;
                settled += is_periodic(h) as usize;
            }
        }
        (live, settled)
    }

    /// Occupied cells inside the `w` x `h` rectangle at (x, y).
    pub fn count_in(&self, x: usize, y: usize, w: usize, h: usize) -> usize {
        let (x1, y1) = ((x + w).min(self.cols), (y + h).min(self.rows));
        let mut n = 0;
        for yy in y..y1 {
            n += self.cur[yy * self.cols + x..yy * self.cols + x1].iter().filter(|&&c| c != 0).count();
        }
        n
    }

    /// Top-left of the emptiest of `tries` random `w` x `h` rectangles.
    pub fn emptiest_spot<R: Rng>(&self, rng: &mut R, w: usize, h: usize, tries: usize) -> (usize, usize) {
        let (w, h) = (w.min(self.cols), h.min(self.rows));
        let mut best = (0, 0);
        let mut best_n = usize::MAX;
        for _ in 0..tries.max(1) {
            let x = rng.gen_range(0..=self.cols - w);
            let y = rng.gen_range(0..=self.rows - h);
            let n = self.count_in(x, y, w, h);
            if n < best_n {
                best_n = n;
                best = (x, y);
            }
        }
        best
    }

    /// Scatter random live cells over a rectangle without touching what is
    /// already alive. Out-of-range parts of the rectangle are ignored.
    pub fn soup_patch<R: Rng>(&mut self, rng: &mut R, x: usize, y: usize, w: usize, h: usize, density: f64) {
        let d = density.clamp(0.0, 1.0);
        for yy in y..(y + h).min(self.rows) {
            for xx in x..(x + w).min(self.cols) {
                let i = yy * self.cols + xx;
                if self.cur[i] == 0 && rng.gen_bool(d) {
                    self.cur[i] = 1;
                    self.age[i] = 0;
                    self.trail[i] = 0;
                }
            }
        }
    }

    /// OR a pattern in at (x, y); optionally clear a margin around it first.
    /// The caller guarantees it fits.
    pub fn place(&mut self, pat: &Pattern, x: usize, y: usize, clear_margin: usize) {
        if clear_margin > 0 {
            let x0 = x.saturating_sub(clear_margin);
            let y0 = y.saturating_sub(clear_margin);
            let x1 = (x + pat.w + clear_margin).min(self.cols);
            let y1 = (y + pat.h + clear_margin).min(self.rows);
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let i = yy * self.cols + xx;
                    self.cur[i] = 0;
                    self.age[i] = 0;
                    self.trail[i] = 0;
                }
            }
        }
        for &(cx, cy) in &pat.cells {
            let i = (y + cy) * self.cols + x + cx;
            self.cur[i] = 1;
            self.age[i] = 0;
            self.trail[i] = 0;
        }
    }

    /// Drop a small random blob somewhere; used for rules where Conway's
    /// ships don't work, and as "load spikes" for the system-driven mode.
    pub fn spark<R: Rng>(&mut self, rng: &mut R) {
        let (bw, bh) = (3.min(self.cols), 3.min(self.rows));
        let x = rng.gen_range(0..=self.cols - bw);
        let y = rng.gen_range(0..=self.rows - bh);
        for dy in 0..bh {
            for dx in 0..bw {
                if rng.gen_bool(0.6) {
                    let i = (y + dy) * self.cols + x + dx;
                    self.cur[i] = 1;
                    self.age[i] = 0;
                    self.trail[i] = 0;
                }
            }
        }
    }

    /// Put `ship` (default heading east, or south-east for the glider) on a
    /// random edge, mirrored/transposed so it flies inwards. Only the ship's
    /// own cells are touched. Returns false if it can't fit.
    pub fn place_ship_on_edge<R: Rng>(&mut self, rng: &mut R, glider: bool, lwss: bool) -> bool {
        let edge = rng.gen_range(0..4); // 0 top, 1 bottom, 2 left, 3 right
        let flip = rng.gen_bool(0.5);
        let pat = if glider {
            // heads SE; mirror so the heading has a positive component away from the edge
            let (h, v) = match edge {
                0 => (flip, false),
                1 => (flip, true),
                2 => (false, flip),
                _ => (true, flip),
            };
            patterns::glider().transformed(false, h, v)
        } else {
            debug_assert!(lwss);
            // heads E; transpose for top/bottom edges
            match edge {
                0 => patterns::lwss().transformed(true, flip, false),
                1 => patterns::lwss().transformed(true, flip, true),
                2 => patterns::lwss().transformed(false, false, flip),
                _ => patterns::lwss().transformed(false, true, flip),
            }
        };
        if pat.w > self.cols || pat.h > self.rows {
            return false;
        }
        let (mx, my) = (self.cols - pat.w, self.rows - pat.h);
        let (x, y) = match edge {
            0 => (rng.gen_range(0..=mx), 0),
            1 => (rng.gen_range(0..=mx), my),
            2 => (0, rng.gen_range(0..=my)),
            _ => (mx, rng.gen_range(0..=my)),
        };
        self.place(&pat, x, y, 0);
        true
    }

    /// Place at a random position with at least `margin` free cells to the border.
    pub fn place_random<R: Rng>(&mut self, rng: &mut R, pat: &Pattern, margin: usize, clear: usize) -> bool {
        if pat.w + 2 * margin > self.cols || pat.h + 2 * margin > self.rows {
            return false;
        }
        let x = rng.gen_range(margin..=self.cols - pat.w - margin);
        let y = rng.gen_range(margin..=self.rows - pat.h - margin);
        self.place(pat, x, y, clear);
        true
    }

    /// Gosper gun in a random corner, mirrored so it fires into the grid.
    pub fn place_gun<R: Rng>(&mut self, rng: &mut R) -> bool {
        let (hf, vf) = (rng.gen_bool(0.5), rng.gen_bool(0.5));
        let gun = patterns::gosper_gun().transformed(false, hf, vf);
        let m = 3;
        if gun.w + 2 * m > self.cols || gun.h + 2 * m > self.rows {
            return false;
        }
        let x = if hf { self.cols - gun.w - m } else { m };
        let y = if vf { self.rows - gun.h - m } else { m };
        self.place(&gun, x, y, 2);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(l: &Life) -> usize {
        l.population()
    }

    #[test]
    fn blinker_oscillates() {
        let mut l = Life::new(5, 5, 0, CONWAY);
        l.place(&Pattern::from_art(&["###"]), 1, 2, 0);
        let a = l.hash();
        assert!(l.step());
        assert_ne!(a, l.hash());
        assert!(l.step());
        assert_eq!(a, l.hash());
    }

    #[test]
    fn block_is_stable() {
        let mut l = Life::new(6, 6, 0, CONWAY);
        l.place(&Pattern::from_art(&["##", "##"]), 2, 2, 0);
        assert!(!l.step());
    }

    #[test]
    fn edge_ships_add_exact_cells_everywhere() {
        let mut rng = rand::thread_rng();
        for _ in 0..500 {
            let mut l = Life::new(11, 7, 0, CONWAY);
            assert!(l.place_ship_on_edge(&mut rng, true, false));
            assert_eq!(count(&l), 5);
            let mut l = Life::new(11, 7, 0, CONWAY);
            assert!(l.place_ship_on_edge(&mut rng, false, true));
            assert_eq!(count(&l), 9);
        }
    }

    #[test]
    fn seeds_blinker_dies_and_explodes_as_expected() {
        // Seeds: B2/S. Two adjacent cells give birth to the two cells either side.
        let mut l = Life::new(8, 8, 0, SEEDS);
        l.place(&Pattern::from_art(&["##"]), 3, 3, 0);
        l.step();
        assert_eq!(count(&l), 4); // the pair dies; four cells above/below are born
    }

    #[test]
    fn brain_cycles_through_dying() {
        let mut l = Life::new(8, 8, 0, Rule::Brain);
        l.place(&Pattern::from_art(&["##"]), 3, 3, 0);
        l.step();
        assert!(l.cells().iter().any(|&c| c == 2));
        assert_eq!(l.cells().iter().filter(|&&c| c == 2).count(), 2);
    }

    #[test]
    fn age_and_trail_track_state() {
        let mut l = Life::new(6, 6, 3, CONWAY);
        l.place(&Pattern::from_art(&["##", "##"]), 2, 2, 0);
        l.step();
        l.step();
        assert_eq!(l.age()[2 * 6 + 2], 2);
        l.clear();
        l.place(&Pattern::from_art(&["#"]), 2, 2, 0);
        l.step(); // lone cell dies
        assert_eq!(l.trail()[2 * 6 + 2], 3);
        l.step();
        assert_eq!(l.trail()[2 * 6 + 2], 2);
    }

    #[test]
    fn census_counts_only_settled_cells() {
        let mut l = Life::new(30, 30, 0, CONWAY);
        l.place(&Pattern::from_art(&["##", "##"]), 2, 2, 0); // block
        l.place(&Pattern::from_art(&["###"]), 10, 10, 0); // blinker
        l.place(&patterns::glider(), 20, 5, 0); // keeps moving
        for _ in 0..70 {
            l.step();
        }
        assert_eq!(l.census(), (12, 7));
    }

    #[test]
    fn periodic_detection_covers_period_30_but_not_pulses() {
        let p30: u64 = (0..64).fold(0, |h, i| h | ((i % 30 == 0) as u64) << i);
        assert!(is_periodic(p30));
        assert!(is_periodic(u64::MAX));
        assert!(!is_periodic(0b0111_1000_0000));
    }

    #[test]
    fn soup_patch_keeps_existing_cells_and_emptiest_spot_avoids_them() {
        let mut rng = rand::thread_rng();
        let mut l = Life::new(40, 20, 0, CONWAY);
        l.soup_patch(&mut rng, 0, 0, 20, 20, 1.0);
        assert_eq!(l.population(), 400);
        l.soup_patch(&mut rng, 0, 0, 40, 20, 1.0);
        assert_eq!(l.population(), 800);
        let mut l = Life::new(40, 20, 0, CONWAY);
        l.soup_patch(&mut rng, 0, 0, 20, 20, 1.0);
        for _ in 0..20 {
            let (x, y) = l.emptiest_spot(&mut rng, 10, 10, 64);
            assert_eq!(l.count_in(x, y, 10, 10), 0);
        }
    }

    #[test]
    fn rule_parsing() {
        assert_eq!(Rule::parse("B3/S23"), Some(CONWAY));
        assert_eq!(Rule::parse("highlife"), Some(HIGHLIFE));
        assert_eq!(Rule::parse("b2/s"), Some(SEEDS));
        assert_eq!(Rule::parse("nope"), None);
        assert_eq!(DAY_NIGHT.name(), "daynight");
        assert_eq!(Rule::parse("b36/s245").unwrap().name(), "b36/s245");
    }

    #[test]
    fn resize_keeps_overlap() {
        let mut l = Life::new(4, 4, 0, CONWAY);
        l.place(&Pattern::from_art(&["#"]), 1, 1, 0);
        l.resize(8, 2);
        assert_eq!(l.cells().len(), 16);
        assert_eq!(l.cells()[8 + 1], 1);
    }
}