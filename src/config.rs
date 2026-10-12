//! Plain `key = value` config at %APPDATA%\Cascade\config.ini.
//! Keys missing from an older file simply take their defaults.

use std::path::PathBuf;

const DEFAULT: &str = "\
# Cascade config. Edit, save, then right-click the notch or tray icon > Reload config.

# ---- Notch ----
notch = true
# Hide the notch and dock while a fullscreen app or game is in front.
hide_in_fullscreen = true
# Grow the notch with album art / timer while music or a focus timer runs.
live_activity = true
visualizer = true
# Show volume / brightness changes in the notch.
volume_osd = true
brightness_osd = true
# Hide the built-in Windows 11 volume/brightness flyout while the notch runs.
hide_windows_flyout = true
# Scroll over the closed notch to change volume.
scroll_volume = true
expand_delay_ms = 120
collapse_delay_ms = 300
# Resting notch size in pixels at 100% scaling, and an overall size multiplier.
idle_width = 180
idle_height = 28
scale = 1.0

# ---- Dock ----
dock = true
# Slide the dock away until the mouse touches the bottom edge.
dock_autohide = false
# Hide the Windows taskbar while the dock is on (restored when Cascade quits).
hide_taskbar = true
dock_icon_size = 46
# Grow icons under the cursor (0 = off).
dock_magnify = 0.35

# ---- Focus timer ----
focus_minutes = 25
break_minutes = 5

# ---- Weather (Open-Meteo, no account needed) ----
weather = true
# Empty = detect from your IP. Or a city name, or \"lat,lon\".
weather_location =
fahrenheit = false

# ---- Updates ----
# Check GitHub for a newer build every few hours (right-click > Update to install).
update_check = true
# Only needed while the GitHub repo is private: a fine-grained token with
# read-only \"Contents\" access to CornyBirdy/Cascade.
github_token =

# ---- General ----
# Frame-rate cap while animating (also capped to your refresh rate).
max_fps = 144
";

#[derive(Clone, Debug)]
pub struct Config {
    pub notch: bool,
    pub hide_in_fullscreen: bool,
    pub live_activity: bool,
    pub visualizer: bool,
    pub volume_osd: bool,
    pub brightness_osd: bool,
    pub hide_windows_flyout: bool,
    pub scroll_volume: bool,
    pub expand_delay_ms: u32,
    pub collapse_delay_ms: u32,
    pub idle_width: f32,
    pub idle_height: f32,
    pub scale: f32,
    pub dock: bool,
    pub dock_autohide: bool,
    pub hide_taskbar: bool,
    pub dock_icon_size: f32,
    pub dock_magnify: f32,
    pub focus_minutes: u32,
    pub break_minutes: u32,
    pub weather: bool,
    pub weather_location: String,
    pub fahrenheit: bool,
    pub update_check: bool,
    pub github_token: String,
    pub max_fps: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            notch: true,
            hide_in_fullscreen: true,
            live_activity: true,
            visualizer: true,
            volume_osd: true,
            brightness_osd: true,
            hide_windows_flyout: true,
            scroll_volume: true,
            expand_delay_ms: 120,
            collapse_delay_ms: 300,
            idle_width: 180.0,
            idle_height: 28.0,
            scale: 1.0,
            dock: true,
            dock_autohide: false,
            hide_taskbar: true,
            dock_icon_size: 46.0,
            dock_magnify: 0.35,
            focus_minutes: 25,
            break_minutes: 5,
            weather: true,
            weather_location: String::new(),
            fahrenheit: false,
            update_check: true,
            github_token: String::new(),
            max_fps: 144,
        }
    }
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Cascade")
}

pub fn path() -> PathBuf {
    dir().join("config.ini")
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl Config {
    /// Reads the config, writing the commented default file on first run.
    pub fn load() -> Self {
        let path = path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => {
                let _ = std::fs::create_dir_all(dir());
                let _ = std::fs::write(&path, DEFAULT);
                DEFAULT.to_string()
            }
        };
        let mut c = Config::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let b = parse_bool(v);
            let n = v.parse::<f32>().ok();
            let set_b = |slot: &mut bool| {
                if let Some(b) = b {
                    *slot = b;
                }
            };
            let set_u = |slot: &mut u32, lo: f32, hi: f32| {
                if let Some(n) = n {
                    *slot = n.clamp(lo, hi) as u32;
                }
            };
            let set_f = |slot: &mut f32, lo: f32, hi: f32| {
                if let Some(n) = n {
                    *slot = n.clamp(lo, hi);
                }
            };
            match k {
                "notch" => set_b(&mut c.notch),
                "hide_in_fullscreen" => set_b(&mut c.hide_in_fullscreen),
                "live_activity" => set_b(&mut c.live_activity),
                "visualizer" => set_b(&mut c.visualizer),
                "volume_osd" => set_b(&mut c.volume_osd),
                "brightness_osd" => set_b(&mut c.brightness_osd),
                "hide_windows_flyout" => set_b(&mut c.hide_windows_flyout),
                "scroll_volume" => set_b(&mut c.scroll_volume),
                "expand_delay_ms" => set_u(&mut c.expand_delay_ms, 0.0, 5000.0),
                "collapse_delay_ms" => set_u(&mut c.collapse_delay_ms, 0.0, 5000.0),
                "idle_width" => set_f(&mut c.idle_width, 60.0, 400.0),
                "idle_height" => set_f(&mut c.idle_height, 12.0, 60.0),
                "scale" => set_f(&mut c.scale, 0.5, 3.0),
                "dock" => set_b(&mut c.dock),
                "dock_autohide" => set_b(&mut c.dock_autohide),
                "hide_taskbar" => set_b(&mut c.hide_taskbar),
                "dock_icon_size" => set_f(&mut c.dock_icon_size, 24.0, 96.0),
                "dock_magnify" => set_f(&mut c.dock_magnify, 0.0, 1.0),
                "focus_minutes" => set_u(&mut c.focus_minutes, 1.0, 240.0),
                "break_minutes" => set_u(&mut c.break_minutes, 1.0, 120.0),
                "weather" => set_b(&mut c.weather),
                "weather_location" => c.weather_location = v.trim_matches('"').to_string(),
                "fahrenheit" => set_b(&mut c.fahrenheit),
                "update_check" => set_b(&mut c.update_check),
                "github_token" => c.github_token = v.trim_matches('"').to_string(),
                "max_fps" => set_u(&mut c.max_fps, 15.0, 500.0),
                _ => {}
            }
        }
        c
    }

    /// Rewrites one key in the file, keeping comments and other lines.
    pub fn set(key: &str, value: &str) {
        let path = path();
        let text = std::fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT.to_string());
        let mut found = false;
        let mut out: Vec<String> = text
            .lines()
            .map(|line| {
                let is_key = !line.trim_start().starts_with('#')
                    && line.split_once('=').map(|(k, _)| k.trim() == key).unwrap_or(false);
                if is_key {
                    found = true;
                    format!("{key} = {value}")
                } else {
                    line.to_string()
                }
            })
            .collect();
        if !found {
            out.push(format!("{key} = {value}"));
        }
        let _ = std::fs::create_dir_all(dir());
        let _ = std::fs::write(&path, out.join("\r\n") + "\r\n");
    }

    pub fn set_bool(key: &str, value: bool) {
        Self::set(key, if value { "true" } else { "false" });
    }
}
