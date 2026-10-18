//! CPU rendering. Each cell is reduced to a 32-bit *display key*
//! (top byte = ASCII glyph or 0, low 24 bits = RGB). Frames are drawn by
//! diffing keys: only cells whose key differs from what a given buffer
//! already shows are repainted, and only cells that differ from what the
//! compositor last saw are reported as damage.
//!
//! Between generations `Fade` crossfades the keys of the cells that changed,
//! so births and deaths ease in and out instead of popping.

use std::time::{Duration, Instant};

use crate::config::{ColorMode, Config, Rgb};
use crate::controller::{Sim, RAIN_LEVELS};
use crate::font::Font;
use crate::look::{rotate_hue, Look};

pub type Key = u32;
pub const SENTINEL: Key = u32::MAX;

const IDX_BG: u8 = 0;
const IDX_AGE0: u8 = 1; // newborn, mature, old, ancient
const IDX_DYING: u8 = 5;
const IDX_FIXED: u8 = 6;
const IDX_OVERLAY: u8 = 7;
const IDX_TRAIL0: u8 = 8;
const TRAIL_STEPS: usize = 8;
const IDX_RAIN0: u8 = 16;
const RAIN_STEPS: usize = 6;

/// Colonies shade by age too: newborn cells are lightened, old ones sink
/// towards the background. Fractions of the colony colour that are kept.
const COLONY_LIGHT: Rgb = [0xeb, 0xdb, 0xb2];

/// Glow: how strongly nearby live cells tint an empty cell, and the ceiling,
/// so a halo never looks like a live cell.
const GLOW_GAIN: f32 = 3.0;
const GLOW_CAP: f32 = 0.42;

/// Generations before a cell counts as mature / old / ancient.
const AGE_STEPS: [u8; 3] = [3, 12, 60];

/// A crossfade finishes after this fraction of the generation interval,
/// so each state is held briefly before the next one starts to arrive.
const FADE_FRACTION: f64 = 0.85;
const FADE_MAX: Duration = Duration::from_millis(1000);

pub fn fade_duration(interval: Duration) -> Duration {
    interval.mul_f64(FADE_FRACTION).min(FADE_MAX)
}

fn pack(c: Rgb, glyph: u8) -> Key {
    (glyph as u32) << 24 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32
}

fn key_rgb(k: Key) -> Rgb {
    [(k >> 16) as u8, (k >> 8) as u8, k as u8]
}

fn key_glyph(k: Key) -> u8 {
    (k >> 24) as u8
}

fn blend(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 * t + y as f32 * (1.0 - t)).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

/// Colour moves from `from` to `to` as `t` goes 0 -> 1; the glyph is always
/// the destination's (glyph cells are rain and the overlay line).
pub fn blend_keys(from: Key, to: Key, t: f32) -> Key {
    pack(blend(key_rgb(to), key_rgb(from), t), key_glyph(to))
}

pub struct Renderer {
    pal: Vec<Rgb>,
    font: Font,
    fixed: bool,
    round: bool,
    colony_cols: Vec<Rgb>,
}

/// Palette with the current `Look` (hue drift, night dimming) applied.
struct Table {
    pal: Vec<Rgb>,
    /// Per colony: newborn, mature, old, ancient.
    col_live: Vec<[Rgb; 4]>,
    col_trail: Vec<[Rgb; TRAIL_STEPS]>,
}

impl Renderer {
    pub fn new(cfg: &Config) -> Self {
        let p = &cfg.palette;
        let (fixed, live) = match cfg.color {
            ColorMode::Age => (false, p.mature),
            ColorMode::Fixed(c) => (true, c),
        };
        let mut pal = vec![[0u8; 3]; 22];
        pal[IDX_BG as usize] = p.bg;
        for (i, c) in [p.newborn, p.mature, p.old, p.ancient].into_iter().enumerate() {
            pal[IDX_AGE0 as usize + i] = c;
        }
        pal[IDX_DYING as usize] = p.dying;
        pal[IDX_FIXED as usize] = live;
        pal[IDX_OVERLAY as usize] = p.overlay;
        for i in 0..TRAIL_STEPS {
            pal[IDX_TRAIL0 as usize + i] = blend(live, p.bg, 0.5 * (i + 1) as f32 / TRAIL_STEPS as f32);
        }
        for i in 0..RAIN_STEPS {
            pal[IDX_RAIN0 as usize + i] = blend(p.rain, p.bg, 0.5 * (i + 1) as f32 / RAIN_STEPS as f32);
        }
        let colony_cols = cfg.palette.colonies.iter().copied().take(8).collect();
        Self { pal, font: Font::build(), fixed, round: cfg.cell_round, colony_cols }
    }

    /// The palette as it should look right now. Background and overlay text
    /// stay put; everything else follows hue drift and night dimming.
    fn table(&self, look: Look) -> Table {
        let bg = self.pal[IDX_BG as usize];
        let adj = |c: Rgb| -> Rgb {
            let c = rotate_hue(c, look.hue_deg());
            if look.dim > 0 {
                blend(c, bg, 1.0 - look.dim_frac())
            } else {
                c
            }
        };
        let mut pal = self.pal.clone();
        for (i, c) in pal.iter_mut().enumerate() {
            if i != IDX_BG as usize && i != IDX_OVERLAY as usize {
                *c = adj(*c);
            }
        }
        let col_live = self
            .colony_cols
            .iter()
            .map(|&c| [adj(blend(c, COLONY_LIGHT, 0.7)), adj(c), adj(blend(c, bg, 0.82)), adj(blend(c, bg, 0.65))])
            .collect();
        let col_trail = self
            .colony_cols
            .iter()
            .map(|&c| {
                let mut t = [[0u8; 3]; TRAIL_STEPS];
                for (i, slot) in t.iter_mut().enumerate() {
                    *slot = adj(blend(c, bg, 0.5 * (i + 1) as f32 / TRAIL_STEPS as f32));
                }
                t
            })
            .collect();
        Table { pal, col_live, col_trail }
    }

    fn key(&self, idx: u8, glyph: u8) -> Key {
        pack(self.pal[idx as usize], glyph)
    }

    /// Build display keys for one frame. `out.len() == cols * rows`.
    pub fn compose(&self, sim: &Sim, cfg: &Config, hz: f64, look: Look, out: &mut [Key]) {
        let life = &sim.life;
        let (cols, rows) = (life.cols(), life.rows());
        let (cells, age, trail, colony) = (life.cells(), life.age(), life.trail(), life.colony());
        let tl = life.trail_len().max(1) as usize;
        let rain = sim.rain.as_ref();
        let t = self.table(look);
        let ncol = t.col_live.len();
        let use_col = cfg.colonies && !self.fixed && life.colonies_on() && ncol >= 2;
        let key = |idx: u8, glyph: u8| pack(t.pal[idx as usize], glyph);

        for i in 0..out.len() {
            out[i] = match cells[i] {
                1 => {
                    let class = AGE_STEPS.iter().take_while(|&&s| age[i] >= s).count();
                    if use_col {
                        pack(t.col_live[colony[i] as usize % ncol][class], 0)
                    } else if self.fixed {
                        key(IDX_FIXED, 0)
                    } else {
                        key(IDX_AGE0 + (class as u8 + sim.heat).min(3), 0)
                    }
                }
                2 => key(IDX_DYING, 0),
                _ => {
                    let tr = trail[i] as usize;
                    if tr > 0 {
                        let step = (tr * TRAIL_STEPS).div_ceil(tl).saturating_sub(1).min(TRAIL_STEPS - 1);
                        if use_col {
                            pack(t.col_trail[colony[i] as usize % ncol][step], 0)
                        } else {
                            key(IDX_TRAIL0 + step as u8, 0)
                        }
                    } else if let Some(r) = rain.filter(|r| r.level()[i] > 0) {
                        let l = r.level()[i] as usize;
                        let step = (l * RAIN_STEPS).div_ceil(RAIN_LEVELS as usize).saturating_sub(1).min(RAIN_STEPS - 1);
                        key(IDX_RAIN0 + step as u8, r.glyph()[i])
                    } else {
                        key(IDX_BG, 0)
                    }
                }
            };
        }

        if cfg.vignette > 0.0 {
            self.apply_vignette(out, cols, rows, cfg.vignette as f32);
        }
        if cfg.glow > 0.0 {
            self.add_glow(out, cells, cols, rows, cfg.glow as f32);
        }

        if cfg.overlay {
            let text = format!("gen {} pop {} {} {:.0}hz", sim.gen, life.population(), life.rule().name(), hz);
            let row = if rows >= 3 { rows - 2 } else { 0 };
            for (n, c) in text.bytes().enumerate() {
                let col = 1 + n;
                if col >= cols {
                    break;
                }
                let g = if self.font.has(c) && c != b' ' { c } else { 0 };
                // A blank overlay cell is a plain bg cell, so the line reads as one block of text.
                out[row * cols + col] = if g == 0 { self.key(IDX_BG, 0) } else { self.key(IDX_OVERLAY, g) };
            }
        }
    }

    /// Dim everything that is not background towards the background the
    /// further it is from the centre of the screen.
    fn apply_vignette(&self, out: &mut [Key], cols: usize, rows: usize, depth: f32) {
        let bg = self.pal[IDX_BG as usize];
        let bg_key = pack(bg, 0);
        let (hc, hr) = (cols as f32 / 2.0, rows as f32 / 2.0);
        for y in 0..rows {
            let ny = (y as f32 + 0.5 - hr) / hr;
            for x in 0..cols {
                let i = y * cols + x;
                if out[i] == bg_key {
                    continue;
                }
                let nx = (x as f32 + 0.5 - hc) / hc;
                let r = ((nx * nx + ny * ny) / 2.0).sqrt();
                let e = ((r - 0.45) / 0.55).clamp(0.0, 1.0);
                let f = 1.0 - depth * e * e * (3.0 - 2.0 * e);
                if f < 0.999 {
                    out[i] = pack(blend(key_rgb(out[i]), bg, f), key_glyph(out[i]));
                }
            }
        }
    }

    /// A soft halo: empty cells next to live ones take a little of their
    /// colour (a 5-tap blur of the live cells, at cell resolution). Crossfades
    /// like any other key, so halos fade in and out with the cells.
    fn add_glow(&self, out: &mut [Key], cells: &[u8], cols: usize, rows: usize, amount: f32) {
        let n = cols * rows;
        let mut ch = [vec![0f32; n], vec![0f32; n], vec![0f32; n], vec![0f32; n]]; // weight, r, g, b
        let mut any = false;
        for i in 0..n {
            if cells[i] == 1 {
                let c = key_rgb(out[i]);
                ch[0][i] = 1.0;
                for k in 0..3 {
                    ch[k + 1][i] = c[k] as f32;
                }
                any = true;
            }
        }
        if !any {
            return;
        }
        let mut tmp = vec![0f32; n];
        for plane in ch.iter_mut() {
            blur5(plane, &mut tmp, cols, rows);
        }
        for i in 0..n {
            let w = ch[0][i];
            if cells[i] == 1 || w < 0.004 || key_glyph(out[i]) != 0 {
                continue;
            }
            let avg = [(ch[1][i] / w).round() as u8, (ch[2][i] / w).round() as u8, (ch[3][i] / w).round() as u8];
            let s = (amount * GLOW_GAIN * w).min(GLOW_CAP);
            out[i] = pack(blend(avg, key_rgb(out[i]), s), 0);
        }
    }

    fn draw_cell(&self, t: &mut Target, cx: usize, cy: usize, key: Key) {
        let c = key_rgb(key);
        let color = [c[2], c[1], c[0], 0xFF]; // XRGB8888, little endian: B G R X
        let b = self.pal[IDX_BG as usize];
        let bg = [b[2], b[1], b[0], 0xFF];
        let glyph = key_glyph(key);
        let (x0, y0) = (cx * t.cell, cy * t.cell);
        let (x1, y1) = ((x0 + t.cell).min(t.w), (y0 + t.cell).min(t.h));
        let stride = t.w * 4;
        // Glyph cells (rain, overlay text) keep the full cell so text stays
        // legible; plain cells are an inset (optionally rounded) square.
        let gap = if glyph == 0 { t.gap.min(t.cell.saturating_sub(1)) } else { 0 };
        let side = t.cell - gap;
        let lead = gap / 2; // gap is split between the leading and trailing edge
        // ~0.4 of the side: at 7-8px that trims the corner pixels, on big cells it is a real curve
        let radius = if self.round && glyph == 0 { ((side as f32 * 0.4).ceil() as usize).clamp(1, (side / 2).max(1)) } else { 0 };
        for y in y0..y1 {
            let row = &mut t.canvas[y * stride..(y + 1) * stride];
            for x in x0..x1 {
                let on = if glyph != 0 {
                    self.font.pixel(glyph, x - x0, y - y0, t.cell)
                } else {
                    in_square(x - x0, y - y0, lead, side, radius)
                };
                row[x * 4..x * 4 + 4].copy_from_slice(if on { &color } else { &bg });
            }
        }
    }

    /// Repaint differing cells into `t`, update `shown` (what this buffer
    /// holds) and `presented` (what the compositor will show), and collect
    /// damage rectangles in buffer coordinates.
    pub fn draw(
        &self,
        t: &mut Target,
        display: &[Key],
        shown: &mut [Key],
        presented: &mut [Key],
        damage: &mut Vec<[i32; 4]>,
    ) {
        damage.clear();
        for cy in 0..t.rows {
            let (mut lo, mut hi) = (usize::MAX, 0usize);
            for cx in 0..t.cols {
                let i = cy * t.cols + cx;
                let k = display[i];
                if shown[i] != k {
                    self.draw_cell(t, cx, cy, k);
                    shown[i] = k;
                }
                if presented[i] != k {
                    presented[i] = k;
                    lo = lo.min(cx);
                    hi = cx;
                }
            }
            if lo != usize::MAX {
                let x = lo * t.cell;
                let y = cy * t.cell;
                let w = ((hi + 1) * t.cell).min(t.w).saturating_sub(x);
                let h = ((cy + 1) * t.cell).min(t.h).saturating_sub(y);
                damage.push([x as i32, y as i32, w as i32, h as i32]);
            }
        }
        // Too many requests cost more than they save: send one bounding box.
        if damage.len() > 64 {
            let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, 0, 0);
            for d in damage.iter() {
                x0 = x0.min(d[0]);
                y0 = y0.min(d[1]);
                x1 = x1.max(d[0] + d[2]);
                y1 = y1.max(d[1] + d[3]);
            }
            damage.clear();
            damage.push([x0, y0, x1 - x0, y1 - y0]);
        }
    }
}

/// In-place separable [1 4 6 4 1]/16 blur of a `cols` x `rows` plane; outside
/// the board counts as zero. `tmp` is scratch of the same size.
fn blur5(p: &mut [f32], tmp: &mut [f32], cols: usize, rows: usize) {
    const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    for y in 0..rows {
        let row = y * cols;
        for x in 0..cols {
            let mut acc = 0.0;
            for (k, w) in K.iter().enumerate() {
                let xx = x as isize + k as isize - 2;
                if xx >= 0 && (xx as usize) < cols {
                    acc += w * p[row + xx as usize];
                }
            }
            tmp[row + x] = acc;
        }
    }
    for y in 0..rows {
        for x in 0..cols {
            let mut acc = 0.0;
            for (k, w) in K.iter().enumerate() {
                let yy = y as isize + k as isize - 2;
                if yy >= 0 && (yy as usize) < rows {
                    acc += w * tmp[yy as usize * cols + x];
                }
            }
            p[y * cols + x] = acc;
        }
    }
}

/// True if pixel (x, y) of a cell lies inside the `side`-wide square that
/// starts `lead` pixels in from the cell's top-left corner, with corners
/// rounded to `radius` (0 = sharp).
fn in_square(x: usize, y: usize, lead: usize, side: usize, radius: usize) -> bool {
    if x < lead || y < lead || x >= lead + side || y >= lead + side {
        return false;
    }
    if radius == 0 {
        return true;
    }
    let (lx, ly) = ((x - lead) as f32 + 0.5, (y - lead) as f32 + 0.5);
    let (s, r) = (side as f32, radius as f32);
    // distance past the corner-circle centre, per axis (0 inside the straight part)
    let dx = if lx < r { r - lx } else if lx > s - r { lx - (s - r) } else { 0.0 };
    let dy = if ly < r { r - ly } else if ly > s - r { ly - (s - r) } else { 0.0 };
    dx * dx + dy * dy <= r * r
}

/// Crossfade state for one wall. Compose the new generation into
/// `target_mut()`, call `begin`, then call `step` on every frame; `step`
/// only touches the cells that actually change and does nothing once the
/// fade is over, so an idle board costs next to nothing.
pub struct Fade {
    from: Vec<Key>,
    to: Vec<Key>,
    changing: Vec<usize>,
    start: Instant,
    dur: Duration,
    active: bool,
}

impl Fade {
    pub fn new(n: usize, now: Instant) -> Self {
        Self { from: vec![0; n], to: vec![0; n], changing: Vec::new(), start: now, dur: Duration::ZERO, active: false }
    }

    pub fn target_mut(&mut self) -> &mut [Key] {
        &mut self.to
    }

    /// Show the composed target immediately (no fade).
    pub fn snap(&mut self, display: &mut [Key]) {
        display.copy_from_slice(&self.to);
        self.changing.clear();
        self.active = false;
    }

    /// Start fading from whatever `display` currently shows to the target.
    /// A fade still in progress is picked up from its current colours.
    pub fn begin(&mut self, display: &mut [Key], now: Instant, dur: Duration, smooth: bool) {
        self.changing.clear();
        for (i, (&d, &t)) in display.iter().zip(&self.to).enumerate() {
            if d != t {
                self.changing.push(i);
            }
        }
        if !smooth || dur.is_zero() {
            for &i in &self.changing {
                display[i] = self.to[i];
            }
            self.changing.clear();
            self.active = false;
            return;
        }
        self.from.copy_from_slice(display);
        self.start = now;
        self.dur = dur;
        self.active = !self.changing.is_empty();
    }

    /// Advance the crossfade; true if `display` was modified.
    pub fn step(&mut self, now: Instant, display: &mut [Key]) -> bool {
        if !self.active {
            return false;
        }
        let x = now.saturating_duration_since(self.start).as_secs_f32() / self.dur.as_secs_f32().max(1e-3);
        let t = x.clamp(0.0, 1.0);
        let e = t * t * (3.0 - 2.0 * t); // smoothstep
        for &i in &self.changing {
            display[i] = if t >= 1.0 { self.to[i] } else { blend_keys(self.from[i], self.to[i], e) };
        }
        if t >= 1.0 {
            self.active = false;
            self.changing.clear();
        }
        true
    }

    #[cfg(test)]
    fn active(&self) -> bool {
        self.active
    }
}

/// Geometry of the pixel buffer being drawn into.
pub struct Target<'a> {
    pub canvas: &'a mut [u8],
    pub w: usize,
    pub h: usize,
    pub cell: usize,
    pub gap: usize,
    pub cols: usize,
    pub rows: usize,
}

/// Grid size for a pixel surface (rounded up so edge cells are drawn clipped).
pub fn grid_dims(w: usize, h: usize, cell: usize) -> (usize, usize) {
    (w.div_ceil(cell).max(1), h.div_ceil(cell).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn diff_draw_only_touches_changed_cells() {
        let cfg = Config::default();
        let r = Renderer::new(&cfg);
        let (w, h, cell) = (30usize, 20usize, 8usize);
        let (cols, rows) = grid_dims(w, h, cell);
        let mut canvas = vec![0u8; w * h * 4];
        let mut shown = vec![SENTINEL; cols * rows];
        let mut presented = vec![SENTINEL; cols * rows];
        let mut display = vec![r.key(IDX_BG, 0); cols * rows];
        let mut dmg = Vec::new();

        let mut t = Target { canvas: &mut canvas, w, h, cell, gap: 0, cols, rows };
        r.draw(&mut t, &display, &mut shown, &mut presented, &mut dmg);
        assert_eq!(dmg.len(), rows); // first frame: every row damaged

        display[cols + 1] = r.key(IDX_AGE0, 0);
        let mut t = Target { canvas: &mut canvas, w, h, cell, gap: 0, cols, rows };
        r.draw(&mut t, &display, &mut shown, &mut presented, &mut dmg);
        assert_eq!(dmg, vec![[8, 8, 8, 8]]);

        // Nothing changed: nothing damaged.
        let mut t = Target { canvas: &mut canvas, w, h, cell, gap: 0, cols, rows };
        r.draw(&mut t, &display, &mut shown, &mut presented, &mut dmg);
        assert!(dmg.is_empty());
    }

    fn pixel(canvas: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
        let i = (y * w + x) * 4;
        [canvas[i + 2], canvas[i + 1], canvas[i]]
    }

    #[test]
    fn gap_and_rounding_shape_the_cell() {
        let (w, h, cell) = (16usize, 8usize, 8usize);
        let live = [200u8, 100, 50];
        let draw = |gap: usize, round: bool| {
            let cfg = Config { cell_round: round, color: ColorMode::Fixed(live), ..Config::default() };
            let r = Renderer::new(&cfg);
            let mut canvas = vec![0u8; w * h * 4];
            let mut t = Target { canvas: &mut canvas, w, h, cell, gap, cols: 2, rows: 1 };
            r.draw_cell(&mut t, 0, 0, r.key(IDX_FIXED, 0));
            (canvas, r.pal[IDX_BG as usize])
        };

        let (c, _) = draw(0, false); // old look: solid block
        assert!((0..8).all(|i| pixel(&c, w, i, 0) == live && pixel(&c, w, 7, i) == live));

        let (c, bg) = draw(2, false); // one pixel of gap on every side
        assert_eq!(pixel(&c, w, 0, 4), bg);
        assert_eq!(pixel(&c, w, 1, 4), live);
        assert_eq!(pixel(&c, w, 6, 4), live);
        assert_eq!(pixel(&c, w, 7, 4), bg);
        assert_eq!(pixel(&c, w, 1, 1), live); // corners are still sharp

        let (c, bg) = draw(1, true); // 7px square, corners clipped
        assert_eq!(pixel(&c, w, 0, 0), bg); // corner rounded off
        assert_eq!(pixel(&c, w, 3, 0), live); // edge middle filled
        assert_eq!(pixel(&c, w, 7, 3), bg); // trailing-edge pixel is the gap
        assert_eq!(pixel(&c, w, 3, 3), live);
        let (c, _) = draw(0, true);
        assert_ne!(pixel(&c, w, 0, 0), live); // corner pixel rounded off
        assert_eq!(pixel(&c, w, 3, 0), live); // edges stay filled
    }

    #[test]
    fn compose_produces_age_colors_and_overlay() {
        let cfg = Config { overlay: true, colonies: false, vignette: 0.0, glow: 0.0, ..Config::default() };
        let r = Renderer::new(&cfg);
        let sim = Sim::new(60, 10, Instant::now(), &cfg);
        let mut out = vec![0; 600];
        r.compose(&sim, &cfg, 4.0, Look::default(), &mut out);
        let newborn = pack(cfg.palette.newborn, 0);
        assert!(out.iter().any(|&k| k == newborn)); // newborn soup
        assert!(out.iter().any(|&k| key_glyph(k) != 0 && key_rgb(k) == cfg.palette.overlay));
    }

    fn plain_cfg() -> Config {
        Config { colonies: false, vignette: 0.0, glow: 0.0, trail: false, overlay: false, ..Config::default() }
    }

    fn one_cell_sim(cols: usize, rows: usize, at: (usize, usize), cfg: &Config) -> Sim {
        let mut sim = Sim::new(cols, rows, Instant::now(), cfg);
        sim.life.clear();
        sim.life.set_alive(at.1 * cols + at.0);
        sim
    }

    #[test]
    fn glow_tints_neighbours_but_not_far_cells_or_live_ones() {
        let cfg = Config { glow: 0.5, ..plain_cfg() };
        let r = Renderer::new(&cfg);
        let (cols, rows) = (21, 11);
        let sim = one_cell_sim(cols, rows, (10, 5), &cfg);
        let mut out = vec![0; cols * rows];
        r.compose(&sim, &cfg, 4.0, Look::default(), &mut out);
        let bg = pack(cfg.palette.bg, 0);
        let at = |x: usize, y: usize| out[y * cols + x];
        assert_ne!(at(11, 5), bg, "next to the live cell");
        assert_ne!(at(12, 5), bg, "two cells away still glows a little");
        assert!(key_rgb(at(11, 5))[0] > key_rgb(at(12, 5))[0].min(key_rgb(at(11, 5))[0]) || at(11, 5) != at(12, 5));
        assert_eq!(at(15, 5), bg, "far away stays background");
        assert_eq!(at(0, 0), bg);
        let live = pack(cfg.palette.newborn, 0);
        assert_eq!(at(10, 5), live, "live cells are untouched");
        // the halo must never be as bright as the cell itself
        let halo = key_rgb(at(11, 5));
        assert!(halo[0] < key_rgb(live)[0] && halo[1] < key_rgb(live)[1]);

        let off = Config { glow: 0.0, ..plain_cfg() };
        let mut out2 = vec![0; cols * rows];
        Renderer::new(&off).compose(&sim, &off, 4.0, Look::default(), &mut out2);
        assert_eq!(out2[5 * cols + 11], bg);
    }

    #[test]
    fn vignette_dims_corners_but_not_the_centre_or_background() {
        let cfg = Config { vignette: 0.5, ..plain_cfg() };
        let r = Renderer::new(&cfg);
        let (cols, rows) = (40, 20);
        let mut sim = Sim::new(cols, rows, Instant::now(), &cfg);
        sim.life.clear();
        for (x, y) in [(20, 10), (0, 0), (39, 19)] {
            sim.life.set_alive(y * cols + x);
        }
        let mut out = vec![0; cols * rows];
        r.compose(&sim, &cfg, 4.0, Look::default(), &mut out);
        let full = pack(cfg.palette.newborn, 0);
        assert_eq!(out[10 * cols + 20], full);
        let corner = key_rgb(out[0]);
        let bg = cfg.palette.bg;
        assert!(corner[0] < key_rgb(full)[0] && corner[0] >= bg[0], "corner is dimmed towards bg: {corner:?}");
        assert_eq!(out[5 * cols + 5], pack(bg, 0));
    }

    #[test]
    fn colonies_draw_in_colony_colours_with_age_shading() {
        let cfg = Config { colonies: true, ..plain_cfg() };
        let r = Renderer::new(&cfg);
        let (cols, rows) = (8, 4);
        let mut sim = Sim::new(cols, rows, Instant::now(), &cfg);
        sim.life.clear();
        sim.life.set_brush(1);
        sim.life.set_alive(0);
        sim.life.set_brush(2);
        sim.life.set_alive(1);
        let mut out = vec![0; cols * rows];
        r.compose(&sim, &cfg, 4.0, Look::default(), &mut out);
        assert_ne!(out[0], out[1], "different colonies, different colours");
        let c1 = key_rgb(out[0]);
        let base = cfg.palette.colonies[1];
        assert!(c1[0] >= base[0] || c1[1] >= base[1], "newborn is the lightened colony colour");
        let off = Config { colonies: false, ..plain_cfg() };
        let mut out2 = vec![0; cols * rows];
        Renderer::new(&off).compose(&sim, &off, 4.0, Look::default(), &mut out2);
        assert_eq!(out2[0], out2[1], "without colonies both are plain newborn colour");
    }

    #[test]
    fn look_shifts_cell_colours_but_never_the_background() {
        let cfg = plain_cfg();
        let r = Renderer::new(&cfg);
        let sim = one_cell_sim(9, 5, (4, 2), &cfg);
        let mut a = vec![0; 45];
        let mut b = vec![0; 45];
        r.compose(&sim, &cfg, 4.0, Look::default(), &mut a);
        r.compose(&sim, &cfg, 4.0, Look { hue: 40, dim: 16 }, &mut b);
        assert_ne!(a[2 * 9 + 4], b[2 * 9 + 4]);
        assert_eq!(a[0], b[0], "background is not tinted");
        let dimmed = key_rgb(b[2 * 9 + 4]);
        let rotated = rotate_hue(cfg.palette.newborn, 40.0);
        assert!(dimmed[0] < rotated[0] || dimmed[1] < rotated[1], "night dimming darkens");
    }

    #[test]
    fn blur_keeps_total_weight_inside_the_board() {
        let (cols, rows) = (15, 15);
        let mut p = vec![0f32; cols * rows];
        p[7 * cols + 7] = 1.0;
        let mut tmp = vec![0f32; cols * rows];
        blur5(&mut p, &mut tmp, cols, rows);
        let sum: f32 = p.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4, "{sum}");
        assert!(p[7 * cols + 7] > p[7 * cols + 8] && p[7 * cols + 8] > p[7 * cols + 9]);
    }

    #[test]
    fn blend_keys_endpoints_and_midpoint() {
        let a = pack([0, 0, 0], 0);
        let b = pack([200, 100, 50], b'A');
        assert_eq!(blend_keys(a, b, 0.0), pack([0, 0, 0], b'A'));
        assert_eq!(blend_keys(a, b, 1.0), b);
        assert_eq!(key_rgb(blend_keys(a, b, 0.5)), [100, 50, 25]);
    }

    #[test]
    fn fade_eases_changed_cells_and_then_goes_idle() {
        let t0 = Instant::now();
        let (bg, live) = (pack([0, 0, 0], 0), pack([200, 200, 200], 0));
        let mut display = vec![bg; 4];
        let mut f = Fade::new(4, t0);
        f.target_mut().copy_from_slice(&[bg, live, bg, bg]);
        f.begin(&mut display, t0, Duration::from_millis(200), true);
        assert!(f.active());
        assert_eq!(display[1], bg); // nothing shown yet

        assert!(f.step(t0 + Duration::from_millis(100), &mut display));
        let mid = key_rgb(display[1])[0];
        assert!(mid > 60 && mid < 140, "mid-fade colour {mid}");
        assert_eq!((display[0], display[2], display[3]), (bg, bg, bg));

        assert!(f.step(t0 + Duration::from_millis(250), &mut display));
        assert_eq!(display[1], live);
        assert!(!f.active());
        assert!(!f.step(t0 + Duration::from_millis(300), &mut display)); // idle: no work
    }

    #[test]
    fn fade_restarts_from_what_is_shown_and_smooth_off_snaps() {
        let t0 = Instant::now();
        let (a, b, c) = (pack([0, 0, 0], 0), pack([100, 100, 100], 0), pack([200, 200, 200], 0));
        let mut display = vec![a];
        let mut f = Fade::new(1, t0);
        f.target_mut()[0] = b;
        f.begin(&mut display, t0, Duration::from_millis(100), true);
        f.step(t0 + Duration::from_millis(50), &mut display);
        let shown = display[0];
        f.target_mut()[0] = c;
        f.begin(&mut display, t0 + Duration::from_millis(50), Duration::from_millis(100), true);
        assert_eq!(display[0], shown); // no jump when a new generation lands mid-fade
        f.begin(&mut display, t0, Duration::from_millis(100), false);
        assert_eq!(display[0], c);
        assert!(!f.active());
    }

    /// Not a test: `LW_PPM=/tmp/out.ppm cargo test --release dump_frame -- --ignored`
    /// renders a few minutes of a default board to a PPM image.
    #[test]
    #[ignore]
    fn dump_frame() {
        use std::{io::Write, time::Duration};
        let path = std::env::var("LW_PPM").unwrap_or_else(|_| "/tmp/frame.ppm".into());
        let mut cfg = Config { fps: 8.0, ..Config::default() };
        if std::env::var("LW_NOGLOW").is_ok() {
            cfg.glow = 0.0;
            cfg.vignette = 0.0;
        }
        let (w, h, cell) = (1280usize, 720usize, 8usize);
        let (cols, rows) = grid_dims(w, h, cell);
        let t0 = Instant::now();
        let mut sim = Sim::new(cols, rows, t0, &cfg);
        let mut now = t0;
        for _ in 0..(8 * 150) {
            now += Duration::from_millis(125);
            sim.tick(now, &cfg, 0.0);
        }
        let r = Renderer::new(&cfg);
        let mut keys = vec![0; cols * rows];
        r.compose(&sim, &cfg, 8.0, Look::default(), &mut keys);
        let mut canvas = vec![0u8; w * h * 4];
        let mut shown = vec![SENTINEL; cols * rows];
        let mut presented = vec![SENTINEL; cols * rows];
        let mut dmg = Vec::new();
        let mut t = Target { canvas: &mut canvas, w, h, cell, gap: cfg.cell_gap, cols, rows };
        r.draw(&mut t, &keys, &mut shown, &mut presented, &mut dmg);
        let mut f = std::fs::File::create(path).unwrap();
        write!(f, "P6\n{w} {h}\n255\n").unwrap();
        let rgb: Vec<u8> = canvas.chunks(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
        f.write_all(&rgb).unwrap();
    }
}
