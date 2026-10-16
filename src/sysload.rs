//! CPU utilisation from /proc/stat, smoothed. Used by `sysload` mode.

pub struct CpuLoad {
    prev: Option<(u64, u64)>, // (idle, total)
    ema: f32,
    buf: String,
}

/// Parse the aggregate "cpu ..." line into (idle, total) jiffies.
pub fn parse_cpu_line(line: &str) -> Option<(u64, u64)> {
    let mut it = line.split_whitespace();
    if it.next()? != "cpu" {
        return None;
    }
    let (mut idle, mut total) = (0u64, 0u64);
    for (i, v) in it.take(8).enumerate() {
        let v: u64 = v.parse().ok()?;
        total += v;
        if i == 3 || i == 4 {
            idle += v; // idle + iowait
        }
    }
    (total > 0).then_some((idle, total))
}

impl CpuLoad {
    pub fn new() -> Self {
        Self { prev: None, ema: 0.0, buf: String::new() }
    }

    /// 0.0 (idle) ..= 1.0 (all cores busy).
    pub fn value(&self) -> f32 {
        self.ema
    }

    pub fn sample(&mut self) {
        use std::io::Read;
        self.buf.clear();
        let ok = std::fs::File::open("/proc/stat").and_then(|mut f| f.read_to_string(&mut self.buf)).is_ok();
        if !ok {
            return;
        }
        let Some((idle, total)) = self.buf.lines().next().and_then(parse_cpu_line) else { return };
        if let Some((pi, pt)) = self.prev {
            let (di, dt) = (idle.saturating_sub(pi), total.saturating_sub(pt));
            if dt > 0 {
                let load = 1.0 - di as f32 / dt as f32;
                self.ema += 0.35 * (load.clamp(0.0, 1.0) - self.ema);
            }
        }
        self.prev = Some((idle, total));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_stat_line() {
        let (idle, total) = parse_cpu_line("cpu  100 0 50 800 50 0 0 0 0 0").unwrap();
        assert_eq!(idle, 850);
        assert_eq!(total, 1000);
        assert!(parse_cpu_line("cpu0 1 2 3").is_none());
    }

    #[test]
    fn real_sample_does_not_panic() {
        let mut c = CpuLoad::new();
        c.sample();
        c.sample();
        assert!((0.0..=1.0).contains(&c.value()));
    }
}