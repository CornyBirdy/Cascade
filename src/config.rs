//! Plain `key = value` config at %APPDATA%\Cascade\config.ini.

use std::path::PathBuf;

const DEFAULT: &str = "\
# Cascade config. Edit, save, then right-click the notch > Reload config.

# Hide the notch while a fullscreen app or game is in front.
hide_in_fullscreen = true

# Grow the notch with album art and a visualizer while music plays.
live_activity = true
visualizer = true

# Show volume / brightness changes in the notch.
volume_osd = true
brightness_osd = true

# Hide the built-in Windows 11 volume/brightness flyout while Cascade runs.
hide_windows_flyout = true

# Scroll over the notch to change volume.
scroll_volume = true

# Hover delays, in milliseconds.
expand_delay_ms = 120
collapse_delay_ms = 300

# Frame-rate cap while animating (also capped to your refresh rate).
max_fps = 144

# Resting notch size in pixels at 100% scaling, and an overall size multiplier.
idle_width = 180
idle_height = 28
scale = 1.0
";

#[derive(Clone, Debug)]
pub struct Config {
    pub hide_in_fullscreen: bool,
    pub live_activity: bool,
    pub visualizer: bool,
    pub volume_osd: bool,
    pub brightness_osd: bool,
    pub hide_windows_flyout: bool,
    pub scroll_volume: bool,
    pub expand_delay_ms: u32,
    pub collapse_delay_ms: u32,
    pub max_fps: u32,
    pub idle_width: f32,
    pub idle_height: f32,
    pub scale: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hide_in_fullscreen: true,
            live_activity: true,
            visualizer: true,
            volume_osd: true,
            brightness_osd: true,
            hide_windows_flyout: true,
            scroll_volume: true,
            expand_delay_ms: 120,
            collapse_delay_ms: 300,
            max_fps: 144,
            idle_width: 180.0,
            idle_height: 28.0,
            scale: 1.0,
        }
    }
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Cascade").join("config.ini")
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
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
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
            match k {
                "hide_in_fullscreen" => c.hide_in_fullscreen = b.unwrap_or(c.hide_in_fullscreen),
                "live_activity" => c.live_activity = b.unwrap_or(c.live_activity),
                "visualizer" => c.visualizer = b.unwrap_or(c.visualizer),
                "volume_osd" => c.volume_osd = b.unwrap_or(c.volume_osd),
                "brightness_osd" => c.brightness_osd = b.unwrap_or(c.brightness_osd),
                "hide_windows_flyout" => c.hide_windows_flyout = b.unwrap_or(c.hide_windows_flyout),
                "scroll_volume" => c.scroll_volume = b.unwrap_or(c.scroll_volume),
                "expand_delay_ms" => c.expand_delay_ms = n.map_or(c.expand_delay_ms, |n| n.clamp(0.0, 5000.0) as u32),
                "collapse_delay_ms" => {
                    c.collapse_delay_ms = n.map_or(c.collapse_delay_ms, |n| n.clamp(0.0, 5000.0) as u32)
                }
                "max_fps" => c.max_fps = n.map_or(c.max_fps, |n| n.clamp(15.0, 500.0) as u32),
                "idle_width" => c.idle_width = n.map_or(c.idle_width, |n| n.clamp(60.0, 400.0)),
                "idle_height" => c.idle_height = n.map_or(c.idle_height, |n| n.clamp(12.0, 60.0)),
                "scale" => c.scale = n.map_or(c.scale, |n| n.clamp(0.5, 3.0)),
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
                let is_key = line
                    .split_once('=')
                    .map(|(k, _)| k.trim() == key && !line.trim_start().starts_with('#'))
                    .unwrap_or(false);
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
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, out.join("\r\n") + "\r\n");
    }
}
