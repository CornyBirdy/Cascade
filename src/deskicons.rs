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
//! Not (yet) as in Explorer: icons are arranged by name, not where you drag
//! them, and are renamed from their menu's Rename only where the program
//! behind it handles that.

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
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
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

/// The icons' window.
pub struct DeskIcons {
    pub hwnd: HWND,
    layer: Layer,
    items: Vec<Item>,
    bitmaps: Vec<Option<ID2D1Bitmap>>,
    generation: u32,
    selected: Option<usize>,
    hovered: Option<usize>,
    tracking: bool,
    last_click: Option<(usize, Instant)>,
    /// The main screen's free area (the icons go down its left side).
    area: RECT,
    k: f32,
    pub dirty: bool,
}

impl DeskIcons {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            layer: Layer::default(),
            items: Vec::new(),
            bitmaps: Vec::new(),
            generation: 0,
            selected: None,
            hovered: None,
            tracking: false,
            last_click: None,
            area: RECT::default(),
            k: 1.0,
            dirty: true,
        }
    }

    pub fn set_items(&mut self, items: Vec<Item>) {
        let selected = self.selected.and_then(|i| self.items.get(i)).map(|i| i.path.clone());
        self.items = items;
        self.bitmaps = (0..self.items.len()).map(|_| None).collect();
        self.selected = selected.and_then(|p| self.items.iter().position(|i| i.path == p));
        self.hovered = None;
        self.dirty = true;
    }

    pub fn place(&mut self, area: RECT, k: f32) {
        if area != self.area || k != self.k {
            self.area = area;
            self.k = k;
            self.dirty = true;
        }
    }

    fn rows(&self) -> usize {
        let h = (self.area.bottom - self.area.top) as f32 / self.k - 2.0 * MARGIN;
        ((h / CELL_H).floor() as usize).max(1)
    }

    fn cell(&self, i: usize) -> (f32, f32) {
        let rows = self.rows();
        (MARGIN + (i / rows) as f32 * CELL_W, MARGIN + (i % rows) as f32 * CELL_H)
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        let (lx, ly) = (x as f32 / self.k - MARGIN, y as f32 / self.k - MARGIN);
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
        let h = self.hit(x, y);
        if h != self.hovered {
            self.hovered = h;
            self.dirty = true;
        }
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        self.hovered = None;
        self.dirty = true;
    }

    /// A press: selects, and a second press soon after on the same icon opens it.
    pub fn on_press(&mut self, x: i32, y: i32) {
        let hit = self.hit(x, y);
        let now = Instant::now();
        let double = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() } as u128;
        if let (Some(i), Some((j, t))) = (hit, self.last_click) {
            if i == j && now.duration_since(t).as_millis() <= double {
                if let Some(item) = self.items.get(i) {
                    crate::shell::open(&item.path);
                }
                self.last_click = None;
                return;
            }
        }
        self.last_click = hit.map(|i| (i, now));
        if hit != self.selected {
            self.selected = hit;
            self.dirty = true;
        }
    }

    /// The icon under (x, y), for its menu.
    pub fn item_at(&mut self, x: i32, y: i32) -> Option<String> {
        let i = self.hit(x, y)?;
        self.selected = Some(i);
        self.dirty = true;
        self.items.get(i).map(|it| it.path.clone())
    }

    pub fn render(&mut self, gfx: &mut Gfx) -> Result<()> {
        self.dirty = false;
        if !shown() || self.items.is_empty() {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return Ok(());
        }
        if self.generation != gfx.generation {
            self.bitmaps.iter_mut().for_each(|b| *b = None);
            self.generation = gfx.generation;
        }
        for (i, item) in self.items.iter().enumerate() {
            if self.bitmaps[i].is_none() && !item.pixels.is_empty() {
                let s = crate::icons::ICON_SIZE;
                self.bitmaps[i] = gfx.bitmap(s, s, &item.pixels).ok();
            }
        }
        let k = self.k;
        let rows = self.rows();
        let cols = self.items.len().div_ceil(rows);
        let (w, h) = (2.0 * MARGIN + cols as f32 * CELL_W, 2.0 * MARGIN + rows.min(self.items.len()) as f32 * CELL_H);
        let (pw, ph) = ((w * k).ceil() as i32, (h * k).ceil() as i32);
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            for (i, item) in self.items.iter().enumerate() {
                let (x, y) = self.cell(i);
                // Faint, but there: the whole cell takes clicks (a fully clear pixel lets them through).
                let bg = if Some(i) == self.selected {
                    crate::theme::accent().alpha(0.35)
                } else if Some(i) == self.hovered {
                    Color::white(0.10)
                } else {
                    Color::white(0.012)
                };
                p.rrect(x + 2.0, y + 2.0, CELL_W - 4.0, CELL_H - 4.0, 6.0, bg);
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
        }
        let pos = POINT { x: self.area.left, y: self.area.top };
        let r = gfx.present(&self.layer, self.hwnd, pos, pw, ph, 255);
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        r
    }
}

// ------------------------------------------------------------ the menu --

thread_local! {
    /// The shell menu on screen, which draws and fills its own submenus (Open with, Send to).
    static MENU: RefCell<Option<IContextMenu2>> = const { RefCell::new(None) };
}

/// Lets the shell menu on screen handle its own submenus' messages. True if it took one.
pub fn menu_message(m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if !matches!(m, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
        return None;
    }
    MENU.with(|cm| {
        let cm = cm.borrow();
        let cm = cm.as_ref()?;
        unsafe {
            if let Ok(cm3) = cm.cast::<IContextMenu3>() {
                let mut r = LRESULT(0);
                cm3.HandleMenuMsg2(m, wp, lp, Some(&mut r)).ok()?;
                return Some(r);
            }
            cm.HandleMenuMsg(m, wp, lp).ok()?;
        }
        Some(LRESULT(0))
    })
}

/// Shows the shell's own menu for the desktop item `path` at the pointer, and does what's picked.
pub fn item_menu(owner: HWND, path: &str) {
    unsafe {
        let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(path), None) else { return };
        let Ok(cm) = item.BindToHandler::<_, IContextMenu>(None, &BHID_SFUIObject) else { return };
        let Ok(menu) = CreatePopupMenu() else { return };
        const FIRST: u32 = 1;
        if cm.QueryContextMenu(menu, 0, FIRST, 0x7FFF, CMF_NORMAL | CMF_CANRENAME).is_err() {
            let _ = DestroyMenu(menu);
            return;
        }
        MENU.with(|m| *m.borrow_mut() = cm.cast::<IContextMenu2>().ok());
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(owner);
        let id = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, p.x, p.y, owner, None);
        MENU.with(|m| *m.borrow_mut() = None);
        let _ = DestroyMenu(menu);
        let id = id.0 as u32;
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
            config::log(&format!("desktop: menu command {} on {path}: {r:?}", id - FIRST));
        }
        msg::post(msg::DESK_ICONS, 0, 0);
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
