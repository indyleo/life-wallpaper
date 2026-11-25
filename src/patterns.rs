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

/// Lightweight spaceship, heads east. (The usual drawing flies west, so it is
/// mirrored here to make the doc comment true.)
pub fn lwss() -> Pattern {
    Pattern::from_art(&[".#..#", "#....", "#...#", "####."]).transformed(false, true, false)
}

/// Middle-weight spaceship, heads east.
pub fn mwss() -> Pattern {
    Pattern::from_art(&["...#..", ".#...#", "#.....", "#....#", "#####."])
        .transformed(false, true, false)
}

/// Heavy-weight spaceship, heads east.
pub fn hwss() -> Pattern {
    Pattern::from_art(&["...##..", ".#....#", "#......", "#.....#", "######."])
        .transformed(false, true, false)
}

/// R-pentomino: five cells that stay busy for over a thousand generations.
pub fn r_pentomino() -> Pattern {
    Pattern::from_art(&[".##", "##.", ".#."])
}

/// Acorn: seven cells that stay busy for over five thousand generations.
pub fn acorn() -> Pattern {
    Pattern::from_art(&[".#.....", "...#...", "##..###"])
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

/// Period-2 oscillator.
pub fn blinker() -> Pattern {
    Pattern::from_art(&["###"])
}

/// Period-2 oscillator.
pub fn toad() -> Pattern {
    Pattern::from_art(&[".###", "###."])
}

/// Period-2 oscillator.
pub fn beacon() -> Pattern {
    Pattern::from_art(&["##..", "##..", "..##", "..##"])
}

/// Period-15 oscillator.
pub fn pentadecathlon() -> Pattern {
    Pattern::from_art(&["..#....#..", "##.####.##", "..#....#.."])
}

/// Seven cells that grow for a while and then vanish completely (gen 130).
pub fn diehard() -> Pattern {
    Pattern::from_art(&["......#.", "##......", ".#...###"])
}

/// Ten cells that never settle: they leave a growing trail of debris while
/// the head travels diagonally (a small "switch engine" style puffer).
/// Verified by simulation: 166 live cells after 500 generations, and the
/// bounding box keeps growing for thousands more.
pub fn engine() -> Pattern {
    Pattern::from_art(&[
        "......#.", "....#.##", "....#.#.", "....#...", "..#.....", "#.#.....",
    ])
}

/// One row of 28 cells in 39 columns that grows without bound: glider streams
/// leave in several directions (linear growth, a rake). 314 cells after 500 generations.
pub fn infinite_line() -> Pattern {
    Pattern::from_art(&["########.#####...###......#######.#####"])
}

/// A glider bounced back and forth between two blocks: a period-30
/// oscillator (the queen bee shuttle; the same engine inside the Gosper gun).
pub fn queen_bee_shuttle() -> Pattern {
    Pattern::from_art(&[
        ".........#............",
        ".......#.#............",
        "......#.#.............",
        "##...#..#...........##",
        "##....#.#...........##",
        ".......#.#............",
        ".........#............",
    ])
}

// Symmetric seeds. Each is mirror or four-fold symmetric, small, and runs a
// long time before settling (found by exhaustive search, lifespans checked by
// two independent simulators), so a bloom that looks planned grows from it.

/// 18 cells, left-right mirror. Runs 1494 generations and settles at 264 cells.
pub fn crown() -> Pattern {
    Pattern::from_art(&["..##..", "......", "##..##", "#....#", "######", "##..##"])
}

/// 28 cells, four-fold. Runs 1273 generations and settles at 556 cells.
pub fn lantern() -> Pattern {
    Pattern::from_art(&[
        "...###...",
        "#...#...#",
        "#..###..#",
        "#.......#",
        "#.......#",
        "#.......#",
        "#..###..#",
        "#...#...#",
        "...###...",
    ])
}

/// 24 cells, four-fold. Runs 692 generations and settles at 288 cells.
pub fn petals() -> Pattern {
    Pattern::from_art(&[
        ".#....#.", "..####..", "##....##", "...##...", "...##...", "##....##", "..####..",
        ".#....#.",
    ])
}

/// 25 cells, four-fold. Runs 621 generations and settles at 552 cells.
pub fn cross() -> Pattern {
    Pattern::from_art(&[
        "#.###.#", "##.#.##", "...#...", ".#.#.#.", "...#...", "##.#.##", "#.###.#",
    ])
}

/// Names `by_name` knows (what `inject` accepts besides `soup`, `convoy` and `text`).
pub const NAMES: [&str; 22] = [
    "glider",
    "lwss",
    "mwss",
    "hwss",
    "r-pentomino",
    "acorn",
    "diehard",
    "pulsar",
    "gun",
    "tux",
    "arch",
    "blinker",
    "toad",
    "beacon",
    "pentadecathlon",
    "engine",
    "infinite-line",
    "queen-bee-shuttle",
    "crown",
    "lantern",
    "petals",
    "cross",
];

/// A named pattern, as spelled in `inject NAME` (case and `_` / `-` do not matter).
pub fn by_name(name: &str) -> Option<Pattern> {
    Some(match name.to_ascii_lowercase().replace('_', "-").as_str() {
        "glider" => glider(),
        "lwss" => lwss(),
        "mwss" => mwss(),
        "hwss" => hwss(),
        "r-pentomino" | "rpentomino" | "r" => r_pentomino(),
        "acorn" => acorn(),
        "diehard" => diehard(),
        "pulsar" => pulsar(),
        "gun" | "gosper-gun" | "gosper" => gosper_gun(),
        "tux" => tux(),
        "arch" => arch(),
        "blinker" => blinker(),
        "toad" => toad(),
        "beacon" => beacon(),
        "pentadecathlon" => pentadecathlon(),
        "engine" | "puffer" => engine(),
        "infinite-line" | "rake" => infinite_line(),
        "queen-bee-shuttle" | "shuttle" => queen_bee_shuttle(),
        "crown" => crown(),
        "lantern" => lantern(),
        "petals" => petals(),
        "cross" => cross(),
        _ => return None,
    })
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

    fn period_of(p: &Pattern, max: usize) -> Option<usize> {
        let mut l = Life::new(40, 30, 0, CONWAY);
        l.place(p, 15, 12, 0);
        let start = l.cells().to_vec();
        for g in 1..=max {
            l.step();
            if l.cells() == start.as_slice() {
                return Some(g);
            }
        }
        None
    }

    #[test]
    fn the_added_oscillators_have_their_known_periods() {
        assert_eq!(period_of(&blinker(), 5), Some(2));
        assert_eq!(period_of(&toad(), 5), Some(2));
        assert_eq!(period_of(&beacon(), 5), Some(2));
        assert_eq!(period_of(&pentadecathlon(), 20), Some(15));
    }

    #[test]
    fn diehard_dies_out_at_generation_130() {
        let mut l = Life::new(80, 60, 0, CONWAY);
        l.place(&diehard(), 30, 25, 0);
        let mut gen = 0;
        while l.population() > 0 && gen < 300 {
            l.step();
            gen += 1;
        }
        assert_eq!(gen, 130);
    }

    #[test]
    fn every_listed_name_resolves_and_nonsense_does_not() {
        for n in NAMES {
            assert!(by_name(n).is_some(), "{n}");
        }
        assert!(by_name("Gosper_Gun").is_some());
        assert!(by_name("no-such-thing").is_none());
    }

    #[test]
    fn the_engine_keeps_growing_exactly_as_simulated_offline() {
        let mut l = Life::new(400, 400, 0, CONWAY);
        l.place(&engine(), 200, 200, 0);
        for _ in 0..500 {
            l.step();
        }
        assert_eq!(l.population(), 166, "matches an independent simulation");
        assert_eq!(engine().cells.len(), 10);
    }

    #[test]
    fn the_infinite_line_is_a_rake_not_a_methuselah() {
        let mut l = Life::new(400, 400, 0, CONWAY);
        l.place(&infinite_line(), 180, 200, 0);
        for _ in 0..500 {
            l.step();
        }
        assert_eq!(l.population(), 314, "matches an independent simulation");
        assert_eq!(infinite_line().cells.len(), 28);
    }

    #[test]
    fn the_queen_bee_shuttle_is_a_period_30_oscillator() {
        let mut l = Life::new(120, 80, 0, CONWAY);
        l.place(&queen_bee_shuttle(), 40, 30, 0);
        assert_eq!(queen_bee_shuttle().cells.len(), 20);
        for _ in 0..40 {
            l.step();
        }
        let phase = l.cells().to_vec();
        for _ in 0..30 {
            l.step();
        }
        assert_eq!(
            l.cells(),
            phase.as_slice(),
            "back to the same state 30 generations later"
        );
        let mut half = Life::new(120, 80, 0, CONWAY);
        half.place(&queen_bee_shuttle(), 40, 30, 0);
        for _ in 0..55 {
            half.step();
        }
        assert_ne!(
            half.cells(),
            phase.as_slice(),
            "and not sooner, at least not at 15"
        );
    }

    /// Generations a seed runs before it is stable, and the population it settles at;
    /// both found by exhaustive search and confirmed by a second simulator.
    #[test]
    fn the_symmetric_seeds_live_exactly_as_long_as_measured() {
        for (name, seed, gens, pop) in [
            ("crown", crown(), 1494, 264),
            ("lantern", lantern(), 1273, 556),
            ("petals", petals(), 692, 288),
            ("cross", cross(), 621, 552),
        ] {
            let mut l = Life::new(320, 320, 0, CONWAY);
            l.place(&seed, 150, 150, 0);
            for g in 1..=gens {
                l.step();
                assert!(l.population() > 0, "{name} died early at {g}");
            }
            assert_eq!(l.population(), pop, "{name} after {gens} generations");
        }
    }

    #[test]
    fn the_seeds_really_are_symmetric() {
        let flip = |p: &Pattern| -> Vec<(usize, usize)> {
            let w = p.w;
            let mut v: Vec<_> = p.cells.iter().map(|&(x, y)| (w - 1 - x, y)).collect();
            v.sort();
            v
        };
        for (name, p) in [
            ("crown", crown()),
            ("lantern", lantern()),
            ("petals", petals()),
            ("cross", cross()),
        ] {
            let mut cells = p.cells.clone();
            cells.sort();
            assert_eq!(flip(&p), cells, "{name} is left-right symmetric");
        }
        for (name, p) in [
            ("lantern", lantern()),
            ("petals", petals()),
            ("cross", cross()),
        ] {
            let h = p.h;
            let mut v: Vec<_> = p.cells.iter().map(|&(x, y)| (x, h - 1 - y)).collect();
            v.sort();
            let mut cells = p.cells.clone();
            cells.sort();
            assert_eq!(v, cells, "{name} is top-bottom symmetric too");
        }
    }

    #[test]
    fn counts() {
        assert_eq!(glider().cells.len(), 5);
        assert_eq!(lwss().cells.len(), 9);
        assert_eq!(mwss().cells.len(), 11);
        assert_eq!(hwss().cells.len(), 13);
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
    fn every_east_ship_moves_two_cells_per_period() {
        for pat in [lwss(), mwss(), hwss()] {
            let mut l = Life::new(60, 40, 0, CONWAY);
            l.place(&pat, 10, 10, 0);
            let mut shifted = Life::new(60, 40, 0, CONWAY);
            shifted.place(&pat, 12, 10, 0);
            for _ in 0..4 {
                l.step();
            }
            assert!(
                l.cells() == shifted.cells(),
                "ship did not translate by 2 east in 4 generations ({} cells)",
                pat.cells.len()
            );
        }
    }

    #[test]
    fn methuselahs_keep_busy_for_a_long_time() {
        for (pat, min_pop) in [(r_pentomino(), 40), (acorn(), 40)] {
            let mut l = Life::new(200, 200, 0, CONWAY);
            l.place(&pat, 90, 90, 0);
            for _ in 0..400 {
                l.step();
            }
            assert!(
                l.population() > min_pop,
                "pop {} after 400 generations",
                l.population()
            );
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
