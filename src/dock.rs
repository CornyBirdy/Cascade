//! The taskbar: Start, search, pinned + running apps, weather, status icons
//! and the clock, drawn Windows 11 style over an acrylic blur.
//!
//! Window changes arrive through WinEvent hooks (no polling); a short
//! debounce then rescans the top-level windows. Like the notch, it is a
//! layered window drawn with Direct2D only while something changes.

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use windows::core::{Result, BOOL, BSTR, GUID, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, DATE_SHORTDATE, TIME_NOSECONDS};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::backdrop::Backdrop;
use crate::config::{self, Config};
use crate::gfx::{Align, Color, Gfx, Layer, Painter};
use crate::icons::{IconResult, ICON_SIZE};
use crate::media::MediaInfo;
use crate::sysmon::{self, Stats};
use crate::shell;
use crate::spring::Spring;
use crate::systray::Systray;
use crate::theme;
use crate::weather::Weather;

/// Room above the bar for tooltips (transparent, click-through).
const TIP_ROOM: f32 = 40.0;
const GAP: f32 = 4.0;
const SEARCH_W: f32 = 112.0;
const WEATHER_W: f32 = 106.0;
const CLOCK_W: f32 = 78.0;
const TRAY_W: f32 = 28.0;
const TRAY_ICON_W: f32 = 28.0;
const STATUS_ICON: f32 = 24.0;
/// Space between floating islands.
const ISLAND_GAP: f32 = 8.0;
const CHIP_STAT_W: f32 = 48.0;
const CHIP_NET_W: f32 = 84.0;
const CHIP_MUSIC_W: f32 = 196.0;
const WIDGETS_BTN_W: f32 = 28.0;
/// Space between widgets on the same side.
const ITEM_GAP: f32 = 2.0;

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
    generation: u32,
    /// Bitmaps already shrunk to the pixel sizes in use: (px, bitmap).
    bitmaps: Vec<(u32, ID2D1Bitmap)>,
}

impl Icon {
    /// The icon at exactly `px` pixels square (made on first use).
    fn at(&mut self, gfx: &Gfx, px: u32) -> Option<ID2D1Bitmap> {
        let px = px.clamp(8, ICON_SIZE);
        if self.generation != gfx.generation {
            self.bitmaps.clear();
            self.generation = gfx.generation;
        }
        if let Some((_, b)) = self.bitmaps.iter().find(|(s, _)| *s == px) {
            return Some(b.clone());
        }
        let n = ICON_SIZE as usize;
        let small = crate::gfx::shrink(self.pixels.as_ref()?, n, n, px as usize, px as usize);
        let b = gfx.bitmap(px, px, &small).ok()?;
        if self.bitmaps.len() >= 24 {
            self.bitmaps.remove(0);
        }
        self.bitmaps.push((px, b.clone()));
        Some(b)
    }
}

/// Small live widgets on the taskbar.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Chip {
    Weather,
    Music,
    Cpu,
    Ram,
    Gpu,
    Net,
    /// The button that opens the widgets panel.
    Widgets,
}

impl Chip {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "weather" => Chip::Weather,
            "music" => Chip::Music,
            "cpu" => Chip::Cpu,
            "ram" | "memory" => Chip::Ram,
            "gpu" => Chip::Gpu,
            "net" | "network" => Chip::Net,
            "widgets" => Chip::Widgets,
            _ => return None,
        })
    }
}

/// "weather, cpu" -> the widgets, in that order (unknown names and repeats ignored).
fn parse_chips(list: &str) -> Vec<Chip> {
    let mut out = Vec::new();
    for name in list.split(',') {
        if let Some(c) = Chip::parse(name) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
}

/// What the taskbar widgets show.
pub struct WidgetData<'a> {
    pub stats: &'a Stats,
    pub media: Option<&'a MediaInfo>,
    pub art: Option<ID2D1Bitmap>,
}

/// Things on the taskbar you can point at.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hot {
    Start,
    Search,
    Weather,
    /// The button that opens the widgets panel.
    Widgets,
    /// A small widget (opens the panel).
    Chip(Chip),
    /// The play/pause button on the music widget.
    MusicToggle,
    App(usize),
    Tray,
    /// A notification-area icon (index into the systray's icons).
    TrayIcon(usize),
    Status,
    Clock,
}

/// Live system state shown on the right of the taskbar.
#[derive(Clone, Copy, Default)]
pub struct Status {
    pub wifi: Option<bool>,
    pub volume: f32,
    pub muted: bool,
    pub battery: Option<(u8, bool)>,
}

#[derive(Clone, Copy, Default)]
struct Span {
    x: f32,
    w: f32,
}

impl Span {
    fn contains(&self, x: f32) -> bool {
        x >= self.x && x < self.x + self.w
    }
    fn centre(&self) -> f32 {
        self.x + self.w / 2.0
    }
}

/// Last frame's layout, in layout units relative to the window.
#[derive(Default)]
struct Geo {
    y0: f32,
    y1: f32,
    start: Span,
    search: Option<Span>,
    apps: Vec<Span>,
    /// The ^ button, when some icons are hidden under it.
    tray: Option<Span>,
    tray_icons: Vec<(usize, Span)>,
    /// Widgets on the left and right sides, in screen order.
    left: Vec<(Chip, Span)>,
    right: Vec<(Chip, Span)>,
    status: Span,
    clock: Span,
    /// Rounded panels (x0, x1) that get a blur behind them.
    islands: Vec<(f32, f32)>,
}

pub struct Dock {
    pub hwnd: HWND,
    pub trigger: HWND,
    layer: Layer,
    backdrops: Vec<Backdrop>,
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
    /// Start sits alone on its own island at the far left.
    start_left: bool,
    /// The window is on screen (auto-hide hides it once faded out).
    window_visible: bool,
    autohide: bool,
    hide_taskbar: bool,
    pub reveal: Spring,
    reserved: bool,
    reserve_slot: Option<RECT>,

    // Appearance.
    full: bool,
    left: bool,
    bar_h: f32,
    margin: f32,
    padding: f32,
    tile: f32,
    radius: f32,
    icon: f32,
    magnify: f32,
    acrylic: bool,
    opacity: f32,
    show_search: bool,
    show_status: bool,
    show_clock: bool,
    show_weather: bool,
    show_tray: bool,
    left_cfg: Vec<Chip>,
    right_cfg: Vec<Chip>,

    k: f32,
    monitor: RECT,
    /// Bottom edge of the taskbar area, in screen pixels.
    bottom: i32,

    geo: Geo,
    hover_x: Option<f32>,
    pub hovered: Option<Hot>,
    pressed: Option<(Hot, i32)>,
    dragging: Option<usize>,
    tracking: bool,
    /// A pop-up from the taskbar (Start menu, tray flyout) is open, so
    /// auto-hide keeps it in view.
    held: bool,
    pub foreground: HWND,
    pub dirty: bool,
    /// Last frame's window rect in screen pixels.
    pub screen: RECT,
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

/// Whether a window belongs on the taskbar, the same way Windows decides.
pub fn is_app_window(h: HWND, own_pid: u32) -> bool {
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
        let mut class = [0u16; 64];
        let n = GetClassNameW(h, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..n]);
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        // Cascade's own settings window is a normal app window.
        if pid == own_pid && class != "CascadeSettings" {
            return false;
        }
        !matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
    }
}

pub fn process_path(pid: u32) -> Option<String> {
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

/// The process behind window `h`. A Store app's frame belongs to
/// ApplicationFrameHost, which hosts every Store app's frame, so that one
/// answers with the app inside it instead.
fn app_pid(h: HWND) -> Option<u32> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    if !process_path(pid).is_some_and(|p| p.to_ascii_lowercase().ends_with("\\applicationframehost.exe")) {
        return Some(pid);
    }
    let mut found: (u32, u32) = (pid, 0);
    unsafe extern "system" fn child(c: HWND, lp: LPARAM) -> BOOL {
        let found = &mut *(lp.0 as *mut (u32, u32));
        let mut pid = 0u32;
        GetWindowThreadProcessId(c, Some(&mut pid));
        if pid != 0 && pid != found.0 {
            found.1 = pid;
            return BOOL(0);
        }
        BOOL(1)
    }
    unsafe {
        let _ = EnumChildWindows(Some(h), Some(child), LPARAM(&mut found as *mut _ as isize));
    }
    (found.1 != 0).then_some(found.1)
}

/// The id the dock uses for the app that owns `h`: the exe path, or
/// `shell:AppsFolder\<AppUserModelID>` for Store apps.
pub fn window_app_id(h: HWND) -> Option<String> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    let path = process_path(pid)?;
    if path.to_ascii_lowercase().ends_with("\\applicationframehost.exe") {
        app_user_model_id(h).map(|a| format!("shell:AppsFolder\\{a}"))
    } else {
        Some(path)
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
        // Windows won't let us bring an admin app to the front (or restore
        // it); the admin helper, if it's on, can.
        if GetForegroundWindow() != h || IsIconic(h).as_bool() {
            crate::helper::request_focus(h);
        }
    }
}

fn clock_text() -> (String, String) {
    let mut t = [0u16; 64];
    let mut d = [0u16; 64];
    unsafe {
        let nt = GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, None, PCWSTR::null(), Some(&mut t));
        let nd = GetDateFormatEx(PCWSTR::null(), DATE_SHORTDATE, None, PCWSTR::null(), Some(&mut d), PCWSTR::null());
        let s = |b: &[u16], n: i32| String::from_utf16_lossy(&b[..(n.max(1) - 1) as usize]);
        (s(&t, nt), s(&d, nd))
    }
}

/// The Windows 11 logo: four equal, flat, square panes with a thin cross
/// between them, in the blue that fades from light (top left) to deep (bottom
/// right). `size` is the width of the whole mark.
pub fn start_logo(p: &Painter, cx: f32, cy: f32, size: f32, alpha: f32) {
    let g = size * 0.055;
    let q = (size - g) / 2.0;
    let (light, deep) = ([0.40, 0.78, 1.0], [0.0, 0.42, 0.88]);
    const STRIPS: usize = 8;
    for (left, top) in [(true, true), (false, true), (true, false), (false, false)] {
        let x = if left { cx - g / 2.0 - q } else { cx + g / 2.0 };
        let y = if top { cy - g / 2.0 - q } else { cy + g / 2.0 };
        // The fade runs diagonally; each pane is a few strips of it.
        let u = if left { 0.0 } else { 1.0 };
        for i in 0..STRIPS {
            let v = (if top { 0.0 } else { 1.0 }) + (i as f32 + 0.5) / STRIPS as f32;
            let t = ((u + v / 2.0) / 2.0).clamp(0.0, 1.0);
            let c: [f32; 3] = std::array::from_fn(|k| light[k] + (deep[k] - light[k]) * t);
            let (sy, sh) = (y + q * i as f32 / STRIPS as f32, q / STRIPS as f32 + 0.5);
            p.rrect(x, sy, q, sh.min(y + q - sy), 0.0, Color(c[0], c[1], c[2], alpha));
        }
    }
}

/// How wide a widget is, or None if it has nothing to show right now.
fn chip_width(c: Chip, has_media: bool, has_weather: bool) -> Option<f32> {
    match c {
        Chip::Weather => has_weather.then_some(WEATHER_W),
        Chip::Music => has_media.then_some(CHIP_MUSIC_W),
        Chip::Net => Some(CHIP_NET_W),
        Chip::Widgets => Some(WIDGETS_BTN_W),
        Chip::Cpu | Chip::Ram | Chip::Gpu => Some(CHIP_STAT_W),
    }
}

impl Dock {
    pub fn new(hwnd: HWND, trigger: HWND, icon_tx: Sender<String>, backdrops: Vec<Backdrop>) -> Self {
        let mut d = Self {
            hwnd,
            trigger,
            layer: Layer::default(),
            backdrops,
            apps: Vec::new(),
            icons: HashMap::new(),
            names: HashMap::new(),
            icon_tx,
            pid_paths: HashMap::new(),
            enabled: false,
            shown: false,
            wanted: true,
            start_left: false,
            window_visible: false,
            autohide: false,
            hide_taskbar: false,
            reveal: Spring::new(0.0, 300.0, 30.0),
            reserved: false,
            reserve_slot: None,
            full: true,
            left: false,
            bar_h: 48.0,
            margin: 8.0,
            padding: 6.0,
            tile: 6.0,
            radius: 8.0,
            icon: 24.0,
            magnify: 0.0,
            acrylic: true,
            opacity: 0.55,
            show_search: true,
            show_status: true,
            show_clock: true,
            show_weather: true,
            show_tray: true,
            left_cfg: Vec::new(),
            right_cfg: Vec::new(),
            k: 1.0,
            monitor: RECT::default(),
            bottom: 0,
            geo: Geo::default(),
            hover_x: None,
            hovered: None,
            pressed: None,
            dragging: None,
            tracking: false,
            held: false,
            foreground: HWND::default(),
            dirty: true,
            screen: RECT::default(),
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

    pub fn hovered_app(&self) -> Option<usize> {
        match self.hovered {
            Some(Hot::App(i)) if i < self.apps.len() => Some(i),
            _ => None,
        }
    }

    /// Pinned apps as (target, name), for the launcher.
    pub fn pinned_apps(&self) -> Vec<(String, String)> {
        (0..self.apps.len()).filter(|&i| self.apps[i].pinned).map(|i| (self.apps[i].id.clone(), self.name(i))).collect()
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

    /// The icon for any app id or file at `px` pixels, loading it in the
    /// background if needed.
    pub fn icon(&mut self, id: &str, gfx: &Gfx, px: u32) -> Option<ID2D1Bitmap> {
        self.request_icon(id);
        self.icons.get_mut(&id.to_ascii_lowercase())?.at(gfx, px)
    }

    fn request_icon(&mut self, id: &str) {
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
        icon.bitmaps.clear();
        self.dirty = true;
    }

    pub fn name(&self, i: usize) -> String {
        let id = &self.apps[i].id;
        self.names
            .get(&id.to_ascii_lowercase())
            .cloned()
            .unwrap_or_else(|| id.rsplit(['\\', '/']).next().unwrap_or(id).trim_end_matches(".exe").to_string())
    }

    fn save_pinned(&self) {
        let list: Vec<&str> = self.apps.iter().filter(|a| a.pinned).map(|a| a.id.as_str()).collect();
        let _ = std::fs::create_dir_all(config::dir());
        let _ = std::fs::write(pinned_path(), list.join("\r\n") + "\r\n");
    }

    // ------------------------------------------------------------ placement --

    /// Applies config and screen geometry, and reserves screen space when the
    /// taskbar is always shown. Visibility is separate (`set_visible`) so a
    /// fullscreen game never changes the work area of other windows.
    pub fn configure(&mut self, cfg: &Config, monitor: RECT, k: f32) {
        self.enabled = cfg.dock;
        self.autohide = cfg.dock_autohide;
        self.hide_taskbar = cfg.hide_taskbar;
        self.full = cfg.taskbar_style != "floating";
        self.left = cfg.taskbar_align == "left";
        self.start_left = cfg.taskbar_start == "left";
        self.margin = cfg.taskbar_gap;
        self.padding = cfg.taskbar_pad.clamp(0.0, 24.0);
        self.tile = cfg.taskbar_tile.clamp(0.0, 16.0);
        self.icon = cfg.dock_icon_size.clamp(16.0, 48.0);
        // Icon, its highlight margin, then the same padding all round.
        self.bar_h = (self.icon + 2.0 * self.tile + 2.0 * self.padding).max(28.0);
        // A full-width bar flush with the screen edges has square corners.
        self.radius = if self.full && self.margin < 1.0 { 0.0 } else { cfg.corner_radius() };
        self.magnify = cfg.dock_magnify;
        self.acrylic = theme::blurs(cfg.acrylic);
        self.opacity = cfg.taskbar_opacity;
        self.show_search = cfg.show_search;
        self.show_status = cfg.show_status;
        self.show_clock = cfg.show_clock;
        self.show_weather = cfg.weather;
        self.show_tray = cfg.tray_icons;
        self.left_cfg = parse_chips(&cfg.taskbar_left);
        self.right_cfg = parse_chips(&cfg.taskbar_right);
        self.monitor = monitor;
        self.k = k;
        for b in &self.backdrops {
            theme::style_backdrop(b, cfg.acrylic, self.opacity, self.radius);
        }

        if self.enabled && !self.autohide {
            let px = self.reserve_px(k);
            self.reserve_slot = Some(shell::reserve_bottom(self.hwnd, monitor, px, &mut self.reserved, self.reserve_slot));
        } else if self.reserved {
            shell::unregister_appbar(self.hwnd);
            self.reserved = false;
            self.reserve_slot = None;
        }
        self.refresh_bottom();
        self.apply_visible(true);
    }

    /// Screen strip kept free for the bar: the bar, the gap under it and the
    /// same gap above it, so windows stop as far above the bar as the bar
    /// sits above the screen edge.
    fn reserve_px(&self, k: f32) -> i32 {
        ((self.bar_h + 2.0 * self.margin) * k).round() as i32
    }

    /// Recomputes where the taskbar sits (after work-area changes).
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
            let px = self.reserve_px(self.k);
            let slot = shell::reserve_bottom(self.hwnd, self.monitor, px, &mut self.reserved, self.reserve_slot);
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
            self.hide_window();
        }
        self.dirty = true;
    }

    fn hide_window(&mut self) {
        for b in &mut self.backdrops {
            b.hide();
        }
        if self.window_visible {
            self.window_visible = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
        self.layer.release();
    }

    pub fn shutdown(&mut self) {
        if self.reserved {
            shell::unregister_appbar(self.hwnd);
            self.reserved = false;
        }
        for b in &mut self.backdrops {
            b.hide();
        }
    }

    /// Brings the taskbar in (auto-hide, or when it becomes visible).
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

    /// Keeps the taskbar in view while a pop-up is open. Returns true when
    /// the last one closed and auto-hide should be armed again.
    pub fn set_held(&mut self, held: bool) -> bool {
        let was = self.held;
        self.held = held;
        if held {
            self.show_now();
        }
        was && !held && self.autohide
    }

    /// Slides the taskbar away if auto-hide is on and nothing holds it open.
    pub fn hide_if_idle(&mut self) -> bool {
        if self.autohide && self.hover_x.is_none() && self.pressed.is_none() && !self.held {
            self.reveal.set(0.0);
            for b in &mut self.backdrops {
                b.hide();
            }
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

    /// Size of a square button: the icon plus its highlight margin.
    fn button(&self) -> f32 {
        (self.icon + 2.0 * self.tile).min(self.bar_h)
    }

    /// Space between a bar's end and the first or last button: the same as
    /// above and below the buttons.
    fn edge(&self) -> f32 {
        self.padding
    }

    fn status_icons(&self, status: &Status) -> usize {
        2 + status.battery.is_some() as usize
    }

    /// Lays everything out for a window `ww` layout units wide. The same
    /// padding is used between every island edge and what's inside it.
    fn layout(
        &self,
        ww: f32,
        wh: f32,
        status: &Status,
        tray: &[usize],
        overflow: bool,
        has_media: bool,
        has_weather: bool,
    ) -> Geo {
        let b = self.button();
        let pad = self.edge();
        let m = self.margin;
        let y1 = wh - m;
        let y0 = y1 - self.bar_h;
        // Start alone at the far left gets its own island; everything else
        // begins after it.
        let start_w = b + 2.0 * pad;
        let (x0, x1) = (if self.start_left { m + start_w + ISLAND_GAP } else { m }, ww - m);

        // Centre group: Start, search, apps.
        let slots: Vec<f32> = self.apps.iter().map(|a| (b + GAP) * a.presence.value.clamp(0.0, 1.0)).collect();
        let search_w = if self.show_search { SEARCH_W + GAP } else { 0.0 };
        let lead = if self.start_left { 0.0 } else { b + GAP };
        let group_w = (lead + search_w + slots.iter().sum::<f32>() - GAP).max(0.0);

        // Left side widgets, from the bar's left end.
        let mut left = Vec::new();
        let mut lx = x0 + pad;
        for &c in &self.left_cfg {
            if let Some(w) = chip_width(c, has_media, has_weather) {
                left.push((c, Span { x: lx, w }));
                lx += w + ITEM_GAP;
            }
        }
        let left_end = if left.is_empty() { x0 } else { lx - ITEM_GAP + pad };
        let gx = match (self.left, left.is_empty(), self.full) {
            (false, ..) => (ww - group_w) / 2.0,
            (true, true, _) => x0 + pad,
            (true, false, true) => left_end + 16.0,
            (true, false, false) => left_end + ISLAND_GAP + pad,
        };

        let start = if self.start_left { Span { x: m + pad, w: b } } else { Span { x: gx, w: b } };
        let mut x = gx + lead;
        let search = self.show_search.then(|| {
            let s = Span { x, w: SEARCH_W };
            x += SEARCH_W + GAP;
            s
        });
        let mut apps = Vec::new();
        for w in &slots {
            apps.push(Span { x, w: (w - GAP).max(0.0) });
            x += w;
        }

        // Right, from the edge in: clock, status, tray icons, ^.
        let mut rx = x1 - pad;
        let clock = if self.show_clock { Span { x: rx - CLOCK_W, w: CLOCK_W } } else { Span { x: rx, w: 0.0 } };
        rx = clock.x;
        let status_w = if self.show_status { STATUS_ICON * self.status_icons(status) as f32 + 12.0 } else { 0.0 };
        let status_span = Span { x: rx - status_w, w: status_w };
        rx = status_span.x;
        let mut tray_icons = Vec::new();
        for &i in tray.iter().rev() {
            rx -= TRAY_ICON_W;
            tray_icons.push((i, Span { x: rx, w: TRAY_ICON_W }));
        }
        tray_icons.reverse();
        let tray_span = overflow.then(|| {
            rx -= TRAY_W;
            Span { x: rx, w: TRAY_W }
        });
        // Right side widgets, ending at the tray.
        let mut right = Vec::new();
        for &c in self.right_cfg.iter().rev() {
            if let Some(w) = chip_width(c, has_media, has_weather) {
                rx -= w;
                right.push((c, Span { x: rx, w }));
                rx -= ITEM_GAP;
            }
        }
        right.reverse();
        let right_x = rx - pad;

        let mut islands = if self.full {
            vec![(x0, x1)]
        } else {
            let mut v = Vec::new();
            if !left.is_empty() {
                v.push((x0, left_end));
            }
            if group_w > 0.0 {
                v.push((gx - pad, gx + group_w + pad));
            }
            if right_x < x1 - pad - 1.0 {
                v.push((right_x, x1));
            }
            v
        };
        if self.start_left {
            islands.insert(0, (m, m + start_w));
        }
        Geo {
            y0,
            y1,
            start,
            search,
            apps,
            tray: tray_span,
            tray_icons,
            left,
            right,
            status: status_span,
            clock,
            islands,
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hot> {
        let g = &self.geo;
        if y < g.y0 || y > g.y1 {
            return None;
        }
        if g.start.contains(x) {
            return Some(Hot::Start);
        }
        if g.search.is_some_and(|s| s.contains(x)) {
            return Some(Hot::Search);
        }
        if let Some(i) = g.apps.iter().position(|s| s.w > 4.0 && s.contains(x)) {
            return Some(Hot::App(i));
        }
        if let Some((c, s)) = g.left.iter().chain(g.right.iter()).find(|(_, s)| s.contains(x)) {
            return Some(match c {
                Chip::Weather => Hot::Weather,
                Chip::Widgets => Hot::Widgets,
                // The play/pause button is the music widget's right edge.
                Chip::Music if x >= s.x + s.w - 30.0 => Hot::MusicToggle,
                c => Hot::Chip(*c),
            });
        }
        if g.tray.is_some_and(|s| s.contains(x)) {
            return Some(Hot::Tray);
        }
        if let Some((i, _)) = g.tray_icons.iter().find(|(_, s)| s.contains(x)) {
            return Some(Hot::TrayIcon(*i));
        }
        if g.status.w > 0.0 && g.status.contains(x) {
            return Some(Hot::Status);
        }
        if g.clock.w > 0.0 && g.clock.contains(x) {
            return Some(Hot::Clock);
        }
        None
    }

    // -------------------------------------------------------------- input --

    /// Returns true when the hovered thing changed.
    pub fn on_move(&mut self, x: i32, y: i32) -> bool {
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
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        self.hover_x = Some(lx);
        if let Some((Hot::App(i), start)) = self.pressed {
            if self.dragging.is_none() && (x - start).abs() > (6.0 * self.k) as i32 && self.apps[i].pinned {
                self.dragging = Some(i);
            }
        }
        if let Some(from) = self.dragging {
            let to = self.geo.apps.iter().position(|s| s.contains(lx));
            if let Some(to) = to {
                if to != from && self.apps[to].pinned {
                    let app = self.apps.remove(from);
                    self.apps.insert(to, app);
                    self.dragging = Some(to);
                    self.pressed = Some((Hot::App(to), self.pressed.map_or(x, |p| p.1)));
                }
            }
        }
        let hovered = match self.dragging {
            Some(i) => Some(Hot::App(i)),
            None => self.hit(lx, ly),
        };
        let changed = hovered != self.hovered;
        self.hovered = hovered;
        if changed {
            self.dirty = true;
        }
        changed
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        if self.pressed.is_none() {
            self.hover_x = None;
            self.hovered = None;
        }
        self.dirty = true;
    }

    pub fn on_down(&mut self, x: i32) {
        if let Some(h) = self.hovered {
            self.pressed = Some((h, x));
            unsafe {
                SetCapture(self.hwnd);
            }
            self.dirty = true;
        }
    }

    /// Mouse released. Returns what was clicked.
    pub fn on_up(&mut self) -> Option<Hot> {
        unsafe {
            let _ = ReleaseCapture();
        }
        let pressed = self.pressed.take();
        self.dirty = true;
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
            app.bounce.kick(10.0);
            self.dirty = true;
            return;
        }
        let fg = unsafe { GetForegroundWindow() };
        unsafe {
            match app.windows.iter().position(|&w| w == fg) {
                Some(_) if app.windows.len() == 1 && !IsIconic(fg).as_bool() => {
                    let _ = ShowWindow(fg, SW_MINIMIZE);
                    if !IsIconic(fg).as_bool() {
                        crate::helper::request_minimize(fg);
                    }
                }
                Some(pos) => focus_window(app.windows[(pos + 1) % app.windows.len()]),
                None => focus_window(app.windows[0]),
            }
        }
    }

    pub fn launch_new(&mut self, i: usize) {
        if let Some(app) = self.apps.get_mut(i) {
            shell::open(&app.id);
            app.bounce.kick(10.0);
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

    /// Ends the app's processes outright, like Task Manager's End task.
    /// File Explorer windows belong to the shell itself, so those are only
    /// asked to close.
    pub fn force_close(&self, i: usize) {
        let Some(app) = self.apps.get(i) else { return };
        let me = unsafe { GetCurrentProcessId() };
        let mut pids: Vec<u32> = Vec::new();
        for &w in &app.windows {
            // No process to end (a suspended Store app has no window of its
            // own), the shell, or Cascade: just ask the window to close.
            let pid = app_pid(w).filter(|&p| {
                p != me && !process_path(p).unwrap_or_default().to_ascii_lowercase().ends_with("\\explorer.exe")
            });
            let Some(pid) = pid else {
                unsafe {
                    let _ = PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                continue;
            };
            if !pids.contains(&pid) {
                pids.push(pid);
            }
        }
        for pid in pids {
            unsafe {
                if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                    let _ = TerminateProcess(h, 1);
                    let _ = CloseHandle(h);
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
        let mut moving = self.reveal.step(dt);
        let centres: Vec<f32> = self.geo.apps.iter().map(|s| s.centre()).collect();
        for (i, a) in self.apps.iter_mut().enumerate() {
            let target = match (self.hover_x, self.magnify > 0.0, centres.get(i)) {
                (Some(h), true, Some(c)) => {
                    let d = (h - c).abs() / (self.bar_h * 1.6);
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
            self.hovered = None;
        }
        moving
    }

    pub fn render(
        &mut self,
        gfx: &mut Gfx,
        status: &Status,
        weather: Option<&Weather>,
        tray: &mut Systray,
        wd: &WidgetData,
    ) -> Result<()> {
        self.dirty = false;
        if !self.shown || !self.window_visible {
            return Ok(());
        }
        let k = self.k;
        // App icons at exactly the size they're drawn, so they stay sharp.
        // While magnified (or fading in) they get a bitmap of their own at the
        // size they're drawn, in steps of two pixels, instead of stretching one.
        let icon_px = (self.icon * k).round() as u32;
        let app_icons: Vec<Option<ID2D1Bitmap>> = (0..self.apps.len())
            .map(|i| {
                let a = &self.apps[i];
                let scale = (1.0 + self.magnify * a.mag.value) * a.presence.value.clamp(0.0, 1.0);
                let px = if (scale - 1.0).abs() < 0.01 {
                    icon_px
                } else {
                    ((self.icon * k * scale / 2.0).round() as u32) * 2
                };
                let key = a.id.to_ascii_lowercase();
                self.icons.get_mut(&key).and_then(|ic| ic.at(gfx, px))
            })
            .collect();
        let (inline, overflow) =
            if self.show_tray { (tray.inline(), !tray.overflow().is_empty()) } else { (Vec::new(), false) };
        let tray_px = (16.0 * k).round() as u32;
        let tray_icons: Vec<Option<ID2D1Bitmap>> = inline.iter().map(|&i| tray.bitmap(i, gfx, tray_px)).collect();

        let mon_w = self.monitor.right - self.monitor.left;
        let pw = mon_w;
        let wh = TIP_ROOM + self.bar_h + self.margin;
        let ph = (wh * k).ceil() as i32;
        let ww = pw as f32 / k;
        // Auto-hide fades the bar while it sinks a little.
        let hidden_px = (14.0 * k * (1.0 - self.reveal.value.clamp(0.0, 1.0))) as i32;
        let pos = POINT { x: self.monitor.left, y: self.bottom - ph + hidden_px };
        self.screen = RECT { left: pos.x, top: pos.y, right: pos.x + pw, bottom: pos.y + ph };
        let has_weather = self.show_weather && weather.is_some();
        self.geo = self.layout(ww, wh, status, &inline, overflow, wd.media.is_some(), has_weather);
        let g = &self.geo;

        // Blur behind each island, just under this window.
        let settled = self.reveal.value > 0.95;
        for (i, b) in self.backdrops.iter_mut().enumerate() {
            match g.islands.get(i) {
                Some(&(x0, x1)) if self.acrylic && settled => b.place(
                    RECT {
                        left: pos.x + (x0 * k).round() as i32,
                        top: pos.y + (g.y0 * k).round() as i32,
                        right: pos.x + (x1 * k).round() as i32,
                        bottom: pos.y + (g.y1 * k).round() as i32,
                    },
                    self.hwnd,
                ),
                _ => b.hide(),
            }
        }

        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            let r = self.radius;
            let bh = g.y1 - g.y0;
            for &(x0, x1) in &g.islands {
                // Over the blur only the theme's wash (it also keeps the
                // pixels clickable); without blur a solid panel.
                let blurred = self.acrylic && settled;
                theme::panel(&p, x0, g.y0, x1 - x0, bh, r, blurred, self.opacity, 0.6 + 0.4 * self.opacity, 1.0);
            }
            let b = self.button();
            let by = g.y0 + (bh - b) / 2.0;
            let pressed = self.pressed.map(|p| p.0);
            let hover_bg = |h: Hot| -> Option<Color> {
                if pressed == Some(h) {
                    Some(Color::white(0.05))
                } else if self.hovered == Some(h) {
                    Some(Color::white(0.08))
                } else {
                    None
                }
            };

            // Start.
            if let Some(c) = hover_bg(Hot::Start) {
                p.rrect(g.start.x, by, g.start.w, b, 5.0, c);
            }
            start_logo(&p, g.start.centre(), by + b / 2.0, self.icon * 0.92, 1.0);

            // Search.
            if let Some(s) = g.search {
                let sh = (b - 8.0).min(34.0);
                let sy = g.y0 + (bh - sh) / 2.0;
                let a = if self.hovered == Some(Hot::Search) { 0.11 } else { 0.06 };
                p.rrect(s.x, sy, s.w, sh, sh / 2.0, Color::white(a));
                p.stroke_rrect(s.x + 0.5, sy + 0.5, s.w - 1.0, sh - 1.0, sh / 2.0, Color::white(0.07), 1.0);
                let cy = sy + sh / 2.0;
                p.stroke_circle(s.x + 18.0, cy - 1.0, 5.5, Color::white(0.75), 1.5);
                p.line(s.x + 22.0, cy + 3.0, s.x + 25.5, cy + 6.5, Color::white(0.75), 1.5);
                p.text("Search", &f.body, s.x + 32.0, sy, s.w - 36.0, sh, Align::Left, Color::white(0.75));
            }

            // Apps.
            let mut tip: Option<(f32, String)> = None;
            for (i, a) in self.apps.iter().enumerate() {
                let Some(s) = g.apps.get(i).copied() else { break };
                let presence = a.presence.value.clamp(0.0, 1.0);
                if s.w < 2.0 {
                    continue;
                }
                let cx = s.centre();
                let running = !a.windows.is_empty();
                let active = a.windows.contains(&self.foreground);
                let bg = if active { Some(Color::white(if self.hovered == Some(Hot::App(i)) { 0.11 } else { 0.07 })) } else { hover_bg(Hot::App(i)) };
                if let Some(c) = bg {
                    p.rrect(s.x, by, s.w, b, 5.0, c.alpha(presence));
                }
                let size = self.icon * (1.0 + self.magnify * a.mag.value) * presence;
                let iy = by + (b - size) / 2.0 - 1.0 - a.bounce.value.max(0.0) * 6.0;
                match app_icons.get(i).cloned().flatten() {
                    // Always pixel for pixel, so it's sharp.
                    Some(bmp) => p.icon(&bmp, cx, iy + size / 2.0, presence),
                    None => p.rrect(cx - size * 0.4, iy + size * 0.1, size * 0.8, size * 0.8, size * 0.2, Color::white(0.15 * presence)),
                }
                if running {
                    let (w, c) = if active { (16.0, theme::accent()) } else { (6.0, Color::white(0.55)) };
                    p.rrect(cx - w / 2.0, by + b - 3.0, w, 3.0, 1.5, c.alpha(presence));
                }
                if self.hovered == Some(Hot::App(i)) && self.dragging.is_none() && presence > 0.9 {
                    tip = Some((cx, self.name(i)));
                }
            }

            // Hidden icons (^), tray icons, status, clock.
            if let Some(t) = g.tray {
                if let Some(c) = hover_bg(Hot::Tray) {
                    p.rrect(t.x, by, t.w, b, 5.0, c);
                }
                let (tx, ty) = (t.centre(), g.y0 + bh / 2.0);
                p.line(tx - 4.5, ty + 2.0, tx, ty - 2.5, Color::white(0.85), 1.4);
                p.line(tx, ty - 2.5, tx + 4.5, ty + 2.0, Color::white(0.85), 1.4);
            }
            for (&(i, t), icon) in g.tray_icons.iter().zip(&tray_icons) {
                if let Some(c) = hover_bg(Hot::TrayIcon(i)) {
                    p.rrect(t.x, by, t.w, b, 5.0, c);
                }
                match icon {
                    Some(bmp) => p.icon(bmp, t.centre(), g.y0 + bh / 2.0, 1.0),
                    None => p.rrect(t.centre() - 7.0, g.y0 + bh / 2.0 - 7.0, 14.0, 14.0, 3.0, Color::white(0.15)),
                }
                if self.hovered == Some(Hot::TrayIcon(i)) {
                    if let Some(icon) = tray.icons.get(i) {
                        let first = icon.tip.lines().next().unwrap_or("").trim().to_string();
                        if !first.is_empty() {
                            tip = Some((t.centre(), first));
                        }
                    }
                }
            }

            // Widgets, left and right side.
            let mid_y = g.y0 + bh / 2.0;
            for &(chip, s) in g.left.iter().chain(g.right.iter()) {
                let hot = match chip {
                    Chip::Weather => Hot::Weather,
                    Chip::Widgets => Hot::Widgets,
                    c => Hot::Chip(c),
                };
                let hover = hover_bg(hot).or_else(|| if chip == Chip::Music { hover_bg(Hot::MusicToggle) } else { None });
                if let Some(c) = hover {
                    p.rrect(s.x, by, s.w, b, 5.0, c);
                }
                let st = wd.stats;
                let grey = Color::white(0.55);
                let white = Color::white(0.95);
                let two = |top: &str, bottom: &str, top_c: Color, bottom_c: Color, font_b: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat| {
                    p.text(top, &f.small, s.x, mid_y - 16.0, s.w, 16.0, Align::Center, top_c);
                    p.text(bottom, font_b, s.x, mid_y, s.w, 16.0, Align::Center, bottom_c);
                };
                match chip {
                    Chip::Weather => {
                        let Some(wx) = weather else { continue };
                        crate::ui::weather_icon(&p, s.x + 18.0, mid_y, 24.0, wx.code, wx.day, 1.0);
                        let unit = if wx.fahrenheit { "F" } else { "C" };
                        p.text(&format!("{:.0}\u{b0}{unit}", wx.temp), &f.label, s.x + 36.0, mid_y - 17.0, s.w - 38.0, 17.0, Align::Left, white);
                        p.text(wx.describe(), &f.small, s.x + 36.0, mid_y, s.w - 38.0, 15.0, Align::Left, Color::white(0.6));
                    }
                    Chip::Widgets => {
                        let c = Color::white(0.85);
                        for (dx, dy) in [(-4.5, -4.5), (0.5, -4.5), (-4.5, 0.5), (0.5, 0.5)] {
                            p.rrect(s.centre() + dx, mid_y + dy, 4.0, 4.0, 1.2, c);
                        }
                    }
                    Chip::Cpu => two("CPU", &format!("{:.0}%", st.cpu), grey, white, &f.label),
                    Chip::Ram => two("RAM", &format!("{:.0}%", st.ram), grey, white, &f.label),
                    Chip::Gpu => match st.gpu {
                        Some(g) => two("GPU", &format!("{g:.0}%"), grey, white, &f.label),
                        None => two("GPU", "\u{2014}", grey, grey, &f.label),
                    },
                    Chip::Net => {
                        two(
                            &format!("\u{2193} {}", sysmon::speed(st.down)),
                            &format!("\u{2191} {}", sysmon::speed(st.up)),
                            white,
                            white,
                            &f.small,
                        );
                    }
                    Chip::Music => {
                        let Some(m) = wd.media else { continue };
                        let art = 28.0;
                        match &wd.art {
                            Some(bmp) => p.image(bmp, s.x + 4.0, mid_y - art / 2.0, art, art, 1.0),
                            None => p.rrect(s.x + 4.0, mid_y - art / 2.0, art, art, 5.0, Color::white(0.12)),
                        }
                        let tw = s.w - art - 8.0 - 34.0;
                        p.text(&m.title, &f.small, s.x + art + 12.0, mid_y - 16.0, tw, 16.0, Align::Left, white);
                        p.text(&m.artist, &f.small, s.x + art + 12.0, mid_y, tw, 16.0, Align::Left, grey);
                        // Play / pause.
                        let (bx, c) = (s.x + s.w - 16.0, Color::white(0.92));
                        if self.hovered == Some(Hot::MusicToggle) {
                            p.circle(bx, mid_y, 12.0, Color::white(0.10));
                        }
                        if m.playing {
                            p.rrect(bx - 4.5, mid_y - 5.5, 3.2, 11.0, 1.0, c);
                            p.rrect(bx + 1.3, mid_y - 5.5, 3.2, 11.0, 1.0, c);
                        } else {
                            p.polygon(&[(bx - 3.5, mid_y - 6.0), (bx - 3.5, mid_y + 6.0), (bx + 5.5, mid_y)], c);
                        }
                    }
                }
            }

            if g.status.w > 0.0 {
                if let Some(c) = hover_bg(Hot::Status) {
                    p.rrect(g.status.x, by, g.status.w, b, 5.0, c);
                }
                let white = Color::white(0.92);
                // Whole pixels everywhere, so the thin strokes stay crisp at
                // any display scale.
                let mut x = p.snap(g.status.x + 6.0 + STATUS_ICON / 2.0);
                let cy = p.snap(g.y0 + bh / 2.0);
                let sw = p.px(1.5);
                match status.wifi {
                    Some(on) => {
                        let c = if on { white } else { Color::white(0.4) };
                        let base = cy + 5.0;
                        p.arc(x, base, 4.5, -2.4, -0.74, c, sw);
                        p.arc(x, base, 8.5, -2.4, -0.74, c, sw);
                        p.circle(x, base, 1.5, c);
                    }
                    None => {
                        // Wired: a small monitor.
                        let sw = p.px(1.0);
                        let (lx, ty) = (p.snap_line(x - 7.0, sw), p.snap_line(cy - 6.0, sw));
                        p.stroke_rrect(lx, ty, 14.0, 10.0, 1.5, white, sw);
                        let sx = p.snap_line(x, sw);
                        p.line(sx, ty + 10.0, sx, p.snap_line(cy + 7.0, sw), white, sw);
                        let fy = p.snap_line(cy + 7.0, sw);
                        p.line(lx + 3.0, fy, lx + 11.0, fy, white, sw);
                    }
                }
                x += STATUS_ICON;
                crate::ui::speaker(&p, x, cy, 15.0, status.volume, status.muted, white);
                x += STATUS_ICON;
                if let Some((pct, charging)) = status.battery {
                    let sw = p.px(1.0);
                    let (bx, ty) = (p.snap_line(x - 8.0, sw), p.snap_line(cy - 4.5, sw));
                    p.stroke_rrect(bx, ty, 15.0, 9.0, 2.0, white, sw);
                    p.rrect(bx + 15.5, p.snap(cy - 1.5), 1.5, 3.0, 0.7, white);
                    let c = if charging { Color(0.45, 0.9, 0.5, 1.0) } else if pct <= 20 { Color(1.0, 0.4, 0.35, 1.0) } else { white };
                    p.rrect(bx + 1.5, ty + 1.5, 12.0 * pct as f32 / 100.0, 6.0, 1.0, c);
                }
            }
            if g.clock.w > 0.0 {
                if let Some(c) = hover_bg(Hot::Clock) {
                    p.rrect(g.clock.x, by, g.clock.w, b, 5.0, c);
                }
                let (time, date) = clock_text();
                let cy = g.y0 + bh / 2.0;
                p.text(&time, &f.small, g.clock.x, cy - 16.0, g.clock.w - 8.0, 16.0, Align::Right, Color::white(0.95));
                p.text(&date, &f.small, g.clock.x, cy, g.clock.w - 8.0, 16.0, Align::Right, Color::white(0.95));
            }

            // Tooltip above the hovered app or tray icon.
            if let Some((cx, name)) = tip {
                let tw = gfx.measure(&name, &f.body).min(ww - 20.0);
                let (lw, lh) = (tw + 20.0, 28.0);
                let lx = (cx - lw / 2.0).clamp(4.0, ww - lw - 4.0);
                let ly = g.y0 - lh - 8.0;
                p.rrect(lx, ly, lw, lh, 6.0, Color(0.16, 0.16, 0.17, 0.97));
                p.stroke_rrect(lx + 0.5, ly + 0.5, lw - 1.0, lh - 1.0, 6.0, Color::white(0.1), 1.0);
                p.text(&name, &f.body, lx, ly, lw, lh, Align::Center, Color::white(0.95));
            }
        }
        let alpha = (self.reveal.value.clamp(0.0, 1.0) * 255.0) as u8;
        gfx.present(&self.layer, self.hwnd, pos, pw, ph, alpha)?;
        if self.autohide && self.reveal.target == 0.0 && self.reveal.value < 0.01 && self.window_visible {
            // Fully faded out: stop covering anything.
            self.hide_window();
        }
        Ok(())
    }

    /// Screen x of app `i`'s centre (for anchoring the preview).
    pub fn app_screen_x(&self, i: usize) -> i32 {
        let c = self.geo.apps.get(i).map(|s| s.centre()).unwrap_or(0.0);
        self.screen.left + (c * self.k) as i32
    }

    /// The bar in screen pixels (for the hidden tray window).
    pub fn bar_rect(&self) -> RECT {
        let g = &self.geo;
        RECT {
            left: self.screen.left + (self.margin * self.k) as i32,
            top: self.screen.top + (g.y0 * self.k) as i32,
            right: self.screen.right - (self.margin * self.k) as i32,
            bottom: self.screen.top + (g.y1 * self.k) as i32,
        }
    }

    /// Screen x of the ^ button's centre.
    pub fn tray_button_x(&self) -> i32 {
        let c = self.geo.tray.map(|t| t.centre()).unwrap_or(0.0);
        self.screen.left + (c * self.k) as i32
    }

    /// Screen y of the bar's top edge.
    pub fn panel_top(&self) -> i32 {
        // Where the bar sits fully revealed, so the Start menu doesn't land
        // on a bar that is still sliding up from auto-hide.
        let sunk = (14.0 * self.k * (1.0 - self.reveal.value.clamp(0.0, 1.0))) as i32;
        self.screen.top - sunk + (self.geo.y0 * self.k) as i32
    }

    /// Screen x the Start menu should line up with (its left edge when the
    /// taskbar is left-aligned), or None when centred.
    pub fn start_anchor(&self) -> Option<i32> {
        if self.start_left {
            // The menu lines up with the Start button's island at the left edge.
            return Some(self.screen.left + (self.margin * self.k) as i32);
        }
        self.left.then(|| self.screen.left + ((self.geo.start.x - 4.0).max(self.margin) * self.k) as i32)
    }
}
