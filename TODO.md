**Things I Like**

- [ ] **Code-rain hybrid:** Render a faint matrix or hex-digit layer over the dead cells so the background looks like a terminal.
- [ ] **Stamped patterns:** Occasionally drop a pixel-art Tux, an Arch logo, or `>_` and `$ sudo` text into the grid and let it decay. A tiny bitmap font plus a stamp function would cover all of these.
- [ ] **System-driven behavior:** Read `/proc/loadavg` or CPU usage, and let it change the tick rate, the spawn density or the colour. Idle gives a calm slow board, and a compile gives a frantic bright one.
- [ ] **Other rulesets:** Add selectable rules such as HighLife, Day & Night, Seeds and Brian's Brain, and rotate them on each soft reset. Brian's Brain and Seeds look especially good as a wallpaper.
- [ ] **Pattern library:** Occasionally inject more than gliders, such as lightweight spaceships, a pulsar, or a Gosper glider gun on a longer timer. A gun gives a permanent stream of gliders and is the classic "this is a Life wallpaper" sight.
- [ ] **Cell age colors:** Colour cells by age, so newborns are yellow, mature cells green and old ones fading to orange or red. This stays inside the Gruvbox palette and looks far less flat than one live colour.
- [ ] **Death trails:** Fade dead cells over a few frames with a dim afterglow, like CRT phosphor. You'd need per-cell intensity, which is cheap, and it makes 4 Hz feel much smoother.
- [ ] **Auto-start on every monitor:** Spawn one thread or surface per output instead of running one instance per monitor.
- [ ] **HiDPI:** Handle `scale_factor_changed` and `wp_fractional_scale` so the grid is crisp on scaled monitors. This is the one real functional gap right now.
- [ ] **Dirty-rect rendering:** Redraw only the cells that changed and track damage instead of repainting the whole frame. That matters for larger screens and for a lower CPU use at idle.
- [ ] **Population overlay:** An optional tiny corner readout of generation and live count, in the style of a terminal status line.
- [ ] **Output hotplug:** Recreate the surface when a monitor is plugged in or removed, instead of exiting.
- [ ] **Config file:** Read `~/.config/life-wallpaper/config.toml` for palette, cell size, density, tick rate, intervals and rules, and reload it on `SIGHUP`. The hardcoded constants in `controller.rs` and `render.rs` are the ones to move.
- [ ] **More CLI flags:** `--cell-size`, `--fps`, `--density`, `--rule`, `--color`, plus `--list-outputs`.