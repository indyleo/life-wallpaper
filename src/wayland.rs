//! Wayland side: one background layer surface ("wall") per output, wl_shm
//! double buffering, fractional scaling, hotplug, and presenting.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

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

use crate::audio::{self, AudioVis, Envelope, Frame, Light};
use crate::config::{Config, ConfigSource, InjectReq};
use crate::controller::{Edge, Sim, Spot};
use crate::feed::{self, Weather};
use crate::hypr::{self, HyprEvent, Placed, Raw};
use crate::ipc::{self, Command};
use crate::look::{self, Look};
use crate::mpris;
use crate::power::{self, Power};
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
    /// Audio light on top of the board (set every tick by the app).
    light: Light,
    /// Night / battery / idle dimming, 0..1 (the light never defeats it).
    dim: f32,
    /// `display` with the light applied (only used while the light is on).
    lit: Vec<Key>,
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
        let (cols, rows) = render::grid_dims_for(cfg.grid, pw, ph, cell);

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
        let mut pool =
            SlotPool::new(stride as usize * ph * 2, shm).expect("failed to create wl_shm pool");
        for _ in 0..2 {
            let (buffer, _) = pool
                .create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Xrgb8888)
                .expect("failed to create wl_buffer");
            self.bufs.push(Buf {
                buffer,
                shown: vec![SENTINEL; cols * rows],
            });
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
        r.compose(
            sim,
            cfg,
            hz * sim.steps() as f64,
            look,
            self.fade.target_mut(),
        );
        self.fade.snap(&mut self.display);
        self.flush(r, qh);
    }

    /// A new generation was just simulated: start fading towards it.
    fn next_generation(
        &mut self,
        r: &Renderer,
        cfg: &Config,
        hz: f64,
        look: Look,
        now: Instant,
        interval: Duration,
    ) {
        let Some(sim) = self.sim.as_ref() else { return };
        if self.bufs.len() != 2 {
            return;
        }
        r.compose(
            sim,
            cfg,
            hz * sim.steps() as f64,
            look,
            self.fade.target_mut(),
        );
        self.fade.begin(
            &mut self.display,
            now,
            render::fade_duration(interval),
            cfg.smooth,
        );
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
        if cfg.pause_hidden
            && !self.frame_pending
            && now.saturating_duration_since(self.frame_sent) >= PROBE_EVERY
        {
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
        } else if self.frame_seen
            && self.frame_pending
            && now.saturating_duration_since(self.frame_sent) > HIDDEN_AFTER
        {
            self.hidden = true;
        }
    }

    /// If anything differs from what the compositor shows, repaint only the
    /// changed cells into a free buffer and commit with per-row damage. Does
    /// nothing (no commit) when nothing changed.
    fn flush(&mut self, r: &Renderer, qh: &QueueHandle<App>) {
        // With audio light on, what is shown is the board plus the light; the
        // diff below is against that, so a steady level repaints nothing.
        let lit = !self.light.is_neutral();
        if lit {
            match self.sim.as_ref() {
                Some(sim) => r.apply_light(
                    &self.display,
                    sim.life.cells(),
                    self.cols,
                    self.rows,
                    self.light,
                    self.dim,
                    &mut self.lit,
                ),
                None => {
                    self.lit.clear();
                    self.lit.extend_from_slice(&self.display);
                }
            }
        }
        let src: &[Key] = if lit { &self.lit } else { &self.display };
        if src == self.presented.as_slice() {
            return;
        }
        let Some(pool) = self.pool.as_mut() else {
            return;
        };

        for k in 0..2 {
            let idx = (self.next_buf + k) % 2;
            let Buf { buffer, shown } = &mut self.bufs[idx];
            let Some(canvas) = buffer.canvas(pool) else {
                continue; // still held by the compositor
            };
            let mut t = Target {
                canvas,
                w: self.px.0,
                h: self.px.1,
                cell: self.cell,
                gap: self.gap,
                cols: self.cols,
                rows: self.rows,
            };
            r.draw(&mut t, src, shown, &mut self.presented, &mut self.damage);

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
    pending: Vec<Placed>,
    hypr: hypr::Tracker,
    beats: Vec<Beat>,
    /// Shared with the audio reader; sampled once per tick.
    vis: Arc<AudioVis>,
    env: Envelope,
    light: Light,
    frame: Frame,
    /// Paused over the control socket.
    paused: bool,
    power: Power,
    next_power: Instant,
    low_power: bool,
    last_activity: Instant,
    idle_now: bool,
    next_cursor: Instant,
    last_cursor: Option<(i32, i32)>,
    /// A pointer move to act on this tick: layout position and the previous one.
    pointer: Option<((f64, f64), Option<(i32, i32)>)>,
    weather: Option<Weather>,
    next_snow: Instant,
    bass_since: Option<Instant>,
    next_bloom: Instant,
    /// `Some(false)` while the active player is paused or stopped.
    music_playing: Option<bool>,
    last_track: Option<(String, String)>,
    track_buckets: Vec<Option<usize>>,
    next_stats: Instant,
    pending_scene: Option<String>,
    pending_inject: Vec<InjectReq>,
    scene_on_exit: Option<String>,
}

/// What a beat from the audio analyzer asks the boards to do.
enum Beat {
    Glider(f32),
    Spark,
    Noise,
    /// Sustained loud bass: a pulsar blooms.
    Bloom,
    /// Snow weather: a few cells fall in at the top.
    Snow,
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
        let frac_mgr = globals
            .bind::<WpFractionalScaleManagerV1, _, _>(qh, 1..=1, GlobalData)
            .ok();
        let viewporter = globals
            .bind::<WpViewporter, _, _>(qh, 1..=1, GlobalData)
            .ok();
        let (cfg_fps, cfg_save) = (cfg.fps, cfg.save_secs.max(1));
        let locate = cfg.hypr_events && cfg.hypr_locate;
        let config_path = src.path.clone().or_else(ConfigSource::default_path);
        let config_modified = config_path
            .as_ref()
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
            hypr: hypr::Tracker::new(locate, hypr::socket_path().is_some(), Instant::now()),
            beats: Vec::new(),
            vis: Arc::new(AudioVis::new(Instant::now())),
            env: Envelope::default(),
            light: Light::default(),
            frame: Frame::default(),
            paused: false,
            power: Power::default(),
            next_power: Instant::now(),
            low_power: false,
            last_activity: Instant::now(),
            idle_now: false,
            next_cursor: Instant::now(),
            last_cursor: None,
            pointer: None,
            weather: None,
            next_snow: Instant::now(),
            bass_since: None,
            next_bloom: Instant::now(),
            music_playing: None,
            last_track: None,
            track_buckets: Vec::new(),
            next_stats: Instant::now() + Duration::from_secs(1),
            pending_scene: None,
            pending_inject: Vec::new(),
            scene_on_exit: None,
        }
    }

    /// Whether the Hyprland event socket should be watched at all.
    pub fn hypr_enabled(&self) -> bool {
        self.cfg.hypr_events
    }

    /// Lines read from Hyprland's event socket. Events that can be answered
    /// now are queued for the next tick; opens wait for the window's geometry.
    pub fn push_events(&mut self, raws: &mut Vec<Raw>) {
        if self.cfg.hypr_events {
            let now = Instant::now();
            if !raws.is_empty() {
                self.last_activity = now; // any window or workspace event counts as someone being there
            }
            for raw in raws.drain(..) {
                self.hypr.on_raw(raw, now, &mut self.pending);
            }
            // a burst of events (workspace flicking) is capped; the sims also have a cooldown
            self.pending.truncate(32);
        }
        raws.clear();
    }

    /// Whether audio was asked for (read once at start-up).
    pub fn audio_enabled(&self) -> bool {
        self.cfg.audio
    }
    pub fn audio_command(&self) -> &str {
        &self.cfg.audio_command
    }
    pub fn audio_restart(&self) -> bool {
        self.cfg.audio_restart_backoff
    }
    pub fn audio_restart_max(&self) -> u64 {
        self.cfg.audio_restart_max_secs
    }
    pub fn verbose(&self) -> bool {
        self.cfg.verbose
    }

    /// A fresh analyzer with the configured sensitivities (read whenever the helper starts).
    pub fn audio_analyzer(&self) -> audio::Analyzer {
        let c = &self.cfg;
        audio::Analyzer::new(
            c.audio_sensitivity,
            [
                c.audio_band_bass_sensitivity,
                c.audio_band_mid_sensitivity,
                c.audio_band_treble_sensitivity,
            ],
        )
    }

    /// Should the audio helper be running right now? Not while nobody could
    /// see the result (every output hidden, paused, low-power mode that
    /// switches audio off) or when nothing would use it: decoding for nobody
    /// is wasted work.
    pub fn audio_wanted(&self) -> bool {
        let c = &self.cfg;
        c.audio
            && !self.paused
            && !(self.low_power && c.battery_disable.audio)
            && !(!self.walls.is_empty() && self.walls.iter().all(|w| w.hidden))
            && (c.audio_gliders
                || c.audio_react
                || c.audio_band_mid_sparks
                || c.audio_band_treble_noise)
    }

    /// The light is live: animate at `render_fps` even without `smooth`.
    fn light_live(&self) -> bool {
        self.cfg.audio && self.cfg.audio_react && self.audio_wanted()
    }

    /// Audio arrived: update the shared light signal and queue whatever the
    /// beats are routed to.
    pub fn push_audio(&mut self, b: &audio::Block, now: Instant) {
        self.vis.push(now, b);
        if self.paused {
            return;
        }
        let c = &self.cfg;
        let mut add = |beat: Beat| {
            if self.beats.len() < 8 {
                self.beats.push(beat);
            }
        };
        if c.audio_gliders {
            let src = if c.audio_band_bass_gliders {
                b.beat[audio::BASS]
            } else {
                b.beat[audio::FULL]
            };
            if let Some(s) = src {
                add(Beat::Glider(s));
            }
        }
        if c.audio_band_mid_sparks && b.beat[audio::MID].is_some() {
            add(Beat::Spark);
        }
        if c.audio_band_treble_noise && b.beat[audio::TREBLE].is_some() {
            add(Beat::Noise);
        }
    }

    // ---- media player ----

    pub fn mpris_enabled(&self) -> bool {
        self.cfg.mpris
    }
    pub fn mpris_command(&self) -> String {
        mpris::command(&self.cfg.mpris_player)
    }

    /// Track / status lines from the player helper.
    pub fn push_tracks(&mut self, tracks: Vec<mpris::Track>) {
        for t in tracks {
            if self.cfg.verbose {
                eprintln!(
                    "life-wallpaper: mpris: {:?} {} - {}",
                    t.status, t.artist, t.title
                );
            }
            self.music_playing = Some(t.status == mpris::Status::Playing);
            if t.status != mpris::Status::Playing {
                continue;
            }
            let key = (t.artist.clone(), t.title.clone());
            if self.last_track.as_ref() != Some(&key) {
                self.last_track = Some(key);
                let bucket = (self.cfg.mpris_artist_colors && !t.artist.is_empty())
                    .then(|| mpris::artist_bucket(&t.artist, 64));
                if self.track_buckets.len() < 4 {
                    self.track_buckets.push(bucket);
                }
            }
        }
    }

    /// The player helper is gone: back to normal activity.
    pub fn mpris_gone(&mut self) {
        self.music_playing = None;
    }

    // ---- command and weather ----

    pub fn command_enabled(&self) -> bool {
        !self.cfg.command.trim().is_empty()
    }
    pub fn command_line(&self) -> &str {
        &self.cfg.command
    }
    pub fn command_interval(&self) -> u64 {
        self.cfg.command_interval_secs
    }
    pub fn weather_enabled(&self) -> bool {
        self.cfg.weather && !self.cfg.weather_command.trim().is_empty()
    }
    pub fn weather_line(&self) -> &str {
        &self.cfg.weather_command
    }
    pub fn weather_interval(&self) -> u64 {
        self.cfg.weather_interval_secs
    }

    /// One line printed by `command`: an event for the boards.
    pub fn push_command_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        match feed::parse_event(line) {
            Ok(cmd) => {
                let reply = self.ipc_command(line, cmd);
                if self.cfg.verbose || reply.starts_with("err") {
                    eprintln!("life-wallpaper: command: {line} -> {reply}");
                }
            }
            Err(e) => eprintln!("life-wallpaper: command: ignoring {line:?}: {e}"),
        }
    }

    /// The whole answer of one `weather_command` run.
    pub fn push_weather(&mut self, text: &str) {
        match feed::parse_weather(text) {
            Some(w) => {
                if self.cfg.verbose {
                    eprintln!("life-wallpaper: weather: {:?}, {:?} C", w.sky, w.temp_c);
                }
                self.weather = Some(w);
            }
            None => {
                eprintln!("life-wallpaper: weather: could not read the answer of weather_command")
            }
        }
    }

    // ---- control socket, scenes ----

    pub fn ipc_enabled(&self) -> bool {
        self.cfg.ipc
    }
    pub fn ipc_socket_path(&self) -> Option<PathBuf> {
        ipc::socket_path(&self.cfg.ipc_path, self.cfg.output.as_deref())
    }

    /// `--load-scene` and `--inject` (applied once every output has a board)
    /// and `--save-scene` (on exit).
    pub fn set_startup(
        &mut self,
        load: Option<String>,
        save: Option<String>,
        inject: Vec<InjectReq>,
    ) {
        self.pending_scene = load;
        self.scene_on_exit = save;
        self.pending_inject = inject;
    }

    fn save_scene_of_first_wall(&self, name: &str) -> Result<PathBuf, String> {
        let sim = self
            .walls
            .iter()
            .find_map(|w| w.sim.as_ref())
            .ok_or("no board yet")?;
        state::save_scene(&self.cfg.scenes_dir, name, &sim.life, sim.base_rule())
    }

    fn load_scene_into_walls(&mut self, name: &str) -> Result<usize, String> {
        let saved = state::load_scene(&self.cfg.scenes_dir, name)?;
        let mut n = 0;
        for sim in self.walls.iter_mut().filter_map(|w| w.sim.as_mut()) {
            sim.load_scene(&saved)?;
            n += 1;
        }
        if n == 0 {
            return Err("no board yet".into());
        }
        Ok(n)
    }

    /// One line of numbers per output (the same for `--stats` and the socket's `stats`).
    pub fn stats_line(&self) -> String {
        let mut parts = Vec::new();
        for w in &self.walls {
            let Some(sim) = w.sim.as_ref() else { continue };
            let (live, settled) = sim.life.census();
            let name = self
                .output_state
                .info(&w.output)
                .and_then(|i| i.name)
                .unwrap_or_else(|| "?".into());
            let mut s = format!(
                "out={name} gen={} pop={live} settled={settled} rule={}",
                sim.gen,
                sim.life.rule().name()
            );
            let cc = sim.colony_counts();
            if !cc.is_empty() {
                s.push_str(&format!(
                    " colonies={}",
                    cc.iter()
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>()
                        .join("/")
                ));
            }
            if self.cfg.sysload {
                s.push_str(&format!(" heat={} load={:.2}", sim.heat, self.load.value()));
            }
            parts.push(s);
        }
        let mut line = if parts.is_empty() {
            "no outputs yet".to_string()
        } else {
            parts.join(" | ")
        };
        if self.cfg.audio {
            line.push_str(&format!(
                " audio env={:.2} flash={:.2}",
                self.frame.level[audio::FULL],
                self.frame.flash[audio::FULL]
            ));
        }
        if self.paused {
            line.push_str(" paused");
        }
        line
    }

    /// Run one command from the control socket; the reply is one line.
    pub fn ipc_command(&mut self, line: &str, cmd: Command) -> String {
        if self.cfg.verbose {
            eprintln!("life-wallpaper: ipc: {line}");
        }
        let now = Instant::now();
        match cmd {
            Command::Help => ipc::HELP.to_string(),
            Command::Stats => format!("ok {}", self.stats_line()),
            Command::Pause => {
                self.paused = true;
                "ok paused".into()
            }
            Command::Resume => {
                self.paused = false;
                "ok resumed".into()
            }
            Command::Quit => {
                self.exit = true;
                "ok bye".into()
            }
            Command::Set(key, val) => {
                let key = key.replace('-', "_");
                const AT_START: [&str; 16] = [
                    "output",
                    "audio",
                    "audio_command",
                    "audio_sensitivity",
                    "audio_band_bass_sensitivity",
                    "audio_band_mid_sensitivity",
                    "audio_band_treble_sensitivity",
                    "ipc",
                    "ipc_path",
                    "mpris",
                    "mpris_player",
                    "command",
                    "command_interval_secs",
                    "weather",
                    "weather_command",
                    "weather_interval_secs",
                ];
                if AT_START.contains(&key.as_str()) {
                    return format!("err {key} is read at start-up; restart to change it");
                }
                let mut new = self.cfg.clone();
                if let Err(e) = new.set(&key, &val).and_then(|_| new.validate()) {
                    return format!("err {e}");
                }
                self.apply_config(new);
                format!("ok {key}={val} (until the next config reload)")
            }
            Command::Reseed { rule, density } => {
                let mut c = self.cfg.clone();
                if let Some(r) = rule {
                    if let Err(e) = c.set("rule", &r) {
                        return format!("err {e}");
                    }
                }
                if let Some(d) = density {
                    if let Err(e) = c.set("reseed_density", &d) {
                        return format!("err {e}");
                    }
                }
                let mut n = 0;
                for sim in self.walls.iter_mut().filter_map(|w| w.sim.as_mut()) {
                    sim.reseed(&c);
                    n += 1;
                }
                if n == 0 {
                    "err no board yet".into()
                } else {
                    format!("ok reseeded {n} output(s)")
                }
            }
            Command::Inject { name, at, edge } => {
                let edge = match edge.as_deref().map(|e| Edge::parse(e).ok_or(e)) {
                    Some(Err(bad)) => {
                        return format!("err unknown edge {bad:?} (top, bottom, left, right)")
                    }
                    Some(Ok(e)) => Some(e),
                    None => None,
                };
                let App { walls, cfg, .. } = self;
                let (mut ok, mut first_err) = (0, None);
                for sim in walls.iter_mut().filter_map(|w| w.sim.as_mut()) {
                    match sim.inject(&name, at, edge, cfg, now, true) {
                        Ok(_) => ok += 1,
                        Err(e) => first_err = first_err.or(Some(e)),
                    }
                }
                match (ok, first_err) {
                    (0, Some(e)) => format!("err {e}"),
                    (0, None) => "err no board yet".into(),
                    (n, _) => format!("ok injected {name} on {n} output(s)"),
                }
            }
            Command::SaveScene(name) => match self.save_scene_of_first_wall(&name) {
                Ok(p) => format!("ok saved {}", p.display()),
                Err(e) => format!("err {e}"),
            },
            Command::LoadScene(name) => match self.load_scene_into_walls(&name) {
                Ok(n) => format!("ok loaded {name} on {n} output(s)"),
                Err(e) => format!("err {e}"),
            },
        }
    }

    /// Battery and idle state decide things once per tick.
    fn update_modes(&mut self, now: Instant) {
        if self.cfg.power && now >= self.next_power {
            self.next_power = now + Duration::from_secs(10);
            self.power = power::read();
        }
        self.low_power = self.cfg.power && self.power.low(self.cfg.battery_threshold);

        self.poll_pointer(now);
        self.idle_now = self.cfg.idle
            && hypr::socket_path().is_some()
            && now.saturating_duration_since(self.last_activity)
                >= Duration::from_secs(self.cfg.idle_secs);
    }

    /// Where the pointer is (`hyprctl cursorpos`), for `cursor` (inject at it)
    /// and `idle` (any movement counts as activity). Only asks when one of the
    /// two is on and Hyprland is there; at `cursor_interval_ms` for the first,
    /// every 2 s for the second.
    fn poll_pointer(&mut self, now: Instant) {
        if !(self.cfg.cursor || self.cfg.idle)
            || hypr::socket_path().is_none()
            || now < self.next_cursor
        {
            return;
        }
        let every = if self.cfg.cursor {
            Duration::from_millis(self.cfg.cursor_interval_ms)
        } else {
            Duration::from_secs(2)
        };
        self.next_cursor = now + every;
        let Some((gx, gy)) = hypr::query("cursorpos").and_then(|t| hypr::parse_cursorpos(&t))
        else {
            return;
        };
        let here = (gx.floor() as i32, gy.floor() as i32);
        if self.last_cursor == Some(here) {
            return; // still: nothing to do, and not activity
        }
        self.last_activity = now;
        if self.cfg.cursor {
            self.pointer = Some(((gx, gy), self.last_cursor));
            if !self.hypr.has_monitors() {
                self.hypr.want_refresh(now); // output geometry is needed to find the cell
            }
        }
        self.last_cursor = Some(here);
    }

    /// The config the simulations run on right now: the configured one with
    /// low-power, idle and music-paused adjustments, or `None` if none apply.
    fn effective(&self) -> Option<Config> {
        let c = &self.cfg;
        let idle_ff = self.idle_now && c.idle_fastforward;
        let calm_music = self.music_playing == Some(false);
        let wet = c.weather && self.weather.is_some_and(|w| w.rains());
        if !(self.low_power || idle_ff || calm_music || wet) {
            return None;
        }
        let mut e = c.clone();
        if wet {
            e.rain = true;
        }
        if self.low_power {
            let d = c.battery_disable;
            e.traffic &= !d.traffic;
            e.stamps &= !d.stamps;
            e.patterns &= !d.patterns;
        }
        if idle_ff {
            e.fastforward = true;
        }
        if calm_music {
            e.fastforward = false;
            e.noise_per_sec = 0.0;
        }
        Some(e)
    }

    /// Extra dimming from low-power and idle mode (stacks with `night_dim`).
    fn extra_dim(&self) -> f64 {
        let b = if self.low_power {
            self.cfg.battery_dim
        } else {
            0.0
        };
        let i = if self.idle_now {
            self.cfg.idle_dim
        } else {
            0.0
        };
        1.0 - (1.0 - b) * (1.0 - i)
    }

    pub fn wall_count(&self) -> usize {
        self.walls.len()
    }

    pub fn output_filter(&self) -> Option<&str> {
        self.cfg.output.as_deref()
    }

    /// Time between generations (CPU load speeds it up in `sysload` mode).
    pub fn interval(&self) -> Duration {
        let mut base = 1.0 / self.cfg.fps;
        if self.low_power {
            base /= self.cfg.battery_fps_scale;
        }
        if self.idle_now {
            base /= self.cfg.idle_fps_scale;
        }
        let load = if self.cfg.sysload {
            0.6 + 2.4 * self.load.value() as f64
        } else {
            1.0
        };
        // the tempo slowly swells and ebbs (more so when idle)
        let depth = if self.idle_now {
            self.cfg.breathe.max(self.cfg.idle_breathe)
        } else {
            self.cfg.breathe
        };
        let breath = if depth > 0.0 {
            let phase = self.start.elapsed().as_secs_f64() / self.cfg.breathe_secs.max(1) as f64
                * std::f64::consts::TAU;
            1.0 + depth * phase.sin()
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
        if self.cfg.smooth || self.light_live() {
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
        self.update_modes(now);
        self.look = look::compute(&self.cfg, now.duration_since(self.start), self.local_min)
            .with_extra_dim(self.extra_dim());
        // audio light: smooth the shared signal once per tick; nothing at all when off
        if self.light_live() {
            self.frame = self.env.step(&self.vis, now, &self.cfg);
            self.light = Light::from_frame(&self.frame, &self.cfg);
        } else if !self.light.is_neutral() || self.frame != Frame::default() {
            self.light = Light::default();
            self.frame = Frame::default();
            self.env = Envelope::default();
        }
        if self.cfg.weather && self.cfg.weather_palette {
            if let Some(w) = self.weather {
                self.look.hue = self.look.hue.wrapping_add(w.hue());
            }
        }
        if self.cfg.weather
            && self.weather.is_some_and(|w| w.snows())
            && now >= self.next_snow
            && !self.paused
        {
            self.next_snow = now + Duration::from_millis(600);
            if self.beats.len() < 8 {
                self.beats.push(Beat::Snow);
            }
        }
        // sustained loud bass (two seconds above 0.6) blooms a pulsar, then rests for 10 s
        if self.cfg.audio_band_bass_bloom
            && self.light_live()
            && self.frame.level[audio::BASS] > 0.6
        {
            let since = *self.bass_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= Duration::from_secs(2)
                && now >= self.next_bloom
            {
                self.next_bloom = now + Duration::from_secs(10);
                self.bass_since = None;
                if self.beats.len() < 8 {
                    self.beats.push(Beat::Bloom);
                }
            }
        } else {
            self.bass_since = None;
        }
        if self.cfg.stats && now >= self.next_stats {
            self.next_stats = now + Duration::from_secs(1);
            eprintln!("life-wallpaper: {}", self.stats_line());
        }
        if (self.pending_scene.is_some() || !self.pending_inject.is_empty())
            && !self.walls.is_empty()
            && self.walls.iter().all(|w| w.sim.is_some())
        {
            if let Some(name) = self.pending_scene.take() {
                match self.load_scene_into_walls(&name) {
                    Ok(_) => eprintln!("life-wallpaper: loaded scene {name:?}"),
                    Err(e) => eprintln!("life-wallpaper: --load-scene: {e}"),
                }
            }
            for req in std::mem::take(&mut self.pending_inject) {
                let edge = req.edge.as_deref().and_then(Edge::parse);
                for sim in self.walls.iter_mut().filter_map(|w| w.sim.as_mut()) {
                    if let Err(e) = sim.inject(&req.name, req.at, edge, &self.cfg, now, false) {
                        eprintln!("life-wallpaper: --inject {}: {e}", req.name);
                    }
                }
            }
        }
        let due = now >= self.next_gen;
        let gen_due = due && !self.paused;
        if due {
            if self.cfg.sysload && now >= self.next_sample {
                self.load.sample();
                self.next_sample = now + Duration::from_secs(1);
            }
            // Schedule from the previous deadline to avoid drift; never from the past.
            let iv = self.interval();
            let next = self.next_gen + iv;
            self.next_gen = if next > now { next } else { now + iv };
        }
        let load = if self.cfg.sysload {
            self.load.value()
        } else {
            0.0
        };
        let hz = self.hz();
        let interval = self.interval();
        let look = self.look;
        let local_min = self.local_min;
        // Window geometry: refresh the cache when an event asked for it, then
        // release the opens that were waiting for it.
        if self.hypr.refresh_due(now) {
            let (clients, monitors) = (hypr::query("j/clients"), hypr::query("j/monitors"));
            self.hypr.refresh(clients.as_deref(), monitors.as_deref());
        }
        self.hypr.ready(now, &mut self.pending);
        let mut events = std::mem::take(&mut self.pending);
        let mut beats = std::mem::take(&mut self.beats);
        let mut buckets = std::mem::take(&mut self.track_buckets);
        if self.paused {
            events.clear();
            beats.clear();
            buckets.clear();
        }
        let pointer = self.pointer.take();
        let (light, dim) = (self.light, self.look.dim_frac());
        let guns_off = self.low_power && self.cfg.battery_disable.guns;
        let eff = self.effective();

        // Resolve pointer coordinates before borrowing App mutably.
        let pointer = pointer.and_then(|((gx, gy), prev)| {
            let (name, lx, ly) = self.hypr.locate_point(gx, gy)?;

            let prev_local = prev
                .and_then(|(px, py)| self.hypr.locate_point(px as f64, py as f64))
                .filter(|(n, _, _)| *n == name)
                .map(|(_, x, y)| (x, y));

            Some((name, (lx, ly), prev_local))
        });

        let App {
            walls,
            cfg,
            renderer,
            qh,
            output_state,
            ..
        } = &mut *self;

        let sim_cfg: &Config = eff.as_ref().unwrap_or(&*cfg);
        let names: Vec<Option<String>> =
            if events.iter().any(|e| e.place.is_some()) || pointer.is_some() {
                walls
                    .iter()
                    .map(|w| output_state.info(&w.output).and_then(|i| i.name))
                    .collect()
            } else {
                Vec::new()
            };
        for (n, w) in walls.iter_mut().enumerate() {
            w.light = light;
            w.dim = dim;
            if !w.hidden {
                let (cols, rows) = (w.cols, w.rows);
                if let Some(sim) = w.sim.as_mut() {
                    sim.set_guns_off(guns_off);
                    for ev in &events {
                        match &ev.place {
                            // nobody knows where: every output reacts at a random spot
                            None => sim.on_event(ev.ev, now, sim_cfg),
                            // known: only the output the window is on reacts, at its spot
                            Some(p)
                                if names.get(n).and_then(|nm| nm.as_deref())
                                    == Some(p.monitor.as_str()) =>
                            {
                                if let Some(spot) =
                                    Spot::from_px(p.x, p.y, p.w, p.h, cfg.cell_size, cols, rows)
                                {
                                    sim.on_event_at(ev.ev, spot, now, sim_cfg);
                                }
                            }
                            Some(_) => {}
                        }
                    }
                    for ev in &events {
                        if cfg.workspace_ripple && ev.ev == HyprEvent::Workspace {
                            sim.on_workspace_ripple(sim_cfg);
                        }
                    }
                    for b in &beats {
                        match *b {
                            Beat::Glider(strength) => sim.on_beat(strength, now),
                            Beat::Spark => sim.on_spark(now),
                            Beat::Noise => sim.on_noise(),
                            Beat::Bloom => {
                                sim.on_bloom(now);
                            }
                            Beat::Snow => sim.on_snow(),
                        }
                    }
                    if let Some((name, (lx, ly), prev)) = &pointer {
                        if names.get(n).and_then(|nm| nm.as_deref()) == Some(name.as_str()) {
                            let cell = |x: i32, y: i32| {
                                (
                                    (x.max(0) as usize) / cfg.cell_size.max(1),
                                    (y.max(0) as usize) / cfg.cell_size.max(1),
                                )
                            };
                            sim.on_cursor(
                                cfg.cursor_mode,
                                prev.map(|(x, y)| cell(x, y)),
                                cell(*lx, *ly),
                                cfg.cursor_radius as usize,
                            );
                        }
                    }
                    for &bucket in &buckets {
                        sim.on_track(bucket, now);
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
                    sim.set_clock(local_min);
                    sim.tick(now, sim_cfg, load);
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
        let modified = self
            .config_path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        if modified != self.config_modified {
            self.reload_config();
        }
    }

    /// Re-read config file + CLI overrides and rebuild every wall.
    /// Also refresh the timestamp after failed reloads to avoid retrying every frame.
    pub fn reload_config(&mut self) {
        self.config_modified = self
            .config_path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        match self.src.load() {
            Ok(new) => {
                self.apply_config(new);
                eprintln!("life-wallpaper: config reloaded");
            }
            Err(e) => eprintln!("life-wallpaper: config reload failed, keeping old config: {e}"),
        }
    }

    /// Adopt `new` and rebuild every wall. Keys that are read once at
    /// start-up (output filter, audio helper, control socket, player helper)
    /// keep their running values.
    fn apply_config(&mut self, mut new: Config) {
        {
            new.output = self.cfg.output.clone(); // output filter needs a restart
            new.audio = self.cfg.audio; // so does the audio helper
            new.audio_command = self.cfg.audio_command.clone();
            new.audio_sensitivity = self.cfg.audio_sensitivity;
            new.audio_band_bass_sensitivity = self.cfg.audio_band_bass_sensitivity;
            new.audio_band_mid_sensitivity = self.cfg.audio_band_mid_sensitivity;
            new.audio_band_treble_sensitivity = self.cfg.audio_band_treble_sensitivity;
            new.ipc = self.cfg.ipc;
            new.ipc_path = self.cfg.ipc_path.clone();
            new.mpris = self.cfg.mpris;
            new.mpris_player = self.cfg.mpris_player.clone();
            new.command = self.cfg.command.clone();
            new.command_interval_secs = self.cfg.command_interval_secs;
            new.weather = self.cfg.weather;
            new.weather_command = self.cfg.weather_command.clone();
            new.weather_interval_secs = self.cfg.weather_interval_secs;
            {
                self.cfg = new;
                self.hypr = hypr::Tracker::new(
                    self.cfg.hypr_events && self.cfg.hypr_locate,
                    hypr::socket_path().is_some(),
                    Instant::now(),
                );
                self.renderer = Renderer::new(&self.cfg);
                let (now, hz, look) = (Instant::now(), self.hz(), self.look);
                let names: Vec<Option<String>> = self
                    .walls
                    .iter()
                    .map(|w| self.output_state.info(&w.output).and_then(|i| i.name))
                    .collect();
                let App {
                    walls,
                    cfg,
                    shm,
                    renderer,
                    qh,
                    ..
                } = self;
                for (w, name) in walls.iter_mut().zip(&names) {
                    if let Some(sim) = w.sim.as_mut() {
                        sim.reconfigure(now, cfg);
                    }
                    w.rebuild(cfg, shm, now, name.as_deref());
                    w.present(renderer, cfg, hz, look, qh);
                }
            }
        }
    }

    /// Drop every Wayland object we own.
    pub fn shutdown(&mut self) {
        if let Some(name) = self.scene_on_exit.take() {
            match self.save_scene_of_first_wall(&name) {
                Ok(p) => eprintln!("life-wallpaper: saved scene to {}", p.display()),
                Err(e) => eprintln!("life-wallpaper: --save-scene: {e}"),
            }
        }
        if self.cfg.persist {
            self.save_all();
        }
        self.walls.clear();
    }

    fn wall_idx(&self, surface: &WlSurface) -> Option<usize> {
        self.walls
            .iter()
            .position(|w| w.layer.wl_surface() == surface)
    }

    fn refresh(&mut self, i: usize) {
        let (hz, look) = (self.hz(), self.look);
        let name = self
            .output_state
            .info(&self.walls[i].output)
            .and_then(|o| o.name);
        let App {
            walls,
            cfg,
            shm,
            renderer,
            qh,
            ..
        } = self;
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
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Background,
            Some("life-wallpaper"),
            Some(output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(0, 0);

        let (fractional, viewport) = match (&self.frac_mgr, &self.viewporter) {
            (Some(m), Some(v)) => {
                let s = layer.wl_surface();
                (
                    Some(m.get_fractional_scale(s, qh, s.clone())),
                    Some(v.get_viewport(s, qh, GlobalData)),
                )
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
            light: Light::default(),
            dim: 0.0,
            lit: Vec::new(),
        });
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        // Output went away (or compositor revoked us): drop that wall and keep
        // running so a re-plugged monitor gets a fresh one.
        self.walls
            .retain(|w| w.layer.wl_surface() != layer.wl_surface());
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32, // SCTK already acked this configure
    ) {
        let Some(i) = self.wall_idx(layer.wl_surface()) else {
            return;
        };
        let (w, h) = configure.new_size;
        let size = (if w == 0 { 1920 } else { w }, if h == 0 { 1080 } else { h });
        let wall = &mut self.walls[i];
        if wall.logical != size || wall.bufs.is_empty() {
            wall.logical = size;
            self.refresh(i);
        } else {
            wall.presented.fill(SENTINEL); // answer the configure with a full commit
            let (hz, look) = (self.hz(), self.look);
            let App {
                walls,
                cfg,
                renderer,
                qh,
                ..
            } = self;
            walls[i].present(renderer, cfg, hz, look, qh);
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &WlSurface,
        new_factor: i32,
    ) {
        // Integer-scale fallback; with fractional-scale the protocol event wins.
        if let Some(i) = self.wall_idx(surface) {
            let s = new_factor.max(1) as u32 * 120;
            if self.walls[i].fractional.is_none() && self.walls[i].scale120 != s {
                self.walls[i].scale120 = s;
                self.refresh(i);
            }
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wl_output::Transform,
    ) {
    }
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
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.maybe_add_wall(qh, &output);
    }
    fn update_output(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.maybe_add_wall(qh, &output); // names can arrive late
    }
    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
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
    fn event(
        _: &mut Self,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        unreachable!("wp_fractional_scale_manager_v1 has no events")
    }
}

impl Dispatch<WpViewporter, GlobalData> for App {
    fn event(
        _: &mut Self,
        _: &WpViewporter,
        _: <WpViewporter as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        unreachable!("wp_viewporter has no events")
    }
}

impl Dispatch<WpViewport, GlobalData> for App {
    fn event(
        _: &mut Self,
        _: &WpViewport,
        _: <WpViewport as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        unreachable!("wp_viewport has no events")
    }
}

impl Dispatch<WpFractionalScaleV1, WlSurface> for App {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        surface: &WlSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
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
