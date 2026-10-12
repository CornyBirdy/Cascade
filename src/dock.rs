//! The dock: pinned and running apps along the bottom of the screen.
//!
//! Window changes arrive through WinEvent hooks (no polling); a short
//! debounce then rescans the top-level windows. Like the notch, it is a
//! layered window drawn with Direct2D only while something animates.

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use windows::core::{Result, BOOL, BSTR, GUID};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, Config};
use crate::gfx::{Align, Color, Gfx, Layer};
use crate::icons::{IconResult, ICON_SIZE};
use crate::shell;
use crate::spring::Spring;

const PAD: f32 = 9.0;
const GAP: f32 = 7.0;
const MARGIN: f32 = 8.0;
/// Room above the panel for magnified icons and the name label.
const LABEL_ROOM: f32 = 40.0;

/// Pseudo-app for the launcher button at the left end of the dock.
pub const START_ID: &str = "cascade:start";

const PKEY_APP_USER_MODEL_ID: windows::Win32::Foundation::PROPERTYKEY =
    windows::Win32::Foundation::PROPERTYKEY { fmtid: GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3), pid: 5 };

pub struct DockApp {
    /// An .exe path, or `shell:AppsFolder\<AppUserModelID>` for Store apps.
    pub id: String,
    pub pinned: bool,
    /// Top-level windows, front-most first.
    pub windows: Vec<HWND>,
    presence: Spring,
    mag: Spring,
    bounce: Spring,
}

impl DockApp {
    fn new(id: String, pinned: bool) -> Self {
        Self {
            id,
            pinned,
            windows: Vec::new(),
            presence: Spring::new(0.0, 380.0, 30.0),
            mag: Spring::new(0.0, 520.0, 34.0),
            bounce: Spring::new(0.0, 260.0, 9.0),
        }
    }
}

#[derive(Default)]
struct Icon {
    pixels: Option<Vec<u8>>,
    bitmap: Option<(u32, ID2D1Bitmap)>,
}

pub struct Dock {
    pub hwnd: HWND,
    pub trigger: HWND,
    layer: Layer,
    pub apps: Vec<DockApp>,
    icons: HashMap<String, Icon>,
    names: HashMap<String, String>,
    icon_tx: Sender<String>,
    pid_paths: HashMap<u32, String>,

    pub enabled: bool,
    /// Logically on (enabled and nothing fullscreen).
    pub shown: bool,
    /// What the app asked for (false while something is fullscreen).
    wanted: bool,
    /// The window is on screen (auto-hide hides it once faded out).
    window_visible: bool,
    autohide: bool,
    hide_taskbar: bool,
    size: f32,
    magnify: f32,
    pub reveal: Spring,
    reserved: bool,
    reserve_slot: Option<RECT>,

    k: f32,
    monitor: RECT,
    /// Bottom edge of the dock area, in screen pixels.
    bottom: i32,

    /// Cursor x relative to the dock's centre, in layout units.
    hover_rel: Option<f32>,
    pub hovered: Option<usize>,
    pressed: Option<(usize, i32)>,
    dragging: Option<usize>,
    tracking: bool,
    pub foreground: HWND,
    pub dirty: bool,
    /// Last frame's window rect in screen pixels, for anchoring the preview.
    pub screen: RECT,
    win_w: f32,
}

fn pinned_path() -> std::path::PathBuf {
    config::dir().join("pinned.txt")
}

fn load_pinned() -> Vec<String> {
    match std::fs::read_to_string(pinned_path()) {
        Ok(t) => t.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect(),
        Err(_) => {
            let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
            vec![format!("{windir}\\explorer.exe")]
        }
    }
}

unsafe extern "system" fn collect_windows(h: HWND, out: LPARAM) -> BOOL {
    (*(out.0 as *mut Vec<HWND>)).push(h);
    true.into()
}

fn is_cloaked(h: HWND) -> bool {
    let mut cloaked = 0u32;
    unsafe { DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4).is_ok() && cloaked != 0 }
}

/// Whether a window belongs on the dock, the same way the taskbar decides.
fn is_app_window(h: HWND, own_pid: u32) -> bool {
    unsafe {
        if !IsWindowVisible(h).as_bool() {
            return false;
        }
        if GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return false;
        }
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0 {
            return false;
        }
        if GetWindowTextLengthW(h) == 0 || is_cloaked(h) {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == own_pid {
            return false;
        }
        let mut class = [0u16; 64];
        let n = GetClassNameW(h, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..n]);
        !matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
    }
}

fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        ok.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn app_user_model_id(h: HWND) -> Option<String> {
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(h).ok()?;
        let value = store.GetValue(&PKEY_APP_USER_MODEL_ID).ok()?;
        let s = BSTR::try_from(&value).ok()?.to_string();
        (!s.is_empty()).then_some(s)
    }
}

pub fn focus_window(h: HWND) {
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    }
}

impl Dock {
    pub fn new(hwnd: HWND, trigger: HWND, icon_tx: Sender<String>) -> Self {
        let mut d = Self {
            hwnd,
            trigger,
            layer: Layer::default(),
            apps: Vec::new(),
            icons: HashMap::new(),
            names: HashMap::new(),
            icon_tx,
            pid_paths: HashMap::new(),
            enabled: false,
            shown: false,
            wanted: true,
            window_visible: false,
            autohide: false,
            hide_taskbar: false,
            size: 46.0,
            magnify: 0.35,
            reveal: Spring::new(0.0, 300.0, 30.0),
            reserved: false,
            reserve_slot: None,
            k: 1.0,
            monitor: RECT::default(),
            bottom: 0,
            hover_rel: None,
            hovered: None,
            pressed: None,
            dragging: None,
            tracking: false,
            foreground: HWND::default(),
            dirty: true,
            screen: RECT::default(),
            win_w: 0.0,
        };
        for id in load_pinned() {
            let mut app = DockApp::new(id, true);
            app.presence.snap(1.0);
            d.request_icon(&app.id);
            d.apps.push(app);
        }
        d
    }

    pub fn is(&self, h: HWND) -> bool {
        h == self.hwnd || h == self.trigger
    }

    /// Adds or removes the launcher button at the left end.
    pub fn set_start_button(&mut self, on: bool) {
        let has = self.apps.first().is_some_and(|a| a.id == START_ID);
        if on && !has {
            let mut app = DockApp::new(START_ID.to_string(), true);
            app.presence.snap(1.0);
            self.apps.insert(0, app);
        } else if !on && has {
            self.apps.remove(0);
        }
        self.hovered = None;
        self.dirty = true;
    }

    pub fn is_start(&self, i: usize) -> bool {
        self.apps.get(i).is_some_and(|a| a.id == START_ID)
    }

    /// Pinned apps as (target, name), for the launcher.
    pub fn pinned_apps(&self) -> Vec<(String, String)> {
        (0..self.apps.len())
            .filter(|&i| self.apps[i].pinned && !self.is_start(i))
            .map(|i| (self.apps[i].id.clone(), self.name(i)))
            .collect()
    }

    pub fn is_pinned(&self, id: &str) -> bool {
        self.apps.iter().any(|a| a.pinned && a.id.eq_ignore_ascii_case(id))
    }

    /// Pins or unpins an app by id (used by the launcher).
    pub fn set_pinned(&mut self, id: &str, pin: bool) {
        match self.apps.iter().position(|a| a.id.eq_ignore_ascii_case(id)) {
            Some(i) => {
                if self.apps[i].pinned != pin {
                    self.toggle_pin(i);
                }
            }
            None if pin => {
                let mut app = DockApp::new(id.to_string(), true);
                app.presence.set(1.0);
                self.request_icon(id);
                self.apps.push(app);
                self.save_pinned();
                self.dirty = true;
            }
            None => {}
        }
    }

    /// The icon for any app id, loading it in the background if needed.
    pub fn icon(&mut self, id: &str, gfx: &Gfx) -> Option<ID2D1Bitmap> {
        self.request_icon(id);
        let icon = self.icons.get_mut(&id.to_ascii_lowercase())?;
        let stale = icon.bitmap.as_ref().is_none_or(|(g, _)| *g != gfx.generation);
        if stale {
            if let Some(px) = &icon.pixels {
                icon.bitmap = gfx.bitmap(ICON_SIZE, ICON_SIZE, px).ok().map(|b| (gfx.generation, b));
            }
        }
        icon.bitmap.as_ref().map(|(_, b)| b.clone())
    }

    fn request_icon(&mut self, id: &str) {
        if id == START_ID {
            return;
        }
        let key = id.to_ascii_lowercase();
        if let std::collections::hash_map::Entry::Vacant(e) = self.icons.entry(key) {
            e.insert(Icon::default());
            let _ = self.icon_tx.send(id.to_string());
        }
    }

    pub fn on_icon(&mut self, r: IconResult) {
        let key = r.id.to_ascii_lowercase();
        self.names.insert(key.clone(), r.name);
        let icon = self.icons.entry(key).or_default();
        icon.pixels = (!r.pixels.is_empty()).then_some(r.pixels);
        icon.bitmap = None;
        self.dirty = true;
    }

    pub fn name(&self, i: usize) -> String {
        let id = &self.apps[i].id;
        if id == START_ID {
            return "Start".to_string();
        }
        self.names
            .get(&id.to_ascii_lowercase())
            .cloned()
            .unwrap_or_else(|| id.rsplit(['\\', '/']).next().unwrap_or(id).trim_end_matches(".exe").to_string())
    }

    fn save_pinned(&self) {
        let list: Vec<&str> =
            self.apps.iter().filter(|a| a.pinned && a.id != START_ID).map(|a| a.id.as_str()).collect();
        let _ = std::fs::create_dir_all(config::dir());
        let _ = std::fs::write(pinned_path(), list.join("\r\n") + "\r\n");
    }

    // ------------------------------------------------------------ placement --

    /// Applies config and screen geometry, and reserves screen space when the
    /// dock is always shown. Visibility is separate (`set_visible`) so a
    /// fullscreen game never changes the work area of other windows.
    pub fn configure(&mut self, cfg: &Config, monitor: RECT, k: f32) {
        self.enabled = cfg.dock;
        self.autohide = cfg.dock_autohide;
        self.hide_taskbar = cfg.hide_taskbar;
        self.size = cfg.dock_icon_size;
        self.magnify = cfg.dock_magnify;
        self.monitor = monitor;
        self.k = k;

        if self.enabled && !self.autohide {
            let panel_px = ((self.panel_h() + MARGIN * 2.0) * k).round() as i32;
            self.reserve_slot =
                Some(shell::reserve_bottom(self.hwnd, monitor, panel_px, &mut self.reserved, self.reserve_slot));
        } else if self.reserved {
            shell::unregister_appbar(self.hwnd);
            self.reserved = false;
            self.reserve_slot = None;
        }
        self.refresh_bottom();
        self.apply_visible(true);
    }

    /// Recomputes where the dock sits (after work-area changes).
    pub fn refresh_bottom(&mut self) {
        self.bottom = match self.reserve_slot {
            Some(slot) => slot.bottom,
            None if self.hide_taskbar => self.monitor.bottom,
            None => {
                let taskbar = shell::bottom_taskbar_top(self.monitor).unwrap_or(i32::MAX);
                shell::work_area().bottom.min(self.monitor.bottom).min(taskbar)
            }
        };
        self.dirty = true;
    }

    /// Re-applies the app-bar reservation after the shell moved things.
    pub fn reposition(&mut self) {
        if self.reserved {
            let panel_px = ((self.panel_h() + MARGIN * 2.0) * self.k).round() as i32;
            let slot = shell::reserve_bottom(self.hwnd, self.monitor, panel_px, &mut self.reserved, self.reserve_slot);
            self.reserve_slot = Some(slot);
            self.bottom = slot.bottom;
            self.dirty = true;
        }
    }

    /// `visible` is false while a fullscreen app is in front.
    pub fn set_visible(&mut self, visible: bool) {
        self.wanted = visible;
        self.apply_visible(false);
    }

    fn apply_visible(&mut self, force: bool) {
        let visible = self.wanted && self.enabled;
        if visible == self.shown && !force {
            return;
        }
        self.shown = visible;
        unsafe {
            if visible && self.autohide {
                let _ = SetLayeredWindowAttributes(self.trigger, COLORREF(0), 1, LWA_ALPHA);
                let _ = SetWindowPos(
                    self.trigger,
                    Some(HWND_TOPMOST),
                    self.monitor.left,
                    self.bottom - 2,
                    self.monitor.right - self.monitor.left,
                    2,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            } else {
                let _ = ShowWindow(self.trigger, SW_HIDE);
            }
        }
        if visible && !self.autohide {
            self.show_now();
        } else {
            self.reveal.snap(0.0);
            if self.window_visible {
                self.window_visible = false;
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
            }
            self.layer.release();
        }
        self.dirty = true;
    }

    pub fn shutdown(&mut self) {
        if self.reserved {
            shell::unregister_appbar(self.hwnd);
            self.reserved = false;
        }
    }

    /// Brings the dock in (auto-hide, or when it becomes visible).
    pub fn show_now(&mut self) {
        if !self.shown {
            return;
        }
        self.reveal.set(1.0);
        if !self.window_visible {
            self.window_visible = true;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        self.dirty = true;
    }

    /// Slides the dock away if auto-hide is on and nothing holds it open.
    pub fn hide_if_idle(&mut self) -> bool {
        if self.autohide && self.hover_rel.is_none() && self.pressed.is_none() {
            self.reveal.set(0.0);
            return true;
        }
        false
    }

    // ------------------------------------------------------------- windows --

    /// Re-reads the top-level windows and regroups them by app.
    pub fn scan(&mut self) {
        let mut all: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(collect_windows), LPARAM(&mut all as *mut _ as isize));
        }
        let own = unsafe { GetCurrentProcessId() };
        let mut seen_pids = Vec::new();
        for a in &mut self.apps {
            a.windows.clear();
        }
        for h in all {
            if !is_app_window(h, own) {
                continue;
            }
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
            seen_pids.push(pid);
            let path = match self.pid_paths.get(&pid) {
                Some(p) => p.clone(),
                None => {
                    let Some(p) = process_path(pid) else { continue };
                    self.pid_paths.insert(pid, p.clone());
                    p
                }
            };
            let id = if path.to_ascii_lowercase().ends_with("\\applicationframehost.exe") {
                match app_user_model_id(h) {
                    Some(aumid) => format!("shell:AppsFolder\\{aumid}"),
                    None => continue,
                }
            } else {
                path
            };
            match self.apps.iter_mut().find(|a| a.id.eq_ignore_ascii_case(&id)) {
                Some(a) => a.windows.push(h),
                None => {
                    let mut a = DockApp::new(id.clone(), false);
                    a.windows.push(h);
                    self.request_icon(&id);
                    self.apps.push(a);
                }
            }
        }
        self.pid_paths.retain(|pid, _| seen_pids.contains(pid));
        for a in &mut self.apps {
            a.presence.set(if a.pinned || !a.windows.is_empty() { 1.0 } else { 0.0 });
        }
        self.foreground = unsafe { GetForegroundWindow() };
        self.dirty = true;
    }

    pub fn tracks(&self, h: HWND) -> bool {
        self.apps.iter().any(|a| a.windows.contains(&h))
    }

    // ----------------------------------------------------------- geometry --

    fn panel_h(&self) -> f32 {
        self.size + PAD * 2.0 + 4.0
    }

    /// Width taken by app `i` right now (layout units).
    fn slot_w(&self, a: &DockApp) -> f32 {
        (self.size * (1.0 + self.magnify * a.mag.value) + GAP) * a.presence.value.clamp(0.0, 1.0)
    }

    fn panel_w(&self) -> f32 {
        let content: f32 = self.apps.iter().map(|a| self.slot_w(a)).sum();
        (content - GAP).max(self.size * 0.5) + PAD * 2.0
    }

    /// Unmagnified centre of each app, relative to the dock's centre.
    fn base_centres(&self) -> Vec<f32> {
        let slots: Vec<f32> = self.apps.iter().map(|a| (self.size + GAP) * a.presence.target).collect();
        let total: f32 = slots.iter().sum::<f32>() - GAP;
        let mut x = -total / 2.0;
        slots
            .iter()
            .map(|w| {
                let c = x + (w - GAP).max(0.0) / 2.0;
                x += w;
                c
            })
            .collect()
    }

    fn index_at(&self, rel: f32) -> Option<usize> {
        let half = (self.size + GAP) / 2.0;
        self.base_centres()
            .iter()
            .enumerate()
            .filter(|(i, _)| self.apps[*i].presence.target > 0.5)
            .find(|(_, c)| (rel - **c).abs() <= half)
            .map(|(i, _)| i)
    }

    // -------------------------------------------------------------- input --

    pub fn on_move(&mut self, x: i32, y: i32) -> bool {
        unsafe {
            if !self.tracking {
                let mut t = windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT>() as u32,
                    dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                self.tracking = windows::Win32::UI::Input::KeyboardAndMouse::TrackMouseEvent(&mut t).is_ok();
            }
        }
        let _ = y;
        let rel = x as f32 / self.k - self.win_w / 2.0;
        self.hover_rel = Some(rel);
        if let Some((i, start)) = self.pressed {
            if self.dragging.is_none()
                && (x - start).abs() > (6.0 * self.k) as i32
                && self.apps[i].pinned
                && !self.is_start(i)
            {
                self.dragging = Some(i);
            }
        }
        if let Some(from) = self.dragging {
            if let Some(to) = self.index_at(rel) {
                if to != from && self.apps[to].pinned && !self.is_start(to) {
                    let app = self.apps.remove(from);
                    self.apps.insert(to, app);
                    self.dragging = Some(to);
                    self.pressed = Some((to, self.pressed.map_or(x, |p| p.1)));
                }
            }
        }
        let hovered = if self.dragging.is_some() { self.dragging } else { self.index_at(rel) };
        let changed = hovered != self.hovered;
        self.hovered = hovered;
        self.dirty = true;
        changed
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        if self.pressed.is_none() {
            self.hover_rel = None;
            self.hovered = None;
        }
        self.dirty = true;
    }

    pub fn on_down(&mut self, x: i32) {
        if let Some(i) = self.hovered {
            self.pressed = Some((i, x));
            unsafe {
                windows::Win32::UI::Input::KeyboardAndMouse::SetCapture(self.hwnd);
            }
        }
    }

    /// Mouse released. Returns the app index for a plain click.
    pub fn on_up(&mut self) -> Option<usize> {
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
        }
        let pressed = self.pressed.take();
        if self.dragging.take().is_some() {
            self.save_pinned();
            return None;
        }
        match (pressed, self.hovered) {
            (Some((p, _)), Some(h)) if p == h => Some(p),
            _ => None,
        }
    }

    /// Click on an app: launch it, focus it, minimise it or cycle its windows.
    pub fn activate(&mut self, i: usize) {
        let Some(app) = self.apps.get_mut(i) else { return };
        if app.windows.is_empty() {
            shell::open(&app.id);
            app.bounce.kick(14.0);
            self.dirty = true;
            return;
        }
        let fg = unsafe { GetForegroundWindow() };
        unsafe {
            match app.windows.iter().position(|&w| w == fg) {
                Some(_) if app.windows.len() == 1 && !IsIconic(fg).as_bool() => {
                    let _ = ShowWindow(fg, SW_MINIMIZE);
                }
                Some(pos) => focus_window(app.windows[(pos + 1) % app.windows.len()]),
                None => focus_window(app.windows[0]),
            }
        }
    }

    pub fn launch_new(&mut self, i: usize) {
        if let Some(app) = self.apps.get_mut(i) {
            shell::open(&app.id);
            app.bounce.kick(14.0);
            self.dirty = true;
        }
    }

    pub fn toggle_pin(&mut self, i: usize) {
        if let Some(app) = self.apps.get_mut(i) {
            app.pinned = !app.pinned;
            let keep = app.pinned || !app.windows.is_empty();
            app.presence.set(if keep { 1.0 } else { 0.0 });
            self.save_pinned();
            self.dirty = true;
        }
    }

    pub fn close_all(&self, i: usize) {
        if let Some(app) = self.apps.get(i) {
            for &w in &app.windows {
                unsafe {
                    let _ = PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
    }

    // ------------------------------------------------------------- frames --

    pub fn wants_frame(&self) -> bool {
        self.shown
            && self.window_visible
            && (self.dirty
                || !self.reveal.settled()
                || self.apps.iter().any(|a| !a.presence.settled() || !a.mag.settled() || !a.bounce.settled()))
    }

    /// Steps springs; returns true while the size or position is changing.
    pub fn step(&mut self, dt: f32) -> bool {
        let centres = self.base_centres();
        let mut moving = self.reveal.step(dt);
        for (i, a) in self.apps.iter_mut().enumerate() {
            let target = match (self.hover_rel, self.magnify > 0.0) {
                (Some(h), true) => {
                    let d = (h - centres[i]).abs() / (self.size * 2.2);
                    if d < 1.0 {
                        (1.0 + (std::f32::consts::PI * d).cos()) / 2.0
                    } else {
                        0.0
                    }
                }
                _ => 0.0,
            };
            a.mag.set(target);
            moving |= a.presence.step(dt);
            moving |= a.mag.step(dt);
            moving |= a.bounce.step(dt);
        }
        // Drop apps that finished sliding out.
        let before = self.apps.len();
        self.apps.retain(|a| a.presence.target > 0.5 || a.presence.value > 0.01);
        if self.apps.len() != before {
            self.hovered = self.hover_rel.and_then(|r| self.index_at(r));
        }
        if self.shown && self.autohide && self.reveal.target == 0.0 && self.reveal.value < 0.01 {
            self.reveal.snap(0.0);
        }
        moving
    }

    pub fn render(&mut self, gfx: &mut Gfx) -> Result<()> {
        self.dirty = false;
        if !self.shown || !self.window_visible {
            return Ok(());
        }
        // Upload icon bitmaps that arrived (or were lost with a new target).
        for a in &self.apps {
            if let Some(icon) = self.icons.get_mut(&a.id.to_ascii_lowercase()) {
                let stale = icon.bitmap.as_ref().is_none_or(|(g, _)| *g != gfx.generation);
                if stale {
                    if let Some(px) = &icon.pixels {
                        icon.bitmap = gfx.bitmap(ICON_SIZE, ICON_SIZE, px).ok().map(|b| (gfx.generation, b));
                    }
                }
            }
        }

        let k = self.k;
        let s = self.size;
        let panel_w = self.panel_w();
        let panel_h = self.panel_h();
        let win_w = panel_w + 260.0;
        let win_h = LABEL_ROOM + s * self.magnify + panel_h + MARGIN;
        self.win_w = win_w;
        let mon_w = self.monitor.right - self.monitor.left;
        let mut pw = (win_w * k).ceil() as i32;
        if (mon_w - pw) % 2 != 0 {
            pw += 1;
        }
        let ph = (win_h * k).ceil() as i32;
        // Auto-hide fades the dock while it sinks a little.
        let hidden_px = (18.0 * k * (1.0 - self.reveal.value.clamp(0.0, 1.0))) as i32;
        let pos = POINT { x: self.monitor.left + (mon_w - pw) / 2, y: self.bottom - ph + hidden_px };
        self.screen = RECT { left: pos.x, top: pos.y, right: pos.x + pw, bottom: pos.y + ph };
        let ww = pw as f32 / k;

        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let panel_x = (ww - panel_w) / 2.0;
            let panel_y = win_h - MARGIN - panel_h;
            p.rrect(panel_x, panel_y, panel_w, panel_h, 18.0, Color(0.10, 0.10, 0.11, 0.86));
            p.stroke_rrect(panel_x + 0.5, panel_y + 0.5, panel_w - 1.0, panel_h - 1.0, 17.5, Color::white(0.10), 1.0);

            let mut x = panel_x + PAD;
            let base_y = panel_y + PAD + s;
            let mut label: Option<(f32, f32, String)> = None;
            for (i, a) in self.apps.iter().enumerate() {
                let presence = a.presence.value.clamp(0.0, 1.0);
                let size = s * (1.0 + self.magnify * a.mag.value) * presence;
                let cx = x + size / 2.0;
                let y = base_y - size - a.bounce.value.max(0.0) * s * 0.35;
                let alpha = presence;
                match self.icons.get(&a.id.to_ascii_lowercase()).and_then(|ic| ic.bitmap.as_ref()) {
                    _ if a.id == START_ID => {
                        // Launcher button: a soft tile with four squares.
                        let hovered = self.hovered == Some(i);
                        p.rrect(
                            x + size * 0.06,
                            y + size * 0.06,
                            size * 0.88,
                            size * 0.88,
                            size * 0.24,
                            Color(0.27, 0.55, 1.0, if hovered { 0.95 } else { 0.8 } * alpha),
                        );
                        let q = size * 0.17;
                        let g = size * 0.05;
                        let (cx0, cy0) = (x + size / 2.0, y + size / 2.0);
                        for (left, top) in [(true, true), (false, true), (true, false), (false, false)] {
                            let qx = if left { cx0 - q - g / 2.0 } else { cx0 + g / 2.0 };
                            let qy = if top { cy0 - q - g / 2.0 } else { cy0 + g / 2.0 };
                            p.rrect(qx, qy, q, q, q * 0.25, Color::white(0.95 * alpha));
                        }
                    }
                    Some((_, bmp)) => p.image(bmp, x, y, size, size, alpha),
                    None => p.rrect(
                        x + size * 0.1,
                        y + size * 0.1,
                        size * 0.8,
                        size * 0.8,
                        size * 0.2,
                        Color::white(0.15 * alpha),
                    ),
                }
                if !a.windows.is_empty() {
                    let active = a.windows.contains(&self.foreground);
                    let (w, c) = if active { (10.0, Color::white(0.95)) } else { (4.0, Color::white(0.55)) };
                    p.rrect(cx - w / 2.0, panel_y + panel_h - 6.5, w, 4.0, 2.0, c.alpha(presence));
                }
                if self.hovered == Some(i) && self.dragging.is_none() && presence > 0.9 {
                    label = Some((cx, y, self.name(i)));
                }
                x += self.slot_w(a);
            }
            if let Some((cx, top, name)) = label {
                let tw = gfx.measure(&name, &gfx.fonts.label).min(ww - 20.0);
                let (lw, lh) = (tw + 20.0, 24.0);
                let lx = (cx - lw / 2.0).clamp(4.0, ww - lw - 4.0);
                let ly = top - lh - 6.0;
                p.rrect(lx, ly, lw, lh, 8.0, Color(0.10, 0.10, 0.11, 0.92));
                p.text(&name, &gfx.fonts.label, lx, ly, lw, lh, Align::Center, Color::white(0.95));
            }
        }
        let alpha = (self.reveal.value.clamp(0.0, 1.0) * 255.0) as u8;
        gfx.present(&self.layer, self.hwnd, pos, pw, ph, alpha)?;
        if self.autohide && self.reveal.target == 0.0 && self.reveal.value < 0.01 && self.window_visible {
            // Fully faded out: stop covering anything.
            self.window_visible = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
        Ok(())
    }

    /// Screen x of app `i`'s centre (for anchoring the preview).
    pub fn app_screen_x(&self, i: usize) -> i32 {
        let centres = self.base_centres();
        let c = centres.get(i).copied().unwrap_or(0.0);
        (self.screen.left + self.screen.right) / 2 + (c * self.k) as i32
    }

    pub fn panel_top(&self) -> i32 {
        self.screen.bottom - ((MARGIN + self.panel_h()) * self.k) as i32
    }
}
