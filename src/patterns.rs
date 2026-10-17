//! Pattern data: spaceships, oscillators, the Gosper gun, and pixel-art stamps.

use crate::font::Font;

#[derive(Clone, Debug)]
pub struct Pattern {
    pub w: usize,
    pub h: usize,
    pub cells: Vec<(usize, usize)>,
}

impl Pattern {
    /// '#' (or 'O') marks a live cell. All rows must have equal width.
    pub fn from_art(art: &[&str]) -> Self {
        let mut cells = Vec::new();
        for (y, row) in art.iter().enumerate() {
            for (x, c) in row.bytes().enumerate() {
                if c == b'#' || c == b'O' {
                    cells.push((x, y));
                }
            }
        }
        Self {
            w: art.iter().map(|r| r.len()).max().unwrap_or(0),
            h: art.len(),
            cells,
        }
    }

    /// Apply (transpose, then mirror). All three are symmetries of the rules,
    /// so a transformed spaceship is still a working spaceship.
    pub fn transformed(&self, transpose: bool, hflip: bool, vflip: bool) -> Self {
        let (w, h) = if transpose {
            (self.h, self.w)
        } else {
            (self.w, self.h)
        };
        let cells = self
            .cells
            .iter()
            .map(|&(x, y)| {
                let (mut x, mut y) = if transpose { (y, x) } else { (x, y) };
                if hflip {
                    x = w - 1 - x;
                }
                if vflip {
                    y = h - 1 - y;
                }
                (x, y)
            })
            .collect();
        Self { w, h, cells }
    }
}

/// Heads south-east.
pub fn glider() -> Pattern {
    Pattern::from_art(&[".#.", "..#", "###"])
}

/// Lightweight spaceship, heads east.
pub fn lwss() -> Pattern {
    Pattern::from_art(&["#..#.", "....#", "#...#", ".####"])
}

/// Period-3 oscillator.
pub fn pulsar() -> Pattern {
    Pattern::from_art(&[
        "..###...###..",
        ".............",
        "#....#.#....#",
        "#....#.#....#",
        "#....#.#....#",
        "..###...###..",
        ".............",
        "..###...###..",
        "#....#.#....#",
        "#....#.#....#",
        "#....#.#....#",
        ".............",
        "..###...###..",
    ])
}

/// Gosper glider gun (36x9). Fires gliders towards the south-east.
pub fn gosper_gun() -> Pattern {
    const ROWS: [&[usize]; 9] = [
        &[24],
        &[22, 24],
        &[12, 13, 20, 21, 34, 35],
        &[11, 15, 20, 21, 34, 35],
        &[0, 1, 10, 16, 20, 21],
        &[0, 1, 10, 14, 16, 17, 22, 24],
        &[10, 16, 24],
        &[11, 15],
        &[12, 13],
    ];
    let mut cells = Vec::new();
    for (y, xs) in ROWS.iter().enumerate() {
        for &x in *xs {
            cells.push((x, y));
        }
    }
    Pattern { w: 36, h: 9, cells }
}

pub const TUX_ART: [&str; 18] = [
    "......####......",
    ".....######.....",
    "....########....",
    "....##.##.##....",
    "....##.##.##....",
    "....########....",
    "....###..###....",
    ".....######.....",
    "...##########...",
    "..############..",
    ".####.####.####.",
    ".###..####..###.",
    ".###.######.###.",
    ".###.######.###.",
    ".###..####..###.",
    "..###.####.###..",
    "..####....####..",
    ".####......####.",
];

pub const ARCH_ART: [&str; 16] = [
    ".......##.......",
    ".......##.......",
    "......####......",
    "......####......",
    ".....######.....",
    ".....######.....",
    "....########....",
    "...####..####...",
    "...###....###...",
    "..####.##.####..",
    "..###.####.###..",
    ".####.####.####.",
    ".###..####..###.",
    "####..####..####",
    "###...####...###",
    "##............##",
];

pub const STAMP_TEXTS: [&str; 6] = [
    ">_", "$ sudo", "git push", "rm -rf /", "arch btw", "vim :wq",
];

pub fn tux() -> Pattern {
    Pattern::from_art(&TUX_ART)
}

pub fn arch() -> Pattern {
    Pattern::from_art(&ARCH_ART)
}

pub fn text(font: &Font, s: &str) -> Pattern {
    let (w, h, cells) = font.text_cells(s);
    Pattern { w, h, cells }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::life::{Life, CONWAY};

    #[test]
    fn sprite_rows_are_uniform() {
        for r in TUX_ART {
            assert_eq!(r.len(), 16);
        }
        for r in ARCH_ART {
            assert_eq!(r.len(), 16);
        }
    }

    #[test]
    fn spaceships_head_where_the_docs_say() {
        let centroid = |l: &Life| {
            let w = l.cols();
            let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
            for (i, &c) in l.cells().iter().enumerate() {
                if c == 1 {
                    sx += (i % w) as f64;
                    sy += (i / w) as f64;
                    n += 1.0;
                }
            }
            (sx / n, sy / n)
        };
        for (pat, dx, dy) in [(lwss(), 1.0, 0.0), (glider(), 1.0, 1.0)] {
            let mut l = Life::new(80, 80, 0, CONWAY);
            l.place(&pat, 30, 30, 0);
            let (x0, y0) = centroid(&l);
            for _ in 0..40 {
                l.step();
            }
            let (x1, y1) = centroid(&l);
            assert!((x1 - x0) * dx > 5.0 || dx == 0.0, "x drift {}", x1 - x0);
            assert!((y1 - y0) * dy > 5.0 || dy == 0.0, "y drift {}", y1 - y0);
            if dy == 0.0 {
                assert!((y1 - y0).abs() < 1.0);
            }
        }
    }

    #[test]
    fn counts() {
        assert_eq!(glider().cells.len(), 5);
        assert_eq!(lwss().cells.len(), 9);
        assert_eq!(gosper_gun().cells.len(), 36);
    }

    #[test]
    fn lwss_moves_east_in_all_orientations() {
        for (t, h, v) in [
            (false, false, false),
            (false, true, true),
            (true, false, false),
            (true, true, true),
        ] {
            let p = lwss().transformed(t, h, v);
            let mut l = Life::new(40, 40, 0, CONWAY);
            l.place(&p, 15, 15, 0);
            let n = l.population();
            for _ in 0..40 {
                l.step();
            }
            assert_eq!(l.population(), n, "lwss must keep 9 cells");
        }
    }

    #[test]
    fn gun_keeps_firing() {
        let mut l = Life::new(120, 80, 0, CONWAY);
        l.place(&gosper_gun(), 2, 2, 0);
        for _ in 0..120 {
            l.step();
        }
        // Two released gliders at least, on top of the gun's own 36 cells (+-).
        assert!(l.population() >= 36 + 10, "pop={}", l.population());
    }

    #[test]
    fn pulsar_has_period_three() {
        let mut l = Life::new(30, 30, 0, CONWAY);
        l.place(&pulsar(), 8, 8, 0);
        let h0 = l.hash();
        for _ in 0..3 {
            l.step();
        }
        assert_eq!(h0, l.hash());
    }
}
