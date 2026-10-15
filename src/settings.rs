//! The settings window ("Cascade Settings" in the Start menu). Drawn with
//! Direct2D like the rest of Cascade; only the text boxes are native EDIT
//! controls. Every change is written to config.ini and applied at once.

use std::cell::RefCell;

use windows::core::{w, Result, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;
use crate::gfx::{Align, Color, Fonts, Gfx, Measure, Painter};
use crate::theme;
use crate::weather::Lookup;

const CW: f32 = 900.0;
const CH: f32 = 620.0;
const SIDEBAR: f32 = 220.0;
const ROW_H: f32 = 58.0;
const ROW_GAP: f32 = 8.0;
/// A group heading, and a row with no description (e.g. a sidebar shortcut).
const HEAD_H: f32 = 38.0;
const COMPACT_H: f32 = 44.0;
const TOP: f32 = 84.0;
const BG: Color = Color(0.11, 0.11, 0.12, 1.0);
/// Narrowest a segmented button gets (they widen to fit their labels).
const CHOICE_W: f32 = 84.0;
/// Each extra line of a wrapped description.
const LINE_H: f32 = 15.0;
/// Most lines a description wraps to.
const MAX_LINES: usize = 3;

pub const SECTIONS: [&str; 11] = [
    "General",
    "Appearance",
    "Notch",
    "Taskbar",
    "Start menu",
    "Shortcuts",
    "Widgets",
    "Screens",
    "Focus & weather",
    "Updates",
    "About",
];

#[derive(Clone, Copy)]
enum Unit {
    Fps,
    Px,
    Ms,
    Min,
    Times,
    Percent,
    Count,
    Seconds,
    Degrees,
}

enum Ctl {
    Toggle,
    Slider {
        min: f32,
        max: f32,
        step: f32,
        unit: Unit,
    },
    Text {
        secret: bool,
    },
    /// Segmented buttons: (config value, label).
    Choice(&'static [(&'static str, &'static str)]),
    Button(&'static str),
    /// The themes, as a grid of samples to click.
    Gallery,
    /// Tiles to drag between lanes (which widgets show, where and in what order).
    Board(Board),
    Note,
    /// A group title with no control.
    Heading,
}

struct Row {
    key: &'static str,
    title: &'static str,
    desc: &'static str,
    ctl: Ctl,
}

const fn row(key: &'static str, title: &'static str, desc: &'static str, ctl: Ctl) -> Row {
    Row { key, title, desc, ctl }
}

fn heading(key: &'static str, title: &'static str) -> Row {
    row(key, title, "", Ctl::Heading)
}

fn row_h(r: &Row) -> f32 {
    match r.ctl {
        Ctl::Heading => HEAD_H,
        Ctl::Gallery => BOARD_HEAD + gallery_rows() as f32 * (SWATCH_H + SWATCH_GAP) - SWATCH_GAP + 16.0,
        Ctl::Board(b) => BOARD_HEAD + b.lanes().len() as f32 * (LANE_H + LANE_GAP) - LANE_GAP + 16.0,
        Ctl::Note => ROW_H,
        _ if r.desc.is_empty() => COMPACT_H,
        _ => ROW_H,
    }
}

fn slider(min: f32, max: f32, step: f32, unit: Unit) -> Ctl {
    Ctl::Slider { min, max, step, unit }
}

/// Room for a board row's title and description, above its lanes.
const BOARD_HEAD: f32 = 58.0;
const LANE_H: f32 = 52.0;
const LANE_GAP: f32 = 6.0;
/// The lane's name, left of its tiles.
const LANE_LABEL: f32 = 70.0;
const TILE_W: f32 = 66.0;
const TILE_H: f32 = 42.0;
const TILE_GAP: f32 = 6.0;

/// The theme gallery: columns of samples, each a preview over its name.
const SWATCH_COLS: usize = 5;
const SWATCH_PREVIEW: f32 = 50.0;
const SWATCH_H: f32 = SWATCH_PREVIEW + 24.0;
const SWATCH_GAP: f32 = 10.0;

fn gallery_rows() -> usize {
    theme::THEMES.len().div_ceil(SWATCH_COLS)
}

/// Sample `k` of a gallery whose samples fill `area`: (x, y, w).
fn swatch_rect(area: (f32, f32, f32, f32), k: usize) -> (f32, f32, f32) {
    let w = (area.2 - (SWATCH_COLS - 1) as f32 * SWATCH_GAP) / SWATCH_COLS as f32;
    let (col, line) = (k % SWATCH_COLS, k / SWATCH_COLS);
    (area.0 + col as f32 * (w + SWATCH_GAP), area.1 + line as f32 * (SWATCH_H + SWATCH_GAP), w)
}

/// A set of things arranged by dragging tiles between lanes: shown lanes
/// (each saved as a comma-separated config value) and a last lane holding
/// the hidden ones.
#[derive(Clone, Copy, PartialEq)]
enum Board {
    /// The taskbar's widgets: left of the apps, right of them, or off.
    Taskbar,
    /// The widgets panel's cards.
    Panel,
}

impl Board {
    /// (config name, label) of everything on the board.
    fn items(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Board::Taskbar => &[
                ("weather", "Weather"),
                ("music", "Music"),
                ("cpu", "CPU"),
                ("ram", "Memory"),
                ("gpu", "GPU"),
                ("net", "Network"),
                ("widgets", "Widgets"),
            ],
            Board::Panel => {
                &[("media", "Media"), ("system", "System"), ("weather", "Weather"), ("calendar", "Calendar")]
            }
        }
    }

    fn lanes(self) -> &'static [&'static str] {
        match self {
            Board::Taskbar => &["Left", "Right", "Hidden"],
            Board::Panel => &["Shown", "Hidden"],
        }
    }

    /// The config value behind each shown lane.
    fn keys(self) -> &'static [&'static str] {
        match self {
            Board::Taskbar => &["taskbar_left", "taskbar_right"],
            Board::Panel => &["widgets_panel"],
        }
    }

    fn label(self, item: &str) -> &'static str {
        self.items().iter().find(|(k, _)| *k == item).map_or("", |(_, l)| *l)
    }

    /// What each lane holds, in order; the hidden lane last. Unknown names
    /// and repeats in the config are dropped.
    fn read(self, get: &dyn Fn(&str) -> String) -> Vec<Vec<&'static str>> {
        let mut seen: Vec<&'static str> = Vec::new();
        let mut lanes = Vec::new();
        for key in self.keys() {
            let mut lane = Vec::new();
            for name in get(key).split(',') {
                let name = name.trim().to_ascii_lowercase();
                // Other spellings the taskbar and panel accept.
                let name = match (self, name.as_str()) {
                    (Board::Taskbar, "memory") => "ram",
                    (Board::Taskbar, "network") => "net",
                    (Board::Panel, "music") => "media",
                    (Board::Panel, "stats") => "system",
                    (_, n) => n,
                };
                if let Some(&(item, _)) = self.items().iter().find(|(k, _)| *k == name) {
                    if !seen.contains(&item) {
                        seen.push(item);
                        lane.push(item);
                    }
                }
            }
            lanes.push(lane);
        }
        lanes.push(self.items().iter().map(|(k, _)| *k).filter(|k| !seen.contains(k)).collect());
        lanes
    }
}

/// A tile being dragged on a board (layout units).
#[derive(Clone, Copy)]
struct TileDrag {
    row: usize,
    item: &'static str,
    lane: usize,
    start: (f32, f32),
    at: (f32, f32),
    /// Where in the tile it was picked up.
    grab: (f32, f32),
    /// Dragged rather than only clicked.
    moved: bool,
}

/// Lane `j` of a board whose lanes start at `area`.
fn lane_rect(area: (f32, f32, f32, f32), j: usize) -> (f32, f32, f32, f32) {
    (area.0, area.1 + j as f32 * (LANE_H + LANE_GAP), area.2, LANE_H)
}

/// Tile slot `k` in a lane.
fn tile_rect(lane: (f32, f32, f32, f32), k: usize) -> (f32, f32) {
    (lane.0 + LANE_LABEL + k as f32 * (TILE_W + TILE_GAP), lane.1 + (LANE_H - TILE_H) / 2.0)
}

/// The lane and slot a tile dropped at (x, y) goes to, given the lanes
/// without it.
fn drop_slot(area: (f32, f32, f32, f32), lanes: &[Vec<&'static str>], x: f32, y: f32) -> (usize, usize) {
    let j = ((y - area.1) / (LANE_H + LANE_GAP)).floor().clamp(0.0, lanes.len() as f32 - 1.0) as usize;
    let lane = lane_rect(area, j);
    let k = (0..lanes[j].len()).filter(|&k| tile_rect(lane, k).0 + TILE_W / 2.0 < x).count();
    (j, k)
}

/// The little picture on a board tile.
fn tile_icon(p: &Painter, item: &str, cx: f32, cy: f32, c: Color) {
    match item {
        "weather" => crate::ui::weather_icon(p, cx, cy, 17.0, 2, true, c.3),
        "music" | "media" => {
            p.circle(cx - 2.4, cy + 4.0, 2.8, c);
            p.line(cx + 0.2, cy + 4.0, cx + 0.2, cy - 6.0, c, 1.4);
            p.line(cx + 0.2, cy - 6.0, cx + 5.0, cy - 3.6, c, 1.4);
        }
        "cpu" => {
            p.stroke_rrect(cx - 5.0, cy - 5.0, 10.0, 10.0, 1.5, c, 1.3);
            p.rrect(cx - 2.0, cy - 2.0, 4.0, 4.0, 0.8, c);
            for t in [-2.5, 2.5] {
                p.line(cx + t, cy - 8.0, cx + t, cy - 5.5, c, 1.2);
                p.line(cx + t, cy + 5.5, cx + t, cy + 8.0, c, 1.2);
                p.line(cx - 8.0, cy + t, cx - 5.5, cy + t, c, 1.2);
                p.line(cx + 5.5, cy + t, cx + 8.0, cy + t, c, 1.2);
            }
        }
        "ram" => {
            p.stroke_rrect(cx - 8.0, cy - 4.5, 16.0, 8.0, 1.5, c, 1.3);
            for i in 0..3 {
                p.rrect(cx - 5.5 + i as f32 * 4.0, cy - 2.5, 3.0, 4.0, 0.6, c);
            }
            for i in 0..4 {
                let x = cx - 6.0 + i as f32 * 4.0;
                p.line(x, cy + 3.5, x, cy + 6.0, c, 1.2);
            }
        }
        "gpu" => {
            p.stroke_rrect(cx - 7.0, cy - 5.0, 15.0, 10.0, 1.5, c, 1.3);
            p.stroke_circle(cx + 1.0, cy, 2.8, c, 1.2);
            p.line(cx - 7.0, cy + 5.0, cx - 7.0, cy + 7.5, c, 1.3);
        }
        "net" => {
            p.line(cx - 3.0, cy + 6.0, cx - 3.0, cy - 4.0, c, 1.4);
            p.polygon(&[(cx - 6.0, cy - 3.0), (cx - 3.0, cy - 7.0), (cx, cy - 3.0)], c);
            p.line(cx + 3.0, cy - 6.0, cx + 3.0, cy + 4.0, c, 1.4);
            p.polygon(&[(cx, cy + 3.0), (cx + 3.0, cy + 7.0), (cx + 6.0, cy + 3.0)], c);
        }
        "widgets" => {
            for (dx, dy) in [(-6.0, -6.0), (1.0, -6.0), (-6.0, 1.0), (1.0, 1.0)] {
                p.rrect(cx + dx, cy + dy, 5.0, 5.0, 1.4, c);
            }
        }
        "system" => {
            p.arc(cx, cy + 2.5, 7.0, std::f32::consts::PI, std::f32::consts::TAU, c, 1.4);
            p.line(cx, cy + 2.5, cx + 3.5, cy - 2.5, c, 1.4);
            p.circle(cx, cy + 2.5, 1.6, c);
        }
        "calendar" => {
            p.stroke_rrect(cx - 6.0, cy - 5.0, 12.0, 11.0, 2.2, c, 1.3);
            p.line(cx - 6.0, cy - 1.5, cx + 6.0, cy - 1.5, c, 1.3);
            p.line(cx - 3.0, cy - 7.0, cx - 3.0, cy - 4.0, c, 1.3);
            p.line(cx + 3.0, cy - 7.0, cx + 3.0, cy - 4.0, c, 1.3);
        }
        _ => {}
    }
}

/// A board tile: its picture over its name.
fn draw_tile(p: &Painter, f: &Fonts, item: &str, label: &str, x: f32, y: f32, bg: f32, a: f32) {
    p.rrect(x, y, TILE_W, TILE_H, 9.0, Color::white(bg));
    tile_icon(p, item, x + TILE_W / 2.0, y + 15.0, Color::white(0.92 * a));
    p.text(label, &f.small, x, y + 24.0, TILE_W, 16.0, Align::Center, Color::white(0.8 * a));
}

/// A fully saturated colour at `hue` degrees round the colour wheel.
fn hue_color(hue: f32) -> Color {
    let h = hue.rem_euclid(360.0) / 60.0;
    let x = 1.0 - (h.rem_euclid(2.0) - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    Color(r, g, b, 1.0)
}

const GOOD: Color = Color(0.42, 0.84, 0.52, 1.0);
const BAD: Color = Color(1.0, 0.46, 0.42, 1.0);
const WARN: Color = Color(0.98, 0.76, 0.36, 1.0);

/// The line under the Location box: whether the place was found (and
/// which one), still being looked up, or not found. `on` is whether the
/// weather is turned on at all.
fn location_note(location: &str, on: bool) -> Option<(String, Color, Option<bool>)> {
    if !on {
        return None;
    }
    Some(match crate::weather::lookup(location)? {
        Lookup::Checking => ("Looking it up\u{2026}".into(), Color::white(0.55), None),
        Lookup::Found(name) if location.trim().is_empty() => (format!("From your IP: {name}"), GOOD, Some(true)),
        Lookup::Found(name) => (format!("Found: {name}"), GOOD, Some(true)),
        Lookup::NotFound => ("Not found. Try \"City, Country\" or lat,lon.".into(), BAD, Some(false)),
        Lookup::Offline => ("Couldn't check: no connection. Retrying soon.".into(), WARN, None),
    })
}

fn rows(section: usize) -> Vec<Row> {
    use Ctl::*;
    match section {
        0 => vec![
            row("autostart", "Start with Windows", "Launch Cascade when you sign in.", Toggle),
            row("hide_in_fullscreen", "Hide in fullscreen", "Get out of the way of games and videos.", Toggle),
            row("alt_tab", "Alt+Tab switcher", "Cascade's own, with live previews, instead of Windows'.", Toggle),
            row(
                "max_fps",
                "Animation frame cap",
                "Also limited to your monitor's refresh rate.",
                slider(30.0, 240.0, 10.0, Unit::Fps),
            ),
        ],
        // Everything about the look, on one page: the theme (the title and
        // description show the one picked), its colour, and the glass.
        1 => vec![
            row("theme", "Theme", "", Gallery),
            heading("h_colour", "Colour"),
            row(
                "theme_color",
                "Based on",
                "The theme's own colours, your Windows accent, none, or your own.",
                Choice(&[("theme", "Theme"), ("accent", "Accent"), ("neutral", "None"), ("custom", "Custom")]),
            ),
            row(
                "theme_hue",
                "Custom colour",
                "Drag to pick one.",
                slider(0.0, 360.0, 1.0, Unit::Degrees),
            ),
            row(
                "theme_intensity",
                "Colour intensity",
                "0% = grey, 100% = as the theme was made, 200% = twice as strong.",
                slider(0.0, 2.0, 0.05, Unit::Percent),
            ),
            heading("h_glass", "Glass"),
            row("acrylic", "Blur", "Blur what's behind panels and the lip.", Toggle),
            row(
                "taskbar_corners",
                "Corners",
                "Taskbar, Start menu, panels and the lip.",
                Choice(&[("round", "Round"), ("small", "Small"), ("square", "Square")]),
            ),
            row(
                "taskbar_opacity",
                "Tint",
                "How strong the glass's tint is: taskbar, panels and the lip.",
                slider(0.0, 1.0, 0.05, Unit::Percent),
            ),
            row(
                "start_opacity",
                "Start menu tint",
                "The same, for the Start menu.",
                slider(0.0, 1.0, 0.05, Unit::Percent),
            ),
            row(
                "theme_edge",
                "Edge",
                "How strongly the edge catches (or bends) the light. 0% = no outline.",
                slider(0.0, 2.0, 0.05, Unit::Percent),
            ),
            row(
                "refraction",
                "Refraction",
                "Clear glass bends what's behind it round its edge. Uses a little CPU.",
                Toggle,
            ),
        ],
        2 => vec![
            row("notch", "Show the notch", "The island at the top of your main screen.", Toggle),
            row(
                "notch_style",
                "Style",
                "Lip: pulled down from a thin strip on hover.",
                Choice(&[("lip", "Lip"), ("notch", "Notch")]),
            ),
            row(
                "notch_edge",
                "Edge",
                "Which edge of the screen it sits on.",
                Choice(&[("top", "Top"), ("bottom", "Bottom"), ("left", "Left"), ("right", "Right")]),
            ),
            row(
                "notch_position",
                "Position along the edge",
                "0% = the left or top end, 50% = the middle.",
                slider(0.0, 1.0, 0.01, Unit::Percent),
            ),
            row("lip_width", "Lip length", "How long the resting lip is.", slider(40.0, 400.0, 2.0, Unit::Px)),
            row("lip_height", "Lip thickness", "Push the pointer into it to open.", slider(2.0, 16.0, 1.0, Unit::Px)),
            heading("h_music", "While music or the focus timer plays"),
            row(
                "live_activity",
                "Live activity",
                "Show it on the closed notch. Off: it stays plain.",
                Toggle,
            ),
            row(
                "visualizer",
                "Bars in the notch",
                "Moving bars beside the album art when the notch grows for music, and on the open notch's Media tab.",
                Toggle,
            ),
            row(
                "lip_visualizer",
                "Moving lip line",
                "The coloured line pulses out from the middle with the music instead of staying still.",
                Toggle,
            ),
            heading("h_more", "Pop-ups, size and hover"),
            row("volume_osd", "Volume pop-up", "Show volume changes in the notch.", Toggle),
            row("brightness_osd", "Brightness pop-up", "Show brightness changes in the notch.", Toggle),
            row(
                "hide_windows_flyout",
                "Hide the Windows flyout",
                "Hide the built-in volume / brightness flyout.",
                Toggle,
            ),
            row("scroll_volume", "Scroll to change volume", "Scroll over the closed notch.", Toggle),
            row("scale", "Size", "Scales the whole notch.", slider(0.6, 2.0, 0.05, Unit::Times)),
            row(
                "idle_width",
                "Notch width",
                "Width of the closed notch (Notch style).",
                slider(100.0, 320.0, 2.0, Unit::Px),
            ),
            row(
                "expand_delay_ms",
                "Hover delay",
                "How long to hover before it opens.",
                slider(0.0, 800.0, 10.0, Unit::Ms),
            ),
        ],
        3 => vec![
            row("dock", "Show the taskbar", "Cascade's taskbar along the bottom.", Toggle),
            row("hide_taskbar", "Replace the Windows taskbar", "Hide the Windows one while Cascade's is on.", Toggle),
            row("dock_autohide", "Auto-hide", "Push the mouse to the bottom edge to show it.", Toggle),
            row(
                "taskbar_style",
                "Style",
                "One bar, or separate floating islands.",
                Choice(&[("full", "Full width"), ("floating", "Floating")]),
            ),
            row(
                "taskbar_align",
                "Alignment",
                "Where Start and your apps sit.",
                Choice(&[("center", "Centre"), ("left", "Left")]),
            ),
            row(
                "taskbar_start",
                "Start button",
                "With your apps (centre or left, as set above), or alone on its own island at the far left.",
                Choice(&[("with_apps", "With the apps"), ("left", "Far left")]),
            ),
            row("dock_icon_size", "Icon size", "Size of the app icons.", slider(16.0, 48.0, 1.0, Unit::Px)),
            row(
                "taskbar_tile",
                "Icon padding",
                "Space between an icon and the edge of its highlight.",
                slider(0.0, 16.0, 1.0, Unit::Px),
            ),
            row(
                "taskbar_pad",
                "Bar padding",
                "Gap from the bar's edge to its buttons, the same on all four sides.",
                slider(0.0, 24.0, 1.0, Unit::Px),
            ),
            row(
                "taskbar_gap",
                "Gap from the screen edge",
                "Set it equal to Bar padding for an even look. 0 = flush.",
                slider(0.0, 32.0, 1.0, Unit::Px),
            ),
            row("show_search", "Search box", "Next to the Start button.", Toggle),
            row("show_status", "Network, volume and battery", "Status icons by the clock.", Toggle),
            row(
                "status_click",
                "Clicking them",
                "The notch's Controls, or Windows' quick settings.",
                Choice(&[("notch", "Notch"), ("windows", "Windows")]),
            ),
            row("show_clock", "Clock", "Opens the widgets panel, with your notifications, when clicked.", Toggle),
            row(
                "tray_icons",
                "App icons by the clock",
                "Tray icons. Which show is set in Windows' taskbar settings.",
                Toggle,
            ),
            row(
                "dock_magnify",
                "Magnification",
                "Grow icons under the cursor (0 for the Windows look).",
                slider(0.0, 1.0, 0.05, Unit::Percent),
            ),
        ],
        4 => vec![
            heading("h_open", "Opening"),
            row("launcher", "Cascade Start menu", "Opened by the Start button on the taskbar.", Toggle),
            row("launcher_win_key", "Open with the Win key", "Win+E, Win+R and other shortcuts keep working.", Toggle),
            row(
                "admin_helper",
                "Work over admin apps",
                "So the Win key works over Task Manager. Asks for permission once.",
                Toggle,
            ),
            heading("h_look", "Look"),
            row(
                "start_position",
                "Position",
                "Where it opens. Auto follows the taskbar's alignment.",
                Choice(&[("auto", "Auto"), ("center", "Centre"), ("left", "Left")]),
            ),
            row("start_height", "Height", "How tall the menu is.", slider(500.0, 800.0, 10.0, Unit::Px)),
            heading("h_parts", "What it shows"),
            row("start_show_pinned", "Pinned apps", "The grid of pinned apps at the top.", Toggle),
            row("start_columns", "Pinned columns", "Apps per row in Pinned.", slider(3.0, 6.0, 1.0, Unit::Count)),
            row("start_pinned_rows", "Pinned rows", "Rows of pinned apps.", slider(1.0, 4.0, 1.0, Unit::Count)),
            row("start_show_recent", "Recent files", "Under the pinned apps.", Toggle),
            row("start_show_all_apps", "All apps list", "Every app A to Z on the right.", Toggle),
            row("start_show_sidebar", "Sidebar", "Account, folders, settings and power.", Toggle),
            row(
                "start_sidebar_compact",
                "Compact sidebar",
                "A slim icon rail. The button at its top opens the full sidebar.",
                Toggle,
            ),
            heading("h_side", "Sidebar buttons"),
            row("start_show_profile", "Account button", "Your name at the top of the sidebar.", Toggle),
            row("start_show_cascade", "Cascade button", "Opens Cascade's settings, above Power.", Toggle),
            heading("h_places", "Sidebar shortcuts"),
            row("start_place_settings", "Settings", "", Toggle),
            row("start_place_explorer", "File Explorer", "", Toggle),
            row("start_place_downloads", "Downloads", "", Toggle),
            row("start_place_documents", "Documents", "", Toggle),
            row("start_place_pictures", "Pictures", "", Toggle),
            row("start_place_music", "Music", "", Toggle),
            row("start_place_videos", "Videos", "", Toggle),
            row("start_place_network", "Network", "", Toggle),
        ],
        5 => vec![
            heading("h_menus", "Windows' shortcut menus, in Cascade's look"),
            row(
                "shortcut_win_x",
                "Win+X",
                "The quick links menu (Device Manager, Terminal, Task Manager, shut down...) above Start. Right-clicking Start opens it too.",
                Toggle,
            ),
            row(
                "shortcut_win_n",
                "Win+N",
                "The widgets panel, with your notifications and the calendar.",
                Toggle,
            ),
            row(
                "shortcut_win_a",
                "Win+A",
                "The notch's Controls (Wi-Fi, Bluetooth, volume, brightness) instead of Windows' quick settings.",
                Toggle,
            ),
            row(
                "shortcuts_note",
                "Turned off",
                "The key opens Windows' own menu, as usual. Other Win+key shortcuts are never touched.",
                Note,
            ),
        ],
        6 => vec![
            heading("h_bar", "Taskbar"),
            row(
                "taskbar_board",
                "Taskbar widgets",
                "Drag to arrange, or between sides. Click one to show or hide it.",
                Board(self::Board::Taskbar),
            ),
            heading("h_panel", "Widgets panel"),
            row(
                "panel_board",
                "Panel cards",
                "Drag to put them in order. Click one to show or hide it.",
                Board(self::Board::Panel),
            ),
            row(
                "widget_visualizer",
                "Music visualizer",
                "Bars that move with the music, on the music widget and the panel's music card.",
                Toggle,
            ),
            row(
                "widgets_notifications",
                "Notifications card",
                "Windows' notifications at the top of the panel. The clock opens it.",
                Toggle,
            ),
            row("widgets_columns", "Panel columns", "Cards side by side.", Choice(&[("1", "One"), ("2", "Two")])),
            row(
                "stats_seconds",
                "Stats refresh",
                "Seconds between readings (only while one is on screen).",
                slider(1.0, 10.0, 1.0, Unit::Seconds),
            ),
        ],
        7 => vec![
            row(
                "mouse_match",
                "Match the pointer across screens",
                "Keeps the pointer's real-world height when it crosses between screens of different sizes.",
                Toggle,
            ),
            row(
                "mouse_align",
                "Screens line up at the",
                "Auto reads it from the Windows arrangement.",
                Choice(&[("auto", "Auto"), ("top", "Top"), ("middle", "Middle"), ("bottom", "Bottom")]),
            ),
            row(
                "screen_inches",
                "Screen sizes",
                "Diagonal inches, left to right (27,24). Empty = read from the screens.",
                Text { secret: false },
            ),
            row("screens_info", "Detected", "", Note),
        ],
        8 => vec![
            row("focus_minutes", "Focus length", "Length of a focus session.", slider(5.0, 90.0, 5.0, Unit::Min)),
            row("break_minutes", "Break length", "Length of a break.", slider(1.0, 30.0, 1.0, Unit::Min)),
            row("weather", "Show weather", "On the taskbar, and in the notch when nothing plays.", Toggle),
            row(
                "weather_location",
                "Location",
                "City, or lat,lon. Empty = detect from your IP.",
                Text { secret: false },
            ),
            row("fahrenheit", "Use Fahrenheit", "Otherwise Celsius.", Toggle),
            row(
                "weather_click",
                "Clicking the weather",
                "Opens this.",
                Choice(&[
                    ("panel", "Panel"),
                    ("google", "Google"),
                    ("bing", "Bing"),
                    ("custom", "Custom"),
                    ("none", "Nothing"),
                ]),
            ),
            row("weather_url", "Custom page", "Used by Custom. {q} is replaced by the search.", Text { secret: false }),
        ],
        9 => vec![
            row("update_now", "Update Cascade", "Download and install the newest build.", Button("Update now")),
            row("update_check", "Check automatically", "Look for a new build every few hours.", Toggle),
            row("github_token", "GitHub token", "Only needed while the repo is private.", Text { secret: true }),
        ],
        _ => vec![
            row("about", "Cascade", "A tiny native notch, taskbar and Start menu for Windows.", Note),
            row("resources", "Resource use", "", Note),
            row("open_config", "Config file", "Every setting, as plain text.", Button("Open")),
            row("open_folder", "Data folder", "Config, pinned apps and logs.", Button("Open")),
        ],
    }
}

/// Whether row `key` of `section` would change anything with the other
/// settings as they are. Rows that wouldn't (a lip setting in Notch style,
/// the Start menu's parts with Cascade's Start menu off) are left out, and
/// come back as soon as they would.
fn shown(section: usize, key: &str, get: &dyn Fn(&str) -> String) -> bool {
    let on = |k: &str| get(k) == "true";
    let lip = get("notch_style") == "lip";
    let side = matches!(get("notch_edge").as_str(), "left" | "right");
    match (section, key) {
        // Appearance: what the theme picked can use.
        (1, "theme_hue") => get("theme_color") == "custom",
        // With no colour everything is grey already.
        (1, "theme_intensity") => get("theme_color") != "neutral",
        (1, "acrylic" | "refraction") => {
            let theme = get("theme");
            let t = theme::THEMES.iter().find(|t| t.key == theme).copied().unwrap_or(theme::THEMES[0]);
            t.glass != theme::Glass::Solid && (key == "acrylic" || (on("acrylic") && t.refract.is_some()))
        }
        // Notch.
        (2, "notch") => true,
        (2, _) if !on("notch") => false,
        (2, "lip_width" | "lip_height") => lip,
        (2, "idle_width") => !lip,
        // The line only shows on a slim strip, and only with Live activity.
        (2, "lip_visualizer") => on("live_activity") && (lip || side),
        (2, "hide_windows_flyout") => on("volume_osd") || on("brightness_osd"),
        // Taskbar.
        (3, "dock") => true,
        (3, _) if !on("dock") => false,
        (3, "status_click") => on("show_status"),
        // Start menu: the helper also serves Alt+Tab and the shortcuts.
        (4, "launcher" | "admin_helper") => true,
        (4, _) if !on("launcher") => false,
        (4, "start_columns" | "start_pinned_rows") => on("start_show_pinned"),
        (4, k) if k.starts_with("start_place_") => on("start_show_sidebar"),
        (4, "start_sidebar_compact" | "start_show_profile" | "start_show_cascade") => on("start_show_sidebar"),
        // Widgets: settings for widgets that aren't placed anywhere.
        (6, "widget_visualizer") => {
            let bar = Board::Taskbar.read(get);
            bar[..2].iter().any(|l| l.contains(&"music")) || Board::Panel.read(get)[0].contains(&"media")
        }
        // The open panel always reads once a second; this is the taskbar's.
        (6, "stats_seconds") => {
            Board::Taskbar.read(get)[..2].iter().flatten().any(|n| matches!(*n, "cpu" | "ram" | "gpu" | "net"))
        }
        (6, "widgets_columns") => Board::Panel.read(get)[0].len() > 1,
        // Screens.
        (7, "mouse_align" | "screen_inches") => on("mouse_match"),
        // Weather.
        (8, "weather_location" | "fahrenheit" | "weather_click") => on("weather"),
        (8, "weather_url") => on("weather") && get("weather_click") == "custom",
        _ => true,
    }
}

/// The rows of `section` that apply (see `shown`), without headings left
/// with nothing under them.
fn visible_rows(section: usize, get: &dyn Fn(&str) -> String) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    for r in rows(section).into_iter().filter(|r| matches!(r.ctl, Ctl::Heading) || shown(section, r.key, get)) {
        if matches!(r.ctl, Ctl::Heading) && out.last().is_some_and(|l| matches!(l.ctl, Ctl::Heading)) {
            out.pop();
        }
        out.push(r);
    }
    if out.last().is_some_and(|l| matches!(l.ctl, Ctl::Heading)) {
        out.pop();
    }
    out
}

/// Descriptions that depend on other settings: what a row does with the
/// notch as it's set up, or (true) what it's waiting on elsewhere, shown as
/// a warning.
fn live_desc(key: &str, get: &dyn Fn(&str) -> String) -> Option<(&'static str, bool)> {
    let on = |k: &str| get(k) == "true";
    let lip = get("notch_style") == "lip";
    let side = matches!(get("notch_edge").as_str(), "left" | "right");
    let taskbar_off = "Needs Cascade's taskbar: turn on Taskbar > Show the taskbar.";
    Some(match key {
        "live_activity" if lip => (
            "A coloured line lights up on the closed lip, in the album art's colour or showing the timer's progress. Off: the lip stays plain.",
            false,
        ),
        "live_activity" if side => (
            "A coloured line lights up on the closed notch, in the album art's colour or showing the timer's progress. Off: it stays plain.",
            false,
        ),
        "live_activity" => {
            ("The closed notch grows to show the album art or the timer. Off: it stays plain.", false)
        }
        // Without the notch growing for music, the Media tab is all that's left.
        "visualizer" if lip || side || !on("live_activity") => ("Moving bars on the open notch's Media tab.", false),
        "shortcut_win_n" if !on("dock") => (taskbar_off, true),
        "shortcut_win_a" if !on("notch") => ("Needs the notch: turn on Notch > Show the notch.", true),
        "status_click" if get("status_click") == "notch" && !on("notch") => {
            ("The notch is off, so they open Windows' quick settings.", true)
        }
        "taskbar_board" | "panel_board" if !on("dock") => (taskbar_off, true),
        "start_opacity" if !on("launcher") => {
            ("Needs Cascade's Start menu: turn on Start menu > Cascade Start menu.", true)
        }
        _ => return None,
    })
}

fn fmt(v: f32, unit: Unit) -> String {
    match unit {
        Unit::Fps => format!("{v:.0} fps"),
        Unit::Px => format!("{v:.0} px"),
        Unit::Ms => format!("{v:.0} ms"),
        Unit::Min => format!("{v:.0} min"),
        Unit::Times => format!("{v:.2}\u{d7}"),
        Unit::Percent => format!("{:.0}%", v * 100.0),
        Unit::Count => format!("{v:.0}"),
        Unit::Seconds => format!("{v:.0} s"),
        Unit::Degrees => format!("{v:.0}\u{b0}"),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Nav(usize),
    Row(usize),
    /// The "reset to default" button of a row.
    Reset(usize),
    /// A board tile: row, lane, slot.
    Tile(usize, usize, usize),
    /// A theme in the gallery: row, theme.
    Swatch(usize, usize),
}

/// The default for `key`, if the row should offer a reset button.
fn default_of(key: &str) -> Option<String> {
    if matches!(key, "autostart" | "github_token" | "admin_helper") {
        return None;
    }
    let d = Config::default().get(key);
    (!d.is_empty() || matches!(key, "weather_location" | "weather_url")).then_some(d)
}

/// Whether `value` differs from the default (sliders compare as numbers).
fn changed(r: &Row, value: &str) -> Option<String> {
    let d = default_of(r.key)?;
    let differs = match r.ctl {
        Ctl::Slider { step, .. } => match (value.parse::<f32>(), d.parse::<f32>()) {
            (Ok(a), Ok(b)) => (a - b).abs() > step / 2.0,
            _ => value != d,
        },
        Ctl::Toggle | Ctl::Choice(_) | Ctl::Text { .. } => value != d,
        _ => false,
    };
    differs.then_some(d)
}

pub enum Change {
    Set(&'static str, String),
    /// Several values at once (e.g. both sides of the taskbar).
    Many(Vec<(&'static str, String)>),
    Action(&'static str),
}

pub struct Settings {
    pub hwnd: HWND,
    k: f32,
    section: usize,
    scroll: f32,
    hovered: Option<Hit>,
    tracking: bool,
    drag: Option<(usize, f32)>,
    tile_drag: Option<TileDrag>,
    /// Text boxes: window, row key, and the text they were loaded with (only
    /// a box the user actually edited is written back). Every text row of the
    /// section has one; those of rows left out are hidden.
    edits: Vec<(HWND, &'static str, String)>,
    font: HFONT,
    pub brush: HBRUSH,
    measure: Option<Measure>,
    /// The current section's rows sized to their text.
    fit: RefCell<Fit>,
}

/// The rows shown (see `shown`), their heights (descriptions wrap onto
/// more lines) and segmented-button widths, worked out for one section.
#[derive(Default)]
struct Fit {
    section: Option<usize>,
    keys: Vec<&'static str>,
    heights: Vec<f32>,
    segs: Vec<f32>,
}

fn edit_text(h: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(h, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

impl Settings {
    /// Opens the window (class `CascadeSettings`, registered by the app).
    pub fn create(instance: HINSTANCE) -> Result<Self> {
        unsafe {
            let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
            let hwnd = CreateWindowExW(
                WS_EX_APPWINDOW,
                w!("CascadeSettings"),
                w!("Cascade Settings"),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                100,
                100,
                None,
                None,
                Some(instance),
                None,
            )?;
            let dark = BOOL(1);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
            // Caption colour matching the content (Windows 11).
            let caption = COLORREF(0x001F1C1C);
            let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(35), &caption as *const _ as *const _, 4);
            let mut s = Self {
                hwnd,
                k: 1.0,
                section: 0,
                scroll: 0.0,
                hovered: None,
                tracking: false,
                drag: None,
                tile_drag: None,
                edits: Vec::new(),
                font: HFONT::default(),
                brush: CreateSolidBrush(COLORREF(0x002B2B2B)),
                measure: Measure::new().ok(),
                fit: RefCell::default(),
            };
            s.apply_dpi(None);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            Ok(s)
        }
    }

    /// Sizes the window for its DPI (centred, or at `suggested`).
    pub fn apply_dpi(&mut self, suggested: Option<RECT>) {
        unsafe {
            self.k = GetDpiForWindow(self.hwnd).max(96) as f32 / 96.0;
            let (cw, ch) = ((CW * self.k) as i32, (CH * self.k) as i32);
            let mut r = RECT { left: 0, top: 0, right: cw, bottom: ch };
            let _ = AdjustWindowRectEx(
                &mut r,
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                false,
                WS_EX_APPWINDOW,
            );
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let (x, y) = match suggested {
                Some(s) => (s.left, s.top),
                None => {
                    let mon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTOPRIMARY);
                    let mut mi =
                        MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                    let _ = GetMonitorInfoW(mon, &mut mi);
                    let wa = mi.rcWork;
                    (wa.left + (wa.right - wa.left - w) / 2, wa.top + (wa.bottom - wa.top - h) / 2)
                }
            };
            let _ = SetWindowPos(self.hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font.into());
            }
            self.font = CreateFontW(
                -(14.0 * self.k) as i32,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            );
        }
    }

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn content_w(&self) -> f32 {
        CW - SIDEBAR - 24.0 - 28.0
    }

    /// The current section's rows as shown: those that apply with the
    /// settings as they were when last painted.
    fn rows(&self) -> Vec<Row> {
        let fit = self.fit.borrow();
        let all = rows(self.section);
        if fit.section != Some(self.section) {
            return all;
        }
        all.into_iter().filter(|r| fit.keys.contains(&r.key)).collect()
    }

    /// How far the page is scrolled, kept in range when rows above the end
    /// are left out.
    fn scroll(&self) -> f32 {
        let content = TOP + self.offset(usize::MAX) + 16.0;
        self.scroll.min((content - CH).max(0.0))
    }

    fn row_y(&self, i: usize) -> f32 {
        TOP + self.offset(i) - self.scroll()
    }

    /// Height of row `i` of the current section.
    fn row_h(&self, i: usize) -> f32 {
        let fit = self.fit.borrow();
        match fit.heights.get(i) {
            Some(&h) if fit.section == Some(self.section) => h,
            _ => self.rows().get(i).map_or(ROW_H, row_h),
        }
    }

    /// Distance from the top of the first row to the top of row `i`.
    fn offset(&self, i: usize) -> f32 {
        let fit = self.fit.borrow();
        if fit.section == Some(self.section) {
            return fit.heights.iter().take(i).map(|h| h + ROW_GAP).sum();
        }
        drop(fit);
        self.rows().iter().take(i).map(|r| row_h(r) + ROW_GAP).sum()
    }

    /// Width of each segment of row `i`'s segmented buttons.
    fn seg_w(&self, i: usize) -> f32 {
        let fit = self.fit.borrow();
        match fit.segs.get(i) {
            Some(&w) if fit.section == Some(self.section) => w,
            _ => CHOICE_W,
        }
    }

    /// Room for row `i`'s title and description: up to its control (and the
    /// reset button beside it).
    fn text_w(&self, i: usize, r: &Row) -> f32 {
        let cw = self.content_w();
        match r.ctl {
            Ctl::Board(_) | Ctl::Gallery | Ctl::Note | Ctl::Heading => cw - 36.0,
            _ => {
                let (rx, ..) = self.reset_rect(i, &r.ctl);
                rx - 8.0 - (SIDEBAR + 24.0 + 18.0)
            }
        }
    }

    /// A row's description as shown: live text for some rows, and for the
    /// weather location a note (with its colour and a tick or cross).
    fn desc(&self, r: &Row, get: &dyn Fn(&str) -> String, build: u32) -> (String, Color, Option<bool>) {
        let picked =
            matches!(r.ctl, Ctl::Gallery).then(|| theme::THEMES.iter().find(|t| t.key == get(r.key))).flatten();
        let desc = if let Some(t) = picked {
            t.about.to_string()
        } else if r.key == "about" && build > 0 {
            format!("{} Build {build}.", r.desc)
        } else if r.key == "screens_info" {
            crate::mouseedge::info()
        } else if r.key == "resources" {
            crate::usage::summary()
        } else if let Some((text, warn)) = live_desc(r.key, get) {
            return (text.to_string(), if warn { WARN } else { Color::white(0.55) }, None);
        } else if r.key == "admin_helper" && get(r.key) == "true" {
            // Live status, so it's clear whether it's working.
            if crate::helper::running() {
                "Running.".to_string()
            } else {
                "Not running. Details in cascade.log in the data folder.".to_string()
            }
        } else {
            r.desc.to_string()
        };
        // Live feedback on the location: found (and where), or not.
        let note =
            if r.key == "weather_location" { location_note(&get(r.key), get("weather") == "true") } else { None };
        note.unwrap_or((desc, Color::white(0.55), None))
    }

    /// Picks the current section's rows that apply and sizes them to their
    /// text: segmented buttons wide enough for their labels, descriptions
    /// wrapped instead of cut off. Returns whether anything moved.
    fn refit(&self, get: &dyn Fn(&str) -> String, build: u32) -> bool {
        let rows = visible_rows(self.section, get);
        let m = self.measure.as_ref();
        let segs: Vec<f32> = rows
            .iter()
            .map(|r| match (&r.ctl, m) {
                (Ctl::Choice(opts), Some(m)) => {
                    opts.iter().map(|(_, l)| (m.width(l, &m.body) + 28.0).ceil()).fold(CHOICE_W, f32::max)
                }
                _ => CHOICE_W,
            })
            .collect();
        let before = std::mem::replace(
            &mut *self.fit.borrow_mut(),
            Fit {
                section: Some(self.section),
                keys: rows.iter().map(|r| r.key).collect(),
                heights: rows.iter().map(row_h).collect(),
                segs,
            },
        );
        let Some(m) = m else {
            let fit = self.fit.borrow();
            return before.section != fit.section || before.keys != fit.keys;
        };
        let heights: Vec<f32> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let base = row_h(r);
                if matches!(r.ctl, Ctl::Board(_) | Ctl::Gallery | Ctl::Heading) {
                    return base;
                }
                let (desc, _, mark) = self.desc(r, get, build);
                if desc.is_empty() {
                    return base;
                }
                let w = self.text_w(i, r) - if mark.is_some() { 15.0 } else { 0.0 };
                let lines = m.lines(&desc, &m.small, w).min(MAX_LINES);
                base + (lines - 1) as f32 * LINE_H
            })
            .collect();
        let mut fit = self.fit.borrow_mut();
        fit.heights = heights;
        before.section != fit.section
            || before.keys != fit.keys
            || before.heights != fit.heights
            || before.segs != fit.segs
    }

    /// Moves the text boxes onto their rows (after rows above changed height
    /// or were left out), hiding those whose rows are left out.
    fn place_edits(&self) {
        let k = self.k;
        let pad = (6.0 * k) as i32;
        let rows = self.rows();
        for &(e, key, _) in &self.edits {
            unsafe {
                let Some(i) = rows.iter().position(|r| r.key == key) else {
                    let _ = ShowWindow(e, SW_HIDE);
                    continue;
                };
                let (x, y, w, h) = self.ctl_rect(i, &rows[i].ctl);
                let _ = SetWindowPos(
                    e,
                    None,
                    (x * k) as i32 + pad,
                    (y * k) as i32 + pad,
                    (w * k) as i32 - pad * 2,
                    (h * k) as i32 - pad * 2,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    }

    /// Control rect (x, y, w, h) of row `i`.
    fn ctl_rect(&self, i: usize, ctl: &Ctl) -> (f32, f32, f32, f32) {
        let x1 = SIDEBAR + 24.0 + self.content_w() - 18.0;
        let y = self.row_y(i);
        let h = self.row_h(i);
        match ctl {
            Ctl::Toggle => (x1 - 44.0, y + (h - 22.0) / 2.0, 44.0, 22.0),
            Ctl::Slider { .. } => (x1 - 170.0, y + 14.0, 170.0, h - 28.0),
            Ctl::Text { .. } => (x1 - 250.0, y + (h - 30.0) / 2.0, 250.0, 30.0),
            Ctl::Choice(opts) => {
                let w = opts.len() as f32 * self.seg_w(i);
                (x1 - w, y + (h - 32.0) / 2.0, w, 32.0)
            }
            Ctl::Button(_) => (x1 - 130.0, y + (h - 32.0) / 2.0, 130.0, 32.0),
            // The samples, under the title.
            Ctl::Gallery => (
                SIDEBAR + 24.0 + 18.0,
                y + BOARD_HEAD,
                self.content_w() - 36.0,
                gallery_rows() as f32 * (SWATCH_H + SWATCH_GAP) - SWATCH_GAP,
            ),
            // The lanes, under the title.
            Ctl::Board(b) => (
                SIDEBAR + 24.0 + 18.0,
                y + BOARD_HEAD,
                self.content_w() - 36.0,
                b.lanes().len() as f32 * (LANE_H + LANE_GAP) - LANE_GAP,
            ),
            Ctl::Note | Ctl::Heading => (x1, y, 0.0, 0.0),
        }
    }

    /// The reset button of row `i` (x, y, size).
    fn reset_rect(&self, i: usize, ctl: &Ctl) -> (f32, f32, f32) {
        let (cx, _, _, _) = self.ctl_rect(i, ctl);
        let x = match ctl {
            Ctl::Slider { .. } => cx - 112.0,
            _ => cx - 36.0,
        };
        (x, self.row_y(i) + (self.row_h(i) - 26.0) / 2.0, 26.0)
    }

    /// Creates the text boxes for the current section.
    pub fn sync_edits(&mut self, get: &dyn Fn(&str) -> String) {
        self.refit(get, crate::update::current());
        unsafe {
            for (h, _, _) in self.edits.drain(..) {
                let _ = DestroyWindow(h);
            }
            let instance = GetWindowLongPtrW(self.hwnd, GWLP_HINSTANCE);
            // One for every text row, shown or not, so a row coming back
            // finds its box; `place_edits` puts them on their rows.
            for (i, r) in rows(self.section).iter().enumerate() {
                let Ctl::Text { secret } = r.ctl else { continue };
                let mut style = WS_CHILD | WS_TABSTOP | WINDOW_STYLE(0x80); // ES_AUTOHSCROLL
                if secret {
                    style |= WINDOW_STYLE(0x20); // ES_PASSWORD
                }
                let loaded = get(r.key);
                let Ok(e) = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("EDIT"),
                    &windows::core::HSTRING::from(loaded.as_str()),
                    style,
                    0,
                    0,
                    0,
                    0,
                    Some(self.hwnd),
                    Some(HMENU((1000 + i) as *mut _)),
                    Some(HINSTANCE(instance as *mut _)),
                    None,
                ) else {
                    continue;
                };
                SendMessageW(e, WM_SETFONT, Some(WPARAM(self.font.0 as usize)), Some(LPARAM(1)));
                self.edits.push((e, r.key, loaded));
            }
        }
        self.place_edits();
    }

    /// A text box lost focus: returns its new value.
    pub fn commit_edit(&self, edit: HWND) -> Option<Change> {
        let (_, key, loaded) = self.edits.iter().find(|(h, _, _)| *h == edit)?;
        let text = edit_text(edit).trim().to_string();
        (text != loaded.trim()).then_some(Change::Set(*key, text))
    }

    /// All text boxes' values (when the window closes).
    pub fn commit_all(&self) -> Vec<Change> {
        self.edits.iter().filter_map(|(h, _, _)| self.commit_edit(*h)).collect()
    }

    fn hit(&self, x: i32, y: i32, get: &dyn Fn(&str) -> String) -> Option<Hit> {
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        if lx < SIDEBAR {
            let i = ((ly - 24.0) / 40.0).floor();
            return (ly >= 24.0 && (i as usize) < SECTIONS.len()).then_some(Hit::Nav(i as usize));
        }
        for (i, r) in self.rows().iter().enumerate() {
            let (rx, ry, rs) = self.reset_rect(i, &r.ctl);
            if lx >= rx && lx < rx + rs && ly >= ry && ly < ry + rs && changed(r, &get(r.key)).is_some() {
                return Some(Hit::Reset(i));
            }
        }
        for (i, r) in self.rows().iter().enumerate() {
            if let Ctl::Gallery = r.ctl {
                let area = self.ctl_rect(i, &r.ctl);
                for k in 0..theme::THEMES.len() {
                    let (sx, sy, sw) = swatch_rect(area, k);
                    if lx >= sx && lx < sx + sw && ly >= sy && ly < sy + SWATCH_H && ly >= TOP - 8.0 {
                        return Some(Hit::Swatch(i, k));
                    }
                }
                continue;
            }
            if let Ctl::Board(b) = r.ctl {
                let area = self.ctl_rect(i, &r.ctl);
                for (j, lane) in b.read(get).iter().enumerate() {
                    for k in 0..lane.len() {
                        let (tx, ty) = tile_rect(lane_rect(area, j), k);
                        if lx >= tx && lx < tx + TILE_W && ly >= ty && ly < ty + TILE_H && ly >= TOP - 8.0 {
                            return Some(Hit::Tile(i, j, k));
                        }
                    }
                }
                continue;
            }
            let (cx, cy, cw, ch) = self.ctl_rect(i, &r.ctl);
            let (cx, cw) = if matches!(r.ctl, Ctl::Slider { .. }) { (cx - 8.0, cw + 16.0) } else { (cx, cw) };
            if lx >= cx && lx < cx + cw && ly >= cy && ly < cy + ch && ly >= TOP - 8.0 {
                return Some(Hit::Row(i));
            }
        }
        None
    }

    fn slider_value(&self, i: usize, x: i32) -> Option<f32> {
        let rs = self.rows();
        let Ctl::Slider { min, max, step, .. } = rs.get(i)?.ctl else { return None };
        let (cx, _, cw, _) = self.ctl_rect(i, &rs[i].ctl);
        let t = ((x as f32 / self.k - cx) / cw).clamp(0.0, 1.0);
        let v = min + (max - min) * t;
        Some(((v - min) / step).round() * step + min)
    }

    pub fn on_move(&mut self, x: i32, y: i32, get: &dyn Fn(&str) -> String) {
        unsafe {
            if !self.tracking {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                self.tracking = TrackMouseEvent(&mut t).is_ok();
            }
        }
        if let Some(d) = &mut self.tile_drag {
            let at = (x as f32 / self.k, y as f32 / self.k);
            d.at = at;
            if (at.0 - d.start.0).hypot(at.1 - d.start.1) > 4.0 {
                d.moved = true;
            }
            self.invalidate();
            return;
        }
        if let Some((i, _)) = self.drag {
            if let Some(v) = self.slider_value(i, x) {
                self.drag = Some((i, v));
                self.invalidate();
            }
            return;
        }
        let h = self.hit(x, y, get);
        if h != self.hovered {
            self.hovered = h;
            self.invalidate();
        }
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        self.hovered = None;
        self.invalidate();
    }

    pub fn on_down(&mut self, x: i32, y: i32, get: &dyn Fn(&str) -> String) -> Option<Change> {
        let hit = self.hit(x, y, get)?;
        match hit {
            Hit::Reset(i) => {
                let r = self.rows().into_iter().nth(i)?;
                let d = changed(&r, &get(r.key))?;
                self.hovered = None;
                if matches!(r.ctl, Ctl::Text { .. }) {
                    // Show the default in the text box straight away.
                    if let Some((h, _, loaded)) = self.edits.iter_mut().find(|(_, key, _)| *key == r.key) {
                        unsafe {
                            let _ = SetWindowTextW(*h, &windows::core::HSTRING::from(d.as_str()));
                        }
                        *loaded = d.clone();
                    }
                }
                self.invalidate();
                Some(Change::Set(r.key, d))
            }
            Hit::Swatch(i, k) => {
                let r = self.rows().into_iter().nth(i)?;
                let value = theme::THEMES.get(k)?.key;
                self.invalidate();
                (get(r.key) != value).then(|| Change::Set(r.key, value.to_string()))
            }
            Hit::Tile(i, j, k) => {
                let r = self.rows().into_iter().nth(i)?;
                let Ctl::Board(b) = r.ctl else { return None };
                let item = *b.read(get).get(j)?.get(k)?;
                let (tx, ty) = tile_rect(lane_rect(self.ctl_rect(i, &r.ctl), j), k);
                let at = (x as f32 / self.k, y as f32 / self.k);
                self.tile_drag =
                    Some(TileDrag { row: i, item, lane: j, start: at, at, grab: (at.0 - tx, at.1 - ty), moved: false });
                unsafe {
                    SetCapture(self.hwnd);
                }
                self.invalidate();
                None
            }
            Hit::Nav(i) => {
                if i != self.section {
                    self.section = i;
                    self.scroll = 0.0;
                    self.hovered = None;
                    self.sync_edits(get);
                    self.invalidate();
                }
                None
            }
            Hit::Row(i) => {
                let r = self.rows().into_iter().nth(i)?;
                match r.ctl {
                    Ctl::Toggle => {
                        let on = get(r.key) == "true";
                        self.invalidate();
                        Some(Change::Set(r.key, (!on).to_string()))
                    }
                    Ctl::Slider { .. } => {
                        self.drag = Some((i, self.slider_value(i, x)?));
                        unsafe {
                            SetCapture(self.hwnd);
                        }
                        self.invalidate();
                        None
                    }
                    Ctl::Choice(opts) => {
                        let (cx, ..) = self.ctl_rect(i, &r.ctl);
                        let j = ((x as f32 / self.k - cx) / self.seg_w(i)).floor().clamp(0.0, opts.len() as f32 - 1.0);
                        let value = opts[j as usize].0;
                        (get(r.key) != value).then(|| Change::Set(r.key, value.to_string()))
                    }
                    Ctl::Button(_) => Some(Change::Action(r.key)),
                    _ => None,
                }
            }
        }
    }

    pub fn on_up(&mut self, x: i32, y: i32, get: &dyn Fn(&str) -> String) -> Option<Change> {
        if let Some(d) = self.tile_drag.take() {
            unsafe {
                let _ = ReleaseCapture();
            }
            self.invalidate();
            let r = self.rows().into_iter().nth(d.row)?;
            let Ctl::Board(b) = r.ctl else { return None };
            let mut lanes = b.read(get);
            for lane in &mut lanes {
                lane.retain(|&n| n != d.item);
            }
            let hidden = lanes.len() - 1;
            let (j, k) = if d.moved {
                drop_slot(self.ctl_rect(d.row, &r.ctl), &lanes, x as f32 / self.k, y as f32 / self.k)
            } else if d.lane == hidden {
                // A click shows a hidden one, at the end of the last lane...
                (hidden - 1, lanes[hidden - 1].len())
            } else {
                // ...and hides a shown one.
                (hidden, lanes[hidden].len())
            };
            let lane = &mut lanes[j];
            lane.insert(k.min(lane.len()), d.item);
            return Some(Change::Many(b.keys().iter().zip(&lanes).map(|(key, l)| (*key, l.join(","))).collect()));
        }
        let (i, v) = self.drag.take()?;
        unsafe {
            let _ = ReleaseCapture();
        }
        self.invalidate();
        let r = self.rows().into_iter().nth(i)?;
        if r.key == "theme_hue" {
            // Picking a colour means using it.
            return Some(Change::Many(vec![(r.key, format!("{v}")), ("theme_color", "custom".to_string())]));
        }
        Some(Change::Set(r.key, format!("{v}")))
    }

    pub fn on_wheel(&mut self, delta: i16, get: &dyn Fn(&str) -> String) {
        let content = TOP + self.offset(usize::MAX) + 16.0;
        let max = (content - CH).max(0.0);
        let before = self.scroll();
        self.scroll = (before - delta as f32 / 120.0 * 60.0).clamp(0.0, max);
        if self.scroll != before {
            self.sync_edits(get);
            self.invalidate();
        }
    }

    pub fn paint(&self, gfx: &mut Gfx, get: &dyn Fn(&str) -> String, build: u32) -> Result<()> {
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        let (pw, ph) = ((CW * self.k) as i32, (CH * self.k) as i32);
        if self.refit(get, build) {
            self.place_edits();
        }
        let r = (|| {
            gfx.begin_dc(hdc, pw, ph, self.k, BG)?;
            {
                let p = gfx.painter();
                let f = &gfx.fonts;
                // Sidebar.
                p.rrect(0.0, 0.0, SIDEBAR, CH, 0.0, Color(0.09, 0.09, 0.10, 1.0));
                for (i, name) in SECTIONS.iter().enumerate() {
                    let y = 24.0 + i as f32 * 40.0;
                    let active = i == self.section;
                    if active || self.hovered == Some(Hit::Nav(i)) {
                        p.rrect(
                            12.0,
                            y + 2.0,
                            SIDEBAR - 24.0,
                            36.0,
                            8.0,
                            Color::white(if active { 0.09 } else { 0.05 }),
                        );
                    }
                    if active {
                        p.rrect(12.0, y + 12.0, 3.0, 16.0, 1.5, theme::accent());
                    }
                    p.text(
                        name,
                        &f.body,
                        28.0,
                        y + 2.0,
                        SIDEBAR - 40.0,
                        36.0,
                        Align::Left,
                        Color::white(if active { 0.95 } else { 0.7 }),
                    );
                }
                if build > 0 {
                    p.text(
                        &format!("Build {build}"),
                        &f.small,
                        24.0,
                        CH - 40.0,
                        160.0,
                        20.0,
                        Align::Left,
                        Color::white(0.35),
                    );
                }

                // Content.
                let x0 = SIDEBAR + 24.0;
                let cw = self.content_w();
                p.clip(SIDEBAR, 0.0, CW - SIDEBAR, CH);
                p.text(
                    SECTIONS[self.section],
                    &f.big,
                    x0,
                    22.0 - self.scroll(),
                    cw,
                    44.0,
                    Align::Left,
                    Color::white(0.95),
                );
                for (i, r) in self.rows().iter().enumerate() {
                    let y = self.row_y(i);
                    let h = self.row_h(i);
                    if y + h < 0.0 || y > CH {
                        continue;
                    }
                    if matches!(r.ctl, Ctl::Heading) {
                        p.text(
                            r.title,
                            &f.label,
                            x0 + 4.0,
                            y + 12.0,
                            cw,
                            22.0,
                            Align::Left,
                            theme::lighten(theme::accent(), 0.3),
                        );
                        continue;
                    }
                    p.rrect(x0, y, cw, h, 8.0, Color::white(0.045));
                    let text_w = self.text_w(i, r);
                    // The gallery's title and description are the picked theme's.
                    let picked = matches!(r.ctl, Ctl::Gallery)
                        .then(|| theme::THEMES.iter().find(|t| t.key == get(r.key)))
                        .flatten();
                    let title = picked.map_or(r.title, |t| t.name);
                    let (desc, tone, mark) = self.desc(r, get, build);
                    if desc.is_empty() {
                        p.text(
                            title,
                            &f.label,
                            x0 + 18.0,
                            y + (h - 20.0) / 2.0,
                            text_w,
                            20.0,
                            Align::Left,
                            Color::white(0.95),
                        );
                    } else {
                        p.text(title, &f.label, x0 + 18.0, y + 9.0, text_w, 20.0, Align::Left, Color::white(0.95));
                        let mut dx = x0 + 18.0;
                        if let Some(ok) = mark {
                            let (mx, my) = (dx + 5.0, y + 39.0);
                            if ok {
                                p.line(mx - 4.0, my, mx - 1.2, my + 3.0, tone, 1.6);
                                p.line(mx - 1.2, my + 3.0, mx + 4.5, my - 3.5, tone, 1.6);
                            } else {
                                p.line(mx - 3.5, my - 3.5, mx + 3.5, my + 3.5, tone, 1.6);
                                p.line(mx - 3.5, my + 3.5, mx + 3.5, my - 3.5, tone, 1.6);
                            }
                            dx += 15.0;
                        }
                        let dw = text_w - (dx - x0 - 18.0);
                        if matches!(r.ctl, Ctl::Board(_) | Ctl::Gallery) {
                            p.text(&desc, &f.small, dx, y + 30.0, dw, 18.0, Align::Left, tone);
                        } else {
                            // Wrapped onto as many lines as the row was sized for.
                            let lines = ((h - row_h(r)) / LINE_H).round() as usize + 1;
                            p.text_wrap(&desc, &f.small, dx, y + 31.0, dw, lines as f32 * LINE_H + 2.0, tone);
                        }
                    }
                    let (cx, cy, cwid, ch) = self.ctl_rect(i, &r.ctl);
                    let hovered = self.hovered == Some(Hit::Row(i));
                    match r.ctl {
                        Ctl::Toggle => {
                            let on = get(r.key) == "true";
                            if on {
                                p.rrect(
                                    cx,
                                    cy,
                                    cwid,
                                    ch,
                                    ch / 2.0,
                                    if hovered { theme::accent_hover() } else { theme::accent() },
                                );
                                p.circle(cx + cwid - ch / 2.0, cy + ch / 2.0, ch / 2.0 - 4.0, Color::white(1.0));
                            } else {
                                p.stroke_rrect(
                                    cx + 0.5,
                                    cy + 0.5,
                                    cwid - 1.0,
                                    ch - 1.0,
                                    ch / 2.0,
                                    Color::white(if hovered { 0.75 } else { 0.55 }),
                                    1.2,
                                );
                                p.circle(
                                    cx + ch / 2.0,
                                    cy + ch / 2.0,
                                    ch / 2.0 - 5.5,
                                    Color::white(if hovered { 0.85 } else { 0.7 }),
                                );
                            }
                        }
                        Ctl::Slider { min, max, unit, .. } => {
                            let v = match self.drag {
                                Some((d, v)) if d == i => v,
                                _ => get(r.key).parse::<f32>().unwrap_or(min),
                            };
                            let t = ((v - min) / (max - min)).clamp(0.0, 1.0);
                            let ty = cy + ch / 2.0;
                            let knob = if hovered || self.drag.is_some_and(|(d, _)| d == i) { 9.0 } else { 7.5 };
                            if r.key == "theme_hue" {
                                // The colour wheel, unrolled, with the knob in the colour picked.
                                let stops: Vec<(f32, Color)> =
                                    (0..=6).map(|k| (k as f32 / 6.0, hue_color(k as f32 * 60.0))).collect();
                                if let Some(b) = p.linear(cx, ty, cx + cwid, ty, &stops) {
                                    p.fill_rrect_with(cx, ty - 3.0, cwid, 6.0, 3.0, &b);
                                }
                                p.circle(cx + cwid * t, ty, knob, Color::white(1.0));
                                p.circle(cx + cwid * t, ty, knob - 2.5, hue_color(v));
                            } else {
                                p.rrect(cx, ty - 2.0, cwid, 4.0, 2.0, Color::white(0.18));
                                p.rrect(cx, ty - 2.0, (cwid * t).max(4.0), 4.0, 2.0, theme::accent());
                                p.circle(cx + cwid * t, ty, knob, Color::white(1.0));
                            }
                            p.text(&fmt(v, unit), &f.num, cx - 84.0, y, 70.0, h, Align::Right, Color::white(0.8));
                        }
                        Ctl::Text { .. } => {
                            p.rrect(cx, cy, cwid, ch, 6.0, Color(0.17, 0.17, 0.17, 1.0));
                            p.stroke_rrect(cx + 0.5, cy + 0.5, cwid - 1.0, ch - 1.0, 6.0, Color::white(0.12), 1.0);
                        }
                        Ctl::Choice(opts) => {
                            let current = get(r.key);
                            p.rrect(cx, cy, cwid, ch, 6.0, Color::white(0.06));
                            p.stroke_rrect(cx + 0.5, cy + 0.5, cwid - 1.0, ch - 1.0, 6.0, Color::white(0.10), 1.0);
                            let seg = self.seg_w(i);
                            for (j, (value, label)) in opts.iter().enumerate() {
                                let sx = cx + j as f32 * seg;
                                let on = current == *value;
                                if on {
                                    p.rrect(sx + 3.0, cy + 3.0, seg - 6.0, ch - 6.0, 4.0, theme::accent());
                                }
                                let c = Color::white(if on { 1.0 } else { 0.75 });
                                p.text(label, &f.body, sx, cy, seg, ch, Align::Center, c);
                            }
                        }
                        Ctl::Button(label) => {
                            p.rrect(
                                cx,
                                cy,
                                cwid,
                                ch,
                                6.0,
                                if hovered { theme::accent_hover() } else { theme::accent() },
                            );
                            p.text(label, &f.label, cx, cy, cwid, ch, Align::Center, Color::white(1.0));
                        }
                        Ctl::Gallery => {
                            let picked = get(r.key);
                            for (k, t) in theme::THEMES.iter().enumerate() {
                                let (sx, sy, sw) = swatch_rect((cx, cy, cwid, ch), k);
                                let on = t.key == picked;
                                let over = self.hovered == Some(Hit::Swatch(i, k));
                                // A sample of the glass over a colourful backdrop,
                                // in the colour settings below.
                                theme::preview(&p, &theme::resolve(*t), sx, sy, sw, SWATCH_PREVIEW, 10.0);
                                if on {
                                    p.stroke_rrect(
                                        sx - 3.0,
                                        sy - 3.0,
                                        sw + 6.0,
                                        SWATCH_PREVIEW + 6.0,
                                        13.0,
                                        theme::accent(),
                                        2.0,
                                    );
                                } else if over {
                                    p.stroke_rrect(
                                        sx - 2.5,
                                        sy - 2.5,
                                        sw + 5.0,
                                        SWATCH_PREVIEW + 5.0,
                                        12.5,
                                        Color::white(0.35),
                                        1.0,
                                    );
                                }
                                let a = if on {
                                    0.95
                                } else if over {
                                    0.85
                                } else {
                                    0.65
                                };
                                p.text(
                                    t.name,
                                    &f.small,
                                    sx - 4.0,
                                    sy + SWATCH_PREVIEW + 4.0,
                                    sw + 8.0,
                                    18.0,
                                    Align::Center,
                                    Color::white(a),
                                );
                            }
                        }
                        Ctl::Board(b) => {
                            let area = (cx, cy, cwid, ch);
                            let mut lanes = b.read(get);
                            let drag = self.tile_drag.filter(|d| d.row == i && d.moved);
                            // While a tile is dragged, the others make room where it would land.
                            let target = drag.map(|d| {
                                for lane in &mut lanes {
                                    lane.retain(|&n| n != d.item);
                                }
                                drop_slot(area, &lanes, d.at.0, d.at.1)
                            });
                            let hidden = lanes.len() - 1;
                            for (j, lane) in lanes.iter().enumerate() {
                                let lr = lane_rect(area, j);
                                p.rrect(
                                    lr.0,
                                    lr.1,
                                    lr.2,
                                    lr.3,
                                    10.0,
                                    Color::white(if j == hidden { 0.02 } else { 0.035 }),
                                );
                                p.stroke_rrect(
                                    lr.0 + 0.5,
                                    lr.1 + 0.5,
                                    lr.2 - 1.0,
                                    lr.3 - 1.0,
                                    10.0,
                                    Color::white(0.06),
                                    1.0,
                                );
                                p.text(
                                    b.lanes()[j],
                                    &f.small,
                                    lr.0 + 14.0,
                                    lr.1,
                                    LANE_LABEL - 14.0,
                                    lr.3,
                                    Align::Left,
                                    Color::white(0.5),
                                );
                                let mut slot = 0;
                                for (k, &item) in lane.iter().enumerate() {
                                    if target == Some((j, k)) {
                                        slot += 1;
                                    }
                                    let (tx, ty) = tile_rect(lr, slot);
                                    let pressed = self.tile_drag.is_some_and(|d| d.row == i && d.item == item);
                                    let hovered = self.hovered == Some(Hit::Tile(i, j, k));
                                    let bg = if pressed {
                                        0.16
                                    } else if hovered {
                                        0.12
                                    } else {
                                        0.075
                                    };
                                    let a = if j == hidden { 0.45 } else { 1.0 };
                                    draw_tile(&p, f, item, b.label(item), tx, ty, bg, a);
                                    slot += 1;
                                }
                                if let Some((_, tk)) = target.filter(|&(tj, _)| tj == j) {
                                    let (tx, ty) = tile_rect(lr, tk);
                                    p.rrect(tx, ty, TILE_W, TILE_H, 9.0, theme::accent().alpha(0.14));
                                    p.stroke_rrect(
                                        tx + 0.75,
                                        ty + 0.75,
                                        TILE_W - 1.5,
                                        TILE_H - 1.5,
                                        9.0,
                                        theme::accent(),
                                        1.5,
                                    );
                                } else if lane.is_empty() {
                                    let hint = if j == hidden { "Nothing hidden" } else { "Drag one here" };
                                    let (tx, _) = tile_rect(lr, 0);
                                    p.text(hint, &f.small, tx, lr.1, 200.0, lr.3, Align::Left, Color::white(0.3));
                                }
                            }
                            // The dragged tile follows the pointer, lifted a little.
                            if let Some(d) = drag {
                                let (tx, ty) = (d.at.0 - d.grab.0, d.at.1 - d.grab.1);
                                p.rrect(tx + 2.0, ty + 4.0, TILE_W, TILE_H, 9.0, Color(0.0, 0.0, 0.0, 0.35));
                                p.rrect(tx, ty, TILE_W, TILE_H, 9.0, Color(0.20, 0.20, 0.22, 1.0));
                                draw_tile(&p, f, d.item, b.label(d.item), tx, ty, 0.06, 1.0);
                                p.stroke_rrect(
                                    tx + 0.75,
                                    ty + 0.75,
                                    TILE_W - 1.5,
                                    TILE_H - 1.5,
                                    9.0,
                                    theme::accent(),
                                    1.5,
                                );
                            }
                        }
                        Ctl::Note | Ctl::Heading => {}
                    }
                    let value = match self.drag {
                        Some((d, v)) if d == i => format!("{v}"),
                        _ => get(r.key),
                    };
                    if changed(r, &value).is_some() {
                        let (rx, ry, rs) = self.reset_rect(i, &r.ctl);
                        let (cx, cy) = (rx + rs / 2.0, ry + rs / 2.0);
                        if self.hovered == Some(Hit::Reset(i)) {
                            p.circle(cx, cy, rs / 2.0, Color::white(0.10));
                        }
                        // A circular "undo" arrow.
                        let c = Color::white(0.75);
                        p.arc(cx, cy, 6.0, -0.6, 4.1, c, 1.5);
                        let (ax, ay) = (cx + 6.0 * (-0.6f32).cos(), cy + 6.0 * (-0.6f32).sin());
                        p.polygon(&[(ax - 3.5, ay - 2.0), (ax + 2.5, ay - 3.5), (ax + 1.0, ay + 2.5)], c);
                    }
                }
                p.unclip();
            }
            gfx.end()
        })();
        unsafe {
            let _ = EndPaint(self.hwnd, &ps);
        }
        r
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font.into());
            }
            let _ = DeleteObject(self.brush.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boards_read_lists() {
        let get = |k: &str| match k {
            "taskbar_left" => "Weather, memory,bogus".to_string(),
            "taskbar_right" => "cpu,gpu,gpu,weather".to_string(),
            _ => String::new(),
        };
        let lanes = Board::Taskbar.read(&get);
        assert_eq!(lanes, vec![vec!["weather", "ram"], vec!["cpu", "gpu"], vec!["music", "net", "widgets"]]);
        let lanes = Board::Panel.read(&|_: &str| "stats,calendar,notifications".to_string());
        assert_eq!(lanes, vec![vec!["system", "calendar"], vec!["media", "weather"]]);
    }

    fn keys(section: usize, set: &[(&str, &str)]) -> Vec<&'static str> {
        let get = |k: &str| {
            set.iter().find(|(s, _)| *s == k).map_or_else(|| Config::default().get(k), |(_, v)| v.to_string())
        };
        visible_rows(section, &get).iter().map(|r| r.key).collect()
    }

    #[test]
    fn rows_follow_other_settings() {
        // The lip's own settings only in Lip style, the notch's only in Notch.
        let lip = keys(2, &[("notch_style", "lip"), ("notch_edge", "top")]);
        assert!(lip.contains(&"lip_width") && lip.contains(&"lip_visualizer") && !lip.contains(&"idle_width"));
        let notch = keys(2, &[("notch_style", "notch"), ("notch_edge", "top")]);
        assert!(notch.contains(&"idle_width") && !notch.contains(&"lip_width") && !notch.contains(&"lip_visualizer"));
        // ...except that a notch on a side edge rests as a line too.
        assert!(keys(2, &[("notch_style", "notch"), ("notch_edge", "left")]).contains(&"lip_visualizer"));
        assert!(!keys(2, &[("notch_style", "lip"), ("live_activity", "false")]).contains(&"lip_visualizer"));
        // The notch off: only the switch, and no headings left on their own.
        assert_eq!(keys(2, &[("notch", "false")]), vec!["notch"]);
        assert_eq!(keys(4, &[("launcher", "false")]), vec!["h_open", "launcher", "admin_helper"]);
        // A sidebar that's off takes its buttons, and their headings, with it.
        let start = keys(4, &[("start_show_sidebar", "false")]);
        assert!(!start.iter().any(|k| k.starts_with("start_place_") || *k == "h_side" || *k == "h_places"));
        assert!(keys(8, &[("weather", "true"), ("weather_click", "custom")]).contains(&"weather_url"));
        assert!(!keys(8, &[("weather", "true"), ("weather_click", "google")]).contains(&"weather_url"));
        assert!(!keys(8, &[("weather", "false"), ("weather_click", "custom")]).contains(&"weather_location"));
        assert!(!keys(1, &[("theme_color", "theme")]).contains(&"theme_hue"));
        assert!(keys(1, &[("theme_color", "custom")]).contains(&"theme_hue"));
        // Refraction only for clear glass that bends light, with the blur on.
        assert!(keys(1, &[("theme", "liquid_glass"), ("acrylic", "true")]).contains(&"refraction"));
        assert!(!keys(1, &[("theme", "acrylic"), ("acrylic", "true")]).contains(&"refraction"));
        assert!(!keys(1, &[("theme", "obsidian")]).contains(&"acrylic"));
        // Widget settings follow where the widgets are.
        assert!(!keys(6, &[("taskbar_left", ""), ("taskbar_right", ""), ("widgets_panel", "calendar")])
            .contains(&"widget_visualizer"));
        assert!(keys(6, &[("taskbar_left", "music"), ("taskbar_right", "")]).contains(&"widget_visualizer"));
    }

    #[test]
    fn tiles_drop_between_others() {
        let area = (0.0, 0.0, 600.0, 2.0 * LANE_H + LANE_GAP);
        let lanes = vec![vec!["media", "system"], vec!["weather"]];
        let (x0, _) = tile_rect(lane_rect(area, 0), 0);
        // Left of the first tile's middle: first. Past the second: last.
        assert_eq!(drop_slot(area, &lanes, x0 + 10.0, 10.0), (0, 0));
        assert_eq!(drop_slot(area, &lanes, x0 + 2.0 * (TILE_W + TILE_GAP), 10.0), (0, 2));
        // Lower down, the hidden lane; below everything, still the last lane.
        assert_eq!(drop_slot(area, &lanes, 0.0, LANE_H + 10.0).0, 1);
        assert_eq!(drop_slot(area, &lanes, 0.0, 999.0).0, 1);
    }
}
