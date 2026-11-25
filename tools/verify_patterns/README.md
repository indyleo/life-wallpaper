# Verifying patterns

Independent checks used for the patterns added to `src/patterns.rs`. None of this is built or run by cargo.

* `sim.py` – a tiny set-based Life simulator (the reference).
* `hl.py` – Hashlife in Python (validated against `sim.py`: R-pentomino, 162 cells at generation 1024, 116 at 1151).
* `classify.py` – population at 1024, 2048, ... generations; a ratio near 2 per doubling is linear growth, near 4 is a breeder.
* `hsearch2.py` – random combinations of the engine, the infinite line and the Gosper gun, looking for quadratic growth (none found).
* `life_long.c` – exhaustive search of small symmetric seeds for long lifespans (`gcc -O3 -march=native life_long.c`; arguments: size, mode 4 = four-fold or 2 = mirror).

Rust tests in `src/patterns.rs` pin the exact populations measured here.
