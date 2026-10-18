//! Wayland side: one background layer surface ("wall") per output, wl_shm
//! double buffering, fractional scaling, hotplug, and presenting.

use std::{path::PathBuf, time::{Duration, Instant, SystemTime}};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    globals::GlobalData,
    output::{OutputHandler, OutputState},
    reexports::{
        client::{
            globals::GlobalList,
            protocol::{wl_output, wl_shm, wl_surface::WlSurface},
            Connection, Dispatch, Proxy, QueueHandle,
        },
        protocols::wp::{
            fractional_scale::v1::client::{
                wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
                wp_fractional_scale_v1::{self, WpFractionalScaleV1},
            },
            viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{
        slot::{Buffer, SlotPool},
        Shm, ShmHandler,
    },
};

use crate::config::{Config, ConfigSource};
use crate::controller::Sim;
use crate::hypr::HyprEvent;
use crate::look::{self, Look};
use crate::render::{self, Fade, Key, Renderer, Target, SENTINEL};
use crate::state;
use crate::sysload::CpuLoad;

/// A frame callback not answered for this long means the compositor isn't
/// showing the wallpaper (fullscreen window on top, screen off, ...).
const HIDDEN_AFTER: Duration = Duration::from_millis(750);
/// While nothing is being committed, keep a callback in flight at this pace
/// so a covered wallpaper is still noticed.
const PROBE_EVERY: Duration = Duration::from_secs(1);
/// Timer pace while every wall is hidden: just enough to notice un-hiding.
const HIDDEN_POLL: Duration = Duration::from_millis(200);

struct Buf {
    buffer: Buffer,
    /// What this buffer's pixels currently show (display keys).
    shown: Vec<Key>,
}

/// One output's wallpaper surface plus its own simulation.
struct Wall {
    output: wl_output::WlOutput,
    layer: LayerSurface,
    fractional: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    scale120: u32, // preferred scale in 1/120ths (120 = 1.0)
    logical: (u32, u32),
    px: (usize, usize), // buffer size in device pixels
    cell: usize,        // cell size in device pixels
    gap: usize,         // gap between cells in device pixels
    cols: usize,
    rows: usize,
    pool: Option<SlotPool>,
    bufs: Vec<Buf>, // exactly two once configured
    next_buf: usize,
    sim: Option<Sim>,
    state_path: Option<PathBuf>,
    fade: Fade,
    display: Vec<Key>,   // what we want on screen right now (mid-fade colours)
    presented: Vec<Key>, // what the compositor currently shows
    damage: Vec<[i32; 4]>,
    // frame-callback bookkeeping for pause-when-hidden
    frame_pending: bool,
    frame_sent: Instant,
    frame_seen: bool,
    hidden: bool,
    resume: bool,
}

impl Drop for Wall {
    fn drop(&mut self) {
        if let Some(f) = self.fractional.take() {
            f.destroy();
        }
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
        // layer, pool and buffers are destroyed by their own Drop impls.
    }
}

impl Wall {
    /// (Re)create buffers and grid for the current logical size and scale.
    fn rebuild(&mut self, cfg: &Config, shm: &Shm, now: Instant, output_name: Option<&str>) {
        let (lw, lh) = self.logical;
        if lw == 0 || lh == 0 {
            return;
        }
        let s = self.scale120.max(120) as u64;
        let pw = ((lw as u64 * s + 60) / 120).max(1) as usize;
        let ph = ((lh as u64 * s + 60) / 120).max(1) as usize;
        let cell = ((cfg.cell_size as u64 * s + 60) / 120).max(1) as usize;
        let gap = ((cfg.cell_gap as u64 * s + 60) / 120) as usize;
        let (cols, rows) = render::grid_dims(pw, ph, cell);

        match self.sim.as_mut() {
            Some(sim) => sim.resize(cols, rows, cfg),
            None => {
                let mut sim = Sim::new(cols, rows, now, cfg);
                self.state_path = state::path_for(output_name);
                if cfg.persist {
                    if let Some(saved) = self.state_path.as_deref().and_then(state::load) {
                        sim.restore(&saved, cfg);
                    }
                }
                self.sim = Some(sim);
            }
        }

        let surface = self.layer.wl_surface();
        match &self.viewport {
            Some(vp) => vp.set_destination(lw as i32, lh as i32),
            None => surface.set_buffer_scale((s / 120).max(1) as i32),
        }

        self.bufs.clear();
        self.pool = None;
        let stride = (pw * 4) as i32;
        let mut pool = SlotPool::new(stride as usize * ph * 2, shm).expect("failed to create wl_shm pool");
        for _ in 0..2 {
            let (buffer, _) = pool
                .create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Xrgb8888)
                .expect("failed to create wl_buffer");
            self.bufs.push(Buf { buffer, shown: vec![SENTINEL; cols * rows] });
        }
        self.pool = Some(pool);
        self.display = vec![0; cols * rows];
        self.fade = Fade::new(cols * rows, now);
        self.presented = vec![SENTINEL; cols * rows];
        self.px = (pw, ph);
        self.cell = cell;
        self.gap = gap;
        self.hidden = false;
        self.resume = false;
        self.frame_pending = false;
        self.cols = cols;
        self.rows = rows;
        self.next_buf = 0;
    }

    /// Show the current generation immediately (no fade) and commit it.
    fn present(&mut self, r: &Renderer, cfg: &Config, hz: f64, look: Look, qh: &QueueHandle<App>) {
        let Some(sim) = self.sim.as_ref() else { return };
        if self.bufs.len() != 2 {
            return;
        }
        r.compose(sim, cfg, hz * sim.steps() as f64, look, self.fade.target_mut());
        self.fade.snap(&mut self.display);
        self.flush(r, qh);
    }

    /// A new generation was just simulated: start fading towards it.
    fn next_generation(&mut self, r: &Renderer, cfg: &Config, hz: f64, look: Look, now: Instant, interval: Duration) {
        let Some(sim) = self.sim.as_ref() else { return };
        if self.bufs.len() != 2 {
            return;
        }
        r.compose(sim, cfg, hz * sim.steps() as f64, look, self.fade.target_mut());
        self.fade.begin(&mut self.display, now, render::fade_duration(interval), cfg.smooth);
    }

    /// One animation frame: advance the crossfade, then commit if needed.
    fn frame(&mut self, r: &Renderer, cfg: &Config, qh: &QueueHandle<App>, now: Instant) {
        if self.bufs.len() != 2 {
            return;
        }
        self.fade.step(now, &mut self.display);
        self.flush(r, qh);
        // Nothing to commit for a while? Send an empty commit with a frame
        // request so we can still tell when the wallpaper gets covered.
        if cfg.pause_hidden && !self.frame_pending && now.saturating_duration_since(self.frame_sent) >= PROBE_EVERY {
            self.request_frame(qh, now);
            self.layer.commit();
        }
    }

    /// Ask for a frame callback with the next commit.
    fn request_frame(&mut self, qh: &QueueHandle<App>, now: Instant) {
        let surface = self.layer.wl_surface();
        surface.frame(qh, surface.clone());
        self.frame_pending = true;
        self.frame_sent = now;
    }

    /// Covered = a frame callback we asked for hasn't come back in time. Only
    /// trusted once this compositor has answered one before, so a compositor
    /// that never sends them can't pause us forever.
    fn update_hidden(&mut self, now: Instant, cfg: &Config) {
        if !cfg.pause_hidden {
            self.hidden = false;
        } else if self.frame_seen && self.frame_pending && now.saturating_duration_since(self.frame_sent) > HIDDEN_AFTER {
            self.hidden = true;
        }
    }

    /// If anything differs from what the compositor shows, repaint only the
    /// changed cells into a free buffer and commit with per-row damage. Does
    /// nothing (no commit) when nothing changed.
    fn flush(&mut self, r: &Renderer, qh: &QueueHandle<App>) {
        if self.display == self.presented {
            return;
        }
        let Some(pool) = self.pool.as_mut() else { return };

        for k in 0..2 {
            let idx = (self.next_buf + k) % 2;
            let Buf { buffer, shown } = &mut self.bufs[idx];
            let Some(canvas) = buffer.canvas(pool) else {
                continue; // still held by the compositor
            };
            let mut t = Target { canvas, w: self.px.0, h: self.px.1, cell: self.cell, gap: self.gap, cols: self.cols, rows: self.rows };
            r.draw(&mut t, &self.display, shown, &mut self.presented, &mut self.damage);

            let surface = self.layer.wl_surface();
            if buffer.attach_to(surface).is_err() {
                self.presented.fill(SENTINEL);
                continue;
            }
            for d in &self.damage {
                surface.damage_buffer(d[0], d[1], d[2], d[3]);
            }
            if !self.frame_pending {
                self.request_frame(qh, Instant::now());
            }
            self.layer.commit();
            self.next_buf = (idx + 1) % 2;
            return;
        }
        // Both buffers busy: nothing recorded as presented, so the next frame retries.
    }
}

pub struct App {
    registry_state: RegistryState,
    pub output_state: OutputState,
    qh: QueueHandle<Self>,
    shm: Shm,
    compositor: CompositorState,
    layer_shell: LayerShell,
    frac_mgr: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,

    walls: Vec<Wall>,
    cfg: Config,
    src: ConfigSource,
    config_path: Option<PathBuf>,
    config_modified: Option<SystemTime>,
    renderer: Renderer,
    load: CpuLoad,
    next_sample: Instant,
    next_gen: Instant,
    next_save: Instant,
    next_config_check: Instant,
    spawn: bool,
    pub exit: bool,
    start: Instant,
    look: Look,
    local_min: Option<u32>,
    next_clock: Instant,
    pending: Vec<HyprEvent>,
}

impl App {
    pub fn new(
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        compositor: CompositorState,
        layer_shell: LayerShell,
        shm: Shm,
        cfg: Config,
        src: ConfigSource,
        spawn: bool,
    ) -> Self {
        let frac_mgr = globals.bind::<WpFractionalScaleManagerV1, _, _>(qh, 1..=1, GlobalData).ok();
        let viewporter = globals.bind::<WpViewporter, _, _>(qh, 1..=1, GlobalData).ok();
        let (cfg_fps, cfg_save) = (cfg.fps, cfg.save_secs.max(1));
        let config_path = src.path.clone().or_else(ConfigSource::default_path);
        let config_modified = config_path.as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        Self {
            registry_state: RegistryState::new(globals),
            output_state: OutputState::new(globals, qh),
            qh: qh.clone(),
            shm,
            compositor,
            layer_shell,
            frac_mgr,
            viewporter,
            walls: Vec::new(),
            renderer: Renderer::new(&cfg),
            cfg,
            src,
            config_path,
            config_modified,
            load: CpuLoad::new(),
            next_sample: Instant::now(),
            next_gen: Instant::now() + Duration::from_secs_f64(1.0 / cfg_fps),
            next_save: Instant::now() + Duration::from_secs(cfg_save),
            next_config_check: Instant::now(),
            spawn,
            exit: false,
            start: Instant::now(),
            look: Look::default(),
            local_min: look::local_minutes(),
            next_clock: Instant::now() + Duration::from_secs(30),
            pending: Vec::new(),
        }
    }

    /// Whether the Hyprland event socket should be watched at all.
    pub fn hypr_enabled(&self) -> bool {
        self.cfg.hypr_events
    }

    /// Hyprland events read from the socket; handled on the next tick.
    pub fn push_events(&mut self, evs: &mut Vec<HyprEvent>) {
        if self.cfg.hypr_events {
            // a burst of events (workspace flicking) is capped; the sims also have a cooldown
            let room = 32usize.saturating_sub(self.pending.len());
            self.pending.extend(evs.drain(..).take(room));
        }
        evs.clear();
    }

    pub fn wall_count(&self) -> usize {
        self.walls.len()
    }

    pub fn output_filter(&self) -> Option<&str> {
        self.cfg.output.as_deref()
    }

    /// Time between generations (CPU load speeds it up in `sysload` mode).
    pub fn interval(&self) -> Duration {
        let base = 1.0 / self.cfg.fps;
        let load = if self.cfg.sysload { 0.6 + 2.4 * self.load.value() as f64 } else { 1.0 };
        // the tempo slowly swells and ebbs
        let breath = if self.cfg.breathe > 0.0 {
            let phase = self.start.elapsed().as_secs_f64() / self.cfg.breathe_secs.max(1) as f64 * std::f64::consts::TAU;
            1.0 + self.cfg.breathe * phase.sin()
        } else {
            1.0
        };
        Duration::from_secs_f64(base / (load * breath).max(0.05))
    }

    fn hz(&self) -> f64 {
        1.0 / self.interval().as_secs_f64()
    }

    /// How often the timer fires: every animation frame while crossfading,
    /// otherwise once per generation.
    pub fn frame_interval(&self) -> Duration {
        if !self.walls.is_empty() && self.walls.iter().all(|w| w.hidden) {
            return HIDDEN_POLL;
        }
        let tick = self.interval();
        if self.cfg.smooth {
            tick.min(Duration::from_secs_f64(1.0 / self.cfg.render_fps))
        } else {
            tick
        }
    }

    /// Called from the single calloop timer: steps the simulation when a
    /// generation is due, then advances the crossfade and commits a frame.
    pub fn on_tick(&mut self, now: Instant) {
        if now >= self.next_config_check {
            self.next_config_check = now + Duration::from_millis(500);
            self.check_config_file();
        }
        if now >= self.next_clock {
            self.next_clock = now + Duration::from_secs(30);
            self.local_min = look::local_minutes();
        }
        self.look = look::compute(&self.cfg, now.duration_since(self.start), self.local_min);
        let gen_due = now >= self.next_gen;
        if gen_due {
            if self.cfg.sysload && now >= self.next_sample {
                self.load.sample();
                self.next_sample = now + Duration::from_secs(1);
            }
            // Schedule from the previous deadline to avoid drift; never from the past.
            let iv = self.interval();
            let next = self.next_gen + iv;
            self.next_gen = if next > now { next } else { now + iv };
        }
        let load = if self.cfg.sysload { self.load.value() } else { 0.0 };
        let hz = self.hz();
        let interval = self.interval();
        let look = self.look;
        let events = std::mem::take(&mut self.pending);
        let App { walls, cfg, renderer, qh, .. } = &mut *self;
        for w in walls.iter_mut() {
            if !w.hidden {
                if let Some(sim) = w.sim.as_mut() {
                    for &ev in &events {
                        sim.on_event(ev, now, cfg);
                    }
                }
            }
            w.update_hidden(now, cfg);
            if w.hidden {
                continue; // covered: no simulating, no drawing
            }
            if w.resume {
                // Visible again: restart the timers and jump to the current board.
                w.resume = false;
                if let Some(sim) = w.sim.as_mut() {
                    sim.resume(now, cfg);
                }
                w.present(renderer, cfg, hz, look, qh);
                continue;
            }
            if gen_due {
                if let Some(sim) = w.sim.as_mut() {
                    sim.tick(now, cfg, load);
                }
                w.next_generation(renderer, cfg, hz, look, now, interval);
            }
            w.frame(renderer, cfg, qh, now);
        }
        if self.cfg.persist && self.cfg.save_secs > 0 && now >= self.next_save {
            self.next_save = now + Duration::from_secs(self.cfg.save_secs);
            self.save_all();
        }
    }

    /// Write every wall's board to its state file.
    fn save_all(&self) {
        for w in &self.walls {
            if let (Some(sim), Some(path)) = (w.sim.as_ref(), w.state_path.as_ref()) {
                if let Err(e) = state::save(path, &sim.life, sim.base_rule()) {
                    eprintln!("life-wallpaper: could not save {}: {e}", path.display());
                }
            }
        }
    }

    /// Poll the config file so edits apply automatically without a signal/restart.
    /// Metadata polling is cheap and avoids adding a platform-specific watcher.
    fn check_config_file(&mut self) {
        let modified = self.config_path.as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        if modified != self.config_modified {
            self.reload_config();
        }
    }

    /// Re-read config file + CLI overrides and rebuild every wall.
    /// Also refresh the timestamp after failed reloads to avoid retrying every frame.
    pub fn reload_config(&mut self) {
        self.config_modified = self.config_path.as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        match self.src.load() {
            Ok(mut new) => {
                new.output = self.cfg.output.clone(); // output filter needs a restart
                self.cfg = new;
                self.renderer = Renderer::new(&self.cfg);
                let (now, hz, look) = (Instant::now(), self.hz(), self.look);
                let names: Vec<Option<String>> =
                    self.walls.iter().map(|w| self.output_state.info(&w.output).and_then(|i| i.name)).collect();
                let App { walls, cfg, shm, renderer, qh, .. } = self;
                for (w, name) in walls.iter_mut().zip(&names) {
                    if let Some(sim) = w.sim.as_mut() {
                        sim.reconfigure(now, cfg);
                    }
                    w.rebuild(cfg, shm, now, name.as_deref());
                    w.present(renderer, cfg, hz, look, qh);
                }
                eprintln!("life-wallpaper: config reloaded");
            }
            Err(e) => eprintln!("life-wallpaper: config reload failed, keeping old config: {e}"),
        }
    }

    /// Drop every Wayland object we own.
    pub fn shutdown(&mut self) {
        if self.cfg.persist {
            self.save_all();
        }
        self.walls.clear();
    }

    fn wall_idx(&self, surface: &WlSurface) -> Option<usize> {
        self.walls.iter().position(|w| w.layer.wl_surface() == surface)
    }

    fn refresh(&mut self, i: usize) {
        let (hz, look) = (self.hz(), self.look);
        let name = self.output_state.info(&self.walls[i].output).and_then(|o| o.name);
        let App { walls, cfg, shm, renderer, qh, .. } = self;
        let w = &mut walls[i];
        w.rebuild(cfg, shm, Instant::now(), name.as_deref());
        w.present(renderer, cfg, hz, look, qh);
    }

    /// Create a wall for `output` if it passes the `--output` filter.
    fn maybe_add_wall(&mut self, qh: &QueueHandle<Self>, output: &wl_output::WlOutput) {
        if !self.spawn || self.walls.iter().any(|w| &w.output == output) {
            return;
        }
        if let Some(want) = &self.cfg.output {
            let name = self.output_state.info(output).and_then(|i| i.name);
            if name.as_deref() != Some(want.as_str()) {
                return; // not ours (or name not known yet: update_output retries)
            }
        }
        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(qh, surface, Layer::Background, Some("life-wallpaper"), Some(output));
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(0, 0);

        let (fractional, viewport) = match (&self.frac_mgr, &self.viewporter) {
            (Some(m), Some(v)) => {
                let s = layer.wl_surface();
                (Some(m.get_fractional_scale(s, qh, s.clone())), Some(v.get_viewport(s, qh, GlobalData)))
            }
            _ => (None, None),
        };
        layer.commit(); // initial commit, no buffer -> configure

        self.walls.push(Wall {
            output: output.clone(),
            layer,
            fractional,
            viewport,
            scale120: 120,
            logical: (0, 0),
            px: (0, 0),
            cell: 8,
            gap: 0,
            cols: 0,
            rows: 0,
            pool: None,
            bufs: Vec::new(),
            next_buf: 0,
            sim: None,
            state_path: None,
            fade: Fade::new(0, Instant::now()),
            display: Vec::new(),
            presented: Vec::new(),
            damage: Vec::new(),
            frame_pending: false,
            frame_sent: Instant::now(),
            frame_seen: false,
            hidden: false,
            resume: false,
        });
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        // Output went away (or compositor revoked us): drop that wall and keep
        // running so a re-plugged monitor gets a fresh one.
        self.walls.retain(|w| w.layer.wl_surface() != layer.wl_surface());
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32, // SCTK already acked this configure
    ) {
        let Some(i) = self.wall_idx(layer.wl_surface()) else { return };
        let (w, h) = configure.new_size;
        let size = (if w == 0 { 1920 } else { w }, if h == 0 { 1080 } else { h });
        let wall = &mut self.walls[i];
        if wall.logical != size || wall.bufs.is_empty() {
            wall.logical = size;
            self.refresh(i);
        } else {
            wall.presented.fill(SENTINEL); // answer the configure with a full commit
            let (hz, look) = (self.hz(), self.look);
            let App { walls, cfg, renderer, qh, .. } = self;
            walls[i].present(renderer, cfg, hz, look, qh);
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, new_factor: i32) {
        // Integer-scale fallback; with fractional-scale the protocol event wins.
        if let Some(i) = self.wall_idx(surface) {
            let s = new_factor.max(1) as u32 * 120;
            if self.walls[i].fractional.is_none() && self.walls[i].scale120 != s {
                self.walls[i].scale120 = s;
                self.refresh(i);
            }
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
        // The compositor drew (or can draw) our surface again.
        if let Some(i) = self.wall_idx(surface) {
            let w = &mut self.walls[i];
            w.frame_pending = false;
            w.frame_seen = true;
            if w.hidden {
                w.hidden = false;
                w.resume = true;
            }
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.maybe_add_wall(qh, &output);
    }
    fn update_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.maybe_add_wall(qh, &output); // names can arrive late
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.walls.retain(|w| w.output != output);
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

// --- fractional scale + viewporter plumbing -------------------------------

impl Dispatch<WpFractionalScaleManagerV1, GlobalData> for App {
    fn event(_: &mut Self, _: &WpFractionalScaleManagerV1, _: <WpFractionalScaleManagerV1 as Proxy>::Event, _: &GlobalData, _: &Connection, _: &QueueHandle<Self>) {
        unreachable!("wp_fractional_scale_manager_v1 has no events")
    }
}

impl Dispatch<WpViewporter, GlobalData> for App {
    fn event(_: &mut Self, _: &WpViewporter, _: <WpViewporter as Proxy>::Event, _: &GlobalData, _: &Connection, _: &QueueHandle<Self>) {
        unreachable!("wp_viewporter has no events")
    }
}

impl Dispatch<WpViewport, GlobalData> for App {
    fn event(_: &mut Self, _: &WpViewport, _: <WpViewport as Proxy>::Event, _: &GlobalData, _: &Connection, _: &QueueHandle<Self>) {
        unreachable!("wp_viewport has no events")
    }
}

impl Dispatch<WpFractionalScaleV1, WlSurface> for App {
    fn event(state: &mut Self, _: &WpFractionalScaleV1, event: wp_fractional_scale_v1::Event, surface: &WlSurface, _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            if let Some(i) = state.wall_idx(surface) {
                if state.walls[i].scale120 != scale {
                    state.walls[i].scale120 = scale;
                    state.refresh(i);
                }
            }
        }
    }
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_layer!(App);
delegate_registry!(App);
