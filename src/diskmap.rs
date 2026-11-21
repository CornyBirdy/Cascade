//! Disk usage (`cascade.exe --disk [folder]`): what's taking the space, as a
//! map of rectangles sized by how much each folder and file holds, with the
//! biggest listed beside it. Click a folder to go into it, Backspace or ‹
//! to go back up; right-click for Open, Show in Files, Copy path, Delete.
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

use crate::appwin::{self, size_text, LINE, PANEL};
use crate::gfx::{Align, Color, Gfx};
use crate::theme;

const TOP: f32 = 52.0;
const LIST_W: f32 = 340.0;
const STATUS: f32 = 30.0;
const GAP: f32 = 2.0;
const DONE: u32 = WM_APP + 1;

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
}

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

fn paint(s: &mut State) {
    let hwnd = s.hwnd;
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
        p.text(
            &path_text,
            &f.title,
            52.0,
            12.0,
            w - 60.0 - drives.len() as f32 * 96.0,
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
        WM_TIMER => appwin::redraw(hwnd),
        DONE => {
            with(|s| s.walked());
        }
        WM_MOUSEMOVE => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| {
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
                    let n = s.drives.len();
                    for (k, (l, _, _)) in s.drives.clone().into_iter().enumerate() {
                        let dx = w - (n - k) as f32 * 96.0 - 4.0;
                        if x >= dx && x < dx + 88.0 {
                            s.go(PathBuf::from(format!("{l}:\\")));
                        }
                    }
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
                if let Some(i) = s.hit(x, y) {
                    s.open(i);
                }
            });
        }
        WM_RBUTTONUP => {
            let (x, y) = appwin::mouse(hwnd, lp);
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
