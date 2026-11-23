//! Files (`cascade.exe --files [folder]`): Cascade's own file manager, so a
//! folder has somewhere to open with no Explorer at all. Tabs; back,
//! forward, up; an address box that takes paths, `shell:` names and "This
//! PC"; a search box that filters as you type and, on Enter, searches every
//! folder below (instantly with the file index on); a sidebar with your
//! folders and drives; details or icons (pictures and videos as thumbnails).
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
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::appwin::{self, size_text, LINE, PANEL};
use crate::filesio::{self, Item, Loc};
use crate::gfx::{Align, Color, Gfx, Painter};
use crate::{msg, theme};

const TABS_H: f32 = 38.0;
const BAR_H: f32 = 44.0;
const CMD_H: f32 = 40.0;
const SIDE_W: f32 = 214.0;
const HEAD_H: f32 = 30.0;
const ROW_H: f32 = 30.0;
const STATUS_H: f32 = 26.0;
const TAB_W: f32 = 200.0;

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
/// Lines: `hidden=1`, `cols=170,170,100`, `name=300` (0 = takes the rest) and
/// `f|<folder, lower case>|details|<sort column>|<ascending>|<group>`.
#[derive(Default)]
struct ViewStore {
    folders: HashMap<String, FolderView>,
    hidden: bool,
    cols: Option<[f32; 3]>,
    name: Option<f32>,
    /// How big icons are drawn in Icons view (1 = medium).
    scale: Option<f32>,
    /// The folders pinned in the sidebar (None: the usual six).
    pins: Option<Vec<String>>,
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
            } else if let Some(v) = line.strip_prefix("cols=") {
                let n: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if let [a, b, c] = n[..] {
                    s.cols = Some([a.clamp(60.0, 600.0), b.clamp(60.0, 600.0), c.clamp(60.0, 600.0)]);
                }
            } else if let Some(v) = line.strip_prefix("name=") {
                s.name = v.trim().parse::<f32>().ok().filter(|n| *n >= 120.0).map(|n| n.min(1400.0));
            } else if let Some(v) = line.strip_prefix("scale=") {
                s.scale =
                    v.trim().parse::<f32>().ok().map(|n| n.clamp(ICON_STEPS[0], ICON_STEPS[ICON_STEPS.len() - 1]));
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
            out.push_str(&format!("cols={},{},{}\n", c[0], c[1], c[2]));
        }
        out.push_str(&format!("name={}\n", self.name.unwrap_or(0.0)));
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
    Address,
    Item(usize),
    Empty,
}

/// How far a group heading takes (Group by).
const GROUP_H: f32 = 34.0;

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
    /// The Date modified, Type and Size columns' widths (Name takes the rest),
    /// and a column edge being dragged: (column, where the drag began, its width then).
    col_w: [f32; 3],
    col_drag: Option<(usize, f32, f32)>,
    /// The Name column's width once its edge has been dragged (else it takes what's left).
    name_fixed: Option<f32>,
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

/// The command bar: (glyph, label).
const CMDS: [(u32, &str); 10] = [
    (0xE710, "New"),
    (0xE8C6, "Cut"),
    (0xE8C8, "Copy"),
    (0xE77F, "Paste"),
    (0xE8AC, "Rename"),
    (0xE72D, "Share"),
    (0xE74D, "Delete"),
    (0xE8CB, "Sort"),
    (0xE8FD, "View"),
    (0xE712, "More"),
];

/// The command bar as Explorer's: New with its label, then Cut, Copy,
/// Paste, Rename, Share (with Quick Share) and Delete as icons, Sort and
/// View with labels, and More, in groups with a line between them. (x,
/// width) of command `i`.
fn cmd_rect(i: usize) -> (f32, f32) {
    const W: [f32; 10] = [84.0, 40.0, 40.0, 40.0, 40.0, 40.0, 40.0, 84.0, 84.0, 40.0];
    let mut x = 12.0;
    for (j, w) in W.iter().enumerate() {
        if starts_group(j) {
            x += 14.0;
        }
        if j == i {
            return (x, *w);
        }
        x += w + 2.0;
    }
    (x, 40.0)
}

/// Where a new group of commands starts (a line before it).
fn starts_group(i: usize) -> bool {
    matches!(i, 1 | 7 | 9)
}

/// Commands that show their label (and a chevron: they open a menu, or New).
fn labelled(i: usize) -> bool {
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
        (SIDE_W, y0, w - SIDE_W, h - y0 - STATUS_H)
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
            View::Details => {
                (x0 + 8.0, top + pos as f32 * ROW_H + self.heads_before(pos) as f32 * GROUP_H, cw - 24.0, ROW_H)
            }
            View::Icons => {
                let c = self.cols();
                let (tw, th) = (self.tile_w(), self.tile_h());
                (x0 + 8.0 + (pos % c) as f32 * tw, top + (pos / c) as f32 * th, tw - 6.0, th - 6.0)
            }
        }
    }

    fn content_height(&self) -> f32 {
        let n = self.t().order.len();
        match self.view {
            View::Details => n as f32 * ROW_H + self.t().heads.len() as f32 * GROUP_H + 8.0,
            View::Icons => (n.div_ceil(self.cols())) as f32 * self.tile_h() + 8.0,
        }
    }

    /// Group headings at or before item `pos` (Details only).
    fn heads_before(&self, pos: usize) -> usize {
        if self.view != View::Details {
            return 0;
        }
        self.t().heads.partition_point(|h| h.0 <= pos)
    }

    /// Column `i`'s width (Name, 0, takes what the others leave).
    fn col_width(&self, i: usize, cw: f32) -> f32 {
        if i == 0 {
            self.name_w(cw)
        } else {
            self.col_w[i - 1]
        }
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
        if let Some(key) = folder_key(&self.t().loc) {
            if here == DEFAULT_VIEW {
                self.store.folders.remove(&key);
            } else {
                self.store.folders.insert(key, here);
            }
        }
        self.store.hidden = self.show_hidden;
        self.store.cols = Some(self.col_w);
        self.store.scale = Some(self.icon_scale);
        self.store.name = self.name_fixed;
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
        let target = match &self.side[i].2 {
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
            open_file(&item.path);
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
                    open_file(&item.path);
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
        use crate::menu::{check, show, Entry, Place};
        let entries = vec![
            check(1, "Details", self.view == View::Details),
            check(2, "Icons", self.view == View::Icons),
            Entry::Separator,
            check(3, "Hidden items", self.show_hidden),
            Entry::Separator,
            check(10, "Group by: none", self.group == 0),
            check(11, "Group by type", self.group == 1),
            check(12, "Group by date modified", self.group == 2),
            check(13, "Group by size", self.group == 3),
        ];
        let chosen = show(self.hwnd, entries, Place::Cursor);
        match chosen {
            1 => self.view = View::Details,
            2 => self.view = View::Icons,
            g @ 10..=13 => {
                self.group = (g - 10) as u8;
                self.resort();
            }
            3 => {
                self.show_hidden = !self.show_hidden;
                self.remember();
                self.load();
            }
            _ => {}
        }
        if matches!(chosen, 1 | 2 | 10..=13) {
            self.remember();
        }
        let m = self.max_scroll();
        let t = self.tab();
        t.scroll = t.scroll.min(m);
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
        let (w, _) = appwin::client(self.hwnd);
        if y < TABS_H {
            if let Some(b) = appwin::caption_at(x, y, w, TABS_H) {
                return Some(Hit::Caption(b));
            }
            let tw = self.tab_w(w);
            for i in 0..self.tabs.len() {
                let tx = 8.0 + i as f32 * (tw + 4.0);
                if x >= tx && x < tx + tw {
                    return Some(if x > tx + tw - 28.0 { Hit::TabClose(i) } else { Hit::Tab(i) });
                }
            }
            let px = 8.0 + self.tabs.len() as f32 * (tw + 4.0);
            return (x >= px && x < px + 32.0).then_some(Hit::NewTab);
        }
        if y < TABS_H + BAR_H {
            let (ax, aw, _) = self.box_layout(w);
            if x >= ax && x < ax + aw {
                if !self.editing_addr {
                    for (cx, cw, i) in self.crumb_layout(w) {
                        if x >= cx && x < cx + cw {
                            return Some(Hit::Crumb(i));
                        }
                    }
                }
                return Some(Hit::Address);
            }
            return match ((x - 8.0) / 36.0).floor() as i32 {
                0 => Some(Hit::Back),
                1 => Some(Hit::Forward),
                2 => Some(Hit::Up),
                3 => Some(Hit::Refresh),
                _ => None,
            };
        }
        if y < TABS_H + BAR_H + CMD_H {
            return (0..CMDS.len())
                .find(|&i| {
                    let (cx, cw) = cmd_rect(i);
                    x >= cx && x < cx + cw
                })
                .map(Hit::Cmd);
        }
        if x < SIDE_W {
            let i = ((y - TABS_H - BAR_H - CMD_H - 8.0) / 32.0).floor();
            return (i >= 0.0 && (i as usize) < self.side.len()).then_some(Hit::Side(i as usize));
        }
        let (x0, y0, cw, _) = self.content();
        if self.view == View::Details && y < y0 + HEAD_H {
            let mut cx = x0 + 8.0;
            for i in 0..COLS.len() {
                let cwid = self.col_width(i, cw);
                // The edge after a column: drag it to make the column wider or narrower.
                if (x - (cx + cwid)).abs() < 5.0 {
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
            let iw = if self.view == View::Details { self.name_w(cw) + 440.0 } else { iw };
            if x >= ix && x < ix + iw && y >= iy && y < iy + ih {
                return Some(Hit::Item(pos));
            }
        }
        Some(Hit::Empty)
    }

    /// The Name column's width: as dragged, or what the other columns leave.
    fn name_w(&self, cw: f32) -> f32 {
        self.name_fixed.unwrap_or_else(|| (cw - 24.0 - self.col_w.iter().sum::<f32>()).max(160.0))
    }

    fn tab_w(&self, w: f32) -> f32 {
        // Room kept for the new-tab button, somewhere to drag the window by, and its buttons.
        ((w - 60.0 - 48.0 - 3.0 * appwin::CAPTION_W) / self.tabs.len() as f32 - 4.0).clamp(80.0, TAB_W)
    }

    /// The display positions on screen now.
    fn visible(&self) -> std::ops::Range<usize> {
        let (_, y0, _, ch) = self.content();
        let top = self.rows_top();
        let scroll = self.t().scroll;
        let n = self.t().order.len();
        let (row, per) = match self.view {
            View::Details => (ROW_H, 1),
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
        let icons: Vec<(usize, Option<ID2D1Bitmap>)> = vis
            .clone()
            .map(|pos| {
                let i = self.tabs[self.cur].order[pos];
                (pos, self.icon_for(i, big))
            })
            .collect();
        let side: Vec<Option<ID2D1Bitmap>> = (0..self.side.len()).map(|i| self.side_icon(i)).collect();
        let hwnd = self.hwnd;
        let Some(mut gfx) = self.gfx.take() else { return };
        appwin::paint(hwnd, &mut gfx, |p, f, w, h| self.draw(p, f, w, h, &icons, &side));
        self.gfx = Some(gfx);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        p: &Painter,
        f: &crate::gfx::Fonts,
        w: f32,
        h: f32,
        icons: &[(usize, Option<ID2D1Bitmap>)],
        side: &[Option<ID2D1Bitmap>],
    ) {
        let accent = theme::accent();
        let hov = self.hovered;
        let ic = f.icons.as_ref();
        let glyph_at = |code: u32, x: f32, y: f32, size: f32, c: Color| {
            if let Some(ic) = ic {
                p.text(&glyph(code), ic, x, y, size, size, Align::Center, c);
            }
        };
        // Tabs.
        let tw = self.tab_w(w);
        for (i, t) in self.tabs.iter().enumerate() {
            let tx = 8.0 + i as f32 * (tw + 4.0);
            let on = i == self.cur;
            let lit = hov == Some(Hit::Tab(i)) || hov == Some(Hit::TabClose(i));
            p.rrect(
                tx,
                6.0,
                tw,
                TABS_H - 6.0,
                6.0,
                if on { PANEL } else { Color::white(if lit { 0.06 } else { 0.0 }) },
            );
            p.text(
                &t.loc.title(),
                &f.body,
                tx + 12.0,
                6.0,
                tw - 44.0,
                TABS_H - 6.0,
                Align::Left,
                Color::white(if on { 0.95 } else { 0.7 }),
            );
            if on || lit {
                if hov == Some(Hit::TabClose(i)) {
                    p.rrect(tx + tw - 26.0, 13.0, 20.0, 20.0, 4.0, Color::white(0.1));
                }
                glyph_at(0xE711, tx + tw - 26.0, 13.0, 20.0, Color::white(0.7));
            }
        }
        let px = 8.0 + self.tabs.len() as f32 * (tw + 4.0);
        if hov == Some(Hit::NewTab) {
            p.rrect(px, 9.0, 30.0, 26.0, 5.0, Color::white(0.08));
        }
        glyph_at(0xE710, px + 5.0, 12.0, 20.0, Color::white(0.8));
        let lit = match hov {
            Some(Hit::Caption(b)) => Some(b),
            _ => None,
        };
        appwin::caption_buttons(p, f, self.hwnd, w, TABS_H, lit);
        // The bar: back, forward, up, refresh (the address and search boxes are text boxes over it).
        p.rrect(0.0, TABS_H, w, BAR_H + CMD_H, 0.0, PANEL);
        let t = self.t();
        let busy = self.busy();
        let can = [!t.back.is_empty(), !t.forward.is_empty(), !matches!(t.loc, Loc::Home), true];
        for (k, (code, hit)) in
            [(0xE72B, Hit::Back), (0xE72A, Hit::Forward), (0xE74A, Hit::Up), (0xE72C, Hit::Refresh)].iter().enumerate()
        {
            let x = 8.0 + k as f32 * 36.0;
            if hov == Some(*hit) && can[k] {
                p.rrect(x, TABS_H + 7.0, 32.0, 30.0, 5.0, Color::white(0.08));
            }
            let ink = Color::white(if can[k] { 0.85 } else { 0.3 });
            if *hit == Hit::Refresh && busy {
                // Turning while it reads the folder again.
                let turn = self.refresh_at.map_or(0.0, |t| t.elapsed().as_secs_f32()) * 540.0;
                p.rotated(turn % 360.0, x + 16.0, TABS_H + 22.0, || {
                    glyph_at(*code, x + 7.0, TABS_H + 12.0, 18.0, accent)
                });
            } else {
                glyph_at(*code, x + 7.0, TABS_H + 12.0, 18.0, ink);
            }
        }
        // The address and search boxes, in exactly the text boxes' colour so
        // each reads as one box (not a box in a box).
        let (ax, aw, sw) = self.box_layout(w);
        let field = Color(48.0 / 255.0, 48.0 / 255.0, 48.0 / 255.0, 1.0);
        let addr_lit = matches!(hov, Some(Hit::Address) | Some(Hit::Crumb(_)));
        p.rrect(ax, TABS_H + 7.0, aw, 30.0, 5.0, field);
        p.stroke_rrect(
            ax + 0.5,
            TABS_H + 7.5,
            aw - 1.0,
            29.0,
            5.0,
            Color::white(if addr_lit { 0.12 } else { 0.06 }),
            1.0,
        );
        p.rrect(w - sw - 10.0, TABS_H + 7.0, sw, 30.0, 5.0, field);
        p.stroke_rrect(w - sw - 9.5, TABS_H + 7.5, sw - 1.0, 29.0, 5.0, Color::white(0.06), 1.0);
        if self.editing_addr {
            p.rrect(ax + 1.0, TABS_H + 35.0, aw - 2.0, 2.0, 1.0, accent);
        } else {
            // The path as parts to click, as Explorer shows it.
            let parts = crumbs(&t.loc);
            let layout = self.crumb_layout(w);
            if layout.first().is_some_and(|l| l.2 > 0) {
                glyph_at(0xE712, ax + 6.0, TABS_H + 14.0, 16.0, Color::white(0.55));
            }
            for (n, (cx, cwid, i)) in layout.iter().enumerate() {
                if hov == Some(Hit::Crumb(*i)) {
                    p.rrect(*cx, TABS_H + 10.0, *cwid, 24.0, 4.0, Color::white(0.08));
                }
                let last = n + 1 == layout.len();
                p.text(
                    &parts[*i].0,
                    &f.body,
                    cx + 6.0,
                    TABS_H + 7.0,
                    cwid - 6.0,
                    30.0,
                    Align::Left,
                    Color::white(if last { 0.95 } else { 0.78 }),
                );
                if !last {
                    glyph_at(0xE76C, cx + cwid + 1.0, TABS_H + 17.0, 10.0, Color::white(0.5));
                }
            }
        }
        // Commands.
        p.rrect(0.0, TABS_H + BAR_H - 0.5, w, 1.0, 0.0, LINE);
        let cy = TABS_H + BAR_H;
        for (i, (code, label)) in CMDS.iter().enumerate() {
            let (x, bw) = cmd_rect(i);
            if starts_group(i) {
                p.line(x - 8.0, cy + 10.0, x - 8.0, cy + CMD_H - 10.0, Color::white(0.12), 1.0);
            }
            if hov == Some(Hit::Cmd(i)) {
                p.rrect(x, cy + 5.0, bw, 30.0, 5.0, Color::white(0.08));
            }
            // Explorer's colours: New in the accent, Delete in red, the rest plain.
            let ink = match i {
                0 => accent,
                5 => Color(0.36, 0.62, 1.0, 1.0),
                6 => Color(0.95, 0.45, 0.42, 1.0),
                _ => Color::white(0.85),
            };
            if labelled(i) {
                glyph_at(*code, x + 8.0, cy + 12.0, 16.0, ink);
                p.text(label, &f.body, x + 30.0, cy + 5.0, bw - 46.0, 30.0, Align::Left, Color::white(0.88));
                glyph_at(0xE70D, x + bw - 18.0, cy + 15.0, 10.0, Color::white(0.55));
            } else {
                glyph_at(*code, x + (bw - 16.0) / 2.0, cy + 12.0, 16.0, ink);
            }
        }
        // Sidebar.
        let y0 = TABS_H + BAR_H + CMD_H;
        p.rrect(0.0, y0, SIDE_W, h - y0, 0.0, Color(0.11, 0.11, 0.115, 1.0));
        for (i, (g, name, loc)) in self.side.iter().enumerate() {
            let y = y0 + 8.0 + i as f32 * 32.0;
            let on = *loc == t.loc;
            if on || hov == Some(Hit::Side(i)) {
                p.rrect(6.0, y, SIDE_W - 12.0, 30.0, 5.0, Color::white(if on { 0.09 } else { 0.05 }));
            }
            if on {
                p.rrect(6.0, y + 8.0, 3.0, 14.0, 1.5, accent);
            }
            match side.get(i).cloned().flatten() {
                Some(b) => p.image(&b, 15.0, y + 6.0, 18.0, 18.0, 1.0),
                None => glyph_at(*g as u32, 16.0, y + 7.0, 16.0, Color::white(0.8)),
            }
            p.text(name, &f.body, 42.0, y, SIDE_W - 52.0, 30.0, Align::Left, Color::white(0.9));
            // A line between your folders, the drives and the rest, as Explorer has.
            let next = self.side.get(i + 1).map(|s| &s.2);
            let group_ends = matches!((loc, next), (Loc::Path(_), Some(Loc::Shell(..))))
                || (i == self.pin_n && matches!(next, Some(Loc::Shell(..)) | Some(Loc::Path(_))));
            if group_ends {
                p.line(16.0, y + 31.0, SIDE_W - 16.0, y + 31.0, Color::white(0.07), 1.0);
            }
        }
        // Content.
        let (x0, cy0, cw, chh) = self.content();
        p.clip(x0, cy0, cw, chh);
        let scroll = t.scroll;
        if self.view == View::Details {
            let mut cx = x0 + 8.0;
            for (i, (name, _)) in COLS.iter().enumerate() {
                let width = self.col_width(i, cw);
                // The line after each column, brighter where it can be dragged.
                {
                    let lit =
                        matches!(hov, Some(Hit::HeaderEdge(e)) if e == i) || self.col_drag.is_some_and(|d| d.0 == i);
                    p.line(
                        cx + width - 0.5,
                        cy0 + 7.0,
                        cx + width - 0.5,
                        cy0 + HEAD_H - 7.0,
                        Color::white(if lit { 0.45 } else { 0.10 }),
                        1.0,
                    );
                }
                if hov == Some(Hit::Header(i)) {
                    p.rrect(cx, cy0 + 2.0, width, HEAD_H - 4.0, 4.0, Color::white(0.05));
                }
                let arrow = if self.sort.0 == i {
                    if self.sort.1 {
                        " \u{2191}"
                    } else {
                        " \u{2193}"
                    }
                } else {
                    ""
                };
                p.text(
                    &format!("{name}{arrow}"),
                    &f.label,
                    cx + 10.0,
                    cy0,
                    width - 14.0,
                    HEAD_H,
                    Align::Left,
                    Color::white(0.6),
                );
                cx += width;
            }
            p.rrect(x0 + 8.0, cy0 + HEAD_H - 1.0, cw - 24.0, 1.0, 0.0, LINE);
        }
        p.unclip();
        let top = self.rows_top();
        p.clip(x0, top, cw, chh - (top - cy0));
        let biggest = t.items.iter().filter(|i| i.folder).filter_map(|i| i.size).max().unwrap_or(0);
        // Group headings (Group by), above each group's first row.
        if self.view == View::Details {
            for (k, (pos, name, count)) in t.heads.iter().enumerate() {
                let hy = top + *pos as f32 * ROW_H + k as f32 * GROUP_H - scroll;
                if hy + GROUP_H < top || hy > cy0 + chh {
                    continue;
                }
                let label = format!("{name} ({count})");
                p.text(&label, &f.label, x0 + 16.0, hy + 4.0, cw - 40.0, GROUP_H - 4.0, Align::Left, accent);
                let lw = p.measure(&label, &f.label);
                p.line(
                    x0 + 24.0 + lw,
                    hy + GROUP_H / 2.0 + 2.0,
                    x0 + cw - 24.0,
                    hy + GROUP_H / 2.0 + 2.0,
                    Color::white(0.08),
                    1.0,
                );
            }
        }
        for (pos, bmp) in icons {
            let i = t.order[*pos];
            let item = &t.items[i];
            let (ix, iy, iw, ih) = self.item_rect(*pos);
            let iy = iy - scroll;
            let selected = t.sel.get(i) == Some(&true);
            let lit = hov == Some(Hit::Item(*pos));
            if selected || lit {
                let bg = if selected { accent.alpha(if lit { 0.38 } else { 0.28 }) } else { Color::white(0.06) };
                p.rrect(ix, iy + 1.0, iw, ih - 2.0, 5.0, bg);
            }
            let alpha = if item.hidden { 0.55 } else { 1.0 };
            match self.view {
                View::Details => {
                    if let Some(b) = bmp {
                        p.image(b, ix + 8.0, iy + 6.0, 18.0, 18.0, alpha);
                    }
                    let nw = self.name_w(cw);
                    p.text(&item.name, &f.body, ix + 34.0, iy, nw - 40.0, ih, Align::Left, Color::white(0.95 * alpha));
                    let mut cx = ix + nw;
                    let size = match (item.size, item.space) {
                        (_, Some((free, total))) => format!("{} free of {}", size_text(free), size_text(total)),
                        (Some(s), _) => size_text(s),
                        _ => String::new(),
                    };
                    for (k, text) in [&item.date, &item.kind, &size].iter().enumerate() {
                        let cwid = self.col_w[k];
                        if k == 2 {
                            // TreeSize's bars: a folder's share of the biggest here; a drive's fill.
                            let (bx, bw) = (cx + 8.0, cwid - 14.0);
                            if let Some((free, total)) = item.space {
                                let used = if total > 0 { 1.0 - free as f32 / total as f32 } else { 0.0 };
                                let c = if used > 0.9 { Color(0.9, 0.3, 0.28, 1.0) } else { accent };
                                p.rrect(bx, iy + ih - 7.0, bw, 3.0, 1.5, Color::white(0.1));
                                p.rrect(bx, iy + ih - 7.0, bw * used, 3.0, 1.5, c);
                            } else if let (true, Some(sz)) = (item.folder, item.size) {
                                let share = sz as f32 / biggest.max(1) as f32;
                                p.rrect(bx, iy + 5.0, (bw * share).max(2.0), ih - 10.0, 3.0, accent.alpha(0.22));
                            }
                        }
                        let align = if k == 2 && item.space.is_none() { Align::Right } else { Align::Left };
                        let ink = if k == 2 && item.folder && item.size.is_some() { 0.8 } else { 0.6 };
                        p.text(text, &f.small, cx + 10.0, iy, cwid - 18.0, ih, align, Color::white(ink));
                        cx += cwid;
                    }
                }
                View::Icons => {
                    let s = 72.0 * self.icon_scale;
                    if let Some(b) = bmp {
                        p.image(b, ix + (iw - s) / 2.0, iy + 6.0, s, s, alpha);
                    }
                    if let Some((free, total)) = item.space {
                        let frac = if total > 0 { 1.0 - free as f32 / total as f32 } else { 0.0 };
                        p.rrect(ix + 12.0, iy + 8.0 + s, iw - 24.0, 4.0, 2.0, Color::white(0.12));
                        let c = if frac > 0.9 { Color(0.9, 0.3, 0.28, 1.0) } else { accent };
                        p.rrect(ix + 12.0, iy + 8.0 + s, (iw - 24.0) * frac, 4.0, 2.0, c);
                    }
                    p.text_wrap(
                        &item.name,
                        &f.small,
                        ix + 4.0,
                        iy + 14.0 + s,
                        iw - 8.0,
                        30.0,
                        Color::white(0.95 * alpha),
                    );
                }
            }
        }
        if t.order.is_empty() {
            let msg = if t.loading {
                "Loading\u{2026}"
            } else if !t.filter.is_empty() {
                "Nothing here matches"
            } else {
                "This folder is empty"
            };
            p.text(msg, &f.body, x0, top + 30.0, cw, 24.0, Align::Center, Color::white(0.45));
        }
        // The box being dragged.
        if let Some(((ax_, ay_), (bx, by))) = self.band {
            // Kept in the content's coordinates (it scrolls with it).
            let (ay_, by) = (ay_ - scroll, by - scroll);
            let (l, tp) = (ax_.min(bx), ay_.min(by));
            let (bw, bh) = ((ax_ - bx).abs(), (ay_ - by).abs());
            p.rrect(l, tp, bw, bh, 2.0, accent.alpha(0.2));
            p.stroke_rrect(l + 0.5, tp + 0.5, bw - 1.0, bh - 1.0, 2.0, accent.alpha(0.8), 1.0);
        }
        // Scrollbar.
        let maxs = self.max_scroll();
        if maxs > 0.0 {
            let track = chh - (top - cy0);
            let thumb = (track * track / (track + maxs)).max(30.0);
            let ty = top + (track - thumb) * (scroll / maxs);
            p.rrect(x0 + cw - 8.0, ty, 4.0, thumb, 2.0, Color::white(0.25));
        }
        p.unclip();
        // Status.
        let sel = t.selected();
        let mut status = format!("{} item{}", t.order.len(), if t.order.len() == 1 { "" } else { "s" });
        if busy {
            status = "Refreshing\u{2026}".to_string();
            // A bar runs along the bottom of the command bar.
            let secs = self.refresh_at.map_or(0.0, |t| t.elapsed().as_secs_f32());
            let (seg, by) = (w * 0.3, TABS_H + BAR_H + CMD_H - 3.0);
            let at = -seg + (secs * 0.9 % 1.0) * (w + seg);
            let (l, r) = (at.max(0.0), (at + seg).min(w));
            p.rrect(0.0, by, w, 3.0, 0.0, accent.alpha(0.18));
            if r > l {
                p.rrect(l, by, r - l, 3.0, 1.5, accent);
            }
        }
        if !sel.is_empty() {
            let bytes: u64 = sel.iter().filter_map(|&i| t.items[i].size).sum();
            status += &format!(
                "  \u{b7}  {} selected{}",
                sel.len(),
                if bytes > 0 { format!(", {}", size_text(bytes)) } else { String::new() }
            );
        }
        p.text(
            &status,
            &f.small,
            SIDE_W + 12.0,
            h - STATUS_H,
            w - SIDE_W - 24.0,
            STATUS_H,
            Align::Left,
            Color::white(0.55),
        );
    }

    /// The address box's x and width, and the search box's width.
    fn box_layout(&self, w: f32) -> (f32, f32, f32) {
        let ax = 8.0 + 4.0 * 36.0 + 6.0;
        let sw = 240.0f32.min(w * 0.25);
        (ax, w - ax - sw - 22.0, sw)
    }

    /// Where the path's parts go in the address bar: (x, width, part), the
    /// first ones left out when the path is too long for it.
    fn crumb_layout(&self, w: f32) -> Vec<(f32, f32, usize)> {
        let (ax, aw, _) = self.box_layout(w);
        let parts = crumbs(&self.t().loc);
        let width = |s: &str| self.measure.as_ref().map_or(s.len() as f32 * 7.5, |m| m.width(s, &m.body)) + 12.0;
        let widths: Vec<f32> = parts.iter().map(|(name, _)| width(name)).collect();
        const CHEVRON: f32 = 14.0;
        let room = aw - 32.0;
        let mut first = 0;
        while first + 1 < parts.len() && widths[first..].iter().map(|w| w + CHEVRON).sum::<f32>() > room {
            first += 1;
        }
        let mut x = ax + if first > 0 { 26.0 } else { 6.0 };
        let mut out = Vec::new();
        for (i, wd) in widths.iter().enumerate().skip(first) {
            out.push((x, *wd, i));
            x += wd + CHEVRON;
        }
        out
    }

    fn place_boxes(&self) {
        let (w, _) = appwin::client(self.hwnd);
        let k = appwin::scale(self.hwnd);
        let (ax, aw, sw) = self.box_layout(w);
        unsafe {
            // The address box only while it's typed in; the path's parts show otherwise.
            let shown = if self.editing_addr { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
            let flags = SWP_NOZORDER | SWP_NOACTIVATE;
            let _ = SetWindowPos(self.addr, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | flags | shown);
            let _ = SetWindowPos(
                self.addr,
                None,
                ((ax + 8.0) * k) as i32,
                ((TABS_H + 13.0) * k) as i32,
                ((aw - 16.0) * k) as i32,
                (20.0 * k) as i32,
                flags,
            );
            let _ = SetWindowPos(
                self.search,
                None,
                ((w - sw - 2.0) * k) as i32,
                ((TABS_H + 13.0) * k) as i32,
                ((sw - 16.0) * k) as i32,
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
            Some(Hit::Cmd(i)) => self.command(i),
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
                self.col_drag = Some((i, x, self.col_width(i, cw)));
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
            None => {}
        }
        appwin::redraw(self.hwnd);
    }

    fn on_move(&mut self, x: f32, y: f32) {
        if let Some((i, from, width)) = self.col_drag {
            if i == 0 {
                self.name_fixed = Some((width + x - from).clamp(120.0, 1400.0));
            } else {
                self.col_w[i - 1] = (width + x - from).clamp(60.0, 600.0);
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
        let t = self.tab();
        t.scroll = (t.scroll - delta as f32 / 120.0 * ROW_H * 3.0).clamp(0.0, m);
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
                let page = ((self.content().3 / ROW_H) as isize).max(1);
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
                let next = if at < 0 { 0 } else { (at + step).clamp(0, n as isize - 1) } as usize;
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
                            open_file(text.trim());
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
fn open_file(path: &str) {
    if crate::viewer::should_handle(path) {
        appwin::launch(&["--view", path]);
        return;
    }
    crate::shell::open(path);
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
            SetTextColor(dc, COLORREF(0x00F0F0F0));
            SetBkColor(dc, COLORREF(0x00303030));
            if let Some(b) = with(|s| s.brush) {
                return LRESULT(b.0 as isize);
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
                brush: CreateSolidBrush(COLORREF(0x00303030)),
                hovered: None,
                band: None,
                press: None,
                rename: None,
                last_click: None,
                side: sidebar(&pins),
                typed: (String::new(), Instant::now()),
                col_w: store.cols.unwrap_or([COLS[1].1, COLS[2].1, COLS[3].1]),
                col_drag: None,
                name_fixed: store.name,
                editing_addr: false,
                group: 0,
                measure: crate::gfx::Measure::new().ok(),
                icon_scale: store.scale.unwrap_or(1.0),
                refresh_at: None,
                pin_n: pins.len(),
                store,
            })
        });
    }
    with(|s| {
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
        store.cols = Some([150.0, 160.0, 90.0]);
        store.name = Some(320.0);
        let key = folder_key(&Loc::Path("C:\\Users\\You\\Downloads\\".into())).unwrap();
        assert_eq!(key, "c:\\users\\you\\downloads");
        store.folders.insert(key.clone(), FolderView { view: View::Icons, sort: (1, false), group: 2 });
        let back = ViewStore::parse(&store.text());
        assert!(back.hidden);
        assert_eq!(back.cols, Some([150.0, 160.0, 90.0]));
        assert_eq!(back.name, Some(320.0));
        assert!(back.folders.get(&key) == Some(&FolderView { view: View::Icons, sort: (1, false), group: 2 }));
        // Search results aren't remembered; nonsense lines are skipped.
        assert!(folder_key(&Loc::Search("C:\\".into(), "x".into())).is_none());
        assert!(ViewStore::parse("f|c:\\x|details|9|1|0\nf|c:\\y|icons|1|1|7\n").folders.is_empty());
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
