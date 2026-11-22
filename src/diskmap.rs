//! Disk usage (`cascade.exe --disk [folder]`): what's taking the space, as a
//! tree (TreeSize's view, see `disktree`: open folders to see what's in
//! them, filling in live as it's worked out) or as a map of rectangles sized
//! by how much each folder and file holds, with the biggest listed beside
//! it. Click a folder to go into it, Backspace or ‹ to go back up;
//! right-click for Open, Show in Files, Copy path, Delete.
//!
//! Sizes come from the instant-search index when it's on (every folder's
//! total at once, as WizTree does from the drive's file table); without it,
//! the folder is walked on a thread of its own, with a count as it goes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;

use crate::appwin::{self, size_text, LINE, PANEL};
use crate::gfx::{Align, Color, Gfx};
use crate::theme;

const TOP: f32 = 52.0;
const LIST_W: f32 = 340.0;
const STATUS: f32 = 30.0;
const GAP: f32 = 2.0;
const DONE: u32 = WM_APP + 1;
/// The tree's walk found more (redraw).
const TREE_MORE: u32 = WM_APP + 2;
/// The tree's rows and header.
const TREE_ROW: f32 = 26.0;
const TREE_HEAD: f32 = 30.0;
/// The tree's toolbar (Rescan, Stop, Expand, Show in Files), above it.
const TOOL_H: f32 = 44.0;
/// The tree's toolbar buttons: (glyph, label, width).
const TOOLS: [(u32, &str, f32); 4] =
    [(0xE72C, "Rescan", 96.0), (0xE71A, "Stop", 80.0), (0xE70D, "Expand", 100.0), (0xE8B7, "Show in Files", 132.0)];
/// The tree's bars: a clear blue, whatever the accent (a tool's colour, as TreeSize's).
const BAR: Color = Color(0.29, 0.47, 0.98, 1.0);
/// Where toolbar button `i` starts.
fn tool_x(i: usize) -> f32 {
    12.0 + TOOLS.iter().take(i).map(|t| t.2 + 6.0).sum::<f32>()
}
/// The tree's columns after Name: (title, width).
const TREE_COLS: [(&str, f32); 5] =
    [("Size", 96.0), ("Files", 86.0), ("Folders", 80.0), ("% of parent", 130.0), ("Modified", 100.0)];
/// The Tree / Map switch at the top.
const SWITCH: (f32, f32) = (52.0, 132.0);

/// A folder's contents: its total, and each item (name, folder, size), biggest first.
#[derive(Clone, Default)]
struct Listing {
    total: u64,
    items: Vec<(String, bool, u64)>,
}

/// A walk in progress: files counted so far, and (once done) the result.
type Progress = Mutex<(u64, Option<(PathBuf, Listing)>)>;

struct State {
    hwnd: HWND,
    gfx: Gfx,
    path: PathBuf,
    listing: Option<Listing>,
    /// Folders already worked out (the walk is slow without the index).
    cache: HashMap<PathBuf, Listing>,
    /// Files counted so far by a walk in progress.
    progress: Arc<Progress>,
    /// The map's rectangles: (x, y, w, h, item index; None for "smaller items").
    rects: Vec<(f32, f32, f32, f32, Option<usize>)>,
    hovered: Option<usize>,
    drives: Vec<(char, u64, u64)>,
    /// Shown as the tree (else the map), the tree, its open folders, how far
    /// it's scrolled, the row picked and the row under the pointer, and
    /// the rows as last drawn (for clicks).
    tree_view: bool,
    tree: Option<crate::disktree::Shared>,
    open: std::collections::HashSet<usize>,
    tree_scroll: f32,
    tree_sel: Option<crate::disktree::Row>,
    tree_hot: Option<usize>,
    tree_rows: Vec<(crate::disktree::Row, usize)>,
    /// How the tree's folders are ordered (a click on a column's title).
    tree_sort: crate::disktree::Sort,
    /// The toolbar button under the pointer.
    tool_hot: Option<usize>,
    /// Windows' folder and document icons for the tree (BGRA, `TREE_ICON` square).
    stock: Option<(Vec<u8>, Vec<u8>)>,
}

/// The tree's icons' size in pixels (drawn at 18, so sharp to 200%).
const TREE_ICON: u32 = 36;

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Fixed drives: (letter, used, total).
fn drives() -> Vec<(char, u64, u64)> {
    use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| (b'A' + i) as char)
        .filter_map(|l| unsafe {
            let root = windows::core::HSTRING::from(format!("{l}:\\"));
            if GetDriveTypeW(&root) != 3 {
                return None;
            }
            let (mut free, mut total) = (0u64, 0u64);
            GetDiskFreeSpaceExW(&root, None, Some(&mut total), Some(&mut free)).ok()?;
            Some((l, total - free, total))
        })
        .collect()
}

/// Walks `dir`: each child's total (folders summed all the way down), not
/// following junctions and links. Counts files in `count` as it goes.
fn walk(dir: &Path, count: &Progress) -> Listing {
    fn total(p: &Path, count: &Progress) -> u64 {
        use std::os::windows::fs::MetadataExt;
        let Ok(entries) = std::fs::read_dir(p) else { return 0 };
        let mut sum = 0;
        for e in entries.flatten() {
            let Ok(m) = e.metadata() else { continue };
            // Reparse points (junctions, links, OneDrive placeholders) aren't followed.
            if m.file_attributes() & 0x400 != 0 {
                continue;
            }
            if m.is_dir() {
                sum += total(&e.path(), count);
            } else {
                sum += m.len();
                if let Ok(mut c) = count.lock() {
                    c.0 += 1;
                }
            }
        }
        sum
    }
    let mut items = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            use std::os::windows::fs::MetadataExt;
            let Ok(m) = e.metadata() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if m.file_attributes() & 0x400 != 0 {
                continue;
            }
            let size = if m.is_dir() { total(&e.path(), count) } else { m.len() };
            items.push((name, m.is_dir(), size));
        }
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.2));
    Listing { total: items.iter().map(|i| i.2).sum(), items }
}

impl State {
    fn go(&mut self, path: PathBuf) {
        self.path = path.clone();
        self.hovered = None;
        self.rects.clear();
        if self.tree_view {
            self.start_tree();
            appwin::redraw(self.hwnd);
            return;
        }
        if let Some(l) = self.cache.get(&path) {
            self.listing = Some(l.clone());
            appwin::redraw(self.hwnd);
            return;
        }
        // The index has every folder's total at once.
        if let Some((total, items)) = crate::fileindex::children(&path.to_string_lossy()) {
            let l = Listing { total, items };
            self.cache.insert(path, l.clone());
            self.listing = Some(l);
            appwin::redraw(self.hwnd);
            return;
        }
        self.listing = None;
        let progress = self.progress.clone();
        if let Ok(mut p) = progress.lock() {
            *p = (0, None);
        }
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let l = walk(&path, &progress);
            if let Ok(mut p) = progress.lock() {
                p.1 = Some((path, l));
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), DONE, WPARAM(0), LPARAM(0));
            }
        });
        unsafe {
            SetTimer(Some(self.hwnd), 1, 250, None);
        }
        appwin::redraw(self.hwnd);
    }

    /// A tree of the folder shown (unless there is one already).
    fn start_tree(&mut self) {
        let root_now = self.tree.as_ref().map(|t| t.lock().unwrap_or_else(|e| e.into_inner()).root.clone());
        if root_now.as_ref() == Some(&self.path) {
            return;
        }
        let hwnd = self.hwnd.0 as isize;
        self.tree = Some(crate::disktree::start(&self.path, move || unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), TREE_MORE, WPARAM(0), LPARAM(0));
        }));
        self.open.clear();
        self.open.insert(0);
        self.tree_scroll = 0.0;
        self.tree_sel = None;
        self.tree_hot = None;
    }

    /// Shows the tree or the map.
    fn show_tree(&mut self, on: bool) {
        if on == self.tree_view {
            return;
        }
        self.tree_view = on;
        let path = self.path.clone();
        self.go(path);
    }

    /// The tree's row at `y`, if there's one there.
    fn tree_row_at(&self, y: f32) -> Option<usize> {
        let top = TOP + TOOL_H + TREE_HEAD;
        if y < top {
            return None;
        }
        let i = ((y - top + self.tree_scroll) / TREE_ROW).floor() as usize;
        (i < self.tree_rows.len()).then_some(i)
    }

    /// The toolbar button at (x, y), if there is one.
    fn tool_at(&self, x: f32, y: f32) -> Option<usize> {
        if !(TOP + 4.0..TOP + TOOL_H - 6.0).contains(&y) {
            return None;
        }
        (0..TOOLS.len()).find(|&i| x >= tool_x(i) && x < tool_x(i) + TOOLS[i].2)
    }

    /// A toolbar button: rescan, stop, open levels of the tree, or show the folder in Files.
    fn tool(&mut self, i: usize) {
        match i {
            0 => {
                self.tree = None;
                self.start_tree();
            }
            1 => {
                if let Some(t) = &self.tree {
                    t.lock().unwrap_or_else(|e| e.into_inner()).stop();
                }
            }
            2 => {
                use crate::menu::{item, show, Entry, Place};
                let entries = vec![
                    item(1, "One level"),
                    item(2, "Two levels"),
                    item(3, "Three levels"),
                    Entry::Separator,
                    item(9, "Close all"),
                ];
                let levels = match show(self.hwnd, entries, Place::Cursor) {
                    9 => 0,
                    n @ 1..=3 => n,
                    _ => return,
                };
                self.expand(levels);
            }
            3 => {
                let p = self.selected_path().unwrap_or_else(|| self.path.clone());
                appwin::launch(&["--files", &p.to_string_lossy()]);
            }
            _ => {}
        }
        appwin::redraw(self.hwnd);
    }

    /// Opens every folder down to `levels` below the top (0: just the top).
    fn expand(&mut self, levels: usize) {
        let Some(tree) = self.tree.clone() else { return };
        let mut t = tree.lock().unwrap_or_else(|e| e.into_inner());
        self.open.clear();
        let mut level = vec![0usize];
        for _ in 0..=levels {
            let mut next = Vec::new();
            for id in level {
                t.read_from_index(id);
                self.open.insert(id);
                next.extend(t.nodes[id].children.iter().copied());
            }
            level = next;
            if self.open.len() > 4000 {
                break;
            }
        }
        if levels == 0 {
            self.open.clear();
            self.open.insert(0);
        }
    }

    /// The folder picked in the tree, if there is one.
    fn selected_path(&self) -> Option<PathBuf> {
        use crate::disktree::Row;
        let id = match self.tree_sel? {
            Row::Folder(id) | Row::Files(id) => id,
        };
        Some(self.tree.as_ref()?.lock().unwrap_or_else(|e| e.into_inner()).path_of(id))
    }

    /// A click on the tree's column titles: ordered by that column.
    fn tree_header(&mut self, x: f32, y: f32, w: f32) -> bool {
        use crate::disktree::Sort;
        let top = TOP + TOOL_H;
        if !(top..top + TREE_HEAD).contains(&y) {
            return false;
        }
        let fixed: f32 = TREE_COLS.iter().map(|c| c.1).sum();
        let name_w = (w - 24.0 - fixed - 16.0).max(200.0);
        let mut cx = 12.0 + 8.0 + name_w;
        let mut sort = Sort::Name;
        for (k, (_, cw)) in TREE_COLS.iter().enumerate() {
            if x >= cx {
                sort = [Sort::Size, Sort::Files, Sort::Folders, Sort::Size, Sort::Modified][k];
            }
            cx += cw;
        }
        self.tree_sort = sort;
        appwin::redraw(self.hwnd);
        true
    }

    /// A click in the tree: on a folder's arrow (or `toggle`) opens or closes it.
    fn tree_click(&mut self, x: f32, y: f32, toggle: bool) {
        use crate::disktree::Row;
        let Some(i) = self.tree_row_at(y) else { return };
        let (row, depth) = self.tree_rows[i];
        self.tree_sel = Some(row);
        if let Row::Folder(id) = row {
            let arrow = 12.0 + depth as f32 * 18.0;
            if (toggle || (x >= arrow && x < arrow + 20.0)) && !self.open.remove(&id) {
                if let Some(t) = &self.tree {
                    t.lock().unwrap_or_else(|e| e.into_inner()).read_from_index(id);
                }
                self.open.insert(id);
            }
        }
        appwin::redraw(self.hwnd);
    }

    /// Right-click on a row of the tree: what can be done with that folder.
    fn tree_menu(&mut self, y: f32) {
        use crate::disktree::Row;
        use crate::menu::{item, show, Entry, Place};
        let Some(i) = self.tree_row_at(y) else { return };
        let (row, _) = self.tree_rows[i];
        let Some(tree) = self.tree.clone() else { return };
        let id = match row {
            Row::Folder(id) | Row::Files(id) => id,
        };
        let p = tree.lock().unwrap_or_else(|e| e.into_inner()).path_of(id);
        let entries = vec![
            item(1, "Show in Files"),
            item(2, "Show as a map"),
            item(3, "Copy path"),
            Entry::Separator,
            item(4, "Delete"),
        ];
        match show(self.hwnd, entries, Place::Cursor) {
            1 => appwin::launch(&["--files", &p.to_string_lossy()]),
            2 => {
                self.tree_view = false;
                self.go(p);
            }
            3 => crate::shell::copy_text(&p.to_string_lossy()),
            4 if id != 0 && matches!(row, Row::Folder(_)) => {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if crate::winset::confirm(self.hwnd, &format!("Move \u{201c}{name}\u{201d} to the Recycle Bin?")) {
                    crate::deskicons::delete(self.hwnd, &[p.to_string_lossy().into_owned()], false);
                    self.cache.clear();
                    self.tree = None;
                    self.start_tree();
                }
            }
            _ => {}
        }
    }

    fn walked(&mut self) {
        let done = self.progress.lock().ok().and_then(|mut p| p.1.take());
        if let Some((path, l)) = done {
            self.cache.insert(path.clone(), l.clone());
            if path == self.path {
                self.listing = Some(l);
            }
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), 1);
        }
        appwin::redraw(self.hwnd);
    }

    fn up(&mut self) {
        if let Some(parent) = self.path.parent().map(Path::to_path_buf) {
            self.go(parent);
        }
    }

    fn item_path(&self, i: usize) -> Option<PathBuf> {
        Some(self.path.join(&self.listing.as_ref()?.items.get(i)?.0))
    }

    fn open(&mut self, i: usize) {
        let Some((_, dir, _)) = self.listing.as_ref().and_then(|l| l.items.get(i)).cloned() else { return };
        let Some(p) = self.item_path(i) else { return };
        if dir {
            self.go(p);
        } else {
            crate::shell::open(&p.to_string_lossy());
        }
    }

    fn menu(&mut self, i: usize) {
        use crate::menu::{item, show, Entry, Place};
        let Some(p) = self.item_path(i) else { return };
        let entries =
            vec![item(1, "Open"), item(2, "Show in Files"), item(3, "Copy path"), Entry::Separator, item(4, "Delete")];
        match show(self.hwnd, entries, Place::Cursor) {
            1 => self.open(i),
            2 => appwin::launch(&["--files", &p.to_string_lossy()]),
            3 => crate::shell::copy_text(&p.to_string_lossy()),
            4 => {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if crate::winset::confirm(self.hwnd, &format!("Move \u{201c}{name}\u{201d} to the Recycle Bin?")) {
                    crate::deskicons::delete(self.hwnd, &[p.to_string_lossy().into_owned()], false);
                    // Its size goes from this folder and every one above it.
                    self.cache.clear();
                    let path = self.path.clone();
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    self.go(path);
                }
            }
            _ => {}
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        let (w, _) = appwin::client(self.hwnd);
        // The list on the right.
        if x > w - LIST_W {
            let row = ((y - TOP - 40.0) / 34.0).floor();
            return (row >= 0.0)
                .then_some(row as usize)
                .filter(|&r| self.listing.as_ref().is_some_and(|l| r < l.items.len()));
        }
        self.rects.iter().find(|r| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3).and_then(|r| r.4)
    }
}

/// Lays `sizes` (biggest first) out as rectangles filling (x, y, w, h), as
/// near square as can be (the "squarified" treemap).
fn squarify(sizes: &[f64], x: f32, y: f32, w: f32, h: f32) -> Vec<(f32, f32, f32, f32)> {
    let total: f64 = sizes.iter().sum();
    let mut out = vec![(0.0, 0.0, 0.0, 0.0); sizes.len()];
    if total <= 0.0 || w <= 0.0 || h <= 0.0 {
        return out;
    }
    let scale = (w as f64 * h as f64) / total;
    let areas: Vec<f64> = sizes.iter().map(|s| s * scale).collect();
    let (mut rx, mut ry, mut rw, mut rh) = (x as f64, y as f64, w as f64, h as f64);
    let mut i = 0;
    while i < areas.len() {
        let side = rw.min(rh);
        let worst = |row: &[f64]| {
            let s: f64 = row.iter().sum();
            let (mx, mn) = row.iter().fold((0f64, f64::MAX), |(a, b), &v| (a.max(v), b.min(v)));
            ((side * side * mx) / (s * s)).max((s * s) / (side * side * mn))
        };
        let mut j = i + 1;
        while j < areas.len() && worst(&areas[i..=j]) <= worst(&areas[i..j]) {
            j += 1;
        }
        let row = &areas[i..j];
        let s: f64 = row.iter().sum();
        if rw >= rh {
            // A column along the left.
            let cw = s / rh;
            let mut cy = ry;
            for (k, a) in row.iter().enumerate() {
                let ch = a / cw;
                out[i + k] = (rx as f32, cy as f32, cw as f32, ch as f32);
                cy += ch;
            }
            rx += cw;
            rw -= cw;
        } else {
            let rh2 = s / rw;
            let mut cx = rx;
            for (k, a) in row.iter().enumerate() {
                let cw = a / rh2;
                out[i + k] = (cx as f32, ry as f32, cw as f32, rh2 as f32);
                cx += cw;
            }
            ry += rh2;
            rh -= rh2;
        }
        i = j;
    }
    out
}

/// A colour for an item: folders in the accent's family, files by kind.
fn colour(name: &str, dir: bool, i: usize) -> Color {
    if dir {
        let a = theme::accent();
        let k = 0.55 + 0.35 * ((i * 37 % 10) as f32 / 10.0);
        return Color(a.0 * k, a.1 * k, a.2 * k, 1.0);
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "mkv" | "mov" | "avi" | "webm" => Color(0.85, 0.35, 0.35, 1.0),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "bmp" | "raw" | "dng" => Color(0.95, 0.65, 0.25, 1.0),
        "mp3" | "flac" | "wav" | "m4a" | "ogg" => Color(0.75, 0.45, 0.85, 1.0),
        "zip" | "7z" | "rar" | "gz" | "tar" | "iso" => Color(0.45, 0.75, 0.45, 1.0),
        "exe" | "dll" | "msi" | "sys" => Color(0.40, 0.70, 0.85, 1.0),
        "vhdx" | "vmdk" | "pak" | "bin" | "dat" => Color(0.65, 0.65, 0.40, 1.0),
        _ => Color(0.55, 0.57, 0.62, 1.0),
    }
}

/// Draws the tree in (x, y, w, h): a header, then its rows from `scroll` down.
#[allow(clippy::too_many_arguments)]
fn paint_tree(
    p: &crate::gfx::Painter,
    f: &crate::gfx::Fonts,
    area: (f32, f32, f32, f32),
    t: &crate::disktree::Tree,
    rows: &[(crate::disktree::Row, usize)],
    open: &std::collections::HashSet<usize>,
    scroll: f32,
    sel: Option<crate::disktree::Row>,
    hot: Option<usize>,
    icons: Option<&(ID2D1Bitmap, ID2D1Bitmap)>,
    sort: crate::disktree::Sort,
) {
    use crate::disktree::{Row, Sort};
    let (x, y, w, h) = area;
    let spin =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f32()) * 6.0;
    let last_child = crate::disktree::last_children(rows);
    p.rrect(x, y, w, h, 8.0, PANEL);
    // The header.
    let fixed: f32 = TREE_COLS.iter().map(|c| c.1).sum();
    let name_w = (w - fixed - 16.0).max(200.0);
    // The column titles, the one it's ordered by marked (click one to order by it).
    p.rrect(x, y, w, TREE_HEAD, 8.0, Color::white(0.035));
    let marked = |s: Sort| if s == sort { " \u{25BE}" } else { "" };
    p.text(
        &format!("Name{}", marked(Sort::Name)),
        &f.label,
        x + 14.0,
        y,
        name_w,
        TREE_HEAD,
        Align::Left,
        Color::white(0.7),
    );
    let mut cx = x + 8.0 + name_w;
    for (k, (title, cw)) in TREE_COLS.iter().enumerate() {
        let s = [Sort::Size, Sort::Files, Sort::Folders, Sort::Size, Sort::Modified][k];
        let title = if k == 3 { title.to_string() } else { format!("{title}{}", marked(s)) };
        p.line(cx, y + 8.0, cx, y + TREE_HEAD - 8.0, Color::white(0.08), 1.0);
        p.text(&title, &f.label, cx, y, cw - 12.0, TREE_HEAD, Align::Right, Color::white(0.7));
        cx += cw;
    }
    p.rrect(x + 8.0, y + TREE_HEAD - 1.0, w - 16.0, 1.0, 0.0, LINE);
    // The rows.
    let top = y + TREE_HEAD;
    p.clip(x, top, w, h - TREE_HEAD);
    let first = (scroll / TREE_ROW).floor().max(0.0) as usize;
    let count = ((h - TREE_HEAD) / TREE_ROW).ceil() as usize + 1;
    // The tree's lines: from each open folder down to its last child, with a
    // short line across to each child, as TreeSize and Explorer draw them.
    let line = Color::white(0.16);
    let row_y = |i: usize| top + i as f32 * TREE_ROW - scroll;
    for (i, last) in last_child.iter().enumerate() {
        let Some(last) = *last else { continue };
        let (y0, y1) = (row_y(i) + TREE_ROW - 5.0, row_y(last) + TREE_ROW / 2.0);
        if y1 < top || y0 > y + h {
            continue;
        }
        let lx = x + 12.0 + rows[i].1 as f32 * 18.0 + 8.5;
        p.line(lx, y0, lx, y1, line, 1.0);
        for (j, &(_, depth)) in rows.iter().enumerate().take(last + 1).skip(i + 1) {
            if depth == rows[i].1 + 1 {
                let cy = row_y(j) + TREE_ROW / 2.0;
                if cy >= top - TREE_ROW && cy <= y + h + TREE_ROW {
                    p.line(lx, cy, lx + 9.0, cy, line, 1.0);
                }
            }
        }
    }
    for (i, &(row, depth)) in rows.iter().enumerate().skip(first).take(count) {
        let ry = top + i as f32 * TREE_ROW - scroll;
        let (node, parent_size) = match row {
            Row::Folder(id) => {
                let n = &t.nodes[id];
                (n, n.parent.map_or(n.size, |p| t.nodes[p].size))
            }
            Row::Files(id) => (&t.nodes[id], t.nodes[id].size),
        };
        let (name, size, files, folders, modified) = match row {
            Row::Folder(_) => (node.name.clone(), node.size, node.files, node.folders, node.modified),
            Row::Files(_) => {
                let n = node.own_files;
                (format!("[{n} File{}]", if n == 1 { "" } else { "s" }), node.own_size, n, 0, 0)
            }
        };
        if sel == Some(row) {
            p.rrect(x + 6.0, ry + 1.0, w - 12.0, TREE_ROW - 2.0, 4.0, BAR.alpha(0.30));
        } else if hot == Some(i) {
            p.rrect(x + 6.0, ry + 1.0, w - 12.0, TREE_ROW - 2.0, 4.0, Color::white(0.06));
        } else if i % 2 == 1 {
            // Every other row a shade lighter, to follow a row across.
            p.rrect(x + 6.0, ry, w - 12.0, TREE_ROW, 0.0, Color::white(0.018));
        }
        let ix = x + 12.0 + depth as f32 * 18.0;
        // The arrow: open or closed, for a folder with anything in it.
        if let Row::Folder(id) = row {
            let n = &t.nodes[id];
            let has = !n.children.is_empty() || n.own_files > 0 || (t.indexed && !n.read);
            if has {
                let (ax, ay) = (ix + 8.0, ry + TREE_ROW / 2.0);
                let c = Color::white(0.65);
                if open.contains(&id) {
                    p.line(ax - 4.0, ay - 2.0, ax, ay + 2.0, c, 1.4);
                    p.line(ax, ay + 2.0, ax + 4.0, ay - 2.0, c, 1.4);
                } else {
                    p.line(ax - 2.0, ay - 4.0, ax + 2.0, ay, c, 1.4);
                    p.line(ax + 2.0, ay, ax - 2.0, ay + 4.0, c, 1.4);
                }
            }
        }
        // Windows' own folder icon, or its document icon for the loose files.
        let (gx, gy) = (ix + 22.0, ry + 7.0);
        match (row, icons) {
            (Row::Folder(_), Some((folder, _))) => p.image(folder, gx - 1.0, ry + 4.0, 18.0, 18.0, 1.0),
            (Row::Files(_), Some((_, file))) => p.image(file, gx - 1.0, ry + 4.0, 18.0, 18.0, 1.0),
            (Row::Folder(_), None) => {
                p.rrect(gx, gy, 7.0, 3.0, 1.0, Color(0.96, 0.75, 0.25, 1.0));
                p.rrect(gx, gy + 2.0, 16.0, 11.0, 2.0, Color(0.98, 0.80, 0.32, 1.0));
            }
            (Row::Files(_), None) => {
                p.rrect(gx + 2.0, gy - 1.0, 11.0, 14.0, 2.0, Color::white(0.75));
            }
        }
        // Still being worked out (something in it not read yet): a spinning arc on its icon.
        if let Row::Folder(id) = row {
            if t.nodes[id].pending > 0 && !t.done {
                let (sx, sy) = (gx + 14.0, ry + 18.0);
                p.circle(sx, sy, 5.0, Color(0.12, 0.12, 0.13, 1.0));
                p.arc(sx, sy, 3.5, spin + depth as f32, spin + depth as f32 + 4.2, BAR, 1.6);
            }
        }
        // Its size on a bar for its share of the folder above, as TreeSize
        // shows it beside the name, then the name.
        let share = if matches!(row, Row::Folder(0)) { 1.0 } else { size as f32 / parent_size.max(1) as f32 };
        let (sbx, sbw) = (gx + 24.0, 70.0);
        p.rrect(sbx, ry + 4.0, sbw, TREE_ROW - 8.0, 3.0, Color::white(0.05));
        p.rrect(sbx, ry + 4.0, (sbw * share.clamp(0.0, 1.0)).max(2.0), TREE_ROW - 8.0, 3.0, Color::white(0.16));
        p.text(&size_text(size), &f.small, sbx, ry, sbw - 6.0, TREE_ROW, Align::Right, Color::white(0.92));
        let tx = sbx + sbw + 8.0;
        let name_room = (x + 8.0 + name_w - tx - 8.0).max(0.0);
        let bold = depth <= 1 && matches!(row, Row::Folder(_));
        let font = if bold { &f.title } else { &f.body };
        p.text(&name, font, tx, ry, name_room, TREE_ROW, Align::Left, Color::white(if bold { 0.97 } else { 0.86 }));
        // The numbers.
        let mut cx = x + 8.0 + name_w;
        let cell = |cx: f32, cw: f32, text: &str| {
            p.text(text, &f.small, cx, ry, cw - 12.0, TREE_ROW, Align::Right, Color::white(0.75));
        };
        cell(cx, TREE_COLS[0].1, &size_text(size));
        cx += TREE_COLS[0].1;
        let counts = !t.indexed || matches!(row, Row::Files(_));
        cell(cx, TREE_COLS[1].1, &if counts { files.to_string() } else { String::new() });
        cx += TREE_COLS[1].1;
        cell(
            cx,
            TREE_COLS[2].1,
            &if counts && matches!(row, Row::Folder(_)) { folders.to_string() } else { String::new() },
        );
        cx += TREE_COLS[2].1;
        // % of the folder above, as a blue bar with the number on it.
        let (bx, bw) = (cx + 8.0, TREE_COLS[3].1 - 16.0);
        p.rrect(bx, ry + 5.0, bw, TREE_ROW - 10.0, 3.0, Color::white(0.06));
        p.rrect(bx, ry + 5.0, bw * share.clamp(0.0, 1.0), TREE_ROW - 10.0, 3.0, BAR.alpha(0.85));
        p.stroke_rrect(bx + 0.5, ry + 5.5, bw - 1.0, TREE_ROW - 11.0, 3.0, Color::white(0.10), 1.0);
        p.text(&format!("{:.1} %", share * 100.0), &f.small, bx, ry, bw, TREE_ROW, Align::Center, Color::white(0.92));
        cx += TREE_COLS[3].1;
        cell(cx, TREE_COLS[4].1, &crate::disktree::date(modified));
    }
    p.unclip();
    // Scrollbar.
    let total = rows.len() as f32 * TREE_ROW;
    let view = h - TREE_HEAD;
    if total > view {
        let thumb = (view * view / total).max(30.0);
        let ty = top + (view - thumb) * (scroll / (total - view)).clamp(0.0, 1.0);
        p.rrect(x + w - 8.0, ty, 4.0, thumb, 2.0, Color::white(0.25));
    }
}

fn paint(s: &mut State) {
    let hwnd = s.hwnd;
    let tree_view = s.tree_view;
    let tree = s.tree.clone();
    let open = s.open.clone();
    let (tree_scroll, tree_sel, tree_hot) = (s.tree_scroll, s.tree_sel, s.tree_hot);
    let (tree_sort, tool_hot) = (s.tree_sort, s.tool_hot);
    let mut tree_rows = Vec::new();
    let stock = s.stock.clone();
    let drives = s.drives.clone();
    let path_text = s.path.to_string_lossy().into_owned();
    let listing = s.listing.clone();
    let hovered = s.hovered;
    let counted = s.progress.lock().map(|p| p.0).unwrap_or(0);
    let mut rects = Vec::new();
    appwin::paint(hwnd, &mut s.gfx, |p, f, w, h| {
        // Top: back, the path, the drives.
        p.rrect(12.0, 12.0, 30.0, 28.0, 6.0, Color::white(0.06));
        p.text("\u{2039}", &f.title, 12.0, 10.0, 30.0, 28.0, Align::Center, Color::white(0.85));
        // Tree or map.
        let (sx, sw) = SWITCH;
        p.rrect(sx, 12.0, sw, 28.0, 6.0, Color::white(0.06));
        for (k, label) in ["Tree", "Map"].iter().enumerate() {
            let on = (k == 0) == tree_view;
            let bx = sx + k as f32 * sw / 2.0;
            if on {
                p.rrect(bx + 2.0, 14.0, sw / 2.0 - 4.0, 24.0, 5.0, theme::accent());
            }
            p.text(label, &f.body, bx, 12.0, sw / 2.0, 28.0, Align::Center, Color::white(if on { 1.0 } else { 0.75 }));
        }
        let px = sx + sw + 12.0;
        p.text(
            &path_text,
            &f.title,
            px,
            12.0,
            w - px - 8.0 - drives.len() as f32 * 96.0,
            28.0,
            Align::Left,
            Color::white(0.95),
        );
        for (k, (l, used, total)) in drives.iter().enumerate() {
            let x = w - (drives.len() - k) as f32 * 96.0 - 4.0;
            let on = path_text.starts_with(*l);
            p.rrect(x, 10.0, 88.0, 32.0, 6.0, if on { theme::accent().alpha(0.25) } else { Color::white(0.05) });
            p.text(&format!("{l}:"), &f.label, x + 8.0, 12.0, 30.0, 16.0, Align::Left, Color::white(0.9));
            let frac = if *total > 0 { *used as f32 / *total as f32 } else { 0.0 };
            p.rrect(x + 8.0, 31.0, 72.0, 4.0, 2.0, Color::white(0.12));
            p.rrect(
                x + 8.0,
                31.0,
                72.0 * frac,
                4.0,
                2.0,
                if frac > 0.9 { Color(0.9, 0.35, 0.3, 1.0) } else { theme::accent() },
            );
            p.text(&size_text(total - used), &f.small, x + 32.0, 13.0, 52.0, 14.0, Align::Right, Color::white(0.55));
        }
        let (mx, my, mw, mh) = (12.0, TOP, w - LIST_W - 24.0, h - TOP - STATUS - 6.0);
        if tree_view {
            let Some(tree) = &tree else { return };
            let t = tree.lock().unwrap_or_else(|e| e.into_inner());
            tree_rows = crate::disktree::rows(&t, &open, tree_sort);
            // The toolbar.
            for (i, (glyph, label, bw)) in TOOLS.iter().enumerate() {
                let bx = tool_x(i);
                let enabled = i != 1 || !t.done;
                let lit = tool_hot == Some(i) && enabled;
                p.rrect(bx, TOP + 4.0, *bw, TOOL_H - 12.0, 6.0, Color::white(if lit { 0.10 } else { 0.05 }));
                let ink = Color::white(if enabled { 0.92 } else { 0.35 });
                let gc = match i {
                    0 => Color(0.36, 0.78, 0.45, if enabled { 1.0 } else { 0.4 }),
                    1 => Color(0.95, 0.36, 0.33, if enabled { 1.0 } else { 0.35 }),
                    _ => BAR,
                };
                if let Some(ic) = &f.icons {
                    p.text(
                        &char::from_u32(*glyph).unwrap_or(' ').to_string(),
                        ic,
                        bx + 8.0,
                        TOP + 4.0,
                        20.0,
                        TOOL_H - 12.0,
                        Align::Center,
                        gc,
                    );
                }
                p.text(label, &f.body, bx + 32.0, TOP + 4.0, bw - 36.0, TOOL_H - 12.0, Align::Left, ink);
            }
            let (ty, th) = (TOP + TOOL_H, mh - TOOL_H);
            let max = (tree_rows.len() as f32 * TREE_ROW - (th - TREE_HEAD)).max(0.0);
            let scroll = tree_scroll.min(max);
            let icons = stock.as_ref().and_then(|(folder, file)| {
                Some((p.bitmap(TREE_ICON, TREE_ICON, folder)?, p.bitmap(TREE_ICON, TREE_ICON, file)?))
            });
            paint_tree(
                p,
                f,
                (12.0, ty, w - 24.0, th),
                &t,
                &tree_rows,
                &open,
                scroll,
                tree_sel,
                tree_hot,
                icons.as_ref(),
                tree_sort,
            );
            let root = &t.nodes[0];
            // Working it out: a bar running along the bottom.
            if !t.done {
                let run = (std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f32())
                    * 0.6)
                    % 1.0;
                p.rrect(12.0, h - STATUS - 2.0, w - 24.0, 2.0, 1.0, Color::white(0.06));
                p.rrect(12.0 + (w - 24.0) * run * 0.8, h - STATUS - 2.0, (w - 24.0) * 0.2, 2.0, 1.0, BAR);
            }
            let status = if t.indexed {
                format!("{} \u{b7} from the file index, so it's all here already", size_text(root.size))
            } else if t.done && t.stopped() {
                format!(
                    "Stopped \u{b7} {} so far \u{b7} {} files \u{b7} {} folders",
                    size_text(root.size),
                    root.files,
                    root.folders
                )
            } else if t.done {
                format!("{} \u{b7} {} files \u{b7} {} folders", size_text(root.size), root.files, root.folders)
            } else {
                format!(
                    "Working it out\u{2026} {} so far \u{b7} {} files \u{b7} {} folders found",
                    size_text(root.size),
                    root.files,
                    root.folders
                )
            };
            p.text(&status, &f.small, 14.0, h - STATUS + 6.0, w - 28.0, 18.0, Align::Left, Color::white(0.6));
            return;
        }
        p.rrect(w - LIST_W - 6.0, TOP, LIST_W - 6.0, mh, 8.0, PANEL);
        let Some(l) = listing else {
            p.text(
                &format!("Adding up what\u{2019}s in here\u{2026} {counted} files so far"),
                &f.body,
                mx,
                my + mh / 2.0 - 10.0,
                mw,
                20.0,
                Align::Center,
                Color::white(0.6),
            );
            return;
        };
        // The map: the biggest items, and the rest together as one.
        let shown: Vec<usize> = (0..l.items.len().min(150)).filter(|&i| l.items[i].2 > 0).collect();
        let rest: u64 = l.items.iter().skip(150).map(|i| i.2).sum();
        let mut sizes: Vec<f64> = shown.iter().map(|&i| l.items[i].2 as f64).collect();
        if rest > 0 {
            sizes.push(rest as f64);
        }
        let boxes = squarify(&sizes, mx, my, mw, mh);
        for (k, b) in boxes.iter().enumerate() {
            let idx = shown.get(k).copied();
            let (bx, by, bw, bh) = (b.0 + GAP / 2.0, b.1 + GAP / 2.0, (b.2 - GAP).max(0.0), (b.3 - GAP).max(0.0));
            let c = match idx {
                Some(i) => colour(&l.items[i].0, l.items[i].1, i),
                None => Color::white(0.15),
            };
            let lit = idx.is_some() && idx == hovered;
            p.rrect(bx, by, bw, bh, 3.0, if lit { Color(c.0 * 1.2, c.1 * 1.2, c.2 * 1.2, 1.0) } else { c.alpha(0.85) });
            if bw > 60.0 && bh > 30.0 {
                let name = idx.map_or("Smaller items".to_string(), |i| l.items[i].0.clone());
                let size = idx.map_or(rest, |i| l.items[i].2);
                p.text(&name, &f.label, bx + 6.0, by + 4.0, bw - 12.0, 16.0, Align::Left, Color(0.0, 0.0, 0.0, 0.85));
                if bh > 46.0 {
                    p.text(
                        &size_text(size),
                        &f.small,
                        bx + 6.0,
                        by + 20.0,
                        bw - 12.0,
                        14.0,
                        Align::Left,
                        Color(0.0, 0.0, 0.0, 0.65),
                    );
                }
            }
            rects.push((bx, by, bw, bh, idx));
        }
        // The list: biggest first, with a bar for its share.
        let lx = w - LIST_W + 6.0;
        p.text(
            &format!("{} in all", size_text(l.total)),
            &f.label,
            lx,
            TOP + 12.0,
            LIST_W - 30.0,
            18.0,
            Align::Left,
            Color::white(0.6),
        );
        for (i, (name, dir, size)) in l.items.iter().enumerate() {
            let y = TOP + 40.0 + i as f32 * 34.0;
            if y + 34.0 > TOP + mh {
                break;
            }
            if Some(i) == hovered {
                p.rrect(lx - 4.0, y, LIST_W - 22.0, 32.0, 5.0, Color::white(0.06));
            }
            let share = if l.total > 0 { *size as f32 / l.total as f32 } else { 0.0 };
            let label = if *dir { format!("{name}\\") } else { name.clone() };
            p.text(&label, &f.body, lx + 4.0, y + 3.0, LIST_W - 120.0, 18.0, Align::Left, Color::white(0.92));
            p.text(
                &size_text(*size),
                &f.small,
                lx + LIST_W - 120.0,
                y + 5.0,
                88.0,
                16.0,
                Align::Right,
                Color::white(0.6),
            );
            p.rrect(lx + 4.0, y + 24.0, LIST_W - 40.0, 3.0, 1.5, LINE);
            p.rrect(lx + 4.0, y + 24.0, (LIST_W - 40.0) * share, 3.0, 1.5, colour(name, *dir, i));
        }
        // Status: what's under the pointer.
        let status = match hovered.and_then(|i| l.items.get(i)) {
            Some((n, d, sz)) => format!(
                "{}{}  \u{b7}  {}  \u{b7}  {:.1}% of this folder",
                n,
                if *d { " (folder)" } else { "" },
                size_text(*sz),
                if l.total > 0 { *sz as f64 * 100.0 / l.total as f64 } else { 0.0 }
            ),
            None => "Click a folder to go into it \u{b7} Backspace to go up \u{b7} right-click for more".into(),
        };
        p.text(&status, &f.small, 14.0, h - STATUS + 6.0, w - 28.0, 18.0, Align::Left, Color::white(0.6));
    });
    s.rects = rects;
    if tree_view {
        s.tree_rows = tree_rows;
    }
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_PAINT => {
            if with(paint).is_none() {
                return DefWindowProcW(hwnd, m, wp, lp);
            }
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE | WM_DPICHANGED => appwin::redraw(hwnd),
        WM_TIMER | TREE_MORE => appwin::redraw(hwnd),
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f32;
            with(|s| {
                if s.tree_view {
                    let (_, h) = appwin::client(hwnd);
                    let view = h - TOP - STATUS - 6.0 - TOOL_H - TREE_HEAD;
                    let max = (s.tree_rows.len() as f32 * TREE_ROW - view).max(0.0);
                    s.tree_scroll = (s.tree_scroll - delta / 120.0 * TREE_ROW * 3.0).clamp(0.0, max);
                    appwin::redraw(hwnd);
                }
            });
            return LRESULT(0);
        }
        DONE => {
            with(|s| s.walked());
        }
        WM_MOUSEMOVE => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| {
                if s.tree_view {
                    let hot = s.tree_row_at(y);
                    let tool = s.tool_at(x, y);
                    if hot != s.tree_hot || tool != s.tool_hot {
                        s.tree_hot = hot;
                        s.tool_hot = tool;
                        appwin::redraw(hwnd);
                    }
                    return;
                }
                let h = s.hit(x, y);
                if h != s.hovered {
                    s.hovered = h;
                    appwin::redraw(hwnd);
                }
            });
        }
        WM_LBUTTONUP => {
            let (x, y) = appwin::mouse(hwnd, lp);
            let (w, _) = appwin::client(hwnd);
            with(|s| {
                if y < TOP {
                    if x < 46.0 {
                        s.up();
                    }
                    let (sx, sw) = SWITCH;
                    if x >= sx && x < sx + sw {
                        s.show_tree(x < sx + sw / 2.0);
                        appwin::redraw(hwnd);
                        return;
                    }
                    let n = s.drives.len();
                    for (k, (l, _, _)) in s.drives.clone().into_iter().enumerate() {
                        let dx = w - (n - k) as f32 * 96.0 - 4.0;
                        if x >= dx && x < dx + 88.0 {
                            s.go(PathBuf::from(format!("{l}:\\")));
                        }
                    }
                    return;
                }
                if s.tree_view {
                    if let Some(i) = s.tool_at(x, y) {
                        s.tool(i);
                        return;
                    }
                    if s.tree_header(x, y, w) {
                        return;
                    }
                    s.tree_click(x, y, false);
                    return;
                }
                if let Some(i) = s.hit(x, y) {
                    if s.listing.as_ref().is_some_and(|l| l.items[i].1) {
                        s.open(i);
                    }
                }
            });
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| {
                if s.tree_view {
                    s.tree_click(x, y, true);
                    return;
                }
                if let Some(i) = s.hit(x, y) {
                    s.open(i);
                }
            });
        }
        WM_RBUTTONUP => {
            let (x, y) = appwin::mouse(hwnd, lp);
            if with(|s| s.tree_view) == Some(true) {
                with(|s| s.tree_menu(y));
                return LRESULT(0);
            }
            let i = with(|s| s.hit(x, y)).flatten();
            if let Some(i) = i {
                with(|s| s.menu(i));
            }
        }
        WM_XBUTTONUP | WM_KEYDOWN if m == WM_XBUTTONUP || wp.0 == VK_BACK.0 as usize => {
            with(|s| s.up());
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, m, wp, lp)
}

/// `cascade.exe --disk [folder]`.
pub fn run(start: Option<String>) {
    appwin::init();
    let Ok(gfx) = Gfx::new() else { return };
    let Some(hwnd) = appwin::create(w!("CascadeDisk"), "Disk usage", 1180, 760, Some(proc)) else { return };
    appwin::identity(hwnd, "Cascade.DiskUsage");
    let path = PathBuf::from(start.unwrap_or_else(|| "C:\\".into()));
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            hwnd,
            gfx,
            path: path.clone(),
            listing: None,
            cache: HashMap::new(),
            progress: Arc::new(Mutex::new((0, None))),
            rects: Vec::new(),
            hovered: None,
            drives: drives(),
            tree_view: true,
            tree: None,
            open: std::collections::HashSet::new(),
            tree_scroll: 0.0,
            tree_sel: None,
            tree_hot: None,
            tree_rows: Vec::new(),
            tree_sort: crate::disktree::Sort::Size,
            tool_hot: None,
            stock: crate::disktree::stock_icons(TREE_ICON),
        })
    });
    with(|s| s.go(path));
    appwin::run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn treemap_fills_the_box() {
        let r = squarify(&[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0], 0.0, 0.0, 600.0, 400.0);
        let area: f32 = r.iter().map(|b| b.2 * b.3).sum();
        assert!((area - 240_000.0).abs() < 1.0);
        assert!(r.iter().all(|b| b.0 >= -0.01 && b.1 >= -0.01 && b.0 + b.2 <= 600.01 && b.1 + b.3 <= 400.01));
    }
}
