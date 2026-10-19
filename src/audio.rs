//! Audio reactivity (off by default, `audio = true`). Gliders on beats.
//!
//! No audio library is linked in: a helper process (by default PipeWire's own
//! `pw-cat`, recording the monitor of the default output, so it hears whatever
//! is playing) writes raw mono 16-bit samples to a pipe. This module spawns
//! it, and a small detector turns the samples into beats. With `audio = false`
//! nothing here runs, so the cost when unused is zero: no process, no thread,
//! no extra dependency at build time.
//!
//! The detector is a peak meter with a moving average: a block of samples is a
//! beat when its peak is clearly above the recent average and well above the
//! noise floor, and a short refractory period follows each beat.

use std::{
    io,
    os::fd::AsRawFd,
    process::{Child, ChildStdout, Command, Stdio},
};

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

pub struct BeatDetector {
    /// Beat when peak > avg * ratio.
    ratio: f32,
    avg: f32,
    carry: Vec<i16>,
    /// Block number of the last beat.
    last_beat: Option<u64>,
    /// Blocks seen so far; the first few only train the average.
    blocks: u64,
}

impl BeatDetector {
    /// `sensitivity` 1.0 = beat when a block peaks 1.8x above the recent
    /// average; higher is touchier (2.0: 1.4x), lower is pickier (0.5: 2.6x).
    pub fn new(sensitivity: f64) -> Self {
        Self {
            ratio: 1.0 + 0.8 / sensitivity.clamp(0.2, 5.0) as f32,
            avg: 0.0,
            carry: Vec::with_capacity(BLOCK),
            last_beat: None,
            blocks: 0,
        }
    }

    /// Feed raw bytes (s16le mono). Returns the strength (0..=1) of the
    /// strongest beat found, if any.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<f32> {
        let mut best: Option<f32> = None;
        for pair in bytes.chunks_exact(2) {
            self.carry.push(i16::from_le_bytes([pair[0], pair[1]]));
            if self.carry.len() == BLOCK {
                let peak = self.carry.iter().map(|&s| (s as f32).abs()).fold(0.0, f32::max) / 32768.0;
                self.carry.clear();
                if let Some(s) = self.block(peak) {
                    best = Some(best.map_or(s, |b: f32| b.max(s)));
                }
            }
        }
        best
    }

    fn block(&mut self, peak: f32) -> Option<f32> {
        self.blocks += 1;
        if self.blocks <= WARMUP_BLOCKS {
            self.avg += (peak - self.avg) / self.blocks as f32;
            return None;
        }
        // Compare against the average from *before* this block, so a kick
        // does not raise the bar it has to clear.
        let before = self.avg;
        self.avg += (peak - before) / if peak > before { RISE_BLOCKS } else { FALL_BLOCKS };
        if peak < FLOOR || peak <= before * self.ratio {
            return None;
        }
        if self.last_beat.is_some_and(|b| self.blocks - b < REFRACTORY_BLOCKS) {
            return None;
        }
        self.last_beat = Some(self.blocks);
        // How far above the threshold: ratio..2*ratio maps to 0..1
        Some(((peak / before.max(1e-4) - self.ratio) / self.ratio).clamp(0.0, 1.0))
    }
}

/// The helper process. Dropping it stops the helper (and if we die without
/// getting to that, the helper gets SIGPIPE on its next write and exits).
pub struct Helper(Child);

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
    let prog = parts.next().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "audio_command is empty"))?;
    let mut child = Command::new(prog).args(parts).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
    let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout pipe"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `blocks` blocks of a square wave at `amp` (0..1 of full scale).
    fn blocks(amp: f32, n: usize) -> Vec<u8> {
        let v = (amp * 32767.0) as i16;
        (0..n * BLOCK).flat_map(|i| (if i % 2 == 0 { v } else { -v }).to_le_bytes()).collect()
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
        assert!(beats(1.0, &[0.3; 200]).is_empty(), "a constant level is not a beat");
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
        assert!(hit.is_some(), "a block assembled from many small reads still counts");
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
}
