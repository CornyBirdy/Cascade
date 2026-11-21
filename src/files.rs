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
const TILE_W: f32 = 112.0;
const TILE_H: f32 = 124.0;
const TAB_W: f32 = 200.0;

const LOADED: u32 = WM_APP + 101;
const ICONS: u32 = WM_APP + 102;
const WATCHED: u32 = WM_APP + 103;
const EDIT_KEY: u32 = WM_APP + 104;
const ID_ADDR: usize = 10;
const ID_SEARCH: usize = 11;
const ID_RENAME: usize = 12;

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
    Item(usize),
    Empty,
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
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Loads finished on worker threads: (generation, items).
static LOADS: Mutex<Vec<(u32, Vec<Item>)>> = Mutex::new(Vec::new());
static NEXT_GEN: AtomicU32 = AtomicU32::new(1);
/// The folder being watched for changes (its generation; a newer one stops the old watcher).
static WATCH_GEN: AtomicU32 = AtomicU32::new(0);

fn glyph(code: u32) -> String {
    char::from_u32(code).map(String::from).unwrap_or_default()
}

/// The sidebar: your folders, This PC and each drive, the Recycle Bin, Network.
fn sidebar() -> Vec<(char, String, Loc)> {
    let mut out = vec![('\u{E80F}', "Home".to_string(), Loc::Home)];
    unsafe {
        let folders: [(&windows::core::GUID, char); 6] = [
            (&FOLDERID_Desktop, '\u{E8FC}'),
            (&FOLDERID_Downloads, '\u{E896}'),
            (&FOLDERID_Documents, '\u{E8A5}'),
            (&FOLDERID_Pictures, '\u{E91B}'),
            (&FOLDERID_Music, '\u{E8D6}'),
            (&FOLDERID_Videos, '\u{E714}'),
        ];
        for (id, g) in folders {
            if let Ok(p) = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) {
                let path = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
                out.push((g, name, Loc::Path(path)));
            }
        }
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
const CMDS: [(u32, &str); 9] = [
    (0xE710, "New"),
    (0xE8C6, "Cut"),
    (0xE8C8, "Copy"),
    (0xE77F, "Paste"),
    (0xE8AC, "Rename"),
    (0xE74D, "Delete"),
    (0xE8CB, "Sort"),
    (0xE8FD, "View"),
    (0xE712, "More"),
];

/// The command bar as Explorer's: New with its label, then Cut, Copy,
/// Paste, Rename and Delete as icons, Sort and View with labels, and More,
/// in groups with a line between them. (x, width) of command `i`.
fn cmd_rect(i: usize) -> (f32, f32) {
    const W: [f32; 9] = [84.0, 40.0, 40.0, 40.0, 40.0, 40.0, 84.0, 84.0, 40.0];
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
    matches!(i, 1 | 6 | 8)
}

/// Commands that show their label (and a chevron: they open a menu, or New).
fn labelled(i: usize) -> bool {
    matches!(i, 0 | 6 | 7)
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

    fn cols(&self) -> usize {
        let (_, _, cw, _) = self.content();
        ((cw - 16.0) / TILE_W).floor().max(1.0) as usize
    }

    /// Where item `pos` (in display order) is: (x, y, w, h), before scrolling.
    fn item_rect(&self, pos: usize) -> (f32, f32, f32, f32) {
        let (x0, _, cw, _) = self.content();
        let top = self.rows_top();
        match self.view {
            View::Details => (x0 + 8.0, top + pos as f32 * ROW_H, cw - 24.0, ROW_H),
            View::Icons => {
                let c = self.cols();
                (x0 + 8.0 + (pos % c) as f32 * TILE_W, top + (pos / c) as f32 * TILE_H, TILE_W - 6.0, TILE_H - 6.0)
            }
        }
    }

    fn content_height(&self) -> f32 {
        let n = self.t().order.len();
        match self.view {
            View::Details => n as f32 * ROW_H + 8.0,
            View::Icons => (n.div_ceil(self.cols())) as f32 * TILE_H + 8.0,
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

    fn load(&mut self) {
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
        self.resort();
        appwin::redraw(self.hwnd);
    }

    fn resort(&mut self) {
        let (col, asc) = self.sort;
        for tab in &mut self.tabs {
            let f = tab.filter.to_lowercase();
            let mut order: Vec<usize> = (0..tab.items.len())
                .filter(|&i| f.is_empty() || tab.items[i].name.to_lowercase().contains(&f))
                .collect();
            let items = &tab.items;
            order.sort_by(|&a, &b| {
                let (x, y) = (&items[a], &items[b]);
                // Folders first, whichever way.
                let folders = y.folder.cmp(&x.folder);
                let by = match col {
                    1 => x.modified.cmp(&y.modified),
                    2 => x.kind.to_lowercase().cmp(&y.kind.to_lowercase()),
                    3 => x.size.unwrap_or(0).cmp(&y.size.unwrap_or(0)),
                    _ => natural(&x.name, &y.name),
                };
                folders.then(if asc { by } else { by.reverse() }).then_with(|| natural(&x.name, &y.name))
            });
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
        }
    }

    fn view_menu(&mut self) {
        use crate::menu::{check, show, Entry, Place};
        let entries = vec![
            check(1, "Details", self.view == View::Details),
            check(2, "Icons", self.view == View::Icons),
            Entry::Separator,
            check(3, "Hidden items", self.show_hidden),
        ];
        match show(self.hwnd, entries, Place::Cursor) {
            1 => self.view = View::Details,
            2 => self.view = View::Icons,
            3 => {
                self.show_hidden = !self.show_hidden;
                self.load();
            }
            _ => {}
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

    fn more_menu(&mut self) {
        use crate::menu::{item, show, Entry, Place};
        let entries = vec![
            item(1, "Open in Terminal"),
            item(2, "Copy path"),
            item(3, "Disk usage of this folder"),
            Entry::Separator,
            item(4, "Select all"),
            item(5, "Select none"),
            Entry::Separator,
            item(6, "Properties"),
        ];
        let dir = self.t().loc.dir();
        match show(self.hwnd, entries, Place::Cursor) {
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
            item(7, "Properties"),
        ];
        match show(self.hwnd, entries, Place::Cursor) {
            1 => self.load(),
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
            10 => self.view = View::Details,
            11 => self.view = View::Icons,
            n @ 20..=23 => {
                self.sort = (n - 20, true);
                self.resort();
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
            5 => self.delete(false),
            6 => self.sort_menu(),
            7 => self.view_menu(),
            8 => self.more_menu(),
            _ => {}
        }
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
            let mut cx = x0 + 8.0 + self.name_w(cw);
            if x < cx {
                return Some(Hit::Header(0));
            }
            for (i, (_, cwid)) in COLS.iter().enumerate().skip(1) {
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

    fn name_w(&self, cw: f32) -> f32 {
        (cw - 24.0 - COLS[1..].iter().map(|c| c.1).sum::<f32>()).max(160.0)
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
            View::Icons => (TILE_H, self.cols()),
        };
        let first = ((scroll / row).floor() as usize) * per;
        let count = (((ch - (top - y0)) / row).ceil() as usize + 2) * per;
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
        let hwnd = self.hwnd;
        let Some(mut gfx) = self.gfx.take() else { return };
        appwin::paint(hwnd, &mut gfx, |p, f, w, h| self.draw(p, f, w, h, &icons));
        self.gfx = Some(gfx);
    }

    fn draw(&self, p: &Painter, f: &crate::gfx::Fonts, w: f32, h: f32, icons: &[(usize, Option<ID2D1Bitmap>)]) {
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
        let can = [!t.back.is_empty(), !t.forward.is_empty(), !matches!(t.loc, Loc::Home), true];
        for (k, (code, hit)) in
            [(0xE72B, Hit::Back), (0xE72A, Hit::Forward), (0xE74A, Hit::Up), (0xE72C, Hit::Refresh)].iter().enumerate()
        {
            let x = 8.0 + k as f32 * 36.0;
            if hov == Some(*hit) && can[k] {
                p.rrect(x, TABS_H + 7.0, 32.0, 30.0, 5.0, Color::white(0.08));
            }
            glyph_at(*code, x + 7.0, TABS_H + 12.0, 18.0, Color::white(if can[k] { 0.85 } else { 0.3 }));
        }
        let (ax, aw, sw) = self.box_layout(w);
        p.rrect(ax, TABS_H + 7.0, aw, 30.0, 5.0, Color::white(0.06));
        p.rrect(w - sw - 10.0, TABS_H + 7.0, sw, 30.0, 5.0, Color::white(0.06));
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
                5 => Color(0.95, 0.45, 0.42, 1.0),
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
            glyph_at(*g as u32, 16.0, y + 7.0, 16.0, Color::white(0.8));
            p.text(name, &f.body, 42.0, y, SIDE_W - 52.0, 30.0, Align::Left, Color::white(0.9));
        }
        // Content.
        let (x0, cy0, cw, chh) = self.content();
        p.clip(x0, cy0, cw, chh);
        let scroll = t.scroll;
        if self.view == View::Details {
            let mut cx = x0 + 8.0;
            let nw = self.name_w(cw);
            for (i, (name, cwid)) in COLS.iter().enumerate() {
                let width = if i == 0 { nw } else { *cwid };
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
                        let cwid = COLS[k + 1].1;
                        let align = if k == 2 && item.space.is_none() { Align::Right } else { Align::Left };
                        p.text(text, &f.small, cx + 10.0, iy, cwid - 18.0, ih, align, Color::white(0.6));
                        cx += cwid;
                    }
                }
                View::Icons => {
                    let s = 72.0;
                    if let Some(b) = bmp {
                        p.image(b, ix + (iw - s) / 2.0, iy + 6.0, s, s, alpha);
                    }
                    if let Some((free, total)) = item.space {
                        let frac = if total > 0 { 1.0 - free as f32 / total as f32 } else { 0.0 };
                        p.rrect(ix + 12.0, iy + 80.0, iw - 24.0, 4.0, 2.0, Color::white(0.12));
                        p.rrect(ix + 12.0, iy + 80.0, (iw - 24.0) * frac, 4.0, 2.0, accent);
                    }
                    p.text_wrap(&item.name, &f.small, ix + 4.0, iy + 86.0, iw - 8.0, 30.0, Color::white(0.95 * alpha));
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

    fn place_boxes(&self) {
        let (w, _) = appwin::client(self.hwnd);
        let k = appwin::scale(self.hwnd);
        let (ax, aw, sw) = self.box_layout(w);
        unsafe {
            let flags = SWP_NOZORDER | SWP_NOACTIVATE;
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
            Some(Hit::Refresh) => self.load(),
            Some(Hit::Cmd(i)) => self.command(i),
            Some(Hit::Caption(b)) => appwin::caption_press(self.hwnd, b),
            Some(Hit::Side(i)) => {
                let loc = self.side[i].2.clone();
                self.go(loc);
            }
            Some(Hit::Header(i)) => {
                self.sort = if self.sort.0 == i { (i, !self.sort.1) } else { (i, i != 1 && i != 3) };
                self.resort();
            }
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

    fn on_wheel(&mut self, delta: i16) {
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
            VK_RETURN => self.open_selected(),
            VK_BACK => self.up(),
            VK_LEFT if alt => self.back(),
            VK_RIGHT if alt => self.forward(),
            VK_UP if alt => self.up(),
            VK_F2 => self.start_rename(),
            VK_F5 => self.load(),
            VK_DELETE => self.delete(shift),
            VK_F4 => self.focus_addr(),
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

    fn focus_addr(&self) {
        unsafe {
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
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                gfx_generation: gfx.generation,
                gfx: Some(gfx),
                tabs: vec![Tab::new(loc)],
                cur: 0,
                view: View::Details,
                sort: (0, true),
                show_hidden: false,
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
                side: sidebar(),
                typed: (String::new(), Instant::now()),
            })
        });
    }
    with(|s| {
        s.place_boxes();
        s.load();
    });
    appwin::run();
}
