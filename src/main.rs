//! Conway's Game of Life as a native Wayland wallpaper (wlr-layer-shell, wl_shm).
//! One process drives every output (hotplug aware); `--output NAME` limits it to one.
//! See `life-wallpaper --help`.

mod config;
mod controller;
mod font;
mod hypr;
mod life;
mod look;
mod patterns;
mod render;
mod state;
mod sysload;
mod wayland;

use std::{error::Error, io::Read, time::Instant};

use smithay_client_toolkit::{
    compositor::CompositorState,
    reexports::{
        calloop::{
            generic::Generic,
            signals::{Signal, Signals},
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, Mode, PostAction,
        },
        calloop_wayland_source::WaylandSource,
        client::{globals::registry_queue_init, Connection},
    },
    shell::wlr_layer::LayerShell,
    shm::Shm,
};

use wayland::App;

fn main() -> Result<(), Box<dyn Error>> {
    let cli = match config::parse_cli(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("life-wallpaper: {e}\n\n{}", config::USAGE);
            std::process::exit(2);
        }
    };
    let cfg = cli.source.load().unwrap_or_else(|e| {
        eprintln!("life-wallpaper: {e}");
        std::process::exit(2);
    });

    // --- Wayland setup -----------------------------------------------------
    let conn = Connection::connect_to_env()?;
    let (globals, mut event_queue) = registry_queue_init::<App>(&conn)?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| format!("compositor lacks zwlr_layer_shell_v1: {e}"))?;
    let shm = Shm::bind(&globals, &qh)?;
    let mut app = App::new(&globals, &qh, compositor, layer_shell, shm, cfg, cli.source.clone(), !cli.list_outputs);

    // Outputs (and their names) arrive during these roundtrips; walls are
    // created from the output handlers, so hotplug uses the same path.
    for _ in 0..3 {
        event_queue.roundtrip(&mut app)?;
    }

    if cli.list_outputs {
        for o in app.output_state.outputs() {
            if let Some(i) = app.output_state.info(&o) {
                let size = i.logical_size.map(|(w, h)| format!("{w}x{h}")).unwrap_or_else(|| "?".into());
                println!("{}\t{} {}\t{}\tscale {}", i.name.unwrap_or_else(|| "?".into()), i.make, i.model, size, i.scale_factor);
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

    WaylandSource::new(conn.clone(), event_queue).insert(handle.clone()).expect("failed to register Wayland source");

    handle
        .insert_source(Timer::from_duration(app.frame_interval()), |deadline, _, app: &mut App| {
            let now = Instant::now();
            app.on_tick(now);
            // Schedule from the previous deadline to avoid drift; never from the past.
            let iv = app.frame_interval();
            let next = deadline + iv;
            TimeoutAction::ToInstant(if next > now { next } else { now + iv })
        })
        .expect("failed to register tick timer");

    // Hyprland events (workspace switch, window open / close): optional, and
    // silently skipped when not running under Hyprland.
    if app.hypr_enabled() {
        if let Some(stream) = hypr::connect() {
            let mut lines = hypr::LineBuf::default();
            let mut evs = Vec::new();
            handle
                .insert_source(Generic::new(stream, Interest::READ, Mode::Level), move |_, sock, app: &mut App| {
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
                })
                .expect("failed to register Hyprland event source");
        }
    }

    handle
        .insert_source(Signals::new(&[Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP])?, |ev, _, app: &mut App| {
            match ev.signal() {
                Signal::SIGHUP => app.reload_config(),
                _ => app.exit = true,
            }
        })
        .expect("failed to register signal source");

    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
    }

    // --- Clean shutdown ----------------------------------------------------
    app.shutdown();
    let _ = conn.flush();
    Ok(())
}
