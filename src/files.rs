//! Files (`cascade.exe --files [folder]`): Cascade's own file manager, so a
//! folder has somewhere to open with no Explorer at all. Tabs; back,
//! forward, up; an address box that takes paths, `shell:` names and "This
//! PC"; a search box that filters as you type and, on Enter, searches every
//! folder below (instantly with the file index on); a sidebar with your
//! folders and drives; details or icons (pictures and videos as thumbnails).
//!
//! It looks like Windows 11's File Explorer, down to the pixel where it can
//! be: Explorer's colours (measured from it) in its dark and light modes,
//! following Windows' app mode; its sizes at 100%; Segoe UI Variable at its
//! sizes; Windows' own icon font and the shell's icons (see `Pal`, `Type`).
//!
//! As in Explorer: click, Ctrl+click, Shift+click and drag a box to select;
//! double-click or Enter to open; F2 renames; Delete recycles (Shift for
//! good); Ctrl+C, X, V, A; Ctrl+T, W, Tab for tabs; Alt+Left, Right, Up,
//! Backspace; Ctrl+L, F4 for the address; Ctrl+F, F3 to search; F5;
//! Ctrl+Shift+N for a new folder; typing jumps to a name. Right-click an item
//! for the shell's own menu (Open with, Send to, 7-Zip...) and the
//! background for New, Paste, Sort, View, Terminal and Properties. Files can
//! be dragged in and out, and the folder follows changes on disk.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use windows::core::{implement, w, Ref, Result as WinResult, BOOL, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::appwin::{self, size_text};
use crate::filesio::{self, Item, Loc};
use crate::gfx::{Align, Color, Gfx, Painter};
use crate::{msg, theme};

// Sizes as Windows 11's File Explorer has them (measured from it at 100%).
const TABS_H: f32 = 40.0;
/// The tab in front: from here down to the address row.
const TAB_TOP: f32 = 8.0;
const BAR_H: f32 = 48.0;
const CMD_H: f32 = 48.0;
const SIDE_W: f32 = 230.0;
/// The navigation pane's rows.
const SIDE_ROW: f32 = 32.0;
/// The details pane on the right.
const PANE_W: f32 = 290.0;
const HEAD_H: f32 = 30.0;
const ROW_H: f32 = 28.0;
/// Rows in compact view.
const ROW_COMPACT: f32 = 22.0;
const STATUS_H: f32 = 26.0;
const TAB_W: f32 = 240.0;
/// The minimise, maximise and close buttons' height.
const CAP_H: f32 = 30.0;
/// The buttons in the address row and the command bar: their height, and
/// the width of one with only an icon.
const BTN_H: f32 = 32.0;
const BTN_W: f32 = 40.0;
/// The arrow after each part of the path in the address bar.
const CHEV: f32 = 26.0;

/// `0xRRGGBB` as a colour.
const fn rgb(c: u32) -> Color {
    Color(((c >> 16) & 0xFF) as f32 / 255.0, ((c >> 8) & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0, 1.0)
}

/// Explorer's colours, dark or light as Windows' app mode is (Settings >
/// Personalisation > Colours), measured from Explorer itself.
#[derive(Clone, Copy)]
struct Pal {
    dark: bool,
    /// The tab row (Mica: tinted by the wallpaper while the window's active).
    strip: Color,
    /// The tab in front and the address row under it.
    layer: Color,
    /// The address and search boxes.
    field: Color,
    field_edge: Color,
    /// The command bar, the navigation pane, the list and the status bar.
    base: Color,
    /// The lines under the address row and the command bar.
    line: Color,
    /// The lines between groups of commands, of tabs and in the navigation pane.
    sep: Color,
    /// The lines between the columns' titles.
    head_sep: Color,
    text: Color,
    /// Dates, types, sizes, the columns' titles.
    text2: Color,
    /// Greyed (a button that can't be used now).
    text_off: Color,
    /// Over a button.
    hover: Color,
    /// Rows and navigation pane entries: under the pointer, selected, both,
    /// and selected in a window that isn't in front.
    row_hover: Color,
    row_sel: Color,
    row_sel_hover: Color,
    row_sel_idle: Color,
    /// An edge round a selected row (light mode's blue outline), if any.
    row_edge: Option<Color>,
    /// Group by's headings.
    group: Color,
    scroll: Color,
}

const DARK: Pal = Pal {
    dark: true,
    strip: rgb(0x202020),
    layer: rgb(0x2C2C2C),
    field: rgb(0x383838),
    field_edge: Color(1.0, 1.0, 1.0, 0.03),
    base: rgb(0x191919),
    line: rgb(0x3A3A3A),
    sep: rgb(0x2E2E2E),
    head_sep: rgb(0x5A5A5A),
    text: rgb(0xFFFFFF),
    text2: rgb(0xD0D0D0),
    text_off: rgb(0x858585),
    hover: Color(1.0, 1.0, 1.0, 0.06),
    row_hover: rgb(0x2A2A2A),
    row_sel: rgb(0x333333),
    row_sel_hover: rgb(0x3D3D3D),
    row_sel_idle: rgb(0x2D2D2D),
    row_edge: None,
    group: rgb(0xFFFFFF),
    scroll: Color(1.0, 1.0, 1.0, 0.4),
};

const LIGHT: Pal = Pal {
    dark: false,
    strip: rgb(0xEEEEEE),
    layer: rgb(0xF9F9F9),
    field: rgb(0xFFFFFF),
    field_edge: rgb(0xE5E5E5),
    base: rgb(0xFFFFFF),
    line: rgb(0xE5E5E5),
    sep: rgb(0xE5E5E5),
    head_sep: rgb(0xE5E5E5),
    text: rgb(0x1A1A1A),
    text2: rgb(0x5D5D5D),
    text_off: rgb(0xA0A0A0),
    hover: Color(0.0, 0.0, 0.0, 0.04),
    row_hover: rgb(0xE5F3FF),
    row_sel: rgb(0xCCE8FF),
    row_sel_hover: rgb(0xCCE8FF),
    row_sel_idle: rgb(0xD9D9D9),
    row_edge: Some(rgb(0x99D1FF)),
    group: rgb(0x1E3287),
    scroll: Color(0.0, 0.0, 0.0, 0.45),
};

impl Pal {
    /// Windows' app mode now (light unless apps are set to dark).
    fn current() -> Self {
        use windows::Win32::System::Registry::HKEY_CURRENT_USER;
        let light = crate::winset::reg_dword(
            HKEY_CURRENT_USER,
            "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
            "AppsUseLightTheme",
        );
        if light == Some(0) {
            DARK
        } else {
            LIGHT
        }
    }

    fn colorref(c: Color) -> COLORREF {
        let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        COLORREF(b(c.0) | (b(c.1) << 8) | (b(c.2) << 16))
    }
}

/// Mica, as Windows draws it behind Explorer's tabs: the plain colour with a
/// hint of the wallpaper's colour (its hue, not its brightness). Mica is
/// blurred so far that it's all but one colour, so one colour, the
/// wallpaper's average, stands in for it.
fn mica(base: Color, strength: f32) -> Color {
    let Some(px) = crate::wallpaper::thumbnail(8, 8) else { return base };
    let n = (px.len() / 4).max(1) as f32;
    let (mut r, mut g, mut b) = (0.0f32, 0.0f32, 0.0f32);
    for p in px.chunks_exact(4) {
        b += p[0] as f32;
        g += p[1] as f32;
        r += p[2] as f32;
    }
    let (r, g, b) = (r / n / 255.0, g / n / 255.0, b / n / 255.0);
    let l = 0.299 * r + 0.587 * g + 0.114 * b;
    let tint = |base: f32, c: f32| (base + (c - l) * strength).clamp(0.0, 1.0);
    Color(tint(base.0, r), tint(base.1, g), tint(base.2, b), 1.0)
}

/// The type Files draws with, at Explorer's sizes: 12 px for almost
/// everything, 14 px for the path and the search box; Windows' icon font at
/// the sizes Explorer's buttons use.
struct Type {
    t12: IDWriteTextFormat,
    t14: IDWriteTextFormat,
    i10: Option<IDWriteTextFormat>,
    i12: Option<IDWriteTextFormat>,
    i16: Option<IDWriteTextFormat>,
}

impl Type {
    fn new(gfx: &Gfx) -> Self {
        Type {
            t12: gfx.font(12.0, false).unwrap_or_else(|| gfx.fonts.body.clone()),
            t14: gfx.font(14.0, false).unwrap_or_else(|| gfx.fonts.body.clone()),
            i10: gfx.icon_font(10.0),
            i12: gfx.icon_font(12.0),
            i16: gfx.icon_font(16.0).or_else(|| gfx.fonts.icons.clone()),
        }
    }
}

const LOADED: u32 = WM_APP + 101;
const ICONS: u32 = WM_APP + 102;
const WATCHED: u32 = WM_APP + 103;
const EDIT_KEY: u32 = WM_APP + 104;
const ID_ADDR: usize = 10;
const ID_SEARCH: usize = 11;
const ID_RENAME: usize = 12;

/// How a folder is shown, kept for next time (see `ViewStore`).
#[derive(Clone, Copy, PartialEq)]
struct FolderView {
    view: View,
    sort: (usize, bool),
    group: u8,
}

const DEFAULT_VIEW: FolderView = FolderView { view: View::Details, sort: (0, true), group: 0 };

/// What Files remembers between runs, in `files_view.ini` beside the config:
/// each folder's view, sorting and grouping (as Explorer keeps them per
/// folder), and, for every folder, hidden items and the columns' widths.
/// Lines: `hidden=1`, `colfrac=0.4,0.2,0.2,0.2` (each column's share of the width) and
/// `f|<folder, lower case>|details|<sort column>|<ascending>|<group>`.
#[derive(Default)]
struct ViewStore {
    folders: HashMap<String, FolderView>,
    hidden: bool,
    /// Each column's share of the width (Name, Date modified, Type, Size).
    cols: Option<[f32; 4]>,
    /// How big icons are drawn in Icons view (1 = medium).
    scale: Option<f32>,
    /// The folders pinned in the sidebar (None: the usual six).
    pins: Option<Vec<String>>,
    /// The navigation pane is hidden, the details pane shown, rows packed
    /// closer (compact view), and file name extensions (0 as Windows has
    /// them, 1 shown, 2 hidden).
    hide_nav: bool,
    details: bool,
    compact: bool,
    exts: u8,
}

/// The narrowest each column goes (Name, Date modified, Type, Size).
const COL_MIN: [f32; 4] = [120.0, 80.0, 70.0, 90.0];

/// The columns' usual shares of the width.
const COL_SHARES: [f32; 4] = [0.40, 0.22, 0.18, 0.20];

/// `shares` scaled to add up to 1, or nothing if they aren't sensible.
fn normalised(shares: [f32; 4]) -> Option<[f32; 4]> {
    let sum: f32 = shares.iter().sum();
    (shares.iter().all(|s| s.is_finite() && *s > 0.0) && sum > 0.0).then(|| shares.map(|s| s / sum))
}

/// Widths for `shares` over `total` px: each its share, none under its
/// minimum, the rest of them giving way for that so they still add up to
/// `total` (as far as the minimums allow).
fn fit_columns(shares: [f32; 4], total: f32) -> [f32; 4] {
    let mut w = shares.map(|s| s * total);
    for _ in 0..4 {
        let mut short = 0.0;
        for (x, m) in w.iter_mut().zip(COL_MIN) {
            if *x < m {
                short += m - *x;
                *x = m;
            }
        }
        if short <= 0.01 {
            break;
        }
        // Those with room to spare pay for it, in proportion to the room.
        let room: f32 = w.iter().zip(COL_MIN).map(|(x, m)| x - m).sum();
        if room <= 0.0 {
            break;
        }
        for (x, m) in w.iter_mut().zip(COL_MIN) {
            *x -= short * (*x - m) / room;
        }
    }
    w
}

fn store_path() -> std::path::PathBuf {
    crate::config::dir().join("files_view.ini")
}

/// What a place is remembered by (none for search results).
fn folder_key(loc: &Loc) -> Option<String> {
    match loc {
        Loc::Home => Some("home".into()),
        Loc::Path(p) => Some(p.trim_end_matches('\\').to_lowercase()),
        Loc::Shell(_, name) => Some(format!("shell:{}", name.to_lowercase())),
        Loc::Search(..) => None,
    }
}

impl ViewStore {
    fn parse(text: &str) -> Self {
        let mut s = ViewStore::default();
        for line in text.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("hidden=") {
                s.hidden = v.trim() == "1";
            } else if let Some(v) = line.strip_prefix("colfrac=") {
                let n: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if let [a, b, c, d] = n[..] {
                    s.cols = normalised([a, b, c, d]);
                }
            } else if let Some(v) = line.strip_prefix("scale=") {
                s.scale =
                    v.trim().parse::<f32>().ok().map(|n| n.clamp(ICON_STEPS[0], ICON_STEPS[ICON_STEPS.len() - 1]));
            } else if let Some(v) = line.strip_prefix("nav=") {
                s.hide_nav = v.trim() == "0";
            } else if let Some(v) = line.strip_prefix("details=") {
                s.details = v.trim() == "1";
            } else if let Some(v) = line.strip_prefix("compact=") {
                s.compact = v.trim() == "1";
            } else if let Some(v) = line.strip_prefix("exts=") {
                s.exts = v.trim().parse::<u8>().ok().filter(|n| *n <= 2).unwrap_or(0);
            } else if line == "pins=1" {
                s.pins.get_or_insert_with(Vec::new);
            } else if let Some(v) = line.strip_prefix("pin|") {
                if !v.is_empty() {
                    s.pins.get_or_insert_with(Vec::new).push(v.to_string());
                }
            } else if let Some(v) = line.strip_prefix("f|") {
                let p: Vec<&str> = v.split('|').collect();
                if let [path, view, col, asc, group] = p[..] {
                    let (Ok(col), Ok(group)) = (col.parse::<usize>(), group.parse::<u8>()) else { continue };
                    if col >= COLS.len() || group > 3 {
                        continue;
                    }
                    let view = if view == "icons" { View::Icons } else { View::Details };
                    s.folders.insert(path.to_string(), FolderView { view, sort: (col, asc == "1"), group });
                }
            }
        }
        s
    }

    fn text(&self) -> String {
        let mut out = String::from("# Files: how each folder is shown. Written by Cascade.\n");
        out.push_str(&format!("hidden={}\n", self.hidden as u8));
        if let Some(c) = self.cols {
            out.push_str(&format!("colfrac={},{},{},{}\n", c[0], c[1], c[2], c[3]));
        }
        out.push_str(&format!("nav={}\n", !self.hide_nav as u8));
        out.push_str(&format!("details={}\n", self.details as u8));
        out.push_str(&format!("compact={}\n", self.compact as u8));
        out.push_str(&format!("exts={}\n", self.exts));
        if let Some(k) = self.scale {
            out.push_str(&format!("scale={k}\n"));
        }
        if let Some(pins) = &self.pins {
            out.push_str("pins=1\n");
            for pin in pins {
                out.push_str(&format!("pin|{pin}\n"));
            }
        }
        let mut folders: Vec<_> = self.folders.iter().collect();
        folders.sort_by(|a, b| a.0.cmp(b.0));
        for (path, v) in folders {
            let view = if v.view == View::Icons { "icons" } else { "details" };
            out.push_str(&format!("f|{path}|{view}|{}|{}|{}\n", v.sort.0, v.sort.1 as u8, v.group));
        }
        out
    }

    fn load() -> Self {
        std::fs::read_to_string(store_path()).map(|t| Self::parse(&t)).unwrap_or_default()
    }

    fn save(&self) {
        let path = store_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, self.text());
    }
}

/// The sizes icons step through (Ctrl+wheel, Ctrl+plus and minus).
const ICON_STEPS: [f32; 5] = [0.65, 0.85, 1.0, 1.4, 1.9];
/// How long a refresh shows as busy at least, so it can be seen.
const BUSY_MIN: std::time::Duration = std::time::Duration::from_millis(600);
const TIMER_BUSY: usize = 7;

const COLS: [(&str, f32); 4] = [("Name", 0.0), ("Date modified", 170.0), ("Type", 170.0), ("Size", 100.0)];

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Details,
    Icons,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Tab(usize),
    TabClose(usize),
    NewTab,
    Back,
    Forward,
    Up,
    Refresh,
    Cmd(usize),
    /// Minimise, maximise, close (the window has no title bar of its own).
    Caption(u8),
    Side(usize),
    Header(usize),
    /// The edge between header `i` and the next (drag to resize column `i`).
    HeaderEdge(usize),
    /// A part of the path in the address bar, and the bar around them.
    Crumb(usize),
    /// The arrow after a part of the path: what's inside it, to jump to.
    CrumbMenu(usize),
    /// The arrow after the place's icon at the start of the path: the places
    /// in the navigation pane, to jump to.
    CrumbRoot,
    /// The "..." standing for the first parts of a path too long to show.
    CrumbMore,
    /// The arrow before This PC in the navigation pane: folds its drives away.
    SideFold,
    /// The status bar's view buttons: Details, Large icons.
    ViewButton(u8),
    Address,
    Item(usize),
    /// The details pane.
    Pane,
    Empty,
}

/// How far a group heading takes (Group by).
const GROUP_H: f32 = 34.0;

/// The pictures a frame draws, looked up before drawing starts: each shown
/// item's icon (by position), the navigation pane's, each tab's, the
/// address bar's first, and the details pane's.
struct Pics {
    items: Vec<(usize, Option<ID2D1Bitmap>)>,
    side: Vec<Option<ID2D1Bitmap>>,
    tabs: Vec<Option<ID2D1Bitmap>>,
    root: Option<ID2D1Bitmap>,
    pane: Option<ID2D1Bitmap>,
}

/// Where the status bar's view button `k` (0 Details, 1 Large icons) is,
/// at the right as Explorer has them.
fn status_view_x(w: f32, k: u8) -> f32 {
    w - 10.0 - (2 - k) as f32 * 26.0
}

/// The path as the address bar shows it: each folder on the way, to click.
fn crumbs(loc: &Loc) -> Vec<(String, Loc)> {
    match loc {
        Loc::Path(p) if !p.starts_with("\\\\") => {
            let mut out = vec![("This PC".to_string(), filesio::this_pc())];
            let mut acc = String::new();
            for (k, part) in p.trim_end_matches('\\').split('\\').enumerate() {
                if part.is_empty() {
                    continue;
                }
                if k == 0 {
                    acc = format!("{part}\\");
                    out.push((Loc::Path(acc.clone()).title(), Loc::Path(acc.clone())));
                } else {
                    if !acc.ends_with('\\') {
                        acc.push('\\');
                    }
                    acc.push_str(part);
                    out.push((part.to_string(), Loc::Path(acc.clone())));
                }
            }
            out
        }
        other => vec![(other.title(), other.clone())],
    }
}

/// Which group an item goes in (Group by: 1 type, 2 date modified, 3
/// size): its place among the groups and the group's name.
fn group_of(item: &filesio::Item, by: u8, now: u64) -> (u32, String) {
    match by {
        1 if item.folder => (0, "Folders".into()),
        1 => (1, if item.kind.is_empty() { "Other".into() } else { item.kind.clone() }),
        2 => {
            if item.modified == 0 {
                return (9, "Unknown".into());
            }
            let days = now.saturating_sub(item.modified) / 864_000_000_000;
            let (rank, name) = match days {
                0 => (0, "Today"),
                1 => (1, "Yesterday"),
                2..=6 => (2, "Earlier this week"),
                7..=13 => (3, "Last week"),
                14..=30 => (4, "Earlier this month"),
                31..=61 => (5, "Last month"),
                62..=365 => (6, "Earlier this year"),
                _ => (7, "A long time ago"),
            };
            (rank, name.into())
        }
        3 if item.folder => (0, "Folders".into()),
        3 => {
            let s = item.size.unwrap_or(0);
            let (rank, name) = match s {
                0 => (1, "Empty"),
                1..=16_383 => (2, "Tiny (under 16 KB)"),
                16_384..=1_048_575 => (3, "Small (16 KB to 1 MB)"),
                1_048_576..=134_217_727 => (4, "Medium (1 to 128 MB)"),
                134_217_728..=1_073_741_823 => (5, "Large (128 MB to 1 GB)"),
                1_073_741_824..=4_294_967_295 => (6, "Huge (1 to 4 GB)"),
                _ => (7, "Gigantic (over 4 GB)"),
            };
            (rank, name.into())
        }
        _ => (0, String::new()),
    }
}

struct Tab {
    loc: Loc,
    items: Vec<Item>,
    /// Item indexes in the order shown (sorted and filtered).
    order: Vec<usize>,
    sel: Vec<bool>,
    /// The last clicked position in `order` (Shift+click and arrows go from it).
    anchor: Option<usize>,
    scroll: f32,
    back: Vec<Loc>,
    forward: Vec<Loc>,
    loading: bool,
    generation: u32,
    filter: String,
    /// Group headings (Group by): where each group starts in `order`, its
    /// name, and how many are in it.
    heads: Vec<(usize, String, usize)>,
}

impl Tab {
    fn new(loc: Loc) -> Self {
        Tab {
            loc,
            items: Vec::new(),
            order: Vec::new(),
            sel: Vec::new(),
            anchor: None,
            scroll: 0.0,
            back: Vec::new(),
            forward: Vec::new(),
            loading: true,
            generation: 0,
            filter: String::new(),
            heads: Vec::new(),
        }
    }

    fn selected(&self) -> Vec<usize> {
        self.order.iter().copied().filter(|&i| self.sel.get(i) == Some(&true)).collect()
    }

    fn selected_paths(&self) -> Vec<String> {
        self.selected().into_iter().map(|i| self.items[i].path.clone()).collect()
    }
}

struct State {
    hwnd: HWND,
    /// Taken out while painting (the drawing reads the rest of the state).
    gfx: Option<Gfx>,
    tabs: Vec<Tab>,
    cur: usize,
    view: View,
    sort: (usize, bool),
    show_hidden: bool,
    icons: HashMap<String, Option<ID2D1Bitmap>>,
    asked: HashSet<String>,
    icon_tx: std::sync::mpsc::Sender<(String, String, bool)>,
    icon_done: filesio::Arrived,
    addr: HWND,
    search: HWND,
    font: HFONT,
    brush: HBRUSH,
    hovered: Option<Hit>,
    /// A box being dragged out: start and now (window coordinates).
    band: Option<((f32, f32), (f32, f32))>,
    /// A press on an item that may become a drag out.
    press: Option<(f32, f32, usize)>,
    rename: Option<(HWND, String)>,
    last_click: Option<(usize, Instant)>,
    side: Vec<(char, String, Loc)>,
    typed: (String, Instant),
    gfx_generation: u32,
    /// Each column's share of the width (Name, Date modified, Type, Size), and
    /// an edge being dragged: (column, where the drag began, the widths then).
    col_share: [f32; 4],
    col_drag: Option<(usize, f32, [f32; 4])>,
    /// The address box is being typed in (else the path shows as parts to click).
    editing_addr: bool,
    /// Group by: 0 none, 1 type, 2 date modified, 3 size.
    group: u8,
    measure: Option<crate::gfx::Measure>,
    /// What's remembered of how folders are shown.
    store: ViewStore,
    /// How big icons are drawn in Icons view (1 = medium).
    icon_scale: f32,
    /// When a refresh was asked for (it shows as busy for a moment, and while it loads).
    refresh_at: Option<Instant>,
    /// How many folders are pinned in the sidebar (after Home).
    pin_n: usize,
    /// The navigation pane (the sidebar), the details pane, compact rows.
    nav: bool,
    details: bool,
    compact: bool,
    /// File name extensions: 0 as Windows has them, 1 shown, 2 hidden.
    exts: u8,
    /// What can be undone and redone (renames and new items).
    undo: Vec<Undo>,
    redo: Vec<Undo>,
    /// Explorer's colours (light or dark, as Windows' app mode is) and type.
    pal: Pal,
    ty: Type,
    /// The window's in front (its tab row shows Mica, its selection is bright).
    active: bool,
    /// This PC's drives are shown in the navigation pane (its arrow folds them).
    side_open: bool,
}

/// A change Ctrl+Z can take back.
#[derive(Clone, Debug, PartialEq)]
enum Undo {
    /// An item renamed: where it was, where it is now.
    Rename(String, String),
    /// A new, empty folder or file (undone by deleting it).
    Created(String),
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Loads finished on worker threads: (generation, items).
static LOADS: Mutex<Vec<(u32, Vec<Item>)>> = Mutex::new(Vec::new());
/// Folder sizes worked out: (listing, folder, size).
static SIZES: Mutex<Vec<(u32, String, u64)>> = Mutex::new(Vec::new());
/// The listing whose folders are being sized (an older sizing stops).
static SIZING: AtomicU32 = AtomicU32::new(0);
const SIZED: u32 = WM_APP + 105;
/// The tab row's Mica colour has been worked out (see `mica`).
const MICA: u32 = WM_APP + 106;
/// The tab row's Mica colour, once worked out: (for dark mode, the colour).
static MICA_TINT: Mutex<Option<(bool, Color)>> = Mutex::new(None);
static NEXT_GEN: AtomicU32 = AtomicU32::new(1);
/// The folder being watched for changes (its generation; a newer one stops the old watcher).
static WATCH_GEN: AtomicU32 = AtomicU32::new(0);

fn glyph(code: u32) -> String {
    char::from_u32(code).map(String::from).unwrap_or_default()
}

/// The sidebar: your folders, This PC and each drive, the Recycle Bin, Network.
/// The folders pinned in the sidebar until you change them: Desktop,
/// Downloads, Documents, Pictures, Music and Videos.
fn default_pins() -> Vec<String> {
    let mut out = Vec::new();
    for (id, _) in known_folders() {
        if let Some(p) = known_path(id) {
            out.push(p);
        }
    }
    out
}

fn known_folders() -> [(&'static windows::core::GUID, char); 6] {
    [
        (&FOLDERID_Desktop, '\u{E8FC}'),
        (&FOLDERID_Downloads, '\u{E896}'),
        (&FOLDERID_Documents, '\u{E8A5}'),
        (&FOLDERID_Pictures, '\u{E91B}'),
        (&FOLDERID_Music, '\u{E8D6}'),
        (&FOLDERID_Videos, '\u{E714}'),
    ]
}

fn known_path(id: &windows::core::GUID) -> Option<String> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let path = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        path
    }
}

/// A pinned folder's icon: its own for Windows' usual folders, else a folder.
fn pin_glyph(path: &str) -> char {
    for (id, g) in known_folders() {
        if known_path(id).is_some_and(|k| k.eq_ignore_ascii_case(path)) {
            return g;
        }
    }
    '\u{E8B7}'
}

fn sidebar(pins: &[String]) -> Vec<(char, String, Loc)> {
    let mut out = vec![('\u{E80F}', "Home".to_string(), Loc::Home)];
    for path in pins {
        let name = path.trim_end_matches('\\').rsplit('\\').next().unwrap_or(path).to_string();
        let name = if name.is_empty() { path.clone() } else { name };
        out.push((pin_glyph(path), name, Loc::Path(path.clone())));
    }
    out.push(('\u{E7F8}', "This PC".into(), filesio::this_pc()));
    let mask = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
    for i in 0..26u8 {
        if mask & (1 << i) != 0 {
            let l = (b'A' + i) as char;
            let root = format!("{l}:\\");
            let kind = unsafe { windows::Win32::Storage::FileSystem::GetDriveTypeW(&HSTRING::from(root.as_str())) };
            if matches!(kind, 2..=4) {
                out.push(('\u{EDA2}', format!("{l}:"), Loc::Path(root)));
            }
        }
    }
    // Phones and cameras plugged in (as Explorer lists them under This PC).
    for (name, parse) in filesio::portable_devices() {
        out.push(('\u{E8EA}', name.clone(), Loc::Shell(parse, name)));
    }
    out.push((
        '\u{E74D}',
        "Recycle Bin".into(),
        Loc::Shell("::{645FF040-5081-101B-9F08-00AA002F954E}".into(), "Recycle Bin".into()),
    ));
    out.push((
        '\u{E968}',
        "Network".into(),
        Loc::Shell("::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}".into(), "Network".into()),
    ));
    out
}

/// The command bar, as Explorer's: (glyph, label).
const CMDS: [(u32, &str); 11] = [
    (0xECC8, "New"),
    (0xE8C6, "Cut"),
    (0xE8C8, "Copy"),
    (0xE77F, "Paste"),
    (0xE8AC, "Rename"),
    (0xE72D, "Share"),
    (0xE74D, "Delete"),
    (0xE8CB, "Sort"),
    (0xE8FD, "View"),
    (0xE712, "More"),
    (0xE89F, "Details"),
];

/// Where a new group of commands starts (a line before it).
fn starts_group(i: usize) -> bool {
    matches!(i, 1 | 7 | 9)
}

/// Commands that show their label: New, Sort and View (with an arrow, as
/// they open a menu) and Details.
fn labelled(i: usize) -> bool {
    matches!(i, 0 | 7 | 8 | 10)
}

/// Commands with an arrow after their label.
fn dropdown(i: usize) -> bool {
    matches!(i, 0 | 7 | 8)
}

impl State {
    fn tab(&mut self) -> &mut Tab {
        &mut self.tabs[self.cur]
    }

    fn t(&self) -> &Tab {
        &self.tabs[self.cur]
    }

    fn content(&self) -> (f32, f32, f32, f32) {
        let (w, h) = appwin::client(self.hwnd);
        let y0 = TABS_H + BAR_H + CMD_H;
        let (side, pane) = (self.side_w(), self.pane_w());
        (side, y0, (w - side - pane).max(120.0), h - y0 - STATUS_H)
    }

    /// The navigation pane's width (none when it's hidden).
    fn side_w(&self) -> f32 {
        if self.nav {
            SIDE_W
        } else {
            0.0
        }
    }

    /// The details pane's width (none when it's hidden).
    fn pane_w(&self) -> f32 {
        if self.details {
            PANE_W
        } else {
            0.0
        }
    }

    /// A row's height in Details view.
    fn row_h(&self) -> f32 {
        if self.compact {
            ROW_COMPACT
        } else {
            ROW_H
        }
    }

    fn rows_top(&self) -> f32 {
        let (_, y0, _, _) = self.content();
        if self.view == View::Details {
            y0 + HEAD_H
        } else {
            y0 + 8.0
        }
    }

    /// A tile's size in Icons view: it grows with the icon.
    fn tile_w(&self) -> f32 {
        40.0 + 72.0 * self.icon_scale
    }

    fn tile_h(&self) -> f32 {
        52.0 + 72.0 * self.icon_scale
    }

    fn cols(&self) -> usize {
        let (_, _, cw, _) = self.content();
        ((cw - 16.0) / self.tile_w()).floor().max(1.0) as usize
    }

    /// Where item `pos` (in display order) is: (x, y, w, h), before scrolling.
    fn item_rect(&self, pos: usize) -> (f32, f32, f32, f32) {
        let (x0, _, cw, _) = self.content();
        let top = self.rows_top();
        match self.view {
            View::Details => (
                x0 + 4.0,
                top + pos as f32 * self.row_h() + self.heads_before(pos) as f32 * GROUP_H,
                cw - 20.0,
                self.row_h(),
            ),
            View::Icons => {
                let c = self.cols();
                let (tw, th) = (self.tile_w(), self.tile_h());
                let (col, y) = self.icon_cell(pos, c);
                (x0 + 8.0 + col as f32 * tw, top + y, tw - 6.0, th - 6.0)
            }
        }
    }

    /// Icons view: item `pos`'s column and its distance below the first row,
    /// with `c` tiles to a row. Each group (Group by) starts on a row of its
    /// own, under its heading.
    fn icon_cell(&self, pos: usize, c: usize) -> (usize, f32) {
        let th = self.tile_h();
        let heads = &self.t().heads;
        if heads.is_empty() {
            return (pos % c, (pos / c) as f32 * th);
        }
        let k = heads.partition_point(|h| h.0 <= pos).saturating_sub(1);
        let within = pos - heads[k].0;
        let rows_before: usize = heads[..k].iter().map(|h| h.2.div_ceil(c)).sum();
        (within % c, (k + 1) as f32 * GROUP_H + (rows_before + within / c) as f32 * th)
    }

    /// Where group `k`'s heading is, below the first row (either view).
    fn head_y(&self, k: usize) -> f32 {
        let heads = &self.t().heads;
        match self.view {
            View::Details => heads[k].0 as f32 * self.row_h() + k as f32 * GROUP_H,
            View::Icons => {
                let c = self.cols();
                let rows: usize = heads[..k].iter().map(|h| h.2.div_ceil(c)).sum();
                k as f32 * GROUP_H + rows as f32 * self.tile_h()
            }
        }
    }

    fn content_height(&self) -> f32 {
        let n = self.t().order.len();
        match self.view {
            View::Details => n as f32 * self.row_h() + self.t().heads.len() as f32 * GROUP_H + 8.0,
            View::Icons => {
                let c = self.cols();
                let heads = &self.t().heads;
                let rows = if heads.is_empty() { n.div_ceil(c) } else { heads.iter().map(|h| h.2.div_ceil(c)).sum() };
                rows as f32 * self.tile_h() + heads.len() as f32 * GROUP_H + 8.0
            }
        }
    }

    /// The tile above or below `pos` in Icons view with groups: the one in the
    /// same column in the next row, which may be in the next group (or the
    /// group's last tile if its last row is shorter).
    fn tile_beside(&self, pos: usize, down: bool) -> usize {
        let c = self.cols();
        let (col, y) = self.icon_cell(pos, c);
        let n = self.t().order.len();
        let mut best: Option<(f32, usize)> = None;
        for p in 0..n {
            let (pc, py) = self.icon_cell(p, c);
            if (down && py > y) || (!down && py < y) {
                let key = (py - y).abs() * 100.0 + (pc as f32 - col as f32).abs();
                if best.is_none_or(|b| key < b.0) {
                    best = Some((key, p));
                }
            }
        }
        best.map_or(pos, |b| b.1)
    }

    /// Group headings at or before item `pos` (Details only).
    fn heads_before(&self, pos: usize) -> usize {
        if self.view != View::Details {
            return 0;
        }
        self.t().heads.partition_point(|h| h.0 <= pos)
    }

    /// Column `i`'s width.
    fn col_width(&self, i: usize, cw: f32) -> f32 {
        self.col_widths(cw)[i]
    }

    /// The four columns' widths: they always fill the row, whatever the
    /// window's width, each keeping its share (and its minimum).
    fn col_widths(&self, cw: f32) -> [f32; 4] {
        fit_columns(self.col_share, (cw - 20.0).max(COL_MIN.iter().sum()))
    }

    fn max_scroll(&self) -> f32 {
        let (_, y0, _, ch) = self.content();
        let visible = ch - (self.rows_top() - y0);
        (self.content_height() - visible).max(0.0)
    }

    fn set_title(&self) {
        let t = format!("{} \u{2014} Files", self.t().loc.title());
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(t));
            let _ = SetWindowTextW(self.addr, &HSTRING::from(self.t().loc.address()));
            // EM_SETCUEBANNER: "Search Downloads" in the empty search box, as Explorer says it.
            let cue = HSTRING::from(format!("Search {}", self.t().loc.title()));
            SendMessageW(self.search, 0x1501, Some(WPARAM(1)), Some(LPARAM(cue.as_ptr() as isize)));
        }
    }

    // ------------------------------------------------------- navigating --

    /// The view, sorting and grouping this folder was left with (the plain
    /// ones for a folder not seen before). Search results keep what's set.
    fn apply_view(&mut self) {
        let Some(key) = folder_key(&self.t().loc) else { return };
        let v = self.store.folders.get(&key).copied().unwrap_or(DEFAULT_VIEW);
        self.view = v.view;
        self.sort = v.sort;
        self.group = v.group;
    }

    /// Keeps how the current folder is shown, and Files' own settings (hidden
    /// items, column widths), for next time.
    fn remember(&mut self) {
        let here = FolderView { view: self.view, sort: self.sort, group: self.group };
        // Other Files windows (or one from before an update) write this file
        // too: start from what's on disk so theirs aren't written over.
        let pins = self.store.pins.take();
        self.store = ViewStore::load();
        if pins.is_some() {
            self.store.pins = pins;
        }
        if let Some(key) = folder_key(&self.t().loc) {
            if here == DEFAULT_VIEW {
                self.store.folders.remove(&key);
            } else {
                self.store.folders.insert(key, here);
            }
        }
        self.store.hidden = self.show_hidden;
        self.store.cols = Some(self.col_share);
        self.store.scale = Some(self.icon_scale);
        self.store.hide_nav = !self.nav;
        self.store.details = self.details;
        self.store.compact = self.compact;
        self.store.exts = self.exts;
        self.store.save();
    }

    fn load(&mut self) {
        self.apply_view();
        let generation = NEXT_GEN.fetch_add(1, Ordering::AcqRel);
        let hidden = self.show_hidden;
        let tab = self.tab();
        tab.generation = generation;
        tab.loading = true;
        let loc = tab.loc.clone();
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let items = filesio::load(&loc, hidden);
            if let Ok(mut l) = LOADS.lock() {
                l.push((generation, items));
            }
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), LOADED, WPARAM(0), LPARAM(0));
        });
        self.watch();
        self.set_title();
        appwin::redraw(self.hwnd);
    }

    /// Follows the current folder on disk, reloading when it changes.
    fn watch(&self) {
        use windows::Win32::Storage::FileSystem::*;
        use windows::Win32::System::Threading::WaitForSingleObject;
        let generation = WATCH_GEN.fetch_add(1, Ordering::AcqRel) + 1;
        let Some(dir) = self.t().loc.dir() else { return };
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || unsafe {
            let filter = FILE_NOTIFY_CHANGE_FILE_NAME
                | FILE_NOTIFY_CHANGE_DIR_NAME
                | FILE_NOTIFY_CHANGE_SIZE
                | FILE_NOTIFY_CHANGE_LAST_WRITE
                | FILE_NOTIFY_CHANGE_ATTRIBUTES;
            let Ok(h) = FindFirstChangeNotificationW(&HSTRING::from(dir.as_str()), false, filter) else { return };
            while WATCH_GEN.load(Ordering::Acquire) == generation {
                if WaitForSingleObject(h, 1000).0 == 0 {
                    // Changes come in bursts: wait for the end of it.
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    let _ = FindNextChangeNotification(h);
                    if WATCH_GEN.load(Ordering::Acquire) == generation {
                        let _ =
                            PostMessageW(Some(HWND(hwnd as *mut _)), WATCHED, WPARAM(generation as usize), LPARAM(0));
                    }
                }
            }
            let _ = FindCloseChangeNotification(h);
        });
    }

    fn go(&mut self, loc: Loc) {
        if loc == self.t().loc {
            return;
        }
        let tab = self.tab();
        let old = std::mem::replace(&mut tab.loc, loc);
        tab.back.push(old);
        tab.forward.clear();
        tab.scroll = 0.0;
        tab.anchor = None;
        tab.filter.clear();
        self.clear_search();
        self.load();
    }

    /// Reads the folder again, and shows that it's doing it: the refresh
    /// button turns and a bar runs under the command bar until it's loaded.
    fn refresh(&mut self) {
        self.refresh_at = Some(Instant::now());
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_BUSY, 16, None);
        }
        // A phone plugged in since shows in the sidebar too.
        self.set_side();
        self.load();
    }

    /// Whether a refresh is showing as busy.
    fn busy(&self) -> bool {
        self.refresh_at.is_some_and(|t| {
            let e = t.elapsed();
            e < BUSY_MIN || (self.tabs[self.cur].loading && e < std::time::Duration::from_secs(30))
        })
    }

    fn tick(&mut self) {
        if !self.busy() {
            self.refresh_at = None;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_BUSY);
            }
        }
        appwin::redraw(self.hwnd);
    }

    fn pins(&self) -> Vec<String> {
        self.store.pins.clone().unwrap_or_else(default_pins)
    }

    /// The sidebar from what's pinned.
    fn set_side(&mut self) {
        let pins = self.pins();
        self.pin_n = pins.len();
        self.side = sidebar(&pins);
    }

    /// Pins `path` (a folder) in the sidebar, or takes it off.
    fn toggle_pin(&mut self, path: &str) {
        let mut pins = self.pins();
        match pins.iter().position(|p| p.eq_ignore_ascii_case(path)) {
            Some(i) => {
                pins.remove(i);
            }
            None => pins.push(path.to_string()),
        }
        self.store.pins = Some(pins);
        self.set_side();
        self.store.save();
    }

    /// The right-click menu of sidebar entry `i`: pinned folders can be
    /// taken off and moved up and down.
    fn side_menu(&mut self, i: usize) {
        use crate::menu::{item, show, Entry, Item as MI, Place};
        let pinned = i >= 1 && i <= self.pin_n;
        let mut entries = vec![item(1, "Open"), item(2, "Open in new tab")];
        if pinned {
            entries.push(Entry::Separator);
            entries.push(item(3, "Unpin from sidebar"));
            entries.push(Entry::Item(MI { id: 4, text: "Move up".into(), enabled: i > 1, ..Default::default() }));
            entries.push(Entry::Item(MI {
                id: 5,
                text: "Move down".into(),
                enabled: i < self.pin_n,
                ..Default::default()
            }));
        }
        entries.push(Entry::Separator);
        entries.push(item(6, "Restore the usual folders"));
        let loc = self.side[i].2.clone();
        let choice = show(self.hwnd, entries, Place::Cursor);
        match choice {
            1 => self.go(loc),
            2 => self.new_tab(loc),
            3..=5 if pinned => {
                let mut pins = self.pins();
                let at = i - 1;
                match choice {
                    3 => {
                        pins.remove(at);
                    }
                    4 if at > 0 => pins.swap(at, at - 1),
                    5 if at + 1 < pins.len() => pins.swap(at, at + 1),
                    _ => {}
                }
                self.store.pins = Some(pins);
                self.set_side();
                self.store.save();
            }
            6 => {
                self.store.pins = None;
                self.set_side();
                self.store.save();
            }
            _ => {}
        }
    }

    fn clear_search(&self) {
        unsafe {
            let _ = SetWindowTextW(self.search, w!(""));
        }
    }

    fn back(&mut self) {
        let tab = self.tab();
        if let Some(loc) = tab.back.pop() {
            let old = std::mem::replace(&mut tab.loc, loc);
            tab.forward.push(old);
            tab.scroll = 0.0;
            self.load();
        }
    }

    fn forward(&mut self) {
        let tab = self.tab();
        if let Some(loc) = tab.forward.pop() {
            let old = std::mem::replace(&mut tab.loc, loc);
            tab.back.push(old);
            tab.scroll = 0.0;
            self.load();
        }
    }

    /// The arrow after part `i` of the path: the folders inside it, to jump
    /// to one (the drives for This PC), as in Explorer's address bar.
    fn crumb_menu(&mut self, i: usize) {
        use crate::menu::{item, show, Place};
        let Some((_, loc)) = crumbs(&self.t().loc).into_iter().nth(i) else { return };
        let mut places: Vec<(String, Loc)> = Vec::new();
        match &loc {
            Loc::Path(dir) => {
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for e in entries.flatten() {
                        let Ok(kind) = e.file_type() else { continue };
                        if !kind.is_dir() {
                            continue;
                        }
                        let name = e.file_name().to_string_lossy().into_owned();
                        let path = e.path();
                        if !self.show_hidden {
                            use std::os::windows::fs::MetadataExt;
                            // Hidden (0x2) and system (0x4) folders stay out of the list, as they do in the pane.
                            if e.metadata().is_ok_and(|m| m.file_attributes() & 0x6 != 0) {
                                continue;
                            }
                        }
                        places.push((name, Loc::Path(path.to_string_lossy().into_owned())));
                    }
                }
            }
            Loc::Shell(..) if loc == filesio::this_pc() => {
                // This PC: its drives.
                places = self
                    .side
                    .iter()
                    .filter(|s| matches!(s.2, Loc::Path(_)) && s.1.len() == 2)
                    .map(|s| (s.1.clone(), s.2.clone()))
                    .collect();
            }
            _ => {}
        }
        if places.is_empty() {
            return;
        }
        places.sort_by(|a, b| natural(&a.0, &b.0));
        let entries = places.iter().enumerate().map(|(n, (name, _))| item(n + 1, name.as_str())).collect();
        if let Some(n) = show(self.hwnd, entries, Place::Cursor).checked_sub(1) {
            self.go(places[n].1.clone());
        }
    }

    /// The arrow after the place's icon in the address bar: the places in
    /// the navigation pane, to jump to one.
    fn root_menu(&mut self) {
        use crate::menu::{item, show, Place};
        let places: Vec<(String, Loc)> = self.side.iter().map(|s| (s.1.clone(), s.2.clone())).collect();
        let entries = places.iter().enumerate().map(|(n, (name, _))| item(n + 1, name.as_str())).collect();
        if let Some(n) = show(self.hwnd, entries, Place::Cursor).checked_sub(1) {
            self.go(places[n].1.clone());
        }
    }

    /// The "..." in a long path: the parts left out, nearest first.
    fn hidden_crumbs_menu(&mut self) {
        use crate::menu::{item, show, Place};
        let (w, _) = appwin::client(self.hwnd);
        let hidden = self.crumb_layout(w).1;
        let parts: Vec<(String, Loc)> = crumbs(&self.t().loc).into_iter().take(hidden).rev().collect();
        if parts.is_empty() {
            return;
        }
        let entries = parts.iter().enumerate().map(|(n, (name, _))| item(n + 1, name.as_str())).collect();
        if let Some(n) = show(self.hwnd, entries, Place::Cursor).checked_sub(1) {
            self.go(parts[n].1.clone());
        }
    }

    /// Right-click on Back or Forward: where they'd go, nearest first, to jump
    /// straight to one (as Explorer's list of recent places).
    fn history_menu(&mut self, back: bool) {
        use crate::menu::{item, show, Place};
        let tab = self.t();
        let list: Vec<&Loc> = if back { tab.back.iter().rev().collect() } else { tab.forward.iter().rev().collect() };
        if list.is_empty() {
            return;
        }
        let entries = list.iter().take(12).enumerate().map(|(n, l)| item(n + 1, l.title())).collect();
        let Some(n) = show(self.hwnd, entries, Place::Cursor).checked_sub(1) else { return };
        // Jumping n + 1 places takes n + 1 steps.
        for _ in 0..=n {
            if back {
                self.back();
            } else {
                self.forward();
            }
        }
    }

    /// Ctrl+Z: takes back the last rename or new item.
    fn undo(&mut self) {
        let Some(last) = self.undo.pop() else { return };
        let done = match &last {
            Undo::Rename(from, to) => {
                let ok = !std::path::Path::new(from).exists() && std::fs::rename(to, from).is_ok();
                ok.then(|| last.clone())
            }
            Undo::Created(path) => {
                let p = std::path::Path::new(path);
                // Only what's still empty goes, so nothing put in it since is lost.
                let gone = if p.is_dir() {
                    std::fs::remove_dir(p).is_ok()
                } else {
                    std::fs::metadata(p).is_ok_and(|m| m.len() == 0) && std::fs::remove_file(p).is_ok()
                };
                gone.then(|| last.clone())
            }
        };
        if let Some(d) = done {
            self.redo.push(d);
            self.load();
        }
    }

    /// Ctrl+Y: does again what Ctrl+Z took back.
    fn redo_last(&mut self) {
        let Some(last) = self.redo.pop() else { return };
        let done = match &last {
            Undo::Rename(from, to) => {
                let ok = !std::path::Path::new(to).exists() && std::fs::rename(from, to).is_ok();
                ok.then(|| last.clone())
            }
            Undo::Created(path) => {
                let p = std::path::Path::new(path);
                let ok = !p.exists()
                    && if path.ends_with('\\') || p.extension().is_none() {
                        std::fs::create_dir(p).is_ok()
                    } else {
                        std::fs::write(p, b"").is_ok()
                    };
                ok.then(|| last.clone())
            }
        };
        if let Some(d) = done {
            self.undo.push(d);
            self.load();
        }
    }

    fn up(&mut self) {
        let target = match &self.t().loc {
            Loc::Path(p) => {
                let t = p.trim_end_matches('\\');
                match t.rfind('\\') {
                    Some(i) if i >= 2 => Some(Loc::Path(if i == 2 { t[..3].to_string() } else { t[..i].to_string() })),
                    _ => Some(filesio::this_pc()),
                }
            }
            Loc::Search(scope, _) => Some(Loc::Path(scope.clone())),
            Loc::Shell(..) => Some(Loc::Home),
            Loc::Home => None,
        };
        if let Some(t) = target {
            self.go(t);
        }
    }

    fn loaded(&mut self) {
        let done: Vec<(u32, Vec<Item>)> = LOADS.lock().map(|mut l| std::mem::take(&mut *l)).unwrap_or_default();
        for (generation, items) in done {
            let Some(tab) = self.tabs.iter_mut().find(|t| t.generation == generation) else { continue };
            // Keep the selection across a reload of the same folder.
            let keep: HashSet<String> = tab.selected().into_iter().map(|i| tab.items[i].path.clone()).collect();
            tab.sel = items.iter().map(|i| keep.contains(&i.path)).collect();
            tab.items = items;
            tab.loading = false;
        }
        self.size_folders();
        self.resort();
        appwin::redraw(self.hwnd);
    }

    /// Works out how big each folder in the current one is, as TreeSize
    /// does: at once from the instant-search index when it's running (it
    /// keeps every folder's total), else by adding up each folder's files
    /// in the background, one folder at a time, biggest news first.
    fn size_folders(&mut self) {
        let t = self.t();
        let Some(dir) = t.loc.dir() else { return };
        let generation = t.generation;
        let folders: Vec<String> =
            t.items.iter().filter(|i| i.folder && i.space.is_none()).map(|i| i.path.clone()).collect();
        if folders.is_empty() {
            return;
        }
        SIZING.store(generation, Ordering::Release);
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let post = |path: String, size: u64| {
                if let Ok(mut s) = SIZES.lock() {
                    s.push((generation, path, size));
                }
                unsafe {
                    let _ = PostMessageW(Some(HWND(hwnd as *mut _)), SIZED, WPARAM(0), LPARAM(0));
                }
            };
            if let Some((_, kids)) = crate::fileindex::children(&dir) {
                let base = dir.trim_end_matches('\\').to_string();
                for (name, is_dir, size) in kids {
                    if is_dir {
                        post(format!("{base}\\{name}"), size);
                    }
                }
                return;
            }
            for f in folders {
                let mut total = 0u64;
                let mut stack = vec![std::path::PathBuf::from(&f)];
                while let Some(d) = stack.pop() {
                    if SIZING.load(Ordering::Acquire) != generation {
                        return;
                    }
                    let Ok(list) = std::fs::read_dir(&d) else { continue };
                    for e in list.flatten() {
                        let Ok(m) = e.metadata() else { continue };
                        // Links and junctions point elsewhere: not counted twice.
                        if m.file_type().is_symlink() {
                            continue;
                        }
                        if m.is_dir() {
                            stack.push(e.path());
                        } else {
                            total += m.len();
                        }
                    }
                }
                post(f, total);
            }
        });
    }

    fn sized(&mut self) {
        let done: Vec<(u32, String, u64)> = SIZES.lock().map(|mut s| std::mem::take(&mut *s)).unwrap_or_default();
        for (generation, path, size) in done {
            let Some(tab) = self.tabs.iter_mut().find(|t| t.generation == generation) else { continue };
            if let Some(item) = tab.items.iter_mut().find(|i| i.folder && i.path.eq_ignore_ascii_case(&path)) {
                item.size = Some(size);
            }
        }
        if self.sort.0 == 3 {
            self.resort();
        }
        appwin::redraw(self.hwnd);
    }

    /// The icon of sidebar entry `i` (Windows' own, in colour).
    fn side_icon(&mut self, i: usize) -> Option<ID2D1Bitmap> {
        let loc = self.side[i].2.clone();
        self.loc_icon(&loc)
    }

    fn resort(&mut self) {
        let (col, asc) = self.sort;
        for tab in &mut self.tabs {
            let f = tab.filter.to_lowercase();
            let mut order: Vec<usize> = (0..tab.items.len())
                .filter(|&i| f.is_empty() || tab.items[i].name.to_lowercase().contains(&f))
                .collect();
            let items = &tab.items;
            let by = self.group;
            let now = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
            let now = ((now.dwHighDateTime as u64) << 32) | now.dwLowDateTime as u64;
            let groups: Vec<(u32, String)> = items.iter().map(|it| group_of(it, by, now)).collect();
            order.sort_by(|&a, &b| {
                let (x, y) = (&items[a], &items[b]);
                // In their groups first (Group by), then folders first, whichever way.
                let grouped = groups[a].cmp(&groups[b]);
                if grouped != std::cmp::Ordering::Equal {
                    return grouped;
                }
                let folders = y.folder.cmp(&x.folder);
                let by = match col {
                    1 => x.modified.cmp(&y.modified),
                    2 => x.kind.to_lowercase().cmp(&y.kind.to_lowercase()),
                    3 => x.size.unwrap_or(0).cmp(&y.size.unwrap_or(0)),
                    _ => natural(&x.name, &y.name),
                };
                folders.then(if asc { by } else { by.reverse() }).then_with(|| natural(&x.name, &y.name))
            });
            tab.heads.clear();
            if by != 0 {
                for (pos, &i) in order.iter().enumerate() {
                    match tab.heads.last_mut() {
                        Some(h) if groups[order[h.0]] == groups[i] => h.2 += 1,
                        _ => tab.heads.push((pos, groups[i].1.clone(), 1)),
                    }
                }
            }
            tab.order = order;
        }
    }

    // ------------------------------------------------------------ doing --

    fn open_item(&mut self, i: usize) {
        let Some(item) = self.t().items.get(i).cloned() else { return };
        if item.folder {
            let loc = if std::path::Path::new(&item.path).is_dir() {
                Loc::Path(item.path.clone())
            } else {
                Loc::Shell(item.path.clone(), item.name.clone())
            };
            self.go(loc);
        } else if let Some(dir) = shortcut_folder(&item.path) {
            // A shortcut to a folder opens it here.
            self.go(Loc::Path(dir));
        } else {
            open_file(self.hwnd, &item.path);
        }
    }

    fn open_selected(&mut self) {
        let sel = self.t().selected();
        if sel.len() == 1 {
            self.open_item(sel[0]);
        } else {
            for i in sel {
                let item = self.t().items[i].clone();
                if !item.folder {
                    open_file(self.hwnd, &item.path);
                }
            }
        }
    }

    fn select_only(&mut self, pos: Option<usize>) {
        let tab = self.tab();
        tab.sel.iter_mut().for_each(|s| *s = false);
        if let Some(p) = pos {
            if let Some(&i) = tab.order.get(p) {
                tab.sel[i] = true;
            }
        }
        tab.anchor = pos;
        self.reveal(pos);
    }

    /// Scrolls so position `pos` is in view.
    fn reveal(&mut self, pos: Option<usize>) {
        let Some(pos) = pos else { return };
        let (_, y0, _, ch) = self.content();
        let top = self.rows_top();
        let (_, y, _, h) = self.item_rect(pos);
        let view_h = ch - (top - y0);
        let scroll = self.t().scroll;
        let (ry, rb) = (y - top, y - top + h);
        let s = if ry < scroll {
            ry
        } else if rb > scroll + view_h {
            rb - view_h
        } else {
            scroll
        };
        self.tab().scroll = s.clamp(0.0, self.max_scroll());
    }

    fn delete(&mut self, for_good: bool) {
        // Real files only (a drive, or This PC's entries, can't be recycled).
        let paths: Vec<String> =
            self.t().selected_paths().into_iter().filter(|p| std::path::Path::new(p).exists() && p.len() > 3).collect();
        if !paths.is_empty() {
            crate::deskicons::delete(self.hwnd, &paths, for_good);
        }
    }

    fn copy(&self, cut: bool) {
        let paths = self.t().selected_paths();
        if !paths.is_empty() {
            crate::deskicons::verb(self.hwnd, &paths, if cut { "cut" } else { "copy" }, false);
        }
    }

    fn paste(&self) {
        if let Some(dir) = self.t().loc.dir() {
            crate::deskicons::paste_into(self.hwnd, std::path::Path::new(&dir));
        }
    }

    fn new_folder(&mut self) {
        let Some(dir) = self.t().loc.dir() else { return };
        let Some(path) = crate::deskicons::free_name(std::path::Path::new(&dir), "New folder", "") else { return };
        if std::fs::create_dir(&path).is_ok() {
            self.undo.push(Undo::Created(path.to_string_lossy().into_owned()));
            self.redo.clear();
            *PENDING_RENAME.lock().unwrap_or_else(|e| e.into_inner()) = Some(path.to_string_lossy().into_owned());
            self.load();
        }
    }

    fn new_text(&mut self) {
        let Some(dir) = self.t().loc.dir() else { return };
        let Some(path) = crate::deskicons::free_name(std::path::Path::new(&dir), "New Text Document", ".txt") else {
            return;
        };
        if std::fs::write(&path, b"").is_ok() {
            self.undo.push(Undo::Created(path.to_string_lossy().into_owned()));
            self.redo.clear();
            *PENDING_RENAME.lock().unwrap_or_else(|e| e.into_inner()) = Some(path.to_string_lossy().into_owned());
            self.load();
        }
    }

    /// F2: a text box over the item's name; Enter renames it, Esc leaves it.
    fn start_rename(&mut self) {
        let Some(&i) = self.t().selected().first() else { return };
        let item = self.t().items[i].clone();
        if self.t().loc.dir().is_none() && !matches!(self.t().loc, Loc::Search(..)) {
            return;
        }
        let pos = self.t().order.iter().position(|&x| x == i).unwrap_or(0);
        self.reveal(Some(pos));
        let (x, y, w, h) = self.item_rect(pos);
        let y = y - self.t().scroll;
        let k = appwin::scale(self.hwnd);
        let (ex, ey, ew, eh) = match self.view {
            View::Details => (x + 34.0, y + 3.0, (w * 0.45).max(200.0), h - 6.0),
            View::Icons => (x, y + 78.0, w, 24.0),
        };
        // The real file name (shown names may have had their extension hidden).
        let file_name = std::path::Path::new(&item.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(item.name.clone());
        unsafe {
            let Ok(edit) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                &HSTRING::from(file_name.as_str()),
                WS_CHILD | WS_VISIBLE | WS_BORDER | WINDOW_STYLE(0x80),
                (ex * k) as i32,
                (ey * k) as i32,
                (ew * k) as i32,
                (eh * k) as i32,
                Some(self.hwnd),
                Some(HMENU(ID_RENAME as *mut _)),
                Some(appwin::instance()),
                None,
            ) else {
                return;
            };
            SendMessageW(edit, WM_SETFONT, Some(WPARAM(self.font.0 as usize)), Some(LPARAM(1)));
            let _ = SetWindowSubclass(edit, Some(edit_proc), ID_RENAME, 0);
            let _ = SetFocus(Some(edit));
            // Select the name, not the extension, as Explorer does.
            let stem = if item.folder {
                file_name.encode_utf16().count()
            } else {
                file_name.rfind('.').map_or(file_name.len(), |d| file_name[..d].encode_utf16().count())
            };
            SendMessageW(edit, 0x00B1, Some(WPARAM(0)), Some(LPARAM(stem as isize)));
            self.rename = Some((edit, item.path.clone()));
        }
    }

    fn finish_rename(&mut self, keep: bool) {
        let Some((edit, path)) = self.rename.take() else { return };
        let mut buf = [0u16; 260];
        let n = unsafe { GetWindowTextW(edit, &mut buf) } as usize;
        let name = String::from_utf16_lossy(&buf[..n]).trim().to_string();
        unsafe {
            let _ = DestroyWindow(edit);
            let _ = SetFocus(Some(self.hwnd));
        }
        let old = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !keep || name.is_empty() || name == old {
            return;
        }
        if let Some(dir) = std::path::Path::new(&path).parent() {
            let to = dir.join(&name).to_string_lossy().into_owned();
            self.undo.push(Undo::Rename(path.clone(), to));
            self.redo.clear();
        }
        crate::shell::off_thread(move || unsafe {
            let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(path.as_str()), None) else {
                return;
            };
            if let Ok(op) = CoCreateInstance::<_, IFileOperation>(&FileOperation, None, CLSCTX_ALL) {
                let _ = op.RenameItem(&item, &HSTRING::from(name.as_str()), None);
                let _ = op.PerformOperations();
            }
        });
    }

    fn sort_menu(&mut self) {
        use crate::menu::{check, show, Place};
        let entries = COLS.iter().enumerate().map(|(i, (n, _))| check(i + 1, *n, self.sort.0 == i)).collect();
        if let Some(i) = show(self.hwnd, entries, Place::Cursor).checked_sub(1) {
            self.sort = if self.sort.0 == i { (i, !self.sort.1) } else { (i, i == 0 || i == 2) };
            self.resort();
            self.remember();
        }
    }

    fn view_menu(&mut self) {
        use crate::menu::{check, show, sub, Entry, Place};
        let icons_at = |k: f32| self.view == View::Icons && (self.icon_scale - k).abs() < 0.01;
        let entries = vec![
            check(4, "Extra large icons", icons_at(ICON_STEPS[4])),
            check(5, "Large icons", icons_at(ICON_STEPS[3])),
            check(6, "Medium icons", icons_at(ICON_STEPS[2])),
            check(7, "Small icons", icons_at(ICON_STEPS[1])),
            check(1, "Details", self.view == View::Details),
            Entry::Separator,
            sub(
                "Group by",
                vec![
                    check(10, "None", self.group == 0),
                    check(11, "Type", self.group == 1),
                    check(12, "Date modified", self.group == 2),
                    check(13, "Size", self.group == 3),
                ],
            ),
            Entry::Separator,
            check(20, "Compact view", self.compact),
            sub(
                "Show",
                vec![
                    check(21, "Navigation pane", self.nav),
                    check(22, "Details pane", self.details),
                    Entry::Separator,
                    check(23, "File name extensions", self.shows_extensions()),
                    check(3, "Hidden items", self.show_hidden),
                ],
            ),
        ];
        let chosen = show(self.hwnd, entries, Place::Cursor);
        match chosen {
            1 => self.view = View::Details,
            n @ 4..=7 => self.view_size(match n {
                4 => 1,
                5 => 2,
                6 => 3,
                _ => 4,
            }),
            g @ 10..=13 => {
                self.group = (g - 10) as u8;
                self.resort();
            }
            3 => {
                self.show_hidden = !self.show_hidden;
                self.remember();
                self.load();
            }
            20 => self.compact = !self.compact,
            21 => self.nav = !self.nav,
            22 => self.details = !self.details,
            23 => {
                self.exts = if self.shows_extensions() { 2 } else { 1 };
                filesio::set_ext_mode(self.exts);
                self.load();
            }
            _ => {}
        }
        if matches!(chosen, 1 | 4..=7 | 10..=13 | 20..=23) {
            self.remember();
            self.place_boxes();
        }
        let m = self.max_scroll();
        let t = self.tab();
        t.scroll = t.scroll.min(m);
        appwin::redraw(self.hwnd);
    }

    /// Whether file names show their extensions (as Windows says, unless set here).
    fn shows_extensions(&self) -> bool {
        match self.exts {
            1 => true,
            2 => false,
            _ => !filesio::explorer_settings().2,
        }
    }

    fn new_menu(&mut self) {
        use crate::menu::{item, show, Place};
        match show(self.hwnd, vec![item(1, "Folder"), item(2, "Text Document"), item(3, "Shortcut")], Place::Cursor) {
            1 => self.new_folder(),
            2 => self.new_text(),
            3 => {
                if let Some(dir) = self.t().loc.dir() {
                    crate::shell::launch(
                        "rundll32.exe",
                        &format!("appwiz.cpl,NewLinkHere {}\\", dir.trim_end_matches('\\')),
                        false,
                    );
                }
            }
            _ => {}
        }
    }

    /// "Pin to sidebar" or "Unpin from sidebar" for the folder shown (greyed
    /// where it can't be pinned: This PC, search results...).
    fn pin_entry(&self, id: usize) -> crate::menu::Entry {
        let here = self.pin_target();
        let pinned = here.as_ref().is_some_and(|h| self.pins().iter().any(|p| p.eq_ignore_ascii_case(h)));
        crate::menu::Entry::Item(crate::menu::Item {
            id,
            text: if pinned { "Unpin from sidebar" } else { "Pin to sidebar" }.into(),
            enabled: here.is_some(),
            ..Default::default()
        })
    }

    /// The folder to pin: the one selected (if it's one folder), else the one shown.
    fn pin_target(&self) -> Option<String> {
        let sel = self.t().selected();
        if let [i] = sel[..] {
            let it = &self.t().items[i];
            return (it.folder && std::path::Path::new(&it.path).is_dir()).then(|| it.path.clone());
        }
        match &self.t().loc {
            Loc::Path(p) if !p.starts_with("\\\\") => Some(p.clone()),
            _ => None,
        }
    }

    fn pin_current(&mut self) {
        if let Some(p) = self.pin_target() {
            self.toggle_pin(&p);
        }
    }

    fn more_menu(&mut self) {
        use crate::menu::{item, show, Entry, Place};
        let entries = vec![
            item(1, "Open in Terminal"),
            item(2, "Copy path"),
            item(3, "Disk usage of this folder"),
            self.pin_entry(7),
            Entry::Separator,
            item(4, "Select all"),
            item(5, "Select none"),
            item(8, "Invert selection"),
            Entry::Separator,
            item(6, "Properties"),
        ];
        let dir = self.t().loc.dir();
        match show(self.hwnd, entries, Place::Cursor) {
            7 => self.pin_current(),
            1 => {
                if let Some(d) = dir {
                    if !crate::shell::launch("wt.exe", &format!("-d \"{d}\""), false) {
                        crate::shell::launch("cmd.exe", &format!("/k cd /d \"{d}\""), false);
                    }
                }
            }
            2 => {
                let sel = self.t().selected_paths();
                let text = if sel.is_empty() { dir.unwrap_or_default() } else { sel.join("\r\n") };
                crate::shell::copy_text(&text);
            }
            3 => {
                if let Some(d) = dir {
                    appwin::launch(&["--disk", &d]);
                }
            }
            4 => self.tab().sel.iter_mut().for_each(|s| *s = true),
            5 => self.tab().sel.iter_mut().for_each(|s| *s = false),
            8 => {
                let t = self.tab();
                let order = t.order.clone();
                for i in order {
                    t.sel[i] = !t.sel[i];
                }
            }
            6 => self.properties(),
            _ => {}
        }
    }

    fn properties(&self) {
        let sel = self.t().selected_paths();
        let target = sel.first().cloned().or(self.t().loc.dir());
        if let Some(t) = target {
            unsafe {
                let _ = SHObjectProperties(Some(self.hwnd), SHOP_FILEPATH, &HSTRING::from(t), PCWSTR::null());
            }
        }
    }

    fn background_menu(&mut self) {
        use crate::menu::{item, show, sub, Entry, Item as MI, Place};
        let can_paste = crate::deskicons::can_paste() && self.t().loc.dir().is_some();
        let has_dir = self.t().loc.dir().is_some();
        let entries = vec![
            sub(
                "View",
                vec![
                    crate::menu::check(10, "Details", self.view == View::Details),
                    crate::menu::check(11, "Icons", self.view == View::Icons),
                ],
            ),
            sub(
                "Sort by",
                COLS.iter().enumerate().map(|(i, (n, _))| crate::menu::check(20 + i, *n, self.sort.0 == i)).collect(),
            ),
            item(1, "Refresh"),
            Entry::Separator,
            Entry::Item(MI {
                id: 2,
                text: "Paste".into(),
                hint: "Ctrl+V".into(),
                enabled: can_paste,
                ..Default::default()
            }),
            Entry::Item(MI { id: 3, text: "Paste shortcut".into(), enabled: can_paste, ..Default::default() }),
            Entry::Separator,
            Entry::Item(MI {
                text: "New".into(),
                enabled: has_dir,
                sub: vec![item(4, "Folder"), item(5, "Text Document")],
                ..Default::default()
            }),
            Entry::Separator,
            Entry::Item(MI { id: 6, text: "Open in Terminal".into(), enabled: has_dir, ..Default::default() }),
            self.pin_entry(8),
            item(7, "Properties"),
        ];
        match show(self.hwnd, entries, Place::Cursor) {
            8 => self.pin_current(),
            1 => self.refresh(),
            2 => self.paste(),
            3 => {
                if let Some(d) = self.t().loc.dir() {
                    crate::deskicons::paste_shortcut_into(std::path::PathBuf::from(d));
                }
            }
            4 => self.new_folder(),
            5 => self.new_text(),
            6 => {
                if let Some(d) = self.t().loc.dir() {
                    if !crate::shell::launch("wt.exe", &format!("-d \"{d}\""), false) {
                        crate::shell::launch("cmd.exe", &format!("/k cd /d \"{d}\""), false);
                    }
                }
            }
            7 => self.properties(),
            10 => {
                self.view = View::Details;
                self.remember();
            }
            11 => {
                self.view = View::Icons;
                self.remember();
            }
            n @ 20..=23 => {
                self.sort = (n - 20, true);
                self.resort();
                self.remember();
            }
            _ => {}
        }
    }

    fn command(&mut self, i: usize) {
        match i {
            0 => self.new_menu(),
            1 => self.copy(true),
            2 => self.copy(false),
            3 => self.paste(),
            4 => self.start_rename(),
            5 => self.quick_share(),
            6 => self.delete(false),
            7 => self.sort_menu(),
            8 => self.view_menu(),
            9 => self.more_menu(),
            10 => {
                self.details = !self.details;
                self.remember();
                appwin::redraw(self.hwnd);
            }
            _ => {}
        }
    }

    /// Sends the selected files with Quick Share (to a phone or PC nearby).
    fn quick_share(&mut self) {
        let files: Vec<String> =
            self.t().selected_paths().into_iter().filter(|p| std::path::Path::new(p).is_file()).collect();
        let mut args = vec!["--quickshare-send".to_string()];
        args.extend(files);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        appwin::launch(&refs);
    }

    fn new_tab(&mut self, loc: Loc) {
        self.tabs.push(Tab::new(loc));
        self.cur = self.tabs.len() - 1;
        self.clear_search();
        self.load();
    }

    fn close_tab(&mut self, i: usize) {
        if self.tabs.len() <= 1 {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
            return;
        }
        self.tabs.remove(i);
        if self.cur >= self.tabs.len() || self.cur > i {
            self.cur = self.cur.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.switch(self.cur);
    }

    fn switch(&mut self, i: usize) {
        self.cur = i.min(self.tabs.len() - 1);
        let f = self.t().filter.clone();
        unsafe {
            let _ = SetWindowTextW(self.search, &HSTRING::from(f));
        }
        self.watch();
        self.set_title();
        // Each folder keeps its own view: the one in front's.
        let (view, sort, group) = (self.view, self.sort, self.group);
        self.apply_view();
        if (self.view, self.sort, self.group) != (view, sort, group) {
            self.resort();
        }
        appwin::redraw(self.hwnd);
    }

    // ------------------------------------------------------- hit testing --

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let (w, h) = appwin::client(self.hwnd);
        if y < TABS_H {
            if let Some(b) = appwin::caption_at(x, y, w, CAP_H) {
                return Some(Hit::Caption(b));
            }
            let tw = self.tab_w(w);
            for i in 0..self.tabs.len() {
                let tx = 8.0 + i as f32 * tw;
                if x >= tx && x < tx + tw && y >= TAB_TOP {
                    return Some(if x >= tx + tw - 36.0 { Hit::TabClose(i) } else { Hit::Tab(i) });
                }
            }
            let px = 8.0 + self.tabs.len() as f32 * tw + 4.0;
            return (x >= px && x < px + 40.0 && y >= TAB_TOP).then_some(Hit::NewTab);
        }
        if y < TABS_H + BAR_H {
            let (ax, aw, _) = self.box_layout(w);
            if x >= ax && x < ax + aw {
                if !self.editing_addr {
                    if (ax + 39.0..ax + 65.0).contains(&x) {
                        return Some(Hit::CrumbRoot);
                    }
                    let (layout, hidden) = self.crumb_layout(w);
                    if hidden > 0 && (ax + 65.0..ax + 97.0).contains(&x) {
                        return Some(Hit::CrumbMore);
                    }
                    for (cx, cw, i) in layout {
                        if x >= cx && x < cx + cw {
                            return Some(Hit::Crumb(i));
                        }
                        if x >= cx + cw && x < cx + cw + CHEV {
                            return Some(Hit::CrumbMenu(i));
                        }
                    }
                }
                return Some(Hit::Address);
            }
            let k = ((x - 8.0) / 48.0).floor();
            if k < 0.0 || x - 8.0 - k * 48.0 >= BTN_W {
                return None;
            }
            return match k as i32 {
                0 => Some(Hit::Back),
                1 => Some(Hit::Forward),
                2 => Some(Hit::Up),
                3 => Some(Hit::Refresh),
                _ => None,
            };
        }
        if y < TABS_H + BAR_H + CMD_H {
            let layout = self.cmd_layout(w);
            return (0..CMDS.len())
                .find(|&i| {
                    let (cx, cw) = layout[i];
                    x >= cx && x < cx + cw
                })
                .map(Hit::Cmd);
        }
        if y >= h - STATUS_H {
            return (0..2u8)
                .find(|&k| (status_view_x(w, k)..status_view_x(w, k) + 24.0).contains(&x))
                .map(Hit::ViewButton);
        }
        if self.details && x >= w - PANE_W {
            return Some(Hit::Pane);
        }
        if self.nav && x < SIDE_W {
            for (i, ry) in self.side_layout() {
                if y >= ry && y < ry + SIDE_ROW {
                    if self.side[i].2 == filesio::this_pc() && x < 28.0 {
                        return Some(Hit::SideFold);
                    }
                    return Some(Hit::Side(i));
                }
            }
            return None;
        }
        let (x0, y0, cw, _) = self.content();
        if self.view == View::Details && y < y0 + HEAD_H {
            let mut cx = x0 + 4.0;
            for i in 0..COLS.len() {
                let cwid = self.col_width(i, cw);
                // The edge after a column: drag it to make the column wider or narrower.
                // (The last column's edge is the row's end: nothing to drag there.)
                if i + 1 < COLS.len() && (x - (cx + cwid)).abs() < 5.0 {
                    return Some(Hit::HeaderEdge(i));
                }
                if x < cx + cwid {
                    return Some(Hit::Header(i));
                }
                cx += cwid;
            }
            return None;
        }
        let scroll = self.t().scroll;
        for pos in self.visible() {
            let (ix, iy, iw, ih) = self.item_rect(pos);
            let iy = iy - scroll;
            if x >= ix && x < ix + iw && y >= iy && y < iy + ih {
                return Some(Hit::Item(pos));
            }
        }
        Some(Hit::Empty)
    }

    /// The Name column's width.
    fn name_w(&self, cw: f32) -> f32 {
        self.col_widths(cw)[0]
    }

    /// Each tab's width: as wide as Explorer's (240) while they fit, all the
    /// same, narrower as more are opened.
    fn tab_w(&self, w: f32) -> f32 {
        // Room kept for the new-tab button, somewhere to drag the window by, and its buttons.
        ((w - 8.0 - 48.0 - 60.0 - 3.0 * appwin::CAPTION_W) / self.tabs.len() as f32).clamp(100.0, TAB_W)
    }

    /// Where each command is in a window `w` wide: (x, width). They go from
    /// the left in groups with a line between them, as Explorer's do, the
    /// buttons with a label as wide as it, and Details at the far right.
    fn cmd_layout(&self, w: f32) -> [(f32, f32); 11] {
        let label = |s: &str| self.measure.as_ref().map_or(s.len() as f32 * 7.0, |m| m.width(s, &self.ty.t12));
        let mut out = [(0.0, 0.0); 11];
        let mut x = 8.0;
        for (i, slot) in out.iter_mut().enumerate().take(10) {
            if starts_group(i) {
                x += 5.0;
            }
            let bw = if labelled(i) { 57.0 + label(CMDS[i].1) } else { BTN_W };
            *slot = (x, bw);
            x += bw + 8.0;
        }
        let dw = 46.0 + label(CMDS[10].1);
        out[10] = ((w - 8.0 - dw).max(x), dw);
        out
    }

    /// Whether command `i` can be used now (Explorer greys the rest): Cut,
    /// Copy, Rename, Share and Delete want something selected, Paste files
    /// on the clipboard.
    fn cmd_enabled(&self, i: usize) -> bool {
        match i {
            1 | 2 | 4 | 5 | 6 => self.t().sel.iter().any(|s| *s),
            3 => unsafe { IsClipboardFormatAvailable(15).is_ok() },
            _ => true,
        }
    }

    /// The navigation pane's rows as shown: (entry, y). The drives and
    /// devices under This PC are left out while it's folded, and a little
    /// room is kept for the lines after Home and after your pinned folders.
    fn side_layout(&self) -> Vec<(usize, f32)> {
        let (_, y0, _, _) = self.content();
        let mut y = y0 + 4.0;
        let mut out = Vec::new();
        for i in 0..self.side.len() {
            if !self.side_open && self.side_child(i) {
                continue;
            }
            out.push((i, y));
            y += SIDE_ROW;
            if self.side_divider(i) {
                y += 9.0;
            }
        }
        out
    }

    /// Whether side entry `i` sits under This PC (a drive, a phone...).
    fn side_child(&self, i: usize) -> bool {
        matches!(self.side[i].0, '\u{EDA2}' | '\u{E8EA}')
    }

    /// Whether a line goes after side entry `i`: after Home and after the
    /// last pinned folder, as Explorer's pane has them.
    fn side_divider(&self, i: usize) -> bool {
        i + 1 < self.side.len() && (i == 0 || i == self.pin_n)
    }

    /// The icon of a place (a tab's, the address bar's, the pane's).
    fn loc_icon(&mut self, loc: &Loc) -> Option<ID2D1Bitmap> {
        let target = match loc {
            Loc::Home => "::{F874310E-B6B7-47DC-BC84-B9E6B38F5903}".to_string(),
            Loc::Path(p) => p.clone(),
            Loc::Shell(name, _) => name.clone(),
            Loc::Search(..) => return None,
        };
        let key = format!("side:{target}");
        if let Some(b) = self.icons.get(&key) {
            return b.clone();
        }
        if self.asked.insert(key.clone()) {
            let _ = self.icon_tx.send((key, target, false));
        }
        None
    }

    /// Windows' app mode changed (or the window's just made): Explorer's
    /// light or dark colours, the window's frame and menus to match.
    fn apply_theme(&mut self) {
        self.pal = Pal::current();
        crate::menu::set_light(!self.pal.dark);
        // Mica's tint comes from the wallpaper, which takes a moment to read:
        // the plain colour until then.
        let dark = self.pal.dark;
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let base = if dark { DARK.strip } else { LIGHT.strip };
            let tint = mica(base, if dark { 0.22 } else { 0.30 });
            *MICA_TINT.lock().unwrap_or_else(|e| e.into_inner()) = Some((dark, tint));
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), MICA, WPARAM(0), LPARAM(0));
            }
        });
        unsafe {
            let dark = BOOL(self.pal.dark as i32);
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
            let old = std::mem::replace(&mut self.brush, CreateSolidBrush(Pal::colorref(self.pal.field)));
            if !old.is_invalid() {
                let _ = DeleteObject(old.into());
            }
            for e in [self.addr, self.search] {
                let _ = InvalidateRect(Some(e), None, true);
            }
        }
        self.mica_arrived();
    }

    /// The tab row's Mica colour is ready (for the mode it was made for).
    fn mica_arrived(&mut self) {
        if let Some((dark, tint)) = *MICA_TINT.lock().unwrap_or_else(|e| e.into_inner()) {
            if dark == self.pal.dark {
                self.pal.strip = tint;
            }
        }
        let caption = Pal::colorref(self.pal.strip);
        unsafe {
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWINDOWATTRIBUTE(35), &caption as *const _ as *const _, 4);
        }
        appwin::redraw(self.hwnd);
    }

    /// The display positions on screen now.
    fn visible(&self) -> std::ops::Range<usize> {
        let (_, y0, _, ch) = self.content();
        let top = self.rows_top();
        let scroll = self.t().scroll;
        let n = self.t().order.len();
        let view_h = ch - (top - y0);
        if self.view == View::Icons && !self.t().heads.is_empty() {
            // Grouped tiles: each group's rows that show, from the first to the last.
            let (c, th) = (self.cols(), self.tile_h());
            let (mut lo, mut hi) = (n, 0);
            for (k, h) in self.t().heads.iter().enumerate() {
                let rows = h.2.div_ceil(c);
                let y = self.head_y(k) + GROUP_H;
                if y + rows as f32 * th < scroll - th || y > scroll + view_h + th {
                    continue;
                }
                let first = (((scroll - th - y) / th).floor().max(0.0) as usize).min(rows);
                let last = (((scroll + view_h + th - y) / th).ceil().max(0.0) as usize).min(rows);
                lo = lo.min(h.0 + first * c);
                hi = hi.max((h.0 + last * c).min(h.0 + h.2));
            }
            return lo.min(n)..hi.max(lo.min(n));
        }
        let (row, per) = match self.view {
            View::Details => (self.row_h(), 1),
            View::Icons => (self.tile_h(), self.cols()),
        };
        // Group headings push rows down: start early enough, and take more.
        let heads = if self.view == View::Details { self.t().heads.len() } else { 0 };
        let first = (((scroll - heads as f32 * GROUP_H).max(0.0) / row).floor() as usize) * per;
        let count = (((ch - (top - y0)) / row).ceil() as usize + 2 + heads) * per;
        first.min(n)..(first + count).min(n)
    }

    // ---------------------------------------------------------- drawing --

    fn icon_for(&mut self, i: usize, big: bool) -> Option<ID2D1Bitmap> {
        let item = &self.tabs[self.cur].items[i];
        let thumb = big && !item.folder && filesio::has_thumbnail(&item.path);
        let key = if thumb { format!("thumb:{}", item.path) } else { item.icon_key.clone() };
        if let Some(b) = self.icons.get(&key) {
            return b.clone();
        }
        if self.asked.insert(key.clone()) {
            let _ = self.icon_tx.send((key, item.path.clone(), thumb));
        }
        None
    }

    fn icons_arrived(&mut self) {
        let done: Vec<(String, Vec<u8>)> =
            self.icon_done.lock().map(|mut d| std::mem::take(&mut *d)).unwrap_or_default();
        for (key, pixels) in done {
            let s = crate::icons::ICON_SIZE;
            let b =
                (!pixels.is_empty()).then(|| self.gfx.as_ref().and_then(|g| g.bitmap(s, s, &pixels).ok())).flatten();
            self.icons.insert(key, b);
        }
        appwin::redraw(self.hwnd);
    }

    fn paint(&mut self) {
        // The address box lost the keyboard: back to the path's parts.
        if self.editing_addr && unsafe { GetFocus() } != self.addr {
            self.editing_addr = false;
            self.place_boxes();
        }
        // A lost render target: every bitmap is gone with it.
        let generation = self.gfx.as_ref().map_or(0, |g| g.generation);
        if self.gfx_generation != generation {
            self.icons.clear();
            self.asked.clear();
            self.gfx_generation = generation;
        }
        let vis = self.visible();
        let big = self.view == View::Icons;
        let items: Vec<(usize, Option<ID2D1Bitmap>)> = vis
            .clone()
            .map(|pos| {
                let i = self.tabs[self.cur].order[pos];
                (pos, self.icon_for(i, big))
            })
            .collect();
        let side: Vec<Option<ID2D1Bitmap>> = (0..self.side.len()).map(|i| self.side_icon(i)).collect();
        let tab_locs: Vec<Loc> = self.tabs.iter().map(|t| t.loc.clone()).collect();
        let tabs: Vec<Option<ID2D1Bitmap>> = tab_locs.iter().map(|l| self.loc_icon(l)).collect();
        // The address bar starts with This PC's icon for a folder on a drive, else the place's own.
        let root = match &self.t().loc {
            Loc::Path(p) if !p.starts_with("\\\\") => self.loc_icon(&filesio::this_pc()),
            other => {
                let other = other.clone();
                self.loc_icon(&other)
            }
        };
        // The details pane's big picture of the one thing selected.
        let one = match self.t().selected()[..] {
            [i] if self.details => Some(i),
            _ => None,
        };
        let pane = one.and_then(|i| self.icon_for(i, true));
        let pics = Pics { items, side, tabs, root, pane };
        let hwnd = self.hwnd;
        let Some(mut gfx) = self.gfx.take() else { return };
        appwin::paint(hwnd, &mut gfx, |p, _, w, h| self.draw(p, w, h, &pics));
        self.gfx = Some(gfx);
    }

    /// Fills a row (a file, a navigation pane entry) as selected, under the
    /// pointer, or both, in Explorer's colours.
    fn row_fill(&self, p: &Painter, x: f32, y: f32, w: f32, h: f32, selected: bool, lit: bool) {
        let pal = &self.pal;
        let c = match (selected, lit) {
            (true, true) => pal.row_sel_hover,
            (true, false) if self.active => pal.row_sel,
            (true, false) => pal.row_sel_idle,
            (false, true) => pal.row_hover,
            (false, false) => return,
        };
        p.rrect(x, y, w, h, 4.0, c);
        if let (true, Some(edge)) = (selected && self.active, pal.row_edge) {
            p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 4.0, edge, 1.0);
        }
    }

    fn draw(&self, p: &Painter, w: f32, h: f32, pics: &Pics) {
        let pal = &self.pal;
        let ty = &self.ty;
        let accent = theme::accent();
        let hov = self.hovered;
        let t = self.t();
        let icon = |code: u32, font: &Option<IDWriteTextFormat>, x: f32, y: f32, bw: f32, bh: f32, c: Color| {
            if let Some(f) = font {
                p.text(&glyph(code), f, x, y, bw, bh, Align::Center, c);
            }
        };
        // Mica behind the tabs while the window's in front; plain once it isn't, as Windows does.
        let strip = if self.active {
            pal.strip
        } else if pal.dark {
            DARK.strip
        } else {
            LIGHT.strip
        };
        let top_ink = if self.active { pal.text } else { pal.text_off };
        p.rrect(0.0, 0.0, w, TABS_H, 0.0, strip);
        p.rrect(0.0, TABS_H, w, BAR_H, 0.0, pal.layer);
        p.rrect(0.0, TABS_H + BAR_H, w, h - TABS_H - BAR_H, 0.0, pal.base);
        p.rrect(0.0, TABS_H + BAR_H - 1.0, w, 1.0, 0.0, pal.line);
        p.rrect(0.0, TABS_H + BAR_H + CMD_H - 1.0, w, 1.0, 0.0, pal.line);

        // Tabs: the one in front joins the address row; a line between the others.
        let tw = self.tab_w(w);
        let tab_line = if pal.dark { rgb(0x3D3D3D) } else { rgb(0xD1D1D1) };
        for (i, tab) in self.tabs.iter().enumerate() {
            let tx = 8.0 + i as f32 * tw;
            let on = i == self.cur;
            let lit = matches!(hov, Some(Hit::Tab(j)) | Some(Hit::TabClose(j)) if j == i);
            if on {
                p.tab(tx, TAB_TOP, tw, TABS_H, 8.0, pal.layer);
            } else if lit {
                p.rrect(tx + 2.0, TAB_TOP + 2.0, tw - 4.0, TABS_H - TAB_TOP - 6.0, 6.0, pal.hover);
            }
            let next_quiet = i + 1 < self.tabs.len()
                && i + 1 != self.cur
                && !matches!(hov, Some(Hit::Tab(j)) | Some(Hit::TabClose(j)) if j == i + 1);
            if !on && !lit && next_quiet {
                p.line(tx + tw - 0.5, TAB_TOP + 8.0, tx + tw - 0.5, TAB_TOP + 24.0, tab_line, 1.0);
            }
            match pics.tabs.get(i).cloned().flatten() {
                Some(b) => p.image(&b, tx + 9.0, TAB_TOP + 8.0, 16.0, 16.0, 1.0),
                None => icon(0xE8B7, &ty.i16, tx + 9.0, TAB_TOP + 8.0, 16.0, 16.0, top_ink),
            }
            let title_ink = if on { top_ink } else { pal.text2 };
            p.text(&tab.loc.title(), &ty.t12, tx + 33.0, TAB_TOP, tw - 33.0 - 38.0, 32.0, Align::Left, title_ink);
            if on || lit {
                let (bx, by) = (tx + tw - 34.0, TAB_TOP + 4.0);
                if hov == Some(Hit::TabClose(i)) {
                    p.rrect(bx, by, 24.0, 24.0, 4.0, pal.hover);
                }
                icon(0xE711, &ty.i10, bx, by, 24.0, 24.0, title_ink);
            }
        }
        let px = 8.0 + self.tabs.len() as f32 * tw + 4.0;
        if hov == Some(Hit::NewTab) {
            p.rrect(px, TAB_TOP + 1.0, 40.0, 30.0, 6.0, pal.hover);
        }
        icon(0xE710, &ty.i12, px, TAB_TOP + 1.0, 40.0, 30.0, top_ink);
        // Minimise, maximise and close, as Windows 11 draws them.
        let maxed = appwin::maximized(self.hwnd);
        for (b, code) in [0xE921u32, if maxed { 0xE923 } else { 0xE922 }, 0xE8BB].iter().enumerate() {
            let x = w - (3 - b) as f32 * appwin::CAPTION_W;
            let lit = hov == Some(Hit::Caption(b as u8));
            let mut ink = top_ink;
            if lit && b == 2 {
                p.rrect(x, 0.0, appwin::CAPTION_W, CAP_H, 0.0, rgb(0xC42B1C));
                ink = rgb(0xFFFFFF);
            } else if lit {
                p.rrect(x, 0.0, appwin::CAPTION_W, CAP_H, 0.0, pal.hover);
            }
            icon(*code, &ty.i10, x, 0.0, appwin::CAPTION_W, CAP_H, ink);
        }

        // The address row: back, forward, up, refresh, then the address and search boxes.
        let by = TABS_H + 8.0;
        let busy = self.busy();
        let can = [!t.back.is_empty(), !t.forward.is_empty(), !matches!(t.loc, Loc::Home), true];
        for (k, (code, hit)) in
            [(0xE72B, Hit::Back), (0xE72A, Hit::Forward), (0xE74A, Hit::Up), (0xE72C, Hit::Refresh)].iter().enumerate()
        {
            let x = 8.0 + k as f32 * 48.0;
            if hov == Some(*hit) && can[k] {
                p.rrect(x, by, BTN_W, BTN_H, 4.0, pal.hover);
            }
            let ink = if can[k] { pal.text } else { pal.text_off };
            if *hit == Hit::Refresh && busy {
                // Turning while it reads the folder again.
                let turn = self.refresh_at.map_or(0.0, |t| t.elapsed().as_secs_f32()) * 540.0;
                p.rotated(turn % 360.0, x + BTN_W / 2.0, by + BTN_H / 2.0, || {
                    icon(*code, &ty.i16, x, by, BTN_W, BTN_H, accent)
                });
            } else {
                icon(*code, &ty.i16, x, by, BTN_W, BTN_H, ink);
            }
        }
        let (ax, aw, sw) = self.box_layout(w);
        let sx = w - sw - 10.0;
        for (bx, bw) in [(ax, aw), (sx, sw)] {
            p.rrect(bx, by, bw, BTN_H, 4.0, pal.field);
            p.stroke_rrect(bx + 0.5, by + 0.5, bw - 1.0, BTN_H - 1.0, 4.0, pal.field_edge, 1.0);
        }
        icon(0xE721, &ty.i12, sx + sw - 37.0, by, 24.0, BTN_H, pal.text2);
        if self.editing_addr {
            // Typing: the accent along the bottom, as a text box in focus has.
            p.rrect(ax + 1.0, by + BTN_H - 2.0, aw - 2.0, 2.0, 1.0, accent);
        } else {
            match &pics.root {
                Some(b) => p.image(b, ax + 13.0, by + 8.0, 16.0, 16.0, 1.0),
                None => icon(0xE721, &ty.i16, ax + 13.0, by + 8.0, 16.0, 16.0, pal.text2),
            }
            let crumb_hover = |x: f32, cw: f32| p.rrect(x, by + 4.0, cw, BTN_H - 8.0, 4.0, pal.hover);
            if hov == Some(Hit::CrumbRoot) {
                crumb_hover(ax + 39.0, CHEV);
            }
            icon(0xE76C, &ty.i12, ax + 39.0, by, CHEV, BTN_H, pal.text2);
            let parts = crumbs(&t.loc);
            let (layout, hidden) = self.crumb_layout(w);
            if hidden > 0 {
                if hov == Some(Hit::CrumbMore) {
                    crumb_hover(ax + 65.0, 32.0);
                }
                icon(0xE712, &ty.i12, ax + 65.0, by, 32.0, BTN_H, pal.text);
            }
            for (cx, cwid, i) in layout {
                if hov == Some(Hit::Crumb(i)) {
                    crumb_hover(cx, cwid);
                }
                if hov == Some(Hit::CrumbMenu(i)) {
                    crumb_hover(cx + cwid, CHEV);
                }
                p.text(&parts[i].0, &ty.t14, cx + 8.0, by, cwid - 16.0, BTN_H, Align::Left, pal.text);
                icon(0xE76C, &ty.i12, cx + cwid, by, CHEV, BTN_H, pal.text2);
            }
        }

        // The command bar.
        let cy = TABS_H + BAR_H;
        let layout = self.cmd_layout(w);
        for (i, (code, label)) in CMDS.iter().enumerate() {
            let (x, bw) = layout[i];
            if starts_group(i) {
                p.line(x - 6.5, cy + 12.0, x - 6.5, cy + 36.0, pal.sep, 1.0);
            }
            let enabled = self.cmd_enabled(i);
            let on = i == 10 && self.details;
            if on {
                p.rrect(x, cy + 8.0, bw, BTN_H, 4.0, pal.hover);
            }
            if hov == Some(Hit::Cmd(i)) && enabled {
                p.rrect(x, cy + 8.0, bw, BTN_H, 4.0, pal.hover);
            }
            let ink = if enabled { pal.text } else { pal.text_off };
            if labelled(i) {
                icon(*code, &ty.i16, x + 11.0, cy + 8.0, 16.0, BTN_H, ink);
                p.text(label, &ty.t12, x + 35.0, cy + 8.0, bw - 35.0, BTN_H, Align::Left, ink);
                if dropdown(i) {
                    icon(0xE70D, &ty.i10, x + bw - 22.0, cy + 8.0, 12.0, BTN_H, ink);
                }
            } else {
                icon(*code, &ty.i16, x, cy + 8.0, bw, BTN_H, ink);
            }
        }

        // The navigation pane.
        let y0 = TABS_H + BAR_H + CMD_H;
        let foot = h - STATUS_H;
        if self.nav {
            p.clip(0.0, y0, SIDE_W, foot - y0);
            let hl_w = SIDE_W - 11.0;
            for (i, ry) in self.side_layout() {
                let (g, name, loc) = &self.side[i];
                let on = *loc == t.loc;
                let lit = hov == Some(Hit::Side(i)) || (hov == Some(Hit::SideFold) && *loc == filesio::this_pc());
                self.row_fill(p, 3.0, ry, hl_w, SIDE_ROW - 1.0, on, lit);
                let indent = if self.side_child(i) { 8.0 } else { 0.0 };
                if *loc == filesio::this_pc() {
                    let code = if self.side_open { 0xE70D } else { 0xE76C };
                    icon(code, &ty.i10, 8.0, ry, 16.0, SIDE_ROW, pal.text2);
                }
                match pics.side.get(i).cloned().flatten() {
                    Some(b) => p.image(&b, 34.0 + indent, ry + 8.0, 16.0, 16.0, 1.0),
                    None => icon(*g as u32, &ty.i16, 34.0 + indent, ry + 8.0, 16.0, 16.0, pal.text),
                }
                let pinned = i >= 1 && i <= self.pin_n;
                let tw = hl_w - 52.0 - indent - if pinned { 28.0 } else { 6.0 };
                p.text(name, &ty.t12, 55.0 + indent, ry, tw, SIDE_ROW, Align::Left, pal.text);
                if pinned {
                    icon(0xE718, &ty.i12, hl_w - 20.0, ry, 24.0, SIDE_ROW, pal.text2);
                }
                if self.side_divider(i) {
                    p.line(16.0, ry + SIDE_ROW + 4.0, SIDE_W - 16.0, ry + SIDE_ROW + 4.0, pal.sep, 1.0);
                }
            }
            p.unclip();
        }

        // The list: the columns' titles, then the rows (or tiles).
        let (x0, cy0, cw, chh) = self.content();
        let lx = x0 + 4.0;
        p.clip(x0, cy0, cw, chh);
        let scroll = t.scroll;
        if self.view == View::Details {
            let mut cx = lx;
            for (i, (name, _)) in COLS.iter().enumerate() {
                let width = self.col_width(i, cw);
                if hov == Some(Hit::Header(i)) {
                    p.rrect(cx, cy0, width, HEAD_H - 2.0, 0.0, pal.hover);
                }
                let dragged =
                    matches!(hov, Some(Hit::HeaderEdge(e)) if e == i) || self.col_drag.is_some_and(|d| d.0 == i);
                let edge = if dragged { pal.text2 } else { pal.head_sep };
                p.line(cx + width - 0.5, cy0, cx + width - 0.5, cy0 + HEAD_H - 5.0, edge, 1.0);
                let tx = if i == 0 { cx + 25.0 } else { cx + 7.0 };
                p.text(name, &ty.t12, tx, cy0, width - (tx - cx) - 6.0, HEAD_H - 2.0, Align::Left, pal.text2);
                // The sort arrow: a small chevron over the middle of the title.
                if self.sort.0 == i {
                    let code = if self.sort.1 { 0xE70E } else { 0xE70D };
                    icon(code, &ty.i10, cx, cy0 - 3.0, width, 12.0, pal.text2);
                }
                cx += width;
            }
        }
        p.unclip();
        let top = self.rows_top();
        p.clip(x0, top, cw, chh - (top - cy0));
        // Group headings (Group by), above each group's first row.
        for (k, (_, name, count)) in t.heads.iter().enumerate() {
            let hy = top + self.head_y(k) - scroll;
            if hy + GROUP_H < top || hy > cy0 + chh {
                continue;
            }
            let label = format!("{name} ({count})");
            p.text(&label, &ty.t12, lx + 10.0, hy + 4.0, cw - 40.0, GROUP_H - 4.0, Align::Left, pal.group);
            let lw = p.measure(&label, &ty.t12);
            let ly = hy + GROUP_H / 2.0 + 2.0;
            p.line(lx + 20.0 + lw, ly, x0 + cw - 20.0, ly, pal.sep, 1.0);
        }
        for (pos, bmp) in &pics.items {
            let i = t.order[*pos];
            let item = &t.items[i];
            let (ix, iy, iw, ih) = self.item_rect(*pos);
            let iy = iy - scroll;
            let selected = t.sel.get(i) == Some(&true);
            let lit = hov == Some(Hit::Item(*pos));
            self.row_fill(p, ix, iy, iw, ih, selected, lit);
            let alpha = if item.hidden { 0.55 } else { 1.0 };
            match self.view {
                View::Details => {
                    if let Some(b) = bmp {
                        p.image(b, ix + 14.0, iy + (ih - 16.0) / 2.0, 16.0, 16.0, alpha);
                    }
                    let widths = self.col_widths(cw);
                    let nw = widths[0];
                    p.text(&item.name, &ty.t12, ix + 32.0, iy, nw - 38.0, ih, Align::Left, pal.text.alpha(alpha));
                    let mut cx = ix + nw;
                    let size = match (item.size, item.space) {
                        (_, Some((free, total))) => format!("{} free of {}", size_text(free), size_text(total)),
                        (Some(s), _) => size_text(s),
                        _ => String::new(),
                    };
                    for (k, text) in [&item.date, &item.kind, &size].iter().enumerate() {
                        let cwid = widths[k + 1];
                        let align = if k == 2 && item.space.is_none() { Align::Right } else { Align::Left };
                        p.text(text, &ty.t12, cx + 7.0, iy, cwid - 19.0, ih, align, pal.text2.alpha(alpha));
                        cx += cwid;
                    }
                }
                View::Icons => {
                    let s = 72.0 * self.icon_scale;
                    if let Some(b) = bmp {
                        p.image(b, ix + (iw - s) / 2.0, iy + 6.0, s, s, alpha);
                    }
                    if let Some((free, total)) = item.space {
                        // A drive's bar, as Explorer shows it: red when it's nearly full.
                        let frac = if total > 0 { 1.0 - free as f32 / total as f32 } else { 0.0 };
                        let track = if pal.dark { rgb(0x3A3A3A) } else { rgb(0xE6E6E6) };
                        p.rrect(ix + 12.0, iy + 8.0 + s, iw - 24.0, 5.0, 0.0, track);
                        let c = if frac > 0.9 { rgb(0xDA2626) } else { accent };
                        p.rrect(ix + 12.0, iy + 8.0 + s, (iw - 24.0) * frac, 5.0, 0.0, c);
                    }
                    p.text_wrap(&item.name, &ty.t12, ix + 4.0, iy + 16.0 + s, iw - 8.0, 32.0, pal.text.alpha(alpha));
                }
            }
        }
        if t.order.is_empty() {
            let msg = if t.loading {
                "Working on it\u{2026}"
            } else if !t.filter.is_empty() {
                "No items match your search."
            } else {
                "This folder is empty."
            };
            p.text(msg, &ty.t12, x0, top + 16.0, cw, 24.0, Align::Center, pal.text2);
        }
        // The box being dragged.
        if let Some(((ax_, ay_), (bx, by_))) = self.band {
            // Kept in the content's coordinates (it scrolls with it).
            let (ay_, by_) = (ay_ - scroll, by_ - scroll);
            let (l, tp) = (ax_.min(bx), ay_.min(by_));
            let (bw, bh) = ((ax_ - bx).abs(), (ay_ - by_).abs());
            p.rrect(l, tp, bw, bh, 0.0, accent.alpha(0.25));
            p.stroke_rrect(l + 0.5, tp + 0.5, bw - 1.0, bh - 1.0, 0.0, accent, 1.0);
        }
        // Scrollbar: Windows 11's thin one.
        let maxs = self.max_scroll();
        if maxs > 0.0 {
            let track = chh - (top - cy0);
            let thumb = (track * track / (track + maxs)).max(30.0);
            let ty_ = top + (track - thumb) * (scroll / maxs);
            p.rrect(x0 + cw - 7.0, ty_ + 2.0, 3.0, thumb - 4.0, 1.5, pal.scroll);
        }
        p.unclip();

        // The status bar, across the bottom: how many, what's selected, and the view buttons.
        let sy = h - STATUS_H;
        let sel = t.selected();
        let n = t.order.len();
        let mut parts = vec![if busy {
            "Refreshing\u{2026}".to_string()
        } else {
            format!("{n} item{}", if n == 1 { "" } else { "s" })
        }];
        if !sel.is_empty() {
            let bytes: u64 = sel.iter().filter_map(|&i| t.items[i].size).sum();
            parts.push(format!(
                "{} item{} selected{}",
                sel.len(),
                if sel.len() == 1 { "" } else { "s" },
                if bytes > 0 { format!("  {}", size_text(bytes)) } else { String::new() }
            ));
        }
        let mut x = 12.0;
        for (k, part) in parts.iter().enumerate() {
            if k > 0 {
                p.line(x + 0.5, sy + 7.0, x + 0.5, sy + STATUS_H - 7.0, pal.text_off, 1.0);
                x += 13.0;
            }
            let pw = p.measure(part, &ty.t12);
            p.text(part, &ty.t12, x, sy, pw + 4.0, STATUS_H, Align::Left, pal.text);
            x += pw + 12.0;
        }
        for (k, code) in [0xE8FDu32, 0xE8A9].iter().enumerate() {
            let vx = status_view_x(w, k as u8);
            let on = (k == 0) == (self.view == View::Details);
            if on || hov == Some(Hit::ViewButton(k as u8)) {
                p.rrect(vx, sy + 3.0, 24.0, STATUS_H - 6.0, 3.0, pal.hover);
            }
            if on {
                p.stroke_rrect(vx + 0.5, sy + 3.5, 23.0, STATUS_H - 7.0, 3.0, pal.text_off, 1.0);
            }
            icon(*code, &ty.i12, vx, sy, 24.0, STATUS_H, pal.text);
        }
        if busy {
            // A bar runs along the bottom of the command bar.
            let secs = self.refresh_at.map_or(0.0, |t| t.elapsed().as_secs_f32());
            let (seg, bar_y) = (w * 0.3, TABS_H + BAR_H + CMD_H - 3.0);
            let at = -seg + (secs * 0.9 % 1.0) * (w + seg);
            let (l, r) = (at.max(0.0), (at + seg).min(w));
            p.rrect(0.0, bar_y, w, 3.0, 0.0, accent.alpha(0.18));
            if r > l {
                p.rrect(l, bar_y, r - l, 3.0, 1.5, accent);
            }
        }
        if self.details {
            self.draw_pane(p, w, h, &pics.pane);
        }
    }

    /// The details pane: a big picture of what's selected (its thumbnail or
    /// icon) with its name, kind, size, dates and where it is; or, with
    /// several selected, how many and how big; or, with none, the folder.
    fn draw_pane(&self, p: &Painter, w: f32, h: f32, pic: &Option<ID2D1Bitmap>) {
        let (pal, ty) = (&self.pal, &self.ty);
        let t = self.t();
        let x = w - PANE_W;
        let y0 = TABS_H + BAR_H + CMD_H;
        let foot = h - STATUS_H;
        p.rrect(x, y0, PANE_W, foot - y0, 0.0, pal.base);
        p.line(x + 0.5, y0, x + 0.5, foot, pal.sep, 1.0);
        let sel = t.selected();
        let line = |label: &str, value: &str, y: &mut f32| {
            if value.is_empty() {
                return;
            }
            p.text(label, &ty.t12, x + 18.0, *y, PANE_W - 36.0, 16.0, Align::Left, pal.text2);
            let lines = ((p.measure(value, &ty.t12) * 1.1 / (PANE_W - 36.0)).ceil() as usize).clamp(1, 4) as f32;
            p.text_wrap(value, &ty.t12, x + 18.0, *y + 17.0, PANE_W - 36.0, 16.0 * lines, pal.text);
            *y += 24.0 + 16.0 * lines;
        };
        let mut y = y0 + 22.0;
        match sel[..] {
            [i] => {
                let item = &t.items[i];
                let (bw, bh) = (PANE_W - 36.0, 150.0);
                if let Some(b) = pic {
                    let s = 96.0f32.min(bh - 16.0);
                    p.image(b, x + 18.0 + (bw - s) / 2.0, y + (bh - s) / 2.0, s, s, 1.0);
                }
                y += bh + 14.0;
                p.text_wrap(&item.name, &ty.t14, x + 18.0, y, PANE_W - 36.0, 40.0, pal.text);
                y += 46.0;
                let size = match (item.size, item.space) {
                    (_, Some((free, total))) => format!("{} free of {}", size_text(free), size_text(total)),
                    (Some(s), _) => size_text(s),
                    _ => String::new(),
                };
                line("Type", &item.kind, &mut y);
                line(if item.folder { "Contents" } else { "Size" }, &size, &mut y);
                line("Date modified", &item.date, &mut y);
                if let Some(dir) = std::path::Path::new(&item.path).parent().filter(|_| !item.path.starts_with("::")) {
                    line("Location", &dir.to_string_lossy(), &mut y);
                }
            }
            [] => {
                let n = t.order.len();
                p.text(&t.loc.title(), &ty.t14, x + 18.0, y, PANE_W - 36.0, 28.0, Align::Left, pal.text);
                y += 40.0;
                line("Items", &format!("{n}"), &mut y);
                let files: u64 = t.items.iter().filter(|i| !i.folder).filter_map(|i| i.size).sum();
                if files > 0 {
                    line("Files' size", &size_text(files), &mut y);
                }
                p.text(
                    "Select a single file to get more information.",
                    &ty.t12,
                    x + 18.0,
                    y + 6.0,
                    PANE_W - 36.0,
                    20.0,
                    Align::Left,
                    pal.text2,
                );
            }
            _ => {
                let bytes: u64 = sel.iter().filter_map(|&i| t.items[i].size).sum();
                let label = format!("{} items selected", sel.len());
                p.text(&label, &ty.t14, x + 18.0, y, PANE_W - 36.0, 28.0, Align::Left, pal.text);
                y += 40.0;
                if bytes > 0 {
                    line("Total size", &size_text(bytes), &mut y);
                }
                let folders = sel.iter().filter(|&&i| t.items[i].folder).count();
                line("Folders", &format!("{folders}"), &mut y);
                line("Files", &format!("{}", sel.len() - folders), &mut y);
            }
        }
    }

    /// The address box's x and width, and the search box's width.
    fn box_layout(&self, w: f32) -> (f32, f32, f32) {
        let ax = 8.0 + 4.0 * 48.0;
        let sw = (w * 0.22).clamp(160.0, 360.0);
        (ax, (w - ax - sw - 20.0).max(80.0), sw)
    }

    /// Where the path's parts go in the address bar: (x, width, part) each,
    /// with its arrow after it, and how many of the first parts are left out
    /// (behind the "..." button) when the path is too long for the bar.
    fn crumb_layout(&self, w: f32) -> (Vec<(f32, f32, usize)>, usize) {
        let (ax, aw, _) = self.box_layout(w);
        let parts = crumbs(&self.t().loc);
        let width = |s: &str| self.measure.as_ref().map_or(s.len() as f32 * 8.0, |m| m.width(s, &self.ty.t14)) + 16.0;
        let widths: Vec<f32> = parts.iter().map(|(name, _)| width(name)).collect();
        // The place's icon and the arrow after it come first.
        const LEAD: f32 = 65.0;
        let room = aw - LEAD - 6.0;
        let mut first = 0;
        let need =
            |first: usize| widths[first..].iter().map(|w| w + CHEV).sum::<f32>() + if first > 0 { 32.0 } else { 0.0 };
        while first + 1 < parts.len() && need(first) > room {
            first += 1;
        }
        let mut x = ax + LEAD + if first > 0 { 32.0 } else { 0.0 };
        let mut out = Vec::new();
        for (i, wd) in widths.iter().enumerate().skip(first) {
            out.push((x, *wd, i));
            x += wd + CHEV;
        }
        (out, first)
    }

    fn place_boxes(&self) {
        let (w, _) = appwin::client(self.hwnd);
        let k = appwin::scale(self.hwnd);
        let (ax, aw, sw) = self.box_layout(w);
        let sx = w - sw - 10.0;
        unsafe {
            // The address box only while it's typed in; the path's parts show otherwise.
            let shown = if self.editing_addr { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
            let flags = SWP_NOZORDER | SWP_NOACTIVATE;
            let _ = SetWindowPos(self.addr, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | flags | shown);
            let _ = SetWindowPos(
                self.addr,
                None,
                ((ax + 12.0) * k) as i32,
                ((TABS_H + 14.0) * k) as i32,
                ((aw - 24.0) * k) as i32,
                (20.0 * k) as i32,
                flags,
            );
            let _ = SetWindowPos(
                self.search,
                None,
                ((sx + 12.0) * k) as i32,
                ((TABS_H + 14.0) * k) as i32,
                ((sw - 50.0) * k) as i32,
                (20.0 * k) as i32,
                flags,
            );
        }
    }

    // ------------------------------------------------------------ input --

    fn on_down(&mut self, x: f32, y: f32, ctrl: bool, shift: bool) {
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        match self.hit(x, y) {
            Some(Hit::Tab(i)) => self.switch(i),
            Some(Hit::TabClose(i)) => self.close_tab(i),
            Some(Hit::NewTab) => self.new_tab(Loc::Home),
            Some(Hit::Back) => self.back(),
            Some(Hit::Forward) => self.forward(),
            Some(Hit::Up) => self.up(),
            Some(Hit::Refresh) => self.refresh(),
            Some(Hit::Cmd(i)) if self.cmd_enabled(i) => self.command(i),
            Some(Hit::Cmd(_)) => {}
            Some(Hit::ViewButton(k)) => {
                if k == 0 {
                    self.view_size(6);
                } else {
                    self.view_size(2);
                }
            }
            Some(Hit::SideFold) => self.side_open = !self.side_open,
            Some(Hit::CrumbRoot) => self.root_menu(),
            Some(Hit::CrumbMore) => self.hidden_crumbs_menu(),
            Some(Hit::Caption(b)) => appwin::caption_press(self.hwnd, b),
            Some(Hit::Side(i)) => {
                let loc = self.side[i].2.clone();
                self.go(loc);
            }
            Some(Hit::Header(i)) => {
                self.sort = if self.sort.0 == i { (i, !self.sort.1) } else { (i, i != 1 && i != 3) };
                self.resort();
                self.remember();
            }
            Some(Hit::HeaderEdge(i)) => {
                let (_, _, cw, _) = self.content();
                self.col_drag = Some((i, x, self.col_widths(cw)));
                unsafe {
                    SetCapture(self.hwnd);
                }
            }
            Some(Hit::Crumb(i)) => {
                if let Some((_, loc)) = crumbs(&self.t().loc).into_iter().nth(i) {
                    if loc != self.t().loc {
                        self.go(loc);
                    }
                }
            }
            Some(Hit::CrumbMenu(i)) => self.crumb_menu(i),
            Some(Hit::Address) => self.focus_addr(),
            Some(Hit::Item(pos)) => {
                let i = self.t().order[pos];
                let double = unsafe { GetDoubleClickTime() } as u128;
                if let Some((j, t)) = self.last_click {
                    if j == pos && t.elapsed().as_millis() <= double && !ctrl && !shift {
                        self.last_click = None;
                        self.open_item(i);
                        return;
                    }
                }
                self.last_click = Some((pos, Instant::now()));
                let tab = self.tab();
                if ctrl {
                    tab.sel[i] = !tab.sel[i];
                    tab.anchor = Some(pos);
                } else if shift {
                    let a = tab.anchor.unwrap_or(pos);
                    let (lo, hi) = (a.min(pos), a.max(pos));
                    tab.sel.iter_mut().for_each(|s| *s = false);
                    for p in lo..=hi {
                        let k = tab.order[p];
                        tab.sel[k] = true;
                    }
                } else {
                    if !tab.sel[i] {
                        tab.sel.iter_mut().for_each(|s| *s = false);
                        tab.sel[i] = true;
                    }
                    tab.anchor = Some(pos);
                    self.press = Some((x, y, pos));
                }
                unsafe {
                    SetCapture(self.hwnd);
                }
            }
            Some(Hit::Empty) => {
                if !ctrl {
                    self.tab().sel.iter_mut().for_each(|s| *s = false);
                }
                let sy = y + self.t().scroll;
                self.band = Some(((x, sy), (x, sy)));
                unsafe {
                    SetCapture(self.hwnd);
                }
            }
            Some(Hit::Pane) | None => {}
        }
        appwin::redraw(self.hwnd);
    }

    fn on_move(&mut self, x: f32, y: f32) {
        if let Some((i, from, start)) = self.col_drag {
            // The column and the one after it share what they had between
            // them: widening one narrows the other, and the rest stay put.
            if i + 1 < start.len() {
                let pair = start[i] + start[i + 1];
                let mut w = start;
                w[i] = (start[i] + x - from).clamp(COL_MIN[i], (pair - COL_MIN[i + 1]).max(COL_MIN[i]));
                w[i + 1] = pair - w[i];
                if let Some(shares) = normalised(w) {
                    self.col_share = shares;
                }
            }
            appwin::redraw(self.hwnd);
            return;
        }
        if let Some((px, py, _)) = self.press {
            if (x - px).hypot(y - py) > 6.0 {
                self.press = None;
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.drag_out();
                return;
            }
        }
        if let Some((a, _)) = self.band {
            let sy = y + self.t().scroll;
            self.band = Some((a, (x, sy)));
            let (l, tp, r, b) = (a.0.min(x), a.1.min(sy), a.0.max(x), a.1.max(sy));
            let (_, _, cw, _) = self.content();
            let n = self.t().order.len();
            let mut hits = Vec::new();
            for pos in 0..n {
                let (ix, iy, iw, ih) = self.item_rect(pos);
                let iw = if self.view == View::Details { self.name_w(cw) } else { iw };
                if ix < r && ix + iw > l && iy < b && iy + ih > tp {
                    hits.push(self.t().order[pos]);
                }
            }
            let tab = self.tab();
            tab.sel.iter_mut().for_each(|s| *s = false);
            for i in hits {
                tab.sel[i] = true;
            }
            appwin::redraw(self.hwnd);
            return;
        }
        let h = self.hit(x, y);
        if h != self.hovered {
            self.hovered = h;
            appwin::redraw(self.hwnd);
        }
    }

    fn on_up(&mut self) {
        self.press = None;
        if self.col_drag.take().is_some() {
            self.remember();
        }
        if self.band.take().is_some() {
            appwin::redraw(self.hwnd);
        }
        unsafe {
            let _ = ReleaseCapture();
        }
    }

    fn on_right(&mut self, x: f32, y: f32) {
        match self.hit(x, y) {
            Some(Hit::Item(pos)) => {
                let i = self.t().order[pos];
                if !self.t().sel[i] {
                    self.select_only(Some(pos));
                }
                let paths = self.t().selected_paths();
                appwin::redraw(self.hwnd);
                crate::deskicons::item_menu(self.hwnd, &paths);
            }
            Some(Hit::Empty) => {
                self.tab().sel.iter_mut().for_each(|s| *s = false);
                appwin::redraw(self.hwnd);
                self.background_menu();
            }
            Some(Hit::Side(i)) => self.side_menu(i),
            Some(Hit::Back) => self.history_menu(true),
            Some(Hit::Forward) => self.history_menu(false),
            Some(Hit::Tab(i)) | Some(Hit::TabClose(i)) => {
                use crate::menu::{item, show, Place};
                match show(
                    self.hwnd,
                    vec![item(1, "Duplicate tab"), item(2, "Close tab"), item(3, "Close other tabs")],
                    Place::Cursor,
                ) {
                    1 => {
                        let loc = self.tabs[i].loc.clone();
                        self.new_tab(loc);
                    }
                    2 => self.close_tab(i),
                    3 => {
                        let keep = self.tabs.remove(i);
                        self.tabs = vec![keep];
                        self.switch(0);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Bigger or smaller icons (Ctrl+wheel, Ctrl+plus and minus): Icons view
    /// steps through its sizes; bigger from Details goes to Icons, and
    /// smaller than the smallest goes back to Details.
    fn zoom_icons(&mut self, bigger: bool) {
        if self.view == View::Details {
            if !bigger {
                return;
            }
            self.view = View::Icons;
        } else {
            let at = ICON_STEPS.iter().position(|s| *s >= self.icon_scale - 0.01).unwrap_or(2);
            if bigger {
                self.icon_scale = ICON_STEPS[(at + 1).min(ICON_STEPS.len() - 1)];
            } else if at == 0 {
                self.view = View::Details;
            } else {
                self.icon_scale = ICON_STEPS[at - 1];
            }
        }
        let m = self.max_scroll();
        let t = self.tab();
        t.scroll = t.scroll.min(m);
        self.remember();
        appwin::redraw(self.hwnd);
    }

    /// Explorer's Ctrl+Shift+1 to 4 (extra large to small icons) and 6 (details).
    fn view_size(&mut self, n: u8) {
        match n {
            1 => (self.view, self.icon_scale) = (View::Icons, ICON_STEPS[4]),
            2 => (self.view, self.icon_scale) = (View::Icons, ICON_STEPS[3]),
            3 => (self.view, self.icon_scale) = (View::Icons, ICON_STEPS[2]),
            4 => (self.view, self.icon_scale) = (View::Icons, ICON_STEPS[1]),
            _ => self.view = View::Details,
        }
        let m = self.max_scroll();
        let t = self.tab();
        t.scroll = t.scroll.min(m);
        self.remember();
    }

    fn on_wheel(&mut self, delta: i16) {
        if unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 {
            self.zoom_icons(delta > 0);
            return;
        }
        let m = self.max_scroll();
        let row = if self.view == View::Details { self.row_h() } else { ROW_H };
        let t = self.tab();
        t.scroll = (t.scroll - delta as f32 / 120.0 * row * 3.0).clamp(0.0, m);
        appwin::redraw(self.hwnd);
    }

    fn on_key(&mut self, vk: u16) -> bool {
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
        let (ctrl, shift, alt) = (down(VK_CONTROL), down(VK_SHIFT), down(VK_MENU));
        let n = self.t().order.len();
        let k = VIRTUAL_KEY(vk);
        match k {
            VK_RETURN if alt => self.properties(),
            VK_RETURN => self.open_selected(),
            VK_BACK => self.up(),
            VK_ESCAPE => self.tab().sel.iter_mut().for_each(|s| *s = false),
            VK_LEFT if alt => self.back(),
            VK_RIGHT if alt => self.forward(),
            VK_UP if alt => self.up(),
            VK_F2 => self.start_rename(),
            VK_P if alt && shift => {
                self.details = !self.details;
                self.remember();
            }
            VK_F5 => self.refresh(),
            VK_DELETE => self.delete(shift),
            VK_F4 => self.focus_addr(),
            VK_D if alt => self.focus_addr(),
            VK_F3 => unsafe {
                let _ = SetFocus(Some(self.search));
            },
            VK_TAB if ctrl => {
                let len = self.tabs.len();
                let next = if shift { (self.cur + len - 1) % len } else { (self.cur + 1) % len };
                self.switch(next);
            }
            VK_UP | VK_DOWN | VK_LEFT | VK_RIGHT | VK_HOME | VK_END | VK_PRIOR | VK_NEXT if n > 0 => {
                let per = if self.view == View::Icons { self.cols() as isize } else { 1 };
                let page = ((self.content().3 / self.row_h()) as isize).max(1);
                let at = self.t().anchor.map_or(-1, |a| a as isize);
                let step = match k {
                    VK_UP => -per,
                    VK_DOWN => per,
                    VK_LEFT => -1,
                    VK_RIGHT => 1,
                    VK_HOME => -(n as isize),
                    VK_END => n as isize,
                    VK_PRIOR => -page * per,
                    _ => page * per,
                };
                if self.view == View::Details && matches!(k, VK_LEFT | VK_RIGHT) {
                    return true;
                }
                let mut next = if at < 0 { 0 } else { (at + step).clamp(0, n as isize - 1) } as usize;
                if at >= 0 && self.view == View::Icons && !self.t().heads.is_empty() && matches!(k, VK_UP | VK_DOWN) {
                    next = self.tile_beside(at as usize, k == VK_DOWN);
                }
                self.select_only(Some(next));
            }
            _ if ctrl => match vk as u8 {
                // Ctrl+Shift+1 to 4, 6: the size of icons, details. Ctrl+1 to 9: that tab.
                d @ (b'1'..=b'4' | b'6') if shift => self.view_size(d - b'0'),
                d @ b'1'..=b'9' => {
                    let tab = (d - b'1') as usize;
                    if tab < self.tabs.len() {
                        self.switch(tab);
                    }
                }
                // Ctrl+plus and minus (the keypad's too).
                0xBB | 0x6B => self.zoom_icons(true),
                0xBD | 0x6D => self.zoom_icons(false),
                b'R' => self.refresh(),
                b'Z' => self.undo(),
                b'Y' => self.redo_last(),
                b'D' => self.delete(false),
                b'E' => unsafe {
                    let _ = SetFocus(Some(self.search));
                },
                // Ctrl+Shift+. shows and hides hidden items, as in Explorer.
                0xBE if shift => {
                    self.show_hidden = !self.show_hidden;
                    self.remember();
                    self.load();
                }
                b'A' => self.tab().sel.iter_mut().for_each(|s| *s = true),
                b'C' => self.copy(false),
                b'X' => self.copy(true),
                b'V' => self.paste(),
                b'T' => self.new_tab(Loc::Home),
                b'W' => self.close_tab(self.cur),
                b'L' => self.focus_addr(),
                b'F' => unsafe {
                    let _ = SetFocus(Some(self.search));
                },
                b'N' if shift => self.new_folder(),
                b'N' => {
                    let loc = self.t().loc.clone();
                    if let Loc::Path(p) = loc {
                        appwin::launch(&["--files", &p]);
                    }
                }
                _ => return false,
            },
            _ => return false,
        }
        appwin::redraw(self.hwnd);
        true
    }

    /// Typing a name's first letters jumps to it.
    fn on_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        let (typed, at) = &mut self.typed;
        if at.elapsed().as_millis() > 900 {
            typed.clear();
        }
        typed.push(c.to_lowercase().next().unwrap_or(c));
        *at = Instant::now();
        let want = typed.clone();
        let t = self.t();
        if let Some(pos) = t.order.iter().position(|&i| t.items[i].name.to_lowercase().starts_with(&want)) {
            self.select_only(Some(pos));
            appwin::redraw(self.hwnd);
        }
    }

    fn focus_addr(&mut self) {
        self.editing_addr = true;
        self.place_boxes();
        unsafe {
            let _ = SetWindowTextW(self.addr, &HSTRING::from(self.t().loc.address()));
            let _ = SetFocus(Some(self.addr));
            SendMessageW(self.addr, 0x00B1, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
    }

    fn edit_text(h: HWND) -> String {
        let mut buf = [0u16; 1024];
        let n = unsafe { GetWindowTextW(h, &mut buf) } as usize;
        String::from_utf16_lossy(&buf[..n])
    }

    /// Enter or Esc in one of the text boxes.
    fn on_edit_key(&mut self, id: usize, enter: bool) {
        match id {
            ID_ADDR => {
                if enter {
                    let text = Self::edit_text(self.addr);
                    match Loc::parse(&text) {
                        Some(loc) => self.go(loc),
                        None => {
                            // Not a place: maybe a program or a web address.
                            open_file(self.hwnd, text.trim());
                        }
                    }
                } else {
                    self.set_title();
                }
                unsafe {
                    let _ = SetFocus(Some(self.hwnd));
                }
            }
            ID_SEARCH => {
                let q = Self::edit_text(self.search);
                if enter && !q.trim().is_empty() {
                    // Every folder below this one (the file index makes it instant).
                    let scope = match &self.t().loc {
                        Loc::Path(p) => p.clone(),
                        Loc::Search(s, _) => s.clone(),
                        _ => String::new(),
                    };
                    let scope = if scope.is_empty() { "C:\\".to_string() } else { scope };
                    self.tab().filter.clear();
                    self.go(Loc::Search(scope, q.trim().to_string()));
                    unsafe {
                        let _ = SetWindowTextW(self.search, &HSTRING::from(q.trim()));
                    }
                } else if !enter {
                    self.clear_search();
                    self.tab().filter.clear();
                    self.resort();
                    unsafe {
                        let _ = SetFocus(Some(self.hwnd));
                    }
                }
            }
            ID_RENAME => self.finish_rename(enter),
            _ => {}
        }
        appwin::redraw(self.hwnd);
    }

    /// Files dragged out of the window (to an app, the desktop, another folder).
    fn drag_out(&mut self) {
        let paths = self.t().selected_paths();
        if paths.is_empty() {
            return;
        }
        unsafe {
            let mut pidls = Vec::new();
            for p in &paths {
                let mut pidl = std::ptr::null_mut();
                if SHParseDisplayName(&HSTRING::from(p.as_str()), None, &mut pidl, 0, None).is_ok() {
                    pidls.push(pidl as *const windows::Win32::UI::Shell::Common::ITEMIDLIST);
                }
            }
            let array = SHCreateShellItemArrayFromIDLists(&pidls);
            for p in &pidls {
                CoTaskMemFree(Some(*p as *const _));
            }
            let Ok(data) = array.and_then(|a| a.BindToHandler::<_, IDataObject>(None, &BHID_DataObject)) else {
                return;
            };
            let source: IDropSource = DragSource.into();
            let mut effect = DROPEFFECT_NONE;
            let _ = DoDragDrop(&data, &source, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK, &mut effect);
        }
    }
}

/// "file10" after "file9", as Explorer sorts.
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (wa, wb): (Vec<u16>, Vec<u16>) = (a.encode_utf16().chain([0]).collect(), b.encode_utf16().chain([0]).collect());
    let r = unsafe { StrCmpLogicalW(PCWSTR(wa.as_ptr()), PCWSTR(wb.as_ptr())) };
    r.cmp(&0)
}

/// Opens a file with its app. Pictures open in Cascade's viewer when their
/// app can't open without Explorer (the Photos app is a Store app).
fn open_file(owner: HWND, path: &str) {
    if crate::viewer::should_handle(path) {
        appwin::launch(&["--view", path]);
        return;
    }
    crate::shell::open_from(Some(owner), path);
}

/// The folder a shortcut (.lnk) points to, if it points to one.
fn shortcut_folder(path: &str) -> Option<String> {
    if !path.to_ascii_lowercase().ends_with(".lnk") {
        return None;
    }
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = windows::core::Interface::cast(&link).ok()?;
        file.Load(&HSTRING::from(path), STGM_READ).ok()?;
        let mut buf = [0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
        let target = String::from_utf16_lossy(&buf[..n]);
        std::path::Path::new(&target).is_dir().then_some(target)
    }
}

static PENDING_RENAME: Mutex<Option<String>> = Mutex::new(None);

#[implement(IDropSource)]
struct DragSource;

impl IDropSource_Impl for DragSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        const MK_LBUTTON: u32 = 1;
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

#[implement(IDropTarget)]
struct DropHere;

fn set_effect(out: *mut DROPEFFECT, e: DROPEFFECT) {
    if let Some(o) = unsafe { out.as_mut() } {
        *o = e;
    }
}

/// Where a drop goes: the folder under the pointer, or the one open.
fn drop_dir(pt: &POINTL) -> Option<String> {
    with(|s| {
        let mut p = POINT { x: pt.x, y: pt.y };
        unsafe {
            let _ = ScreenToClient(s.hwnd, &mut p);
        }
        let k = appwin::scale(s.hwnd);
        if let Some(Hit::Item(pos)) = s.hit(p.x as f32 / k, p.y as f32 / k) {
            let item = &s.t().items[s.t().order[pos]];
            if item.folder && std::path::Path::new(&item.path).is_dir() {
                return Some(item.path.clone());
            }
        }
        s.t().loc.dir()
    })
    .flatten()
}

fn how(keys: MODIFIERKEYS_FLAGS, dir: &str, first: Option<&str>) -> DROPEFFECT {
    const MK_CONTROL: u32 = 0x8;
    const MK_SHIFT: u32 = 0x4;
    if keys.0 & MK_CONTROL != 0 {
        return DROPEFFECT_COPY;
    }
    if keys.0 & MK_SHIFT != 0 {
        return DROPEFFECT_MOVE;
    }
    let drive = |p: &str| p.get(..2).map(str::to_ascii_lowercase);
    match first {
        Some(f) if drive(f) == drive(dir) => DROPEFFECT_MOVE,
        _ => DROPEFFECT_COPY,
    }
}

static DRAG_FIRST: Mutex<Option<String>> = Mutex::new(None);

impl IDropTarget_Impl for DropHere_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        out: *mut DROPEFFECT,
    ) -> WinResult<()> {
        let files = data.ok().map(crate::drop::files).unwrap_or_default();
        *DRAG_FIRST.lock().unwrap_or_else(|e| e.into_inner()) = files.first().cloned();
        let e = match (files.is_empty(), drop_dir(pt)) {
            (false, Some(d)) => how(keys, &d, files.first().map(String::as_str)),
            _ => DROPEFFECT_NONE,
        };
        set_effect(out, e);
        Ok(())
    }

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, out: *mut DROPEFFECT) -> WinResult<()> {
        let first = DRAG_FIRST.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let e = match (first, drop_dir(pt)) {
            (Some(f), Some(d)) => how(keys, &d, Some(&f)),
            _ => DROPEFFECT_NONE,
        };
        set_effect(out, e);
        Ok(())
    }

    fn DragLeave(&self) -> WinResult<()> {
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        out: *mut DROPEFFECT,
    ) -> WinResult<()> {
        let files = data.ok().map(crate::drop::files).unwrap_or_default();
        let Some(dir) = drop_dir(pt) else { return Ok(()) };
        let files: Vec<String> = files
            .into_iter()
            .filter(|f| std::path::Path::new(f).parent() != Some(std::path::Path::new(&dir)))
            .collect();
        if files.is_empty() {
            set_effect(out, DROPEFFECT_NONE);
            return Ok(());
        }
        let e = how(keys, &dir, files.first().map(String::as_str));
        set_effect(out, e);
        std::thread::spawn(move || unsafe {
            let from: Vec<u16> = files.iter().flat_map(|f| f.encode_utf16().chain([0])).chain([0]).collect();
            let to: Vec<u16> = dir.encode_utf16().chain([0, 0]).collect();
            let mut op = SHFILEOPSTRUCTW {
                wFunc: if e == DROPEFFECT_MOVE { FO_MOVE } else { FO_COPY },
                pFrom: PCWSTR(from.as_ptr()),
                pTo: PCWSTR(to.as_ptr()),
                fFlags: FOF_ALLOWUNDO.0 as u16,
                ..Default::default()
            };
            let _ = SHFileOperationW(&mut op);
        });
        Ok(())
    }
}

unsafe extern "system" fn edit_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM, id: usize, _: usize) -> LRESULT {
    match m {
        WM_KEYDOWN if wp.0 == 0x0D || wp.0 == 0x1B => {
            if let Ok(parent) = GetParent(hwnd) {
                let _ = PostMessageW(Some(parent), EDIT_KEY, WPARAM(id), LPARAM((wp.0 == 0x0D) as isize));
            }
            return LRESULT(0);
        }
        WM_CHAR if wp.0 == 0x0D || wp.0 == 0x1B => return LRESULT(0),
        WM_KILLFOCUS if id == ID_RENAME => {
            if let Ok(parent) = GetParent(hwnd) {
                let _ = PostMessageW(Some(parent), EDIT_KEY, WPARAM(id), LPARAM(1));
            }
        }
        _ => {}
    }
    DefSubclassProc(hwnd, m, wp, lp)
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let shift_ctrl = |wp: WPARAM| (wp.0 & 0x8 != 0, wp.0 & 0x4 != 0);
    match m {
        WM_PAINT => {
            if with(|s| s.paint()).is_none() {
                return DefWindowProcW(hwnd, m, wp, lp);
            }
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_NCCALCSIZE => {
            if let Some(r) = appwin::frameless_calc(hwnd, wp, lp) {
                return r;
            }
        }
        WM_NCHITTEST => {
            if let Some(r) = appwin::edge_hit(hwnd, lp) {
                return r;
            }
            // The tab row's empty part moves the window, as a title bar does.
            let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            let k = appwin::scale(hwnd);
            let (x, y) = (pt.x as f32 / k, pt.y as f32 / k);
            if (0.0..TABS_H).contains(&y) && with(|s| s.hit(x, y)).flatten().is_none() {
                return LRESULT(HTCAPTION as isize);
            }
        }
        WM_SIZE | WM_DPICHANGED => {
            with(|s| {
                s.place_boxes();
                let mx = s.max_scroll();
                let t = s.tab();
                t.scroll = t.scroll.min(mx);
            });
            appwin::redraw(hwnd);
        }
        WM_CTLCOLOREDIT => {
            let dc = HDC(wp.0 as *mut _);
            if let Some((b, pal)) = with(|s| (s.brush, s.pal)) {
                SetTextColor(dc, Pal::colorref(pal.text));
                SetBkColor(dc, Pal::colorref(pal.field));
                return LRESULT(b.0 as isize);
            }
        }
        // Mica shows behind the tabs only while the window's in front, as in Explorer.
        WM_ACTIVATE => {
            with(|s| {
                s.active = wp.0 & 0xFFFF != 0;
                appwin::redraw(s.hwnd);
            });
        }
        // Windows' app mode (light or dark) or the wallpaper changed.
        WM_SETTINGCHANGE => {
            let what =
                if lp.0 != 0 { PCWSTR(lp.0 as *const u16).to_string().unwrap_or_default() } else { String::new() };
            if what == "ImmersiveColorSet" || wp.0 == SPI_SETDESKWALLPAPER.0 as usize {
                with(|s| s.apply_theme());
            }
        }
        WM_COMMAND => {
            // The search box filters as you type.
            let (id, code) = (wp.0 & 0xFFFF, (wp.0 >> 16) & 0xFFFF);
            // The address box lost the keyboard: the path's parts come back.
            if id == ID_ADDR && code == 0x0200 {
                appwin::redraw(hwnd);
            }
            if id == ID_SEARCH && code == 0x0300 {
                with(|s| {
                    let q = State::edit_text(s.search);
                    if !matches!(s.t().loc, Loc::Search(..)) {
                        s.tab().filter = q;
                        s.resort();
                        s.tab().scroll = 0.0;
                        appwin::redraw(s.hwnd);
                    }
                });
            }
        }
        EDIT_KEY => {
            with(|s| s.on_edit_key(wp.0, lp.0 != 0));
        }
        LOADED => {
            with(|s| {
                s.loaded();
                // A folder just made: selected, with its name ready to type.
                let pending = PENDING_RENAME.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(p) = pending {
                    let pos = s.t().order.iter().position(|&i| s.t().items[i].path.eq_ignore_ascii_case(&p));
                    if let Some(pos) = pos {
                        s.select_only(Some(pos));
                        s.start_rename();
                    }
                }
            });
        }
        WATCHED => {
            with(|s| {
                if s.t().loc.dir().is_some() && s.rename.is_none() {
                    s.load();
                }
            });
        }
        ICONS => {
            with(|s| s.icons_arrived());
        }
        MICA => {
            with(|s| s.mica_arrived());
        }
        SIZED => {
            with(|s| s.sized());
        }
        msg::DESK_MENU_SHOW => {
            crate::deskicons::show_asked(hwnd);
        }
        msg::DESK_RENAME => {
            with(|s| s.start_rename());
        }
        msg::DESK_ICONS => {
            with(|s| {
                if s.t().loc.dir().is_none() {
                    s.load();
                }
            });
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let (x, y) = appwin::mouse(hwnd, lp);
            let (ctrl, shift) = shift_ctrl(wp);
            with(|s| s.on_down(x, y, ctrl, shift));
        }
        // Over a column's edge (or dragging one): the resize pointer, as Explorer shows.
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(hwnd, &mut pt);
            let k = appwin::scale(hwnd);
            let edge = with(|s| {
                s.col_drag.is_some() || matches!(s.hit(pt.x as f32 / k, pt.y as f32 / k), Some(Hit::HeaderEdge(_)))
            });
            if edge == Some(true) {
                SetCursor(LoadCursorW(None, IDC_SIZEWE).ok());
                return LRESULT(1);
            }
        }
        WM_MOUSEMOVE => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| s.on_move(x, y));
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut t);
        }
        0x02A3 => {
            // WM_MOUSELEAVE
            with(|s| {
                if s.hovered.take().is_some() {
                    appwin::redraw(hwnd);
                }
            });
        }
        WM_LBUTTONUP => {
            with(|s| s.on_up());
        }
        WM_MBUTTONUP => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| match s.hit(x, y) {
                Some(Hit::Tab(i)) | Some(Hit::TabClose(i)) => s.close_tab(i),
                Some(Hit::Item(pos)) => {
                    let item = s.t().items[s.t().order[pos]].clone();
                    if item.folder {
                        let loc = if std::path::Path::new(&item.path).is_dir() {
                            Loc::Path(item.path)
                        } else {
                            Loc::Shell(item.path, item.name)
                        };
                        s.new_tab(loc);
                    }
                }
                _ => {}
            });
        }
        WM_RBUTTONUP => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| s.on_right(x, y));
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16;
            with(|s| s.on_wheel(delta));
        }
        WM_TIMER if wp.0 == TIMER_BUSY => {
            with(|s| s.tick());
        }
        WM_XBUTTONUP => {
            let back = (wp.0 >> 16) & 0xFFFF == 1;
            with(|s| if back { s.back() } else { s.forward() });
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if with(|s| s.on_key(wp.0 as u16)) == Some(true) {
                return LRESULT(0);
            }
        }
        WM_CHAR => {
            if let Some(c) = char::from_u32(wp.0 as u32) {
                with(|s| s.on_char(c));
            }
        }
        WM_DESTROY => {
            let _ = RevokeDragDrop(hwnd);
            PostQuitMessage(0);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, m, wp, lp)
}

/// `cascade.exe --files [folder]`.
pub fn run(start: Option<String>) {
    appwin::init();
    let Ok(gfx) = Gfx::new() else { return };
    let ty = Type::new(&gfx);
    let Some(hwnd) = appwin::create(w!("CascadeFiles"), "Files", 1200, 760, Some(proc)) else { return };
    appwin::frameless(hwnd);
    appwin::identity(hwnd, "Cascade.Files");
    // Shell menus and their commands report back to this window (see `deskicons::item_menu`).
    msg::set_target(hwnd);
    let loc = start.as_deref().and_then(Loc::parse).unwrap_or(Loc::Home);
    unsafe {
        let inst = appwin::instance();
        let mk = |id: usize| {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                w!(""),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x80),
                0,
                0,
                10,
                10,
                Some(hwnd),
                Some(HMENU(id as *mut _)),
                Some(inst),
                None,
            )
            .unwrap_or_default()
        };
        let (addr, search) = (mk(ID_ADDR), mk(ID_SEARCH));
        let k = appwin::scale(hwnd);
        let font = CreateFontW(
            (-14.0 * k) as i32,
            0,
            0,
            0,
            400,
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
        for e in [addr, search] {
            SendMessageW(e, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        }
        let _ = SetWindowSubclass(addr, Some(edit_proc), ID_ADDR, 0);
        let _ = SetWindowSubclass(search, Some(edit_proc), ID_SEARCH, 0);
        // EM_SETCUEBANNER: a grey hint in the empty search box.
        SendMessageW(search, 0x1501, Some(WPARAM(1)), Some(LPARAM(w!("Search").as_ptr() as isize)));
        let (icon_tx, icon_done) = filesio::icon_worker(hwnd, ICONS);
        let target: IDropTarget = DropHere.into();
        let _ = RegisterDragDrop(hwnd, &target);
        let store = ViewStore::load();
        filesio::set_ext_mode(store.exts);
        let pins = store.pins.clone().unwrap_or_else(default_pins);
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                gfx_generation: gfx.generation,
                gfx: Some(gfx),
                tabs: vec![Tab::new(loc)],
                cur: 0,
                view: View::Details,
                sort: (0, true),
                show_hidden: store.hidden,
                icons: HashMap::new(),
                asked: HashSet::new(),
                icon_tx,
                icon_done,
                addr,
                search,
                font,
                brush: HBRUSH::default(),
                hovered: None,
                band: None,
                press: None,
                rename: None,
                last_click: None,
                side: sidebar(&pins),
                typed: (String::new(), Instant::now()),
                col_share: store.cols.unwrap_or(COL_SHARES),
                col_drag: None,
                editing_addr: false,
                group: 0,
                measure: crate::gfx::Measure::new().ok(),
                icon_scale: store.scale.unwrap_or(1.0),
                refresh_at: None,
                pin_n: pins.len(),
                nav: !store.hide_nav,
                details: store.details,
                compact: store.compact,
                exts: store.exts,
                undo: Vec::new(),
                redo: Vec::new(),
                store,
                pal: DARK,
                ty,
                active: true,
                side_open: true,
            })
        });
    }
    with(|s| {
        s.apply_theme();
        s.place_boxes();
        s.load();
    });
    appwin::run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_in_parts() {
        let parts = crumbs(&Loc::Path("C:\\Users\\you\\Downloads".into()));
        let names: Vec<&str> = parts.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["This PC", "Local Disk (C:)", "Users", "you", "Downloads"]);
        assert_eq!(parts[2].1, Loc::Path("C:\\Users".into()));
        assert_eq!(parts[1].1, Loc::Path("C:\\".into()));
        assert_eq!(crumbs(&Loc::Home).len(), 1);
    }

    #[test]
    fn remembers_how_folders_are_shown() {
        let mut store = ViewStore::default();
        store.hidden = true;
        store.cols = Some([0.4, 0.2, 0.2, 0.2]);
        store.hide_nav = true;
        store.details = true;
        store.compact = true;
        store.exts = 1;
        let key = folder_key(&Loc::Path("C:\\Users\\You\\Downloads\\".into())).unwrap();
        assert_eq!(key, "c:\\users\\you\\downloads");
        store.folders.insert(key.clone(), FolderView { view: View::Icons, sort: (1, false), group: 2 });
        let back = ViewStore::parse(&store.text());
        assert!(back.hidden);
        assert_eq!(back.cols, Some([0.4, 0.2, 0.2, 0.2]));
        assert!(back.hide_nav && back.details && back.compact);
        assert_eq!(back.exts, 1);
        // Nothing saved: the navigation pane shows, the rest is off.
        let plain = ViewStore::parse("");
        assert!(!plain.hide_nav && !plain.details && !plain.compact && plain.exts == 0);
        assert!(back.folders.get(&key) == Some(&FolderView { view: View::Icons, sort: (1, false), group: 2 }));
        // Search results aren't remembered; nonsense lines are skipped.
        assert!(folder_key(&Loc::Search("C:\\".into(), "x".into())).is_none());
        assert!(ViewStore::parse("f|c:\\x|details|9|1|0\nf|c:\\y|icons|1|1|7\n").folders.is_empty());
    }

    #[test]
    fn columns_always_fill_the_row() {
        for total in [400.0f32, 700.0, 1100.0, 2400.0] {
            let w = fit_columns(COL_SHARES, total);
            assert!((w.iter().sum::<f32>() - total).abs() < 0.5, "{total}: {w:?}");
            assert!(w.iter().zip(COL_MIN).all(|(x, m)| *x >= m - 0.01), "{total}: {w:?}");
        }
        // Widening one column by taking from the next leaves the rest alone.
        let before = fit_columns(COL_SHARES, 1000.0);
        let mut moved = before;
        moved[1] += 40.0;
        moved[2] -= 40.0;
        let shares = normalised(moved).unwrap();
        let after = fit_columns(shares, 1000.0);
        assert!((after[1] - before[1] - 40.0).abs() < 0.5 && (after[0] - before[0]).abs() < 0.5);
        assert!(normalised([1.0, 0.0, 1.0, 1.0]).is_none());
    }

    #[test]
    fn groups() {
        let day = 864_000_000_000u64;
        let now = 1000 * day;
        let file = |modified: u64, size: u64| filesio::Item {
            modified,
            size: Some(size),
            kind: "Text Document".into(),
            ..Default::default()
        };
        assert_eq!(group_of(&file(now, 1), 2, now).1, "Today");
        assert_eq!(group_of(&file(now - day, 1), 2, now).1, "Yesterday");
        assert_eq!(group_of(&file(now - 400 * day, 1), 2, now).1, "A long time ago");
        assert_eq!(group_of(&file(now, 5_000_000), 3, now).1, "Medium (1 to 128 MB)");
        assert_eq!(group_of(&file(now, 0), 1, now).1, "Text Document");
    }
}
