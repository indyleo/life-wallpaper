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
        for (i, c) in [p.newborn, p.mature, p.old, p.ancient]
            .into_iter()
            .enumerate()
        {
            pal[IDX_AGE0 as usize + i] = c;
        }
        pal[IDX_DYING as usize] = p.dying;
        pal[IDX_FIXED as usize] = live;
        pal[IDX_OVERLAY as usize] = p.overlay;
        for i in 0..TRAIL_STEPS {
            pal[IDX_TRAIL0 as usize + i] =
                blend(live, p.bg, 0.5 * (i + 1) as f32 / TRAIL_STEPS as f32);
        }
        for i in 0..RAIN_STEPS {
            pal[IDX_RAIN0 as usize + i] =
                blend(p.rain, p.bg, 0.5 * (i + 1) as f32 / RAIN_STEPS as f32);
        }
        Self {
            pal,
            font: Font::build(),
            fixed,
            round: cfg.cell_round,
        }
    }

    fn key(&self, idx: u8, glyph: u8) -> Key {
        pack(self.pal[idx as usize], glyph)
    }

    /// Build display keys for one frame. `out.len() == cols * rows`.
    pub fn compose(&self, sim: &Sim, cfg: &Config, hz: f64, out: &mut [Key]) {
        let life = &sim.life;
        let (cells, age, trail) = (life.cells(), life.age(), life.trail());
        let tl = life.trail_len().max(1) as usize;
        let rain = sim.rain.as_ref();

        for i in 0..out.len() {
            out[i] = match cells[i] {
                1 => {
                    if self.fixed {
                        self.key(IDX_FIXED, 0)
                    } else {
                        let a = age[i];
                        let class = AGE_STEPS.iter().take_while(|&&t| a >= t).count();
                        self.key(IDX_AGE0 + (class as u8 + sim.heat).min(3), 0)
                    }
                }
                2 => self.key(IDX_DYING, 0),
                _ => {
                    let t = trail[i] as usize;
                    if t > 0 {
                        let step = (t * TRAIL_STEPS)
                            .div_ceil(tl)
                            .saturating_sub(1)
                            .min(TRAIL_STEPS - 1);
                        self.key(IDX_TRAIL0 + step as u8, 0)
                    } else if let Some(r) = rain.filter(|r| r.level()[i] > 0) {
                        let l = r.level()[i] as usize;
                        let step = (l * RAIN_STEPS)
                            .div_ceil(RAIN_LEVELS as usize)
                            .saturating_sub(1)
                            .min(RAIN_STEPS - 1);
                        self.key(IDX_RAIN0 + step as u8, r.glyph()[i])
                    } else {
                        self.key(IDX_BG, 0)
                    }
                }
            };
        }

        if cfg.overlay {
            let (cols, rows) = (life.cols(), life.rows());
            let text = format!(
                "gen {} pop {} {} {:.0}hz",
                sim.gen,
                life.population(),
                life.rule().name(),
                hz
            );
            let row = if rows >= 3 { rows - 2 } else { 0 };
            for (n, c) in text.bytes().enumerate() {
                let col = 1 + n;
                if col >= cols {
                    break;
                }
                let g = if self.font.has(c) && c != b' ' { c } else { 0 };
                // A blank overlay cell is a plain bg cell, so the line reads as one block of text.
                out[row * cols + col] = if g == 0 {
                    self.key(IDX_BG, 0)
                } else {
                    self.key(IDX_OVERLAY, g)
                };
            }
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
        let gap = if glyph == 0 {
            t.gap.min(t.cell.saturating_sub(1))
        } else {
            0
        };
        let side = t.cell - gap;
        let lead = gap / 2; // gap is split between the leading and trailing edge
                            // ~0.4 of the side: at 7-8px that trims the corner pixels, on big cells it is a real curve
        let radius = if self.round && glyph == 0 {
            ((side as f32 * 0.4).ceil() as usize).clamp(1, (side / 2).max(1))
        } else {
            0
        };
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
    let dx = if lx < r {
        r - lx
    } else if lx > s - r {
        lx - (s - r)
    } else {
        0.0
    };
    let dy = if ly < r {
        r - ly
    } else if ly > s - r {
        ly - (s - r)
    } else {
        0.0
    };
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
        Self {
            from: vec![0; n],
            to: vec![0; n],
            changing: Vec::new(),
            start: now,
            dur: Duration::ZERO,
            active: false,
        }
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
        let x = now.saturating_duration_since(self.start).as_secs_f32()
            / self.dur.as_secs_f32().max(1e-3);
        let t = x.clamp(0.0, 1.0);
        let e = t * t * (3.0 - 2.0 * t); // smoothstep
        for &i in &self.changing {
            display[i] = if t >= 1.0 {
                self.to[i]
            } else {
                blend_keys(self.from[i], self.to[i], e)
            };
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

        let mut t = Target {
            canvas: &mut canvas,
            w,
            h,
            cell,
            gap: 0,
            cols,
            rows,
        };
        r.draw(&mut t, &display, &mut shown, &mut presented, &mut dmg);
        assert_eq!(dmg.len(), rows); // first frame: every row damaged

        display[cols + 1] = r.key(IDX_AGE0, 0);
        let mut t = Target {
            canvas: &mut canvas,
            w,
            h,
            cell,
            gap: 0,
            cols,
            rows,
        };
        r.draw(&mut t, &display, &mut shown, &mut presented, &mut dmg);
        assert_eq!(dmg, vec![[8, 8, 8, 8]]);

        // Nothing changed: nothing damaged.
        let mut t = Target {
            canvas: &mut canvas,
            w,
            h,
            cell,
            gap: 0,
            cols,
            rows,
        };
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
            let cfg = Config {
                cell_round: round,
                color: ColorMode::Fixed(live),
                ..Config::default()
            };
            let r = Renderer::new(&cfg);
            let mut canvas = vec![0u8; w * h * 4];
            let mut t = Target {
                canvas: &mut canvas,
                w,
                h,
                cell,
                gap,
                cols: 2,
                rows: 1,
            };
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
        let cfg = Config {
            overlay: true,
            ..Config::default()
        };
        let r = Renderer::new(&cfg);
        let sim = Sim::new(60, 10, Instant::now(), &cfg);
        let mut out = vec![0; 600];
        r.compose(&sim, &cfg, 4.0, &mut out);
        let newborn = pack(cfg.palette.newborn, 0);
        assert!(out.contains(&newborn)); // newborn soup
        assert!(out
            .iter()
            .any(|&k| key_glyph(k) != 0 && key_rgb(k) == cfg.palette.overlay));
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
        let (a, b, c) = (
            pack([0, 0, 0], 0),
            pack([100, 100, 100], 0),
            pack([200, 200, 200], 0),
        );
        let mut display = vec![a];
        let mut f = Fade::new(1, t0);
        f.target_mut()[0] = b;
        f.begin(&mut display, t0, Duration::from_millis(100), true);
        f.step(t0 + Duration::from_millis(50), &mut display);
        let shown = display[0];
        f.target_mut()[0] = c;
        f.begin(
            &mut display,
            t0 + Duration::from_millis(50),
            Duration::from_millis(100),
            true,
        );
        assert_eq!(display[0], shown); // no jump when a new generation lands mid-fade
        f.begin(&mut display, t0, Duration::from_millis(100), false);
        assert_eq!(display[0], c);
        assert!(!f.active());
    }
}
