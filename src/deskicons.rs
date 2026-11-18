//! Desktop icons in shell mode: what Explorer's desktop shows (your Desktop
//! folder and the public one, and the icons Windows is set to show there,
//! like This PC and the Recycle Bin), in columns from the top left of the
//! main screen. Double-click one to open it; right-click it for its own
//! menu, the one Explorer shows (open with, cut, copy, delete, properties
//! and whatever programs add); drop files on the desktop to put them there.
//! Right-click the desktop for New folder, and to hide or show the icons
//! (Windows' own setting for it).
//!
//! The icons sit in a see-through window just above the desktop (below
//! every app), so a wallpaper app's picture shows behind them.
//!
//! As in Explorer: drag a rubber band over the wallpaper to select icons,
//! Ctrl+click and Shift+click to add, and the keys: Enter, Delete (Shift for
//! good), F2 to rename, Ctrl+A, Ctrl+C, Ctrl+X, Ctrl+V, the arrows, F5 and
//! the menu key. Not (yet) as in Explorer: icons are arranged by name, not
//! where you drag them, and can't be dragged out onto apps.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

use windows::core::{implement, Interface, Ref, Result, HSTRING, PCSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::Registry::HKEY_CURRENT_USER;
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::System::SystemServices::{SFGAO_FILESYSTEM, SFGAO_FOLDER};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gfx::{Color, Gfx, Layer};
use crate::{config, msg};

const CELL_W: f32 = 80.0;
const CELL_H: f32 = 90.0;
const ICON: f32 = 44.0;
const MARGIN: f32 = 6.0;
const ADVANCED: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced";

/// An icon on the desktop.
pub struct Item {
    /// How the shell names it (a path, or ::{GUID} for This PC and the like).
    pub path: String,
    pub name: String,
    /// Premultiplied BGRA, `icons::ICON_SIZE` square (empty if none).
    pixels: Vec<u8>,
}

/// Whether Windows is set to show desktop icons.
pub fn shown() -> bool {
    crate::winset::reg_dword(HKEY_CURRENT_USER, ADVANCED, "HideIcons") != Some(1)
}

pub fn set_shown(on: bool) {
    crate::winset::set_reg_dword(HKEY_CURRENT_USER, ADVANCED, "HideIcons", (!on) as u32);
    msg::post(msg::DESK_ICONS, 0, 0);
}

fn desktop_dirs() -> Vec<PathBuf> {
    [
        std::env::var_os("USERPROFILE").map(|u| PathBuf::from(u).join("Desktop")),
        std::env::var_os("PUBLIC").map(|u| PathBuf::from(u).join("Desktop")),
    ]
    .into_iter()
    .flatten()
    .filter(|p| p.is_dir())
    .collect()
}

/// The user's own Desktop folder (where new folders and dropped files go).
fn user_desktop() -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

unsafe fn take(p: PWSTR) -> String {
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

/// Reads what the desktop shows, on a thread of its own; posts `msg::DESK_ITEMS`.
pub fn spawn_load() {
    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let items = load();
        let ptr = Box::into_raw(Box::new(items));
        if !msg::post(msg::DESK_ITEMS, 0, ptr as isize) {
            drop(Box::from_raw(ptr));
        }
    });
}

unsafe fn load() -> Vec<Item> {
    let mut out: Vec<(u8, Item)> = Vec::new();
    let wic: Option<IWICImagingFactory> = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok();
    let Ok(desktop) = SHGetDesktopFolder() else { return Vec::new() };
    let mut list: Option<IEnumIDList> = None;
    let flags = (SHCONTF_FOLDERS.0 | SHCONTF_NONFOLDERS.0) as u32;
    if desktop.EnumObjects(HWND::default(), flags, &mut list).is_err() {
        return Vec::new();
    }
    let Some(list) = list else { return Vec::new() };
    loop {
        let mut pidl = [std::ptr::null_mut(); 1];
        let mut got = 0u32;
        if list.Next(&mut pidl, Some(&mut got)).is_err() || got == 0 {
            break;
        }
        let pidl = pidl[0];
        let item: Result<IShellItem> = SHCreateItemFromIDList(pidl);
        CoTaskMemFree(Some(pidl as *const _));
        let Ok(item) = item else { continue };
        let (Ok(name), Ok(path)) =
            (item.GetDisplayName(SIGDN_NORMALDISPLAY), item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING))
        else {
            continue;
        };
        let (name, path) = (take(name), take(path));
        let attrs = item.GetAttributes(SFGAO_FOLDER | SFGAO_FILESYSTEM).unwrap_or_default();
        // Windows' own icons (This PC, the Recycle Bin) first, then folders, then files.
        let rank = if attrs.0 & SFGAO_FILESYSTEM.0 == 0 {
            0
        } else if attrs.0 & SFGAO_FOLDER.0 != 0 {
            1
        } else {
            2
        };
        let pixels = wic.as_ref().and_then(|w| crate::icons::load_icon(&item, w).ok()).unwrap_or_default();
        out.push((rank, Item { path, name, pixels }));
    }
    out.sort_by_key(|(rank, item)| (*rank, item.name.to_lowercase()));
    out.into_iter().map(|(_, i)| i).collect()
}

/// Watches the desktop folders, and says when something in them changed.
pub fn watch() {
    use windows::Win32::Storage::FileSystem::*;
    use windows::Win32::System::Threading::{WaitForMultipleObjects, INFINITE};
    std::thread::spawn(|| unsafe {
        let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME;
        let handles: Vec<HANDLE> = desktop_dirs()
            .iter()
            .filter_map(|d| FindFirstChangeNotificationW(&HSTRING::from(d.as_os_str()), false, filter).ok())
            .collect();
        if handles.is_empty() {
            return;
        }
        loop {
            let r = WaitForMultipleObjects(&handles, false, INFINITE);
            let i = r.0 as usize;
            if i >= handles.len() {
                return;
            }
            // Changes come in bursts (a copy of many files): wait for the end of it.
            std::thread::sleep(std::time::Duration::from_millis(400));
            for h in &handles {
                let _ = FindNextChangeNotification(*h);
            }
            msg::post(msg::DESK_ICONS, 0, 0);
        }
    });
}

/// The whole desktop, every screen (virtual-screen pixels).
fn virtual_screen() -> RECT {
    unsafe {
        let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (w, h) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        RECT { left: x, top: y, right: x + w.max(1), bottom: y + h.max(1) }
    }
}

/// The icons' window. As Explorer's icon view, it covers the whole desktop
/// (every screen) just above the wallpaper: it takes the desktop's clicks
/// and keys, sets the pointer (a wallpaper app's window under it never
/// does), and draws the icons down the main screen's left side.
pub struct DeskIcons {
    pub hwnd: HWND,
    layer: Layer,
    items: Vec<Item>,
    bitmaps: Vec<Option<ID2D1Bitmap>>,
    generation: u32,
    /// Selected, one flag per item.
    sel: Vec<bool>,
    /// Where Shift+click and the arrow keys go from.
    anchor: Option<usize>,
    hovered: Option<usize>,
    tracking: bool,
    last_click: Option<(usize, Instant)>,
    /// A rubber band being dragged: where it started and where the pointer
    /// is (window pixels), and what was selected before it (Ctrl adds).
    band: Option<(POINT, POINT, Vec<bool>)>,
    /// The main screen's free area (the icons go down its left side).
    area: RECT,
    /// The whole desktop: where the window is.
    screen: RECT,
    k: f32,
    pub dirty: bool,
}

/// What a click on the desktop asks for, done by the app outside its handler.
pub enum Ask {
    /// The shell menu for these items.
    ItemMenu(Vec<String>),
    /// The desktop's own menu (New folder, Show desktop icons, wallpaper).
    DesktopMenu,
}

impl DeskIcons {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            layer: Layer::default(),
            items: Vec::new(),
            bitmaps: Vec::new(),
            generation: 0,
            sel: Vec::new(),
            anchor: None,
            hovered: None,
            tracking: false,
            last_click: None,
            band: None,
            area: RECT::default(),
            screen: RECT::default(),
            k: 1.0,
            dirty: true,
        }
    }

    pub fn set_items(&mut self, items: Vec<Item>) {
        let keep: Vec<String> = self.selected_paths();
        self.items = items;
        self.bitmaps = (0..self.items.len()).map(|_| None).collect();
        self.sel = self.items.iter().map(|i| keep.contains(&i.path)).collect();
        self.anchor = self.anchor.filter(|&a| a < self.items.len());
        self.hovered = None;
        self.dirty = true;
    }

    pub fn place(&mut self, area: RECT, k: f32) {
        let screen = virtual_screen();
        if area != self.area || k != self.k || screen != self.screen {
            self.area = area;
            self.screen = screen;
            self.k = k;
            self.dirty = true;
        }
    }

    fn selected_paths(&self) -> Vec<String> {
        self.items.iter().zip(&self.sel).filter(|(_, s)| **s).map(|(i, _)| i.path.clone()).collect()
    }

    /// The icons' corner in the window (logical units).
    fn origin(&self) -> (f32, f32) {
        ((self.area.left - self.screen.left) as f32 / self.k, (self.area.top - self.screen.top) as f32 / self.k)
    }

    fn rows(&self) -> usize {
        let h = (self.area.bottom - self.area.top) as f32 / self.k - 2.0 * MARGIN;
        ((h / CELL_H).floor() as usize).max(1)
    }

    /// Item `i`'s cell (logical, in the window).
    fn cell(&self, i: usize) -> (f32, f32) {
        let rows = self.rows();
        let (ox, oy) = self.origin();
        (ox + MARGIN + (i / rows) as f32 * CELL_W, oy + MARGIN + (i % rows) as f32 * CELL_H)
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        if !shown() {
            return None;
        }
        let (ox, oy) = self.origin();
        let (lx, ly) = (x as f32 / self.k - ox - MARGIN, y as f32 / self.k - oy - MARGIN);
        if lx < 0.0 || ly < 0.0 {
            return None;
        }
        let (c, r) = ((lx / CELL_W) as usize, (ly / CELL_H) as usize);
        let rows = self.rows();
        if r >= rows {
            return None;
        }
        let i = c * rows + r;
        (i < self.items.len()).then_some(i)
    }

    fn select_only(&mut self, i: Option<usize>) {
        self.sel.iter_mut().for_each(|s| *s = false);
        if let Some(s) = i.and_then(|i| self.sel.get_mut(i)) {
            *s = true;
        }
        self.anchor = i;
        self.dirty = true;
    }

    pub fn on_move(&mut self, x: i32, y: i32) {
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
        if let Some((start, _, before)) = self.band.take() {
            let now = POINT { x, y };
            // Everything the band touches is selected (added to what was, with Ctrl).
            let (l, t) = (start.x.min(x) as f32 / self.k, start.y.min(y) as f32 / self.k);
            let (r, b) = (start.x.max(x) as f32 / self.k, start.y.max(y) as f32 / self.k);
            for i in 0..self.items.len() {
                let (cx, cy) = self.cell(i);
                let (cx0, cy0, cx1, cy1) = (cx + 6.0, cy + 4.0, cx + CELL_W - 6.0, cy + CELL_H - 4.0);
                let touched = cx0 < r && cx1 > l && cy0 < b && cy1 > t;
                self.sel[i] = before.get(i).copied().unwrap_or(false) || touched;
            }
            self.band = Some((start, now, before));
            self.dirty = true;
            return;
        }
        let h = self.hit(x, y);
        if h != self.hovered {
            self.hovered = h;
            self.dirty = true;
        }
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        if self.hovered.take().is_some() {
            self.dirty = true;
        }
    }

    /// A press: selects (Ctrl adds or takes away, Shift selects a run), a
    /// second press soon after on the same icon opens it, and a press on the
    /// wallpaper starts a rubber band.
    pub fn on_press(&mut self, x: i32, y: i32, ctrl: bool, shift: bool) {
        let hit = self.hit(x, y);
        let now = Instant::now();
        let double = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() } as u128;
        if let (Some(i), Some((j, t))) = (hit, self.last_click) {
            if i == j && now.duration_since(t).as_millis() <= double && !ctrl && !shift {
                self.open_selected();
                self.last_click = None;
                return;
            }
        }
        self.last_click = hit.map(|i| (i, now));
        match hit {
            Some(i) if ctrl => {
                self.sel[i] = !self.sel[i];
                self.anchor = Some(i);
            }
            Some(i) if shift => {
                let a = self.anchor.unwrap_or(i);
                let (lo, hi) = (a.min(i), a.max(i));
                for (k, s) in self.sel.iter_mut().enumerate() {
                    *s = k >= lo && k <= hi;
                }
            }
            Some(i) => {
                if !self.sel[i] {
                    self.select_only(Some(i));
                }
                self.anchor = Some(i);
            }
            None => {
                let before = if ctrl { self.sel.clone() } else { vec![false; self.items.len()] };
                if !ctrl {
                    self.sel.iter_mut().for_each(|s| *s = false);
                }
                self.band = Some((POINT { x, y }, POINT { x, y }, before));
                unsafe {
                    SetCapture(self.hwnd);
                }
            }
        }
        self.dirty = true;
    }

    /// The button went up: a press on an icon among several selected (and
    /// no drag) leaves just that one selected, as in Explorer.
    pub fn on_release(&mut self, x: i32, y: i32, ctrl: bool, shift: bool) {
        if self.band.take().is_some() {
            unsafe {
                let _ = ReleaseCapture();
            }
            self.dirty = true;
            return;
        }
        if let Some(i) = self.hit(x, y) {
            if !ctrl && !shift && self.sel.iter().filter(|s| **s).count() > 1 {
                self.select_only(Some(i));
            }
        }
    }

    /// A right-click: the shell menu for what's selected (selecting the icon
    /// first if it wasn't), or the desktop's menu.
    pub fn on_right(&mut self, x: i32, y: i32) -> Ask {
        match self.hit(x, y) {
            Some(i) => {
                if !self.sel[i] {
                    self.select_only(Some(i));
                }
                Ask::ItemMenu(self.selected_paths())
            }
            None => {
                self.select_only(None);
                Ask::DesktopMenu
            }
        }
    }

    fn open_selected(&self) {
        for p in self.selected_paths() {
            crate::shell::open(&p);
        }
    }

    /// A key, while the desktop has the focus. Returns what to ask the app for.
    pub fn on_key(&mut self, vk: u16) -> Option<Ask> {
        use windows::Win32::UI::Input::KeyboardAndMouse::*;
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
        let (ctrl, shift) = (down(VK_CONTROL), down(VK_SHIFT));
        let paths = self.selected_paths();
        match VIRTUAL_KEY(vk) {
            VK_RETURN => self.open_selected(),
            VK_DELETE if !paths.is_empty() => verb(self.hwnd, &paths, "delete", shift),
            VK_F2 => {
                if let Some(i) = self.anchor.filter(|&a| self.sel.get(a) == Some(&true)) {
                    self.rename(i);
                }
            }
            VK_F5 => {
                msg::post(msg::DESK_ICONS, 0, 0);
            }
            VK_APPS => {
                return Some(if paths.is_empty() { Ask::DesktopMenu } else { Ask::ItemMenu(paths) });
            }
            k if ctrl && k.0 == b'A' as u16 => {
                self.sel.iter_mut().for_each(|s| *s = true);
                self.dirty = true;
            }
            k if ctrl && k.0 == b'C' as u16 && !paths.is_empty() => verb(self.hwnd, &paths, "copy", false),
            k if ctrl && k.0 == b'X' as u16 && !paths.is_empty() => verb(self.hwnd, &paths, "cut", false),
            k if ctrl && k.0 == b'V' as u16 => paste(self.hwnd),
            VK_UP | VK_DOWN | VK_LEFT | VK_RIGHT if !self.items.is_empty() => {
                let rows = self.rows() as isize;
                let at = self.anchor.map_or(-1, |a| a as isize);
                let step = match VIRTUAL_KEY(vk) {
                    VK_UP => -1,
                    VK_DOWN => 1,
                    VK_LEFT => -rows,
                    _ => rows,
                };
                let next = if at < 0 { 0 } else { (at + step).clamp(0, self.items.len() as isize - 1) } as usize;
                self.select_only(Some(next));
            }
            _ => {}
        }
        None
    }

    /// F2: a text box over the icon's name; Enter renames it, Esc leaves it.
    fn rename(&mut self, i: usize) {
        let Some(item) = self.items.get(i) else { return };
        let (cx, cy) = self.cell(i);
        let k = self.k;
        let (x, y) = (self.screen.left + ((cx - 10.0) * k) as i32, self.screen.top + ((cy + 12.0 + ICON) * k) as i32);
        unsafe {
            let instance = GetWindowLongPtrW(self.hwnd, GWLP_HINSTANCE);
            // ES_CENTER | ES_AUTOHSCROLL
            let style = WS_POPUP | WS_BORDER | WS_VISIBLE | WINDOW_STYLE(0x1 | 0x80);
            let Ok(edit) = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                windows::core::w!("EDIT"),
                &HSTRING::from(item.name.as_str()),
                style,
                x,
                y,
                ((CELL_W + 20.0) * k) as i32,
                (24.0 * k) as i32,
                Some(self.hwnd),
                None,
                Some(HINSTANCE(instance as *mut _)),
                None,
            ) else {
                return;
            };
            let font = windows::Win32::Graphics::Gdi::GetStockObject(windows::Win32::Graphics::Gdi::DEFAULT_GUI_FONT);
            SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            RENAMING.with(|r| *r.borrow_mut() = Some((edit, item.path.clone())));
            let _ = windows::Win32::UI::Shell::SetWindowSubclass(edit, Some(rename_proc), 1, 0);
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(edit));
            // EM_SETSEL: all of it, ready to type over.
            SendMessageW(edit, 0x00B1, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
    }

    pub fn render(&mut self, gfx: &mut Gfx) -> Result<()> {
        self.dirty = false;
        if self.generation != gfx.generation {
            self.bitmaps.iter_mut().for_each(|b| *b = None);
            self.generation = gfx.generation;
        }
        let icons = shown();
        if icons {
            for (i, item) in self.items.iter().enumerate() {
                if self.bitmaps[i].is_none() && !item.pixels.is_empty() {
                    let s = crate::icons::ICON_SIZE;
                    self.bitmaps[i] = gfx.bitmap(s, s, &item.pixels).ok();
                }
            }
        }
        let k = self.k;
        let (pw, ph) = (self.screen.right - self.screen.left, self.screen.bottom - self.screen.top);
        let (w, h) = (pw as f32 / k, ph as f32 / k);
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            // All but clear everywhere, so the whole desktop takes the
            // pointer (a fully clear pixel would let it through).
            p.rrect(0.0, 0.0, w, h, 0.0, Color(0.0, 0.0, 0.0, 1.0 / 255.0));
            for (i, item) in self.items.iter().enumerate().filter(|_| icons) {
                let (x, y) = self.cell(i);
                let selected = self.sel.get(i) == Some(&true);
                let hovered = Some(i) == self.hovered;
                if selected || hovered {
                    let bg = match (selected, hovered) {
                        (true, true) => crate::theme::accent().alpha(0.45),
                        (true, false) => crate::theme::accent().alpha(0.32),
                        _ => Color::white(0.10),
                    };
                    p.rrect(x + 2.0, y + 2.0, CELL_W - 4.0, CELL_H - 4.0, 6.0, bg);
                    if selected {
                        let edge = crate::theme::accent().alpha(0.6);
                        p.stroke_rrect(x + 2.5, y + 2.5, CELL_W - 5.0, CELL_H - 5.0, 6.0, edge, 1.0);
                    }
                }
                let (cx, cy) = (x + CELL_W / 2.0, y + 8.0 + ICON / 2.0);
                match &self.bitmaps[i] {
                    Some(b) => p.image(b, cx - ICON / 2.0, cy - ICON / 2.0, ICON, ICON, 1.0),
                    None => p.rrect(cx - ICON / 2.0, cy - ICON / 2.0, ICON, ICON, 8.0, Color::white(0.2)),
                }
                // The name, two lines at most, with a shadow so it reads over any wallpaper.
                let (tx, ty, tw, th) = (x + 4.0, y + 12.0 + ICON, CELL_W - 8.0, CELL_H - ICON - 16.0);
                p.text_wrap(&item.name, &f.small, tx + 1.0, ty + 1.0, tw, th, Color(0.0, 0.0, 0.0, 0.8));
                p.text_wrap(&item.name, &f.small, tx, ty, tw, th, Color::white(0.97));
            }
            // The rubber band.
            if let Some((a, b, _)) = &self.band {
                let (l, t) = (a.x.min(b.x) as f32 / k, a.y.min(b.y) as f32 / k);
                let (bw, bh) = ((a.x - b.x).abs() as f32 / k, (a.y - b.y).abs() as f32 / k);
                p.rrect(l, t, bw, bh, 2.0, crate::theme::accent().alpha(0.22));
                p.stroke_rrect(l + 0.5, t + 0.5, bw - 1.0, bh - 1.0, 2.0, crate::theme::accent().alpha(0.85), 1.0);
            }
        }
        let pos = POINT { x: self.screen.left, y: self.screen.top };
        let r = gfx.present(&self.layer, self.hwnd, pos, pw, ph, 255);
        unsafe {
            if !IsWindowVisible(self.hwnd).as_bool() {
                // Shown just above the desktop (see `keep_low`), never over apps.
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        r
    }
}

/// Keeps the icons' window just above the desktop window, below every app,
/// whatever asks it to move up (being shown, clicked, given the focus). For
/// its `WM_WINDOWPOSCHANGING`.
pub fn keep_low(hwnd: HWND, pos: &mut WINDOWPOS) {
    if pos.flags.contains(SWP_NOZORDER) {
        return;
    }
    let Some(desktop) = crate::shellmode::desktop() else { return };
    unsafe {
        match GetWindow(desktop, GW_HWNDPREV) {
            // Already right above it.
            Ok(above) if above == hwnd => pos.flags |= SWP_NOZORDER,
            // Under the window that's above the desktop: right above the desktop.
            // An always-on-top window just above it (no app open): the top of the
            // ordinary windows is right above the desktop then.
            Ok(above) if GetWindowLongW(above, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0 => {
                pos.hwndInsertAfter = HWND_TOP
            }
            Ok(above) if !above.is_invalid() => pos.hwndInsertAfter = above,
            _ => pos.hwndInsertAfter = HWND_BOTTOM,
        }
    }
}

// ------------------------------------------------------------ renaming --

thread_local! {
    /// The rename box on screen and the item it renames.
    static RENAMING: RefCell<Option<(HWND, String)>> = const { RefCell::new(None) };
}

/// Ends renaming: renames the item (through the shell, as Explorer does,
/// which keeps hidden extensions) when `keep`.
fn finish_rename(keep: bool) {
    let Some((edit, path)) = RENAMING.with(|r| r.borrow_mut().take()) else { return };
    unsafe {
        let mut buf = [0u16; 260];
        let n = GetWindowTextW(edit, &mut buf) as usize;
        let name = String::from_utf16_lossy(&buf[..n]).trim().to_string();
        let _ = DestroyWindow(edit);
        if !keep || name.is_empty() {
            return;
        }
        let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(path.as_str()), None) else {
            return;
        };
        if let Ok(op) = CoCreateInstance::<_, IFileOperation>(&FileOperation, None, CLSCTX_ALL) {
            let ok =
                op.RenameItem(&item, &HSTRING::from(name.as_str()), None).is_ok() && op.PerformOperations().is_ok();
            config::log(&format!("desktop: renamed {path} to {name}: {ok}"));
        }
        msg::post(msg::DESK_ICONS, 0, 0);
    }
}

unsafe extern "system" fn rename_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM, _: usize, _: usize) -> LRESULT {
    const DONE: u32 = WM_APP + 1;
    match m {
        WM_KEYDOWN if wp.0 == 0x0D || wp.0 == 0x1B => {
            let _ = PostMessageW(Some(hwnd), DONE, WPARAM((wp.0 == 0x0D) as usize), LPARAM(0));
            return LRESULT(0);
        }
        // Enter and Esc make no beep.
        WM_CHAR if wp.0 == 0x0D || wp.0 == 0x1B => return LRESULT(0),
        WM_KILLFOCUS => {
            let _ = PostMessageW(Some(hwnd), DONE, WPARAM(1), LPARAM(0));
        }
        DONE => {
            finish_rename(wp.0 == 1);
            return LRESULT(0);
        }
        _ => {}
    }
    windows::Win32::UI::Shell::DefSubclassProc(hwnd, m, wp, lp)
}

// ------------------------------------------------------- shell commands --

/// The shell's menu handler for several items (those in the same folder as
/// the first: a menu covers one folder's items), or for one.
unsafe fn context_menu(paths: &[String]) -> Option<IContextMenu> {
    let first = paths.first()?;
    let parent = |p: &str| PathBuf::from(p).parent().map(|d| d.to_path_buf());
    let same: Vec<&String> = paths.iter().filter(|p| parent(p) == parent(first)).collect();
    if same.len() > 1 {
        let mut pidls = Vec::new();
        for p in &same {
            let mut pidl = std::ptr::null_mut();
            if SHParseDisplayName(&HSTRING::from(p.as_str()), None, &mut pidl, 0, None).is_ok() {
                pidls.push(pidl as *const ITEMIDLIST);
            }
        }
        let array = SHCreateShellItemArrayFromIDLists(&pidls);
        for p in &pidls {
            CoTaskMemFree(Some(*p as *const _));
        }
        if let Ok(cm) = array.and_then(|a| a.BindToHandler::<_, IContextMenu>(None, &BHID_SFUIObject)) {
            return Some(cm);
        }
    }
    let item = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(first.as_str()), None).ok()?;
    item.BindToHandler::<_, IContextMenu>(None, &BHID_SFUIObject).ok()
}

/// Runs one of the shell's verbs ("delete", "copy", "cut", "paste") on items.
fn verb(owner: HWND, paths: &[String], name: &str, shift: bool) {
    unsafe {
        let Some(cm) = context_menu(paths) else { return };
        // Handlers know their verbs once they've filled a menu.
        if let Ok(menu) = CreatePopupMenu() {
            let _ = cm.QueryContextMenu(menu, 0, 1, 0x7FFF, CMF_NORMAL);
            let verb = std::ffi::CString::new(name).unwrap_or_default();
            let mut info = CMINVOKECOMMANDINFO {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                hwnd: owner,
                lpVerb: PCSTR(verb.as_ptr() as *const u8),
                nShow: SW_SHOWNORMAL.0,
                ..Default::default()
            };
            if shift {
                info.fMask |= CMIC_MASK_SHIFT_DOWN;
            }
            let r = cm.InvokeCommand(&info);
            config::log(&format!("desktop: {name} on {} item(s): {r:?}", paths.len()));
            let _ = DestroyMenu(menu);
        }
        msg::post(msg::DESK_ICONS, 0, 0);
    }
}

/// Ctrl+V: what's on the clipboard, into the Desktop folder.
pub fn paste(owner: HWND) {
    if let Some(dir) = user_desktop() {
        verb(owner, &[dir.to_string_lossy().into_owned()], "paste", false);
    }
}

/// Shows the shell's own menu for the desktop items `paths` at the pointer, and does what's picked.
pub fn item_menu(owner: HWND, paths: &[String]) {
    unsafe {
        let Some(cm) = context_menu(paths) else { return };
        let Ok(menu) = CreatePopupMenu() else { return };
        const FIRST: u32 = 1;
        if cm.QueryContextMenu(menu, 0, FIRST, 0x7FFF, CMF_NORMAL | CMF_CANRENAME).is_err() {
            let _ = DestroyMenu(menu);
            return;
        }
        // Cascade's own menu, read from the shell's: its submenus (Open
        // with, Send to) are filled by the shell as they'd be opened.
        let cm2 = cm.cast::<IContextMenu2>().ok();
        let entries = crate::menu::from_hmenu(menu, &mut |sub, i| {
            if let Some(c) = &cm2 {
                let _ = c.HandleMenuMsg(WM_INITMENUPOPUP, WPARAM(sub.0 as usize), LPARAM(i as isize));
            }
        });
        let id = crate::menu::show(owner, entries, crate::menu::Place::Cursor) as u32;
        // Rename only works in a folder view: the desktop does it itself (F2).
        let rename = id >= FIRST && {
            let mut buf = [0u8; 64];
            cm.GetCommandString(
                (id - FIRST) as usize,
                GCS_VERBA,
                None,
                windows::core::PSTR(buf.as_mut_ptr()),
                buf.len() as u32,
            )
            .is_ok()
                && buf.starts_with(b"rename\0")
        };
        let _ = DestroyMenu(menu);
        if rename {
            msg::post(msg::DESK_RENAME, 0, 0);
            return;
        }
        if id >= FIRST {
            let info = CMINVOKECOMMANDINFO {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                hwnd: owner,
                // The command's number, passed as the verb's "pointer".
                lpVerb: PCSTR(std::ptr::without_provenance((id - FIRST) as usize)),
                nShow: SW_SHOWNORMAL.0,
                ..Default::default()
            };
            let r = cm.InvokeCommand(&info);
            config::log(&format!("desktop: menu command {} on {} item(s): {r:?}", id - FIRST, paths.len()));
        }
        msg::post(msg::DESK_ICONS, 0, 0);
    }
}

impl DeskIcons {
    /// Starts renaming the selected icon (the menu's Rename).
    pub fn rename_selected(&mut self) {
        if let Some(i) =
            self.anchor.filter(|&a| self.sel.get(a) == Some(&true)).or_else(|| self.sel.iter().position(|s| *s))
        {
            self.rename(i);
        }
    }
}

/// Makes a new folder on the desktop ("New folder", or "New folder (2)" and on).
pub fn new_folder() {
    let Some(dir) = user_desktop() else { return };
    for n in 1..100 {
        let name = if n == 1 { "New folder".to_string() } else { format!("New folder ({n})") };
        let path = dir.join(name);
        if !path.exists() {
            let ok = std::fs::create_dir(&path).is_ok();
            config::log(&format!("desktop: made {} ({ok})", path.display()));
            break;
        }
    }
}

// ------------------------------------------------------------ dropping --

#[implement(IDropTarget)]
struct Drop;

fn effect(out: *mut DROPEFFECT, e: DROPEFFECT) {
    if let Some(o) = unsafe { out.as_mut() } {
        *o = e;
    }
}

/// Move within a drive, copy across drives, as Explorer does (Ctrl: always copy).
fn how(keys: MODIFIERKEYS_FLAGS, first: Option<&str>) -> DROPEFFECT {
    const MK_CONTROL: u32 = 0x8;
    let Some(dir) = user_desktop() else { return DROPEFFECT_NONE };
    if keys.0 & MK_CONTROL != 0 {
        return DROPEFFECT_COPY;
    }
    let drive = |p: &str| p.get(..2).map(str::to_ascii_lowercase);
    match first {
        Some(f) if drive(f) == drive(&dir.to_string_lossy()) => DROPEFFECT_MOVE,
        _ => DROPEFFECT_COPY,
    }
}

impl IDropTarget_Impl for Drop_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        out: *mut DROPEFFECT,
    ) -> Result<()> {
        let files = data.ok().map(crate::drop::files).unwrap_or_default();
        effect(out, if files.is_empty() { DROPEFFECT_NONE } else { how(keys, files.first().map(String::as_str)) });
        Ok(())
    }

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, _: &POINTL, out: *mut DROPEFFECT) -> Result<()> {
        effect(out, how(keys, None));
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, _: &POINTL, out: *mut DROPEFFECT) -> Result<()> {
        let files = data.ok().map(crate::drop::files).unwrap_or_default();
        let Some(dir) = user_desktop() else { return Ok(()) };
        // Files already on the desktop dropped back on it: nothing to do.
        let files: Vec<String> =
            files.into_iter().filter(|f| PathBuf::from(f).parent() != Some(dir.as_path())).collect();
        if files.is_empty() {
            effect(out, DROPEFFECT_NONE);
            return Ok(());
        }
        let e = how(keys, files.first().map(String::as_str));
        effect(out, e);
        // Windows' own copy (or move), with its progress and "replace?" boxes, on a thread of its own.
        std::thread::spawn(move || unsafe {
            let from: Vec<u16> = files.iter().flat_map(|f| f.encode_utf16().chain([0])).chain([0]).collect();
            let to: Vec<u16> = dir.as_os_str().to_string_lossy().encode_utf16().chain([0, 0]).collect();
            let mut op = SHFILEOPSTRUCTW {
                wFunc: if e == DROPEFFECT_MOVE { FO_MOVE } else { FO_COPY },
                pFrom: windows::core::PCWSTR(from.as_ptr()),
                pTo: windows::core::PCWSTR(to.as_ptr()),
                fFlags: FOF_ALLOWUNDO.0 as u16,
                ..Default::default()
            };
            let r = SHFileOperationW(&mut op);
            config::log(&format!("desktop: dropped {} item(s) (result {r})", files.len()));
        });
        Ok(())
    }
}

/// Makes `hwnd` (the desktop's windows) take dropped files.
pub fn register_drop(hwnd: HWND) {
    let target: IDropTarget = Drop.into();
    unsafe {
        let _ = RegisterDragDrop(hwnd, &target);
    }
}

pub fn revoke_drop(hwnd: HWND) {
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }
}
