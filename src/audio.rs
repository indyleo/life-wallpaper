//! Audio reactivity (off by default, `audio = true`).
//!
//! No audio library is linked in: a helper process (by default PipeWire's own
//! `pw-cat`, recording the monitor of the default output, so it hears whatever
//! is playing; `parec` and `pacat` are tried if it is not installed) writes
//! raw mono 16-bit samples to a pipe. This module spawns it, supervises it
//! (restart with backoff when it exits), and turns the samples into
//!
//! * **beats**, from a peak meter with a moving average, for the full range
//!   and for three bands (bass / mid / treble, split with two single-pole
//!   filters, no FFT), and
//! * a **light signal** (`AudioVis`): the latest level and last beat per band,
//!   shared with the renderer, which smooths it per frame (`Envelope`).
//!
//! With `audio = false` nothing here runs, so the cost when unused is zero:
//! no process, no thread, no filtering.
//!
//! The beat detector: a block of samples is a beat when its peak is clearly
//! above the recent average and well above the noise floor, and a short
//! refractory period follows each beat.

use std::{
    io,
    os::fd::AsRawFd,
    process::{Child, ChildStdout, Command, Stdio},
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
    time::{Duration, Instant},
};

use crate::config::{Config, DEFAULT_AUDIO_COMMAND};

/// Samples per analysis block (about 46 ms at 11.025 kHz).
const BLOCK: usize = 512;

/// Peaks below this (of full scale) are silence or hiss, never a beat.
const FLOOR: f32 = 0.03;

/// No second beat inside this many blocks (6 = about 280 ms). Counted in
/// blocks of audio, not wall-clock time, so a read that happens to hold two
/// beats still separates them correctly.
const REFRACTORY_BLOCKS: u64 = 6;

/// The first blocks only train the average (a plain mean, so a steady tone
/// is not mistaken for a run of beats while the average catches up from zero).
const WARMUP_BLOCKS: u64 = 8;

/// The moving average climbs towards a louder level over about this many
/// blocks (~0.55 s), so a sustained louder passage stops counting as beats...
const RISE_BLOCKS: f32 = 12.0;

/// ... and sinks over about this many (~1.5 s) once things get quieter, so a
/// kick does not leave the bar high for long.
const FALL_BLOCKS: f32 = 32.0;

/// Sample rate the helper is asked for.
const RATE: f32 = 11_025.0;

/// Band edges: bass below `LOW_HZ`, mid up to `HIGH_HZ`, treble above.
const LOW_HZ: f32 = 150.0;
const HIGH_HZ: f32 = 1500.0;

/// Signals the analyzer follows: the full range, then the three bands.
pub const SIGNALS: usize = 4;
pub const FULL: usize = 0;
pub const BASS: usize = 1;
pub const MID: usize = 2;
pub const TREBLE: usize = 3;
const NAMES: [&str; SIGNALS] = ["full", "bass", "mid", "treble"];

/// Per-signal noise floor for beats. Band signals carry less energy than the
/// full range, so their floors sit lower.
const SIGNAL_FLOOR: [f32; SIGNALS] = [FLOOR, 0.02, 0.012, 0.006];

/// Blocks per verbose report (about one second).
const REPORT_BLOCKS: u32 = 22;

/// The peak meter for one signal.
struct Core {
    /// Beat when peak > avg * ratio.
    ratio: f32,
    avg: f32,
    floor: f32,
    /// Block number of the last beat.
    last_beat: Option<u64>,
    /// Blocks seen so far; the first few only train the average.
    blocks: u64,
    // diagnostics for `--verbose`, reset by `take_diag`
    d_blocks: u32,
    d_peak: f32,
    d_beats: u32,
    d_thr: f32,
}

impl Core {
    fn new(sensitivity: f64, floor: f32) -> Self {
        Self {
            ratio: 1.0 + 0.8 / sensitivity.clamp(0.2, 5.0) as f32,
            avg: 0.0,
            floor,
            last_beat: None,
            blocks: 0,
            d_blocks: 0,
            d_peak: 0.0,
            d_beats: 0,
            d_thr: 0.0,
        }
    }

    fn block(&mut self, peak: f32) -> Option<f32> {
        self.blocks += 1;
        self.d_blocks += 1;
        self.d_peak = self.d_peak.max(peak);
        if self.blocks <= WARMUP_BLOCKS {
            self.avg += (peak - self.avg) / self.blocks as f32;
            return None;
        }
        // Compare against the average from *before* this block, so a kick
        // does not raise the bar it has to clear.
        let before = self.avg;
        self.avg += (peak - before)
            / if peak > before {
                RISE_BLOCKS
            } else {
                FALL_BLOCKS
            };
        self.d_thr = (before * self.ratio).max(self.floor);
        if peak < self.floor || peak <= before * self.ratio {
            return None;
        }
        if self
            .last_beat
            .is_some_and(|b| self.blocks - b < REFRACTORY_BLOCKS)
        {
            return None;
        }
        self.last_beat = Some(self.blocks);
        self.d_beats += 1;
        // How far above the threshold: ratio..2*ratio maps to 0..1
        Some(((peak / before.max(1e-4) - self.ratio) / self.ratio).clamp(0.0, 1.0))
    }

    /// `blocks peak/avg/threshold beats` since the last call.
    fn take_diag(&mut self) -> String {
        let s = format!(
            "{:.3}/{:.3}/{:.3} b{}",
            self.d_peak, self.avg, self.d_thr, self.d_beats
        );
        self.d_blocks = 0;
        self.d_peak = 0.0;
        self.d_beats = 0;
        s
    }
}

/// What one `feed` found.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Block {
    /// Loudest block peak (0..=1 of full scale) per signal in this feed.
    pub level: [f32; SIGNALS],
    /// Strength (0..=1) of the strongest beat per signal in this feed.
    pub beat: [Option<f32>; SIGNALS],
    /// Whole blocks analysed.
    pub blocks: u32,
}

/// Beat detection and band levels over a stream of s16le mono samples.
pub struct Analyzer {
    cores: [Core; SIGNALS],
    a_lo: f32,
    a_hi: f32,
    lo: f32,
    hi: f32,
    peak: [f32; SIGNALS],
    n: usize,
    spare: Option<u8>,
}

impl Analyzer {
    /// `master` is the sensitivity of the full-range detector; a band uses its
    /// own value if given, else the master's.
    pub fn new(master: f64, bands: [Option<f64>; 3]) -> Self {
        let coef = |hz: f32| 1.0 - (-std::f32::consts::TAU * hz / RATE).exp();
        let sens = |k: usize| {
            if k == FULL {
                master
            } else {
                bands[k - 1].unwrap_or(master)
            }
        };
        Self {
            cores: [
                Core::new(sens(0), SIGNAL_FLOOR[0]),
                Core::new(sens(1), SIGNAL_FLOOR[1]),
                Core::new(sens(2), SIGNAL_FLOOR[2]),
                Core::new(sens(3), SIGNAL_FLOOR[3]),
            ],
            a_lo: coef(LOW_HZ),
            a_hi: coef(HIGH_HZ),
            lo: 0.0,
            hi: 0.0,
            peak: [0.0; SIGNALS],
            n: 0,
            spare: None,
        }
    }

    /// Feed raw bytes (s16le mono). A sample split across two reads is joined.
    pub fn feed(&mut self, bytes: &[u8]) -> Block {
        let mut out = Block::default();
        let mut it = bytes.iter().copied();
        loop {
            let lo_byte = match self.spare.take() {
                Some(b) => b,
                None => match it.next() {
                    Some(b) => b,
                    None => break,
                },
            };
            let Some(hi_byte) = it.next() else {
                self.spare = Some(lo_byte);
                break;
            };
            let x = i16::from_le_bytes([lo_byte, hi_byte]) as f32 / 32768.0;
            // two single-pole low-passes split the signal into three bands
            self.lo += self.a_lo * (x - self.lo);
            self.hi += self.a_hi * (x - self.hi);
            let sig = [x, self.lo, self.hi - self.lo, x - self.hi];
            for k in 0..SIGNALS {
                self.peak[k] = self.peak[k].max(sig[k].abs());
            }
            self.n += 1;
            if self.n == BLOCK {
                self.end_block(&mut out);
            }
        }
        out
    }

    fn end_block(&mut self, out: &mut Block) {
        out.blocks += 1;
        for k in 0..SIGNALS {
            let peak = self.peak[k];
            out.level[k] = out.level[k].max(peak);
            if let Some(s) = self.cores[k].block(peak) {
                out.beat[k] = Some(out.beat[k].map_or(s, |b: f32| b.max(s)));
            }
            self.peak[k] = 0.0;
        }
        self.n = 0;
    }

    /// The `--verbose` line (peak/avg/threshold and beat count per signal),
    /// about once a second of audio.
    pub fn report(&mut self) -> Option<String> {
        let blocks = self.cores[FULL].d_blocks;
        if blocks < REPORT_BLOCKS {
            return None;
        }
        let mut s = format!("audio blocks={blocks}");
        for k in 0..SIGNALS {
            s.push_str(&format!(" | {} {}", NAMES[k], self.cores[k].take_diag()));
        }
        Some(s)
    }
}

// ------------------------------------------------------------- light signal

const NEVER: u64 = u64::MAX;

fn zeros() -> [AtomicU32; SIGNALS] {
    [
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
    ]
}

/// What the audio reader tells the renderer. Written when audio arrives,
/// sampled every frame by `Envelope`; shared by every output.
pub struct AudioVis {
    base: Instant,
    /// When data last arrived (ms since `base`).
    stamp_ms: AtomicU64,
    /// Latest block peak per signal (f32 bits).
    level: [AtomicU32; SIGNALS],
    /// Time of the last beat per signal (ms since `base`), `NEVER` if none.
    beat_ms: [AtomicU64; SIGNALS],
    beat_strength: [AtomicU32; SIGNALS],
}

impl AudioVis {
    pub fn new(now: Instant) -> Self {
        Self {
            base: now,
            stamp_ms: AtomicU64::new(NEVER),
            level: zeros(),
            beat_ms: [
                AtomicU64::new(NEVER),
                AtomicU64::new(NEVER),
                AtomicU64::new(NEVER),
                AtomicU64::new(NEVER),
            ],
            beat_strength: zeros(),
        }
    }

    fn ms(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.base).as_millis() as u64
    }

    pub fn push(&self, now: Instant, b: &Block) {
        if b.blocks == 0 {
            return;
        }
        let t = self.ms(now);
        self.stamp_ms.store(t, Ordering::Relaxed);
        for k in 0..SIGNALS {
            self.level[k].store(b.level[k].to_bits(), Ordering::Relaxed);
            if let Some(s) = b.beat[k] {
                self.beat_strength[k].store(s.to_bits(), Ordering::Relaxed);
                self.beat_ms[k].store(t, Ordering::Relaxed);
            }
        }
    }
}

/// Smoothed levels and decaying flashes, per signal, for one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub level: [f32; SIGNALS],
    pub flash: [f32; SIGNALS],
}

/// Turns the raw latest-level signal into a smooth, frame-rate independent
/// envelope: fast attack, slow release, so 30 or 60 fps look the same.
#[derive(Default)]
pub struct Envelope {
    env: [f32; SIGNALS],
    last: Option<Instant>,
}

impl Envelope {
    pub fn step(&mut self, vis: &AudioVis, now: Instant, cfg: &Config) -> Frame {
        let dt = self
            .last
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32())
            .min(0.5);
        self.last = Some(now);
        let t_now = vis.ms(now);
        let stamp = vis.stamp_ms.load(Ordering::Relaxed);
        // no data for a while (helper gone, paused): the light fades out
        let fresh = stamp != NEVER && t_now.saturating_sub(stamp) < 400;
        let (gain, floor) = (cfg.audio_gain as f32, cfg.audio_floor as f32);
        let (attack, release) = (
            cfg.audio_attack_ms as f32 / 1000.0,
            cfg.audio_release_ms as f32 / 1000.0,
        );
        let flash_ms = cfg.audio_flash_ms as f32;
        let mut f = Frame::default();
        for k in 0..SIGNALS {
            let raw = if fresh {
                f32::from_bits(vis.level[k].load(Ordering::Relaxed))
            } else {
                0.0
            };
            let target = ((raw * gain - floor) / (1.0 - floor).max(0.01)).clamp(0.0, 1.0);
            let tau = (if target > self.env[k] {
                attack
            } else {
                release
            })
            .max(1e-3);
            self.env[k] += (target - self.env[k]) * (1.0 - (-dt / tau).exp());
            f.level[k] = self.env[k];
            let at = vis.beat_ms[k].load(Ordering::Relaxed);
            if at != NEVER {
                let el = t_now.saturating_sub(at) as f32;
                if el < flash_ms {
                    f.flash[k] = f32::from_bits(vis.beat_strength[k].load(Ordering::Relaxed))
                        .max(0.4)
                        * (-3.0 * el / flash_ms).exp();
                }
            }
        }
        f
    }
}

/// What the renderer applies on top of the board, each 0..=1, already scaled
/// by the config and rounded to 1/16 steps (so an idle envelope changes
/// nothing and a moving one repaints only when a step is crossed).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Light {
    /// Live cells lift towards white.
    pub brighten: f32,
    /// Extra halo around live cells.
    pub glow: f32,
    /// Faint tint of the background.
    pub bg: f32,
    /// Short beat flash (lifts live cells and the background a little).
    pub flash: f32,
}

impl Light {
    pub fn is_neutral(&self) -> bool {
        *self == Light::default()
    }

    /// Bass drives the glow and the flash, mid the brightness, treble the
    /// background shimmer; the full-range beat flashes too.
    pub fn from_frame(f: &Frame, cfg: &Config) -> Light {
        if !cfg.audio || !cfg.audio_react {
            return Light::default();
        }
        let q = |x: f32| (x.clamp(0.0, 1.0) * 16.0).round() / 16.0;
        Light {
            brighten: q(cfg.audio_brighten as f32 * f.level[MID]),
            glow: q(cfg.audio_glow as f32 * f.level[BASS]),
            bg: q(cfg.audio_bg as f32 * f.level[TREBLE]),
            flash: q(cfg.audio_flash as f32 * f.flash[FULL].max(f.flash[BASS])),
        }
    }
}

// ------------------------------------------------------------- the helper

/// The helper process. Dropping it stops the helper (and if we die without
/// getting to that, the helper gets SIGPIPE on its next write and exits).
pub struct Helper(Child);

impl Helper {
    /// How the helper ended, if it has (for the log).
    pub fn status(&mut self) -> Option<String> {
        self.0.try_wait().ok().flatten().map(|s| s.to_string())
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start `cmd` (split on whitespace, no shell) with its stdout as a
/// non-blocking pipe. Errors say why it could not start, e.g. `pw-cat` missing.
pub fn spawn(cmd: &str) -> io::Result<(Helper, ChildStdout)> {
    let mut parts = cmd.split_whitespace();
    let prog = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "audio_command is empty"))?;
    let mut child = Command::new(prog)
        .args(parts)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout pipe"))?;
    // SAFETY: plain fcntl calls on a descriptor we own.
    unsafe {
        let fd = stdout.as_raw_fd();
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            let e = io::Error::last_os_error();
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    }
    Ok((Helper(child), stdout))
}

/// What one non-blocking read of the pipe produced.
pub enum Read {
    Data(usize),
    /// Nothing more right now.
    Empty,
    /// The helper closed its end (exited).
    Closed,
}

pub fn read_nonblocking(out: &ChildStdout, buf: &mut [u8]) -> io::Result<Read> {
    loop {
        // SAFETY: `buf` is valid for `buf.len()` bytes and the fd is open.
        let n = unsafe { libc::read(out.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            return Ok(Read::Data(n as usize));
        }
        if n == 0 {
            return Ok(Read::Closed);
        }
        let e = io::Error::last_os_error();
        match e.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => return Ok(Read::Empty),
            _ => return Err(e),
        }
    }
}

// ------------------------------------------------------------- supervisor

/// Same recording as the default, through PulseAudio's tools (they work on
/// PipeWire too, via pipewire-pulse).
const PAREC: &str = "parec --format=s16le --rate=11025 --channels=1 --device=@DEFAULT_MONITOR@";
const PACAT: &str =
    "pacat --record --format=s16le --rate=11025 --channels=1 --device=@DEFAULT_MONITOR@";

const FIRST_DELAY: Duration = Duration::from_millis(500);

/// A helper that dies this soon without ever producing audio did not work
/// (wrong tool, no sound server); the next candidate is tried.
const EARLY_DEATH: Duration = Duration::from_secs(3);

/// Decides which helper to start and when to start it again. Pure state; the
/// event loop does the I/O. With the stock command it falls back through
/// pw-cat, parec and pacat; with a custom `audio_command` it only ever runs that.
pub struct Supervisor {
    cmds: Vec<String>,
    idx: usize,
    delay: Duration,
    running: bool,
    gave_up: bool,
    retry_at: Option<Instant>,
    started: Option<Instant>,
    got_data: bool,
}

impl Supervisor {
    pub fn new(command: &str) -> Self {
        let cmds = if command == DEFAULT_AUDIO_COMMAND {
            vec![
                DEFAULT_AUDIO_COMMAND.to_string(),
                PAREC.to_string(),
                PACAT.to_string(),
            ]
        } else {
            vec![command.to_string()]
        };
        Self::with_commands(cmds)
    }

    pub fn with_commands(cmds: Vec<String>) -> Self {
        Self {
            cmds,
            idx: 0,
            delay: FIRST_DELAY,
            running: false,
            gave_up: false,
            retry_at: None,
            started: None,
            got_data: false,
        }
    }

    #[cfg(test)]
    pub fn running(&self) -> bool {
        self.running
    }

    pub fn command(&self) -> &str {
        &self.cmds[self.idx.min(self.cmds.len() - 1)]
    }

    /// Time to (re)start the helper?
    pub fn due(&self, now: Instant) -> bool {
        !self.running && !self.gave_up && self.retry_at.map_or(true, |t| now >= t)
    }

    /// Start the first candidate that can be started, beginning with the one
    /// that last worked. On failure the next attempt is scheduled with backoff.
    pub fn start(
        &mut self,
        now: Instant,
        max_delay: Duration,
        restart: bool,
    ) -> io::Result<(Helper, ChildStdout)> {
        let n = self.cmds.len();
        let mut last = None;
        for k in 0..n {
            let i = (self.idx + k) % n;
            match spawn(&self.cmds[i]) {
                Ok(r) => {
                    self.idx = i;
                    self.running = true;
                    self.started = Some(now);
                    self.got_data = false;
                    return Ok(r);
                }
                Err(e) => last = Some(e),
            }
        }
        self.fail(now, max_delay, restart);
        Err(last.unwrap_or_else(|| io::Error::other("no audio command")))
    }

    /// Audio arrived: the helper works, so the backoff starts over.
    pub fn saw_data(&mut self) {
        self.got_data = true;
        self.delay = FIRST_DELAY;
    }

    /// The helper exited (the pipe closed).
    pub fn exited(&mut self, now: Instant, max_delay: Duration, restart: bool) {
        self.running = false;
        let early = !self.got_data
            && self
                .started
                .is_some_and(|t| now.saturating_duration_since(t) < EARLY_DEATH);
        if early && self.cmds.len() > 1 {
            self.idx = (self.idx + 1) % self.cmds.len();
        }
        self.fail(now, max_delay, restart);
    }

    /// The helper was dropped on purpose (nothing is looking at the wallpaper):
    /// start it again as soon as it is wanted, without any backoff.
    pub fn paused(&mut self) {
        self.running = false;
        self.retry_at = None;
    }

    fn fail(&mut self, now: Instant, max_delay: Duration, restart: bool) {
        self.running = false;
        if !restart {
            self.gave_up = true;
            return;
        }
        self.retry_at = Some(now + self.delay);
        self.delay = (self.delay * 2).min(max_delay.max(FIRST_DELAY));
    }

    /// Time until the next attempt (for the log).
    pub fn retry_in(&self, now: Instant) -> Option<Duration> {
        self.retry_at.map(|t| t.saturating_duration_since(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full-range detector, the way the tests below drive it.
    struct BeatDetector(Analyzer);

    impl BeatDetector {
        fn new(sensitivity: f64) -> Self {
            Self(Analyzer::new(sensitivity, [None; 3]))
        }
        fn feed(&mut self, bytes: &[u8]) -> Option<f32> {
            self.0.feed(bytes).beat[FULL]
        }
    }

    /// `blocks` blocks of a square wave at `amp` (0..1 of full scale).
    fn blocks(amp: f32, n: usize) -> Vec<u8> {
        let v = (amp * 32767.0) as i16;
        (0..n * BLOCK)
            .flat_map(|i| (if i % 2 == 0 { v } else { -v }).to_le_bytes())
            .collect()
    }

    /// Run `pattern` (amplitude per block) through a detector; the indices of
    /// the blocks that were beats.
    fn beats(sensitivity: f64, pattern: &[f32]) -> Vec<usize> {
        let mut d = BeatDetector::new(sensitivity);
        let mut out = Vec::new();
        for (i, &a) in pattern.iter().enumerate() {
            if d.feed(&blocks(a, 1)).is_some() {
                out.push(i);
            }
        }
        out
    }

    #[test]
    fn silence_and_a_steady_tone_never_beat() {
        assert!(beats(1.0, &[0.0; 200]).is_empty());
        assert!(
            beats(1.0, &[0.3; 200]).is_empty(),
            "a constant level is not a beat"
        );
        assert!(beats(1.0, &[0.01; 200]).is_empty(), "hiss below the floor");
    }

    #[test]
    fn kicks_over_a_quiet_bed_beat_once_each() {
        // 0.1 bed with a 0.8 kick every 12 blocks (~550 ms), each kick two blocks long
        let mut p = vec![0.1; 200];
        let kicks = [30, 42, 54, 66, 78, 90];
        for &k in &kicks {
            p[k] = 0.8;
            p[k + 1] = 0.7;
        }
        let b = beats(1.0, &p);
        assert_eq!(b, kicks.to_vec(), "one beat per kick, none in between");
    }

    #[test]
    fn refractory_period_stops_double_triggers() {
        let mut p = vec![0.1; 120];
        for i in [40, 42, 44] {
            p[i] = 0.9; // 92 ms apart: inside the refractory window
        }
        assert_eq!(beats(1.0, &p), vec![40]);
    }

    #[test]
    fn two_beats_in_one_read_are_still_two_beats() {
        // one big read holding bed, kick, gap, kick: refractory is counted in audio, not arrival time
        let mut bytes = blocks(0.1, 40);
        for i in 0..14 {
            bytes.extend(blocks(if i == 0 || i == 12 { 0.9 } else { 0.1 }, 1));
        }
        let mut d = BeatDetector::new(1.0);
        let mut n = 0;
        // feed block by block through one buffer to count every beat, not just the strongest
        for chunk in bytes.chunks(BLOCK * 2) {
            n += d.feed(chunk).is_some() as usize;
        }
        assert_eq!(n, 2);
        // ... and one feed() over everything reports the strongest of them
        let mut d = BeatDetector::new(1.0);
        assert!(d.feed(&bytes).is_some());
    }

    #[test]
    fn sensitivity_decides_how_soft_a_beat_may_be() {
        let mut p = vec![0.2; 120];
        p[60] = 0.34; // 1.7x the bed
        assert!(beats(0.5, &p).is_empty(), "picky ignores it");
        assert_eq!(beats(2.0, &p), vec![60], "touchy hears it");
    }

    #[test]
    fn samples_split_across_reads_and_odd_byte_counts_are_fine() {
        let mut d = BeatDetector::new(1.0);
        let bed = blocks(0.1, 40);
        for chunk in bed.chunks(333) {
            // 333 is odd; the detector only ever sees whole samples here
            let _ = d.feed(&chunk[..chunk.len() & !1]);
        }
        let kick = blocks(0.9, 1);
        let mut hit = None;
        for chunk in kick.chunks(100) {
            hit = hit.or(d.feed(chunk));
        }
        assert!(
            hit.is_some(),
            "a block assembled from many small reads still counts"
        );
    }

    #[test]
    fn strength_is_within_zero_to_one() {
        let mut d = BeatDetector::new(1.0);
        d.feed(&blocks(0.05, 40));
        let s = d.feed(&blocks(1.0, 1)).unwrap();
        assert!((0.0..=1.0).contains(&s));
    }

    #[test]
    fn spawn_reads_a_helper_process_and_notices_when_it_exits() {
        // `printf` stands in for pw-cat: 4 bytes of "audio", then exit
        let (_helper, stdout) = spawn("printf abcd").expect("printf exists");
        let mut buf = [0u8; 16];
        let mut got = 0;
        let mut closed = false;
        for _ in 0..200 {
            match read_nonblocking(&stdout, &mut buf).unwrap() {
                Read::Data(n) => got += n,
                Read::Closed => {
                    closed = true;
                    break;
                }
                Read::Empty => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        assert_eq!(got, 4);
        assert!(closed);
        assert!(spawn("definitely-not-a-real-program-xyz").is_err());
        assert!(spawn("   ").is_err());
    }

    /// `n` samples of a sine at `hz`, amplitude `amp`.
    fn sine(hz: f32, amp: f32, n: usize) -> Vec<u8> {
        (0..n)
            .flat_map(|i| {
                (((std::f32::consts::TAU * hz * i as f32 / RATE).sin() * amp * 32767.0) as i16)
                    .to_le_bytes()
            })
            .collect()
    }

    #[test]
    fn bands_separate_low_and_high_tones() {
        let mut a = Analyzer::new(1.0, [None; 3]);
        let low = a.feed(&sine(80.0, 0.5, BLOCK * 6));
        assert!(low.level[BASS] > 0.3, "{low:?}");
        assert!(low.level[TREBLE] < 0.1, "{low:?}");
        let mut a = Analyzer::new(1.0, [None; 3]);
        let high = a.feed(&sine(3500.0, 0.5, BLOCK * 6));
        assert!(high.level[TREBLE] > 0.2, "{high:?}");
        assert!(high.level[BASS] < 0.1, "{high:?}");
        assert!(high.level[FULL] > 0.4);
    }

    #[test]
    fn a_bass_kick_is_a_bass_beat_and_mostly_bass() {
        let mut a = Analyzer::new(1.0, [None; 3]);
        a.feed(&sine(80.0, 0.05, BLOCK * 40));
        let kick = a.feed(&sine(80.0, 0.8, BLOCK));
        assert!(kick.beat[BASS].is_some(), "{kick:?}");
        assert!(
            kick.level[BASS] > kick.level[TREBLE] * 5.0,
            "a low tone is mostly bass: {kick:?}"
        );
    }

    #[test]
    fn a_band_sensitivity_overrides_the_master() {
        let bed = sine(80.0, 0.2, BLOCK * 40);
        let bump = sine(80.0, 0.34, BLOCK);
        let mut picky = Analyzer::new(2.0, [Some(0.5), None, None]); // master touchy, bass picky
        picky.feed(&bed);
        assert!(picky.feed(&bump).beat[BASS].is_none());
        let mut follow = Analyzer::new(2.0, [None; 3]);
        follow.feed(&bed);
        assert!(follow.feed(&bump).beat[BASS].is_some());
    }

    #[test]
    fn a_sample_split_across_reads_is_joined() {
        let bytes = sine(200.0, 0.5, BLOCK * 3);
        let mut whole = Analyzer::new(1.0, [None; 3]);
        let a = whole.feed(&bytes);
        let mut split = Analyzer::new(1.0, [None; 3]);
        let mut b = Block::default();
        for chunk in bytes.chunks(333) {
            let r = split.feed(chunk);
            b.blocks += r.blocks;
            for k in 0..SIGNALS {
                b.level[k] = b.level[k].max(r.level[k]);
            }
        }
        assert_eq!(a.blocks, b.blocks);
        for k in 0..SIGNALS {
            assert!((a.level[k] - b.level[k]).abs() < 1e-4);
        }
    }

    #[test]
    fn verbose_report_comes_about_once_a_second_with_every_band() {
        let mut a = Analyzer::new(1.0, [None; 3]);
        a.feed(&sine(100.0, 0.3, BLOCK * 10));
        assert!(a.report().is_none());
        a.feed(&sine(100.0, 0.3, BLOCK * 13));
        let line = a.report().expect("22+ blocks seen");
        for name in NAMES {
            assert!(line.contains(name), "{line}");
        }
        assert!(a.report().is_none(), "the counters start over");
    }

    #[test]
    fn envelope_rises_fast_falls_slowly_and_ignores_the_frame_rate() {
        let cfg = Config {
            audio: true,
            audio_gain: 1.0,
            audio_floor: 0.0,
            audio_attack_ms: 30,
            audio_release_ms: 300,
            ..Config::default()
        };
        let t0 = Instant::now();
        let loud = Block {
            level: [0.8; SIGNALS],
            beat: [None; SIGNALS],
            blocks: 1,
        };
        let run = |fps: u64| {
            let vis = AudioVis::new(t0);
            let mut env = Envelope::default();
            let mut now = t0;
            env.step(&vis, now, &cfg);
            vis.push(now, &loud);
            let mut after_attack = 0.0;
            for i in 1..=(fps / 5) {
                // 200 ms of loud audio, kept fresh
                now = t0 + Duration::from_micros(i * 1_000_000 / fps);
                vis.push(now, &loud);
                after_attack = env.step(&vis, now, &cfg).level[0];
            }
            (after_attack, vis, env, now)
        };
        let (a30, ..) = run(30);
        let (a60, ..) = run(60);
        assert!(a30 > 0.7, "attack is fast: {a30}");
        assert!(
            (a30 - a60).abs() < 0.03,
            "30 and 60 fps agree: {a30} vs {a60}"
        );
        // silence: the data goes stale and the envelope decays, not snaps
        let (_, vis, mut env, now) = run(30);
        let soon = env.step(&vis, now + Duration::from_millis(600), &cfg).level[0];
        let later = env
            .step(&vis, now + Duration::from_millis(2000), &cfg)
            .level[0];
        assert!(soon > 0.0 && soon < 0.8, "{soon}");
        assert!(later < soon, "{later} < {soon}");
    }

    #[test]
    fn a_beat_flashes_briefly_and_the_light_is_quantised() {
        let cfg = Config {
            audio: true,
            audio_flash_ms: 150,
            audio_flash: 1.0,
            ..Config::default()
        };
        let t0 = Instant::now();
        let vis = AudioVis::new(t0);
        let mut env = Envelope::default();
        let mut beat = Block {
            blocks: 1,
            ..Block::default()
        };
        beat.beat[FULL] = Some(1.0);
        vis.push(t0, &beat);
        let f0 = env.step(&vis, t0, &cfg);
        assert!(f0.flash[FULL] > 0.9);
        let f1 = env.step(&vis, t0 + Duration::from_millis(60), &cfg);
        assert!(f1.flash[FULL] > 0.0 && f1.flash[FULL] < f0.flash[FULL]);
        let f2 = env.step(&vis, t0 + Duration::from_millis(200), &cfg);
        assert_eq!(f2.flash[FULL], 0.0, "gone after audio_flash_ms");
        let l = Light::from_frame(&f0, &cfg);
        assert!(l.flash > 0.5);
        assert_eq!(l.flash * 16.0, (l.flash * 16.0).round(), "1/16 steps");
        let off = Config {
            audio_react: false,
            ..cfg.clone()
        };
        assert!(
            Light::from_frame(&f0, &off).is_neutral(),
            "no light without audio_react"
        );
        let no_audio = Config {
            audio: false,
            ..cfg
        };
        assert!(
            Light::from_frame(&f0, &no_audio).is_neutral(),
            "no light without audio"
        );
    }

    #[test]
    fn supervisor_falls_back_to_the_next_tool_and_backs_off() {
        let now = Instant::now();
        let mut s = Supervisor::with_commands(vec![
            "definitely-not-a-real-program-1".into(),
            "printf abcd".into(),
        ]);
        assert!(s.due(now));
        let started = s.start(now, Duration::from_secs(30), true);
        assert!(started.is_ok(), "falls through to the second command");
        assert_eq!(s.command(), "printf abcd");
        assert!(s.running() && !s.due(now));
        // it dies at once without producing audio: counts as a failure, next candidate, backoff
        s.exited(
            now + Duration::from_millis(50),
            Duration::from_secs(30),
            true,
        );
        assert!(!s.running() && !s.due(now + Duration::from_millis(100)));
        assert!(
            s.due(now + Duration::from_millis(600)),
            "first retry after 0.5 s"
        );
        // the delays double up to the cap
        let mut gaps = Vec::new();
        for _ in 0..8 {
            let t = now + Duration::from_secs(100);
            s.exited(t, Duration::from_secs(4), true);
            gaps.push(s.retry_in(t).unwrap().as_millis());
        }
        assert_eq!(gaps[..4], [1000, 2000, 4000, 4000], "{gaps:?}");
    }

    #[test]
    fn supervisor_resets_after_audio_and_can_be_told_not_to_restart() {
        let now = Instant::now();
        let mut s = Supervisor::with_commands(vec!["printf x".into()]);
        s.start(now, Duration::from_secs(30), true).unwrap();
        s.exited(now, Duration::from_secs(30), true);
        s.exited(now, Duration::from_secs(30), true);
        s.saw_data();
        let t = now + Duration::from_secs(60);
        s.exited(t, Duration::from_secs(30), true);
        assert_eq!(
            s.retry_in(t).unwrap().as_millis(),
            500,
            "a healthy run resets the backoff"
        );
        let mut off = Supervisor::with_commands(vec!["printf x".into()]);
        off.start(now, Duration::from_secs(30), false).unwrap();
        off.exited(now, Duration::from_secs(30), false);
        assert!(
            !off.due(now + Duration::from_secs(3600)),
            "restart off: stays down"
        );
        // a pause is not a failure: no backoff
        let mut p = Supervisor::with_commands(vec!["printf x".into()]);
        p.start(now, Duration::from_secs(30), true).unwrap();
        p.paused();
        assert!(p.due(now));
    }

    #[test]
    fn a_custom_command_has_no_fallbacks() {
        let s = Supervisor::new("my-recorder --raw");
        assert_eq!(s.cmds.len(), 1);
        assert_eq!(Supervisor::new(DEFAULT_AUDIO_COMMAND).cmds.len(), 3);
        let mut dead = Supervisor::new("definitely-not-a-real-program-2");
        assert!(dead
            .start(Instant::now(), Duration::from_secs(30), true)
            .is_err());
        assert!(
            !dead.due(Instant::now()),
            "scheduled for later, not hammered"
        );
    }
}
