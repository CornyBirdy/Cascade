//! Answers for the Start menu's search box beyond apps: sums ("12*4+3",
//! "sqrt(2)", "2^10"), unit conversions ("10 km to mi", "180 lb in kg",
//! "25 c to f"), quick commands ("lock", "restart", "bluetooth") and a web
//! search for whatever was typed.

/// What picking an answer does.
#[derive(Clone, Debug, PartialEq)]
pub enum Run {
    /// Copy this to the clipboard (a sum's or conversion's result).
    Copy(String),
    /// Open a program, settings page or link.
    Open(String),
    /// "lock", "sleep", "signout", "restart" or "shutdown".
    Power(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Sum,
    Unit,
    Command,
    Web,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub kind: Kind,
    pub title: String,
    /// The line under it ("Enter to copy").
    pub detail: String,
    pub run: Run,
}

// ---------------------------------------------------------------- sums --

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&mut self) -> Option<u8> {
        while self.s.get(self.i) == Some(&b' ') {
            self.i += 1;
        }
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        loop {
            if self.eat(b'+') {
                v += self.term()?;
            } else if self.eat(b'-') {
                v -= self.term()?;
            } else {
                return Some(v);
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.power()?;
        loop {
            if self.eat(b'*') || self.eat(b'x') {
                v *= self.power()?;
            } else if self.eat(b'/') {
                v /= self.power()?;
            } else if self.eat(b'%') {
                v %= self.power()?;
            } else if self.peek() == Some(b'(') {
                // 2(3+4)
                v *= self.power()?;
            } else {
                return Some(v);
            }
        }
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.unary()?;
        if self.eat(b'^') {
            // Right to left: 2^3^2 = 2^9.
            return Some(base.powf(self.power()?));
        }
        Some(base)
    }

    fn unary(&mut self) -> Option<f64> {
        if self.eat(b'-') {
            return Some(-self.unary()?);
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.primary()
    }

    fn primary(&mut self) -> Option<f64> {
        let c = self.peek()?;
        if c == b'(' {
            self.i += 1;
            let v = self.expr()?;
            return self.eat(b')').then_some(v);
        }
        if c.is_ascii_digit() || c == b'.' {
            let start = self.i;
            while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
                self.i += 1;
            }
            // 1e6, 2.5e-3
            if self.s.get(self.i) == Some(&b'e')
                && self.s.get(self.i + 1).is_some_and(|c| c.is_ascii_digit() || *c == b'-' || *c == b'+')
            {
                self.i += 2;
                while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit()) {
                    self.i += 1;
                }
            }
            return std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok();
        }
        if c.is_ascii_alphabetic() {
            let start = self.i;
            while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphabetic() || c.is_ascii_digit()) {
                self.i += 1;
            }
            let name = std::str::from_utf8(&self.s[start..self.i]).ok()?;
            match name {
                "pi" => return Some(std::f64::consts::PI),
                "e" => return Some(std::f64::consts::E),
                _ => {}
            }
            let f: fn(f64) -> f64 = match name {
                "sqrt" => f64::sqrt,
                "sin" => f64::sin,
                "cos" => f64::cos,
                "tan" => f64::tan,
                "ln" => f64::ln,
                "log" => f64::log10,
                "abs" => f64::abs,
                "round" => f64::round,
                "floor" => f64::floor,
                "ceil" => f64::ceil,
                _ => return None,
            };
            if !self.eat(b'(') {
                return None;
            }
            let v = self.expr()?;
            return self.eat(b')').then(|| f(v));
        }
        None
    }
}

/// The value of a sum like "12*(3+4)", if `q` is one (a lone number isn't).
pub fn calc(q: &str) -> Option<f64> {
    let s: String = q
        .trim()
        .trim_start_matches('=')
        .to_ascii_lowercase()
        .replace('\u{d7}', "*")
        .replace('\u{f7}', "/")
        .replace("**", "^")
        .replace(',', "");
    // A sign in front alone doesn't make a sum.
    let body = s.trim().trim_start_matches(['-', '+']);
    let has_op = body.chars().any(|c| "+-*/^%(x".contains(c))
        || ["sqrt", "sin", "cos", "tan", "ln", "log", "abs", "pi"].iter().any(|f| s.contains(f));
    if !has_op || !s.bytes().all(|c| c.is_ascii()) {
        return None;
    }
    let mut p = Parser { s: s.as_bytes(), i: 0 };
    let v = p.expr()?;
    (p.peek().is_none() && v.is_finite()).then_some(v)
}

/// A number as people write it: up to 10 significant digits, no trailing
/// zeros, very large or small ones in scientific notation.
pub fn number(v: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let a = v.abs();
    if !(1e-6..1e15).contains(&a) {
        return format!("{v:.6e}").replace("e", "\u{d7}10^");
    }
    let digits = (9 - a.log10().floor() as i32).clamp(0, 12) as usize;
    let s = format!("{v:.digits$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" {
        "0".into()
    } else {
        s
    }
}

// --------------------------------------------------------------- units --

#[derive(Clone, Copy, PartialEq)]
enum Dim {
    Length,
    Mass,
    Volume,
    Speed,
    Data,
    Time,
    Temp,
}

/// (names, dimension, size in the dimension's base unit).
const UNITS: &[(&[&str], Dim, f64)] = &[
    (&["mm", "millimetre", "millimetres", "millimeter", "millimeters"], Dim::Length, 0.001),
    (&["cm", "centimetre", "centimetres", "centimeter", "centimeters"], Dim::Length, 0.01),
    (&["m", "metre", "metres", "meter", "meters"], Dim::Length, 1.0),
    (&["km", "kilometre", "kilometres", "kilometer", "kilometers"], Dim::Length, 1000.0),
    (&["in", "inch", "inches", "\""], Dim::Length, 0.0254),
    (&["ft", "foot", "feet", "'"], Dim::Length, 0.3048),
    (&["yd", "yard", "yards"], Dim::Length, 0.9144),
    (&["mi", "mile", "miles"], Dim::Length, 1609.344),
    (&["nmi", "nautical mile", "nautical miles"], Dim::Length, 1852.0),
    (&["mg", "milligram", "milligrams"], Dim::Mass, 1e-6),
    (&["g", "gram", "grams"], Dim::Mass, 0.001),
    (&["kg", "kilo", "kilos", "kilogram", "kilograms"], Dim::Mass, 1.0),
    (&["t", "tonne", "tonnes"], Dim::Mass, 1000.0),
    (&["oz", "ounce", "ounces"], Dim::Mass, 0.028_349_523_125),
    (&["lb", "lbs", "pound", "pounds"], Dim::Mass, 0.453_592_37),
    (&["st", "stone", "stones"], Dim::Mass, 6.350_293_18),
    (&["ml", "millilitre", "millilitres", "milliliter", "milliliters"], Dim::Volume, 0.001),
    (&["l", "litre", "litres", "liter", "liters"], Dim::Volume, 1.0),
    (&["gal", "gallon", "gallons"], Dim::Volume, 3.785_411_784),
    (&["floz", "fl oz", "fluid ounce", "fluid ounces"], Dim::Volume, 0.029_573_529_562_5),
    (&["cup", "cups"], Dim::Volume, 0.25),
    (&["tsp", "teaspoon", "teaspoons"], Dim::Volume, 0.005),
    (&["tbsp", "tablespoon", "tablespoons"], Dim::Volume, 0.02),
    (&["m/s", "mps"], Dim::Speed, 1.0),
    (&["km/h", "kmh", "kph", "kmph"], Dim::Speed, 1.0 / 3.6),
    (&["mph"], Dim::Speed, 0.447_04),
    (&["kn", "kt", "knot", "knots"], Dim::Speed, 0.514_444_444),
    (&["b", "byte", "bytes"], Dim::Data, 1.0),
    (&["kb", "kilobyte", "kilobytes"], Dim::Data, 1024.0),
    (&["mb", "megabyte", "megabytes"], Dim::Data, 1_048_576.0),
    (&["gb", "gigabyte", "gigabytes"], Dim::Data, 1_073_741_824.0),
    (&["tb", "terabyte", "terabytes"], Dim::Data, 1_099_511_627_776.0),
    (&["s", "sec", "secs", "second", "seconds"], Dim::Time, 1.0),
    (&["min", "mins", "minute", "minutes"], Dim::Time, 60.0),
    (&["h", "hr", "hrs", "hour", "hours"], Dim::Time, 3600.0),
    (&["day", "days"], Dim::Time, 86_400.0),
    (&["week", "weeks"], Dim::Time, 604_800.0),
    (&["c", "\u{b0}c", "celsius"], Dim::Temp, 0.0),
    (&["f", "\u{b0}f", "fahrenheit"], Dim::Temp, 1.0),
    (&["k", "kelvin"], Dim::Temp, 2.0),
];

/// The unit called `name`: (its usual short name, dimension, size).
fn unit(name: &str) -> Option<(&'static str, Dim, f64)> {
    let name = name.trim().to_lowercase();
    UNITS.iter().find(|(names, ..)| names.contains(&name.as_str())).map(|(names, d, f)| (names[0], *d, *f))
}

/// Temperature `v` in scale `from` (0 C, 1 F, 2 K) as scale `to`.
fn temperature(v: f64, from: f64, to: f64) -> f64 {
    let c = match from as u8 {
        1 => (v - 32.0) * 5.0 / 9.0,
        2 => v - 273.15,
        _ => v,
    };
    match to as u8 {
        1 => c * 9.0 / 5.0 + 32.0,
        2 => c + 273.15,
        _ => c,
    }
}

/// "10 km to mi" -> (10, km, 6.21371, mi).
pub fn convert(q: &str) -> Option<(f64, &'static str, f64, &'static str)> {
    let q = q.trim().to_lowercase();
    let (left, right) = [" to ", " in ", " = ", " -> ", " as "].iter().find_map(|sep| q.split_once(sep))?;
    let left = left.trim();
    // The number, then the unit (with or without a space: "10km").
    let split = left.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == ',')).unwrap_or(left.len());
    let value: f64 = left[..split].replace(',', "").parse().ok()?;
    let (from_name, from_dim, from_size) = unit(&left[split..])?;
    let (to_name, to_dim, to_size) = unit(right)?;
    if from_dim != to_dim {
        return None;
    }
    let out = if from_dim == Dim::Temp { temperature(value, from_size, to_size) } else { value * from_size / to_size };
    out.is_finite().then_some((value, from_name, out, to_name))
}

fn unit_label(name: &str) -> &str {
    match name {
        "c" => "\u{b0}C",
        "f" => "\u{b0}F",
        "k" => "K",
        n => n,
    }
}

// ------------------------------------------------------------ commands --

/// (title, words that find it, what it does).
const COMMANDS: &[(&str, &[&str], CommandRun)] = &[
    ("Lock", &["lock"], CommandRun::Power("lock")),
    ("Sleep", &["sleep"], CommandRun::Power("sleep")),
    ("Sign out", &["sign out", "log off", "log out", "logout"], CommandRun::Power("signout")),
    ("Restart", &["restart", "reboot"], CommandRun::Power("restart")),
    ("Shut down", &["shut down", "shutdown", "power off", "turn off"], CommandRun::Power("shutdown")),
    ("Task Manager", &["task manager", "taskmgr"], CommandRun::Open("taskmgr.exe")),
    ("Settings", &["settings"], CommandRun::Open("ms-settings:")),
    ("Control Panel", &["control panel"], CommandRun::Open("control.exe")),
    ("Device Manager", &["device manager"], CommandRun::Open("devmgmt.msc")),
    ("Terminal", &["terminal", "cmd", "powershell", "command prompt"], CommandRun::Open("wt.exe")),
    ("Bluetooth settings", &["bluetooth"], CommandRun::Open("ms-settings:bluetooth")),
    ("Wi-Fi settings", &["wifi", "wi-fi", "wireless"], CommandRun::Open("ms-settings:network-wifi")),
    ("Display settings", &["display", "resolution", "screen"], CommandRun::Open("ms-settings:display")),
    ("Sound settings", &["sound", "audio"], CommandRun::Open("ms-settings:sound")),
    ("Windows Update", &["windows update", "update"], CommandRun::Open("ms-settings:windowsupdate")),
    ("Apps", &["uninstall", "installed apps"], CommandRun::Open("ms-settings:appsfeatures")),
];

#[derive(Clone, Copy)]
enum CommandRun {
    Power(&'static str),
    Open(&'static str),
}

fn commands(q: &str) -> Vec<Answer> {
    let q = q.trim().to_lowercase();
    if q.len() < 3 {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .filter(|(_, words, _)| words.iter().any(|w| w.starts_with(&q)))
        .map(|&(title, _, run)| Answer {
            kind: Kind::Command,
            title: title.to_string(),
            detail: match run {
                CommandRun::Power(_) => "Power".to_string(),
                CommandRun::Open(_) => "Command".to_string(),
            },
            run: match run {
                CommandRun::Power(p) => Run::Power(p),
                CommandRun::Open(t) => Run::Open(t.to_string()),
            },
        })
        .collect()
}

// ------------------------------------------------------------------ web --

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The search page of `engine` (google, bing or duckduckgo) for `q`.
pub fn web_url(engine: &str, q: &str) -> Option<String> {
    let base = match engine {
        "google" => "https://www.google.com/search?q=",
        "bing" => "https://www.bing.com/search?q=",
        "duckduckgo" => "https://duckduckgo.com/?q=",
        _ => return None,
    };
    Some(format!("{base}{}", url_encode(q.trim())))
}

/// The answers for query `q`: (the ones to show above the apps: a sum or
/// conversion; the ones below them: commands, then the web search).
/// `engine` is the web search (or "none").
pub fn answers(q: &str, engine: &str) -> (Vec<Answer>, Vec<Answer>) {
    let mut top = Vec::new();
    if let Some((value, from, out, to)) = convert(q) {
        let result = number(out);
        top.push(Answer {
            kind: Kind::Unit,
            title: format!("{} {} = {} {}", number(value), unit_label(from), result, unit_label(to)),
            detail: "Enter to copy".into(),
            run: Run::Copy(result),
        });
    } else if let Some(v) = calc(q) {
        let result = number(v);
        top.push(Answer {
            kind: Kind::Sum,
            title: format!("= {result}"),
            detail: format!("{}  \u{b7}  Enter to copy", q.trim()),
            run: Run::Copy(result),
        });
    }
    let mut bottom = commands(q);
    if let Some(url) = web_url(engine, q).filter(|_| !q.trim().is_empty()) {
        bottom.push(Answer {
            kind: Kind::Web,
            title: format!("Search the web for \u{201c}{}\u{201d}", q.trim()),
            detail: match engine {
                "bing" => "Bing",
                "duckduckgo" => "DuckDuckGo",
                _ => "Google",
            }
            .into(),
            run: Run::Open(url),
        });
    }
    (top, bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums() {
        assert_eq!(calc("12*4+3"), Some(51.0));
        assert_eq!(calc("2^10"), Some(1024.0));
        assert_eq!(calc("2^3^2"), Some(512.0));
        assert_eq!(calc("(1+2)*3"), Some(9.0));
        assert_eq!(calc("2(3+4)"), Some(14.0));
        assert_eq!(calc("10 / 4"), Some(2.5));
        assert_eq!(calc("1,000 * 3"), Some(3000.0));
        assert_eq!(calc("sqrt(16)"), Some(4.0));
        assert_eq!(calc("-3+5"), Some(2.0));
        assert_eq!(calc("6 x 7"), Some(42.0));
        assert!(calc("2*pi").is_some_and(|v| (v - std::f64::consts::TAU).abs() < 1e-12));
        // Not sums: a lone number, words, unfinished.
        assert_eq!(calc("42"), None);
        assert_eq!(calc("-5"), None);
        assert_eq!(calc("chrome"), None);
        assert_eq!(calc("3+"), None);
        assert_eq!(calc("excel"), None);
        assert_eq!(number(0.1 + 0.2), "0.3");
        assert_eq!(number(1.0 / 3.0), "0.3333333333");
        assert_eq!(number(51.0), "51");
        assert_eq!(number(-2.5), "-2.5");
    }

    #[test]
    fn conversions() {
        let (v, from, out, to) = convert("10 km to mi").unwrap();
        assert_eq!((v, from, to), (10.0, "km", "mi"));
        assert!((out - 6.213_711_922).abs() < 1e-6);
        assert!(convert("100 c in f").is_some_and(|c| (c.2 - 212.0).abs() < 1e-9));
        assert!(convert("32 °f to c").is_some_and(|c| c.2.abs() < 1e-9));
        assert!(convert("180 lb in kg").is_some_and(|c| (c.2 - 81.646_627).abs() < 1e-5));
        assert!(convert("5ft to cm").is_some_and(|c| (c.2 - 152.4).abs() < 1e-9));
        assert!(convert("1 gb to mb").is_some_and(|c| (c.2 - 1024.0).abs() < 1e-9));
        assert_eq!(convert("10 km to kg"), None);
        assert_eq!(convert("to the moon"), None);
    }

    #[test]
    fn answers_for_a_query() {
        let (top, bottom) = answers("12*3", "google");
        assert_eq!(top[0].title, "= 36");
        assert_eq!(top[0].run, Run::Copy("36".into()));
        assert_eq!(bottom[0].run, Run::Open("https://www.google.com/search?q=12%2A3".into()));
        let (_, bottom) = answers("resta", "none");
        assert_eq!(bottom[0].run, Run::Power("restart"));
        let (top, bottom) = answers("fi", "none");
        assert!(top.is_empty() && bottom.is_empty());
        assert_eq!(url_encode("a b&c"), "a+b%26c");
    }
}
