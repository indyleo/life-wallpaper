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

/// Symmetry of an injected soup patch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sym {
    None,
    /// Left-right mirror.
    Mirror,
    /// Top-bottom mirror.
    Flip,
    /// Both mirrors (four-fold).
    Both,
    /// 180 degree rotation.
    Rot,
}

pub const SYMMETRIES: [Sym; 4] = [Sym::Mirror, Sym::Flip, Sym::Both, Sym::Rot];

/// xorshift64*: a few cycles per call and no allocation, so `step` can roll
/// dice (colony mutation, decay) without taking an `Rng`.
#[inline]
fn rnd(state: &mut u64) -> u32 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
}

/// Colony of a newborn cell: the majority among its live neighbours (ties
/// broken at random), or now and then a random colony (mutation).
#[inline]
fn inherit(cur: &[u8], colony: &[u8], nb: [usize; 8], nc: u8, mut_thr: u32, rng: &mut u64) -> u8 {
    if mut_thr > 0 && rnd(rng) < mut_thr {
        return (rnd(rng) % nc as u32) as u8;
    }
    let mut cnt = [0u8; 8];
    for &j in &nb {
        if cur[j] == 1 {
            cnt[(colony[j] & 7) as usize] += 1;
        }
    }
    let best = *cnt.iter().max().unwrap_or(&0);
    let ties = cnt.iter().filter(|&&c| c == best).count() as u32;
    let mut pick = rnd(rng) % ties.max(1);
    for (k, &c) in cnt.iter().enumerate() {
        if c == best {
            if pick == 0 {
                return (k as u8).min(nc - 1);
            }
            pick -= 1;
        }
    }
    0
}

/// A second life-like rule that applies only to the cells marked in `mask`.
/// Used for rule ripples: the controller switches tiles over one at a time.
struct Alt {
    birth: u16,
    survive: u16,
    /// One byte per cell, 1 = this cell follows the alternative rule.
    mask: Vec<u8>,
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
    /// Colony of every cell. A dead cell keeps its last colony so its death
    /// trail can still be tinted. Only meaningful when `ncolonies >= 2`.
    colony: Vec<u8>,
    /// Consecutive generations each live cell has spent stuck in a still life
    /// or oscillator (counted once the 64-generation history shows it).
    settled: Vec<u8>,
    ncolonies: u8,
    /// Chance (scaled to u32) that a newborn picks a random colony.
    mutation: u32,
    /// Colony given to cells created by `set_alive`, `place` and `spark`.
    brush: u8,
    /// Settled cells older than this may decay (0 = never) ...
    decay_after: u8,
    /// ... with this per-generation probability (scaled to u32).
    decay_thr: u32,
    alt: Option<Alt>,
    rng: u64,
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
            colony: vec![0; n],
            settled: vec![0; n],
            ncolonies: 0,
            mutation: 0,
            brush: 0,
            decay_after: 0,
            decay_thr: 0,
            alt: None,
            rng: rand::thread_rng().gen::<u64>() | 1,
        }
    }

    /// Fix the generator `step` uses for colony mutation and decay, so a
    /// simulation can be replayed exactly (tests).
    pub fn seed_rng(&mut self, seed: u64) {
        self.rng = seed | 1;
    }

    /// Start a rule ripple: cells marked later with `alt_rect` follow `rule`
    /// instead of the board's rule. Only between life-like rules; false (and
    /// nothing changes) otherwise.
    pub fn begin_alt(&mut self, rule: Rule) -> bool {
        match (self.rule, rule) {
            (Rule::LifeLike { .. }, Rule::LifeLike { birth, survive }) => {
                self.alt = Some(Alt { birth, survive, mask: vec![0; self.cur.len()] });
                true
            }
            _ => false,
        }
    }

    /// Mark (or unmark) a rectangle of cells for the alternative rule.
    pub fn alt_rect(&mut self, x: usize, y: usize, w: usize, h: usize, on: bool) {
        let cols = self.cols;
        if let Some(a) = self.alt.as_mut() {
            for yy in y..(y + h).min(self.rows) {
                a.mask[yy * cols + x.min(cols)..yy * cols + (x + w).min(cols)].fill(on as u8);
            }
        }
    }

    /// Back to one rule for the whole board.
    pub fn end_alt(&mut self) {
        self.alt = None;
    }

    /// How many cells currently follow the alternative rule.
    #[cfg(test)]
    pub fn alt_cells(&self) -> usize {
        self.alt.as_ref().map_or(0, |a| a.mask.iter().filter(|&&m| m != 0).count())
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

    /// Colony of every cell (see `colonies_on`).
    pub fn colony(&self) -> &[u8] {
        &self.colony
    }
    pub fn colonies_on(&self) -> bool {
        self.ncolonies >= 2
    }
    pub fn ncolonies(&self) -> usize {
        self.ncolonies as usize
    }

    /// Turn colony tracking on (`n >= 2`, at most 8) or off. Returns true when
    /// it was just switched on, so the caller can `scatter_colonies`.
    pub fn set_colonies(&mut self, n: usize, mutation: f64) -> bool {
        let was_on = self.colonies_on();
        self.ncolonies = if n >= 2 { n.min(8) as u8 } else { 0 };
        self.mutation = (mutation.clamp(0.0, 1.0) * u32::MAX as f64) as u32;
        let nc = self.ncolonies.max(1);
        for c in self.colony.iter_mut() {
            *c %= nc;
        }
        self.brush %= nc;
        self.colonies_on() && !was_on
    }

    /// Colony used by `set_alive`, `place` and `spark` from now on.
    pub fn set_brush(&mut self, colony: u8) {
        self.brush = colony % self.ncolonies.max(1);
    }

    /// Cells that have sat in a still life / oscillator for `after`
    /// generations die with probability `chance` per generation.
    pub fn set_decay(&mut self, after: u32, chance: f64) {
        self.decay_after = after.min(250) as u8;
        self.decay_thr = (chance.clamp(0.0, 1.0) * u32::MAX as f64) as u32;
    }

    /// Give every cell a colony, in a handful of Voronoi patches, so a board
    /// that already exists starts out as neighbouring colonies.
    pub fn scatter_colonies<R: Rng>(&mut self, rng: &mut R) {
        if !self.colonies_on() {
            self.colony.fill(0);
            return;
        }
        let (cols, rows, nc) = (self.cols, self.rows, self.ncolonies);
        let k = (cols * rows / 1600).clamp(4, 14);
        let pts: Vec<(usize, usize, u8)> =
            (0..k).map(|_| (rng.gen_range(0..cols), rng.gen_range(0..rows), rng.gen_range(0..nc))).collect();
        for y in 0..rows {
            for x in 0..cols {
                let mut best = (usize::MAX, 0u8);
                for &(px, py, c) in &pts {
                    let (dx, dy) = (x.abs_diff(px), y.abs_diff(py));
                    let (dx, dy) = (dx.min(cols - dx), dy.min(rows - dy));
                    let d = dx * dx + dy * dy;
                    if d < best.0 {
                        best = (d, c);
                    }
                }
                self.colony[y * cols + x] = best.1;
            }
        }
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
        self.settled.fill(0);
    }

    /// Clear, then make each cell alive with probability `density`.
    pub fn seed<R: Rng>(&mut self, rng: &mut R, density: f64) {
        self.clear();
        let d = density.clamp(0.0, 1.0);
        for c in self.cur.iter_mut() {
            *c = rng.gen_bool(d) as u8;
        }
        self.scatter_colonies(rng);
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
        self.colony = copy(&self.colony, old);
        self.next = vec![0; cols * rows];
        self.hist = vec![0; cols * rows];
        self.settled = vec![0; cols * rows];
        self.alt = None; // its mask no longer fits
        self.cols = cols;
        self.rows = rows;
    }

    /// Advance one generation (edges wrap). Returns `true` if any cell changed.
    pub fn step(&mut self) -> bool {
        let changed = match self.rule {
            Rule::LifeLike { birth, survive } if self.alt.is_some() => self.step_lifelike::<true>(birth, survive),
            Rule::LifeLike { birth, survive } => self.step_lifelike::<false>(birth, survive),
            Rule::Brain => self.step_brain(),
        };
        // Age + death-trail + settledness bookkeeping, one pass. Cells that
        // have been stuck in a still life / oscillator for long enough may die
        // here, which is what keeps the board from clogging up for good.
        let tl = self.trail_len;
        let decay = self.decay_after > 0 && self.decay_thr > 0;
        let mut rng = self.rng;
        for i in 0..self.cur.len() {
            let (was, mut now) = (self.cur[i], self.next[i]);
            if decay && was == 1 && now == 1 && self.settled[i] >= self.decay_after && rnd(&mut rng) < self.decay_thr {
                self.next[i] = 0;
                now = 0;
            }
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
            let h = (self.hist[i] << 1) | (now != 0) as u64;
            self.hist[i] = h;
            self.settled[i] = if now == 1 && is_periodic(h) { self.settled[i].saturating_add(1) } else { 0 };
        }
        self.rng = rng;
        std::mem::swap(&mut self.cur, &mut self.next);
        changed
    }

    /// `MIXED` = some cells follow the alternative rule (`self.alt`). The plain
    /// path is compiled without the per-cell lookup, so a board with no ripple
    /// running costs exactly what it did before.
    fn step_lifelike<const MIXED: bool>(&mut self, birth: u16, survive: u16) -> bool {
        let (cols, rows) = (self.cols, self.rows);
        let (cur, next, colony) = (&self.cur, &mut self.next, &mut self.colony);
        let (alt_mask, alt_b, alt_s): (&[u8], u16, u16) = match (MIXED, self.alt.as_ref()) {
            (true, Some(a)) => (&a.mask, a.birth, a.survive),
            _ => (&[], birth, survive),
        };
        let (nc, mut_thr) = (self.ncolonies, self.mutation);
        let mut rng = self.rng;
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
                let (b, sv) = if MIXED && alt_mask[row + x] != 0 { (alt_b, alt_s) } else { (birth, survive) };
                let mask = if alive == 1 { sv } else { b };
                let v = (mask >> n & 1) as u8;
                next[row + x] = v;
                changed |= v != alive;
                if v == 1 && alive == 0 && nc >= 2 {
                    // Births only happen on cells that are dead in `cur`, and
                    // only live cells' colours are ever read, so writing in
                    // place cannot disturb the neighbours of this pass.
                    let nb = [up + l, up + x, up + r, row + l, row + r, down + l, down + x, down + r];
                    let c = inherit(cur, colony, nb, nc, mut_thr, &mut rng);
                    colony[row + x] = c;
                }
            }
        }
        self.rng = rng;
        changed
    }

    fn step_brain(&mut self) -> bool {
        let (cols, rows) = (self.cols, self.rows);
        let (cur, next, colony) = (&self.cur, &mut self.next, &mut self.colony);
        let (nc, mut_thr) = (self.ncolonies, self.mutation);
        let mut rng = self.rng;
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
                if c == 0 && v == 1 && nc >= 2 {
                    let nb = [up + l, up + x, up + r, row + l, row + r, down + l, down + x, down + r];
                    let col = inherit(cur, colony, nb, nc, mut_thr, &mut rng);
                    colony[row + x] = col;
                }
            }
        }
        self.rng = rng;
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

    /// Indices of currently empty cells in a rectangle, each picked with
    /// probability `density`. Used to drip a soup patch in over a few ticks.
    #[cfg(test)]
    pub fn soup_cells<R: Rng>(&self, rng: &mut R, x: usize, y: usize, w: usize, h: usize, density: f64) -> Vec<usize> {
        let d = density.clamp(0.0, 1.0);
        let mut out = Vec::new();
        for yy in y..(y + h).min(self.rows) {
            for xx in x..(x + w).min(self.cols) {
                let i = yy * self.cols + xx;
                if self.cur[i] == 0 && rng.gen_bool(d) {
                    out.push(i);
                }
            }
        }
        out
    }

    /// Make one cell alive (newborn).
    pub fn set_alive(&mut self, i: usize) {
        if i < self.cur.len() {
            self.born(i, self.brush);
        }
    }

    fn born(&mut self, i: usize, colony: u8) {
        self.cur[i] = 1;
        self.age[i] = 0;
        self.trail[i] = 0;
        self.settled[i] = 0;
        self.colony[i] = colony;
    }

    /// Kill one occupied cell, leaving a full-length death trail.
    pub fn kill(&mut self, i: usize) {
        if i < self.cur.len() && self.cur[i] != 0 {
            self.cur[i] = 0;
            self.age[i] = 0;
            self.trail[i] = self.trail_len;
        }
    }

    /// Groups of touching settled cells (8-neighbourhood, edges wrap), i.e.
    /// whole still lifes / oscillators, as lists of cell indices.
    pub fn settled_clusters(&self) -> Vec<Vec<usize>> {
        let (cols, rows) = (self.cols, self.rows);
        let settled = |i: usize| self.cur[i] != 0 && is_periodic(self.hist[i]);
        let mut seen = vec![false; cols * rows];
        let mut out = Vec::new();
        for start in 0..cols * rows {
            if seen[start] || !settled(start) {
                continue;
            }
            seen[start] = true;
            let mut stack = vec![start];
            let mut cluster = Vec::new();
            while let Some(i) = stack.pop() {
                cluster.push(i);
                let (x, y) = (i % cols, i / cols);
                for dy in [rows - 1, 0, 1] {
                    for dx in [cols - 1, 0, 1] {
                        let j = ((y + dy) % rows) * cols + (x + dx) % cols;
                        if !seen[j] && settled(j) {
                            seen[j] = true;
                            stack.push(j);
                        }
                    }
                }
            }
            out.push(cluster);
        }
        out
    }

    /// Replace the board with a saved one (overlapping part only if the
    /// sizes differ). Rule handling is the caller's job.
    pub fn load(&mut self, saved: &crate::state::Saved) {
        self.clear();
        let cc = self.cols.min(saved.cols);
        for y in 0..self.rows.min(saved.rows) {
            let (d, s) = (y * self.cols, y * saved.cols);
            self.cur[d..d + cc].copy_from_slice(&saved.cells[s..s + cc]);
            self.age[d..d + cc].copy_from_slice(&saved.age[s..s + cc]);
            if saved.colony.len() == saved.cells.len() {
                self.colony[d..d + cc].copy_from_slice(&saved.colony[s..s + cc]);
            }
        }
        let nc = self.ncolonies.max(1);
        for c in self.colony.iter_mut() {
            *c %= nc;
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
            self.born(i, self.brush);
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
                    self.born(i, self.brush);
                }
            }
        }
    }

    /// Put `ship` (default heading east, or south-east for the glider) on a
    /// random edge, mirrored/transposed so it flies inwards. Only the ship's
    /// own cells are touched. Returns false if it can't fit.
    pub fn place_ship_on_edge<R: Rng>(&mut self, rng: &mut R, glider: bool, lwss: bool) -> bool {
        if !glider {
            debug_assert!(lwss);
            return self.place_east_ship_on_edge(rng, &patterns::lwss());
        }
        let edge = rng.gen_range(0..4); // 0 top, 1 bottom, 2 left, 3 right
        let flip = rng.gen_bool(0.5);
        let pat = {
            // heads SE; mirror so the heading has a positive component away from the edge
            let (h, v) = match edge {
                0 => (flip, false),
                1 => (flip, true),
                2 => (false, flip),
                _ => (true, flip),
            };
            patterns::glider().transformed(false, h, v)
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

    /// Put an east-heading spaceship (`lwss`, `mwss`, `hwss`) on a random edge,
    /// transposed / mirrored so it flies inwards. Only its own cells are touched.
    pub fn place_east_ship_on_edge<R: Rng>(&mut self, rng: &mut R, ship: &Pattern) -> bool {
        let edge = rng.gen_range(0..4);
        let flip = rng.gen_bool(0.5);
        let pat = oriented(ship, edge, flip);
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

    /// A convoy: up to `n` copies of an east-heading ship entering from one
    /// random edge in parallel lanes, slightly staggered. Returns how many fit.
    pub fn place_convoy<R: Rng>(&mut self, rng: &mut R, ship: &Pattern, n: usize) -> usize {
        let edge = rng.gen_range(0..4);
        let pat = oriented(ship, edge, rng.gen_bool(0.5));
        let (along_len, lane) = if edge < 2 { (self.cols, pat.w + 5) } else { (self.rows, pat.h + 5) };
        let (pw, ph) = (pat.w, pat.h);
        if pw > self.cols || ph > self.rows {
            return 0;
        }
        let n = n.min((along_len / lane).max(1));
        let span = (n - 1) * lane + if edge < 2 { pw } else { ph };
        if span > along_len {
            return 0;
        }
        let start = rng.gen_range(0..=along_len - span);
        let stagger = *[0usize, 2, 4].get(rng.gen_range(0..3)).unwrap_or(&0);
        let mut placed = 0;
        for k in 0..n {
            let along = start + k * lane;
            let depth = k * stagger;
            let (x, y) = match edge {
                0 => (along, depth),
                1 => (along, self.rows.saturating_sub(ph + depth)),
                2 => (depth, along),
                _ => (self.cols.saturating_sub(pw + depth), along),
            };
            if x + pw <= self.cols && y + ph <= self.rows {
                self.place(&pat, x, y, 0);
                placed += 1;
            }
        }
        placed
    }

    /// A glider that starts upstream of (cx, cy) and flies through it, if the
    /// start position is free. False if it could not be placed.
    pub fn place_glider_toward<R: Rng>(&mut self, rng: &mut R, cx: usize, cy: usize) -> bool {
        let (h, v) = (rng.gen_bool(0.5), rng.gen_bool(0.5));
        let (dx, dy) = (if h { -1i64 } else { 1 }, if v { -1i64 } else { 1 });
        let pat = patterns::glider().transformed(false, h, v);
        if pat.w > self.cols || pat.h > self.rows {
            return false;
        }
        let dist = 18i64;
        let sx = (cx as i64 - dx * dist).clamp(0, (self.cols - pat.w) as i64) as usize;
        let sy = (cy as i64 - dy * dist).clamp(0, (self.rows - pat.h) as i64) as usize;
        if self.count_in(sx.saturating_sub(1), sy.saturating_sub(1), pat.w + 2, pat.h + 2) > 0 {
            return false;
        }
        self.place(&pat, sx, sy, 0);
        true
    }

    /// One random disturbance: a live cell either dies or gets a new
    /// neighbour (of its own colony). Knocks oscillators out of phase.
    /// False if no live cell was found.
    pub fn cosmic_ray<R: Rng>(&mut self, rng: &mut R) -> bool {
        let (cols, rows, n) = (self.cols, self.rows, self.cur.len());
        // A few random probes find a live cell on any normally populated board;
        // on a very sparse one fall back to scanning from a random start.
        let mut pick = (0..48).map(|_| rng.gen_range(0..n)).find(|&i| self.cur[i] == 1);
        if pick.is_none() {
            let start = rng.gen_range(0..n);
            pick = (0..n).map(|k| (start + k) % n).find(|&i| self.cur[i] == 1);
        }
        let Some(i) = pick else { return false };
        if rng.gen_bool(0.35) {
            self.kill(i);
            return true;
        }
        let (x, y) = (i % cols, i / cols);
        for _ in 0..16 {
            let (dx, dy) = (rng.gen_range(0..3usize), rng.gen_range(0..3usize));
            if dx == 1 && dy == 1 {
                continue;
            }
            let j = ((y + rows + dy - 1) % rows) * cols + (x + cols + dx - 1) % cols;
            if self.cur[j] == 0 {
                let c = self.colony[i];
                self.born(j, c);
                return true;
            }
        }
        self.kill(i); // boxed in on every side: the ray just kills it
        true
    }

    /// For each `tile` x `tile` block (row-major, `tcols` blocks per row), the
    /// number of live cells that are NOT settled in a still life / oscillator.
    /// Gliders, chaos and anything young count; a block with nothing in it
    /// scores zero, same as one that only holds still lifes.
    pub fn tile_activity(&self, tile: usize, tcols: usize, out: &mut [u32]) {
        out.fill(0);
        let tile = tile.max(1);
        for y in 0..self.rows {
            let base = (y / tile) * tcols;
            for x in 0..self.cols {
                let i = y * self.cols + x;
                if self.cur[i] == 1 && !is_periodic(self.hist[i]) {
                    out[base + x / tile] += 1;
                }
            }
        }
    }

    /// Indices of occupied cells in a rectangle.
    pub fn live_in_rect(&self, x: usize, y: usize, w: usize, h: usize) -> Vec<usize> {
        let mut out = Vec::new();
        for yy in y..(y + h).min(self.rows) {
            for xx in x..(x + w).min(self.cols) {
                let i = yy * self.cols + xx;
                if self.cur[i] != 0 {
                    out.push(i);
                }
            }
        }
        out
    }

    /// Like `soup_cells`, but the patch is mirror / rotation symmetric. Each
    /// symmetry orbit is rolled once, so the pattern really is symmetric.
    pub fn soup_cells_sym<R: Rng>(&self, rng: &mut R, x: usize, y: usize, w: usize, h: usize, density: f64, sym: Sym) -> Vec<usize> {
        if x >= self.cols || y >= self.rows {
            return Vec::new();
        }
        let (w, h) = (w.min(self.cols - x), h.min(self.rows - y));
        let d = density.clamp(0.0, 1.0);
        let mut mark = vec![false; w * h];
        for dy in 0..h {
            for dx in 0..w {
                let mut orbit = [(dx, dy); 4];
                let n = match sym {
                    Sym::None => 1,
                    Sym::Mirror => {
                        orbit[1] = (w - 1 - dx, dy);
                        2
                    }
                    Sym::Flip => {
                        orbit[1] = (dx, h - 1 - dy);
                        2
                    }
                    Sym::Rot => {
                        orbit[1] = (w - 1 - dx, h - 1 - dy);
                        2
                    }
                    Sym::Both => {
                        orbit[1] = (w - 1 - dx, dy);
                        orbit[2] = (dx, h - 1 - dy);
                        orbit[3] = (w - 1 - dx, h - 1 - dy);
                        4
                    }
                };
                let me = dy * w + dx;
                if orbit[..n].iter().all(|&(ox, oy)| oy * w + ox >= me) && rng.gen_bool(d) {
                    for &(ox, oy) in &orbit[..n] {
                        mark[oy * w + ox] = true;
                    }
                }
            }
        }
        let mut out = Vec::new();
        for dy in 0..h {
            for dx in 0..w {
                let i = (y + dy) * self.cols + x + dx;
                if mark[dy * w + dx] && self.cur[i] == 0 {
                    out.push(i);
                }
            }
        }
        out
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
    pub fn place_gun<R: Rng>(&mut self, rng: &mut R) -> Option<(usize, usize, usize, usize)> {
        let (hf, vf) = (rng.gen_bool(0.5), rng.gen_bool(0.5));
        let gun = patterns::gosper_gun().transformed(false, hf, vf);
        let m = 3;
        if gun.w + 2 * m > self.cols || gun.h + 2 * m > self.rows {
            return None;
        }
        let x = if hf { self.cols - gun.w - m } else { m };
        let y = if vf { self.rows - gun.h - m } else { m };
        self.place(&gun, x, y, 2);
        Some((x, y, gun.w, gun.h))
    }
}

/// An east-heading ship turned so that it flies in from `edge`
/// (0 top, 1 bottom, 2 left, 3 right); `flip` mirrors it across its heading.
fn oriented(ship: &Pattern, edge: usize, flip: bool) -> Pattern {
    match edge {
        0 => ship.transformed(true, flip, false),
        1 => ship.transformed(true, flip, true),
        2 => ship.transformed(false, false, flip),
        _ => ship.transformed(false, true, flip),
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
    fn soup_cells_only_pick_empty_cells_and_emptiest_spot_avoids_life() {
        let mut rng = rand::thread_rng();
        let mut l = Life::new(40, 20, 0, CONWAY);
        let first = l.soup_cells(&mut rng, 0, 0, 20, 20, 1.0);
        assert_eq!(first.len(), 400);
        for i in first {
            l.set_alive(i);
        }
        assert_eq!(l.population(), 400);
        assert_eq!(l.soup_cells(&mut rng, 0, 0, 40, 20, 1.0).len(), 400); // only the empty half
        for _ in 0..20 {
            let (x, y) = l.emptiest_spot(&mut rng, 10, 10, 64);
            assert_eq!(l.count_in(x, y, 10, 10), 0);
        }
    }

    #[test]
    fn kill_leaves_a_trail() {
        let mut l = Life::new(6, 6, 5, CONWAY);
        l.set_alive(8);
        l.kill(8);
        assert_eq!((l.cells()[8], l.trail()[8]), (0, 5));
    }

    #[test]
    fn settled_clusters_are_whole_shapes() {
        let mut l = Life::new(40, 40, 0, CONWAY);
        l.place(&Pattern::from_art(&["##", "##"]), 2, 2, 0);
        l.place(&Pattern::from_art(&["##", "##"]), 20, 20, 0);
        l.place(&Pattern::from_art(&["###"]), 10, 30, 0);
        l.place(&patterns::glider(), 30, 5, 0);
        for _ in 0..70 {
            l.step();
        }
        let mut sizes: Vec<usize> = l.settled_clusters().iter().map(Vec::len).collect();
        sizes.sort();
        assert_eq!(sizes, vec![3, 4, 4]); // glider excluded
    }

    #[test]
    fn load_copies_overlap() {
        let saved = crate::state::Saved { cols: 4, rows: 4, rule: "life".into(), cells: (0..16).map(|i| (i == 5) as u8).collect(), age: vec![7; 16], colony: vec![] };
        let mut l = Life::new(8, 2, 0, CONWAY);
        l.load(&saved);
        assert_eq!(l.cells()[8 + 1], 1);
        assert_eq!(l.population(), 1);
        assert_eq!(l.age()[8 + 1], 7);
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

    #[test]
    fn newborns_inherit_the_majority_colony() {
        let mut l = Life::new(8, 8, 0, CONWAY);
        l.set_colonies(4, 0.0);
        l.set_brush(1);
        l.set_alive(8 + 2);
        l.set_alive(2 * 8 + 1);
        l.set_brush(3);
        l.set_alive(8 + 1);
        l.step(); // the L-tromino fills in its fourth corner
        let born = 2 * 8 + 2;
        assert_eq!(l.cells()[born], 1);
        assert_eq!(l.colony()[born], 1, "two parents of colony 1 beat one of colony 3");
        assert_eq!(l.colony()[8 + 2], 1, "survivors keep their colony");
    }

    #[test]
    fn mutation_sometimes_picks_another_colony_and_stays_in_range() {
        let mut l = Life::new(12, 12, 0, CONWAY);
        l.set_colonies(3, 1.0); // always mutate
        l.set_brush(0);
        for i in [5 * 12 + 4, 5 * 12 + 5, 5 * 12 + 6] {
            l.set_alive(i);
        }
        let mut seen = [false; 3];
        for _ in 0..40 {
            l.step();
            for (i, &c) in l.cells().iter().enumerate() {
                if c == 1 {
                    assert!(l.colony()[i] < 3);
                    seen[l.colony()[i] as usize] = true;
                }
            }
            if l.population() == 0 {
                break;
            }
        }
        assert!(seen[1] || seen[2], "mutation never fired");
    }

    #[test]
    fn colonies_off_costs_nothing_and_changes_nothing() {
        let mut a = Life::new(30, 30, 0, CONWAY);
        let mut b = Life::new(30, 30, 0, CONWAY);
        b.set_colonies(5, 0.1);
        let mut rng = rand::thread_rng();
        a.seed(&mut rng, 0.3);
        for i in 0..900 {
            if a.cells()[i] == 1 {
                b.set_alive(i);
            }
        }
        for _ in 0..50 {
            a.step();
            b.step();
        }
        assert_eq!(a.cells(), b.cells(), "colours must never influence the rules");
    }

    #[test]
    fn decay_only_hits_cells_stuck_in_settled_shapes() {
        let mut l = Life::new(60, 40, 0, CONWAY);
        l.place(&Pattern::from_art(&["##", "##"]), 5, 5, 0); // still life
        l.place(&Pattern::from_art(&["###"]), 20, 20, 0); // oscillator
        l.place(&patterns::glider(), 40, 5, 0); // keeps moving
        l.set_decay(10, 1.0);
        for _ in 0..40 {
            l.step();
        }
        assert_eq!(l.population(), 12 - 0 + 0, "nothing decays before the 64-generation history shows it");
        for _ in 0..100 {
            l.step();
        }
        // block and blinker are gone, the glider is still flying
        assert_eq!(l.population(), 5);
    }

    #[test]
    fn decay_off_by_default() {
        let mut l = Life::new(20, 20, 0, CONWAY);
        l.place(&Pattern::from_art(&["##", "##"]), 5, 5, 0);
        for _ in 0..400 {
            l.step();
        }
        assert_eq!(l.population(), 4);
    }

    #[test]
    fn cosmic_rays_change_the_board_and_need_a_live_cell() {
        let mut rng = rand::thread_rng();
        let mut l = Life::new(20, 20, 3, CONWAY);
        assert!(!l.cosmic_ray(&mut rng), "empty board has nothing to disturb");
        l.place(&Pattern::from_art(&["##", "##"]), 8, 8, 0);
        assert!(l.cosmic_ray(&mut rng));
        assert_ne!(l.population(), 4);
    }

    #[test]
    fn symmetric_soups_are_symmetric() {
        let mut rng = rand::thread_rng();
        let l = Life::new(60, 40, 0, CONWAY);
        let (x, y, w, h) = (10, 6, 21, 15);
        for sym in SYMMETRIES {
            let cells = l.soup_cells_sym(&mut rng, x, y, w, h, 0.4, sym);
            let set: std::collections::HashSet<usize> = cells.iter().copied().collect();
            assert!(set.len() > 20, "{sym:?} produced {}", set.len());
            for &i in &cells {
                let (dx, dy) = (i % 60 - x, i / 60 - y);
                let mirrors: Vec<(usize, usize)> = match sym {
                    Sym::Mirror => vec![(w - 1 - dx, dy)],
                    Sym::Flip => vec![(dx, h - 1 - dy)],
                    Sym::Rot => vec![(w - 1 - dx, h - 1 - dy)],
                    Sym::Both => vec![(w - 1 - dx, dy), (dx, h - 1 - dy), (w - 1 - dx, h - 1 - dy)],
                    Sym::None => vec![],
                };
                for (mx, my) in mirrors {
                    assert!(set.contains(&((y + my) * 60 + x + mx)), "{sym:?}: cell ({dx},{dy}) has no mirror");
                }
            }
        }
        // no symmetry: just an ordinary soup, and it only picks empty cells
        let plain = l.soup_cells_sym(&mut rng, x, y, w, h, 1.0, Sym::None);
        assert_eq!(plain.len(), w * h);
    }

    #[test]
    fn tile_activity_ignores_settled_shapes_and_counts_movers() {
        let mut l = Life::new(64, 32, 0, CONWAY);
        l.place(&Pattern::from_art(&["##", "##"]), 4, 4, 0); // tile 0: block
        l.place(&patterns::glider(), 40, 4, 0); // tile 2 (x 32..48): glider
        for _ in 0..70 {
            l.step();
        }
        let mut out = vec![0u32; 4 * 2];
        l.tile_activity(16, 4, &mut out);
        assert_eq!(out[0], 0, "a block is settled");
        assert!(out[1..4].iter().sum::<u32>() + out[4..].iter().sum::<u32>() >= 5, "the glider counts: {out:?}");
    }

    #[test]
    fn convoys_arrive_in_parallel_lanes_and_fly_inwards() {
        let mut rng = rand::thread_rng();
        for _ in 0..200 {
            let mut l = Life::new(120, 80, 0, CONWAY);
            let n = l.place_convoy(&mut rng, &patterns::mwss(), 3);
            assert!((1..=3).contains(&n));
            assert_eq!(l.population(), n * 11, "ships must not overlap");
        }
        // a lone LWSS on the left edge heads east: after 40 generations it is further right
        let mut l = Life::new(80, 40, 0, CONWAY);
        l.place(&patterns::lwss(), 0, 10, 0);
        for _ in 0..40 {
            l.step();
        }
        let min_x = (0..80 * 40).filter(|&i| l.cells()[i] == 1).map(|i| i % 80).min().unwrap();
        assert!(min_x >= 15, "ship flew east: {min_x}");
    }

    #[test]
    fn edge_ships_of_every_kind_fly_inwards() {
        let mut rng = rand::thread_rng();
        for ship in [patterns::lwss(), patterns::mwss(), patterns::hwss()] {
            for _ in 0..100 {
                let mut l = Life::new(80, 60, 0, CONWAY);
                assert!(l.place_east_ship_on_edge(&mut rng, &ship));
                let c0 = centroid(&l);
                for _ in 0..40 {
                    l.step();
                }
                let c1 = centroid(&l);
                // flying inwards means moving towards the middle of the board
                let mid = (40.0, 30.0);
                let d = |c: (f64, f64)| ((c.0 - mid.0).powi(2) + (c.1 - mid.1).powi(2)).sqrt();
                assert!(d(c1) < d(c0), "ship moved away from the centre: {c0:?} -> {c1:?}");
            }
        }
    }

    fn centroid(l: &Life) -> (f64, f64) {
        let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
        for i in 0..l.cols() * l.rows() {
            if l.cells()[i] == 1 {
                sx += (i % l.cols()) as f64;
                sy += (i / l.cols()) as f64;
                n += 1.0;
            }
        }
        (sx / n, sy / n)
    }

    #[test]
    fn aimed_gliders_pass_through_their_target() {
        let mut rng = rand::thread_rng();
        let mut hits = 0;
        for _ in 0..40 {
            let mut l = Life::new(120, 80, 0, CONWAY);
            if !l.place_glider_toward(&mut rng, 60, 40) {
                continue;
            }
            let mut passed = false;
            for _ in 0..120 {
                l.step();
                passed |= l.count_in(55, 35, 11, 11) > 0;
            }
            assert!(passed, "glider missed its target");
            hits += 1;
        }
        assert!(hits > 30);
    }

    #[test]
    fn rect_helpers_and_gun_rect() {
        let mut rng = rand::thread_rng();
        let mut l = Life::new(120, 80, 0, CONWAY);
        let (x, y, w, h) = l.place_gun(&mut rng).expect("gun fits");
        assert_eq!((w, h), (36, 9));
        assert_eq!(l.live_in_rect(x, y, w, h).len(), 36);
        let tiny = Life::new(10, 10, 0, CONWAY);
        assert!(tiny.live_in_rect(0, 0, 50, 50).is_empty());
    }

    #[test]
    fn brian_births_inherit_colony_too() {
        let mut l = Life::new(10, 10, 0, Rule::Brain);
        l.set_colonies(3, 0.0);
        l.set_brush(2);
        l.set_alive(5 * 10 + 4);
        l.set_alive(5 * 10 + 5);
        l.step();
        for (i, &c) in l.cells().iter().enumerate() {
            if c == 1 {
                assert_eq!(l.colony()[i], 2);
            }
        }
        assert!(l.population() > 0);
    }

    #[test]
    fn scatter_makes_patches_and_resize_keeps_colours() {
        let mut rng = rand::thread_rng();
        let mut l = Life::new(160, 90, 0, CONWAY);
        l.set_colonies(6, 0.0);
        l.scatter_colonies(&mut rng);
        let mut seen = [0usize; 8];
        for &c in l.colony() {
            seen[c as usize] += 1;
        }
        assert!(seen.iter().filter(|&&n| n > 0).count() >= 2);
        let before = l.colony()[3 * 160 + 3];
        l.resize(170, 95);
        assert_eq!(l.colony().len(), 170 * 95);
        assert_eq!(l.colony()[3 * 170 + 3], before);
        assert!(l.set_colonies(0, 0.0) == false);
        assert!(l.set_colonies(4, 0.0), "switching colonies on is reported so the caller can scatter");
    }
}
