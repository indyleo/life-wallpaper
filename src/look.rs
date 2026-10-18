//! Slow, global colour changes: hue drift and night dimming. Everything here
//! is quantised so a `Look` only changes every few seconds to minutes; the
//! crossfade then repaints the board gently instead of every generation.

use std::time::Duration;

use crate::config::{Config, Rgb};

/// What the palette is currently doing. `Default` = untouched.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Look {
    /// Hue rotation in whole degrees.
    pub hue: i16,
    /// Brightness removed, in 1/32ths (0..=26).
    pub dim: u8,
}

impl Look {
    pub fn hue_deg(&self) -> f32 {
        self.hue as f32
    }
    pub fn dim_frac(&self) -> f32 {
        self.dim as f32 / 32.0
    }
    #[cfg(test)]
    pub fn is_neutral(&self) -> bool {
        self.hue == 0 && self.dim == 0
    }
}

/// Minutes into the ramp between day and night (and back).
const RAMP_MIN: f32 = 45.0;

/// 0.0 = full day .. 1.0 = full night for a local time of `minutes` after
/// midnight. Night runs from `start_h` to `end_h` (it may wrap midnight) with
/// a 45 minute ramp at both ends, so the screen never snaps.
pub fn night_factor(minutes: u32, start_h: u32, end_h: u32) -> f32 {
    let m = (minutes % 1440) as f32;
    let (s, e) = ((start_h % 24) as f32 * 60.0, (end_h % 24) as f32 * 60.0);
    if s == e {
        return 0.0;
    }
    // minutes since night started / until night ends, wrapping the day
    let since_start = (m - s).rem_euclid(1440.0);
    let night_len = (e - s).rem_euclid(1440.0);
    if since_start >= night_len {
        // in daylight: the ramp covers the last RAMP_MIN of day before `start`
        return ramp_in(1440.0 - since_start);
    }
    // in the night: full, except for the ramp back to day before `end`
    let until_end = night_len - since_start;
    if until_end < RAMP_MIN {
        until_end / RAMP_MIN
    } else {
        1.0
    }
}

/// Daylight side of the ramp: just before night starts the screen already
/// begins to dim.
fn ramp_in(until_start: f32) -> f32 {
    if until_start < RAMP_MIN {
        1.0 - until_start / RAMP_MIN
    } else {
        0.0
    }
}

/// Local time as minutes after midnight, or `None` if it cannot be read.
pub fn local_minutes() -> Option<u32> {
    // SAFETY: `time` and `localtime_r` are plain libc calls writing into a
    // zeroed `tm` we own.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        Some((tm.tm_hour.clamp(0, 23) * 60 + tm.tm_min.clamp(0, 59)) as u32)
    }
}

/// The look for `elapsed` time since start and the local time of day.
pub fn compute(cfg: &Config, elapsed: Duration, minutes: Option<u32>) -> Look {
    let mut look = Look::default();
    if cfg.hue_drift > 0.0 {
        let phase = elapsed.as_secs_f64() / cfg.hue_drift_secs.max(1) as f64 * std::f64::consts::TAU;
        look.hue = (cfg.hue_drift * phase.sin()).round() as i16;
    }
    if cfg.night_dim > 0.0 {
        if let Some(m) = minutes {
            let f = night_factor(m, cfg.night_start, cfg.night_end) as f64;
            look.dim = (cfg.night_dim * f * 32.0).round().clamp(0.0, 26.0) as u8;
        }
    }
    look
}

/// Rotate the hue of `c` by `deg` degrees in YIQ space (keeps brightness).
pub fn rotate_hue(c: Rgb, deg: f32) -> Rgb {
    if deg == 0.0 {
        return c;
    }
    let (r, g, b) = (c[0] as f32, c[1] as f32, c[2] as f32);
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let i = 0.596 * r - 0.274 * g - 0.322 * b;
    let q = 0.211 * r - 0.523 * g + 0.312 * b;
    let (s, co) = deg.to_radians().sin_cos();
    let (i2, q2) = (i * co - q * s, i * s + q * co);
    let to = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    [to(y + 0.956 * i2 + 0.621 * q2), to(y - 0.272 * i2 - 0.647 * q2), to(y - 1.106 * i2 + 1.703 * q2)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn night_factor_covers_day_night_and_ramps() {
        let f = |h: u32, m: u32| night_factor(h * 60 + m, 22, 7);
        assert_eq!(f(12, 0), 0.0);
        assert_eq!(f(3, 0), 1.0);
        assert_eq!(f(23, 30), 1.0);
        assert!(f(21, 30) > 0.0 && f(21, 30) < 1.0, "ramp in before 22:00");
        assert!(f(6, 30) > 0.0 && f(6, 30) < 1.0, "ramp out before 07:00");
        assert_eq!(f(7, 30), 0.0);
        assert_eq!(night_factor(100, 5, 5), 0.0, "start == end means never");
        // a night that does not wrap midnight
        assert_eq!(night_factor(13 * 60, 12, 14), 1.0);
        assert_eq!(night_factor(9 * 60, 12, 14), 0.0);
    }

    #[test]
    fn hue_rotation_is_identity_at_zero_and_full_turn() {
        let c = [0xfa, 0xbd, 0x2f];
        assert_eq!(rotate_hue(c, 0.0), c);
        let back = rotate_hue(c, 360.0);
        for k in 0..3 {
            assert!((back[k] as i32 - c[k] as i32).abs() <= 2, "{back:?} vs {c:?}");
        }
        assert_ne!(rotate_hue(c, 40.0), c);
        let grey = [0x28, 0x28, 0x28];
        let g2 = rotate_hue(grey, 77.0);
        for k in 0..3 {
            assert!((g2[k] as i32 - 0x28).abs() <= 2, "grey must stay grey");
        }
    }

    #[test]
    fn compute_is_neutral_when_off_and_bounded_when_on() {
        let off = Config::default();
        assert!(compute(&off, Duration::from_secs(123), Some(23 * 60)).is_neutral());
        let on = Config { hue_drift: 20.0, night_dim: 0.5, ..Config::default() };
        for t in (0..3600).step_by(37) {
            let l = compute(&on, Duration::from_secs(t), Some(2 * 60));
            assert!(l.hue.abs() <= 20);
            assert_eq!(l.dim, 16);
        }
        assert_eq!(compute(&on, Duration::ZERO, None).dim, 0, "no clock, no dimming");
    }
}
