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
use windows::Win32::Graphics::Gdi::{MonitorFromRect, MonitorFromWindow, HMONITOR, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::audio::BANDS;
use crate::backdrop::Backdrop;
use crate::config::{self, Config};
use crate::gfx::{Align, Color, Fonts, Gfx, Layer, Painter};
use crate::icons::{IconResult, ICON_SIZE};
use crate::media::MediaInfo;
use crate::nvml::Gpu;
use crate::shell;
use crate::spring::Spring;
use crate::sysmon::{self, Stats};
use crate::systray::Systray;
use crate::theme;
use crate::weather::Weather;

/// Room above the bar for tooltips (transparent, click-through).
const TIP_ROOM: f32 = 40.0;
const GAP: f32 = 4.0;
const SEARCH_W: f32 = 112.0;
/// The weather widget's icon, left of its text.
const WEATHER_ICON_W: f32 = 36.0;
/// Longest the weather widget's text gets before it's cut short.
const WEATHER_TEXT_MAX: f32 = 120.0;
const CLOCK_W: f32 = 78.0;
const TRAY_W: f32 = 28.0;
const TRAY_ICON_W: f32 = 28.0;
const STATUS_ICON: f32 = 24.0;
/// Space between floating islands.
const ISLAND_GAP: f32 = 8.0;
const CHIP_STAT_W: f32 = 48.0;
/// Each NVIDIA card's column on the GPU widget (its load and temperature),
/// and with the card's name over it (`gpu_label = name`).
const CHIP_GPU_W: f32 = 66.0;
const CHIP_GPU_NAMED_W: f32 = 92.0;

/// What the GPU widget's big figure is (`gpu_value`).
#[derive(Clone, Copy, PartialEq)]
pub enum GpuValue {
    Load,
    Memory,
    Power,
}
const CHIP_NET_W: f32 = 84.0;
/// The music widget's album art and the space around it, left of the text.
const MUSIC_LEAD: f32 = 40.0;
/// The play/pause button, right of the text.
const MUSIC_BUTTON: f32 = 34.0;
/// The visualizer's bars, between the text and the button.
const MUSIC_BARS: f32 = 34.0;
/// Shortest and longest the music widget's title gets (it fits the song
/// between these; longer ones are cut short).
const MUSIC_TEXT_MIN: f32 = 48.0;
const MUSIC_TEXT_MAX: f32 = 170.0;
const WIDGETS_BTN_W: f32 = 28.0;
/// Space between widgets on the same side.
const ITEM_GAP: f32 = 2.0;
/// The desktops widget's dots, and the longer one for the desktop you're on.
const DESK_DOT: f32 = 6.0;
const DESK_CURRENT_W: f32 = 16.0;
const DESK_DOT_GAP: f32 = 4.0;

/// The desktops widget's numbered boxes (`desktops_style = numbers`).
const DESK_NUM_W: f32 = 18.0;
const DESK_NUM_GAP: f32 = 3.0;

/// How wide the desktops widget's numbered boxes are for `n` desktops.
fn desk_nums_w(n: usize) -> f32 {
    let n = n.max(1) as f32;
    n * DESK_NUM_W + (n - 1.0) * DESK_NUM_GAP
}

/// How wide the desktops widget's dots are for `n` desktops.
fn desk_dots_w(n: usize) -> f32 {
    let n = n.max(1) as f32;
    DESK_CURRENT_W + (n - 1.0) * DESK_DOT + (n - 1.0) * DESK_DOT_GAP
}

const PKEY_APP_USER_MODEL_ID: windows::Win32::Foundation::PROPERTYKEY =
    windows::Win32::Foundation::PROPERTYKEY { fmtid: GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3), pid: 5 };

/// Apps and widgets sliding over to their new places as one is dragged:
/// quick, and only just short of overshooting.
const SLIDE_STIFFNESS: f32 = 700.0;
const SLIDE_DAMPING: f32 = 50.0;
/// How far the dragged app or widget lifts off the bar.
const LIFT: f32 = 3.0;

/// Where the apps (by id) and the widgets are drawn along the bar.
type Places = (Vec<(String, f32)>, Vec<(Chip, f32)>);

pub struct DockApp {
    /// An .exe path, or `shell:AppsFolder\<AppUserModelID>` for Store apps.
    pub id: String,
    pub pinned: bool,
    /// Top-level windows, front-most first.
    pub windows: Vec<HWND>,
    presence: Spring,
    mag: Spring,
    bounce: Spring,
    /// How far it's drawn from its place, easing to 0: it slides over when
    /// the apps are reordered.
    slide: Spring,
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
            slide: Spring::new(0.0, SLIDE_STIFFNESS, SLIDE_DAMPING),
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
    /// A dot per virtual desktop, the one you're on lit (opens Task View).
    Desktops,
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
            "desktops" | "desktop" => Chip::Desktops,
            _ => return None,
        })
    }
}

impl Chip {
    /// Its name in `taskbar_left` / `taskbar_right`.
    pub fn name(self) -> &'static str {
        match self {
            Chip::Weather => "weather",
            Chip::Music => "music",
            Chip::Cpu => "cpu",
            Chip::Ram => "ram",
            Chip::Gpu => "gpu",
            Chip::Net => "net",
            Chip::Widgets => "widgets",
            Chip::Desktops => "desktops",
        }
    }

    /// What pointing at it is.
    fn hot(self) -> Hot {
        match self {
            Chip::Weather => Hot::Weather,
            Chip::Widgets => Hot::Widgets,
            c => Hot::Chip(c),
        }
    }
}

impl Hot {
    /// The widget this is (or is part of), if it's one.
    fn chip(self) -> Option<Chip> {
        match self {
            Hot::Weather => Some(Chip::Weather),
            Hot::Widgets => Some(Chip::Widgets),
            Hot::MusicToggle => Some(Chip::Music),
            Hot::Chip(c) => Some(c),
            _ => None,
        }
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
    /// Each NVIDIA card's own readings (the GPU widget shows one column each).
    pub gpus: &'a [Gpu],
    /// The first shown card's place among all of them (0 unless `gpu_cards`
    /// picks a later one), so "GPU 2" stays GPU 2 shown alone.
    pub gpu_first: usize,
    /// How many virtual desktops there are, and which one you're on.
    pub desks: (usize, usize),
    pub media: Option<&'a MediaInfo>,
    pub art: Option<ID2D1Bitmap>,
    /// The music visualizer's bands and colour, if the music widget shows it.
    pub bars: Option<([f32; BANDS], Color)>,
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
    /// The music widget's width, easing to fit each song.
    music_w: Spring,
    /// The song (title, artist) and the weather (temperature, description)
    /// last measured, with their text's width, so it's measured once per
    /// change rather than every frame.
    music_text: Option<(String, String, f32)>,
    weather_text: Option<(String, String, f32)>,
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
    /// The GPU widget: the temperature over each card's figure, what that
    /// figure is, and the card's name rather than its number.
    gpu_temp: bool,
    gpu_value: GpuValue,
    gpu_names: bool,
    /// The desktops widget as numbered boxes rather than dots.
    desk_numbers: bool,
    right_cfg: Vec<Chip>,

    k: f32,
    monitor: RECT,
    /// Bottom edge of the taskbar area, in screen pixels.
    bottom: i32,

    geo: Geo,
    hover_x: Option<f32>,
    pub hovered: Option<Hot>,
    pressed: Option<(Hot, i32)>,
    /// The app being dragged to a new place.
    dragging: Option<usize>,
    /// The widget being dragged to a new place (on its side or the other).
    drag_chip: Option<Chip>,
    /// The widgets were moved here, for the config to keep.
    chips_changed: bool,
    /// Where on the dragged app or widget the pointer took hold of it.
    grab: f32,
    /// Each widget's slide (as `DockApp::slide`), by `Chip as usize`.
    chip_slide: [Spring; 8],
    /// Where the apps (by id) and widgets were drawn just before they were
    /// moved, until the next layout turns that into their slides.
    pending_slides: Option<Places>,
    tracking: bool,
    /// A pop-up from the taskbar (Start menu, tray flyout) is open, so
    /// auto-hide keeps it in view.
    held: bool,
    pub foreground: HWND,
    pub dirty: bool,
    /// Only the music widget's bars moved since the last frame (see `render_bars`).
    pub bars_dirty: bool,
    /// Last frame's window rect in screen pixels.
    pub screen: RECT,
    /// A taskbar on another screen than the main one: Start, search, apps
    /// and the clock, without the widgets, tray and status icons.
    pub secondary: bool,
    /// Shows only the windows on its own screen (and the pinned apps).
    own_screen: bool,
    /// A fullscreen app is in front on this taskbar's screen (only tracked
    /// for the other screens' taskbars; the main one follows the notch).
    pub fullscreen: bool,
    /// The pinned apps changed here, for the other screens' taskbars to follow.
    pins_changed: bool,
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

pub(crate) fn app_user_model_id(h: HWND) -> Option<String> {
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
        // Windows' foreground lock can turn that down (a click that took a
        // moment to handle, focus stolen in between): Alt+Tab's own way in
        // isn't subject to it.
        if GetForegroundWindow() != h {
            SwitchToThisWindow(h, true);
        }
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

/// How wide a widget is, or None if it has nothing to show right now. The
/// music and weather widgets fit what they show (`music` and `weather`), and
/// the GPU widget has a column for each of the `gpus` NVIDIA cards.
/// `gpu_col` is one card's column.
fn chip_width(
    c: Chip,
    music: Option<f32>,
    weather: Option<f32>,
    gpus: usize,
    gpu_col: f32,
    desks: usize,
    desk_numbers: bool,
) -> Option<f32> {
    match c {
        Chip::Weather => weather,
        Chip::Music => music,
        Chip::Net => Some(CHIP_NET_W),
        Chip::Widgets => Some(WIDGETS_BTN_W),
        // Only with more than one desktop.
        Chip::Desktops if desk_numbers => (desks > 1).then(|| desk_nums_w(desks) + 16.0),
        Chip::Desktops => (desks > 1).then(|| desk_dots_w(desks) + 16.0),
        Chip::Gpu if gpus > 0 => Some(gpu_col * gpus as f32),
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
            music_w: Spring::new(0.0, 300.0, 34.0),
            music_text: None,
            weather_text: None,
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
            gpu_temp: true,
            gpu_value: GpuValue::Load,
            gpu_names: false,
            desk_numbers: false,
            right_cfg: Vec::new(),
            k: 1.0,
            monitor: RECT::default(),
            bottom: 0,
            geo: Geo::default(),
            hover_x: None,
            hovered: None,
            pressed: None,
            dragging: None,
            drag_chip: None,
            chips_changed: false,
            grab: 0.0,
            chip_slide: [Spring::new(0.0, SLIDE_STIFFNESS, SLIDE_DAMPING); 8],
            pending_slides: None,
            tracking: false,
            held: false,
            foreground: HWND::default(),
            dirty: true,
            bars_dirty: false,
            screen: RECT::default(),
            secondary: false,
            own_screen: false,
            fullscreen: false,
            pins_changed: false,
        };
        for id in load_pinned() {
            let mut app = DockApp::new(id, true);
            app.presence.snap(1.0);
            d.request_icon(&app.id);
            d.apps.push(app);
        }
        d
    }

    /// A taskbar for another screen (see `secondary`).
    pub fn new_secondary(hwnd: HWND, trigger: HWND, icon_tx: Sender<String>, backdrops: Vec<Backdrop>) -> Self {
        let mut d = Self::new(hwnd, trigger, icon_tx, backdrops);
        d.secondary = true;
        d
    }

    pub fn is(&self, h: HWND) -> bool {
        h == self.hwnd || h == self.trigger
    }

    /// For a taskbar on another screen to load its icons the same way.
    pub fn icon_sender(&self) -> Sender<String> {
        self.icon_tx.clone()
    }

    /// Physical pixels per layout unit on this taskbar's screen.
    pub fn k(&self) -> f32 {
        self.k
    }

    /// The screen this taskbar is on.
    pub fn monitor(&self) -> RECT {
        self.monitor
    }

    fn hmonitor(&self) -> HMONITOR {
        unsafe { MonitorFromRect(&self.monitor, MONITOR_DEFAULTTONEAREST) }
    }

    /// Whether it shows window `h`, but `h` has since moved to another screen.
    pub fn misplaced(&self, h: HWND) -> bool {
        self.own_screen
            && self.tracks(h)
            && unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) } != self.hmonitor()
    }

    /// The pinned apps, in order.
    pub fn pinned_ids(&self) -> Vec<String> {
        self.apps.iter().filter(|a| a.pinned).map(|a| a.id.clone()).collect()
    }

    /// Whether the pinned apps changed since the last call.
    pub fn take_pins_changed(&mut self) -> bool {
        std::mem::take(&mut self.pins_changed)
    }

    /// Takes on the pinned apps (and their order) another screen's taskbar
    /// changed to. Running apps that aren't pinned keep their places after them.
    pub fn adopt_pins(&mut self, ids: &[String]) {
        let mut rest: Vec<DockApp> = std::mem::take(&mut self.apps);
        let mut apps = Vec::with_capacity(rest.len() + ids.len());
        for id in ids {
            let app = match rest.iter().position(|a| a.id.eq_ignore_ascii_case(id)) {
                Some(i) => rest.remove(i),
                None => {
                    let mut a = DockApp::new(id.clone(), true);
                    a.presence.snap(1.0);
                    self.request_icon(id);
                    a
                }
            };
            apps.push(app);
        }
        for a in &mut apps {
            a.pinned = true;
            a.presence.set(1.0);
        }
        for mut a in rest {
            a.pinned = false;
            a.presence.set(if a.windows.is_empty() { 0.0 } else { 1.0 });
            apps.push(a);
        }
        self.apps = apps;
        self.hovered = None;
        self.dragging = None;
        self.pressed = None;
        self.dirty = true;
    }

    /// Takes the taskbar and its windows down for good (its screen went away).
    pub fn destroy(mut self) {
        self.shutdown();
        crate::drop::revoke(self.hwnd);
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = DestroyWindow(self.trigger);
            for b in &self.backdrops {
                let _ = DestroyWindow(b.hwnd);
            }
        }
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

    fn save_pinned(&mut self) {
        let list: Vec<&str> = self.apps.iter().filter(|a| a.pinned).map(|a| a.id.as_str()).collect();
        let _ = std::fs::create_dir_all(config::dir());
        let _ = std::fs::write(pinned_path(), list.join("\r\n") + "\r\n");
        self.pins_changed = true;
    }

    // ------------------------------------------------------------ placement --

    /// Applies config and screen geometry, and reserves screen space when the
    /// taskbar is always shown. Visibility is separate (`set_visible`) so a
    /// fullscreen game never changes the work area of other windows.
    /// One NVIDIA card's column on the GPU widget.
    fn gpu_col(&self) -> f32 {
        if self.gpu_names {
            CHIP_GPU_NAMED_W
        } else {
            CHIP_GPU_W
        }
    }

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
        self.gpu_temp = cfg.gpu_temp;
        self.gpu_value = match cfg.gpu_value.as_str() {
            "memory" => GpuValue::Memory,
            "power" => GpuValue::Power,
            _ => GpuValue::Load,
        };
        self.gpu_names = cfg.gpu_label == "name";
        self.desk_numbers = cfg.desktops_style == "numbers";
        self.right_cfg = parse_chips(&cfg.taskbar_right);
        self.own_screen = cfg.taskbar_screens == "all" && cfg.taskbar_screen_apps == "screen";
        if self.secondary {
            // The widgets, tray and status icons stay on the main screen.
            self.show_status = false;
            self.show_tray = false;
            self.show_weather = false;
            self.show_clock = cfg.taskbar_screen_clock;
            self.left_cfg.clear();
            self.right_cfg.clear();
        }
        self.monitor = monitor;
        self.k = k;
        for b in &self.backdrops {
            theme::style_backdrop(b, cfg.acrylic, self.opacity, self.radius);
        }

        if self.enabled && !self.autohide {
            let px = self.reserve_px(k);
            self.reserve_slot =
                Some(shell::reserve_bottom(self.hwnd, monitor, px, &mut self.reserved, self.reserve_slot));
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
            // Above Windows' own taskbar on that screen, if it shows one.
            None if self.secondary => shell::work_area_of(self.monitor).bottom.min(self.monitor.bottom),
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
        // Gone from under the pointer: no leave will come to clear the hover.
        if self.pressed.is_some() {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        if self.hovered.is_some() || self.pressed.is_some() || self.tracking {
            self.on_pointer_gone();
        }
        // Nothing to slide while it's away.
        self.pending_slides = None;
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
        let screen = self.own_screen.then(|| self.hmonitor());
        for h in all {
            if !is_app_window(h, own) {
                continue;
            }
            // (A minimised window counts where it will come back.)
            if screen.is_some_and(|m| unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) } != m) {
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
        music: Option<f32>,
        weather: Option<f32>,
        gpus: usize,
        desks: usize,
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
            if let Some(w) = chip_width(c, music, weather, gpus, self.gpu_col(), desks, self.desk_numbers) {
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
            if let Some(w) = chip_width(c, music, weather, gpus, self.gpu_col(), desks, self.desk_numbers) {
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

    /// Asks Windows for a `WM_MOUSELEAVE` once the pointer leaves (at once,
    /// if it already has).
    fn track(&mut self) {
        if self.tracking {
            return;
        }
        unsafe {
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            self.tracking = TrackMouseEvent(&mut t).is_ok();
        }
    }

    /// Returns true when the hovered thing changed.
    pub fn on_move(&mut self, x: i32, y: i32) -> bool {
        self.track();
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        self.hover_x = Some(lx);
        // Pulled sideways far enough, a press becomes a drag: an app (pinned
        // or running) among the apps, or a widget along the bar.
        if let Some((h, start)) = self.pressed {
            if self.dragging.is_none() && self.drag_chip.is_none() && (x - start).abs() > (6.0 * self.k) as i32 {
                let at = start as f32 / self.k;
                match h {
                    Hot::App(i) if i < self.apps.len() => {
                        self.dragging = Some(i);
                        self.grab = at - self.geo.apps.get(i).map_or(at, |s| s.x);
                    }
                    h => {
                        self.drag_chip = h.chip();
                        let g = &self.geo;
                        let span = g.left.iter().chain(&g.right).find(|(c, _)| Some(*c) == self.drag_chip);
                        self.grab = at - span.map_or(at, |(_, s)| s.x);
                    }
                }
            }
        }
        if self.dragging.is_some() || self.drag_chip.is_some() {
            // It follows the pointer.
            self.dirty = true;
        }
        if let Some(from) = self.dragging {
            let to = self.geo.apps.iter().position(|s| s.w > 4.0 && s.contains(lx));
            if let Some(to) = to.filter(|&to| to != from && to < self.apps.len()) {
                self.note_slides();
                let app = self.apps.remove(from);
                self.apps.insert(to, app);
                self.dragging = Some(to);
                self.pressed = Some((Hot::App(to), self.pressed.map_or(x, |p| p.1)));
                self.dirty = true;
            }
        }
        if let Some(c) = self.drag_chip {
            // Where everything is drawn before the move (see `note_slides`).
            let before = (self.app_visuals(), self.chip_visuals());
            if self.move_chip(c, lx) {
                if self.pending_slides.is_none() {
                    self.pending_slides = Some(before);
                }
                self.dirty = true;
            }
        }
        let hovered = match (self.dragging, self.drag_chip) {
            (Some(i), _) => Some(Hot::App(i)),
            (_, Some(c)) => Some(c.hot()),
            _ => self.hit(lx, ly),
        };
        let changed = hovered != self.hovered;
        self.hovered = hovered;
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// Moves widget `c`, being dragged, to where the pointer is (`lx`): onto
    /// the side of the apps it's on, before the first widget there whose
    /// middle is past it. True if that's a new place.
    fn move_chip(&mut self, c: Chip, lx: f32) -> bool {
        let g = &self.geo;
        // The middle of Start, search and the apps divides the two sides.
        let lo = if self.start_left {
            g.search.map(|s| s.x).or_else(|| g.apps.first().map(|s| s.x)).unwrap_or(g.start.x)
        } else {
            g.start.x
        };
        let hi = g.apps.last().map(|s| s.x + s.w).or_else(|| g.search.map(|s| s.x + s.w)).unwrap_or(lo);
        let left_side = lx < (lo + hi) / 2.0;
        let shown = if left_side { &g.left } else { &g.right };
        let before = shown.iter().find(|(x, s)| *x != c && s.centre() > lx).map(|(x, _)| *x);
        let mut left: Vec<Chip> = self.left_cfg.iter().copied().filter(|&x| x != c).collect();
        let mut right: Vec<Chip> = self.right_cfg.iter().copied().filter(|&x| x != c).collect();
        let list = if left_side { &mut left } else { &mut right };
        let at = before.and_then(|b| list.iter().position(|&x| x == b)).unwrap_or(list.len());
        list.insert(at, c);
        if left == self.left_cfg && right == self.right_cfg {
            return false;
        }
        self.left_cfg = left;
        self.right_cfg = right;
        self.chips_changed = true;
        true
    }

    /// Where the dragged app is drawn: under the pointer, kept among the apps.
    fn dragged_app_x(&self) -> Option<f32> {
        let x = self.hover_x? - self.grab;
        let mut shown = self.geo.apps.iter().filter(|s| s.w > 4.0).map(|s| s.x);
        let first = shown.next()?;
        let (lo, hi) = shown.fold((first, first), |(lo, hi), v| (lo.min(v), hi.max(v)));
        Some(x.clamp(lo, hi))
    }

    /// Where the dragged widget, `w` wide, is drawn: under the pointer.
    fn dragged_chip_x(&self, w: f32) -> Option<f32> {
        let ww = (self.monitor.right - self.monitor.left) as f32 / self.k;
        Some((self.hover_x? - self.grab).clamp(0.0, (ww - w).max(0.0)))
    }

    /// Where each app is drawn right now, by id.
    fn app_visuals(&self) -> Vec<(String, f32)> {
        let dragged = self.dragged_app_x();
        let at = |i: usize, a: &DockApp, s: &Span| match dragged {
            Some(x) if self.dragging == Some(i) => x,
            _ => s.x + a.slide.value,
        };
        self.apps.iter().zip(&self.geo.apps).enumerate().map(|(i, (a, s))| (a.id.clone(), at(i, a, s))).collect()
    }

    /// Where each widget is drawn right now.
    fn chip_visuals(&self) -> Vec<(Chip, f32)> {
        let g = &self.geo;
        let at = |c: Chip, s: &Span| match self.dragged_chip_x(s.w) {
            Some(x) if self.drag_chip == Some(c) => x,
            _ => s.x + self.chip_slide[c as usize].value,
        };
        g.left.iter().chain(&g.right).map(|(c, s)| (*c, at(*c, s))).collect()
    }

    /// The apps and widgets are about to move: keeps where they're drawn now,
    /// so they slide from there to their new places rather than jump. (If
    /// they've moved already since the last frame, what's on screen is
    /// still that earlier note.)
    fn note_slides(&mut self) {
        if self.pending_slides.is_none() {
            self.pending_slides = Some((self.app_visuals(), self.chip_visuals()));
        }
    }

    /// A drag ended: the app or widget settles from under the pointer into
    /// its place.
    fn drop_slides(&mut self) {
        let app = self.dragging.and_then(|i| Some((self.apps.get(i)?.id.clone(), self.dragged_app_x()?)));
        let chip = self.drag_chip.and_then(|c| {
            let g = &self.geo;
            let (_, s) = g.left.iter().chain(&g.right).find(|(k, _)| *k == c)?;
            Some((c, self.dragged_chip_x(s.w)?))
        });
        self.note_slides();
        if let Some((apps, chips)) = &mut self.pending_slides {
            if let Some((id, x)) = app {
                apps.retain(|(a, _)| *a != id);
                apps.push((id, x));
            }
            if let Some((c, x)) = chip {
                chips.retain(|(k, _)| *k != c);
                chips.push((c, x));
            }
        }
        self.dirty = true;
    }

    /// Turns the places noted by `note_slides` into slides, now the apps and
    /// widgets have their new places.
    fn start_slides(&mut self) {
        let Some((apps, chips)) = self.pending_slides.take() else { return };
        for (a, s) in self.apps.iter_mut().zip(&self.geo.apps) {
            if let Some((_, x)) = apps.iter().find(|(id, _)| *id == a.id) {
                a.slide.snap(x - s.x);
                a.slide.set(0.0);
            }
        }
        for (c, s) in self.geo.left.iter().chain(&self.geo.right) {
            if let Some((_, x)) = chips.iter().find(|(k, _)| k == c) {
                let slide = &mut self.chip_slide[*c as usize];
                slide.snap(x - s.x);
                slide.set(0.0);
            }
        }
    }

    /// The widgets' new places after one was dragged, as `taskbar_left` and
    /// `taskbar_right` (once, once the drag is over).
    pub fn take_chips(&mut self) -> Option<(String, String)> {
        if self.drag_chip.is_some() || !std::mem::take(&mut self.chips_changed) {
            return None;
        }
        let names = |l: &[Chip]| l.iter().map(|c| c.name()).collect::<Vec<_>>().join(",");
        Some((names(&self.left_cfg), names(&self.right_cfg)))
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        if self.pressed.is_none() {
            self.hover_x = None;
            self.hovered = None;
        }
        self.dirty = true;
    }

    /// The press ended without a release reaching the taskbar (another
    /// window took the mouse, or the button went up somewhere Windows didn't
    /// tell it): ends any drag where it got to, and forgets the hover.
    pub fn on_pointer_gone(&mut self) {
        if self.dragging.is_some() || self.drag_chip.is_some() {
            self.drop_slides();
        }
        self.pressed = None;
        self.drag_chip = None;
        if self.dragging.take().is_some() {
            self.save_pinned();
        }
        self.tracking = false;
        self.hover_x = None;
        self.hovered = None;
        self.dirty = true;
    }

    /// Something else took the mouse while a button was down on the taskbar.
    pub fn on_capture_lost(&mut self) {
        if self.pressed.is_some() {
            self.on_pointer_gone();
        }
    }

    /// Mouse pressed at (x, y). What's under it is looked up again here, not
    /// taken from the last mouse move: the buttons shift when an app opens
    /// or closes, so a click without moving the mouse first would otherwise
    /// land on whichever app used to be there (or nothing).
    pub fn on_down(&mut self, x: i32, y: i32) {
        let hit = self.hit(x as f32 / self.k, y as f32 / self.k);
        if hit != self.hovered {
            self.hovered = hit;
            self.hover_x = Some(x as f32 / self.k);
            self.dirty = true;
        }
        if let Some(h) = self.hovered {
            self.pressed = Some((h, x));
            unsafe {
                SetCapture(self.hwnd);
            }
            self.dirty = true;
        }
    }

    /// Mouse released at (x, y). Returns what was clicked: what it was
    /// pressed on, if it's still under the pointer.
    pub fn on_up(&mut self, x: i32, y: i32) -> Option<Hot> {
        if self.dragging.is_some() || self.drag_chip.is_some() {
            self.drop_slides();
        }
        let pressed = self.pressed.take();
        let dragged = self.dragging.take().is_some();
        let moved_chip = self.drag_chip.take().is_some();
        unsafe {
            let _ = ReleaseCapture();
        }
        // While the mouse was captured, leaving went unsaid: ask again, so a
        // pointer that's no longer here drops the hover (and its tooltip).
        self.tracking = false;
        self.track();
        self.dirty = true;
        if dragged {
            self.save_pinned();
            return None;
        }
        if moved_chip {
            return None;
        }
        let under = self.hit(x as f32 / self.k, y as f32 / self.k);
        match (pressed, under.or(self.hovered)) {
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

    /// Whether the taskbar is on screen (not turned off or hidden for a
    /// fullscreen app).
    pub fn visible(&self) -> bool {
        self.shown && self.window_visible
    }

    pub fn wants_frame(&self) -> bool {
        self.shown
            && self.window_visible
            && (self.dirty
                || self.bars_dirty
                || !self.reveal.settled()
                || !self.music_w.settled()
                || self.chip_slide.iter().any(|s| !s.settled())
                || self
                    .apps
                    .iter()
                    .any(|a| !a.presence.settled() || !a.mag.settled() || !a.bounce.settled() || !a.slide.settled()))
    }

    /// Steps springs; returns true while the size or position is changing.
    pub fn step(&mut self, dt: f32) -> bool {
        let mut moving = self.reveal.step(dt);
        moving |= self.music_w.step(dt);
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
            moving |= a.slide.step(dt);
        }
        for s in &mut self.chip_slide {
            moving |= s.step(dt);
        }
        // Drop apps that finished sliding out.
        let before = self.apps.len();
        self.apps.retain(|a| a.presence.target > 0.5 || a.presence.value > 0.01);
        if self.apps.len() != before {
            // The apps' places changed under any press or drag.
            self.hovered = None;
            if matches!(self.pressed, Some((Hot::App(_), _))) {
                self.pressed = None;
            }
            if self.dragging.take().is_some() {
                self.save_pinned();
            }
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
        let was_dirty = std::mem::take(&mut self.dirty);
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
        // Only the music widget's bars moved: redraw and upload just that.
        let bars_only = std::mem::take(&mut self.bars_dirty) && !was_dirty && self.settled() && self.layer.ready();
        let same_place = self.screen == (RECT { left: pos.x, top: pos.y, right: pos.x + pw, bottom: pos.y + ph });
        if bars_only && same_place && self.render_bars(gfx, wd, pos, pw, ph)? {
            return Ok(());
        }
        self.screen = RECT { left: pos.x, top: pos.y, right: pos.x + pw, bottom: pos.y + ph };
        // The music and weather widgets fit their text.
        let small = &gfx.fonts.small;
        let music_w = wd.media.map(|m| {
            // Measured once per song, not every frame.
            let text = match &self.music_text {
                Some((title, artist, w)) if *title == m.title && *artist == m.artist => *w,
                _ => {
                    let w = gfx.measure(&m.title, small).max(gfx.measure(&m.artist, small));
                    self.music_text = Some((m.title.clone(), m.artist.clone(), w));
                    w
                }
            };
            let bars = if wd.bars.is_some() { MUSIC_BARS } else { 0.0 };
            let w = MUSIC_LEAD + text.clamp(MUSIC_TEXT_MIN, MUSIC_TEXT_MAX).ceil() + bars + MUSIC_BUTTON;
            if self.music_w.value < 1.0 {
                // Appearing: at its size straight away.
                self.music_w.snap(w);
            }
            self.music_w.set(w);
            self.music_w.value
        });
        if music_w.is_none() {
            self.music_w.snap(0.0);
        }
        let weather_w = weather.filter(|_| self.show_weather).map(|wx| {
            let unit = if wx.fahrenheit { "F" } else { "C" };
            let temp = format!("{:.0}\u{b0}{unit}", wx.temp);
            let text = match &self.weather_text {
                Some((t, d, w)) if *t == temp && d == wx.describe() => *w,
                _ => {
                    let w = gfx.measure(&temp, &gfx.fonts.label).max(gfx.measure(wx.describe(), small));
                    self.weather_text = Some((temp, wx.describe().to_string(), w));
                    w
                }
            };
            (WEATHER_ICON_W + text.min(WEATHER_TEXT_MAX) + 10.0).ceil()
        });
        let desks = wd.desks.0;
        self.geo = self.layout(ww, wh, status, &inline, overflow, music_w, weather_w, wd.gpus.len(), desks);
        self.start_slides();
        let g = &self.geo;

        // Blur behind each island, just under this window (placed once the
        // frame is presented). Also while the bar sinks or rises: it follows.
        let glass = self.acrylic && self.reveal.value > theme::GLASS_FROM;
        let islands: Vec<RECT> = g
            .islands
            .iter()
            .map(|&(x0, x1)| RECT {
                left: pos.x + (x0 * k).round() as i32,
                top: pos.y + (g.y0 * k).round() as i32,
                right: pos.x + (x1 * k).round() as i32,
                bottom: pos.y + (g.y1 * k).round() as i32,
            })
            .collect();

        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            let r = self.radius;
            let bh = g.y1 - g.y0;
            for &(x0, x1) in &g.islands {
                // Over the blur only the theme's wash (it also keeps the
                // pixels clickable); without blur a solid panel.
                theme::panel(&p, x0, g.y0, x1 - x0, bh, r, glass, self.opacity, 0.6 + 0.4 * self.opacity, 1.0);
            }
            let b = self.button();
            let by = g.y0 + (bh - b) / 2.0;
            let pressed = self.pressed.map(|p| p.0);
            let dragged = self.dragging.map(Hot::App).or(self.drag_chip.map(Chip::hot));
            let hover_bg = |h: Hot| -> Option<Color> {
                if dragged == Some(h) {
                    Some(Color::white(0.14))
                } else if pressed == Some(h) {
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
            // The dragged app last, so it's drawn over the others.
            let order = (0..self.apps.len()).filter(|&i| self.dragging != Some(i)).chain(self.dragging);
            let held_x = self.dragged_app_x();
            for i in order {
                let Some(a) = self.apps.get(i) else { continue };
                let Some(s) = g.apps.get(i).copied() else { continue };
                let presence = a.presence.value.clamp(0.0, 1.0);
                if s.w < 2.0 {
                    continue;
                }
                // Under the pointer while it's dragged; the others slide over.
                let lifted = self.dragging == Some(i);
                let s = match held_x {
                    Some(x) if lifted => Span { x, w: s.w },
                    _ => Span { x: s.x + a.slide.value, w: s.w },
                };
                let lift = if lifted { LIFT } else { 0.0 };
                let cx = s.centre();
                let running = !a.windows.is_empty();
                let active = a.windows.contains(&self.foreground);
                let bg = if active {
                    Some(Color::white(if self.hovered == Some(Hot::App(i)) { 0.11 } else { 0.07 }))
                } else {
                    hover_bg(Hot::App(i))
                };
                if let Some(c) = bg {
                    p.rrect(s.x, by, s.w, b, 5.0, c.alpha(presence));
                }
                let size = self.icon * (1.0 + self.magnify * a.mag.value) * presence;
                let iy = by + (b - size) / 2.0 - 1.0 - a.bounce.value.max(0.0) * 6.0 - lift;
                match app_icons.get(i).cloned().flatten() {
                    // Always pixel for pixel, so it's sharp.
                    Some(bmp) => p.icon(&bmp, cx, iy + size / 2.0, presence),
                    None => p.rrect(
                        cx - size * 0.4,
                        iy + size * 0.1,
                        size * 0.8,
                        size * 0.8,
                        size * 0.2,
                        Color::white(0.15 * presence),
                    ),
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
            // The dragged one last, so it's drawn over the others.
            let mut chips: Vec<(Chip, Span)> = g.left.iter().chain(g.right.iter()).copied().collect();
            chips.sort_by_key(|(c, _)| self.drag_chip == Some(*c));
            for (chip, s) in chips {
                // Under the pointer while it's dragged; the others slide over.
                let lifted = self.drag_chip == Some(chip);
                let s = match self.dragged_chip_x(s.w) {
                    Some(x) if lifted => Span { x, w: s.w },
                    _ => Span { x: s.x + self.chip_slide[chip as usize].value, w: s.w },
                };
                let lift = if lifted { LIFT } else { 0.0 };
                let (by, mid_y) = (by - lift, mid_y - lift);
                let hot = match chip {
                    Chip::Weather => Hot::Weather,
                    Chip::Widgets => Hot::Widgets,
                    c => Hot::Chip(c),
                };
                let hover =
                    hover_bg(hot).or_else(|| if chip == Chip::Music { hover_bg(Hot::MusicToggle) } else { None });
                if let Some(c) = hover {
                    p.rrect(s.x, by, s.w, b, 5.0, c);
                }
                let st = wd.stats;
                let grey = Color::white(0.55);
                let white = Color::white(0.95);
                let two =
                    |top: &str,
                     bottom: &str,
                     top_c: Color,
                     bottom_c: Color,
                     font_b: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat| {
                        p.text(top, &f.small, s.x, mid_y - 16.0, s.w, 16.0, Align::Center, top_c);
                        p.text(bottom, font_b, s.x, mid_y, s.w, 16.0, Align::Center, bottom_c);
                    };
                match chip {
                    Chip::Weather => {
                        let Some(wx) = weather else { continue };
                        crate::ui::weather_icon(&p, s.x + 18.0, mid_y, 24.0, wx.code, wx.day, 1.0);
                        let unit = if wx.fahrenheit { "F" } else { "C" };
                        let (tx, tw) = (s.x + WEATHER_ICON_W, s.w - WEATHER_ICON_W - 2.0);
                        p.text(
                            &format!("{:.0}\u{b0}{unit}", wx.temp),
                            &f.label,
                            tx,
                            mid_y - 17.0,
                            tw,
                            17.0,
                            Align::Left,
                            white,
                        );
                        p.text(wx.describe(), &f.small, tx, mid_y, tw, 15.0, Align::Left, Color::white(0.6));
                    }
                    Chip::Desktops if self.desk_numbers => {
                        // A numbered box per desktop, the one you're on filled.
                        let (n, current) = wd.desks;
                        let mut x = s.centre() - desk_nums_w(n) / 2.0;
                        for i in 0..n {
                            let on = i == current;
                            let y = mid_y - DESK_NUM_W / 2.0;
                            if on {
                                p.rrect(x, y, DESK_NUM_W, DESK_NUM_W, 5.0, theme::accent());
                            } else {
                                p.stroke_rrect(
                                    x + 0.5,
                                    y + 0.5,
                                    DESK_NUM_W - 1.0,
                                    DESK_NUM_W - 1.0,
                                    5.0,
                                    Color::white(0.35),
                                    1.0,
                                );
                            }
                            let c = if on { Color::white(1.0) } else { Color::white(0.7) };
                            p.text(&(i + 1).to_string(), &f.small, x, y, DESK_NUM_W, DESK_NUM_W, Align::Center, c);
                            x += DESK_NUM_W + DESK_NUM_GAP;
                        }
                    }
                    Chip::Desktops => {
                        // A dot per desktop; the one you're on a longer, lit one.
                        let (n, current) = wd.desks;
                        let mut x = s.centre() - desk_dots_w(n) / 2.0;
                        for i in 0..n {
                            let (dw, c) = if i == current {
                                (DESK_CURRENT_W, theme::accent())
                            } else {
                                (DESK_DOT, Color::white(0.55))
                            };
                            p.rrect(x, mid_y - DESK_DOT / 2.0, dw, DESK_DOT, DESK_DOT / 2.0, c);
                            x += dw + DESK_DOT_GAP;
                        }
                    }
                    Chip::Widgets => {
                        let c = Color::white(0.85);
                        for (dx, dy) in [(-4.5, -4.5), (0.5, -4.5), (-4.5, 0.5), (0.5, 0.5)] {
                            p.rrect(s.centre() + dx, mid_y + dy, 4.0, 4.0, 1.2, c);
                        }
                    }
                    Chip::Cpu => two("CPU", &format!("{:.0}%", st.cpu), grey, white, &f.label),
                    Chip::Ram => two("RAM", &format!("{:.0}%", st.ram), grey, white, &f.label),
                    // One column per NVIDIA card: its figure (load, memory or
                    // power), under its number (or name) and temperature.
                    Chip::Gpu if !wd.gpus.is_empty() => {
                        let n = wd.gpus.len();
                        let col = self.gpu_col();
                        for (i, g) in wd.gpus.iter().enumerate() {
                            let cx = s.x + i as f32 * col;
                            let name = if self.gpu_names {
                                g.name.clone()
                            } else if n > 1 {
                                format!("GPU {}", wd.gpu_first + i + 1)
                            } else {
                                "GPU".to_string()
                            };
                            let top = match g.temp.filter(|_| self.gpu_temp) {
                                Some(t) => format!("{name} \u{b7} {t:.0}\u{b0}"),
                                None => name,
                            };
                            let figure = match self.gpu_value {
                                GpuValue::Load => g.load.map(|l| format!("{l:.0}%")),
                                GpuValue::Memory => g.vram_used.map(|u| format!("{u:.1} GB")),
                                GpuValue::Power => g.power.map(|w| format!("{w:.0} W")),
                            };
                            let (figure, c) = match figure {
                                Some(v) => (v, white),
                                None => ("\u{2014}".to_string(), grey),
                            };
                            p.text(&top, &f.small, cx, mid_y - 16.0, col, 16.0, Align::Center, grey);
                            p.text(&figure, &f.label, cx, mid_y, col, 16.0, Align::Center, c);
                        }
                    }
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
                    Chip::Music => self.draw_music(&p, f, s, mid_y, wd),
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
                    let c = if charging {
                        Color(0.45, 0.9, 0.5, 1.0)
                    } else if pct <= 20 {
                        Color(1.0, 0.4, 0.35, 1.0)
                    } else {
                        white
                    };
                    p.rrect(bx + 1.5, ty + 1.5, 12.0 * pct as f32 / 100.0, 6.0, 1.0, c);
                    if charging {
                        crate::ui::bolt(&p, bx + 7.5, ty + 4.5, 11.0, 1.0);
                    }
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
        for (i, b) in self.backdrops.iter_mut().enumerate() {
            match islands.get(i) {
                Some(&rect) if glass => self.dirty |= b.track(rect, None, self.hwnd),
                Some(&rect) => b.hold(rect),
                None => b.hide(),
            }
        }
        if self.autohide && self.reveal.target == 0.0 && self.reveal.value < 0.01 && self.window_visible {
            // Fully faded out: stop covering anything.
            self.hide_window();
        }
        Ok(())
    }

    /// The music widget at `s`: art, title and artist, the visualizer's bars
    /// and the play / pause button.
    fn draw_music(&self, p: &Painter, f: &Fonts, s: Span, mid_y: f32, wd: &WidgetData) {
        let Some(m) = wd.media else { return };
        let (white, grey) = (Color::white(0.95), Color::white(0.55));
        let art = 28.0;
        match &wd.art {
            Some(bmp) => p.image(bmp, s.x + 4.0, mid_y - art / 2.0, art, art, 1.0),
            None => p.rrect(s.x + 4.0, mid_y - art / 2.0, art, art, 5.0, Color::white(0.12)),
        }
        // Bars moving with the music, between the title and the button.
        let viz = if wd.bars.is_some() { MUSIC_BARS } else { 0.0 };
        let tw = s.w - MUSIC_LEAD - MUSIC_BUTTON - viz;
        if let Some((levels, c)) = &wd.bars {
            let x = s.x + s.w - MUSIC_BUTTON - viz + 2.0;
            crate::ui::spectrum(p, levels, x, mid_y + 9.0, 28.0, 7, 17.0, c.alpha(0.95));
        }
        p.text(&m.title, &f.small, s.x + MUSIC_LEAD, mid_y - 16.0, tw, 16.0, Align::Left, white);
        p.text(&m.artist, &f.small, s.x + MUSIC_LEAD, mid_y, tw, 16.0, Align::Left, grey);
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

    /// Whether anything but the visualizer's bars could look different from
    /// the last frame (a spring still moving, or something marked dirty).
    fn settled(&self) -> bool {
        self.reveal.settled()
            && self.music_w.settled()
            && self.chip_slide.iter().all(|s| s.settled())
            && self
                .apps
                .iter()
                .all(|a| a.presence.settled() && a.mag.settled() && a.bounce.settled() && a.slide.settled())
    }

    /// Redraws only the music widget, for the visualizer's bars: its patch of
    /// the island behind it and the widget, uploaded as that rect alone.
    /// False if there's no music widget on the last frame to redraw.
    fn render_bars(&mut self, gfx: &mut Gfx, wd: &WidgetData, pos: POINT, pw: i32, ph: i32) -> Result<bool> {
        let g = &self.geo;
        let Some(&(_, s)) = g.left.iter().chain(g.right.iter()).find(|(c, _)| *c == Chip::Music) else {
            return Ok(false);
        };
        let Some(&(x0, x1)) = g.islands.iter().find(|(x0, x1)| s.x >= *x0 && s.x + s.w <= *x1) else {
            return Ok(false);
        };
        let k = self.k;
        let bh = g.y1 - g.y0;
        let b = self.button();
        let by = g.y0 + (bh - b) / 2.0;
        let part = RECT {
            left: ((s.x * k).floor() as i32).max(0),
            top: ((by * k).floor() as i32).max(0),
            right: (((s.x + s.w) * k).ceil() as i32).min(pw),
            bottom: (((by + b) * k).ceil() as i32).min(ph),
        };
        if part.right <= part.left || part.bottom <= part.top {
            return Ok(false);
        }
        let glass = self.acrylic && self.reveal.value > theme::GLASS_FROM;
        gfx.begin_part(&mut self.layer, pw, ph, part, k)?;
        {
            let p = gfx.painter();
            let g = &self.geo;
            // The same island and hover as in a full frame, so the pixels match.
            let r = self.radius;
            theme::panel(&p, x0, g.y0, x1 - x0, bh, r, glass, self.opacity, 0.6 + 0.4 * self.opacity, 1.0);
            let pressed = self.pressed.map(|p| p.0);
            let hover = [Hot::Chip(Chip::Music), Hot::MusicToggle].into_iter().find_map(|h| {
                if pressed == Some(h) {
                    Some(Color::white(0.05))
                } else if self.hovered == Some(h) {
                    Some(Color::white(0.08))
                } else {
                    None
                }
            });
            if let Some(c) = hover {
                p.rrect(s.x, by, s.w, b, 5.0, c);
            }
            self.draw_music(&p, &gfx.fonts, s, g.y0 + bh / 2.0, wd);
        }
        let alpha = (self.reveal.value.clamp(0.0, 1.0) * 255.0) as u8;
        gfx.present_part(&self.layer, self.hwnd, pos, pw, ph, alpha, part)?;
        Ok(true)
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

    /// Screen x of the Start button's left edge.
    pub fn start_x(&self) -> i32 {
        self.screen.left + (self.geo.start.x * self.k) as i32
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
