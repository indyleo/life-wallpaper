//! Outside events: the `command` line protocol and the weather parser. No
//! JSON library: events are flat objects, and the weather answer is searched
//! for the two values we need.
//!
//! One event per line from `command`, either a flat JSON object
//!
//! ```text
//! {"inject":"glider","edge":"left"}      {"inject":"lwss","at":[10,5]}
//! {"text":"hello"}                        {"reseed":true,"rule":"highlife","density":0.3}
//! {"set":"glow","value":0.5}              {"pause":true}   {"resume":true}
//! {"load_scene":"night"}                  {"save_scene":"night"}   {"stats":true}
//! ```
//!
//! or the control socket's own syntax (`inject glider --edge left`).

use crate::ipc::{self, Command};

#[derive(Clone, Debug, PartialEq)]
enum Val {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
    Arr(Vec<f64>),
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected {:?} at column {}", c as char, self.i + 1))
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or("unterminated string")?;
                    self.i += 1;
                    out.push(match e {
                        b'n' => b'\n',
                        b't' => b'\t',
                        b'r' => b'\r',
                        b'"' | b'\\' | b'/' => e,
                        _ => {
                            return Err(
                                "only \\n \\t \\r \\\" \\\\ \\/ escapes are supported".into()
                            )
                        }
                    });
                }
                _ => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "text is not UTF-8".to_string())
    }

    fn number(&mut self) -> Result<f64, String> {
        self.ws();
        let start = self.i;
        while self.i < self.b.len()
            && matches!(
                self.b[self.i],
                b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
            )
        {
            self.i += 1;
        }
        std::str::from_utf8(&self.b[start..self.i])
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| format!("bad number at column {}", start + 1))
    }

    fn word(&mut self, w: &str) -> bool {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Val, String> {
        self.ws();
        match self.b.get(self.i) {
            Some(b'"') => self.string().map(Val::Str),
            Some(b'[') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Val::Arr(v));
                }
                loop {
                    v.push(self.number()?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Val::Arr(v));
                        }
                        _ => return Err("expected , or ] in an array of numbers".into()),
                    }
                }
            }
            Some(_) if self.word("true") => Ok(Val::Bool(true)),
            Some(_) if self.word("false") => Ok(Val::Bool(false)),
            Some(_) if self.word("null") => Ok(Val::Null),
            Some(_) => self.number().map(Val::Num),
            None => Err("a value is missing".into()),
        }
    }
}

/// A flat `{"key": value, ...}` object.
fn flat_object(line: &str) -> Result<Vec<(String, Val)>, String> {
    let mut p = P {
        b: line.as_bytes(),
        i: 0,
    };
    p.eat(b'{')?;
    let mut out = Vec::new();
    p.ws();
    if p.b.get(p.i) == Some(&b'}') {
        return Ok(out);
    }
    loop {
        let k = p.string()?;
        p.eat(b':')?;
        out.push((k, p.value()?));
        p.ws();
        match p.b.get(p.i) {
            Some(b',') => p.i += 1,
            Some(b'}') => {
                p.i += 1;
                break;
            }
            _ => return Err("expected , or } after a value".into()),
        }
    }
    p.ws();
    if p.i != p.b.len() {
        return Err("text after the closing }".into());
    }
    Ok(out)
}

fn text_of(v: &Val) -> String {
    match v {
        Val::Str(s) => s.clone(),
        Val::Num(n) => n.to_string(),
        Val::Bool(b) => b.to_string(),
        Val::Null => String::new(),
        Val::Arr(a) => a
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(","),
    }
}

/// One line from `command` as a control-socket command.
pub fn parse_event(line: &str) -> Result<Command, String> {
    let line = line.trim();
    if !line.starts_with('{') {
        return ipc::parse(line);
    }
    let obj = flat_object(line)?;
    let get = |k: &str| obj.iter().find(|(key, _)| key == k).map(|(_, v)| v);
    let at = match get("at") {
        Some(Val::Arr(a)) if a.len() == 2 && a.iter().all(|&n| n >= 0.0) => {
            Some((a[0] as usize, a[1] as usize))
        }
        Some(_) => return Err("\"at\" must be [x, y] with cell coordinates".into()),
        None => None,
    };
    let edge = get("edge").map(text_of);
    if let Some(v) = get("inject") {
        return Ok(Command::Inject {
            name: text_of(v),
            at,
            edge,
        });
    }
    if let Some(v) = get("text") {
        return Ok(Command::Inject {
            name: format!("text={}", text_of(v)),
            at,
            edge: None,
        });
    }
    if get("reseed").is_some() {
        return Ok(Command::Reseed {
            rule: get("rule").map(text_of),
            density: get("density").map(text_of),
        });
    }
    if let Some(k) = get("set") {
        let v = get("value").ok_or("\"set\" needs a \"value\"")?;
        return Ok(Command::Set(text_of(k), text_of(v)));
    }
    for (key, cmd) in [
        ("pause", Command::Pause),
        ("resume", Command::Resume),
        ("quit", Command::Quit),
        ("stats", Command::Stats),
    ] {
        if matches!(get(key), Some(Val::Bool(true))) {
            return Ok(cmd);
        }
    }
    if let Some(v) = get("load_scene") {
        return Ok(Command::LoadScene(text_of(v)));
    }
    if let Some(v) = get("save_scene") {
        return Ok(Command::SaveScene(text_of(v)));
    }
    Err("no known key (inject, text, reseed, set, pause, resume, quit, stats, load_scene, save_scene)".into())
}

// ------------------------------------------------------------------ weather

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sky {
    Clear,
    Cloud,
    Fog,
    Rain,
    Storm,
    Snow,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weather {
    pub sky: Sky,
    pub temp_c: Option<f64>,
}

/// The first value of `"key"` after `from`: a quoted string or a bare token.
fn find_value(text: &str, key: &str, from: usize) -> Option<String> {
    let pat = format!("\"{key}\"");
    let at = from + text.get(from..)?.find(&pat)? + pat.len();
    let rest = text[at..].trim_start().strip_prefix(':')?.trim_start();
    if let Some(q) = rest.strip_prefix('"') {
        let end = q.find('"')?;
        Some(q[..end].to_string())
    } else {
        let end = rest
            .find(|c: char| c == ',' || c == '}' || c == ']' || c.is_whitespace())
            .unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

/// Read a wttr.in `format=j1` answer (or anything shaped like it):
/// `temp_C` and the first `weatherDesc` / `value`.
pub fn parse_weather(text: &str) -> Option<Weather> {
    let temp_c = find_value(text, "temp_C", 0).and_then(|t| t.trim().parse::<f64>().ok());
    let desc_at = text.find("\"weatherDesc\"");
    let desc = desc_at
        .and_then(|i| find_value(text, "value", i))
        .map(|d| d.to_ascii_lowercase());
    if desc.is_none() && temp_c.is_none() {
        return None;
    }
    let d = desc.unwrap_or_default();
    let has = |w: &[&str]| w.iter().any(|x| d.contains(x));
    let sky = if has(&["thunder", "storm"]) {
        Sky::Storm
    } else if has(&["snow", "sleet", "blizzard", "ice pellets", "freezing"]) {
        Sky::Snow
    } else if has(&["rain", "drizzle", "shower"]) {
        Sky::Rain
    } else if has(&["fog", "mist", "haze"]) {
        Sky::Fog
    } else if has(&["cloud", "overcast"]) {
        Sky::Cloud
    } else {
        Sky::Clear
    };
    Some(Weather { sky, temp_c })
}

impl Weather {
    /// A hue shift in degrees: cold leans blue, heat leans warm, wet weather a little cooler.
    pub fn hue(&self) -> i16 {
        let t = match self.temp_c {
            Some(t) if t <= 0.0 => -40,
            Some(t) if t <= 10.0 => -20,
            Some(t) if t >= 30.0 => 30,
            Some(t) if t >= 24.0 => 15,
            _ => 0,
        };
        let s = match self.sky {
            Sky::Rain | Sky::Storm => -15,
            Sky::Snow => -25,
            _ => 0,
        };
        (t + s).clamp(-90, 90)
    }

    pub fn rains(&self) -> bool {
        matches!(self.sky, Sky::Rain | Sky::Storm)
    }

    pub fn snows(&self) -> bool {
        self.sky == Sky::Snow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_events_map_to_commands() {
        assert_eq!(
            parse_event(r#"{"inject":"glider","edge":"left"}"#),
            Ok(Command::Inject {
                name: "glider".into(),
                at: None,
                edge: Some("left".into())
            })
        );
        assert_eq!(
            parse_event(r#"{"inject": "lwss", "at": [10, 5]}"#),
            Ok(Command::Inject {
                name: "lwss".into(),
                at: Some((10, 5)),
                edge: None
            })
        );
        assert_eq!(
            parse_event(r#"{"text":"hi \"there\""}"#),
            Ok(Command::Inject {
                name: "text=hi \"there\"".into(),
                at: None,
                edge: None
            })
        );
        assert_eq!(
            parse_event(r#"{"reseed":true,"rule":"highlife","density":0.3}"#),
            Ok(Command::Reseed {
                rule: Some("highlife".into()),
                density: Some("0.3".into())
            })
        );
        assert_eq!(
            parse_event(r#"{"set":"glow","value":0.5}"#),
            Ok(Command::Set("glow".into(), "0.5".into()))
        );
        assert_eq!(parse_event(r#"{"pause":true}"#), Ok(Command::Pause));
        assert_eq!(
            parse_event(r#"{"load_scene":"night"}"#),
            Ok(Command::LoadScene("night".into()))
        );
    }

    #[test]
    fn plain_lines_use_the_socket_syntax_and_bad_input_is_an_error() {
        assert_eq!(
            parse_event("inject glider --edge left"),
            ipc::parse("inject glider --edge left")
        );
        assert_eq!(parse_event("  stats  "), Ok(Command::Stats));
        for bad in [
            "{",
            "{\"a\"}",
            "{\"a\":}",
            "{\"inject\":\"x\"} trailing",
            "{}",
            "{\"at\":[1,2]}",
            "{\"inject\":\"x\",\"at\":[1]}",
            "{\"set\":\"glow\"}",
            "{\"pause\":false}",
            "not a command",
        ] {
            assert!(parse_event(bad).is_err(), "{bad:?}");
        }
    }

    const WTTR: &str = r#"{"current_condition":[{"FeelsLikeC":"-3","humidity":"80","temp_C": "-2","weatherCode":"326",
        "weatherDesc":[{"value":"Light snow"}],"windspeedKmph":"11"}],"weather":[{"maxtempC":"1","hourly":[{"weatherDesc":[{"value":"Sunny"}]}]}]}"#;

    #[test]
    fn a_wttr_answer_is_read_without_a_json_library() {
        let w = parse_weather(WTTR).unwrap();
        assert_eq!(w.sky, Sky::Snow);
        assert_eq!(w.temp_c, Some(-2.0));
        assert!(w.snows() && !w.rains());
        assert_eq!(w.hue(), -65);
    }

    #[test]
    fn skies_are_told_apart_and_garbage_is_ignored() {
        let mk =
            |d: &str, t: &str| format!(r#"{{"temp_C":"{t}","weatherDesc":[{{"value":"{d}"}}]}}"#);
        assert_eq!(
            parse_weather(&mk("Thundery outbreaks possible", "20"))
                .unwrap()
                .sky,
            Sky::Storm
        );
        assert_eq!(
            parse_weather(&mk("Patchy rain nearby", "12")).unwrap().sky,
            Sky::Rain
        );
        assert_eq!(parse_weather(&mk("Mist", "9")).unwrap().sky, Sky::Fog);
        assert_eq!(
            parse_weather(&mk("Partly cloudy", "18")).unwrap().sky,
            Sky::Cloud
        );
        let hot = parse_weather(&mk("Sunny", "33")).unwrap();
        assert_eq!((hot.sky, hot.hue()), (Sky::Clear, 30));
        assert_eq!(parse_weather("<html>rate limited</html>"), None);
        assert_eq!(parse_weather(""), None);
    }
}
