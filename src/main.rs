//! Conway's Game of Life as a native Wayland wallpaper (wlr-layer-shell, wl_shm).
//! One process drives every output (hotplug aware); `--output NAME` limits it to one.
//! See `life-wallpaper --help`.

mod audio;
mod config;
mod controller;
mod feed;
mod font;
mod hypr;
mod ipc;
mod life;
mod look;
mod mpris;
mod patterns;
mod png;
mod power;
mod render;
mod state;
mod sysload;
mod wayland;

use std::{
    cell::RefCell,
    error::Error,
    io::Read,
    rc::Rc,
    time::{Duration, Instant},
};

use smithay_client_toolkit::{
    compositor::CompositorState,
    reexports::{
        calloop::{
            generic::Generic,
            signals::{Signal, Signals},
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, LoopHandle, Mode, PostAction,
        },
        calloop_wayland_source::WaylandSource,
        client::{globals::registry_queue_init, Connection},
    },
    shell::wlr_layer::LayerShell,
    shm::Shm,
};

use wayland::App;

type Sup = Rc<RefCell<audio::Supervisor>>;

/// A `command` or `weather_command` source: runs once or forever, and
/// hands what it prints to the app.
struct Periodic {
    cmd: String,
    /// 0 = a long-running process, restarted 5 s after it exits.
    interval: Duration,
    next: Instant,
    running: bool,
    /// The whole answer is one document (weather) rather than event lines.
    document: bool,
}

type Per = Rc<RefCell<Periodic>>;

fn periodic(cmd: &str, secs: u64, document: bool) -> Per {
    Rc::new(RefCell::new(Periodic {
        cmd: cmd.to_string(),
        interval: Duration::from_secs(secs),
        next: Instant::now(),
        running: false,
        document,
    }))
}

/// Run the command once (or start it), reading its output without blocking.
fn start_periodic<'l>(handle: &LoopHandle<'l, App>, st: &Per, app: &App) {
    let now = Instant::now();
    let (cmd, document, interval) = {
        let mut s = st.borrow_mut();
        if s.running || now < s.next {
            return;
        }
        s.next = now + s.interval.max(Duration::from_secs(5));
        (s.cmd.clone(), s.document, s.interval)
    };
    let (helper, stdout) = match audio::spawn(&cmd) {
        Ok(h) => h,
        Err(e) => {
            st.borrow_mut().next = now + interval.max(Duration::from_secs(30));
            eprintln!("life-wallpaper: could not run {cmd:?}: {e}");
            return;
        }
    };
    if app.verbose() {
        eprintln!("life-wallpaper: started {cmd:?}");
    }
    st.borrow_mut().running = true;
    let st = st.clone();
    let mut text: Vec<u8> = Vec::new();
    let mut lines_done = 0usize;
    handle
        .insert_source(
            Generic::new(stdout, Interest::READ, Mode::Level),
            move |_, out, app: &mut App| {
                let _keep_alive = &helper;
                let mut buf = [0u8; 8192];
                let mut closed = false;
                loop {
                    match audio::read_nonblocking(&**out, &mut buf) {
                        Ok(audio::Read::Data(n)) => {
                            if text.len() < 256 * 1024 {
                                text.extend_from_slice(&buf[..n]);
                            }
                        }
                        Ok(audio::Read::Empty) => break,
                        Ok(audio::Read::Closed) | Err(_) => {
                            closed = true;
                            break;
                        }
                    }
                }
                if !document {
                    // hand over every complete line (and the last partial one once the process is gone)
                    let upto = if closed {
                        text.len()
                    } else {
                        text.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1)
                    };
                    let chunk: Vec<u8> = text.drain(..upto).collect();
                    for line in String::from_utf8_lossy(&chunk).lines() {
                        app.push_command_line(line);
                        lines_done += 1;
                    }
                }
                if closed {
                    if document {
                        app.push_weather(&String::from_utf8_lossy(&text));
                    } else if app.verbose() {
                        eprintln!("life-wallpaper: command finished after {lines_done} line(s)");
                    }
                    let mut s = st.borrow_mut();
                    s.running = false;
                    return Ok(PostAction::Remove);
                }
                Ok(PostAction::Continue)
            },
        )
        .expect("failed to register command source");
}

/// `--once`: simulate headless and write one frame as a PNG (the path given as `--output`).
fn run_once(cli: &config::Cli, cfg: &config::Config) -> Result<(), Box<dyn Error>> {
    let path = cfg.output.clone().ok_or("--once needs --output FILE.png")?;
    let (w, h) = cli.once_size;
    let (cell, gap) = (cfg.cell_size.max(1), cfg.cell_gap);
    let (cols, rows) = render::grid_dims_for(cfg.grid, w, h, cell);
    let start = Instant::now();
    let mut sim = controller::Sim::new(cols, rows, start, cfg);
    if let Some(name) = &cli.load_scene {
        sim.load_scene(&state::load_scene(&cfg.scenes_dir, name)?)?;
    }
    // simulated time: no waiting, so a preview takes milliseconds
    let step = Duration::from_secs_f64(1.0 / cfg.fps);
    let mut now = start;
    for _ in 0..cli.once_gens {
        now += step;
        sim.tick(now, cfg, 0.0);
    }
    // injected last, so it shows where it was put
    for req in &cli.inject {
        let edge = match req.edge.as_deref() {
            Some(e) => Some(
                controller::Edge::parse(e)
                    .ok_or_else(|| format!("unknown edge {e:?} (top, bottom, left, right)"))?,
            ),
            None => None,
        };
        sim.inject(&req.name, req.at, edge, cfg, now, false)?;
    }
    let r = render::Renderer::new(cfg);
    let mut keys = vec![0; cols * rows];
    r.compose(&sim, cfg, cfg.fps, look::Look::default(), &mut keys);
    let mut canvas = vec![0u8; w * h * 4];
    r.paint_all(
        &mut render::Target {
            canvas: &mut canvas,
            w,
            h,
            cell,
            gap,
            cols,
            rows,
        },
        &keys,
    );
    let mut rgb = Vec::with_capacity(w * h * 3);
    for px in canvas.chunks_exact(4) {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]); // XRGB8888 is B G R X in memory
    }
    std::fs::write(&path, png::encode_rgb(w, h, &rgb)).map_err(|e| format!("{path}: {e}"))?;
    if let Some(name) = &cli.save_scene {
        let p = state::save_scene(&cfg.scenes_dir, name, &sim.life, sim.base_rule())?;
        eprintln!("life-wallpaper: saved scene to {}", p.display());
    }
    eprintln!(
        "life-wallpaper: wrote {path} ({w}x{h}, {cols}x{rows} cells, {} generations)",
        cli.once_gens
    );
    Ok(())
}

/// Start (or restart) the audio helper and feed it into the analyzer.
/// The supervisor decides which tool and when; this wires the pipe into the loop.
fn start_audio<'l>(handle: &LoopHandle<'l, App>, sup: &Sup, app: &App) {
    let now = Instant::now();
    let (max, restart) = (
        Duration::from_secs(app.audio_restart_max()),
        app.audio_restart(),
    );
    let started = sup.borrow_mut().start(now, max, restart);
    let (helper, stdout) = match started {
        Ok(h) => h,
        Err(e) => {
            let retry = sup
                .borrow()
                .retry_in(now)
                .map(|d| format!("; trying again in {:.1}s", d.as_secs_f32()))
                .unwrap_or_default();
            eprintln!(
                "life-wallpaper: audio = true, but could not start {:?}: {e}{retry}",
                sup.borrow().command()
            );
            return;
        }
    };
    if app.verbose() {
        eprintln!(
            "life-wallpaper: audio helper started: {}",
            sup.borrow().command()
        );
    }
    let sup = sup.clone();
    let mut helper = helper;
    let mut analyzer = app.audio_analyzer();
    handle
        .insert_source(Generic::new(stdout, Interest::READ, Mode::Level), move |_, out, app: &mut App| {
            if !app.audio_wanted() {
                // nobody is looking (or paused): drop the helper, the tick timer starts it again when wanted
                sup.borrow_mut().paused();
                return Ok(PostAction::Remove);
            }
            let mut buf = [0u8; 8192];
            let mut closed = false;
            loop {
                match audio::read_nonblocking(&**out, &mut buf) {
                    Ok(audio::Read::Data(n)) => {
                        sup.borrow_mut().saw_data();
                        let block = analyzer.feed(&buf[..n]);
                        app.push_audio(&block, Instant::now());
                    }
                    Ok(audio::Read::Empty) => break,
                    Ok(audio::Read::Closed) | Err(_) => {
                        closed = true;
                        break;
                    }
                }
            }
            if app.verbose() {
                if let Some(line) = analyzer.report() {
                    eprintln!("life-wallpaper: {line}");
                }
            }
            if closed {
                let status = helper.status().unwrap_or_else(|| "its output closed".into());
                let (restart, max) = (app.audio_restart(), Duration::from_secs(app.audio_restart_max()));
                let now = Instant::now();
                sup.borrow_mut().exited(now, max, restart);
                if restart {
                    let wait = sup.borrow().retry_in(now).unwrap_or_default();
                    eprintln!("life-wallpaper: the audio helper exited ({status}); restarting in {:.1}s", wait.as_secs_f32());
                } else {
                    eprintln!("life-wallpaper: the audio helper exited ({status}); no more audio (audio_restart_backoff = false)");
                }
                return Ok(PostAction::Remove);
            }
            Ok(PostAction::Continue)
        })
        .expect("failed to register audio source");
}

/// Start (or restart) `playerctl --follow` and feed track changes to the app.
fn start_mpris<'l>(handle: &LoopHandle<'l, App>, sup: &Sup, app: &App) {
    let now = Instant::now();
    let started = sup.borrow_mut().start(now, Duration::from_secs(30), true);
    let (helper, stdout) = match started {
        Ok(h) => h,
        Err(e) => {
            if app.verbose() {
                eprintln!("life-wallpaper: mpris = true, but could not start playerctl: {e}");
            }
            return;
        }
    };
    let sup = sup.clone();
    let helper = helper; // dropped (and the helper stopped) with this source
    let mut lines = mpris::Lines::default();
    handle
        .insert_source(
            Generic::new(stdout, Interest::READ, Mode::Level),
            move |_, out, app: &mut App| {
                let _keep_alive = &helper; // moves the helper into the closure
                let mut buf = [0u8; 4096];
                let mut tracks = Vec::new();
                let mut closed = false;
                loop {
                    match audio::read_nonblocking(&**out, &mut buf) {
                        Ok(audio::Read::Data(n)) => {
                            sup.borrow_mut().saw_data();
                            lines.feed(&buf[..n], &mut tracks);
                        }
                        Ok(audio::Read::Empty) => break,
                        Ok(audio::Read::Closed) | Err(_) => {
                            closed = true;
                            break;
                        }
                    }
                }
                if !tracks.is_empty() {
                    app.push_tracks(tracks);
                }
                if closed {
                    sup.borrow_mut()
                        .exited(Instant::now(), Duration::from_secs(30), true);
                    app.mpris_gone();
                    return Ok(PostAction::Remove);
                }
                Ok(PostAction::Continue)
            },
        )
        .expect("failed to register media player source");
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = match config::parse_cli(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("life-wallpaper: {e}\n\n{}", config::USAGE);
            std::process::exit(2);
        }
    };
    if cli.version {
        println!("life-wallpaper {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if let Some(path) = &cli.validate {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("{}: {e}", path.display());
            std::process::exit(1);
        });
        let mut c = config::Config::default();
        match c.apply_toml(&text).and_then(|_| c.validate()) {
            Ok(()) => {
                println!("{}: ok", path.display());
                return Ok(());
            }
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                std::process::exit(1);
            }
        }
    }
    let cfg = cli.source.load().unwrap_or_else(|e| {
        eprintln!("life-wallpaper: {e}");
        std::process::exit(2);
    });
    if cli.dump_config {
        print!("{}", cfg.dump());
        return Ok(());
    }
    if cli.once {
        if let Err(e) = run_once(&cli, &cfg) {
            eprintln!("life-wallpaper: --once: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }

    // --- Wayland setup -----------------------------------------------------
    let conn = Connection::connect_to_env()?;
    let (globals, mut event_queue) = registry_queue_init::<App>(&conn)?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|e| format!("compositor lacks zwlr_layer_shell_v1: {e}"))?;
    let shm = Shm::bind(&globals, &qh)?;
    let mut app = App::new(
        &globals,
        &qh,
        compositor,
        layer_shell,
        shm,
        cfg,
        cli.source.clone(),
        !cli.list_outputs,
    );
    app.set_startup(
        cli.load_scene.clone(),
        cli.save_scene.clone(),
        cli.inject.clone(),
    );

    // Outputs (and their names) arrive during these roundtrips; walls are
    // created from the output handlers, so hotplug uses the same path.
    for _ in 0..3 {
        event_queue.roundtrip(&mut app)?;
    }

    if cli.list_outputs {
        for o in app.output_state.outputs() {
            if let Some(i) = app.output_state.info(&o) {
                let size = i
                    .logical_size
                    .map(|(w, h)| format!("{w}x{h}"))
                    .unwrap_or_else(|| "?".into());
                println!(
                    "{}\t{} {}\t{}\tscale {}",
                    i.name.unwrap_or_else(|| "?".into()),
                    i.make,
                    i.model,
                    size,
                    i.scale_factor
                );
            }
        }
        return Ok(());
    }
    if let Some(want) = app.output_filter() {
        if app.wall_count() == 0 {
            eprintln!("life-wallpaper: output {want:?} not found yet; waiting for it (see --list-outputs)");
        }
    }

    // --- Single event loop: Wayland fd + frame timer (generations are stepped from it) + signals --------
    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    let handle = event_loop.handle();

    WaylandSource::new(conn.clone(), event_queue)
        .insert(handle.clone())
        .expect("failed to register Wayland source");

    // Helpers that may need starting again (audio after a crash or a pause,
    // the player follower) are checked from the tick timer, so there is no
    // second timer and nothing runs at all when they are off.
    let audio_sup: Option<Sup> = app
        .audio_enabled()
        .then(|| Rc::new(RefCell::new(audio::Supervisor::new(app.audio_command()))));
    let mpris_sup: Option<Sup> = app
        .mpris_enabled()
        .then(|| Rc::new(RefCell::new(audio::Supervisor::new(&app.mpris_command()))));

    let cmd_src: Option<Per> = app
        .command_enabled()
        .then(|| periodic(app.command_line(), app.command_interval(), false));
    let weather_src: Option<Per> = app
        .weather_enabled()
        .then(|| periodic(app.weather_line(), app.weather_interval(), true));

    let tick_handle = handle.clone();
    let (tick_audio, tick_mpris) = (audio_sup.clone(), mpris_sup.clone());
    let (tick_cmd, tick_weather) = (cmd_src.clone(), weather_src.clone());
    handle
        .insert_source(
            Timer::from_duration(app.frame_interval()),
            move |deadline, _, app: &mut App| {
                let now = Instant::now();
                app.on_tick(now);
                if let Some(sup) = &tick_audio {
                    if app.audio_wanted() && sup.borrow().due(now) {
                        start_audio(&tick_handle, sup, app);
                    }
                }
                if let Some(sup) = &tick_mpris {
                    if sup.borrow().due(now) {
                        start_mpris(&tick_handle, sup, app);
                    }
                }
                for src in [&tick_cmd, &tick_weather].into_iter().flatten() {
                    start_periodic(&tick_handle, src, app);
                }
                // Schedule from the previous deadline to avoid drift; never from the past.
                let iv = app.frame_interval();
                let next = deadline + iv;
                TimeoutAction::ToInstant(if next > now { next } else { now + iv })
            },
        )
        .expect("failed to register tick timer");

    // Hyprland events (workspace switch, window open / close): optional, and
    // silently skipped when not running under Hyprland.
    if app.hypr_enabled() {
        if let Some(stream) = hypr::connect() {
            let mut lines = hypr::LineBuf::default();
            let mut evs = Vec::new();
            handle
                .insert_source(
                    Generic::new(stream, Interest::READ, Mode::Level),
                    move |_, sock, app: &mut App| {
                        let mut buf = [0u8; 4096];
                        loop {
                            match (&**sock).read(&mut buf) {
                                Ok(0) => return Ok(PostAction::Remove), // Hyprland went away
                                Ok(n) => lines.feed(&buf[..n], &mut evs),
                                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                                Err(e) => return Err(e),
                            }
                        }
                        app.push_events(&mut evs);
                        Ok(PostAction::Continue)
                    },
                )
                .expect("failed to register Hyprland event source");
        }
    }

    // Audio reactivity (opt-in): the first helper starts now; later ones from the tick timer.
    if let Some(sup) = &audio_sup {
        if app.audio_wanted() {
            start_audio(&handle, sup, &app);
        }
    }
    if let Some(sup) = &mpris_sup {
        start_mpris(&handle, sup, &app);
    }

    // Control socket (opt-in).
    let mut socket = None;
    if app.ipc_enabled() {
        match app.ipc_socket_path() {
            Some(path) => match ipc::bind(&path) {
                Ok(listener) => {
                    eprintln!("life-wallpaper: control socket at {}", path.display());
                    handle
                        .insert_source(
                            Generic::new(listener, Interest::READ, Mode::Level),
                            |_, l, app: &mut App| {
                                ipc::serve(&**l, &mut |line, cmd| app.ipc_command(line, cmd));
                                Ok(PostAction::Continue)
                            },
                        )
                        .expect("failed to register control socket");
                    socket = Some(path);
                }
                Err(e) => eprintln!("life-wallpaper: ipc = true, but {}: {e}", path.display()),
            },
            None => eprintln!(
                "life-wallpaper: ipc = true, but XDG_RUNTIME_DIR is not set (set ipc_path)"
            ),
        }
    }

    handle
        .insert_source(
            Signals::new(&[Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP])?,
            |ev, _, app: &mut App| match ev.signal() {
                Signal::SIGHUP => app.reload_config(),
                _ => app.exit = true,
            },
        )
        .expect("failed to register signal source");

    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
    }

    // --- Clean shutdown ----------------------------------------------------
    app.shutdown();
    if let Some(path) = socket {
        let _ = std::fs::remove_file(path);
    }
    let _ = conn.flush();
    Ok(())
}
