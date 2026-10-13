//! Notification-area (tray) icons in Cascade's own taskbar.
//!
//! Apps add tray icons by sending WM_COPYDATA to the first window of class
//! "Shell_TrayWnd". Cascade keeps a hidden window of that class in front of
//! Explorer's, reads the icons from it, and passes every message on to
//! Explorer so its own tray stays complete. Nothing is injected into
//! Explorer, and it all runs on events (no polling).

use std::cell::RefCell;
use std::ffi::c_void;
use std::time::Instant;

use windows::core::{w, GUID, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::backdrop::Backdrop;
use crate::gfx::{Color, Gfx, Layer};
use crate::msg;
use crate::spring::Spring;
use crate::theme;

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETVERSION: u32 = 4;
const NIF_MESSAGE: u32 = 0x1;
const NIF_ICON: u32 = 0x2;
const NIF_TIP: u32 = 0x4;
const NIF_STATE: u32 = 0x8;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 0x1;
const NIN_SELECT: u32 = WM_USER;

/// NOTIFYICONDATA as it travels between processes (32-bit handles).
#[repr(C)]
#[derive(Clone, Copy)]
struct NotifyIconData {
    cb_size: u32,
    hwnd: u32,
    uid: u32,
    flags: u32,
    callback: u32,
    icon: u32,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid: GUID,
    balloon_icon: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TrayData {
    signature: u32,
    message: u32,
    nid: NotifyIconData,
}

/// One tray message, already read on the window procedure.
pub struct Event {
    op: u32,
    hwnd: u32,
    uid: u32,
    guid: Option<GUID>,
    flags: u32,
    callback: u32,
    pixels: Option<(u32, u32, Vec<u8>)>,
    tip: String,
    hidden: Option<bool>,
    version: u32,
}

thread_local! {
    static WIC: RefCell<Option<IWICImagingFactory>> = const { RefCell::new(None) };
}

fn wide(s: &[u16]) -> String {
    let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..n])
}

/// The icon's pixels (premultiplied BGRA), read while the sender waits, so
/// the handle is still valid.
unsafe fn icon_pixels(h: HICON) -> Option<(u32, u32, Vec<u8>)> {
    let wic = WIC.with(|w| {
        let mut w = w.borrow_mut();
        if w.is_none() {
            *w = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok();
        }
        w.clone()
    })?;
    let bmp = wic.CreateBitmapFromHICON(h).ok()?;
    let (mut width, mut height) = (0u32, 0u32);
    bmp.GetSize(&mut width, &mut height).ok()?;
    if width == 0 || height == 0 || width > 256 || height > 256 {
        return None;
    }
    let conv = wic.CreateFormatConverter().ok()?;
    conv.Initialize(&bmp, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
        .ok()?;
    let mut px = vec![0u8; (width * height * 4) as usize];
    conv.CopyPixels(std::ptr::null(), width * 4, &mut px).ok()?;
    Some((width, height, px))
}

unsafe fn parse(cds: &COPYDATASTRUCT) -> Option<Event> {
    let len = (cds.cbData as usize).min(std::mem::size_of::<TrayData>());
    if len < 8 + 16 || cds.lpData.is_null() {
        return None;
    }
    let mut d: TrayData = std::mem::zeroed();
    std::ptr::copy_nonoverlapping(cds.lpData as *const u8, &mut d as *mut TrayData as *mut u8, len);
    let n = d.nid;
    let pixels = if n.flags & NIF_ICON != 0 && n.icon != 0 {
        icon_pixels(HICON(n.icon as usize as *mut c_void))
    } else {
        None
    };
    let hidden = (n.flags & NIF_STATE != 0 && n.state_mask & NIS_HIDDEN != 0).then_some(n.state & NIS_HIDDEN != 0);
    let guid = (n.flags & NIF_GUID != 0 && n.guid != GUID::zeroed()).then_some(n.guid);
    Some(Event {
        op: d.message,
        hwnd: n.hwnd,
        uid: n.uid,
        guid,
        flags: n.flags,
        callback: n.callback,
        pixels,
        tip: wide(&n.tip),
        hidden,
        version: n.version,
    })
}

/// Explorer's own tray window (not ours).
fn explorer_tray() -> Option<HWND> {
    crate::shell::explorer_tray()
}

/// Passes a message on to Explorer's tray and returns its answer.
unsafe fn forward(m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(tray) = explorer_tray() else { return LRESULT(0) };
    let mut result = 0usize;
    let _ = SendMessageTimeoutW(tray, m, wp, lp, SMTO_ABORTIFHUNG | SMTO_NORMAL, 2000, Some(&mut result));
    LRESULT(result as isize)
}

unsafe extern "system" fn tray_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_COPYDATA => {
            let cds = &*(lp.0 as *const COPYDATASTRUCT);
            let answer = forward(m, wp, lp);
            if cds.dwData == 1 {
                // Never touches the app state here: SHAppBarMessage and
                // Shell_NotifyIcon calls from Cascade itself land here too.
                if let Some(e) = parse(cds) {
                    let ptr = Box::into_raw(Box::new(e));
                    if !msg::post(msg::TRAY_ICON, 0, ptr as isize) {
                        drop(Box::from_raw(ptr));
                    }
                }
                return LRESULT(1);
            }
            answer
        }
        // Private messages some apps send the taskbar.
        WM_COMMAND => forward(m, wp, lp),
        m if (WM_USER..0x8000).contains(&m) => forward(m, wp, lp),
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// Creates the hidden tray window and asks apps to re-add their icons.
pub fn start(instance: HINSTANCE) -> Option<HWND> {
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(tray_proc),
            hInstance: instance,
            lpszClassName: w!("Shell_TrayWnd"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("Shell_TrayWnd"),
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )
        .ok()?;
        announce(hwnd);
        Some(hwnd)
    }
}

/// Puts our tray window first and tells every app a taskbar exists, so they
/// add their icons to it. Apps that started before Cascade, or before
/// Explorer finished signing in, only do this when asked again.
pub fn announce(hwnd: HWND) {
    unsafe {
        put_first(hwnd);
        let created = RegisterWindowMessageW(w!("TaskbarCreated"));
        let _ = SendNotifyMessageW(HWND_BROADCAST, created, WPARAM(0), LPARAM(0));
    }
}

pub fn stop(hwnd: HWND) {
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// Keeps our tray window the one apps find first.
pub fn put_first(hwnd: HWND) {
    unsafe {
        if FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).ok() != Some(hwnd) {
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
        }
    }
}

/// Gives the (hidden) tray window the taskbar's rect, for apps and flyouts
/// that place themselves next to the taskbar.
pub fn set_rect(hwnd: HWND, r: RECT) {
    unsafe {
        let flags = SWP_NOZORDER | SWP_NOACTIVATE;
        let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, flags);
    }
}

// ------------------------------------------------------------------ model --

pub struct TrayIcon {
    hwnd: u32,
    uid: u32,
    guid: Option<GUID>,
    callback: u32,
    version: u32,
    pub tip: String,
    hidden: bool,
    /// Shown on the taskbar itself (else under the ^ button), as set in
    /// Windows' taskbar settings.
    pub promoted: bool,
    pixels: Option<(u32, u32, Vec<u8>)>,
    generation: u32,
    bitmaps: Vec<(u32, ID2D1Bitmap)>,
}

impl TrayIcon {
    fn window(&self) -> HWND {
        HWND(self.hwnd as usize as *mut c_void)
    }
}

#[derive(Default)]
pub struct Systray {
    pub hwnd: Option<HWND>,
    pub icons: Vec<TrayIcon>,
    /// From the registry: (exe path, uid, promoted).
    settings: Option<Vec<(String, Option<u32>, bool)>>,
    last_click: Option<(usize, bool, Instant)>,
}

/// Expands a `{known-folder-guid}\rest` path from the registry.
fn expand_path(p: &str) -> String {
    let Some(rest) = p.strip_prefix('{') else { return p.to_string() };
    let Some((id, tail)) = rest.split_once('}') else { return p.to_string() };
    let hex: String = id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let Ok(v) = u128::from_str_radix(&hex, 16) else { return p.to_string() };
    if hex.len() != 32 {
        return p.to_string();
    }
    unsafe {
        let Ok(path) = SHGetKnownFolderPath(&GUID::from_u128(v), KF_FLAG_DEFAULT, None) else { return p.to_string() };
        let base = path.to_string().unwrap_or_default();
        CoTaskMemFree(Some(path.0 as *const c_void));
        format!("{base}{tail}")
    }
}

/// Windows 11's per-icon "show on taskbar" choices. None if there are none
/// (older Windows), in which case every icon shows.
fn read_settings() -> Option<Vec<(String, Option<u32>, bool)>> {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!("Control Panel\\NotifyIconSettings"), Some(0), KEY_READ, &mut key)
            != ERROR_SUCCESS
        {
            return None;
        }
        let mut out = Vec::new();
        let mut name = [0u16; 64];
        for i in 0.. {
            let mut len = name.len() as u32;
            if RegEnumKeyExW(key, i, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None) != ERROR_SUCCESS {
                break;
            }
            let sub = HSTRING::from_wide(&name[..len as usize]);
            let mut buf = [0u16; 520];
            let mut size = (buf.len() * 2) as u32;
            let path = if RegGetValueW(
                key,
                &sub,
                w!("ExecutablePath"),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut c_void),
                Some(&mut size),
            ) == ERROR_SUCCESS
            {
                expand_path(&wide(&buf)).to_lowercase()
            } else {
                continue;
            };
            let dword = |value: PCWSTR| {
                let mut v = 0u32;
                let mut size = 4u32;
                (RegGetValueW(key, &sub, value, RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut c_void), Some(&mut size))
                    == ERROR_SUCCESS)
                    .then_some(v)
            };
            out.push((path, dword(w!("UID")), dword(w!("IsPromoted")) == Some(1)));
        }
        let _ = RegCloseKey(key);
        Some(out)
    }
}

impl Systray {
    fn find(&self, e: &Event) -> Option<usize> {
        self.icons.iter().position(|i| match (e.guid, i.guid) {
            (Some(a), Some(b)) => a == b,
            _ => i.hwnd == e.hwnd && i.uid == e.uid,
        })
    }

    fn promoted(&mut self, hwnd: u32, uid: u32) -> bool {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(HWND(hwnd as usize as *mut c_void), Some(&mut pid)) };
        let Some(exe) = crate::dock::process_path(pid).map(|p| p.to_lowercase()) else { return false };
        let lookup = |list: &Vec<(String, Option<u32>, bool)>| {
            list.iter().find(|(p, u, _)| *p == exe && u.is_none_or(|u| u == uid)).map(|e| e.2)
        };
        if let Some(found) = self.settings.as_ref().and_then(lookup) {
            return found;
        }
        // Not known yet (a new app): read the settings again.
        self.settings = read_settings();
        match &self.settings {
            None => true,
            Some(list) => lookup(list).unwrap_or(false),
        }
    }

    /// Applies a tray message. Returns true if anything visible changed.
    pub fn apply(&mut self, e: Event) -> bool {
        let found = self.find(&e);
        match (e.op, found) {
            (NIM_DELETE, Some(i)) => {
                self.icons.remove(i);
                true
            }
            (NIM_SETVERSION, Some(i)) => {
                self.icons[i].version = e.version;
                false
            }
            (NIM_ADD | NIM_MODIFY, Some(i)) => {
                let icon = &mut self.icons[i];
                if e.flags & NIF_MESSAGE != 0 {
                    icon.callback = e.callback;
                }
                if e.flags & NIF_TIP != 0 {
                    icon.tip = e.tip;
                }
                if let Some(h) = e.hidden {
                    icon.hidden = h;
                }
                if e.flags & NIF_ICON != 0 {
                    icon.pixels = e.pixels;
                    icon.bitmaps.clear();
                }
                if e.guid.is_some() {
                    icon.guid = e.guid;
                }
                true
            }
            (NIM_ADD, None) => {
                let promoted = self.promoted(e.hwnd, e.uid);
                self.icons.push(TrayIcon {
                    hwnd: e.hwnd,
                    uid: e.uid,
                    guid: e.guid,
                    callback: if e.flags & NIF_MESSAGE != 0 { e.callback } else { 0 },
                    version: 0,
                    tip: e.tip,
                    hidden: e.hidden.unwrap_or(false),
                    promoted,
                    pixels: e.pixels,
                    generation: 0,
                    bitmaps: Vec::new(),
                });
                true
            }
            _ => false,
        }
    }

    /// Drops icons whose app has gone away without removing them.
    pub fn prune(&mut self) -> bool {
        let before = self.icons.len();
        self.icons.retain(|i| unsafe { IsWindow(Some(i.window())).as_bool() });
        self.icons.len() != before
    }

    /// Icons shown on the taskbar itself.
    pub fn inline(&self) -> Vec<usize> {
        (0..self.icons.len()).filter(|&i| !self.icons[i].hidden && self.icons[i].promoted).collect()
    }

    /// Icons under the ^ button.
    pub fn overflow(&self) -> Vec<usize> {
        (0..self.icons.len()).filter(|&i| !self.icons[i].hidden && !self.icons[i].promoted).collect()
    }

    /// Icon `i` at `px` pixels square.
    pub fn bitmap(&mut self, i: usize, gfx: &Gfx, px: u32) -> Option<ID2D1Bitmap> {
        let icon = self.icons.get_mut(i)?;
        if icon.generation != gfx.generation {
            icon.bitmaps.clear();
            icon.generation = gfx.generation;
        }
        if let Some((_, b)) = icon.bitmaps.iter().find(|(s, _)| *s == px) {
            return Some(b.clone());
        }
        let (w, h, pixels) = icon.pixels.as_ref()?;
        let (w, h) = (*w as usize, *h as usize);
        let side = px as usize;
        let (dw, dh) = if w >= side && h >= side { (side, side) } else { (w, h) };
        let small = crate::gfx::shrink(pixels, w, h, dw, dh);
        let b = gfx.bitmap(dw as u32, dh as u32, &small).ok()?;
        if icon.bitmaps.len() >= 3 {
            icon.bitmaps.remove(0);
        }
        icon.bitmaps.push((px, b.clone()));
        Some(b)
    }

    /// Tells the app its icon was clicked, the way Explorer does.
    pub fn click(&mut self, i: usize, right: bool) {
        let Some(icon) = self.icons.get(i) else { return };
        let now = Instant::now();
        let double_ms = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() } as u128;
        let double = !right
            && self.last_click.is_some_and(|(j, r, t)| j == i && !r && now.duration_since(t).as_millis() <= double_ms);
        self.last_click = Some((i, right, now));
        let hwnd = icon.window();
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            // Let the app bring its window or menu to the front.
            let _ = AllowSetForegroundWindow(pid);
        }
        let msgs: &[u32] = match (right, double) {
            (true, _) => &[WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU],
            (false, true) => &[WM_LBUTTONDBLCLK, WM_LBUTTONUP],
            (false, false) => &[WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT],
        };
        for &m in msgs {
            let (wp, lp) = if icon.version >= 4 {
                let wp = ((pt.y as u16 as usize) << 16) | pt.x as u16 as usize;
                (WPARAM(wp), LPARAM((((icon.uid & 0xFFFF) as isize) << 16) | m as isize))
            } else {
                if m == WM_CONTEXTMENU || m == NIN_SELECT {
                    continue;
                }
                (WPARAM(icon.uid as usize), LPARAM(m as isize))
            };
            unsafe {
                let _ = PostMessageW(Some(hwnd), icon.callback, wp, lp);
            }
        }
    }
}

// ----------------------------------------------------------------- flyout --

const CELL: f32 = 36.0;
const PAD: f32 = 8.0;
const COLS: usize = 5;

/// The hidden-icons flyout above the ^ button.
pub struct Flyout {
    pub hwnd: HWND,
    layer: Layer,
    backdrop: Backdrop,
    pub open: bool,
    visible: bool,
    reveal: Spring,
    items: Vec<usize>,
    hovered: Option<usize>,
    tracking: bool,
    anchor: (i32, i32),
    k: f32,
    acrylic: bool,
    /// The Tint slider.
    opacity: f32,
    radius: f32,
    pub dirty: bool,
}

impl Flyout {
    pub fn new(hwnd: HWND, backdrop: Backdrop) -> Self {
        Self {
            hwnd,
            layer: Layer::default(),
            backdrop,
            open: false,
            visible: false,
            reveal: Spring::new(0.0, 600.0, 44.0),
            items: Vec::new(),
            hovered: None,
            tracking: false,
            anchor: (0, 0),
            k: 1.0,
            acrylic: true,
            opacity: 0.55,
            radius: 8.0,
            dirty: false,
        }
    }

    pub fn configure(&mut self, acrylic: bool, opacity: f32, radius: f32) {
        self.acrylic = theme::style_backdrop(&self.backdrop, acrylic, opacity, radius);
        self.opacity = opacity.clamp(0.0, 1.0);
        self.radius = radius;
    }

    /// Opens centred on `x`, just above `bottom` (screen px).
    pub fn show(&mut self, items: Vec<usize>, x: i32, bottom: i32, k: f32) {
        self.items = items;
        self.anchor = (x, bottom);
        self.k = k;
        self.open = true;
        self.hovered = None;
        if !self.visible {
            self.visible = true;
            self.reveal.snap(0.0);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOW);
            }
        }
        crate::launcher::force_foreground(self.hwnd);
        self.reveal.set(1.0);
        self.dirty = true;
    }

    pub fn hide(&mut self) {
        crate::mousehook::set_rect(1, None);
        if self.open {
            self.open = false;
            self.reveal.set(0.0);
            self.backdrop.hide();
            self.dirty = true;
        }
    }

    fn size(&self) -> (f32, f32) {
        let n = self.items.len().max(1);
        let cols = n.min(COLS);
        let rows = n.div_ceil(COLS);
        (cols as f32 * CELL + PAD * 2.0, rows as f32 * CELL + PAD * 2.0)
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        let (lx, ly) = (x as f32 / self.k - PAD, y as f32 / self.k - PAD);
        if lx < 0.0 || ly < 0.0 {
            return None;
        }
        let (c, r) = ((lx / CELL) as usize, (ly / CELL) as usize);
        if c >= COLS {
            return None;
        }
        self.items.get(r * COLS + c).copied()
    }

    pub fn hovered(&self) -> Option<usize> {
        self.hovered
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

    pub fn wants_frame(&self) -> bool {
        self.visible && (self.dirty || !self.reveal.settled())
    }

    pub fn step(&mut self, dt: f32) -> bool {
        self.reveal.step(dt)
    }

    pub fn render(&mut self, gfx: &mut Gfx, tray: &mut Systray) -> windows::core::Result<()> {
        self.dirty = false;
        if !self.visible {
            return Ok(());
        }
        if !self.open && self.reveal.value < 0.01 {
            self.visible = false;
            self.layer.release();
            self.backdrop.hide();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return Ok(());
        }
        // Icons can go away while the flyout is open.
        self.items.retain(|&i| i < tray.icons.len());
        let k = self.k;
        let (w, h) = self.size();
        let (pw, ph) = ((w * k).ceil() as i32, (h * k).ceil() as i32);
        let slide = ((1.0 - self.reveal.value.clamp(0.0, 1.0)) * 10.0 * k) as i32;
        let (x, y) = (self.anchor.0 - pw / 2, self.anchor.1 - ph);
        let pos = POINT { x, y: y + slide };
        if self.open {
            crate::mousehook::set_rect(1, Some(RECT { left: x, top: y, right: x + pw, bottom: y + ph }));
        }
        // Glass while it slides in and out too (placed once the frame is out).
        let glass = self.acrylic && self.reveal.value > theme::GLASS_FROM;
        let screen = RECT { left: pos.x, top: pos.y, right: pos.x + pw, bottom: pos.y + ph };
        let px = (16.0 * k).round() as u32;
        let icons: Vec<_> = self.items.iter().map(|&i| tray.bitmap(i, gfx, px)).collect();
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            theme::panel(&p, 0.0, 0.0, w, h, self.radius, glass, self.opacity, 0.97, 1.0);
            for (n, (&i, icon)) in self.items.iter().zip(&icons).enumerate() {
                let (cx, cy) = (PAD + (n % COLS) as f32 * CELL, PAD + (n / COLS) as f32 * CELL);
                if self.hovered == Some(i) {
                    p.rrect(cx + 2.0, cy + 2.0, CELL - 4.0, CELL - 4.0, 5.0, Color::white(0.08));
                }
                match icon {
                    Some(b) => p.icon(b, cx + CELL / 2.0, cy + CELL / 2.0, 1.0),
                    None => p.rrect(cx + 11.0, cy + 11.0, 14.0, 14.0, 3.0, Color::white(0.15)),
                }
            }
        }
        let alpha = (self.reveal.value.clamp(0.0, 1.0) * 255.0) as u8;
        let shown = gfx.present(&self.layer, self.hwnd, pos, pw, ph, alpha);
        if !glass {
            self.backdrop.hold(screen);
        } else if self.backdrop.track(screen, None, self.hwnd) {
            self.dirty = true;
        }
        shown
    }
}
