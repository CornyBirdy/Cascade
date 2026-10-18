//! Plain `key = value` config at %APPDATA%\Cascade\config.ini.
//! Keys missing from an older file simply take their defaults.

use std::path::PathBuf;

const DEFAULT: &str = "\
# Cascade config. Edit, save, then right-click the notch or tray icon > Reload config.

# ---- Look ----
# One look for the notch's lip, taskbar, Start menu, panels and Alt+Tab:
# acrylic, liquid_glass, windows_glass, frosted, tinted (your Windows accent
# colour), translucent, rose_pine, neon, obsidian (no blur) or clean_slate
# (no blur).
theme = acrylic
# What the theme's colours are based on: theme (its own), accent (your Windows
# accent colour), neutral (no colour) or custom (theme_hue, in degrees round
# the colour wheel: 0 red, 120 green, 210 blue, 280 purple).
theme_color = theme
theme_hue = 210
# How colourful the theme is: 0 = grey, 1 = as designed, 2 = twice as strong.
theme_intensity = 1
# How strongly the glass's edge catches the light (its outline, and the light
# gathered and glowing along it): 0 = none, 1 = as designed, 2 = twice as strong.
theme_edge = 1
# Clear glass (Liquid Glass, Translucent) bends what's behind it round its
# edge, like real thick glass. Reads the screen behind it a few times a second.
refraction = true

# ---- Notch ----
notch = true
# lip = a thin glass strip at the top of the screen that pulls the notch down
# when the pointer touches it; notch = the classic black notch, always there.
notch_style = lip
# Size of the lip in pixels at 100% scaling (its length along the edge, and
# how far it reaches out).
lip_width = 160
lip_height = 5
# Which edge of the main screen it sits on (top, bottom, left or right), and
# where along it: 0 = the left / top end, 0.5 = the middle, 1 = the right /
# bottom end.
notch_edge = top
notch_position = 0.5
# Hide the notch and dock while a fullscreen app or game is in front.
hide_in_fullscreen = true
# While music or a focus timer plays, show it on the closed notch: the notch
# style grows with album art / the timer, the lip lights a coloured line.
live_activity = true
# Moving bars beside the album art when the notch grows for music, and on the
# open notch's Media tab.
visualizer = true
# Lip style (or a notch on a side edge): the lit line pulses with the music
# instead of staying still. Needs live_activity.
lip_visualizer = true
# Bars that move with the music on the taskbar's music widget and the widgets
# panel's music card.
widget_visualizer = true
# Show volume / brightness changes in the notch.
volume_osd = true
brightness_osd = true
# Hide the built-in Windows 11 volume/brightness flyout while the notch runs.
hide_windows_flyout = true
# Scroll over the closed notch to change volume.
scroll_volume = true
# The notch's Controls also list each app playing sound, with its own
# volume, and the output device to pick.
controls_mixer = true
# Say so in the notch when a USB device is plugged in: known (flight
# controllers, radios, headsets, phones and the ones in devices.txt), all
# (any device) or off. Clicking the pop-up opens what the device goes with.
usb_popups = known
# ...and when one is unplugged.
usb_unplug = true
expand_delay_ms = 120
collapse_delay_ms = 300
# Resting size of the classic notch in pixels at 100% scaling, and an overall
# size multiplier for the notch.
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
# with_apps = Start sits with the apps (centre or left, as above), left = Start alone on its own island at the far left.
taskbar_start = with_apps
# Gap between the taskbar and the screen edges (0 = flush like Windows).
taskbar_gap = 6
# Gap from the taskbar's edge to its buttons, the same on all four sides.
taskbar_pad = 6
# Space between an icon and the edge of its hover highlight. The taskbar's
# height is the icon, twice this and twice taskbar_pad added up.
taskbar_tile = 6
# round, small or square
taskbar_corners = round
# Blur what's behind the taskbar, Start menu, panels and the notch's lip (the
# kind of blur comes from the theme).
acrylic = true
# Tint strength over the blur (0 = clear glass, 1 = solid). Also used by the
# panels and the notch's lip.
taskbar_opacity = 0.55
show_search = true
show_status = true
# What clicking network / volume / battery does: notch (the notch's Controls)
# or windows (Windows' quick settings).
status_click = notch
show_clock = true
# App icons in the notification area (as set in Windows' taskbar settings).
tray_icons = true
dock_icon_size = 24
# Grow icons under the cursor (0 = off).
dock_magnify = 0
# Rest the pointer on a window's preview to see only that window (the others
# turn to glass), as Windows' taskbar does.
taskbar_peek = true
# Other screens: all = a taskbar on every screen, main = only the main one.
taskbar_screens = all
# Which windows each screen's taskbar shows: all, or screen (only the ones on
# that screen; pinned apps show everywhere).
taskbar_screen_apps = all
# The clock on the other screens' taskbars.
taskbar_screen_clock = true

# ---- Screens ----
# Keep the pointer's height true when it crosses between screens of different
# sizes (like LittleBigMouse).
mouse_match = false
# How the screens are lined up: auto (read from the Windows arrangement), top,
# middle or bottom.
mouse_align = auto
# Diagonal sizes in inches, left to right (e.g. 27,24). Empty = read from the screens.
screen_inches =

# ---- Alt+Tab ----
# Cascade's own window switcher (live previews, same theme) instead of Windows'.
alt_tab = true
# How it looks: cards (live previews), large (bigger previews), icons (big app
# icons, the window's title under them) or list (icons and titles in a column).
alt_tab_style = cards

# ---- Widgets ----
# Widgets on the taskbar, left to right on each side. Any of: weather, music,
# cpu, ram, gpu, net, desktops (a dot per virtual desktop, shown with more
# than one) or widgets (the button that opens the panel). Put a name in
# either list, or leave it out of both to hide it. Click one to open the panel.
taskbar_left = weather
taskbar_right = music,cpu,ram,desktops,widgets
# The panel's cards, in order (remove one to hide it): notifications, media, system, weather, calendar.
widgets_panel = media,system,weather,calendar
# Windows' notifications as a card at the top of the panel (also opened by the clock).
widgets_notifications = true
# 1 or 2 columns of cards.
widgets_columns = 1
# NVIDIA cards: which to show (all, or 1, 2... for one of them), on the taskbar
# and in the panel.
gpu_cards = all
# The GPU widget on the taskbar: the temperature over each card's figure, what
# the figure is (load, memory or power) and its label (number or name).
gpu_temp = true
gpu_value = load
gpu_label = number
# Seconds between system stats readings (only taken while one is on screen).
stats_seconds = 2

# ---- Launcher ----
# Start button on the dock that opens Cascade's app launcher.
launcher = true
# Open the launcher with a lone Win key press (Win+E etc. still work).
launcher_win_key = true
# ---- Shortcuts ----
# Cascade's own, themed versions of Windows' shortcut menus, in place of
# Windows' (the key does nothing different when turned off).
# Win+X (and right-clicking Start): the quick links menu, above Start.
shortcut_win_x = true
# Win+N: the widgets panel with your notifications and the calendar (needs
# Cascade's taskbar).
shortcut_win_n = true
# Win+A: the notch's Controls instead of Windows' quick settings.
shortcut_win_a = false
# Win+V: your clipboard history (Windows' own) by the text cursor.
shortcut_win_v = true
# Win+Tab: Task View, the windows of this desktop and your virtual desktops.
shortcut_win_tab = true
# Where the Start menu opens: auto (follows the taskbar), center or left.
start_position = auto
start_height = 620
# Columns and rows of pinned apps.
start_columns = 4
start_pinned_rows = 3
start_show_sidebar = true
# Shrink the sidebar to a slim icon rail (the button at its top opens it).
start_sidebar_compact = true
start_show_recent = true
# Show the Pinned apps grid, and the account button at the top of the sidebar.
start_show_pinned = true
start_show_profile = true
# Show the Cascade (settings) button above Power.
start_show_cascade = true
# Sidebar shortcuts (each can be turned off).
start_place_settings = true
start_place_explorer = true
start_place_downloads = true
start_place_documents = true
start_place_pictures = true
start_place_music = true
start_place_videos = true
start_place_network = true
start_show_all_apps = true
# Typing in Start also answers sums (12*4), converts units (10 km to mi),
# finds commands (lock, restart, bluetooth) and offers a web search with
# google, bing, duckduckgo or none.
start_web_search = google
# Tint over the blur behind the Start menu (0 = clear glass, 1 = solid).
start_opacity = 0.55
# A small helper with admin rights, so the Win key works over apps run as
# administrator (Task Manager). Turn on in Settings: asks for permission once.
admin_helper = false

# ---- Focus timer ----
focus_minutes = 25
break_minutes = 5

# ---- Calendar ----
# Your calendar's private iCal (.ics) link, for its events in the notch: the
# Calendar tab's agenda and dots, and a countdown on the closed notch before
# each event. Google Calendar: Settings > your calendar > Secret address in
# iCal format. Outlook: Settings > Calendar > Shared calendars > Publish.
calendar_url =
# Minutes before an event that the countdown starts (0 = none).
calendar_soon = 10

# ---- Weather (Open-Meteo, no account needed) ----
weather = true
# Empty = detect from your IP. Or a city name (\"Town, Country\" to pick
# between places with the same name), or \"lat,lon\".
weather_location =
fahrenheit = false
# What clicking the weather does: panel (Cascade's widgets panel), google, bing,
# custom (weather_url) or none.
weather_click = panel
# Used when weather_click = custom. {q} becomes e.g. \"weather Sydney\".
weather_url =

# ---- Updates ----
# Check GitHub for a newer build every few hours (right-click > Update to install).
update_check = true
# Only needed while the GitHub repo is private: a fine-grained token with
# read-only \"Contents\" access to CornyBirdy/Cascade.
github_token =

# ---- General ----
# Frame-rate cap while animating (also capped to your refresh rate).
max_fps = 144
# Write each surface's render time and frame rate to cascade.log about once a
# second while something animates (for measuring; costs nothing when off).
perf_log = false
";

#[derive(Clone, Debug)]
pub struct Config {
    pub theme: String,
    pub theme_color: String,
    pub theme_hue: f32,
    pub theme_intensity: f32,
    pub theme_edge: f32,
    pub refraction: bool,
    pub notch: bool,
    pub notch_style: String,
    pub lip_width: f32,
    pub lip_height: f32,
    pub notch_edge: String,
    pub notch_position: f32,
    pub hide_in_fullscreen: bool,
    pub live_activity: bool,
    pub visualizer: bool,
    pub lip_visualizer: bool,
    pub widget_visualizer: bool,
    pub volume_osd: bool,
    pub brightness_osd: bool,
    pub hide_windows_flyout: bool,
    pub scroll_volume: bool,
    pub controls_mixer: bool,
    pub usb_popups: String,
    pub usb_unplug: bool,
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
    pub taskbar_peek: bool,
    pub taskbar_screens: String,
    pub taskbar_screen_apps: String,
    pub taskbar_screen_clock: bool,
    pub taskbar_style: String,
    pub taskbar_align: String,
    pub taskbar_start: String,
    pub taskbar_gap: f32,
    pub taskbar_pad: f32,
    pub taskbar_tile: f32,
    pub taskbar_corners: String,
    pub acrylic: bool,
    pub taskbar_opacity: f32,
    pub show_search: bool,
    pub show_status: bool,
    pub status_click: String,
    pub show_clock: bool,
    pub tray_icons: bool,
    pub alt_tab: bool,
    pub alt_tab_style: String,
    pub mouse_match: bool,
    pub mouse_align: String,
    pub screen_inches: String,
    pub taskbar_left: String,
    pub taskbar_right: String,
    pub widgets_panel: String,
    pub widgets_columns: u32,
    pub gpu_cards: String,
    pub gpu_temp: bool,
    pub gpu_value: String,
    pub gpu_label: String,
    pub widgets_notifications: bool,
    pub stats_seconds: u32,
    pub launcher: bool,
    pub launcher_win_key: bool,
    pub shortcut_win_x: bool,
    pub shortcut_win_n: bool,
    pub shortcut_win_a: bool,
    pub shortcut_win_v: bool,
    pub shortcut_win_tab: bool,
    pub admin_helper: bool,
    pub start_position: String,
    pub start_height: f32,
    pub start_columns: u32,
    pub start_pinned_rows: u32,
    pub start_show_sidebar: bool,
    pub start_sidebar_compact: bool,
    pub start_show_recent: bool,
    pub start_show_pinned: bool,
    pub start_show_profile: bool,
    pub start_show_cascade: bool,
    /// Sidebar shortcuts, in `PLACE_KEYS` order.
    pub start_places: [bool; 8],
    pub start_show_all_apps: bool,
    pub start_web_search: String,
    pub start_opacity: f32,
    pub focus_minutes: u32,
    pub break_minutes: u32,
    pub calendar_url: String,
    pub calendar_soon: u32,
    pub weather: bool,
    pub weather_location: String,
    pub weather_click: String,
    pub weather_url: String,
    pub fahrenheit: bool,
    pub update_check: bool,
    pub github_token: String,
    pub max_fps: u32,
    pub perf_log: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "acrylic".into(),
            theme_color: "theme".into(),
            theme_hue: 210.0,
            theme_intensity: 1.0,
            theme_edge: 1.0,
            refraction: true,
            notch: true,
            notch_style: "lip".into(),
            lip_width: 160.0,
            lip_height: 5.0,
            notch_edge: "top".into(),
            notch_position: 0.5,
            hide_in_fullscreen: true,
            live_activity: true,
            visualizer: true,
            lip_visualizer: true,
            widget_visualizer: true,
            volume_osd: true,
            brightness_osd: true,
            hide_windows_flyout: true,
            scroll_volume: true,
            controls_mixer: true,
            usb_popups: "known".into(),
            usb_unplug: true,
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
            taskbar_peek: true,
            taskbar_screens: "all".into(),
            taskbar_screen_apps: "all".into(),
            taskbar_screen_clock: true,
            taskbar_style: "full".into(),
            taskbar_align: "center".into(),
            taskbar_start: "with_apps".into(),
            taskbar_gap: 6.0,
            taskbar_pad: 6.0,
            taskbar_tile: 6.0,
            taskbar_corners: "round".into(),
            acrylic: true,
            taskbar_opacity: 0.55,
            show_search: true,
            show_status: true,
            status_click: "notch".into(),
            show_clock: true,
            tray_icons: true,
            admin_helper: false,
            start_position: "auto".into(),
            start_height: 620.0,
            start_columns: 4,
            start_pinned_rows: 3,
            start_show_sidebar: true,
            start_sidebar_compact: true,
            start_show_recent: true,
            start_show_pinned: true,
            start_show_profile: true,
            start_show_cascade: true,
            start_places: [true; 8],
            start_show_all_apps: true,
            start_web_search: "google".into(),
            start_opacity: 0.55,
            alt_tab: true,
            alt_tab_style: "cards".into(),
            mouse_match: false,
            mouse_align: "auto".into(),
            screen_inches: String::new(),
            taskbar_left: "weather".into(),
            taskbar_right: "music,cpu,ram,desktops,widgets".into(),
            widgets_panel: "media,system,weather,calendar".into(),
            widgets_columns: 1,
            gpu_cards: "all".into(),
            gpu_temp: true,
            gpu_value: "load".into(),
            gpu_label: "number".into(),
            widgets_notifications: true,
            stats_seconds: 2,
            launcher: true,
            launcher_win_key: true,
            shortcut_win_x: true,
            shortcut_win_n: true,
            shortcut_win_a: false,
            shortcut_win_v: true,
            shortcut_win_tab: true,
            focus_minutes: 25,
            break_minutes: 5,
            calendar_url: String::new(),
            calendar_soon: 10,
            weather: true,
            weather_location: String::new(),
            weather_click: "panel".into(),
            weather_url: String::new(),
            fahrenheit: false,
            update_check: true,
            github_token: String::new(),
            max_fps: 144,
            perf_log: false,
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

/// Config keys of the Start menu's sidebar shortcuts, in `launcher::PLACES` order.
const PLACE_KEYS: [&str; 8] = [
    "start_place_settings",
    "start_place_explorer",
    "start_place_downloads",
    "start_place_documents",
    "start_place_pictures",
    "start_place_music",
    "start_place_videos",
    "start_place_network",
];

fn place_index(key: &str) -> Option<usize> {
    PLACE_KEYS.iter().position(|k| *k == key)
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn list_has(list: &str, name: &str) -> bool {
    list.split(',').any(|n| n.trim().eq_ignore_ascii_case(name))
}

fn list_without(list: &str, name: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case(name))
        .map(String::from)
        .collect()
}

impl Config {
    /// Whether the taskbar widget `name` is on either side.
    pub fn placed(&self, name: &str) -> bool {
        list_has(&self.taskbar_left, name) || list_has(&self.taskbar_right, name)
    }

    /// "left", "right" or "off".
    pub fn place_of(&self, name: &str) -> &'static str {
        if list_has(&self.taskbar_left, name) {
            "left"
        } else if list_has(&self.taskbar_right, name) {
            "right"
        } else {
            "off"
        }
    }

    /// The two lists with `name` moved to `place` (the end of that side's list).
    pub fn with_place(&self, name: &str, place: &str) -> (String, String) {
        let mut left = list_without(&self.taskbar_left, name);
        let mut right = list_without(&self.taskbar_right, name);
        match place {
            "left" => left.push(name.to_string()),
            "right" => right.push(name.to_string()),
            _ => {}
        }
        (left.join(","), right.join(","))
    }

    /// The notch rests as a thin lip that pulls it down on hover.
    pub fn lip(&self) -> bool {
        self.notch_style == "lip"
    }

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
                "theme" if crate::theme::is_known(v) => c.theme = v.into(),
                "theme_color" if crate::theme::SOURCES.contains(&v) => c.theme_color = v.into(),
                "theme_hue" => set_f(&mut c.theme_hue, 0.0, 360.0),
                "theme_intensity" => set_f(&mut c.theme_intensity, 0.0, 2.0),
                "theme_edge" => set_f(&mut c.theme_edge, 0.0, 2.0),
                "refraction" => set_b(&mut c.refraction),
                "notch" => set_b(&mut c.notch),
                "notch_style" if matches!(v, "lip" | "notch") => c.notch_style = v.into(),
                "lip_width" => set_f(&mut c.lip_width, 40.0, 400.0),
                "lip_height" => set_f(&mut c.lip_height, 2.0, 16.0),
                "notch_edge" if matches!(v, "top" | "bottom" | "left" | "right") => c.notch_edge = v.into(),
                "notch_position" => set_f(&mut c.notch_position, 0.0, 1.0),
                "hide_in_fullscreen" => set_b(&mut c.hide_in_fullscreen),
                "live_activity" => set_b(&mut c.live_activity),
                "visualizer" => set_b(&mut c.visualizer),
                "lip_visualizer" => set_b(&mut c.lip_visualizer),
                "widget_visualizer" => set_b(&mut c.widget_visualizer),
                "volume_osd" => set_b(&mut c.volume_osd),
                "brightness_osd" => set_b(&mut c.brightness_osd),
                "hide_windows_flyout" => set_b(&mut c.hide_windows_flyout),
                "scroll_volume" => set_b(&mut c.scroll_volume),
                "controls_mixer" => set_b(&mut c.controls_mixer),
                "usb_popups" if matches!(v, "known" | "all" | "off") => c.usb_popups = v.into(),
                "usb_unplug" => set_b(&mut c.usb_unplug),
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
                "taskbar_peek" => set_b(&mut c.taskbar_peek),
                "taskbar_screens" if matches!(v, "all" | "main") => c.taskbar_screens = v.into(),
                "taskbar_screen_apps" if matches!(v, "all" | "screen") => c.taskbar_screen_apps = v.into(),
                "taskbar_screen_clock" => set_b(&mut c.taskbar_screen_clock),
                "taskbar_style" if matches!(v, "full" | "floating") => c.taskbar_style = v.into(),
                "taskbar_align" if matches!(v, "center" | "left") => c.taskbar_align = v.into(),
                "taskbar_start" if matches!(v, "with_apps" | "left") => c.taskbar_start = v.into(),
                // An earlier build offered the far right; the far left replaced it.
                "taskbar_start" if v == "right" => c.taskbar_start = "left".into(),
                "taskbar_gap" => set_f(&mut c.taskbar_gap, 0.0, 32.0),
                "taskbar_pad" => set_f(&mut c.taskbar_pad, 0.0, 24.0),
                "taskbar_tile" => set_f(&mut c.taskbar_tile, 0.0, 16.0),
                "tray_icons" => set_b(&mut c.tray_icons),
                "admin_helper" => set_b(&mut c.admin_helper),
                "start_position" if matches!(v, "auto" | "center" | "left") => c.start_position = v.into(),
                "start_height" => set_f(&mut c.start_height, 500.0, 800.0),
                "start_columns" => set_u(&mut c.start_columns, 3.0, 6.0),
                "start_pinned_rows" => set_u(&mut c.start_pinned_rows, 1.0, 4.0),
                "start_show_sidebar" => set_b(&mut c.start_show_sidebar),
                "start_sidebar_compact" => set_b(&mut c.start_sidebar_compact),
                "start_show_recent" => set_b(&mut c.start_show_recent),
                "start_show_pinned" => set_b(&mut c.start_show_pinned),
                "start_show_profile" => set_b(&mut c.start_show_profile),
                "start_show_cascade" => set_b(&mut c.start_show_cascade),
                _ if place_index(k).is_some() => {
                    if let (Some(i), Some(on)) = (place_index(k), parse_bool(v)) {
                        c.start_places[i] = on;
                    }
                }
                "start_show_all_apps" => set_b(&mut c.start_show_all_apps),
                "start_web_search" if matches!(v, "google" | "bing" | "duckduckgo" | "none") => {
                    c.start_web_search = v.into()
                }
                "start_opacity" => set_f(&mut c.start_opacity, 0.0, 1.0),
                "taskbar_corners" if matches!(v, "round" | "small" | "square") => c.taskbar_corners = v.into(),
                "acrylic" => set_b(&mut c.acrylic),
                "taskbar_opacity" => set_f(&mut c.taskbar_opacity, 0.0, 1.0),
                "show_search" => set_b(&mut c.show_search),
                "show_status" => set_b(&mut c.show_status),
                "status_click" if matches!(v, "notch" | "windows") => c.status_click = v.into(),
                "show_clock" => set_b(&mut c.show_clock),
                "alt_tab" => set_b(&mut c.alt_tab),
                "alt_tab_style" if matches!(v, "cards" | "large" | "icons" | "list") => c.alt_tab_style = v.into(),
                "mouse_match" => set_b(&mut c.mouse_match),
                "mouse_align" if matches!(v, "auto" | "top" | "middle" | "bottom") => c.mouse_align = v.into(),
                "screen_inches" => c.screen_inches = v.trim_matches('"').to_string(),
                "taskbar_left" => c.taskbar_left = v.trim_matches('"').to_string(),
                "taskbar_right" => c.taskbar_right = v.trim_matches('"').to_string(),
                "widgets_panel" => c.widgets_panel = v.trim_matches('"').to_string(),
                "widgets_columns" => set_u(&mut c.widgets_columns, 1.0, 2.0),
                "gpu_cards" if v == "all" || matches!(v.parse::<u32>(), Ok(1..=8)) => c.gpu_cards = v.into(),
                "gpu_temp" => set_b(&mut c.gpu_temp),
                "gpu_value" if matches!(v, "load" | "memory" | "power") => c.gpu_value = v.into(),
                "gpu_label" if matches!(v, "number" | "name") => c.gpu_label = v.into(),
                "widgets_notifications" => set_b(&mut c.widgets_notifications),
                "stats_seconds" => set_u(&mut c.stats_seconds, 1.0, 10.0),
                "launcher" => set_b(&mut c.launcher),
                "launcher_win_key" => set_b(&mut c.launcher_win_key),
                "shortcut_win_x" => set_b(&mut c.shortcut_win_x),
                "shortcut_win_n" => set_b(&mut c.shortcut_win_n),
                "shortcut_win_a" => set_b(&mut c.shortcut_win_a),
                "shortcut_win_v" => set_b(&mut c.shortcut_win_v),
                "shortcut_win_tab" => set_b(&mut c.shortcut_win_tab),
                "focus_minutes" => set_u(&mut c.focus_minutes, 1.0, 240.0),
                "break_minutes" => set_u(&mut c.break_minutes, 1.0, 120.0),
                "calendar_url" => c.calendar_url = v.trim_matches('"').to_string(),
                "calendar_soon" => set_u(&mut c.calendar_soon, 0.0, 60.0),
                "weather" => set_b(&mut c.weather),
                "weather_location" => c.weather_location = v.trim_matches('"').to_string(),
                "weather_click" if matches!(v, "panel" | "google" | "bing" | "custom" | "none") => {
                    c.weather_click = v.into()
                }
                "weather_url" => c.weather_url = v.trim_matches('"').to_string(),
                "fahrenheit" => set_b(&mut c.fahrenheit),
                "update_check" => set_b(&mut c.update_check),
                "github_token" => c.github_token = v.trim_matches('"').to_string(),
                "max_fps" => set_u(&mut c.max_fps, 15.0, 500.0),
                "perf_log" => set_b(&mut c.perf_log),
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
            "theme" => self.theme.clone(),
            "theme_color" => self.theme_color.clone(),
            "theme_hue" => self.theme_hue.to_string(),
            "theme_intensity" => self.theme_intensity.to_string(),
            "theme_edge" => self.theme_edge.to_string(),
            "refraction" => b(self.refraction),
            "notch" => b(self.notch),
            "notch_style" => self.notch_style.clone(),
            "lip_width" => self.lip_width.to_string(),
            "lip_height" => self.lip_height.to_string(),
            "notch_edge" => self.notch_edge.clone(),
            "notch_position" => self.notch_position.to_string(),
            "hide_in_fullscreen" => b(self.hide_in_fullscreen),
            "live_activity" => b(self.live_activity),
            "visualizer" => b(self.visualizer),
            "lip_visualizer" => b(self.lip_visualizer),
            "widget_visualizer" => b(self.widget_visualizer),
            "volume_osd" => b(self.volume_osd),
            "brightness_osd" => b(self.brightness_osd),
            "hide_windows_flyout" => b(self.hide_windows_flyout),
            "scroll_volume" => b(self.scroll_volume),
            "controls_mixer" => b(self.controls_mixer),
            "usb_popups" => self.usb_popups.clone(),
            "usb_unplug" => b(self.usb_unplug),
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
            "taskbar_peek" => b(self.taskbar_peek),
            "taskbar_screens" => self.taskbar_screens.clone(),
            "taskbar_screen_apps" => self.taskbar_screen_apps.clone(),
            "taskbar_screen_clock" => b(self.taskbar_screen_clock),
            "taskbar_style" => self.taskbar_style.clone(),
            "taskbar_align" => self.taskbar_align.clone(),
            "taskbar_start" => self.taskbar_start.clone(),
            "taskbar_gap" => self.taskbar_gap.to_string(),
            "taskbar_pad" => self.taskbar_pad.to_string(),
            "taskbar_tile" => self.taskbar_tile.to_string(),
            "tray_icons" => b(self.tray_icons),
            "admin_helper" => b(self.admin_helper),
            "start_position" => self.start_position.clone(),
            "start_height" => self.start_height.to_string(),
            "start_columns" => self.start_columns.to_string(),
            "start_pinned_rows" => self.start_pinned_rows.to_string(),
            "start_show_sidebar" => b(self.start_show_sidebar),
            "start_sidebar_compact" => b(self.start_sidebar_compact),
            "start_show_recent" => b(self.start_show_recent),
            "start_show_pinned" => b(self.start_show_pinned),
            "start_show_profile" => b(self.start_show_profile),
            "start_show_cascade" => b(self.start_show_cascade),
            _ if place_index(key).is_some() => b(self.start_places[place_index(key).unwrap_or(0)]),
            "start_show_all_apps" => b(self.start_show_all_apps),
            "start_web_search" => self.start_web_search.clone(),
            "start_opacity" => self.start_opacity.to_string(),
            "taskbar_corners" => self.taskbar_corners.clone(),
            "acrylic" => b(self.acrylic),
            "taskbar_opacity" => self.taskbar_opacity.to_string(),
            "show_search" => b(self.show_search),
            "show_status" => b(self.show_status),
            "status_click" => self.status_click.clone(),
            "show_clock" => b(self.show_clock),
            "alt_tab" => b(self.alt_tab),
            "alt_tab_style" => self.alt_tab_style.clone(),
            "mouse_match" => b(self.mouse_match),
            "mouse_align" => self.mouse_align.clone(),
            "screen_inches" => self.screen_inches.clone(),
            "taskbar_left" => self.taskbar_left.clone(),
            "taskbar_right" => self.taskbar_right.clone(),
            k if k.starts_with("place_") => self.place_of(&k["place_".len()..]).to_string(),
            "widgets_panel" => self.widgets_panel.clone(),
            "widgets_columns" => self.widgets_columns.to_string(),
            "gpu_cards" => self.gpu_cards.clone(),
            "gpu_temp" => b(self.gpu_temp),
            "gpu_value" => self.gpu_value.clone(),
            "gpu_label" => self.gpu_label.clone(),
            "widgets_notifications" => b(self.widgets_notifications),
            "stats_seconds" => self.stats_seconds.to_string(),
            "launcher" => b(self.launcher),
            "launcher_win_key" => b(self.launcher_win_key),
            "shortcut_win_x" => b(self.shortcut_win_x),
            "shortcut_win_n" => b(self.shortcut_win_n),
            "shortcut_win_a" => b(self.shortcut_win_a),
            "shortcut_win_v" => b(self.shortcut_win_v),
            "shortcut_win_tab" => b(self.shortcut_win_tab),
            "focus_minutes" => self.focus_minutes.to_string(),
            "break_minutes" => self.break_minutes.to_string(),
            "calendar_url" => self.calendar_url.clone(),
            "calendar_soon" => self.calendar_soon.to_string(),
            "weather" => b(self.weather),
            "weather_location" => self.weather_location.clone(),
            "weather_click" => self.weather_click.clone(),
            "weather_url" => self.weather_url.clone(),
            "fahrenheit" => b(self.fahrenheit),
            "update_check" => b(self.update_check),
            "github_token" => self.github_token.clone(),
            "max_fps" => self.max_fps.to_string(),
            "perf_log" => b(self.perf_log),
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
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
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
