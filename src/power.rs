//! Battery and AC state from `/sys/class/power_supply` (`power = true`).
//!
//! Read at a low rate by the event loop, never by anything that draws.
//! Machines without a battery always report "not on battery".

use std::{fs, path::Path};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Power {
    /// Running on battery (no mains supply online and a battery discharging).
    pub on_battery: bool,
    /// Charge of the first battery, in percent.
    pub percent: Option<u32>,
}

impl Power {
    /// Low-power mode: on battery, at or below `threshold` percent. A battery
    /// with no readable charge counts as low (better dim than drained).
    pub fn low(&self, threshold: u32) -> bool {
        self.on_battery && self.percent.map_or(true, |p| p <= threshold)
    }
}

fn read_trim(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// Look at every supply under `root`.
pub fn read_from(root: &Path) -> Power {
    let Ok(entries) = fs::read_dir(root) else {
        return Power::default();
    };
    let (mut mains_online, mut discharging, mut has_battery) = (false, false, false);
    let mut percent = None;
    for e in entries.flatten() {
        let dir = e.path();
        match read_trim(&dir.join("type")).as_deref() {
            Some("Mains") | Some("USB") | Some("USB_C") | Some("USB_PD") => {
                if read_trim(&dir.join("online")).as_deref() == Some("1") {
                    mains_online = true;
                }
            }
            Some("Battery") => {
                // peripherals (mice, headsets) show up as batteries with scope Device
                if read_trim(&dir.join("scope")).as_deref() == Some("Device") {
                    continue;
                }
                has_battery = true;
                if read_trim(&dir.join("status")).as_deref() == Some("Discharging") {
                    discharging = true;
                }
                if percent.is_none() {
                    percent = read_trim(&dir.join("capacity")).and_then(|c| c.parse().ok());
                }
            }
            _ => {}
        }
    }
    Power {
        on_battery: has_battery && discharging && !mains_online,
        percent,
    }
}

pub fn read() -> Power {
    read_from(Path::new("/sys/class/power_supply"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supply(root: &Path, name: &str, files: &[(&str, &str)]) {
        let d = root.join(name);
        fs::create_dir_all(&d).unwrap();
        for (k, v) in files {
            fs::write(d.join(k), format!("{v}\n")).unwrap();
        }
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lw-power-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ac_battery_and_desktop_states() {
        let d = tmp("a");
        supply(&d, "AC", &[("type", "Mains"), ("online", "1")]);
        supply(
            &d,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Charging"),
                ("capacity", "55"),
            ],
        );
        assert_eq!(
            read_from(&d),
            Power {
                on_battery: false,
                percent: Some(55)
            }
        );

        let d = tmp("b");
        supply(&d, "AC", &[("type", "Mains"), ("online", "0")]);
        supply(
            &d,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Discharging"),
                ("capacity", "30"),
            ],
        );
        let p = read_from(&d);
        assert!(p.on_battery && p.percent == Some(30));
        assert!(p.low(40) && !p.low(20));

        let d = tmp("c"); // a desktop: no battery at all
        supply(&d, "AC", &[("type", "Mains"), ("online", "1")]);
        assert!(!read_from(&d).on_battery);
        assert_eq!(
            read_from(Path::new("/definitely/not/here")),
            Power::default()
        );
    }

    #[test]
    fn a_mouse_battery_is_not_the_laptop() {
        let d = tmp("d");
        supply(
            &d,
            "hid-1",
            &[
                ("type", "Battery"),
                ("scope", "Device"),
                ("status", "Discharging"),
                ("capacity", "10"),
            ],
        );
        assert!(!read_from(&d).on_battery);
    }
}
