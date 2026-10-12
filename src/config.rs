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
# floating = rounded islands, full = one bar across the screen.
taskbar_style = full
# center or left
taskbar_align = center
taskbar_height = 48
# Gap between the taskbar and the screen edges (0 = flush like Windows).
taskbar_margin = 8
# Space between the taskbar's edges and the buttons inside, on every side.
taskbar_padding = 4
# round, small or square
taskbar_corners = round
# Blur what's behind the taskbar and Start menu (acrylic).
acrylic = true
# Tint strength over the blur (0 = clear glass, 1 = solid).
taskbar_opacity = 0.55
show_search = true
show_status = true
show_clock = true
# App icons in the notification area (as set in Windows' taskbar settings).
tray_icons = true
dock_icon_size = 24
# Grow icons under the cursor (0 = off).
dock_magnify = 0

# ---- Launcher ----
# Start button on the dock that opens Cascade's app launcher.
launcher = true
# Open the launcher with a lone Win key press (Win+E etc. still work).
launcher_win_key = true
# A small helper with admin rights, so the Win key works over apps run as
# administrator (Task Manager). Turn on in Settings: asks for permission once.
admin_helper = false

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
    pub taskbar_style: String,
    pub taskbar_align: String,
    pub taskbar_height: f32,
    pub taskbar_margin: f32,
    pub taskbar_padding: f32,
    pub taskbar_corners: String,
    pub acrylic: bool,
    pub taskbar_opacity: f32,
    pub show_search: bool,
    pub show_status: bool,
    pub show_clock: bool,
    pub tray_icons: bool,
    pub launcher: bool,
    pub launcher_win_key: bool,
    pub admin_helper: bool,
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
            dock_icon_size: 24.0,
            dock_magnify: 0.0,
            taskbar_style: "full".into(),
            taskbar_align: "center".into(),
            taskbar_height: 48.0,
            taskbar_margin: 8.0,
            taskbar_padding: 4.0,
            taskbar_corners: "round".into(),
            acrylic: true,
            taskbar_opacity: 0.55,
            show_search: true,
            show_status: true,
            show_clock: true,
            tray_icons: true,
            admin_helper: false,
            launcher: true,
            launcher_win_key: true,
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
    /// Corner radius (layout px) matching the DWM corner style.
    pub fn corner_radius(&self) -> f32 {
        match self.taskbar_corners.as_str() {
            "square" => 0.0,
            "small" => 4.0,
            _ => 8.0,
        }
    }

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
                "dock_icon_size" => set_f(&mut c.dock_icon_size, 16.0, 48.0),
                "dock_magnify" => set_f(&mut c.dock_magnify, 0.0, 1.0),
                "taskbar_style" if matches!(v, "full" | "floating") => c.taskbar_style = v.into(),
                "taskbar_align" if matches!(v, "center" | "left") => c.taskbar_align = v.into(),
                "taskbar_height" => set_f(&mut c.taskbar_height, 32.0, 80.0),
                "taskbar_margin" => set_f(&mut c.taskbar_margin, 0.0, 32.0),
                "taskbar_padding" => set_f(&mut c.taskbar_padding, 0.0, 16.0),
                "tray_icons" => set_b(&mut c.tray_icons),
                "admin_helper" => set_b(&mut c.admin_helper),
                "taskbar_corners" if matches!(v, "round" | "small" | "square") => c.taskbar_corners = v.into(),
                "acrylic" => set_b(&mut c.acrylic),
                "taskbar_opacity" => set_f(&mut c.taskbar_opacity, 0.0, 1.0),
                "show_search" => set_b(&mut c.show_search),
                "show_status" => set_b(&mut c.show_status),
                "show_clock" => set_b(&mut c.show_clock),
                "launcher" => set_b(&mut c.launcher),
                "launcher_win_key" => set_b(&mut c.launcher_win_key),
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

    /// A setting's current value as text (what the settings window shows).
    pub fn get(&self, key: &str) -> String {
        let b = |v: bool| v.to_string();
        match key {
            "notch" => b(self.notch),
            "hide_in_fullscreen" => b(self.hide_in_fullscreen),
            "live_activity" => b(self.live_activity),
            "visualizer" => b(self.visualizer),
            "volume_osd" => b(self.volume_osd),
            "brightness_osd" => b(self.brightness_osd),
            "hide_windows_flyout" => b(self.hide_windows_flyout),
            "scroll_volume" => b(self.scroll_volume),
            "expand_delay_ms" => self.expand_delay_ms.to_string(),
            "collapse_delay_ms" => self.collapse_delay_ms.to_string(),
            "idle_width" => self.idle_width.to_string(),
            "idle_height" => self.idle_height.to_string(),
            "scale" => self.scale.to_string(),
            "dock" => b(self.dock),
            "dock_autohide" => b(self.dock_autohide),
            "hide_taskbar" => b(self.hide_taskbar),
            "dock_icon_size" => self.dock_icon_size.to_string(),
            "dock_magnify" => self.dock_magnify.to_string(),
            "taskbar_style" => self.taskbar_style.clone(),
            "taskbar_align" => self.taskbar_align.clone(),
            "taskbar_height" => self.taskbar_height.to_string(),
            "taskbar_margin" => self.taskbar_margin.to_string(),
            "taskbar_padding" => self.taskbar_padding.to_string(),
            "tray_icons" => b(self.tray_icons),
            "admin_helper" => b(self.admin_helper),
            "taskbar_corners" => self.taskbar_corners.clone(),
            "acrylic" => b(self.acrylic),
            "taskbar_opacity" => self.taskbar_opacity.to_string(),
            "show_search" => b(self.show_search),
            "show_status" => b(self.show_status),
            "show_clock" => b(self.show_clock),
            "launcher" => b(self.launcher),
            "launcher_win_key" => b(self.launcher_win_key),
            "focus_minutes" => self.focus_minutes.to_string(),
            "break_minutes" => self.break_minutes.to_string(),
            "weather" => b(self.weather),
            "weather_location" => self.weather_location.clone(),
            "fahrenheit" => b(self.fahrenheit),
            "update_check" => b(self.update_check),
            "github_token" => self.github_token.clone(),
            "max_fps" => self.max_fps.to_string(),
            _ => String::new(),
        }
    }

    pub fn set_bool(key: &str, value: bool) {
        Self::set(key, if value { "true" } else { "false" });
    }
}

/// Appends a line to `cascade.log` in the data folder. Only used for rare
/// events (the admin helper being set up or started), to help diagnose them.
pub fn log(line: &str) {
    use std::io::Write;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = std::fs::create_dir_all(dir());
    let path = dir().join("cascade.log");
    // Keep it small: start over past 64 KB.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 64 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{secs}] {line}");
    }
}
