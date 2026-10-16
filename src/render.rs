//! CPU rendering. Each cell is reduced to a 16-bit *display key*
//! (low byte = palette index, high byte = ASCII glyph or 0). Frames are drawn
//! by diffing keys: only cells whose key differs from what a given buffer
//! already shows are repainted, and only cells that differ from what the
//! compositor last saw are reported as damage.

use crate::config::{ColorMode, Config, Rgb};
use crate::controller::{Sim, RAIN_LEVELS};
use crate::font::Font;

pub const SENTINEL: u16 = 0xFFFF;

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

pub struct Renderer {
    pal: Vec<[u8; 4]>,
    font: Font,
    fixed: bool,
}

fn px(c: Rgb) -> [u8; 4] {
    [c[2], c[1], c[0], 0xFF] // XRGB8888, little endian: B G R X
}

fn blend(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 * t + y as f32 * (1.0 - t)).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

impl Renderer {
    pub fn new(cfg: &Config) -> Self {
        let p = &cfg.palette;
        let (fixed, live) = match cfg.color {
            ColorMode::Age => (false, p.mature),
            ColorMode::Fixed(c) => (true, c),
        };
        let mut pal = vec![[0u8; 4]; 22];
        pal[IDX_BG as usize] = px(p.bg);
        for (i, c) in [p.newborn, p.mature, p.old, p.ancient].into_iter().enumerate() {
            pal[IDX_AGE0 as usize + i] = px(c);
        }
        pal[IDX_DYING as usize] = px(p.dying);
        pal[IDX_FIXED as usize] = px(live);
        pal[IDX_OVERLAY as usize] = px(p.overlay);
        for i in 0..TRAIL_STEPS {
            pal[IDX_TRAIL0 as usize + i] = px(blend(live, p.bg, 0.5 * (i + 1) as f32 / TRAIL_STEPS as f32));
        }
        for i in 0..RAIN_STEPS {
            pal[IDX_RAIN0 as usize + i] = px(blend(p.rain, p.bg, 0.5 * (i + 1) as f32 / RAIN_STEPS as f32));
        }
        Self { pal, font: Font::build(), fixed }
    }

    /// Build display keys for one frame. `out.len() == cols * rows`.
    pub fn compose(&self, sim: &Sim, cfg: &Config, hz: f64, out: &mut [u16]) {
        let life = &sim.life;
        let (cells, age, trail) = (life.cells(), life.age(), life.trail());
        let tl = life.trail_len().max(1) as usize;
        let rain = sim.rain.as_ref();

        for i in 0..out.len() {
            out[i] = match cells[i] {
                1 => {
                    if self.fixed {
                        IDX_FIXED as u16
                    } else {
                        let a = age[i];
                        let class = AGE_STEPS.iter().take_while(|&&t| a >= t).count();
                        (IDX_AGE0 + (class as u8 + sim.heat).min(3)) as u16
                    }
                }
                2 => IDX_DYING as u16,
                _ => {
                    let t = trail[i] as usize;
                    if t > 0 {
                        let step = (t * TRAIL_STEPS).div_ceil(tl).saturating_sub(1).min(TRAIL_STEPS - 1);
                        (IDX_TRAIL0 as usize + step) as u16
                    } else if let Some(r) = rain.filter(|r| r.level()[i] > 0) {
                        let l = r.level()[i] as usize;
                        let step = (l * RAIN_STEPS).div_ceil(RAIN_LEVELS as usize).saturating_sub(1).min(RAIN_STEPS - 1);
                        (IDX_RAIN0 as usize + step) as u16 | (r.glyph()[i] as u16) << 8
                    } else {
                        IDX_BG as u16
                    }
                }
            };
        }

        if cfg.overlay {
            let (cols, rows) = (life.cols(), life.rows());
            let text = format!("gen {} pop {} {} {:.0}hz", sim.gen, life.population(), life.rule().name(), hz);
            let row = if rows >= 3 { rows - 2 } else { 0 };
            for (n, c) in text.bytes().enumerate() {
                let col = 1 + n;
                if col >= cols {
                    break;
                }
                let g = if self.font.has(c) && c != b' ' { c as u16 } else { 0 };
                // A blank overlay cell is a plain bg cell, so the line reads as one block of text.
                out[row * cols + col] = if g == 0 { IDX_BG as u16 } else { IDX_OVERLAY as u16 | g << 8 };
            }
        }
    }

    fn draw_cell(&self, t: &mut Target, cx: usize, cy: usize, key: u16) {
        let color = self.pal[(key & 0xFF) as usize];
        let bg = self.pal[IDX_BG as usize];
        let glyph = (key >> 8) as u8;
        let (x0, y0) = (cx * t.cell, cy * t.cell);
        let (x1, y1) = ((x0 + t.cell).min(t.w), (y0 + t.cell).min(t.h));
        let stride = t.w * 4;
        for y in y0..y1 {
            let row = &mut t.canvas[y * stride..(y + 1) * stride];
            for x in x0..x1 {
                let on = glyph == 0 || self.font.pixel(glyph, x - x0, y - y0, t.cell);
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
        display: &[u16],
        shown: &mut [u16],
        presented: &mut [u16],
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

/// Geometry of the pixel buffer being drawn into.
pub struct Target<'a> {
    pub canvas: &'a mut [u8],
    pub w: usize,
    pub h: usize,
    pub cell: usize,
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
        let mut display = vec![0u16; cols * rows];
        let mut dmg = Vec::new();

        let mut t = Target { canvas: &mut canvas, w, h, cell, cols, rows };
        r.draw(&mut t, &display, &mut shown, &mut presented, &mut dmg);
        assert_eq!(dmg.len(), rows); // first frame: every row damaged

        display[cols + 1] = IDX_AGE0 as u16;
        let mut t = Target { canvas: &mut canvas, w, h, cell, cols, rows };
        r.draw(&mut t, &display, &mut shown, &mut presented, &mut dmg);
        assert_eq!(dmg, vec![[8, 8, 8, 8]]);

        // Nothing changed: nothing damaged.
        let mut t = Target { canvas: &mut canvas, w, h, cell, cols, rows };
        r.draw(&mut t, &display, &mut shown, &mut presented, &mut dmg);
        assert!(dmg.is_empty());
    }

    #[test]
    fn compose_produces_age_colors_and_overlay() {
        let cfg = Config { overlay: true, ..Config::default() };
        let r = Renderer::new(&cfg);
        let sim = Sim::new(60, 10, Instant::now(), &cfg);
        let mut out = vec![0u16; 600];
        r.compose(&sim, &cfg, 4.0, &mut out);
        assert!(out.iter().any(|&k| k & 0xFF == IDX_AGE0 as u16)); // newborn soup
        assert!(out.iter().any(|&k| k >> 8 != 0 && k & 0xFF == IDX_OVERLAY as u16));
    }
}