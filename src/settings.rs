//! The settings window ("Cascade Settings" in the Start menu). Laid out as
//! Windows 11's Settings: a sidebar with an icon for each page in two groups
//! (Cascade's own, and Windows'), a page title, and settings as cards in
//! groups, in a window that sizes to the screen and can be resized. It moves
//! as Windows' does: pages slide in, the sidebar's marker glides to the page,
//! scrolling eases, toggles slide and hovers fade. Drawn with Direct2D like
//! the rest of Cascade; only the text boxes are native EDIT controls. Every
//! change is written to config.ini and applied at once.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

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

/// The window's size when it opens (less on a small screen), and the least it goes to.
const DEF_W: f32 = 1120.0;
const DEF_H: f32 = 840.0;
pub const MIN_W: f32 = 820.0;
pub const MIN_H: f32 = 560.0;
const SIDEBAR: f32 = 290.0;
/// Widest the cards get (a wide window centres them).
const MAX_CONTENT: f32 = 1040.0;
const ROW_H: f32 = 68.0;
const ROW_GAP: f32 = 4.0;
/// A group heading, and a row with no description (e.g. a sidebar shortcut).
const HEAD_H: f32 = 46.0;
const COMPACT_H: f32 = 52.0;
const TOP: f32 = 104.0;
const BG: Color = Color(0.125, 0.125, 0.13, 1.0);
const CARD: Color = Color(1.0, 1.0, 1.0, 0.048);
/// The sidebar: a page's height.
const NAV_H: f32 = 36.0;
/// The timer that runs the window's animations.
pub const ANIM_TIMER: usize = 77;
/// How long a page takes to slide in.
const PAGE_S: f32 = 0.30;
/// How long a row brought in by another setting takes to grow open.
const GROW_S: f32 = 0.24;

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Each page's icon in Windows' icon font (none: drawn instead).
fn nav_icon(i: usize) -> Option<char> {
    let code = match i {
        0 => 0xE713,  // Settings
        1 => 0xE790,  // Colour
        4 => 0xE80F,  // Home
        5 => 0xE765,  // Keyboard
        6 => 0xE8A9,  // View all
        7 => 0xE7C4,  // Task view
        8 => 0xE708,  // Quiet hours
        9 => 0xE895,  // Sync
        10 => 0xE946, // Info
        11 => 0xE7F4, // Monitor
        12 => 0xE767, // Volume
        13 => 0xE701, // Wi-Fi
        14 => 0xE772, // Devices
        15 => 0xE771, // Personalise
        16 => 0xE71D, // All apps
        17 => 0xE7FC, // Game
        18 => 0xE7E8, // Power
        19 => 0xE774, // Globe
        20 => 0xE776, // Ease of access
        21 => 0xE72E, // Lock
        22 => 0xEA18, // Shield
        23 => 0xE770, // System
        _ => return None,
    };
    char::from_u32(code)
}
/// Narrowest a segmented button gets (they widen to fit their labels).
const CHOICE_W: f32 = 84.0;
/// Each extra line of a wrapped description.
const LINE_H: f32 = 15.0;
/// Most lines a description wraps to.
const MAX_LINES: usize = 3;

pub const SECTIONS: [&str; 24] = [
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
    // Windows' own settings (see `winset`), after a rule in the sidebar.
    "Display",
    "Sound",
    "Network & internet",
    "Bluetooth & devices",
    "Personalisation",
    "Apps",
    "Gaming",
    "Power & battery",
    "Time & language",
    "Accessibility",
    "Privacy & security",
    "Debloat",
    "System",
];

/// The pages for Windows' own settings, by number (see `winset::page_for`).
pub mod page {
    pub const DISPLAY: usize = 11;
    pub const SOUND: usize = 12;
    pub const NETWORK: usize = 13;
    pub const DEVICES: usize = 14;
    pub const LOOK: usize = 15;
    pub const APPS: usize = 16;
    pub const GAMING: usize = 17;
    pub const POWER: usize = 18;
    pub const TIME: usize = 19;
    pub const ACCESS: usize = 20;
    pub const PRIVACY: usize = 21;
    pub const DEBLOAT: usize = 22;
    pub const SYSTEM: usize = 23;
}

/// The sidebar's pages, in order: as Windows 11's Settings has them, with
/// Cascade's own parts (the notch, taskbar, Start menu...) after
/// Personalisation. Appearance, Screens and About have no entry of their
/// own: they're parts of Personalisation, Display and System (see `merged`).
const NAV: [usize; 21] = [
    0,
    page::LOOK,
    2,
    3,
    4,
    6,
    5,
    8,
    page::DISPLAY,
    page::SOUND,
    page::NETWORK,
    page::DEVICES,
    page::APPS,
    page::GAMING,
    page::POWER,
    page::TIME,
    page::ACCESS,
    page::PRIVACY,
    page::DEBLOAT,
    9,
    page::SYSTEM,
];

/// The page an old page number means now (Appearance, Screens and About
/// were merged into others).
fn merged(section: usize) -> usize {
    match section {
        1 => page::LOOK,
        7 => page::DISPLAY,
        10 => page::SYSTEM,
        s => s,
    }
}

/// Where page `section` is in the sidebar.
fn nav_pos(section: usize) -> Option<usize> {
    NAV.iter().position(|&s| s == merged(section))
}

/// Where sidebar entry `pos` starts (before scrolling).
fn nav_y(pos: usize) -> f32 {
    NAV_TOP + 6.0 + pos as f32 * NAV_H
}

/// The sidebar list's whole height.
fn nav_total() -> f32 {
    nav_y(NAV.len()) - NAV_TOP + 16.0
}

/// The blurred wallpaper behind the window (see `Settings::backdrop`): made
/// on a thread, for the screen it's for.
type Mica = (RECT, u32, u32, Vec<u8>);
static MICA_DONE: std::sync::Mutex<Option<Mica>> = std::sync::Mutex::new(None);
static MICA_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The wallpaper of `screen`, a sixteenth of its size and blurred: drawn
/// back at full size, it's as soft as Windows' Mica.
fn mica_picture(screen: RECT) -> Option<Mica> {
    let (w, h) = (((screen.right - screen.left) / 16).max(8), ((screen.bottom - screen.top) / 16).max(8));
    let mut px = crate::wallpaper::thumbnail(w, h)?;
    for _ in 0..3 {
        blur(&mut px, w as usize, h as usize, 3);
    }
    // Mostly grey, as Mica is: only a hint of the wallpaper's colour.
    for p in px.as_chunks_mut::<4>().0 {
        let l = (p[0] as u32 * 29 + p[1] as u32 * 150 + p[2] as u32 * 77) / 256;
        for c in p.iter_mut().take(3) {
            *c = ((l * 3 + *c as u32) / 4) as u8;
        }
    }
    Some((screen, w as u32, h as u32, px))
}

/// Mica's tint over the wallpaper: Windows' dark grey, the picture only just there.
const MICA_TINT: Color = Color(0.118, 0.118, 0.125, 0.93);

/// The settings window's backdrop, for a window inside it (the Lighting
/// pane), so it sits on the same Mica: `w`×`h` layout units at scale `k`.
pub(crate) fn paint_backdrop(p: &Painter, hwnd: HWND, k: f32, w: f32, h: f32) {
    let Some((screen, bw, bh, px)) = MICA_DONE.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
    let Some(bmp) = p.bitmap(bw, bh, &px) else { return };
    let mut origin = POINT::default();
    unsafe {
        let _ = ClientToScreen(hwnd, &mut origin);
    }
    p.image(
        &bmp,
        (screen.left - origin.x) as f32 / k,
        (screen.top - origin.y) as f32 / k,
        (screen.right - screen.left) as f32 / k,
        (screen.bottom - screen.top) as f32 / k,
        1.0,
    );
    p.rrect(0.0, 0.0, w, h, 0.0, MICA_TINT);
}

/// A box blur of `radius` over BGRA pixels, across then down.
fn blur(px: &mut [u8], w: usize, h: usize, radius: usize) {
    let mut line = Vec::new();
    for (len, step, count, stride) in [(w, 4, h, w * 4), (h, w * 4, w, 4)] {
        for n in 0..count {
            let base = n * stride;
            line.clear();
            line.extend((0..len).map(|i| {
                let o = base + i * step;
                [px[o] as u32, px[o + 1] as u32, px[o + 2] as u32, px[o + 3] as u32]
            }));
            for i in 0..len {
                let (a, b) = (i.saturating_sub(radius), (i + radius).min(len - 1));
                let mut sum = [0u32; 4];
                for v in &line[a..=b] {
                    for c in 0..4 {
                        sum[c] += v[c];
                    }
                }
                let o = base + i * step;
                for c in 0..4 {
                    px[o + c] = (sum[c] / (b - a + 1) as u32) as u8;
                }
            }
        }
    }
}

/// The page that lists what the search box found: one past the real sections,
/// so it has no sidebar entry. Its rows are the matching rows of the others.
const SEARCH: usize = SECTIONS.len();
/// Lighting (RGB), a page under Personalisation: its content is the Lighting
/// pane (see `rgb::app`), a child window placed over the page.
pub const LIGHTING: usize = SECTIONS.len() + 1;
/// The search box's id (for its change notifications) and place in the sidebar.
pub const SEARCH_ID: usize = 900;
const SEARCH_BOX: (f32, f32, f32, f32) = (14.0, 18.0, SIDEBAR - 28.0, 32.0);
/// Where the sidebar's section list starts, under the search box.
const NAV_TOP: f32 = 64.0;
const BOX_BG: Color = Color(0.169, 0.169, 0.169, 1.0);

/// What the search box found: the settings that match what was typed, each
/// group under a heading naming its section, and which section each came
/// from (for `shown`, which goes by the section a row really belongs to).
struct Found {
    rows: Vec<Row>,
    from: Vec<(&'static str, usize)>,
}

thread_local! {
    static FOUND: RefCell<Found> = const { RefCell::new(Found { rows: Vec::new(), from: Vec::new() }) };
}

/// Finds the settings that match every word of `query` in their name,
/// description, key or section.
fn search(query: &str) {
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
    let (mut out, mut from) = (Vec::new(), Vec::new());
    if !words.is_empty() {
        for (s, name) in SECTIONS.iter().enumerate() {
            let hits: Vec<Row> = rows(s)
                .into_iter()
                .filter(|r| !matches!(r.ctl, Ctl::Heading | Ctl::Note))
                .filter(|r| {
                    let text = format!("{} {} {} {}", r.title, r.desc, r.key.replace('_', " "), name).to_lowercase();
                    words.iter().all(|w| text.contains(w))
                })
                .collect();
            if hits.is_empty() {
                continue;
            }
            out.push(heading(name, name));
            for r in hits {
                from.push((r.key, s));
                out.push(r);
            }
        }
        if out.is_empty() {
            out.push(row(
                "search-none",
                "Nothing matches that",
                "Try another word: a setting's name, or what it does.",
                Ctl::Note,
            ));
        }
    }
    FOUND.with(|f| *f.borrow_mut() = Found { rows: out, from });
}

/// Settings whose name holds every word of `query`, for the Start menu's
/// search: (the setting's name, its page's name, its page), the best few.
pub fn find(query: &str) -> Vec<(String, &'static str, usize)> {
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
    if words.is_empty() || query.trim().len() < 3 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (s, name) in SECTIONS.iter().enumerate() {
        for r in rows(s) {
            if matches!(r.ctl, Ctl::Heading | Ctl::Note) {
                continue;
            }
            let text = format!("{} {}", r.title, name).to_lowercase();
            if words.iter().all(|w| text.contains(w.as_str())) {
                out.push((r.title.to_string(), *name, s));
            }
        }
    }
    // Page names that match come first ("display" finds the Display page).
    out.sort_by_key(|(t, page, _)| {
        (!page.to_lowercase().starts_with(&words[0]), !t.to_lowercase().starts_with(&words[0]))
    });
    out.truncate(3);
    out
}

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
    Db,
}

#[derive(Clone)]
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
    /// A value to pick from a list (Windows' settings: a resolution, a
    /// device...): shows the value, and clicking it shows the list.
    Pick,
    /// The themes, as a grid of samples to click.
    Gallery,
    /// An accent colour as dots to click: Cascade's (true: Windows' own),
    /// the first dot following the other one, then Windows' palette
    /// (`theme::PALETTE`), then + for any colour.
    Accents(bool),
    /// Tiles to drag between lanes (which widgets show, where and in what order).
    Board(Board),
    /// The screens as they sit on the desk, to drag into place and click to choose.
    Arrange,
    /// The pen tablet's pressure curve, with points to drag (tip, softness,
    /// full press) and the pen's pressure now on it.
    PenCurve,
    /// A pad to try the pen on.
    PenPad,
    Note,
    /// A group title with no control.
    Heading,
}

#[derive(Clone)]
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
        Ctl::Accents(_) => BOARD_HEAD + accent_rows() as f32 * (DOT + DOT_GAP) - DOT_GAP + 16.0,
        Ctl::Board(b) => BOARD_HEAD + b.lanes().len() as f32 * (LANE_H + LANE_GAP) - LANE_GAP + 16.0,
        Ctl::Arrange => BOARD_HEAD + ARRANGE_H + 16.0,
        Ctl::PenCurve => BOARD_HEAD + CURVE_H + 16.0,
        Ctl::PenPad => BOARD_HEAD + PAD_H + 16.0,
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
/// The pressure graph's height (and width), and the test pad's height.
const CURVE_H: f32 = 190.0;
const PAD_H: f32 = 220.0;
/// The screen arrangement's height, and the room around the screens in it.
const ARRANGE_H: f32 = 230.0;
const ARRANGE_PAD: f32 = 20.0;

/// The screens of an arrangement filling `area` (logical units): how many
/// desktop pixels a unit is, and each screen's box.
fn screen_boxes(area: (f32, f32, f32, f32), rects: &[RECT]) -> (f32, Vec<(f32, f32, f32, f32)>) {
    let (l, t) = rects.iter().fold((i32::MAX, i32::MAX), |(l, t), r| (l.min(r.left), t.min(r.top)));
    let (rr, b) = rects.iter().fold((i32::MIN, i32::MIN), |(rr, b), r| (rr.max(r.right), b.max(r.bottom)));
    if rects.is_empty() {
        return (1.0, Vec::new());
    }
    let (bw, bh) = ((rr - l).max(1) as f32, (b - t).max(1) as f32);
    let k = ((area.2 - 2.0 * ARRANGE_PAD) / bw).min((area.3 - 2.0 * ARRANGE_PAD) / bh).min(0.1);
    let (ox, oy) = (area.0 + (area.2 - bw * k) / 2.0, area.1 + (area.3 - bh * k) / 2.0);
    let boxes = rects
        .iter()
        .map(|r| {
            let (x, y) = (ox + (r.left - l) as f32 * k, oy + (r.top - t) as f32 * k);
            (x + 2.0, y + 2.0, (r.right - r.left) as f32 * k - 4.0, (r.bottom - r.top) as f32 * k - 4.0)
        })
        .collect();
    (1.0 / k, boxes)
}

/// A screen being dragged in the arrangement (layout units).
#[derive(Clone, Copy)]
struct ScreenDrag {
    row: usize,
    screen: usize,
    start: (f32, f32),
    at: (f32, f32),
    moved: bool,
}
const LANE_H: f32 = 52.0;
const LANE_GAP: f32 = 6.0;
/// The lane's name, left of its tiles.
const LANE_LABEL: f32 = 70.0;
const TILE_W: f32 = 66.0;
const TILE_H: f32 = 42.0;
const TILE_GAP: f32 = 6.0;

/// The theme gallery: columns of samples, each a preview over its name.
const SWATCH_COLS: usize = 5;
const SWATCH_PREVIEW: f32 = 72.0;
const SWATCH_H: f32 = SWATCH_PREVIEW + 24.0;
const SWATCH_GAP: f32 = 10.0;

/// The accent dots: the Windows accent first, then the palette.
const DOT: f32 = 30.0;
const DOT_GAP: f32 = 10.0;
const DOT_COLS: usize = 13;

fn accent_rows() -> usize {
    (CUSTOM_DOT + 1).div_ceil(DOT_COLS)
}

/// The last dot: any colour, from a colour picker.
const CUSTOM_DOT: usize = theme::PALETTE.len() + 1;

/// Dot `k`'s centre in an accent grid filling `area`.
fn dot_at(area: (f32, f32, f32, f32), k: usize) -> (f32, f32) {
    let step = ((area.2 - DOT) / (DOT_COLS - 1) as f32).min(DOT + 18.0);
    let (col, line) = (k % DOT_COLS, k / DOT_COLS);
    (area.0 + DOT / 2.0 + col as f32 * step, area.1 + DOT / 2.0 + line as f32 * (DOT + DOT_GAP))
}

/// What dot `k` sets `accent` to.
fn accent_value(k: usize) -> Option<String> {
    match k {
        0 => Some("windows".into()),
        k => theme::PALETTE.get(k - 1).map(|(c, _)| format!("#{c:06X}")),
    }
}

/// The colour picked with the picker, if `accent` is one not in the palette.
fn custom_accent(value: &str) -> Option<u32> {
    theme::parse_accent(value).filter(|c| !theme::PALETTE.iter().any(|(p, _)| p == c))
}

/// Cascade's accent colour now (0xRRGGBB).
fn cascade_accent_rgb() -> Option<u32> {
    let c = theme::accent();
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    Some((ch(c.0) << 16) | (ch(c.1) << 8) | ch(c.2))
}

/// Which dot Windows' accent `value` (#RRGGBB) is: the first when it's
/// Cascade's, a palette colour's, or the last (+) for any other.
fn win_accent_index(value: &str) -> Option<usize> {
    let c = theme::parse_accent(value)?;
    if Some(c) == cascade_accent_rgb() {
        return Some(0);
    }
    Some(theme::PALETTE.iter().position(|(p, _)| *p == c).map_or(CUSTOM_DOT, |i| i + 1))
}

/// Which dot `accent` is.
fn accent_index(value: &str) -> Option<usize> {
    match theme::parse_accent(value) {
        None => Some(0),
        Some(c) => Some(theme::PALETTE.iter().position(|(p, _)| *p == c).map_or(CUSTOM_DOT, |i| i + 1)),
    }
}

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
                ("desktops", "Desktops"),
                ("lang", "Language"),
                ("widgets", "Widgets"),
            ],
            Board::Panel => &[
                ("media", "Media"),
                ("system", "System"),
                ("weather", "Weather"),
                ("calendar", "Calendar"),
                ("clipboard", "Clipboard"),
                ("phone", "Phone"),
            ],
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
        "lang" => {
            // A key with a letter on it.
            p.stroke_rrect(cx - 7.0, cy - 6.0, 14.0, 12.0, 2.0, c, 1.2);
            p.line(cx - 3.0, cy + 3.0, cx, cy - 3.0, c, 1.2);
            p.line(cx, cy - 3.0, cx + 3.0, cy + 3.0, c, 1.2);
            p.line(cx - 1.6, cy + 0.6, cx + 1.6, cy + 0.6, c, 1.0);
        }
        "desktops" => {
            // Two desktops, one behind the other.
            p.stroke_rrect(cx - 7.0, cy - 6.0, 11.0, 8.0, 1.5, c, 1.2);
            p.rrect(cx - 3.0, cy - 2.0, 11.0, 8.0, 1.5, c);
        }
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
        "phone" => {
            p.stroke_rrect(cx - 4.5, cy - 7.0, 9.0, 14.0, 2.2, c, 1.3);
            p.line(cx - 1.5, cy + 4.5, cx + 1.5, cy + 4.5, c, 1.3);
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
    if section == SEARCH {
        return FOUND.with(|f| f.borrow().rows.clone());
    }
    match section {
        0 => vec![
            row("autostart", "Start with Windows", "Launch Cascade when you sign in.", Toggle),
            row(
                "shell_mode",
                "Run as the Windows shell",
                "Cascade in Explorer's place, so the Windows taskbar and Start menu never exist. Turning it on offers to switch now, without signing out.",
                Toggle,
            ),
            row("hide_in_fullscreen", "Hide in fullscreen", "Get out of the way of games and videos.", Toggle),
            row(
                "notch_fullscreen",
                "Notch over fullscreen apps",
                "Keep the notch on top of fullscreen apps and games (the taskbar still hides).",
                Toggle,
            ),
            row(
                "super_drag",
                "Super drag",
                "Hold the Win key and drag a window by any part of it, not only its title bar.",
                Toggle,
            ),
            row(
                "snap_drag",
                "Snap by dragging",
                "Drag a window to a screen's edge to snap it to that half, a corner for a quarter, the top to maximise.",
                Toggle,
            ),
            row("alt_tab", "Alt+Tab switcher", "Cascade's own, with live previews, instead of Windows'.", Toggle),
            row(
                "alt_tab_style",
                "Alt+Tab look",
                "Cards with live previews, larger previews, big app icons, or a list of titles.",
                Choice(&[("cards", "Cards"), ("large", "Large"), ("icons", "Icons"), ("list", "List")]),
            ),
            row(
                "priority",
                "Priority",
                "How much of the PC Cascade gets, so the taskbar, Start and the notch never lag behind a game or a busy app.",
                Choice(&[("high", "High"), ("above_normal", "Above normal"), ("normal", "Normal")]),
            ),
            row(
                "max_fps",
                "Animation frame cap",
                "Also limited to your monitor's refresh rate.",
                slider(30.0, 240.0, 10.0, Unit::Fps),
            ),
        ],
        1 => vec![],
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
            row("clock_seconds", "Seconds on the clock", "Counts the seconds on the notch's clock.", Toggle),
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
            row("charge_osd", "Charger pop-up", "Show plugging the charger in or out, and the battery level.", Toggle),
            row(
                "hide_windows_flyout",
                "Hide the Windows flyout",
                "Hide the built-in volume / brightness flyout.",
                Toggle,
            ),
            row("scroll_volume", "Scroll to change volume", "Scroll over the closed notch.", Toggle),
            row(
                "controls_mixer",
                "Volume mixer",
                "Each app playing sound on Controls, with its own volume, and the output device to pick.",
                Toggle,
            ),
            row(
                "usb_popups",
                "USB devices",
                "Say when one is plugged in: flight controllers, radios, headsets, phones and your list, or any device.",
                Choice(&[("known", "Known"), ("all", "Any"), ("off", "Off")]),
            ),
            row("usb_unplug", "Unplugging too", "Also say when one is unplugged.", Toggle),
            row(
                "edit_devices",
                "Your devices",
                "Names, and what clicking the pop-up opens (devices.txt).",
                Button("Edit list"),
            ),
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
                "taskbar_peek",
                "Peek at windows",
                "Rest the pointer on a window's preview to see where it is: the other windows turn to glass.",
                Toggle,
            ),
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
            row("battery_percent", "Battery percentage", "The charge level as a number next to the battery icon.", Toggle),
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
            heading("h_screens", "Other screens"),
            row(
                "taskbar_screens",
                "Taskbar on",
                "With more than one screen: a taskbar on each, or on the main one only.",
                Choice(&[("all", "Every screen"), ("main", "Main screen")]),
            ),
            row(
                "taskbar_screen_apps",
                "Apps on each taskbar",
                "Every open window, or only the ones on that screen. Pinned apps show on all of them.",
                Choice(&[("all", "All windows"), ("screen", "That screen's")]),
            ),
            row("taskbar_screen_clock", "Clock on other screens", "The widgets and tray stay on the main screen.", Toggle),
        ],
        4 => vec![
            heading("h_open", "Opening"),
            row("launcher", "Cascade Start menu", "Opened by the Start button on the taskbar.", Toggle),
            row("launcher_win_key", "Open with the Win key", "Win+E, Win+R and other shortcuts keep working.", Toggle),
            row(
                "file_index",
                "Instant file search",
                "Finds any file or folder on your drives as you type, from Windows' own file tables, like Everything. Asks for permission once; about 45 MB of memory per million files.",
                Toggle,
            ),
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
            row(
                "start_web_search",
                "Web search",
                "Offered under your search. Sums, unit conversions and commands are answered either way.",
                Choice(&[("google", "Google"), ("bing", "Bing"), ("duckduckgo", "DuckDuckGo"), ("none", "None")]),
            ),
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
            row(
                "profile_picture_choose",
                "Profile picture",
                "On the account button. Your Windows account's picture until you choose one.",
                Button("Choose\u{2026}"),
            ),
            row("profile_picture_clear", "Your own picture", "", Button("Use Windows'")),
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
                "shortcut_win_v",
                "Win+V",
                "Your clipboard history (Windows' own, pins and all) by the text cursor. Click an item or press Enter to paste it.",
                Toggle,
            ),
            row(
                "shortcut_win_tab",
                "Win+Tab",
                "Task View: this desktop's windows to pick from, and your virtual desktops to switch to or add.",
                Toggle,
            ),
            row(
                "desktop_osd",
                "Desktop switch pop-up",
                "Your desktops in the middle of the screen for a moment as you switch between them.",
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
                "desktops_style",
                "Desktops widget",
                "A dot per desktop, or a numbered box for each.",
                Choice(&[("dots", "Dots"), ("numbers", "Numbers")]),
            ),
            heading("h_gpu", "Graphics cards (NVIDIA)"),
            row(
                "gpu_cards",
                "Cards",
                "Every card, or just one of them, on the taskbar and in the panel.",
                Choice(&[("all", "All"), ("1", "GPU 1"), ("2", "GPU 2")]),
            ),
            row("gpu_temp", "Temperature", "Each card's temperature on the taskbar widget.", Toggle),
            row(
                "gpu_value",
                "Taskbar figure",
                "What the GPU widget shows for each card.",
                Choice(&[("load", "Load"), ("memory", "Memory"), ("power", "Power")]),
            ),
            row(
                "gpu_label",
                "Label",
                "Each card by number (GPU 1) or by name (RTX 5070 Ti).",
                Choice(&[("number", "Number"), ("name", "Name")]),
            ),
            row(
                "stats_seconds",
                "Stats refresh",
                "Seconds between readings (only while one is on screen).",
                slider(1.0, 10.0, 1.0, Unit::Seconds),
            ),
        ],
        7 => vec![],
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
            heading("h_calendar", "Calendar"),
            row(
                "calendar_url",
                "Calendar link",
                "Your calendar's private iCal address (Google: its settings > Secret address in iCal format).",
                Text { secret: true },
            ),
            row(
                "calendar_soon",
                "Countdown",
                "How long before an event the closed notch starts counting down to it.",
                slider(0.0, 30.0, 1.0, Unit::Min),
            ),
        ],
        9 => vec![
            heading("h_cascade_update", "Cascade"),
            row("update_check_now", "Updates", "Look for a newer build of Cascade.", Button("Check for updates")),
            row("update_now", "Update Cascade", "Download and install the newest build.", Button("Update now")),
            row("update_branch", "Update from", "Stable releases, or the newest build of a branch.", Pick),
            row("update_check", "Check automatically", "Look for a new build every few hours.", Toggle),
            row("changelog", "What's new", "What changed in each build.", Button("Open changelog")),
            row("github_token", "GitHub token", "Only needed while the repo is private.", Text { secret: true }),
            heading("h_windows_update", "Windows Update"),
            row("win_update", "Windows Update", "Look for updates now.", Button("Check")),
        ],
        page::DISPLAY => vec![
            heading("h_screens", "Screens"),
            row(
                "win_arrange",
                "Arrange your screens",
                "Drag a screen to where it sits on your desk; click one to choose it for the settings below.",
                Ctl::Arrange,
            ),
            row("win_identify", "Identify", "Shows each screen's number on it, for a few seconds.", Button("Identify")),
            row("win_screen", "Screen", "Which screen the settings below are for.", Pick),
            row("win_layout", "Multiple screens", "Extend, duplicate, or show on one screen only (Win+P).", Pick),
            row("win_primary", "Main screen", "Make this the main screen.", Button("Make main")),
            heading("h_scale", "Scale & layout"),
            row("win_scale", "Scale", "Makes text, apps and everything else bigger.", Pick),
            row("win_resolution", "Display resolution", "Windows asks whether to keep a new one.", Pick),
            row("win_refresh", "Refresh rate", "Higher is smoother, for games and scrolling.", Pick),
            row("win_orientation", "Display orientation", "Turn the picture for a screen on its side.", Pick),
            row("win_hdr", "Use HDR", "Brighter highlights and richer colour.", Toggle),
            row("win_display_info", "Advanced display", "", Note),
            heading("h_night", "Night light"),
            row("win_night", "Night light", "Warmer colours, easier on the eyes at night.", Toggle),
            row("win_night_strength", "Strength", "How warm the colours get.", slider(0.1, 1.0, 0.05, Unit::Percent)),
            row("win_night_schedule", "Schedule", "When it comes on.", Pick),
            row("win_gpu_note", "Graphics", "Which GPU each app uses, Auto HDR and variable refresh rate are on the Gaming page.", Note),
            heading("h_crossing", "Moving between screens"),
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
        page::SOUND => vec![
            heading("h_output", "Output"),
            row("win_output", "Play sound through", "The default output, for every app.", Pick),
            row("win_volume", "Volume", "", slider(0.0, 1.0, 0.01, Unit::Percent)),
            heading("h_out_dev", "Output device properties"),
            row("win_snd_out_dev", "Device", "Which output the settings below are for.", Pick),
            row("win_snd_out_vol", "Volume", "This device's own level.", slider(0.0, 1.0, 0.01, Unit::Percent)),
            row("win_snd_out_bal", "Balance", "Left (below 0) or right (above 0).", slider(-1.0, 1.0, 0.05, Unit::Percent)),
            row("win_snd_out_mute", "Mute", "", Toggle),
            row("win_snd_out_format", "Format", "The sample rate and bit depth it plays at.", Pick),
            row("win_snd_out_default", "Default device", "Make this the output for everything.", Button("Set as default")),
            row("win_snd_out_comm", "Default for calls", "Discord, Teams and other calls use this one.", Button("Use for calls")),
            row("win_snd_out_disable", "Turn off", "Hide this device from Windows and every app.", Button("Turn off")),
            heading("h_input", "Input"),
            row("win_input", "Microphone", "The default input apps listen to.", Pick),
            row("win_mic", "Input level", "", slider(0.0, 1.0, 0.01, Unit::Percent)),
            heading("h_in_dev", "Input device properties"),
            row("win_snd_in_dev", "Device", "Which input the settings below are for.", Pick),
            row("win_snd_in_vol", "Input volume", "This device's own level.", slider(0.0, 1.0, 0.01, Unit::Percent)),
            row("win_snd_in_mute", "Mute", "", Toggle),
            row("win_snd_in_format", "Format", "The sample rate and bit depth it records at.", Pick),
            row("win_snd_in_default", "Default device", "Make this the microphone for everything.", Button("Set as default")),
            row("win_snd_in_comm", "Default for calls", "Calls use this microphone.", Button("Use for calls")),
            row("win_snd_in_disable", "Turn off", "Hide this device from Windows and every app.", Button("Turn off")),
            heading("h_sound_more", "More"),
            row("win_snd_disabled", "Turned-off devices", "", Button("Turn on\u{2026}")),
            row("win_snd_mono", "Mono audio", "Both ears hear everything (one-sided headphones, hearing in one ear).", Toggle),
            row("win_snd_ducking", "During calls", "What other sounds do while Windows hears a call.", Pick),
            row("win_mixer", "Volume mixer", "Each app's volume (Windows' classic mixer).", Button("Open")),
            row(
                "win_sound_cpl",
                "More sound settings",
                "Enhancements, spatial sound and system sounds.",
                Button("Open"),
            ),
            heading("h_ducker", "Auto ducker"),
            row(
                "ducker",
                "Auto ducker",
                "While one app is making sound (Discord, with friends talking), turn the others down. They come back when it stops.",
                Toggle,
            ),
            row("ducker_trigger", "When this app makes sound", "Pick from the apps with sound (several if you like).", Pick),
            row("ducker_targets", "Turn these apps down", "Pick the apps, or every other app.", Pick),
            row(
                "ducker_threshold",
                "Counts as sound above",
                "Lower hears quiet voices too; higher only loud sound.",
                slider(-60.0, -10.0, 1.0, Unit::Db),
            ),
            row(
                "ducker_level",
                "Turn them down to",
                "A share of their own volume.",
                slider(0.0, 1.0, 0.05, Unit::Percent),
            ),
            row(
                "ducker_release",
                "Bring them back after",
                "How long it has to be quiet first.",
                slider(200.0, 5000.0, 100.0, Unit::Ms),
            ),
        ],
        page::NETWORK => vec![
            heading("h_net", "Network"),
            row("win_net_info", "Connection", "What you're connected through.", Note),
            row("win_wifi", "Wi-Fi", "", Toggle),
            row("win_wifi_info", "Wi-Fi properties", "", Note),
            row("win_metered", "Metered connection", "Windows and apps hold back big downloads on this Wi-Fi network.", Toggle),
            row("win_airplane", "Airplane mode", "Turns every radio off: Wi-Fi, Bluetooth and mobile data.", Toggle),
            heading("h_join", "Join a network"),
            row("win_join_ssid", "Network", "One in range (with a lock if it needs a password).", Pick),
            row("win_join_pw", "Password", "For a network that needs one.", Text { secret: true }),
            row("win_join", "Join", "Joins it, and remembers it for next time, as Windows does.", Button("Join")),
            row("win_known", "Known networks", "Networks Windows remembers.", Button("Manage\u{2026}")),
            heading("h_adapter", "Adapter settings"),
            row("win_adapter", "Adapter", "Which network adapter the settings below are for.", Pick),
            row("win_adapter_info", "Properties", "", Note),
            row("win_ip_mode", "IP assignment", "Automatic from the router (DHCP), or an address you set.", Pick),
            row("win_ip_addr", "IP address", "Like 192.168.1.20.", Text { secret: false }),
            row("win_ip_mask", "Subnet mask", "Like 255.255.255.0 (or 24).", Text { secret: false }),
            row("win_ip_gateway", "Gateway", "Your router's address, like 192.168.1.1.", Text { secret: false }),
            row("win_dns_mode", "DNS server assignment", "Automatic from the router, or servers you choose.", Pick),
            row("win_dns_preset", "Use a provider", "Fills in a well-known DNS service's addresses.", Pick),
            row("win_dns1", "Preferred DNS", "Like 1.1.1.1.", Text { secret: false }),
            row("win_dns2", "Alternate DNS", "Used when the preferred one doesn't answer.", Text { secret: false }),
            row("win_doh", "DNS over HTTPS", "Encrypts lookups so the network can't read or change them.", Pick),
            row("win_ip_save", "Save", "Applies the IP and DNS settings above (Windows asks for permission).", Button("Save")),
            heading("h_hotspot", "Mobile hotspot"),
            row("win_hotspot", "Mobile hotspot", "Share this PC's internet over Wi-Fi.", Toggle),
            row("win_hotspot_from", "Share my internet connection from", "The connection devices on the hotspot use.", Pick),
            row("win_hotspot_name", "Network name", "What devices see when they look for Wi-Fi (1 to 32 characters).", Text { secret: false }),
            row("win_hotspot_pw", "Network password", "8 to 63 characters.", Text { secret: false }),
            row("win_hotspot_band", "Network band", "5 GHz is faster; 2.4 GHz reaches further and suits older devices.", Pick),
            row("win_hotspot_security", "Security", "WPA3 is stronger; some older devices only know WPA2.", Pick),
            row("win_hotspot_saver", "Power saving", "Turns the hotspot off when no device has been connected for a while.", Toggle),
            row("win_hotspot_devices", "Connected devices", "", Note),
            heading("h_proxy", "Proxy"),
            row("win_proxy_auto", "Automatically detect settings", "Finds the network's proxy by itself.", Toggle),
            row("win_proxy_script_on", "Use setup script", "A proxy auto-config (PAC) address.", Toggle),
            row("win_proxy_script", "Script address", "", Text { secret: false }),
            row("win_proxy_manual", "Use a proxy server", "Every app using Windows' settings goes through it.", Toggle),
            row("win_proxy_server", "Proxy address", "host:port, like proxy.example.com:8080.", Text { secret: false }),
            row("win_proxy_bypass", "Don't use the proxy for", "Addresses separated by semicolons; <local> for local names.", Text { secret: false }),
            heading("h_net_more", "More"),
            row("win_vpn", "VPN", "Connect to a VPN set up on this PC.", Button("Open")),
            row("win_ncpa", "Network adapters", "Windows' classic list: sharing, bridges, each adapter's properties.", Button("Open")),
            row("win_net_reset", "Network reset", "Sets every adapter back to Windows' defaults (needs a restart).", Button("Reset now")),
        ],
        page::DEVICES => vec![
            heading("h_quickshare", "Quick Share"),
            row(
                "quickshare",
                "Quick Share",
                "Send and receive files with Android phones and PCs nearby, over your Wi-Fi.",
                Toggle,
            ),
            row("quickshare_send", "Send files", "To a phone or PC nearby (or right-click files > Share with Quick Share).", Button("Send\u{2026}")),
            row(
                "quickshare_accept",
                "When someone shares",
                "Ask shows who it is and the PIN first; automatically takes it at once.",
                Choice(&[("ask", "Ask"), ("auto", "Automatically")]),
            ),
            row("quickshare_name", "Name", "What phones see this PC as. Empty: this PC's own name.", Text { secret: false }),
            row("quickshare_folder", "Save to", "Where files you receive go. Empty: Downloads.", Text { secret: false }),
            row(
                "quickshare_menus",
                "In right-click menus",
                "Share with Quick Share on files, and in Send to.",
                Toggle,
            ),
            heading("h_tablet", "Pen tablet"),
            row(
                "tablet",
                "Built-in tablet driver",
                "10moon, Vinsa and their relatives, with pressure, without their own driver.",
                Toggle,
            ),
            row(
                "tablet_mode",
                "The pen works as",
                "A pen (Windows Ink: pressure in drawing apps), or a mouse for apps that only take one.",
                Choice(&[("ink", "Pen"), ("mouse", "Mouse")]),
            ),
            row(
                "tablet_screen",
                "Maps to",
                "The whole tablet over every screen, or over the main one.",
                Choice(&[("all", "Every screen"), ("main", "Main screen")]),
            ),
            row(
                "tablet_keep_shape",
                "Keep proportions",
                "Uses the part of the tablet shaped like the screen, so circles stay round.",
                Toggle,
            ),
            row("tablet_left", "Left-handed", "Turns the tablet round, its buttons on the right.", Toggle),
            row(
                "tablet_curve",
                "Pressure",
                "Drag the points: where the pen starts drawing, how soft or firm it feels, and how hard is a full press. The dot is your pen now.",
                PenCurve,
            ),
            row("tablet_test", "Try it", "Draw here with the pen: the line follows your pressure.", PenPad),
            row(
                "tablet_lower",
                "Lower pen button",
                "",
                Choice(&[("right", "Right-click"), ("middle", "Middle-click"), ("eraser", "Eraser"), ("keys", "Shortcut"), ("none", "Nothing")]),
            ),
            row("tablet_lower_keys", "Its shortcut", "Keys it presses, held while the button is: ctrl+z, shift, f5\u{2026}", Text { secret: false }),
            row(
                "tablet_upper",
                "Upper pen button",
                "",
                Choice(&[("right", "Right-click"), ("middle", "Middle-click"), ("eraser", "Eraser"), ("keys", "Shortcut"), ("none", "Nothing")]),
            ),
            row("tablet_upper_keys", "Its shortcut", "Keys it presses, held while the button is: ctrl+z, shift, f5\u{2026}", Text { secret: false }),
            row(
                "tablet_hold_click",
                "Press and hold to right-click",
                "Windows' pen gesture: holding the pen still on the tablet right-clicks. Off, holding it just keeps drawing.",
                Toggle,
            ),
            heading("h_tablet_keys", "Tablet buttons"),
            row(
                "tablet_keys_note",
                "The buttons on the tablet",
                "Each a shortcut (ctrl+z, space, shift, f5\u{2026}, held while the button is), or right, middle, eraser or none. Press one to see which it is.",
                Note,
            ),
            row("tablet_key1", "Button 1", "", Text { secret: false }),
            row("tablet_key2", "Button 2", "", Text { secret: false }),
            row("tablet_key3", "Button 3", "", Text { secret: false }),
            row("tablet_key4", "Button 4", "", Text { secret: false }),
            row("tablet_key5", "Button 5", "", Text { secret: false }),
            row("tablet_key6", "Button 6", "", Text { secret: false }),
            row("tablet_key7", "Button 7", "", Text { secret: false }),
            row("tablet_key8", "Button 8", "", Text { secret: false }),
            row("tablet_key9", "Button 9", "", Text { secret: false }),
            row("tablet_key10", "Button 10", "", Text { secret: false }),
            row("tablet_key11", "Button 11", "", Text { secret: false }),
            row("tablet_key12", "Button 12", "", Text { secret: false }),
            heading("h_bt", "Bluetooth"),
            row("win_bt", "Bluetooth", "", Toggle),
            row("win_bt_add", "Add a device", "Pair a Bluetooth device (Windows' Add a device wizard).", Button("Add device")),
            row("win_bt_devices", "Your devices", "", Button("Remove\u{2026}")),
            row("win_printers", "Printers & scanners", "Printers, scanners and everything plugged in.", Button("Open")),
            heading("h_mouse", "Mouse"),
            row("win_mouse_primary", "Primary mouse button", "Right for left-handed use.", Pick),
            row("win_mouse_speed", "Pointer speed", "", slider(1.0, 20.0, 1.0, Unit::Count)),
            row("win_mouse_precision", "Enhance pointer precision", "Pointer acceleration: off for games and steady aim.", Toggle),
            row("win_mouse_wheel", "Lines to scroll", "How far one notch of the wheel scrolls.", slider(1.0, 100.0, 1.0, Unit::Count)),
            row("win_mouse_inactive", "Scroll inactive windows", "The wheel scrolls what's under the pointer, not only the window in front.", Toggle),
            row("win_mouse_dblclick", "Double-click speed", "The longest gap between the two clicks.", slider(200.0, 900.0, 10.0, Unit::Ms)),
            row("win_mouse_pointers", "Pointers", "The pointer scheme and shadow (Windows' Mouse properties).", Button("Open")),
            heading("h_keyboard", "Keyboard"),
            row("win_kb_delay", "Repeat delay", "How long a key is held before it repeats (0 short, 3 long).", slider(0.0, 3.0, 1.0, Unit::Count)),
            row("win_kb_rate", "Repeat rate", "How fast it repeats (31 fastest).", slider(0.0, 31.0, 1.0, Unit::Count)),
            row("win_kb_blink", "Cursor blink rate", "The gap between blinks of the text cursor.", slider(100.0, 1200.0, 50.0, Unit::Ms)),
            row("win_kb_cues", "Underline access keys", "Shows the underlined letter for Alt shortcuts in menus.", Toggle),
        ],
        // Everything about the look, Cascade's and Windows', on one page: the
        // theme (the title and description show the one picked), its colours, the glass.
        page::LOOK => vec![

            row("theme", "Theme", "", Gallery),
            heading("h_colour", "Colour"),
            row("accent", "Cascade's accent colour", "", Accents(false)),
            row(
                "accent_glass",
                "Accent colour on the glass",
                "Tint the taskbar, Start menu, panels and the lip with it, as Windows can.",
                Toggle,
            ),
            row("win_accent", "Windows' accent colour", "", Accents(true)),
            row("win_accent_bars", "Accent colour on title bars", "And on window borders.", Toggle),
            heading("h_mode", "Light and dark"),
            row("win_dark_apps", "Dark mode for apps", "Apps that follow Windows' choice.", Toggle),
            row("win_dark_windows", "Dark mode for Windows", "Windows' own menus and dialogs.", Toggle),
            row("win_transparency", "Transparency effects", "Windows' and apps' see-through surfaces.", Toggle),
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
                "How strongly the edge catches the light (and, with Refraction, how far it bends). 0% = no outline.",
                slider(0.0, 2.0, 0.05, Unit::Percent),
            ),
            row(
                "refraction",
                "Refraction",
                "The edge bends what's behind it like a lens, as Apple's liquid glass does. Reads the screen just around each panel a few times a second; off draws the edge instead.",
                Toggle,
            ),
            heading("h_windows", "Windows"),
            row(
                "min_animation",
                "Minimise animation",
                "Windows go into their taskbar button when minimised, and come back out of it (where Windows doesn't animate them itself).",
                Toggle,
            ),
            row(
                "menu_animation",
                "Menu animation",
                "Cascade's menus (in Files and everywhere) fade and slide in, and their highlight glides to the row you point at. Off, they appear at once.",
                Toggle,
            ),
            row(
                "wobbly",
                "Wobbly windows",
                "Windows bend like jelly while you drag them, and wobble as they settle.",
                Toggle,
            ),
            row(
                "wobbly_strength",
                "Wobbliness",
                "How far windows bend and swing.",
                slider(0.3, 2.0, 0.1, Unit::Times),
            ),
            row(
                "wobbly_friction",
                "Settling",
                "Low keeps them wobbling a while; high settles them at once.",
                slider(0.1, 1.0, 0.05, Unit::Percent),
            ),
            heading("h_wall", "Background"),
            row("win_wallpaper", "Wallpaper", "A picture for the desktop.", Button("Choose\u{2026}")),
            row("win_wallpaper_fit", "Fit", "How the picture fills each screen.", Pick),
            row("win_lockscreen", "Lock screen picture", "The picture behind the sign-in and lock screen (Windows asks for permission).", Button("Choose\u{2026}")),
            heading("h_rgb", "Lighting"),
            row(
                "lighting_open",
                "RGB lighting",
                "Profiles, effects and layout for Corsair, ASUS Aura, RGB Fusion, Razer, Logitech, HyperX and more, without their apps.",
                Button("Open"),
            ),
        ],
        page::APPS => vec![
            heading("h_installed", "Installed apps"),
            row("win_uninstall", "Uninstall an app", "Desktop programs and Store apps.", Pick),
            row("win_startup", "Startup apps", "Which apps start when you sign in (ticked: on).", Pick),
            row("win_appwiz", "Programs and Features", "Windows' classic list: repair, change and updates.", Button("Open")),
            row("win_features", "Windows features", "Turn optional parts of Windows on or off.", Button("Open")),
            heading("h_defaults", "Default apps"),
            row(
                "file_manager",
                "Open folders in",
                "Cascade's Files (tabs, instant search, no Explorer needed) or File Explorer.",
                Choice(&[("cascade", "Files"), ("explorer", "File Explorer")]),
            ),
            row(
                "photo_viewer",
                "Open pictures in",
                "Auto: Cascade's viewer in shell mode (where Photos can't open), your usual app otherwise.",
                Choice(&[("auto", "Auto"), ("cascade", "Cascade"), ("windows", "Usual app")]),
            ),
            row(
                "win_defaults_note",
                "Default apps",
                "Windows keeps these from being set by other apps: right-click a file > Open with > Choose another app > Always.",
                Note,
            ),
        ],
        page::GAMING => vec![
            heading("h_game", "Gaming"),
            row("win_game_mode", "Game Mode", "Windows puts the game first: no driver installs or update restarts mid-game.", Toggle),
            row("win_game_bar", "Game Bar", "Win+G opens Xbox Game Bar (if it's installed).", Toggle),
            row("win_game_capture", "Record in the background", "Game Bar keeps the last moments of play to save as a clip (costs some performance).", Toggle),
            heading("h_graphics", "Graphics"),
            row("win_gpu_add", "Choose an app's GPU", "Pick a program, then whether it runs on the power-saving or high-performance GPU.", Button("Add an app\u{2026}")),
            row("win_gpu_list", "Apps with a GPU set", "", Button("Change\u{2026}")),
            row("win_gpu_hags", "Hardware-accelerated GPU scheduling", "The GPU schedules its own work: lower latency (needs a restart).", Toggle),
            row("win_gpu_windowed", "Optimisations for windowed games", "Lower latency for games in a window or borderless.", Toggle),
            row("win_gpu_vrr", "Variable refresh rate", "G-Sync / FreeSync for games that don't turn it on themselves.", Toggle),
            row("win_gpu_autohdr", "Auto HDR", "Brighter colour for older games on an HDR screen.", Toggle),
        ],
        page::POWER => vec![
            row("win_battery", "Battery", "This PC's battery.", Note),
            row("win_power_mode", "Power mode", "Balanced, or better battery life, or more speed (on the Balanced plan).", Pick),
            row("win_power_plan", "Power plan", "High performance, Ultimate and the like set everything themselves.", Pick),
            heading("h_plugged", "Plugged in"),
            row("win_screen_ac", "Turn off the screen after", "Plugged in.", Pick),
            row("win_sleep_ac", "Sleep after", "Plugged in.", Pick),
            row("win_hibernate_ac", "Hibernate after", "Plugged in.", Pick),
            heading("h_battery", "On battery"),
            row("win_screen_dc", "Turn off the screen after", "On battery.", Pick),
            row("win_sleep_dc", "Sleep after", "On battery.", Pick),
            row("win_hibernate_dc", "Hibernate after", "On battery.", Pick),
            row("win_saver_at", "Energy saver", "Turns on by itself at this much battery.", Pick),
            row("win_saver_dim", "Lower brightness in energy saver", "", Toggle),
            row(
                "battery_saver",
                "Battery saver",
                "On battery, turn off the visualizer bars and read stats less often.",
                Toggle,
            ),
            heading("h_buttons", "Buttons and lid"),
            row("win_pbutton_ac", "Power button", "What pressing it does (plugged in).", Pick),
            row("win_pbutton_dc", "Power button on battery", "", Pick),
            row("win_sbutton_ac", "Sleep button", "On keyboards that have one.", Pick),
            row("win_lid_ac", "Closing the lid", "Plugged in.", Pick),
            row("win_lid_dc", "Closing the lid on battery", "", Pick),
            row("win_power_cpl", "More power settings", "Windows' classic Power Options: each plan's advanced settings.", Button("Open")),
        ],
        page::TIME => vec![
            heading("h_time", "Date & time"),
            row("win_time_auto", "Set time automatically", "From Windows' time server (Windows asks for permission to change).", Toggle),
            row("win_tz_auto", "Set time zone automatically", "From where the PC is (needs Location).", Toggle),
            row("win_timezone", "Time zone", "Where your clock's time comes from.", Pick),
            row(
                "win_time_sync",
                "Set the time now",
                "Syncs the clock with Windows' time server (Windows asks first).",
                Button("Sync now"),
            ),
            row("win_time_cpl", "Date and time", "Windows' classic date and time, and extra clocks.", Button("Open")),
            heading("h_formats", "Formats"),
            row("win_time_24h", "24-hour clock", "13:00 rather than 1:00 PM.", Toggle),
            row("win_time_date", "Short date", "How dates are written.", Pick),
            row("win_time_week", "First day of the week", "Where calendars start.", Pick),
            heading("h_language", "Language"),
            row("win_layouts", "Keyboard layouts", "The layouts you type with.", Note),
            row("win_intl", "Region and language", "Languages and keyboards to add, and every format.", Button("Open")),
        ],
        page::ACCESS => vec![
            heading("h_vision", "Vision"),
            row("win_acc_text", "Text size", "For Windows' own and Store apps' text.", slider(1.0, 2.25, 0.05, Unit::Percent)),
            row("win_acc_pointer", "Mouse pointer size", "", slider(1.0, 15.0, 1.0, Unit::Count)),
            row("win_acc_caret", "Text cursor thickness", "", slider(1.0, 20.0, 1.0, Unit::Px)),
            row("win_acc_anim", "Animation effects", "Windows' own animations (Cascade's are its own).", Toggle),
            row("win_acc_scrollbars", "Always show scrollbars", "In Windows' and Store apps, rather than only when the pointer is near.", Toggle),
            row("win_acc_notif", "Dismiss notifications after", "", Pick),
            row("win_acc_contrast", "High contrast", "Strong colours for text and edges.", Toggle),
            row("win_acc_colour", "Colour filters", "Greyscale, inverted and colour-blindness filters (Ease of Access).", Button("Open")),
            heading("h_keys", "Keyboard"),
            row("win_acc_sticky", "Sticky keys", "Press Shift, Ctrl, Alt or Win one at a time for shortcuts.", Toggle),
            row("win_acc_sticky_shortcut", "Sticky keys shortcut", "Shift five times turns Sticky keys on (off: no pop-up mid-game).", Toggle),
            row("win_acc_filter", "Filter keys", "Ignores brief or repeated presses.", Toggle),
            row("win_acc_toggle", "Toggle keys", "A tone when Caps, Num or Scroll Lock is pressed.", Toggle),
            heading("h_tools", "Tools"),
            row("win_magnifier", "Magnifier", "Zooms in on part of the screen (Win++).", Button("Open")),
            row("win_narrator", "Narrator", "Reads the screen aloud.", Button("Open")),
            row("win_osk", "On-screen keyboard", "Type with the mouse or a touch screen.", Button("Open")),
            row(
                "win_ease",
                "Ease of Access Center",
                "Windows' classic accessibility settings: mouse keys and more.",
                Button("Open"),
            ),
        ],
        page::PRIVACY => vec![
            heading("h_notify", "Notifications"),
            row("win_dnd", "Do not disturb", "Notification pop-ups stay quiet; they still go in the list.", Toggle),
            row("win_toasts", "Notifications from apps", "Windows' own switch for every app's notifications.", Toggle),
            heading("h_general", "General"),
            row("win_ad_id", "Advertising ID", "Lets apps show ads based on what you do.", Toggle),
            row("win_track_launches", "Track app launches", "Lets Windows sort Start and search by what you use.", Toggle),
            row("win_tailored", "Tailored experiences", "Tips and offers from Microsoft based on diagnostic data.", Toggle),
            row(
                "win_speech",
                "Online speech recognition",
                "Lets dictation (Win+H) use Microsoft's speech service.",
                Toggle,
            ),
            heading("h_privacy", "App permissions"),
            row("win_camera", "Camera", "Let apps use the camera.", Toggle),
            row("win_mic_access", "Microphone", "Let apps use the microphone.", Toggle),
            row("win_location", "Location", "Let apps know where this PC is.", Toggle),
            row("win_cap_contacts", "Contacts", "Let apps read your contacts.", Toggle),
            row("win_cap_calendar", "Calendar", "Let apps read your calendar.", Toggle),
            row("win_cap_calls", "Phone calls", "Let apps make calls.", Toggle),
            row("win_cap_account", "Account info", "Let apps see your name and picture.", Toggle),
            row("win_cap_documents", "Documents", "Let apps open your Documents folder.", Toggle),
            row("win_cap_pictures", "Pictures", "Let apps open your Pictures folder.", Toggle),
            row("win_cap_videos", "Videos", "Let apps open your Videos folder.", Toggle),
            row("win_cap_files", "File system", "Let apps that ask open any file you can.", Toggle),
            row("win_cap_screenshots", "Screenshots", "Let apps take pictures of the screen.", Toggle),
            row("win_cap_radios", "Radios", "Let apps turn Wi-Fi and Bluetooth on and off.", Toggle),
        ],
        page::DEBLOAT => vec![
            row("win_db_status", "Debloat", "", Note),
            row("win_db_all", "Turn everything on", "Every switch below (Windows asks once for the machine-wide ones).", Button("All on")),
            row("win_db_none", "Turn everything off", "Windows' own defaults back, for every switch below.", Button("All off")),
            heading("h_db_ai", "Telemetry and AI"),
            row("win_db_telemetry", "Telemetry", "Optional diagnostic data off, and the telemetry service stopped (Windows asks for permission).", Toggle),
            row("win_db_copilot", "Copilot", "Turns Windows Copilot off.", Toggle),
            row("win_db_recall", "Recall and Click to Do", "No snapshots of what's on your screen, and no Click to Do.", Toggle),
            row("win_db_typing", "Typing and inking data", "Windows stops learning from what you type and write.", Toggle),
            row("win_db_activity", "Activity history", "Windows stops keeping and uploading what you've opened (asks for permission).", Toggle),
            heading("h_db_ads", "Ads and suggestions"),
            row("win_db_suggestions", "Suggestions and sponsored apps", "No suggested apps in Start, no tips, no apps installed by themselves.", Toggle),
            row("win_db_lockscreen", "Lock screen tips", "No fun facts and ads on the lock screen.", Toggle),
            row("win_db_web_search", "Web results in Windows search", "Windows' own search stays on this PC (no Bing).", Toggle),
            row("win_db_feedback", "Feedback requests", "Windows stops asking for feedback.", Toggle),
            heading("h_db_bg", "In the background"),
            row("win_db_edge", "Edge in the background", "No startup boost, and Edge closes when you close it.", Toggle),
            row("win_db_delivery", "Sharing updates with other PCs", "Updates come from Microsoft only, not uploaded to or downloaded from other PCs (asks for permission).", Toggle),
        ],
        page::SYSTEM => vec![
            heading("h_about", "About"),
            row("about", "Cascade", "A tiny native notch, taskbar and Start menu for Windows.", Note),
            row("win_about", "This PC", "Its name, Windows and hardware.", Note),
            row("resources", "Cascade's resource use", "", Note),
            heading("h_cascade_files", "Cascade's files"),
            row("open_config", "Config file", "Every setting, as plain text.", Button("Open")),
            row("open_folder", "Data folder", "Config, pinned apps and logs.", Button("Open")),
            row(
                "defender_exclude",
                "Stop Windows Security deleting Cascade",
                "Adds Cascade's folder to Windows Security's exclusions (Windows asks for permission once).",
                Button("Add exclusion"),
            ),
            heading("h_pc", "This PC"),
            row("win_rename", "Rename this PC", "The name other devices see.", Button("Rename")),
            row("win_account", "Account", "", Note),
            row("win_accounts", "User accounts", "Add, remove and change accounts and passwords.", Button("Open")),
            row("win_lock", "Lock", "Locks the PC (Win+L).", Button("Lock")),
            heading("h_storage", "Storage"),
            row("win_storage", "Storage", "Space on each drive.", Note),
            row("win_diskmap", "Disk usage", "What's taking the space, as a map you can click into.", Button("Open")),
            row("win_storage_sense", "Storage Sense", "Empties the Recycle Bin and temporary files now and then.", Toggle),
            row("win_cleanup", "Free up space", "Windows' Disk Cleanup.", Button("Clean up")),
            heading("h_system_more", "System"),
            row("win_clip_history", "Clipboard history", "Keeps what you copy, for Win+V.", Toggle),
            row("win_rdp", "Remote Desktop", "Lets you connect to this PC from another (Windows asks for permission).", Toggle),
            row("win_devmgr", "Device Manager", "Drivers and hardware.", Button("Open")),
            row("win_env", "Environment variables", "PATH and the like.", Button("Open")),
            row(
                "win_sysprops",
                "Advanced system settings",
                "Performance, virtual memory, startup and recovery.",
                Button("Open"),
            ),
            row("win_taskmgr", "Task Manager", "What's running, and how hard the PC is working.", Button("Open")),
            heading("h_files", "Files"),
            row("win_file_ext", "Show file extensions", "\u{201c}photo.jpg\u{201d} rather than \u{201c}photo\u{201d}, on the desktop and in Open boxes.", Toggle),
            row("win_hidden", "Show hidden files", "On the desktop and in Open boxes.", Toggle),
            heading("h_recovery", "Recovery"),
            row("win_restore", "System Restore", "Puts system files and settings back to an earlier point.", Button("Open")),
            row("win_advanced_startup", "Advanced startup", "Restarts into recovery: safe mode, startup settings, UEFI.", Button("Restart now")),
            row("win_reset_pc", "Reset this PC", "Reinstalls Windows, keeping your files or not.", Button("Reset")),
        ],
        10 => vec![],
        _ => vec![],
    }
}

/// Whether row `key` of `section` would change anything with the other
/// settings as they are. Rows that wouldn't (a lip setting in Notch style,
/// the Start menu's parts with Cascade's Start menu off) are left out, and
/// come back as soon as they would.
fn shown(section: usize, key: &str, get: &dyn Fn(&str) -> String) -> bool {
    if section == SEARCH {
        let from = FOUND.with(|f| f.borrow().from.iter().find(|(k, _)| *k == key).map(|&(_, s)| s));
        return from.is_none_or(|s| shown(s, key, get));
    }
    let on = |k: &str| get(k) == "true";
    let lip = get("notch_style") == "lip";
    let side = matches!(get("notch_edge").as_str(), "left" | "right");
    // Windows' settings that don't apply here (HDR on a screen without it, battery on a desktop...).
    if key.starts_with("win_") && !crate::winset::shown(key) {
        return false;
    }
    match (section, key) {
        (page::LOOK, "wobbly_strength" | "wobbly_friction") => on("wobbly"),
        // Check first; Update now once there's something to update to.
        (9, "update_now") => on("update_available"),
        (9, "update_check_now") => !on("update_available"),
        (page::LOOK, "refraction") => get("theme") == "liquid_glass" && on("acrylic"),
        (0, "alt_tab_style") => on("alt_tab"),
        // Explorer snaps windows when it's the shell; Cascade does when it is.
        (0, "snap_drag") => on("shell_mode") || on("shell_active"),
        // As the shell, Windows starts Cascade itself.
        (0, "autostart") => !on("shell_active"),
        (page::POWER, "battery_saver") => on("battery"),
        // Notch.
        (2, "notch") => true,
        (2, _) if !on("notch") => false,
        (2, "lip_width" | "lip_height") => lip,
        // Only a PC with a battery has a charger to plug in.
        (2, "charge_osd") => on("battery"),
        (2, "idle_width") => !lip,
        // The line only shows on a slim strip, and only with Live activity.
        (2, "lip_visualizer") => on("live_activity") && (lip || side),
        (2, "hide_windows_flyout") => on("volume_osd") || on("brightness_osd"),
        (2, "usb_unplug" | "edit_devices") => get("usb_popups") != "off",
        (page::SOUND, "ducker_trigger" | "ducker_targets" | "ducker_threshold" | "ducker_level" | "ducker_release") => {
            on("ducker")
        }
        // Taskbar.
        (3, "dock") => true,
        (3, _) if !on("dock") => false,
        (3, "status_click") => on("show_status"),
        // Only a PC with a battery has a level to show.
        (3, "battery_percent") => on("show_status") && on("battery"),
        (3, "taskbar_screen_apps" | "taskbar_screen_clock") => get("taskbar_screens") == "all",
        // Start menu: the helper also serves Alt+Tab and the shortcuts.
        (4, "launcher" | "admin_helper") => true,
        (4, _) if !on("launcher") => false,
        (4, "start_columns" | "start_pinned_rows") => on("start_show_pinned"),
        (4, k) if k.starts_with("start_place_") => on("start_show_sidebar"),
        (4, "start_sidebar_compact" | "start_show_profile" | "start_show_cascade") => on("start_show_sidebar"),
        (4, "profile_picture_choose") => on("start_show_sidebar") && on("start_show_profile"),
        (4, "profile_picture_clear") => {
            on("start_show_sidebar") && on("start_show_profile") && !get("profile_picture").trim().is_empty()
        }
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
        (6, "desktops_style") => Board::Taskbar.read(get)[..2].iter().flatten().any(|n| *n == "desktops"),
        // The GPU widget on the taskbar, or the panel's system card.
        (6, "gpu_cards") => {
            Board::Taskbar.read(get)[..2].iter().flatten().any(|n| *n == "gpu")
                || Board::Panel.read(get)[0].contains(&"system")
        }
        (6, "gpu_temp" | "gpu_value" | "gpu_label") => {
            Board::Taskbar.read(get)[..2].iter().flatten().any(|n| *n == "gpu")
        }
        // Screens.
        // The pen tablet's settings only with the driver on.
        (page::DEVICES, "quickshare_accept" | "quickshare_name" | "quickshare_folder" | "quickshare_menus") => {
            on("quickshare")
        }
        (page::DEVICES, "tablet_lower_keys") => on("tablet") && get("tablet_lower") == "keys",
        (page::DEVICES, "tablet_upper_keys") => on("tablet") && get("tablet_upper") == "keys",
        (page::DEVICES, k) if k.starts_with("tablet_") => on("tablet"),
        (page::DISPLAY, "mouse_align" | "screen_inches") => on("mouse_match"),
        // Weather.
        (8, "weather_location" | "fahrenheit" | "weather_click") => on("weather"),
        (8, "weather_url") => on("weather") && get("weather_click") == "custom",
        (8, "calendar_soon") => !get("calendar_url").trim().is_empty(),
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
        "autostart" if on("autostart_late") => (
            "Windows' startup list runs Cascade late. Turn this off and on again to start it as you sign in instead (Windows asks for permission once).",
            true,
        ),
        "shell_mode" if on("shell_active") && !on("shell_mode") => (
            "Off from your next sign-in. For Explorer now: turn this on and off again and say yes, or right-click the taskbar > Quit to Explorer.",
            false,
        ),
        "shell_mode" if on("shell_active") => (
            "Cascade is the shell: Explorer's taskbar and Start menu don't exist. Store apps and Windows' Settings need Explorer. Turning this off offers to bring Explorer back now.",
            false,
        ),
        "shell_mode" if on("shell_mode") => (
            "On from your next sign-in. Store apps and Windows' Settings need Explorer. If Cascade fails to start, Explorer comes back by itself.",
            true,
        ),
        "shell_mode" if !on("dock") || !on("launcher") => (
            "Turn on Cascade's taskbar and Start menu first: as the shell, they are the only ones there are.",
            true,
        ),
        "shortcut_win_n" if !on("dock") => (taskbar_off, true),
        "shortcut_win_a" if !on("notch") => {
            ("The notch's Controls instead of Windows' quick settings. The notch is off, so they come down on their own.", false)
        }
        "status_click" if get("status_click") == "notch" && !on("notch") => {
            ("The notch is off, so Controls come down on their own and go again once they close.", false)
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
        Unit::Db => format!("{v:.0} dB"),
    }
}

/// The pen tablet's pressure curve: where it starts drawing, where a press
/// is full, and how firm it is between (see `tablet::curve`).
#[derive(Clone, Copy, PartialEq, Debug)]
struct PenCurve {
    tip: f32,
    full: f32,
    firmness: f32,
}

impl PenCurve {
    fn read(get: &dyn Fn(&str) -> String) -> PenCurve {
        let num = |k: &str, d: f32| get(k).parse::<f32>().unwrap_or(d);
        PenCurve {
            tip: num("tablet_tip", 0.04).clamp(0.0, 0.3),
            full: num("tablet_full", 0.9).clamp(0.4, 1.0),
            firmness: num("tablet_firmness", 1.0).clamp(0.3, 3.0),
        }
    }

    fn at(&self, level: f32) -> f32 {
        crate::tablet::curve(level, self.tip, self.full, self.firmness)
    }

    /// The draggable points (level, pressure): the tip, the bend, a full press.
    fn points(&self) -> [(f32, f32); 3] {
        let mid = (self.tip + self.full) / 2.0;
        [(self.tip, 0.0), (mid, self.at(mid)), (self.full, 1.0)]
    }

    /// Point `which` dragged to (level, pressure).
    fn moved(mut self, which: u8, level: f32, pressure: f32) -> PenCurve {
        match which {
            0 => self.tip = level.clamp(0.0, 0.3_f32.min(self.full - 0.1)),
            2 => self.full = level.clamp(0.4_f32.max(self.tip + 0.1), 1.0),
            _ => self.firmness = (pressure.clamp(0.04, 0.96).ln() / 0.5f32.ln()).clamp(0.3, 3.0),
        }
        self
    }

    fn change(&self) -> Change {
        let r = |v: f32| format!("{:.2}", v);
        Change::Many(vec![
            ("tablet_tip", r(self.tip)),
            ("tablet_full", r(self.full)),
            ("tablet_firmness", r(self.firmness)),
        ])
    }
}

/// The graph's plot inside the pressure row's control (x, y, w, h).
fn curve_plot(area: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let h = area.3 - 16.0;
    (area.0 + 10.0, area.1 + 6.0, (area.2 * 0.55).clamp(h, 420.0), h)
}

/// The test pad's Clear button (x, y, w, h).
fn pad_clear(area: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    (area.0 + area.2 - 86.0, area.1 + 10.0, 76.0, 28.0)
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
    /// A screen in the arrangement: row, screen.
    Screen(usize, usize),
    /// An accent colour's dot: row, dot (0 the Windows accent).
    Accent(usize, usize),
}

/// The default for `key`, if the row should offer a reset button.
fn default_of(key: &str) -> Option<String> {
    if matches!(key, "autostart" | "shell_mode" | "github_token" | "admin_helper") {
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
    screen_drag: Option<ScreenDrag>,
    /// A point of the pressure graph being dragged: row, which point, and
    /// the curve as it's being set (tip, full press, firmness).
    pen_drag: Option<(usize, u8, PenCurve)>,
    /// The test pad's lines (points in the pad, logical units, with their
    /// pressure), whether the pen's drawing one now, and the pen reading
    /// last shown.
    pad: RefCell<Vec<Vec<(f32, f32, f32)>>>,
    pad_down: bool,
    pen_seq: u64,
    /// Rows that just came into view (a setting turned on brought them):
    /// when, so they grow open rather than appear at once.
    appear: RefCell<HashMap<&'static str, Instant>>,
    /// The wallpaper, blurred, behind everything as Windows 11's Mica is
    /// (see `backdrop`), and the screen it was made for.
    mica: RefCell<Option<Mica>>,
    /// Text boxes: window, row key, and the text they were loaded with (only
    /// a box the user actually edited is written back). Every text row of the
    /// section has one; those of rows left out are hidden.
    edits: Vec<(HWND, &'static str, String)>,
    font: HFONT,
    pub brush: HBRUSH,
    measure: Option<Measure>,
    /// The current section's rows sized to their text.
    fit: RefCell<Fit>,
    /// The search box (a native text box in the sidebar), what it held when
    /// last read, and the section to go back to when it is emptied.
    search_edit: HWND,
    search: String,
    before_search: usize,
    /// The client area, logical units (it can be resized).
    cw: f32,
    ch: f32,
    /// Where scrolling is going (`scroll` eases to it), and the sidebar's scroll.
    scroll_to: f32,
    nav_scroll: f32,
    /// The sidebar's marker, gliding to the open page.
    nav_mark: f32,
    /// When the open page was opened (it slides in).
    page_since: Instant,
    /// How lit each recently hovered thing is (fading in and out).
    glow: Vec<(Hit, f32)>,
    /// Each toggle's knob: where it's drawn (0 off, 1 on) and where it's going.
    knobs: RefCell<HashMap<&'static str, (f32, f32)>>,
    last_tick: Instant,
    animating: std::cell::Cell<bool>,
    /// The Lighting pane (made the first time its page opens), and where it was put.
    lighting: std::cell::Cell<Option<HWND>>,
    lighting_at: std::cell::Cell<(i32, i32, i32, i32)>,
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
    /// Shows page `section` (see `SECTIONS`).
    pub fn show_section(&mut self, section: usize) {
        if section < SECTIONS.len() || section == LIGHTING {
            self.go_to(merged(section));
        }
    }

    /// Opens page `section`: it slides in, and the sidebar shows it.
    fn go_to(&mut self, section: usize) {
        if section != self.section {
            self.page_since = Instant::now();
        }
        self.section = section;
        self.scroll = 0.0;
        self.scroll_to = 0.0;
        self.hovered = None;
        if section == LIGHTING {
            self.room_for_lighting();
        }
        // The sidebar scrolls to show the page.
        if let Some(pos) = nav_pos(section) {
            let y = nav_y(pos) - self.nav_scroll;
            let view = self.ch - 16.0;
            if y < NAV_TOP {
                self.nav_scroll -= NAV_TOP - y;
            } else if y + NAV_H > view {
                self.nav_scroll += y + NAV_H - view;
            }
            self.nav_scroll = self.nav_scroll.clamp(0.0, self.nav_max());
        }
        self.invalidate();
    }

    fn nav_max(&self) -> f32 {
        (NAV_TOP + nav_total() - self.ch).max(0.0)
    }

    /// How far down the page is while it slides in.
    fn page_dy(&self) -> f32 {
        (1.0 - ease(self.page_since.elapsed().as_secs_f32() / PAGE_S)) * 26.0
    }

    /// How lit `h` is (0 to 1).
    fn glow(&self, h: Hit) -> f32 {
        self.glow.iter().find(|(g, _)| *g == h).map_or(0.0, |(_, a)| *a)
    }

    /// The client area changed size.
    pub fn on_size(&mut self) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let (w, h) = ((r.right - r.left) as f32 / self.k, (r.bottom - r.top) as f32 / self.k);
        if w > 0.0 && h > 0.0 && (w != self.cw || h != self.ch) {
            self.cw = w;
            self.ch = h;
            self.fit.borrow_mut().section = None;
            self.nav_scroll = self.nav_scroll.clamp(0.0, self.nav_max());
            self.invalidate();
        }
    }

    /// A frame of the animations; false once everything has settled (the timer stops).
    pub fn tick(&mut self) -> bool {
        let dt = self.last_tick.elapsed().as_secs_f32().min(0.05);
        self.last_tick = Instant::now();
        let follow = |v: &mut f32, to: f32, rate: f32| {
            *v += (to - *v) * (1.0 - (-dt * rate).exp());
            if (to - *v).abs() < 0.3 {
                *v = to;
            }
        };
        let before = self.scroll;
        let max = self.max_scroll();
        self.scroll_to = self.scroll_to.clamp(0.0, max);
        follow(&mut self.scroll, self.scroll_to, 16.0);
        let mark = match self.section {
            LIGHTING => nav_pos(page::LOOK).map_or(self.nav_mark, nav_y),
            s => nav_pos(s).map_or(self.nav_mark, nav_y),
        };
        follow(&mut self.nav_mark, mark, 18.0);
        let hovered = self.hovered;
        if let Some(h) = hovered {
            if !self.glow.iter().any(|(g, _)| *g == h) {
                self.glow.push((h, 0.0));
            }
        }
        for (g, a) in &mut self.glow {
            let to = if Some(*g) == hovered { 1.0 } else { 0.0 };
            *a = if to > *a { (*a + dt / 0.10).min(1.0) } else { (*a - dt / 0.18).max(0.0) };
        }
        self.glow.retain(|(g, a)| *a > 0.0 || Some(*g) == hovered);
        for (pos, to) in self.knobs.borrow_mut().values_mut() {
            *pos = if *to > *pos { (*pos + dt / 0.16).min(*to) } else { (*pos - dt / 0.16).max(*to) };
        }
        let sliding = self.page_since.elapsed().as_secs_f32() < PAGE_S;
        let growing = self.appear.borrow().values().any(|t| t.elapsed().as_secs_f32() < GROW_S);
        if before != self.scroll || sliding || growing {
            self.place_edits();
        }
        let busy = sliding
            || self.scroll != self.scroll_to
            || self.nav_mark != mark
            || self.glow.iter().any(|(g, a)| *a < 1.0 && Some(*g) == hovered || *a > 0.0 && Some(*g) != hovered)
            || self.knobs.borrow().values().any(|(p, t)| p != t)
            || self.pen_drag.is_some()
            || self.appear.borrow().values().any(|t| t.elapsed().as_secs_f32() < GROW_S);
        // With the pen's settings showing, the pen is followed (redrawn only as it moves).
        let watching = self.rows().iter().any(|r| matches!(r.ctl, Ctl::PenCurve | Ctl::PenPad));
        let mut pen_moved = false;
        if watching {
            let live = crate::tablet::live();
            if live.seq != self.pen_seq {
                self.pen_seq = live.seq;
                self.follow_pen(&live);
                pen_moved = true;
            }
        }
        unsafe {
            if busy || pen_moved {
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
            if !busy && !watching {
                let _ = KillTimer(Some(self.hwnd), ANIM_TIMER);
                self.animating.set(false);
            }
        }
        busy || watching
    }

    /// The pen's latest reading: it draws on the test pad while it touches inside it.
    fn follow_pen(&mut self, live: &crate::tablet::Live) {
        let rows = self.rows();
        let Some(i) = rows.iter().position(|r| matches!(r.ctl, Ctl::PenPad)) else { return };
        let (ax, ay, aw, ah) = self.ctl_rect(i, &rows[i].ctl);
        let mut pt = POINT { x: live.x, y: live.y };
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut pt);
        }
        let (lx, ly) = (pt.x as f32 / self.k - ax, pt.y as f32 / self.k - ay);
        let inside = lx >= 0.0 && ly >= 0.0 && lx < aw && ly < ah && ay + ly >= TOP - 8.0;
        if !(live.touching && inside) {
            self.pad_down = false;
            return;
        }
        let mut pad = self.pad.borrow_mut();
        if !self.pad_down || pad.is_empty() {
            // Only so much is kept: the oldest lines go first.
            if pad.len() > 400 {
                pad.remove(0);
            }
            pad.push(Vec::new());
        }
        if let Some(line) = pad.last_mut() {
            line.push((lx, ly, live.pressure));
        }
        self.pad_down = true;
    }

    pub fn create(instance: HINSTANCE) -> Result<Self> {
        unsafe {
            let style = WS_OVERLAPPEDWINDOW;
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
                screen_drag: None,
                pen_drag: None,
                pad: RefCell::default(),
                pad_down: false,
                pen_seq: 0,
                mica: RefCell::new(None),
                appear: RefCell::default(),
                edits: Vec::new(),
                font: HFONT::default(),
                brush: CreateSolidBrush(COLORREF(0x002B2B2B)),
                measure: Measure::new().ok(),
                fit: RefCell::default(),
                search_edit: HWND::default(),
                search: String::new(),
                before_search: 0,
                cw: DEF_W,
                ch: DEF_H,
                scroll_to: 0.0,
                nav_scroll: 0.0,
                nav_mark: nav_y(0),
                page_since: Instant::now(),
                glow: Vec::new(),
                knobs: RefCell::default(),
                last_tick: Instant::now(),
                animating: std::cell::Cell::new(false),
                lighting: std::cell::Cell::new(None),
                lighting_at: std::cell::Cell::new((0, 0, 0, 0)),
            };
            s.apply_dpi(None);
            s.search_edit = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                w!(""),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(0x80), // ES_AUTOHSCROLL
                0,
                0,
                0,
                0,
                Some(hwnd),
                Some(HMENU(SEARCH_ID as *mut _)),
                Some(instance),
                None,
            )
            .unwrap_or_default();
            // EM_SETCUEBANNER: the grey hint while it is empty.
            let hint: Vec<u16> = "Search settings\0".encode_utf16().collect();
            SendMessageW(s.search_edit, 0x1501, Some(WPARAM(1)), Some(LPARAM(hint.as_ptr() as isize)));
            s.place_search();
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            Ok(s)
        }
    }

    /// Sizes the window for its DPI (centred, or at `suggested`).
    pub fn apply_dpi(&mut self, suggested: Option<RECT>) {
        unsafe {
            self.k = GetDpiForWindow(self.hwnd).max(96) as f32 / 96.0;
            match suggested {
                // Windows says where it goes on the new screen.
                Some(s) => {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        s.left,
                        s.top,
                        s.right - s.left,
                        s.bottom - s.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                None => {
                    let mon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTOPRIMARY);
                    let mut mi =
                        MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                    let _ = GetMonitorInfoW(mon, &mut mi);
                    let wa = mi.rcWork;
                    // As big as it's made to be, on a screen that has room for it.
                    let fit_w = ((wa.right - wa.left) as f32 * 0.92 / self.k).max(MIN_W);
                    let fit_h = ((wa.bottom - wa.top) as f32 * 0.92 / self.k).max(MIN_H);
                    let (cw, ch) = ((DEF_W.min(fit_w) * self.k) as i32, (DEF_H.min(fit_h) * self.k) as i32);
                    let mut r = RECT { left: 0, top: 0, right: cw, bottom: ch };
                    let _ = AdjustWindowRectEx(&mut r, WS_OVERLAPPEDWINDOW, false, WS_EX_APPWINDOW);
                    let (w, h) = (r.right - r.left, r.bottom - r.top);
                    let (x, y) = (wa.left + (wa.right - wa.left - w) / 2, wa.top + (wa.bottom - wa.top - h) / 2);
                    let _ = SetWindowPos(self.hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
                }
            }
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
        self.place_search();
        self.on_size();
    }

    /// Puts the search box in its place in the sidebar (and its font).
    fn place_search(&self) {
        if self.search_edit.is_invalid() {
            return;
        }
        let (x, y, w, h) = SEARCH_BOX;
        let k = self.k;
        unsafe {
            SendMessageW(self.search_edit, WM_SETFONT, Some(WPARAM(self.font.0 as usize)), Some(LPARAM(1)));
            let _ = SetWindowPos(
                self.search_edit,
                None,
                ((x + 32.0) * k) as i32,
                ((y + 6.0) * k) as i32,
                ((w - 32.0 - 12.0) * k) as i32,
                ((h - 12.0) * k) as i32,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    /// The search box changed: list what matches, or (once it is emptied)
    /// go back to the section that was open.
    pub fn search_changed(&mut self, get: &dyn Fn(&str) -> String) {
        let text = edit_text(self.search_edit).trim().to_string();
        if text == self.search {
            return;
        }
        self.search = text;
        if self.search.is_empty() {
            if self.section == SEARCH {
                self.section = self.before_search;
            }
        } else {
            if self.section != SEARCH {
                self.before_search = self.section;
                self.section = SEARCH;
            }
            search(&self.search);
        }
        self.scroll = 0.0;
        self.scroll_to = 0.0;
        self.page_since = Instant::now();
        self.hovered = None;
        self.sync_edits(get);
        self.invalidate();
    }

    /// Windows 11's Mica, drawn: the wallpaper of the screen the window is
    /// on, blurred away and darkened, lying still behind the window as it
    /// moves (see `moved`). The blurred picture is made on a thread of its
    /// own the first time; until then it's the plain background.
    fn backdrop(&self, p: &Painter, cw: f32, ch: f32) {
        let screen = unsafe {
            let mon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            mi.rcMonitor
        };
        if !self.mica.borrow().as_ref().is_some_and(|m| m.0 == screen) {
            let done = MICA_DONE.lock().unwrap_or_else(|e| e.into_inner());
            if done.as_ref().is_some_and(|m| m.0 == screen) {
                *self.mica.borrow_mut() = done.clone();
            } else if !MICA_PENDING.swap(true, std::sync::atomic::Ordering::AcqRel) {
                let hwnd = self.hwnd.0 as isize;
                std::thread::spawn(move || {
                    let made = mica_picture(screen);
                    *MICA_DONE.lock().unwrap_or_else(|e| e.into_inner()) = made;
                    MICA_PENDING.store(false, std::sync::atomic::Ordering::Release);
                    unsafe {
                        let _ = InvalidateRect(Some(HWND(hwnd as *mut _)), None, false);
                    }
                });
            }
        }
        let mica = self.mica.borrow();
        let Some((_, w, h, px)) = mica.as_ref() else { return };
        let Some(bmp) = p.bitmap(*w, *h, px) else { return };
        let mut origin = POINT::default();
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut origin);
        }
        let k = self.k;
        p.image(
            &bmp,
            (screen.left - origin.x) as f32 / k,
            (screen.top - origin.y) as f32 / k,
            (screen.right - screen.left) as f32 / k,
            (screen.bottom - screen.top) as f32 / k,
            1.0,
        );
        // Mica's tint: the picture's colour only just showing through.
        p.rrect(0.0, 0.0, cw, ch, 0.0, MICA_TINT);
    }

    /// The window moved: the backdrop stays where the wallpaper is.
    pub fn moved(&self) {
        if self.mica.borrow().is_some() {
            unsafe {
                // The Lighting pane too: it sits on the same backdrop.
                let _ = RedrawWindow(Some(self.hwnd), None, None, RDW_INVALIDATE | RDW_ALLCHILDREN);
            }
        }
    }

    /// The page's title.
    fn title(&self) -> &'static str {
        if self.section == LIGHTING {
            return "Lighting";
        }
        SECTIONS.get(self.section).copied().unwrap_or("Search results")
    }

    /// Lighting wants a wide window: a narrow one grows to the right (moving
    /// left only as far as it must to stay on its screen). The window's own
    /// WM_SIZE can't reach this (it comes while the settings are borrowed),
    /// so the new size is taken here.
    fn room_for_lighting(&mut self) {
        const WANT: f32 = 1320.0;
        if self.cw >= WANT - 1.0 {
            return;
        }
        unsafe {
            let mut r = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut r);
            let mon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            let work = mi.rcWork;
            let extra = ((WANT - self.cw) * self.k).ceil() as i32;
            let width = (r.right - r.left + extra).min(work.right - work.left);
            if width <= r.right - r.left {
                return;
            }
            let x = r.left.min(work.right - width).max(work.left);
            let _ = SetWindowPos(self.hwnd, None, x, r.top, width, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        self.on_size();
    }

    /// Shows, places or hides the Lighting pane for the page that's open.
    fn place_lighting(&self) {
        let open = self.section == LIGHTING;
        let pane = match self.lighting.get() {
            Some(h) if unsafe { IsWindow(Some(h)).as_bool() } => Some(h),
            _ if open => {
                let h = crate::rgb::app::embed(self.hwnd);
                self.lighting.set(h);
                h
            }
            _ => None,
        };
        let Some(pane) = pane else { return };
        unsafe {
            let style = GetWindowLongW(self.hwnd, GWL_STYLE) as u32;
            let clip = WS_CLIPCHILDREN.0;
            if open {
                // The settings window doesn't paint over the pane.
                if style & clip == 0 {
                    SetWindowLongW(self.hwnd, GWL_STYLE, (style | clip) as i32);
                }
                let k = self.k;
                let at = (
                    ((SIDEBAR + 16.0) * k) as i32,
                    (86.0 * k) as i32,
                    ((self.cw - SIDEBAR - 32.0) * k) as i32,
                    ((self.ch - 86.0 - 16.0) * k) as i32,
                );
                if at != self.lighting_at.get() || !IsWindowVisible(pane).as_bool() {
                    self.lighting_at.set(at);
                    let _ = SetWindowPos(
                        pane,
                        None,
                        at.0,
                        at.1,
                        at.2,
                        at.3,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
            } else if IsWindowVisible(pane).as_bool() {
                let _ = ShowWindow(pane, SW_HIDE);
                SetWindowLongW(self.hwnd, GWL_STYLE, (style & !clip) as i32);
            }
        }
    }

    /// Redraws, and runs the animations until they settle.
    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            if !self.animating.replace(true) {
                SetTimer(Some(self.hwnd), ANIM_TIMER, 15, None);
            }
        }
    }

    fn content_w(&self) -> f32 {
        (self.cw - SIDEBAR - 64.0).clamp(300.0, MAX_CONTENT)
    }

    /// The cards' left edge (centred in a wide window).
    fn x0(&self) -> f32 {
        SIDEBAR + ((self.cw - SIDEBAR - self.content_w()) / 2.0).max(28.0)
    }

    fn max_scroll(&self) -> f32 {
        (TOP + self.offset(usize::MAX) + 24.0 - self.ch).max(0.0)
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
        self.scroll.min(self.max_scroll())
    }

    fn row_y(&self, i: usize) -> f32 {
        TOP + self.offset(i) - self.scroll() + self.page_dy()
    }

    /// Height of row `i` of the current section.
    fn row_h(&self, i: usize) -> f32 {
        let fit = self.fit.borrow();
        match fit.heights.get(i) {
            Some(&h) if fit.section == Some(self.section) => h,
            _ => self.rows().get(i).map_or(ROW_H, row_h),
        }
    }

    /// How far open row `key` is (1: fully; less while it grows in).
    fn grown(&self, key: &str) -> f32 {
        match self.appear.borrow().get(key) {
            Some(t) => ease(t.elapsed().as_secs_f32() / GROW_S),
            None => 1.0,
        }
    }

    /// Distance from the top of the first row to the top of row `i`.
    fn offset(&self, i: usize) -> f32 {
        let fit = self.fit.borrow();
        if fit.section == Some(self.section) {
            return fit.heights.iter().zip(&fit.keys).take(i).map(|(h, k)| (h + ROW_GAP) * self.grown(k)).sum();
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
            Ctl::Board(_)
            | Ctl::Gallery
            | Ctl::Arrange
            | Ctl::Accents(_)
            | Ctl::PenCurve
            | Ctl::PenPad
            | Ctl::Note
            | Ctl::Heading => cw - 36.0,
            _ => {
                let (rx, ..) = self.reset_rect(i, &r.ctl);
                rx - 8.0 - (self.x0() + 18.0)
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
        } else if matches!(r.ctl, Ctl::Accents(true)) {
            let v = get(r.key);
            let name = match theme::parse_accent(&v) {
                Some(c) if Some(c) == cascade_accent_rgb() => "the same as Cascade's".to_string(),
                Some(c) => theme::PALETTE
                    .iter()
                    .find(|(p, _)| *p == c)
                    .map_or_else(|| format!("#{c:06X}"), |(_, n)| n.to_string()),
                None => "Windows' default".to_string(),
            };
            format!("Highlights, buttons and links in Windows' own windows: {name}.")
        } else if matches!(r.ctl, Ctl::Accents(false)) {
            // What's picked.
            match theme::parse_accent(&get(r.key)) {
                None => "Your Windows accent colour, followed when you change it.".to_string(),
                Some(c) => match theme::PALETTE.iter().find(|(p, _)| *p == c) {
                    Some((_, name)) => format!("{name}, picked here."),
                    None => format!("#{c:06X}, your own colour (+ to pick another)."),
                },
            }
        } else if r.key == "about" && build > 0 {
            format!("{} Build {build}.", r.desc)
        } else if r.key == "screens_info" {
            crate::mouseedge::info()
        } else if r.key == "resources" {
            crate::usage::summary()
        } else if let Some(text) = r.key.starts_with("win_").then(|| crate::winset::desc(r.key)).flatten() {
            text
        } else if let Some((text, warn)) = live_desc(r.key, get) {
            return (text.to_string(), if warn { WARN } else { Color::white(0.55) }, None);
        } else if let Some(i) = crate::config::tablet_key_index(r.key) {
            let held = crate::tablet::live().buttons & (1 << i) != 0;
            if held {
                return ("Pressed now".to_string(), theme::accent(), None);
            }
            String::new()
        } else if r.key == "profile_picture_clear" {
            get("profile_picture")
        } else if r.key == "update_check_now" && !crate::update::status().is_empty() {
            crate::update::status()
        } else if r.key == "update_now" && crate::update::available() > 0 {
            format!("Build {} is ready: it downloads, and Cascade restarts into it.", crate::update::available())
        } else if r.key == "quickshare" && get(r.key) == "true" && !crate::quickshare::status().is_empty() {
            crate::quickshare::status()
        } else if r.key == "tablet" && get(r.key) == "true" {
            crate::tablet::status()
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
        // Rows new to the page as it was (not a page just opened) grow open.
        if before.section == Some(self.section) {
            let fit = self.fit.borrow();
            let fresh: Vec<&'static str> = fit.keys.iter().filter(|k| !before.keys.contains(k)).copied().collect();
            drop(fit);
            if !fresh.is_empty() {
                let now = Instant::now();
                let mut appear = self.appear.borrow_mut();
                appear.retain(|_, t| t.elapsed().as_secs_f32() < GROW_S);
                for k in fresh {
                    appear.insert(k, now);
                }
                drop(appear);
                self.invalidate();
            }
        } else {
            self.appear.borrow_mut().clear();
        }
        let Some(m) = m else {
            let fit = self.fit.borrow();
            return before.section != fit.section || before.keys != fit.keys;
        };
        let heights: Vec<f32> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let base = row_h(r);
                if matches!(
                    r.ctl,
                    Ctl::Board(_)
                        | Ctl::Gallery
                        | Ctl::Arrange
                        | Ctl::Accents(_)
                        | Ctl::PenCurve
                        | Ctl::PenPad
                        | Ctl::Heading
                ) {
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
        let x1 = self.x0() + self.content_w() - 18.0;
        let y = self.row_y(i);
        let h = self.row_h(i);
        match ctl {
            Ctl::Toggle => (x1 - 40.0, y + (h - 20.0) / 2.0, 40.0, 20.0),
            Ctl::Slider { .. } => (x1 - 170.0, y + 14.0, 170.0, h - 28.0),
            Ctl::Text { .. } => (x1 - 250.0, y + (h - 30.0) / 2.0, 250.0, 30.0),
            Ctl::Choice(opts) => {
                let w = opts.len() as f32 * self.seg_w(i);
                (x1 - w, y + (h - 32.0) / 2.0, w, 32.0)
            }
            Ctl::Button(_) => (x1 - 130.0, y + (h - 32.0) / 2.0, 130.0, 32.0),
            Ctl::Pick => (x1 - 260.0, y + (h - 32.0) / 2.0, 260.0, 32.0),
            // The samples, under the title.
            Ctl::Gallery => (
                self.x0() + 18.0,
                y + BOARD_HEAD,
                self.content_w() - 36.0,
                gallery_rows() as f32 * (SWATCH_H + SWATCH_GAP) - SWATCH_GAP,
            ),
            // The dots, under the title.
            Ctl::Accents(_) => (
                self.x0() + 18.0,
                y + BOARD_HEAD,
                self.content_w() - 36.0,
                accent_rows() as f32 * (DOT + DOT_GAP) - DOT_GAP,
            ),
            // The lanes, under the title.
            Ctl::Board(b) => (
                self.x0() + 18.0,
                y + BOARD_HEAD,
                self.content_w() - 36.0,
                b.lanes().len() as f32 * (LANE_H + LANE_GAP) - LANE_GAP,
            ),
            // The screens, under the title.
            Ctl::Arrange => (self.x0() + 18.0, y + BOARD_HEAD, self.content_w() - 36.0, ARRANGE_H),
            Ctl::PenCurve => (self.x0() + 18.0, y + BOARD_HEAD, self.content_w() - 36.0, CURVE_H),
            Ctl::PenPad => (self.x0() + 18.0, y + BOARD_HEAD, self.content_w() - 36.0, PAD_H),
            Ctl::Note | Ctl::Heading => (x1, y, 0.0, 0.0),
        }
    }

    /// The reset button of row `i` (x, y, size).
    fn reset_rect(&self, i: usize, ctl: &Ctl) -> (f32, f32, f32) {
        let (cx, _, _, _) = self.ctl_rect(i, ctl);
        let x = match ctl {
            Ctl::Slider { .. } => cx - 112.0,
            // Left of its On / Off.
            Ctl::Toggle => cx - 74.0,
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

    /// Windows' settings were read again: boxes for them that haven't been
    /// touched show the new values (the hotspot's name arrives a moment after
    /// the page opens).
    pub fn refresh_edits(&mut self, get: &dyn Fn(&str) -> String) {
        let focus = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() };
        for (h, key, loaded) in &mut self.edits {
            if !key.starts_with("win_") || *h == focus {
                continue;
            }
            let (now, shown) = (get(key), edit_text(*h));
            // A value typed here that Windows now has: it's no longer a change to send.
            if shown == now {
                *loaded = now;
                continue;
            }
            if shown == *loaded && now != *loaded {
                unsafe {
                    let _ = SetWindowTextW(*h, &windows::core::HSTRING::from(now.as_str()));
                }
                *loaded = now;
            }
        }
    }

    /// A text box lost focus: returns its new value.
    pub fn commit_edit(&self, edit: HWND) -> Option<Change> {
        let (_, key, loaded) = self.edits.iter().find(|(h, _, _)| *h == edit)?;
        let text = edit_text(edit).trim().to_string();
        (text != loaded.trim()).then_some(Change::Set(key, text))
    }

    /// All text boxes' values (when the window closes).
    pub fn commit_all(&self) -> Vec<Change> {
        self.edits.iter().filter_map(|(h, _, _)| self.commit_edit(*h)).collect()
    }

    fn hit(&self, x: i32, y: i32, get: &dyn Fn(&str) -> String) -> Option<Hit> {
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        if lx < SIDEBAR {
            if ly < NAV_TOP {
                return None;
            }
            let y = ly + self.nav_scroll;
            return (0..NAV.len()).find(|&p| y >= nav_y(p) && y < nav_y(p) + NAV_H).map(|p| Hit::Nav(NAV[p]));
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
            if let Ctl::Arrange = r.ctl {
                let rects: Vec<RECT> = crate::winset::display::arrangement().0.into_iter().map(|(_, r)| r).collect();
                let (_, boxes) = screen_boxes(self.ctl_rect(i, &r.ctl), &rects);
                for (j, (bx, by, bw, bh)) in boxes.into_iter().enumerate() {
                    if lx >= bx && lx < bx + bw && ly >= by && ly < by + bh && ly >= TOP - 8.0 {
                        return Some(Hit::Screen(i, j));
                    }
                }
                continue;
            }
            if let Ctl::Accents(_) = r.ctl {
                let area = self.ctl_rect(i, &r.ctl);
                for k in 0..=CUSTOM_DOT {
                    let (dx, dy) = dot_at(area, k);
                    let d = DOT / 2.0 + 3.0;
                    if (lx - dx).abs() < d && (ly - dy).abs() < d && ly >= TOP - 8.0 {
                        return Some(Hit::Accent(i, k));
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
        if let Some(d) = &mut self.screen_drag {
            let at = (x as f32 / self.k, y as f32 / self.k);
            d.at = at;
            if (at.0 - d.start.0).hypot(at.1 - d.start.1) > 4.0 {
                d.moved = true;
            }
            self.invalidate();
            return;
        }
        if let Some((i, which, curve)) = self.pen_drag {
            let rows = self.rows();
            let Some(r) = rows.get(i) else { return };
            let (gx, gy, gw, gh) = curve_plot(self.ctl_rect(i, &r.ctl));
            let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
            let moved = curve.moved(which, (lx - gx) / gw, (gy + gh - ly) / gh);
            self.pen_drag = Some((i, which, moved));
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
            Hit::Screen(i, j) => {
                let at = (x as f32 / self.k, y as f32 / self.k);
                self.screen_drag = Some(ScreenDrag { row: i, screen: j, start: at, at, moved: false });
                unsafe {
                    SetCapture(self.hwnd);
                }
                self.invalidate();
                None
            }
            Hit::Accent(i, k) if matches!(self.rows().get(i).map(|r| &r.ctl), Some(Ctl::Accents(true))) => {
                self.invalidate();
                match k {
                    CUSTOM_DOT => Some(Change::Action("win_accent_custom")),
                    0 => Some(Change::Set("win_accent", format!("#{:06X}", cascade_accent_rgb()?))),
                    k => Some(Change::Set("win_accent", accent_value(k)?)),
                }
            }
            Hit::Accent(_, CUSTOM_DOT) => {
                self.invalidate();
                Some(Change::Action("accent_custom"))
            }
            Hit::Accent(i, k) => {
                let r = self.rows().into_iter().nth(i)?;
                let value = accent_value(k)?;
                self.invalidate();
                (get(r.key) != value).then_some(Change::Set(r.key, value))
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
                if i != self.section || !self.search.is_empty() {
                    // Picking a section leaves the search.
                    if !self.search.is_empty() {
                        self.search.clear();
                        unsafe {
                            let _ = SetWindowTextW(self.search_edit, w!(""));
                        }
                    }
                    self.go_to(i);
                    self.sync_edits(get);
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
                    Ctl::PenCurve => {
                        let (gx, gy, gw, gh) = curve_plot(self.ctl_rect(i, &r.ctl));
                        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
                        let curve = PenCurve::read(get);
                        // The nearest point, if the press is near one.
                        let (which, d) = curve
                            .points()
                            .iter()
                            .enumerate()
                            .map(|(j, &(u, v))| (j as u8, (gx + u * gw - lx).hypot(gy + gh - v * gh - ly)))
                            .fold((0u8, f32::MAX), |a, b| if b.1 < a.1 { b } else { a });
                        if d > 26.0 {
                            return None;
                        }
                        self.pen_drag = Some((i, which, curve));
                        unsafe {
                            SetCapture(self.hwnd);
                        }
                        self.invalidate();
                        None
                    }
                    Ctl::PenPad => {
                        let (bx, by, bw, bh) = pad_clear(self.ctl_rect(i, &r.ctl));
                        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
                        if lx >= bx && lx < bx + bw && ly >= by && ly < by + bh {
                            self.pad.borrow_mut().clear();
                            self.invalidate();
                        }
                        None
                    }
                    // The list drops down from the control.
                    Ctl::Pick => {
                        let (cx, cy, cw, ch) = self.ctl_rect(i, &r.ctl);
                        let k = self.k;
                        let mut a = POINT { x: (cx * k) as i32, y: (cy * k) as i32 };
                        let mut b = POINT { x: ((cx + cw) * k) as i32, y: ((cy + ch) * k) as i32 };
                        unsafe {
                            let _ = ClientToScreen(self.hwnd, &mut a);
                            let _ = ClientToScreen(self.hwnd, &mut b);
                        }
                        crate::menu::set_anchor(RECT { left: a.x, top: a.y, right: b.x, bottom: b.y });
                        Some(Change::Action(r.key))
                    }
                    _ => None,
                }
            }
        }
    }

    pub fn on_up(&mut self, x: i32, y: i32, get: &dyn Fn(&str) -> String) -> Option<Change> {
        if let Some((_, _, curve)) = self.pen_drag.take() {
            unsafe {
                let _ = ReleaseCapture();
            }
            self.invalidate();
            return (curve != PenCurve::read(get)).then(|| curve.change());
        }
        if let Some(d) = self.screen_drag.take() {
            unsafe {
                let _ = ReleaseCapture();
            }
            self.invalidate();
            if !d.moved {
                // A click: the settings below are for that screen now.
                crate::winset::display::select(d.screen);
                return None;
            }
            let r = self.rows().into_iter().nth(d.row)?;
            let rects: Vec<RECT> = crate::winset::display::arrangement().0.into_iter().map(|(_, r)| r).collect();
            let (px, _) = screen_boxes(self.ctl_rect(d.row, &r.ctl), &rects);
            let me = rects.get(d.screen)?;
            let (nx, ny) = (x as f32 / self.k - d.start.0, y as f32 / self.k - d.start.1);
            let (left, top) = (me.left + (nx * px) as i32, me.top + (ny * px) as i32);
            let screen = d.screen;
            // Moving the screens tells every window (Cascade's too) as it
            // happens, so it's done away from this window's handler.
            std::thread::spawn(move || crate::winset::display::move_screen(screen, left, top));
            return None;
        }
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
        Some(Change::Set(r.key, format!("{v}")))
    }

    /// The wheel: scrolls the sidebar or the page, whichever the pointer is over (easing there).
    pub fn on_wheel(&mut self, delta: i16, _get: &dyn Fn(&str) -> String) {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        let step = delta as f32 / 120.0 * 100.0;
        if (p.x as f32 / self.k) < SIDEBAR {
            self.nav_scroll = (self.nav_scroll - step * 0.6).clamp(0.0, self.nav_max());
        } else {
            self.scroll_to = (self.scroll_to - step).clamp(0.0, self.max_scroll());
        }
        self.invalidate();
    }

    pub fn paint(&self, gfx: &mut Gfx, get: &dyn Fn(&str) -> String, build: u32) -> Result<()> {
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        let (pw, ph) = ((self.cw * self.k).ceil() as i32, (self.ch * self.k).ceil() as i32);
        if self.refit(get, build) {
            self.place_edits();
        }
        self.place_lighting();
        let (cwin, chin) = (self.cw, self.ch);
        let r = (|| {
            gfx.begin_dc(hdc, pw, ph, self.k, BG)?;
            {
                let p = gfx.painter();
                let f = &gfx.fonts;
                // The backdrop: the wallpaper, blurred and darkened, as
                // Windows 11's Settings sits on Mica.
                self.backdrop(&p, cwin, chin);
                // Sidebar: the search box, then each page with its icon.
                let (bx, by, bw, bh) = SEARCH_BOX;
                p.rrect(bx, by, bw, bh, 5.0, BOX_BG);
                p.stroke_rrect(bx + 0.5, by + 0.5, bw - 1.0, bh - 1.0, 5.0, Color::white(0.06), 1.0);
                p.rrect(bx + 1.0, by + bh - 1.0, bw - 2.0, 1.0, 0.5, Color::white(0.16));
                p.stroke_circle(bx + 17.0, by + 15.0, 5.0, Color::white(0.55), 1.5);
                p.line(bx + 20.5, by + 18.5, bx + 24.0, by + 22.0, Color::white(0.55), 1.5);
                p.clip(0.0, NAV_TOP, SIDEBAR, chin - NAV_TOP);
                let ns = self.nav_scroll;
                for (pos, &i) in NAV.iter().enumerate() {
                    let y = nav_y(pos) - ns;
                    if y + NAV_H < NAV_TOP || y > chin {
                        continue;
                    }
                    let active = i == self.section || (self.section == LIGHTING && i == page::LOOK);
                    let lit = self.glow(Hit::Nav(i));
                    let a = if active { 0.06 + 0.02 * lit } else { 0.04 * lit };
                    if a > 0.0 {
                        p.rrect(12.0, y + 2.0, SIDEBAR - 24.0, NAV_H - 4.0, 5.0, Color::white(a));
                    }
                    let c = Color::white(if active { 0.96 } else { 0.80 + 0.12 * lit });
                    let (ix, iy) = (34.0, y + NAV_H / 2.0);
                    match (nav_icon(i), &f.icons) {
                        (Some(ch), Some(font)) => {
                            p.text(&ch.to_string(), font, ix - 12.0, y, 24.0, NAV_H, Align::Center, c);
                        }
                        // The notch and the taskbar, drawn.
                        _ if i == 2 => p.stroke_rrect(ix - 8.0, iy - 3.5, 16.0, 7.0, 3.5, c, 1.3),
                        _ if i == 3 => {
                            p.stroke_rrect(ix - 8.0, iy - 6.0, 16.0, 12.0, 2.0, c, 1.2);
                            p.rrect(ix - 8.0, iy + 3.0, 16.0, 3.0, 1.0, c);
                        }
                        _ => p.circle(ix, iy, 2.5, c),
                    }
                    p.text(SECTIONS[i], &f.body, 56.0, y, SIDEBAR - 68.0, NAV_H, Align::Left, c);
                }
                // The marker beside the open page, gliding to it.
                if nav_pos(self.section).is_some() || self.section == LIGHTING {
                    let my = self.nav_mark - ns + NAV_H / 2.0;
                    p.rrect(12.0, my - 8.0, 3.0, 16.0, 1.5, theme::accent());
                }
                p.unclip();
                let foot = nav_y(NAV.len()) - ns + 8.0;
                if build > 0 && foot < chin - 30.0 {
                    p.text(
                        &format!("Build {build}"),
                        &f.small,
                        26.0,
                        foot,
                        160.0,
                        20.0,
                        Align::Left,
                        Color::white(0.35),
                    );
                }

                // Content: the title, then the cards.
                let x0 = self.x0();
                let cw = self.content_w();
                let dy = self.page_dy();
                let fade = ease(self.page_since.elapsed().as_secs_f32() / PAGE_S);
                p.clip(SIDEBAR, 0.0, cwin - SIDEBAR, chin);
                p.text(
                    self.title(),
                    &f.big,
                    x0,
                    30.0 - self.scroll() + dy,
                    cw,
                    48.0,
                    Align::Left,
                    Color::white(0.96 * fade.max(0.2)),
                );
                for (i, r) in self.rows().iter().enumerate() {
                    let y = self.row_y(i);
                    let h = self.row_h(i);
                    if y + h < 0.0 || y > chin {
                        continue;
                    }
                    // A row growing open (brought in by another setting) shows
                    // only as much of itself as has opened.
                    let grow = self.grown(r.key);
                    if grow < 1.0 {
                        p.clip(SIDEBAR, y, cwin - SIDEBAR, (h + ROW_GAP) * grow);
                    }
                    if matches!(r.ctl, Ctl::Heading) {
                        p.text(r.title, &f.title, x0 + 2.0, y + 16.0, cw, 24.0, Align::Left, Color::white(0.92));
                        if grow < 1.0 {
                            p.unclip();
                        }
                        continue;
                    }
                    let lit = self.glow(Hit::Row(i)).max(self.glow(Hit::Reset(i)) * 0.6);
                    // A card, as Windows 11's: a faint layer over the backdrop
                    // with a hairline edge, a touch lighter under the pointer.
                    p.rrect(x0, y, cw, h, 6.0, CARD.alpha(CARD.3 + 0.03 * lit));
                    p.stroke_rrect(x0 + 0.5, y + 0.5, cw - 1.0, h - 1.0, 6.0, Color(0.0, 0.0, 0.0, 0.22), 1.0);
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
                            &f.body,
                            x0 + 18.0,
                            y + (h - 20.0) / 2.0,
                            text_w,
                            20.0,
                            Align::Left,
                            Color::white(0.95),
                        );
                    } else {
                        p.text(title, &f.body, x0 + 18.0, y + 13.0, text_w, 20.0, Align::Left, Color::white(0.95));
                        let mut dx = x0 + 18.0;
                        if let Some(ok) = mark {
                            let (mx, my) = (dx + 5.0, y + 43.0);
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
                        if matches!(
                            r.ctl,
                            Ctl::Board(_) | Ctl::Gallery | Ctl::Arrange | Ctl::Accents(_) | Ctl::PenCurve | Ctl::PenPad
                        ) {
                            p.text(&desc, &f.small, dx, y + 34.0, dw, 18.0, Align::Left, tone);
                        } else {
                            // Wrapped onto as many lines as the row was sized for.
                            let lines = ((h - row_h(r)) / LINE_H).round() as usize + 1;
                            p.text_wrap(&desc, &f.small, dx, y + 35.0, dw, lines as f32 * LINE_H + 2.0, tone);
                        }
                    }
                    let (cx, cy, cwid, ch) = self.ctl_rect(i, &r.ctl);
                    let hovered = self.hovered == Some(Hit::Row(i));
                    match r.ctl {
                        Ctl::Toggle => {
                            let on = get(r.key) == "true";
                            // The knob slides (see `tick`).
                            let t = {
                                let mut knobs = self.knobs.borrow_mut();
                                let e = knobs.entry(r.key).or_insert((on as u8 as f32, on as u8 as f32));
                                e.1 = on as u8 as f32;
                                ease(e.0)
                            };
                            let lit = self.glow(Hit::Row(i));
                            p.rrect(cx, cy, cwid, ch, ch / 2.0, theme::accent().alpha(t * (0.9 + 0.1 * lit)));
                            p.stroke_rrect(
                                cx + 0.5,
                                cy + 0.5,
                                cwid - 1.0,
                                ch - 1.0,
                                ch / 2.0,
                                Color::white((0.6 + 0.2 * lit) * (1.0 - t)),
                                1.0,
                            );
                            let kr = 5.0 + t + 0.8 * lit;
                            let kx = cx + ch / 2.0 + (cwid - ch) * t;
                            p.circle(kx, cy + ch / 2.0, kr, Color::white(0.8 + 0.2 * t));
                            let label = if on { "On" } else { "Off" };
                            p.text(
                                label,
                                &f.body,
                                cx - 40.0,
                                cy - 4.0,
                                32.0,
                                ch + 8.0,
                                Align::Right,
                                Color::white(0.85),
                            );
                        }
                        Ctl::Slider { min, max, unit, .. } => {
                            let v = match self.drag {
                                Some((d, v)) if d == i => v,
                                _ => get(r.key).parse::<f32>().unwrap_or(min),
                            };
                            let t = ((v - min) / (max - min)).clamp(0.0, 1.0);
                            let ty = cy + ch / 2.0;
                            let knob = if hovered || self.drag.is_some_and(|(d, _)| d == i) { 9.0 } else { 7.5 };
                            p.rrect(cx, ty - 2.0, cwid, 4.0, 2.0, Color::white(0.18));
                            p.rrect(cx, ty - 2.0, (cwid * t).max(4.0), 4.0, 2.0, theme::accent());
                            p.circle(cx + cwid * t, ty, knob, Color::white(1.0));
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
                            // As Windows' buttons: neutral, lighter under the pointer.
                            let lit = self.glow(Hit::Row(i));
                            p.rrect(cx, cy, cwid, ch, 5.0, Color::white(0.06 + 0.035 * lit));
                            p.stroke_rrect(cx + 0.5, cy + 0.5, cwid - 1.0, ch - 1.0, 5.0, Color::white(0.08), 1.0);
                            p.text(label, &f.body, cx, cy, cwid, ch, Align::Center, Color::white(0.95));
                        }
                        Ctl::Pick => {
                            // As Windows' drop-down boxes.
                            let lit = self.glow(Hit::Row(i));
                            p.rrect(cx, cy, cwid, ch, 5.0, Color::white(0.06 + 0.035 * lit));
                            p.stroke_rrect(cx + 0.5, cy + 0.5, cwid - 1.0, ch - 1.0, 5.0, Color::white(0.08), 1.0);
                            let value = get(r.key);
                            let value = if value.is_empty() { "Choose\u{2026}".to_string() } else { value };
                            p.text(&value, &f.label, cx + 12.0, cy, cwid - 36.0, ch, Align::Left, Color::white(0.9));
                            // A small chevron: the list opens on a click.
                            let (vx, vy) = (cx + cwid - 18.0, cy + ch / 2.0);
                            p.line(vx - 4.0, vy - 2.0, vx, vy + 2.0, Color::white(0.6), 1.5);
                            p.line(vx, vy + 2.0, vx + 4.0, vy - 2.0, Color::white(0.6), 1.5);
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
                        Ctl::Accents(windows) => {
                            let picked =
                                if windows { win_accent_index(&get(r.key)) } else { accent_index(&get(r.key)) };
                            let area = (cx, cy, cwid, ch);
                            let custom = if windows {
                                theme::parse_accent(&get(r.key)).filter(|_| picked == Some(CUSTOM_DOT))
                            } else {
                                custom_accent(&get(r.key))
                            };
                            for k in 0..=CUSTOM_DOT {
                                let (dx, dy) = dot_at(area, k);
                                let colour = match k {
                                    0 if windows => theme::accent(),
                                    0 => theme::windows_accent(),
                                    CUSTOM_DOT => match custom {
                                        Some(c) => {
                                            let f = |s: u32| ((c >> s) & 0xFF) as f32 / 255.0;
                                            Color(f(16), f(8), f(0), 1.0)
                                        }
                                        None => Color::white(0.10),
                                    },
                                    k => {
                                        let c = theme::PALETTE[k - 1].0;
                                        let f = |s: u32| ((c >> s) & 0xFF) as f32 / 255.0;
                                        Color(f(16), f(8), f(0), 1.0)
                                    }
                                };
                                let over = self.hovered == Some(Hit::Accent(i, k));
                                let on = picked == Some(k);
                                let rad = DOT / 2.0 - if on { 3.0 } else { 0.0 } + if over && !on { 1.0 } else { 0.0 };
                                p.circle(dx, dy, rad, colour);
                                if on {
                                    p.stroke_circle(dx, dy, DOT / 2.0 + 1.0, Color::white(0.95), 2.0);
                                    // A tick in the middle.
                                    let ink = Color(0.0, 0.0, 0.0, 0.75);
                                    p.line(dx - 5.0, dy, dx - 1.5, dy + 3.5, ink, 2.0);
                                    p.line(dx - 1.5, dy + 3.5, dx + 5.5, dy - 4.0, ink, 2.0);
                                } else if k == CUSTOM_DOT {
                                    // Any colour: a plus, opening the picker.
                                    p.stroke_circle(dx, dy, DOT / 2.0 - 0.5, Color::white(0.35), 1.0);
                                    let ink = Color::white(0.9);
                                    p.line(dx - 6.0, dy, dx + 6.0, dy, ink, 1.6);
                                    p.line(dx, dy - 6.0, dx, dy + 6.0, ink, 1.6);
                                } else if k == 0 && windows {
                                    // Cascade's accent: a notch on it.
                                    p.rrect(dx - 6.0, dy - 2.5, 12.0, 5.0, 2.5, Color::white(0.9));
                                } else if k == 0 {
                                    // The Windows accent: Windows' four panes on it.
                                    let ink = Color::white(0.9);
                                    for (ox, oy) in [(-5.0, -5.0), (0.5, -5.0), (-5.0, 0.5), (0.5, 0.5)] {
                                        p.rrect(dx + ox, dy + oy, 4.5, 4.5, 0.5, ink);
                                    }
                                }
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
                        Ctl::Arrange => {
                            let area = (cx, cy, cwid, ch);
                            p.rrect(cx, cy, cwid, ch, 10.0, Color::white(0.025));
                            p.stroke_rrect(cx + 0.5, cy + 0.5, cwid - 1.0, ch - 1.0, 10.0, Color::white(0.06), 1.0);
                            let (list, chosen) = crate::winset::display::arrangement();
                            let rects: Vec<RECT> = list.iter().map(|(_, r)| *r).collect();
                            let (px, boxes) = screen_boxes(area, &rects);
                            let drag = self.screen_drag.filter(|d| d.row == i && d.moved);
                            let screen = |p: &Painter,
                                          j: usize,
                                          (bx, by, bw, bh): (f32, f32, f32, f32),
                                          lifted: bool| {
                                let picked = j == chosen;
                                let hovered = self.hovered == Some(Hit::Screen(i, j));
                                let fill = if lifted {
                                    Color(0.22, 0.22, 0.25, 1.0)
                                } else if picked {
                                    theme::accent().alpha(0.30)
                                } else {
                                    Color::white(if hovered { 0.12 } else { 0.08 })
                                };
                                if lifted {
                                    p.rrect(bx + 3.0, by + 5.0, bw, bh, 6.0, Color(0.0, 0.0, 0.0, 0.35));
                                }
                                p.rrect(bx, by, bw, bh, 6.0, fill);
                                let (edge, thick) =
                                    if picked || lifted { (theme::accent(), 2.0) } else { (Color::white(0.22), 1.0) };
                                p.stroke_rrect(bx + 0.5, by + 0.5, bw - 1.0, bh - 1.0, 6.0, edge, thick);
                                p.text(
                                    &(j + 1).to_string(),
                                    &f.big,
                                    bx,
                                    by,
                                    bw,
                                    bh - 6.0,
                                    Align::Center,
                                    Color::white(0.95),
                                );
                                if bh > 70.0 {
                                    let name = if j == 0 { format!("{} (main)", list[j].0) } else { list[j].0.clone() };
                                    p.text(
                                        &name,
                                        &f.small,
                                        bx + 6.0,
                                        by + bh - 24.0,
                                        bw - 12.0,
                                        18.0,
                                        Align::Center,
                                        Color::white(0.7),
                                    );
                                }
                            };
                            for (j, b) in boxes.iter().enumerate() {
                                if drag.is_some_and(|d| d.screen == j) {
                                    continue;
                                }
                                screen(&p, j, *b, false);
                            }
                            if let Some(d) = drag {
                                let (dx, dy) = (d.at.0 - d.start.0, d.at.1 - d.start.1);
                                if let (Some(b), Some(me)) = (boxes.get(d.screen), rects.get(d.screen)) {
                                    // Where it will land, outlined, under the one being dragged.
                                    let (left, top) = (me.left + (dx * px) as i32, me.top + (dy * px) as i32);
                                    if let Some((sx, sy)) = crate::winset::display::snap(&rects, d.screen, left, top) {
                                        let (gx, gy) =
                                            (b.0 + (sx - me.left) as f32 / px, b.1 + (sy - me.top) as f32 / px);
                                        p.rrect(gx, gy, b.2, b.3, 6.0, theme::accent().alpha(0.12));
                                        p.stroke_rrect(
                                            gx + 0.75,
                                            gy + 0.75,
                                            b.2 - 1.5,
                                            b.3 - 1.5,
                                            6.0,
                                            theme::accent().alpha(0.7),
                                            1.5,
                                        );
                                    }
                                    screen(&p, d.screen, (b.0 + dx, b.1 + dy, b.2, b.3), true);
                                }
                            }
                        }
                        Ctl::PenCurve => {
                            let curve = match self.pen_drag {
                                Some((d, _, c)) if d == i => c,
                                _ => PenCurve::read(get),
                            };
                            let dragging = self.pen_drag.map(|(d, w, _)| (d == i).then_some(w)).unwrap_or(None);
                            let (gx, gy, gw, gh) = curve_plot((cx, cy, cwid, ch));
                            let at = |u: f32, v: f32| (gx + u * gw, gy + gh - v * gh);
                            p.rrect(gx, gy, gw, gh, 6.0, Color::white(0.03));
                            p.stroke_rrect(gx + 0.5, gy + 0.5, gw - 1.0, gh - 1.0, 6.0, Color::white(0.07), 1.0);
                            for q in 1..4 {
                                let t = q as f32 / 4.0;
                                p.line(gx + gw * t, gy + 1.0, gx + gw * t, gy + gh - 1.0, Color::white(0.04), 1.0);
                                p.line(gx + 1.0, gy + gh * t, gx + gw - 1.0, gy + gh * t, Color::white(0.04), 1.0);
                            }
                            // Straight through, for comparison.
                            let (ax, ay) = at(0.0, 0.0);
                            let (bx, by) = at(1.0, 1.0);
                            p.line(ax, ay, bx, by, Color::white(0.10), 1.0);
                            // The curve.
                            let accent = theme::accent();
                            let n = 64;
                            let mut last = at(0.0, curve.at(0.0));
                            for q in 1..=n {
                                let u = q as f32 / n as f32;
                                let next = at(u, curve.at(u));
                                p.line(last.0, last.1, next.0, next.1, accent, 2.2);
                                last = next;
                            }
                            // The pen now.
                            let live = crate::tablet::live();
                            if live.near {
                                let (lx, ly) = at(live.level.clamp(0.0, 1.0), live.pressure);
                                p.line(lx, gy + 2.0, lx, gy + gh - 2.0, accent.alpha(0.35), 1.0);
                                p.circle(lx, ly, 5.0, if live.touching { accent } else { Color::white(0.6) });
                            }
                            // The points to drag.
                            for (j, &(u, v)) in curve.points().iter().enumerate() {
                                let (px, py) = at(u, v);
                                let big = dragging == Some(j as u8);
                                p.circle(px, py, if big { 8.0 } else { 6.5 }, Color::white(0.95));
                                p.stroke_circle(px, py, if big { 8.0 } else { 6.5 }, accent, 2.0);
                            }
                            p.text("Lighter", &f.small, gx, gy + gh + 1.0, 80.0, 15.0, Align::Left, Color::white(0.4));
                            p.text(
                                "Harder",
                                &f.small,
                                gx + gw - 80.0,
                                gy + gh + 1.0,
                                80.0,
                                15.0,
                                Align::Right,
                                Color::white(0.4),
                            );
                            // What it's set to, and what the pen's doing.
                            let tx = gx + gw + 24.0;
                            let tw = (cx + cwid - tx).max(0.0);
                            let feel = match curve.firmness {
                                v if v < 0.75 => "Soft",
                                v if v > 1.35 => "Firm",
                                _ => "Even",
                            };
                            let lines = [
                                ("Starts drawing at", format!("{:.0}%", curve.tip * 100.0)),
                                ("Feel", format!("{feel} ({:.1}\u{d7})", curve.firmness)),
                                ("Full pressure at", format!("{:.0}%", curve.full * 100.0)),
                            ];
                            for (k, (name, value)) in lines.iter().enumerate() {
                                let ly = gy + 4.0 + k as f32 * 40.0;
                                p.text(name, &f.small, tx, ly, tw, 16.0, Align::Left, Color::white(0.55));
                                p.text(value, &f.body, tx, ly + 16.0, tw, 20.0, Align::Left, Color::white(0.92));
                            }
                            let now = if !live.near {
                                "Bring the pen to the tablet to see its pressure.".to_string()
                            } else if live.touching {
                                format!(
                                    "Pen: {:.0}% pressed, {:.0}% to apps",
                                    live.level * 100.0,
                                    live.pressure * 100.0
                                )
                            } else {
                                format!("Pen: hovering ({:.0}%)", live.level * 100.0)
                            };
                            p.text_wrap(&now, &f.small, tx, gy + 128.0, tw, 40.0, accent);
                        }
                        Ctl::PenPad => {
                            p.rrect(cx, cy, cwid, ch, 8.0, Color(0.0, 0.0, 0.0, 0.16));
                            p.stroke_rrect(cx + 0.5, cy + 0.5, cwid - 1.0, ch - 1.0, 8.0, Color::white(0.08), 1.0);
                            p.clip(cx, cy.max(TOP - 8.0), cwid, ch);
                            let pad = self.pad.borrow();
                            if pad.is_empty() {
                                p.text(
                                    "Draw here with the pen",
                                    &f.body,
                                    cx,
                                    cy,
                                    cwid,
                                    ch,
                                    Align::Center,
                                    Color::white(0.3),
                                );
                            }
                            let ink = Color::white(0.92);
                            for line in pad.iter() {
                                for w in line.windows(2) {
                                    let ((x1, y1, p1), (x2, y2, p2)) = (w[0], w[1]);
                                    let width = 0.8 + 9.0 * (p1 + p2) / 2.0;
                                    p.line(cx + x1, cy + y1, cx + x2, cy + y2, ink, width);
                                    p.circle(cx + x2, cy + y2, width / 2.0, ink);
                                }
                                if let Some(&(x1, y1, p1)) = line.first() {
                                    p.circle(cx + x1, cy + y1, (0.8 + 9.0 * p1) / 2.0, ink);
                                }
                            }
                            // Where the pen is over the pad.
                            let live = crate::tablet::live();
                            if live.near {
                                let mut pt = POINT { x: live.x, y: live.y };
                                unsafe {
                                    let _ = ScreenToClient(self.hwnd, &mut pt);
                                }
                                let (lx, ly) = (pt.x as f32 / self.k, pt.y as f32 / self.k);
                                if lx >= cx && lx < cx + cwid && ly >= cy && ly < cy + ch {
                                    p.stroke_circle(lx, ly, 3.0 + 6.0 * live.pressure, theme::accent(), 1.5);
                                }
                            }
                            p.unclip();
                            let (bx, by, bw, bh) = pad_clear((cx, cy, cwid, ch));
                            let lit = self.glow(Hit::Row(i));
                            p.rrect(bx, by, bw, bh, 5.0, Color::white(0.06 + 0.03 * lit));
                            p.stroke_rrect(bx + 0.5, by + 0.5, bw - 1.0, bh - 1.0, 5.0, Color::white(0.08), 1.0);
                            p.text("Clear", &f.body, bx, by, bw, bh, Align::Center, Color::white(0.9));
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
                    if grow < 1.0 {
                        p.unclip();
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
        assert_eq!(
            lanes,
            vec![vec!["weather", "ram"], vec!["cpu", "gpu"], vec!["music", "net", "desktops", "lang", "widgets"]]
        );
        let lanes = Board::Panel.read(&|_: &str| "stats,calendar,notifications".to_string());
        assert_eq!(lanes, vec![vec!["system", "calendar"], vec!["media", "weather", "clipboard", "phone"]]);
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
        assert!(keys(page::LOOK, &[("theme", "liquid_glass")]).contains(&"accent"));
        // The tablet's settings go with the driver.
        assert!(keys(page::DEVICES, &[("tablet", "true")]).contains(&"tablet_curve"));
        assert!(!keys(page::DEVICES, &[("tablet", "false")]).iter().any(|k| k.starts_with("tablet_")));
        assert_eq!(accent_index("windows"), Some(0));
        assert_eq!(accent_value(accent_index("#0078D4").unwrap()).as_deref(), Some("#0078D4"));
        // Widget settings follow where the widgets are.
        assert!(!keys(6, &[("taskbar_left", ""), ("taskbar_right", ""), ("widgets_panel", "calendar")])
            .contains(&"widget_visualizer"));
        assert!(keys(6, &[("taskbar_left", "music"), ("taskbar_right", "")]).contains(&"widget_visualizer"));
    }

    #[test]
    fn search_finds_settings_across_sections() {
        search("battery");
        let found = rows(SEARCH);
        let keys: Vec<_> = found.iter().map(|r| r.key).collect();
        assert!(keys.contains(&"battery_saver") && keys.contains(&"battery_percent"));
        // Each group starts with a heading naming its section.
        assert!(matches!(found[0].ctl, Ctl::Heading));
        assert!(found.iter().filter(|r| matches!(r.ctl, Ctl::Heading)).all(|r| SECTIONS.contains(&r.title)));
        // Every word has to match, in any order and any case.
        search("SAVER battery");
        assert!(rows(SEARCH).iter().any(|r| r.key == "battery_saver"));
        search("battery zzzzqq");
        assert_eq!(rows(SEARCH).iter().map(|r| r.key).collect::<Vec<_>>(), vec!["search-none"]);
        // Nothing typed, nothing listed.
        search("   ");
        assert!(rows(SEARCH).is_empty());
    }

    #[test]
    fn search_results_keep_their_own_sections_conditions() {
        search("battery percentage");
        let get = |set: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                set.iter().find(|(s, _)| *s == k).map_or_else(|| Config::default().get(k), |(_, v)| v.to_string())
            }
        };
        // On a laptop with the status icons showing, the percentage row is there...
        assert!(shown(SEARCH, "battery_percent", &get(&[("battery", "true"), ("show_status", "true")])));
        // ...and not without the icons, or without a battery.
        assert!(!shown(SEARCH, "battery_percent", &get(&[("battery", "true"), ("show_status", "false")])));
        assert!(!shown(SEARCH, "battery_percent", &get(&[("battery", "false"), ("show_status", "true")])));
        // The "nothing matches" note always shows.
        assert!(shown(SEARCH, "search-none", &get(&[])));
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
