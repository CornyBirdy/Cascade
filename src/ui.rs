//! The app core: windows, the message loop, the notch (state, layout, input,
//! drawing) and the glue to the dock and previews.
//!
//! Idle cost is zero: the thread blocks in GetMessage until something
//! happens (hover, media/volume/window event, timer). Frames are only
//! produced while a spring is moving, paced by a high-resolution timer.

use std::cell::RefCell;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, ENUM_DATE_FORMATS_FLAGS, TIME_NOSECONDS};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::{ABN_FULLSCREENAPP, ABN_POSCHANGED};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::audio::{self, Visualizer, Volume, BANDS};
use crate::brightness;
use crate::config::{self, Config};
use crate::backdrop::{self, Backdrop};
use crate::dock::{self, Dock, Hot as DockHot};
use crate::gfx::{Align, Color, Gfx, Layer, Painter};
use crate::icons::{self, IconResult};
use crate::keyhook;
use crate::launcher::{self, AppEntry, Launcher};
use crate::media::{self, Art, MediaCmd, MediaInfo};
use crate::msg;
use crate::pomodoro::Pomodoro;
use crate::preview::{self, Preview};
use crate::radios::{self, Kind, RadioCmd, Radios};
use crate::settings::{Change, Settings};
use crate::shell::{self, AppCmd, MenuCmd, MenuState, Tray};
use crate::systray::{self, Flyout, Systray};
use crate::helper;
use crate::spring::Spring;
use crate::update;
use crate::weather::{self, Weather};

/// Width of the concave flares joining the notch to the screen edge.
const EAR: f32 = 9.0;
const PEEK_MS: u32 = 1600;

// Notch timers.
const TIMER_EXPAND: usize = 1;
const TIMER_COLLAPSE: usize = 2;
const TIMER_PEEK: usize = 3;
const TIMER_TICK: usize = 4;
const TIMER_FLYOUT: usize = 5;
const TIMER_POMODORO: usize = 6;
// Dock timers (on the dock window).
const TIMER_SCAN: usize = 10;
const TIMER_DOCK_HIDE: usize = 11;
const TIMER_PREVIEW_SHOW: usize = 12;
const TIMER_PREVIEW_HIDE: usize = 13;
const TIMER_CLOCK: usize = 14;

/// Not exported by the `windows` crate features we enable.
const WM_MOUSELEAVE: u32 = 0x02A3;

const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;
const EVENT_OBJECT_CREATE: u32 = 0x8000;
const EVENT_OBJECT_DESTROY: u32 = 0x8001;
const EVENT_OBJECT_SHOW: u32 = 0x8002;
const EVENT_OBJECT_HIDE: u32 = 0x8003;
const EVENT_OBJECT_CLOAKED: u32 = 0x8017;
const EVENT_OBJECT_UNCLOAKED: u32 = 0x8018;

const BLACK: Color = Color(0.0, 0.0, 0.0, 1.0);
const ACCENT: Color = Color(0.27, 0.55, 1.0, 1.0);
const GREEN: Color = Color(0.40, 0.85, 0.50, 1.0);

#[derive(Clone, Copy, PartialEq, Debug)]
enum View {
    Idle,
    Live,
    Peek,
    Expanded,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Media,
    Controls,
    Calendar,
}

const MODES: [Mode; 3] = [Mode::Media, Mode::Controls, Mode::Calendar];

#[derive(Clone, Copy, PartialEq)]
enum PeekKind {
    Volume,
    Brightness,
    Timer,
    /// A short status line (`App::message`), e.g. update progress.
    Message,
}

/// Clickable things in the open notch.
#[derive(Clone, Copy, PartialEq)]
enum Hot {
    Previous,
    PlayPause,
    Next,
    Tab(usize),
    Wifi,
    Bluetooth,
    Notifications,
    VolumeBar,
    BrightnessBar,
    PomoToggle,
    PomoReset,
    PomoSkip,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app unless it is already borrowed further up the stack
/// (some Win32 calls re-enter the window procedure synchronously).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut app) => app.as_mut().map(f),
        Err(_) => None,
    })
}

struct App {
    hwnd: HWND,
    cfg: Config,
    gfx: Gfx,
    tray: Option<Tray>,
    taskbar_created: u32,

    monitor: RECT,
    /// Physical pixels per layout unit (DPI scale x config scale).
    k: f32,
    /// DPI scale alone (the dock ignores the notch's `scale`).
    dpi: f32,
    refresh_hz: u32,

    layer: Layer,
    w: Spring,
    h: Spring,
    r: Spring,
    fade: Spring,
    level: Spring,
    bars: [Spring; BANDS],
    last_frame: Instant,
    dirty: bool,

    view: View,
    mode: Mode,
    expanded: bool,
    expand_pending: bool,
    tracking: bool,
    hovered: Option<Hot>,
    dragging: Option<Hot>,
    peek: Option<PeekKind>,
    fullscreen: bool,
    shown: bool,
    last_wheel: Instant,

    media: Option<MediaInfo>,
    art: Option<(Arc<Art>, ID2D1Bitmap, u32)>,
    media_tx: Sender<MediaCmd>,
    volume: Option<Volume>,
    vol: (f32, bool),
    brightness: u32,
    brightness_ok: bool,
    brightness_tx: Sender<u32>,
    radios: Radios,
    radio_tx: Sender<RadioCmd>,
    pomo: Pomodoro,
    weather: Option<Weather>,
    visualizer: Visualizer,
    flyout_hidden: bool,

    /// Content origin and width of the last frame, for hit-testing.
    origin: f32,
    content_w: f32,

    dock: Dock,
    preview: Preview,
    hooks: Vec<HWINEVENTHOOK>,
    taskbar_hidden: bool,
    instance: HINSTANCE,
    launcher: Launcher,
    launcher_menu: bool,
    settings: Option<Settings>,
    /// Tray icons from apps, and the flyout for the hidden ones.
    systray: Systray,
    flyout: Flyout,
    /// Last rect given to the hidden tray window.
    tray_rect: RECT,

    message: String,
    message_color: Color,
    update_available: Option<u32>,
    updating: bool,
}

fn create_window(class: PCWSTR, instance: HINSTANCE, ex: WINDOW_EX_STYLE) -> Result<HWND> {
    unsafe { CreateWindowExW(ex, class, w!("Cascade"), WS_POPUP, 0, 0, 1, 1, None, None, Some(instance), None) }
}

pub fn run() -> Result<()> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("CascadeWindow");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let settings_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: LoadIconW(Some(instance), PCWSTR(std::ptr::without_provenance(1))).unwrap_or_default(),
            lpszClassName: w!("CascadeSettings"),
            ..Default::default()
        };
        RegisterClassExW(&settings_class);
        let layered = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let hwnd = create_window(class, instance, layered)?;
        let dock_hwnd = create_window(class, instance, layered)?;
        let trigger = create_window(class, instance, layered)?;
        let preview_hwnd = create_window(class, instance, WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE)?;
        let launcher_hwnd = create_window(class, instance, WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW)?;
        msg::set_target(hwnd);
        backdrop::register(instance);
        // Blur panels: up to three taskbar islands, plus the launcher.
        let dock_backdrops = (0..3).filter_map(|_| Backdrop::create(instance).ok()).collect();
        let launcher_backdrop = Backdrop::create(instance)?;
        let flyout_hwnd = create_window(class, instance, WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW)?;
        let flyout = (flyout_hwnd, Backdrop::create(instance)?);

        let app = App::new(
            hwnd,
            dock_hwnd,
            trigger,
            preview_hwnd,
            (launcher_hwnd, launcher_backdrop),
            dock_backdrops,
            flyout,
            instance,
        )?;
        APP.with(|c| *c.borrow_mut() = Some(app));
        with_app(|a| a.start());

        let timer =
            CreateWaitableTimerExW(None, PCWSTR::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0)
                .or_else(|_| CreateWaitableTimerW(None, false, PCWSTR::null()))?;

        let mut m = MSG::default();
        'run: loop {
            if with_app(|a| a.wants_frame()).unwrap_or(false) {
                while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
                    if m.message == WM_QUIT {
                        break 'run;
                    }
                    let _ = TranslateMessage(&m);
                    DispatchMessageW(&m);
                }
                let wait = with_app(|a| a.frame()).unwrap_or(Duration::ZERO);
                if !wait.is_zero() {
                    let due = -((wait.as_nanos() / 100) as i64);
                    if SetWaitableTimer(timer, &due, 0, None, None, false).is_ok() {
                        MsgWaitForMultipleObjects(Some(&[timer]), false, INFINITE, QS_ALLINPUT);
                    }
                }
            } else {
                if GetMessageW(&mut m, None, 0, 0).0 <= 0 {
                    break;
                }
                let _ = TranslateMessage(&m);
                DispatchMessageW(&m);
            }
        }
        let _ = CloseHandle(timer);
        APP.with(|c| c.borrow_mut().take());
    }
    Ok(())
}

unsafe extern "system" fn win_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
    // OBJID_WINDOW / CHILDID_SELF: the window itself, not a part of it.
    if object == 0 && child == 0 {
        with_app(|a| a.on_win_event(event, hwnd));
    }
}

enum RightClick {
    General,
    App { id: String, name: String, pinned: bool, windows: usize },
    LauncherApp { target: String, pinned: bool },
    Nothing,
}

fn right_click(hwnd: HWND) {
    let Some(target) = with_app(|a| a.right_click_target(hwnd)) else { return };
    match target {
        RightClick::General => {
            let Some(state) = with_app(|a| a.menu_state()) else { return };
            let cmd = shell::show_menu(hwnd, state);
            if cmd == MenuCmd::Quit {
                if let Some(main) = with_app(|a| a.hwnd) {
                    unsafe {
                        let _ = DestroyWindow(main);
                    }
                }
            } else {
                with_app(|a| a.on_menu(cmd));
            }
        }
        RightClick::App { id, name, pinned, windows } => {
            let cmd = shell::show_app_menu(hwnd, &name, pinned, windows);
            with_app(|a| a.on_app_cmd(&id, cmd));
        }
        RightClick::LauncherApp { target, pinned } => {
            with_app(|a| a.launcher_menu = true);
            let toggle = shell::show_pin_menu(hwnd, pinned);
            with_app(|a| {
                a.launcher_menu = false;
                if toggle {
                    a.dock.set_pinned(&target, !pinned);
                }
                if a.launcher.open {
                    launcher::force_foreground(a.launcher.hwnd);
                }
            });
        }
        RightClick::Nothing => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_MOUSEACTIVATE => {
            // Only the notch, dock and previews refuse focus.
            if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_NOACTIVATE.0 != 0 {
                return LRESULT(MA_NOACTIVATE as isize);
            }
        }
        msg::OPEN_LAUNCHER => {
            with_app(|a| a.toggle_launcher());
            return LRESULT(0);
        }
        msg::OPEN_SETTINGS => {
            with_app(|a| a.open_settings());
            return LRESULT(0);
        }
        msg::POWER_MENU => {
            let action = shell::show_power_menu(hwnd);
            with_app(|a| {
                a.launcher_menu = false;
                a.launcher.hide();
            });
            shell::power(action);
            return LRESULT(0);
        }
        msg::APPS => {
            let list = Box::from_raw(lp.0 as *mut Vec<AppEntry>);
            with_app(move |a| a.launcher.set_apps(*list));
            return LRESULT(0);
        }
        WM_RBUTTONUP => {
            right_click(hwnd);
            return LRESULT(0);
        }
        msg::TRAY => {
            let event = (lp.0 & 0xFFFF) as u32;
            if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
                right_click(hwnd);
            }
            return LRESULT(0);
        }
        // Boxed payloads: always reclaim the box, even if the app is busy.
        msg::MEDIA => {
            let info = Box::from_raw(lp.0 as *mut Option<MediaInfo>);
            with_app(move |a| a.on_media(*info));
            return LRESULT(0);
        }
        msg::ICON => {
            let icon = Box::from_raw(lp.0 as *mut IconResult);
            with_app(move |a| {
                a.dock.on_icon(*icon);
                a.launcher.dirty = true;
            });
            return LRESULT(0);
        }
        msg::TRAY_ICON => {
            let e = Box::from_raw(lp.0 as *mut systray::Event);
            with_app(move |a| {
                if a.systray.apply(*e) {
                    a.dock.dirty = true;
                    a.flyout.dirty = true;
                }
            });
            return LRESULT(0);
        }
        msg::WEATHER => {
            let w = Box::from_raw(lp.0 as *mut Weather);
            with_app(move |a| {
                a.weather = Some(*w);
                a.dirty = true;
                a.dock.dirty = true;
            });
            return LRESULT(0);
        }
        WM_PAINT => {
            if with_app(|a| a.paint(hwnd)).unwrap_or(false) {
                return LRESULT(0);
            }
        }
        WM_DESTROY => {
            if with_app(|a| a.hwnd == hwnd).unwrap_or(false) {
                with_app(|a| a.shutdown());
                PostQuitMessage(0);
            } else {
                with_app(|a| {
                    if a.settings.as_ref().is_some_and(|s| s.hwnd == hwnd) {
                        a.settings = None;
                    }
                });
            }
            return LRESULT(0);
        }
        WM_ENDSESSION => {
            if wp.0 != 0 {
                with_app(|a| a.shutdown());
            }
            return LRESULT(0);
        }
        _ => {}
    }
    match with_app(|a| a.handle(hwnd, m, wp, lp)).flatten() {
        Some(r) => r,
        None => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// What the settings window shows for `key`.
fn setting_value(cfg: &Config, key: &str) -> String {
    if key == "autostart" {
        shell::autostart_enabled().to_string()
    } else {
        cfg.get(key)
    }
}

fn lparam_xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn date_text(format: PCWSTR) -> String {
    let mut d = [0u16; 128];
    unsafe {
        let n = GetDateFormatEx(PCWSTR::null(), ENUM_DATE_FORMATS_FLAGS(0), None, format, Some(&mut d), PCWSTR::null());
        String::from_utf16_lossy(&d[..(n.max(1) - 1) as usize])
    }
}

fn clock() -> (String, String) {
    let mut t = [0u16; 64];
    unsafe {
        let n = GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, None, PCWSTR::null(), Some(&mut t));
        (String::from_utf16_lossy(&t[..(n.max(1) - 1) as usize]), date_text(w!("dddd d MMMM")))
    }
}

fn battery() -> Option<(u8, bool)> {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s).ok()? };
    if s.BatteryFlag & 128 != 0 || s.BatteryLifePercent > 100 {
        return None;
    }
    Some((s.BatteryLifePercent, s.ACLineStatus == 1))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

impl App {
    fn new(
        hwnd: HWND,
        dock_hwnd: HWND,
        trigger: HWND,
        preview_hwnd: HWND,
        (launcher_hwnd, launcher_backdrop): (HWND, Backdrop),
        dock_backdrops: Vec<Backdrop>,
        (flyout_hwnd, flyout_backdrop): (HWND, Backdrop),
        instance: HINSTANCE,
    ) -> Result<Self> {
        let cfg = Config::load();
        let gfx = Gfx::new()?;
        let volume = Volume::new().ok();
        let vol = volume.as_ref().and_then(|v| v.get()).unwrap_or((0.0, false));
        let media_tx = media::spawn();
        brightness::spawn_watcher();
        let brightness_tx = brightness::spawn_setter();
        let radio_tx = radios::spawn();
        if cfg.weather {
            weather::spawn(cfg.weather_location.clone(), cfg.fahrenheit);
        }
        shell::register_appbar(hwnd);
        let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        let size = Spring::new(0.0, 420.0, 32.0);
        let mut start_w = size;
        start_w.value = cfg.idle_width * 0.4;
        Ok(Self {
            hwnd,
            gfx,
            tray: Some(Tray::new(hwnd)),
            taskbar_created,
            monitor: RECT::default(),
            k: 1.0,
            dpi: 1.0,
            refresh_hz: 60,
            layer: Layer::default(),
            // Start small and flat so the notch grows in on launch.
            w: start_w,
            h: size,
            r: Spring::new(0.0, 420.0, 34.0),
            fade: Spring::new(0.0, 260.0, 30.0),
            level: Spring::new(vol.0, 500.0, 40.0),
            bars: [Spring::new(0.0, 700.0, 30.0); BANDS],
            last_frame: Instant::now(),
            dirty: true,
            view: View::Idle,
            mode: Mode::Media,
            expanded: false,
            expand_pending: false,
            tracking: false,
            hovered: None,
            dragging: None,
            peek: None,
            fullscreen: shell::fullscreen_now(),
            shown: false,
            last_wheel: Instant::now(),
            media: None,
            art: None,
            media_tx,
            volume,
            vol,
            brightness: 0,
            brightness_ok: false,
            brightness_tx,
            radios: Radios::default(),
            radio_tx,
            pomo: Pomodoro::new(cfg.focus_minutes, cfg.break_minutes),
            weather: None,
            visualizer: Visualizer::default(),
            flyout_hidden: false,
            origin: 0.0,
            content_w: 0.0,
            dock: Dock::new(dock_hwnd, trigger, icons::spawn(), dock_backdrops),
            preview: Preview::new(preview_hwnd),
            hooks: Vec::new(),
            taskbar_hidden: false,
            instance,
            launcher: Launcher::new(launcher_hwnd, launcher_backdrop),
            launcher_menu: false,
            settings: None,
            systray: Systray::default(),
            flyout: Flyout::new(flyout_hwnd, flyout_backdrop),
            tray_rect: RECT::default(),
            message: String::new(),
            message_color: ACCENT,
            update_available: None,
            updating: false,
            cfg,
        })
    }

    fn start(&mut self) {
        shell::ensure_shortcuts();
        launcher::spawn_scan();
        if self.cfg.update_check {
            update::spawn_checker(self.cfg.github_token.clone());
        }
        self.apply_taskbar();
        self.update_display();
        self.apply_flyout();
        self.apply_dock();
        self.retarget();
        if std::env::args().any(|a| a == "--settings") {
            self.open_settings();
        }
        if self.cfg.admin_helper {
            if std::env::args().any(|a| a == "--updated") {
                helper::restart();
            } else {
                helper::start();
            }
        }
    }

    fn shutdown(&mut self) {
        keyhook::set(false);
        self.settings = None;
        self.visualizer.set(false);
        weather::stop();
        for h in self.hooks.drain(..) {
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
        self.preview.hide();
        self.dock.shutdown();
        shell::unregister_appbar(self.hwnd);
        self.tray = None;
        if self.flyout_hidden {
            shell::set_flyout_hidden(false);
            self.flyout_hidden = false;
        }
        if let Some(h) = self.systray.hwnd.take() {
            systray::stop(h);
        }
        if self.taskbar_hidden {
            shell::restore_taskbar();
            self.taskbar_hidden = false;
        }
    }

    fn update_display(&mut self) {
        unsafe {
            let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            self.monitor = mi.rcMonitor;
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            self.dpi = dx as f32 / 96.0;
            self.k = self.dpi * self.cfg.scale;
            let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
            self.refresh_hz = if EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut dm).as_bool()
                && dm.dmDisplayFrequency > 1
            {
                dm.dmDisplayFrequency
            } else {
                60
            };
        }
        self.dirty = true;
    }

    fn apply_flyout(&mut self) {
        let want = self.cfg.notch && self.cfg.hide_windows_flyout && (self.cfg.volume_osd || self.cfg.brightness_osd);
        // Also run when `want` is false: that restores a flyout left hidden
        // by a previous run that did not exit cleanly.
        shell::set_flyout_hidden(want);
        self.flyout_hidden = want;
    }

    fn apply_taskbar(&mut self) {
        if self.cfg.dock && self.cfg.hide_taskbar {
            shell::hide_taskbar();
            self.taskbar_hidden = true;
        } else {
            // Also undoes a hide left behind by a crashed run.
            shell::restore_taskbar();
            self.taskbar_hidden = false;
        }
    }

    fn apply_dock(&mut self) {
        // The launcher also watches the foreground window, to take over when
        // Windows opens its own Start menu (see `on_win_event`).
        let hooks_wanted = self.cfg.dock || (self.cfg.launcher && self.cfg.launcher_win_key);
        if hooks_wanted && self.hooks.is_empty() {
            let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
            let ranges = [
                (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
                (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
                (EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE),
                (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
            ];
            for (lo, hi) in ranges {
                let h = unsafe { SetWinEventHook(lo, hi, None, Some(win_event), 0, 0, flags) };
                if !h.is_invalid() {
                    self.hooks.push(h);
                }
            }
        } else if !hooks_wanted {
            for h in self.hooks.drain(..) {
                unsafe {
                    let _ = UnhookWinEvent(h);
                }
            }
        }
        keyhook::set(self.cfg.launcher && self.cfg.launcher_win_key);
        if !self.cfg.launcher {
            self.launcher.hide();
        }
        self.dock.configure(&self.cfg, self.monitor, self.dpi);
        self.dock.set_visible(!(self.cfg.hide_in_fullscreen && self.fullscreen));
        self.launcher.configure(self.cfg.acrylic, self.cfg.taskbar_opacity, self.cfg.corner_radius());
        self.flyout.configure(self.cfg.acrylic, self.cfg.taskbar_opacity, self.cfg.corner_radius());
        let want_tray = self.cfg.dock && self.cfg.tray_icons;
        match (want_tray, self.systray.hwnd) {
            (true, None) => {
                self.systray.hwnd = systray::start(self.instance);
                self.tray_rect = RECT::default();
            }
            (false, Some(h)) => {
                systray::stop(h);
                self.systray = Systray::default();
                self.flyout.hide();
            }
            _ => {}
        }
        if self.cfg.dock {
            self.dock.scan();
        } else {
            self.preview.hide();
        }
        self.arm_clock();
    }

    // ------------------------------------------------------------ state --

    fn playing(&self) -> bool {
        self.media.as_ref().is_some_and(|m| m.playing)
    }

    fn compute_view(&self) -> View {
        if self.peek.is_some() && !self.expanded {
            View::Peek
        } else if self.expanded {
            View::Expanded
        } else if self.cfg.live_activity && (self.playing() || self.pomo.running()) {
            View::Live
        } else {
            View::Idle
        }
    }

    /// Width, height and corner radius of a view, in layout units.
    fn size_of(&self, view: View) -> (f32, f32, f32) {
        let (iw, ih) = (self.cfg.idle_width, self.cfg.idle_height);
        match view {
            View::Idle => (iw, ih, ih * 0.36),
            View::Live => (iw + 104.0, ih, ih * 0.42),
            View::Peek if self.peek == Some(PeekKind::Timer) => (iw + 170.0, ih + 12.0, (ih + 12.0) * 0.4),
            View::Peek if self.peek == Some(PeekKind::Message) => (iw + 190.0, ih + 12.0, (ih + 12.0) * 0.4),
            View::Peek => (iw + 110.0, ih + 12.0, (ih + 12.0) * 0.4),
            View::Expanded => match self.mode {
                Mode::Media if self.media.is_some() => (420.0, 166.0, 30.0),
                Mode::Media => (380.0, 120.0, 28.0),
                Mode::Controls => (420.0, 172.0, 30.0),
                Mode::Calendar => (420.0, 212.0, 30.0),
            },
        }
    }

    /// Re-derives the view and animation targets after any state change.
    fn retarget(&mut self) {
        let visible = self.cfg.notch && !(self.cfg.hide_in_fullscreen && self.fullscreen);
        if visible != self.shown {
            self.shown = visible;
            unsafe {
                let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
            if !visible {
                self.expanded = false;
                self.peek = None;
                self.layer.release();
            }
        }

        let view = self.compute_view();
        if view != self.view {
            self.view = view;
            self.fade.snap(0.0);
            self.fade.set(1.0);
            unsafe {
                if view == View::Expanded {
                    SetTimer(Some(self.hwnd), TIMER_TICK, 1000, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
                }
            }
        }
        let (w, h, r) = self.size_of(view);
        self.w.set(w);
        self.h.set(h);
        self.r.set(r);

        let bars_visible =
            matches!(view, View::Live | View::Expanded) && (view == View::Live || self.mode == Mode::Media);
        let visualize = self.shown && self.cfg.visualizer && self.playing() && bars_visible;
        self.visualizer.set(visualize);
        if !visualize {
            for b in &mut self.bars {
                b.set(0.0);
            }
        }
        self.dirty = true;
    }

    fn set_mode(&mut self, mode: Mode) {
        if mode != self.mode {
            self.mode = mode;
            self.hovered = None;
            self.fade.snap(0.0);
            self.fade.set(1.0);
            self.retarget();
        }
    }

    fn show_peek(&mut self, kind: PeekKind) {
        if !self.shown {
            return;
        }
        let value = match kind {
            PeekKind::Volume => self.vol.0,
            PeekKind::Brightness => self.brightness as f32 / 100.0,
            PeekKind::Timer | PeekKind::Message => 0.0,
        };
        if self.peek != Some(kind) {
            self.level.snap(value);
        }
        self.level.set(value);
        self.peek = Some(kind);
        unsafe {
            let ms = match kind {
                PeekKind::Timer | PeekKind::Message => 4000,
                _ => PEEK_MS,
            };
            SetTimer(Some(self.hwnd), TIMER_PEEK, ms, None);
            if self.flyout_hidden && matches!(kind, PeekKind::Volume | PeekKind::Brightness) {
                // The flyout host can be created lazily on first use.
                shell::set_flyout_hidden(true);
                SetTimer(Some(self.hwnd), TIMER_FLYOUT, 60, None);
            }
        }
        self.retarget();
    }

    // ----------------------------------------------------------- events --

    fn handle(&mut self, hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        if self.settings.as_ref().is_some_and(|s| s.hwnd == hwnd) {
            return self.handle_settings(m, wp, lp);
        }
        if hwnd == self.launcher.hwnd {
            return self.handle_launcher(m, wp, lp);
        }
        if hwnd == self.flyout.hwnd {
            return self.handle_flyout(m, wp, lp);
        }
        if m == msg::APPBAR {
            match wp.0 as u32 {
                ABN_FULLSCREENAPP => {
                    self.fullscreen = lp.0 != 0;
                    self.retarget();
                    self.dock.set_visible(!(self.cfg.hide_in_fullscreen && self.fullscreen));
                    if self.fullscreen {
                        self.preview.hide();
                    }
                }
                ABN_POSCHANGED if hwnd == self.dock.hwnd => self.dock.reposition(),
                _ => {}
            }
            return Some(LRESULT(0));
        }
        if m == self.taskbar_created && m != 0 {
            if let Some(t) = &self.tray {
                t.add();
            }
            shell::register_appbar(self.hwnd);
            if self.taskbar_hidden {
                shell::hide_taskbar();
            }
            self.dock.configure(&self.cfg, self.monitor, self.dpi);
            return Some(LRESULT(0));
        }
        match m {
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                self.update_display();
                self.dock.configure(&self.cfg, self.monitor, self.dpi);
                self.retarget();
                return Some(LRESULT(0));
            }
            WM_SETTINGCHANGE if wp.0 as u32 == SPI_SETWORKAREA.0 => {
                self.dock.refresh_bottom();
                return None;
            }
            _ => {}
        }
        if self.dock.is(hwnd) {
            return self.handle_dock(hwnd, m, wp, lp);
        }
        if hwnd == self.preview.hwnd {
            return self.handle_preview(m, lp);
        }
        self.handle_notch(m, wp, lp)
    }

    fn handle_notch(&mut self, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match m {
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lp);
                self.on_mouse_move(x, y);
            }
            WM_MOUSELEAVE => self.on_mouse_leave(),
            WM_LBUTTONDOWN => {
                let (x, y) = lparam_xy(lp);
                self.on_press(x, y);
            }
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lp);
                self.on_click(x, y);
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16;
                self.on_wheel(delta);
            }
            WM_TIMER => self.on_timer(wp.0),
            msg::VOLUME => {
                self.vol = (wp.0 as f32 / 1000.0, lp.0 != 0);
                self.dock.dirty = true;
                let in_controls = self.expanded && self.mode == Mode::Controls;
                if in_controls {
                    self.dirty = true;
                } else if self.cfg.volume_osd {
                    self.show_peek(PeekKind::Volume);
                }
            }
            msg::BRIGHTNESS => {
                self.brightness = wp.0 as u32;
                self.brightness_ok = true;
                let in_controls = self.expanded && self.mode == Mode::Controls;
                if in_controls {
                    self.dirty = true;
                } else if self.cfg.brightness_osd {
                    self.show_peek(PeekKind::Brightness);
                }
            }
            msg::BRIGHTNESS_LEVEL => {
                self.brightness = wp.0 as u32;
                self.brightness_ok = lp.0 != 0;
                self.dirty = true;
            }
            msg::UPDATE => self.on_update(wp.0, lp.0 as u32),
            msg::RADIOS => {
                self.radios = Radios::unpack(wp.0);
                self.dirty = true;
                self.dock.dirty = true;
            }
            msg::SPECTRUM => {
                if self.visualizer.running() {
                    for (b, v) in self.bars.iter_mut().zip(audio::unpack(wp.0)) {
                        b.set(v);
                    }
                }
            }
            msg::AUDIO_DEVICE => {
                if let Some(v) = &mut self.volume {
                    v.rebind();
                    if let Some(state) = v.get() {
                        self.vol = state;
                    }
                }
                self.visualizer.restart();
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }

    /// Shows a status line in the notch, or a message box if the notch is off.
    fn notify(&mut self, text: String, color: Color, important: bool) {
        if self.shown {
            self.message = text;
            self.message_color = color;
            self.show_peek(PeekKind::Message);
        } else if important {
            std::thread::spawn(move || unsafe {
                MessageBoxW(None, &windows::core::HSTRING::from(text), w!("Cascade"), MB_OK | MB_ICONINFORMATION);
            });
        }
    }

    fn on_update(&mut self, outcome: usize, build: u32) {
        match outcome {
            update::AVAILABLE => {
                if self.update_available != Some(build) {
                    self.update_available = Some(build);
                    self.notify(
                        format!("Update available (build {build}) \u{2014} right-click to install"),
                        ACCENT,
                        false,
                    );
                }
            }
            update::UP_TO_DATE => {
                self.updating = false;
                self.notify(format!("Cascade is up to date (build {build})"), GREEN, true);
            }
            update::INSTALLING => self.notify(format!("Downloading build {build}\u{2026}"), ACCENT, false),
            update::INSTALLED => {
                self.notify("Restarting into the new version\u{2026}".into(), GREEN, false);
                // Quit outside this borrow so shutdown can run normally.
                unsafe {
                    let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            update::NO_ACCESS => {
                self.updating = false;
                self.notify(
                    "Can't reach the GitHub repo. If it is private, add a github_token in the config file.".into(),
                    Color(1.0, 0.45, 0.4, 1.0),
                    true,
                );
            }
            _ => {
                self.updating = false;
                self.notify("Update failed \u{2014} try again later".into(), Color(1.0, 0.45, 0.4, 1.0), true);
            }
        }
    }

    fn on_media(&mut self, info: Option<MediaInfo>) {
        self.media = info;
        self.retarget();
    }

    fn on_mouse_move(&mut self, x: i32, y: i32) {
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
            let _ = KillTimer(Some(self.hwnd), TIMER_COLLAPSE);
            if !self.expanded && !self.expand_pending {
                self.expand_pending = true;
                SetTimer(Some(self.hwnd), TIMER_EXPAND, self.cfg.expand_delay_ms.max(1), None);
            }
        }
        if let Some(bar) = self.dragging {
            self.drag_to(bar, x);
            return;
        }
        let hit = self.hit(x, y);
        if hit != self.hovered {
            self.hovered = hit;
            self.dirty = true;
        }
    }

    fn on_mouse_leave(&mut self) {
        self.tracking = false;
        if self.dragging.is_some() {
            return;
        }
        self.expand_pending = false;
        self.hovered = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_EXPAND);
            if self.expanded {
                SetTimer(Some(self.hwnd), TIMER_COLLAPSE, self.cfg.collapse_delay_ms.max(1), None);
            }
        }
        self.dirty = true;
    }

    fn on_press(&mut self, x: i32, y: i32) {
        if !self.expanded {
            return;
        }
        if let Some(bar @ (Hot::VolumeBar | Hot::BrightnessBar)) = self.hit(x, y) {
            if bar == Hot::BrightnessBar && !self.brightness_ok {
                return;
            }
            self.dragging = Some(bar);
            unsafe {
                SetCapture(self.hwnd);
            }
            self.drag_to(bar, x);
        }
    }

    /// Slider value for a client-area x position.
    fn slider_value(&self, x: i32) -> f32 {
        let (x0, x1) = (50.0, self.content_w - 60.0);
        ((x as f32 / self.k - self.origin - x0) / (x1 - x0)).clamp(0.0, 1.0)
    }

    fn drag_to(&mut self, bar: Hot, x: i32) {
        let v = self.slider_value(x);
        match bar {
            Hot::VolumeBar => {
                if let Some(vol) = &self.volume {
                    vol.set(v);
                }
                self.vol.0 = v;
                if v > 0.0 {
                    self.vol.1 = false;
                }
            }
            Hot::BrightnessBar => {
                let b = (v * 100.0).round() as u32;
                if b != self.brightness {
                    self.brightness = b;
                    let _ = self.brightness_tx.send(b);
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn on_click(&mut self, x: i32, y: i32) {
        if self.dragging.take().is_some() {
            unsafe {
                let _ = ReleaseCapture();
            }
            return;
        }
        if !self.expanded {
            self.expanded = true;
            self.expand_pending = false;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_EXPAND);
            }
            self.retarget();
            return;
        }
        let Some(hot) = self.hit(x, y) else { return };
        match hot {
            Hot::Tab(i) => self.set_mode(MODES[i]),
            Hot::Previous => {
                let _ = self.media_tx.send(MediaCmd::Previous);
            }
            Hot::Next => {
                let _ = self.media_tx.send(MediaCmd::Next);
            }
            Hot::PlayPause => {
                // Flip right away; the real state follows from the player.
                if let Some(m) = &mut self.media {
                    m.position = m.position_now();
                    m.stamp = Instant::now();
                    m.playing = !m.playing;
                }
                let _ = self.media_tx.send(MediaCmd::PlayPause);
            }
            Hot::Wifi | Hot::Bluetooth => {
                let (kind, slot) = if hot == Hot::Wifi {
                    (Kind::Wifi, &mut self.radios.wifi)
                } else {
                    (Kind::Bluetooth, &mut self.radios.bluetooth)
                };
                if let Some(on) = slot {
                    *on = !*on;
                    let _ = self.radio_tx.send(RadioCmd::Toggle(kind));
                }
            }
            Hot::Notifications => {
                shell::open("ms-actioncenter:");
                self.expanded = false;
            }
            Hot::PomoToggle => {
                self.pomo.toggle();
                unsafe {
                    if self.pomo.running() {
                        SetTimer(Some(self.hwnd), TIMER_POMODORO, 1000, None);
                    } else {
                        let _ = KillTimer(Some(self.hwnd), TIMER_POMODORO);
                    }
                }
            }
            Hot::PomoReset => {
                self.pomo.reset();
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_POMODORO);
                }
            }
            Hot::PomoSkip => {
                self.pomo.skip();
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_POMODORO);
                }
            }
            Hot::VolumeBar | Hot::BrightnessBar => {}
        }
        self.retarget();
    }

    fn on_wheel(&mut self, delta: i16) {
        if self.expanded {
            // Scroll flips between notch modes.
            if self.last_wheel.elapsed() < Duration::from_millis(220) {
                return;
            }
            self.last_wheel = Instant::now();
            let i = MODES.iter().position(|&m| m == self.mode).unwrap_or(0) as i32;
            let next = (i + if delta < 0 { 1 } else { -1 }).rem_euclid(MODES.len() as i32);
            self.set_mode(MODES[next as usize]);
        } else if self.cfg.scroll_volume {
            if let Some(v) = &self.volume {
                v.nudge(if delta > 0 { 0.02 } else { -0.02 });
            }
        }
    }

    fn on_timer(&mut self, id: usize) {
        unsafe {
            if id != TIMER_TICK && id != TIMER_POMODORO {
                let _ = KillTimer(Some(self.hwnd), id);
            }
        }
        match id {
            TIMER_EXPAND => {
                self.expand_pending = false;
                self.expanded = true;
                self.retarget();
            }
            TIMER_COLLAPSE => {
                self.expanded = false;
                self.hovered = None;
                self.mode = Mode::Media;
                self.retarget();
            }
            TIMER_PEEK => {
                self.peek = None;
                self.retarget();
            }
            TIMER_TICK => self.dirty = true,
            TIMER_FLYOUT => shell::set_flyout_hidden(self.flyout_hidden),
            TIMER_POMODORO => {
                if self.pomo.tick() {
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), TIMER_POMODORO);
                        let _ = MessageBeep(MB_ICONASTERISK);
                    }
                    self.show_peek(PeekKind::Timer);
                }
                if self.view == View::Live || (self.view == View::Expanded && self.mode == Mode::Calendar) {
                    self.dirty = true;
                }
            }
            _ => {}
        }
    }

    fn on_win_event(&mut self, event: u32, hwnd: HWND) {
        if self.taskbar_hidden && event == EVENT_OBJECT_SHOW && shell::is_taskbar(hwnd) {
            shell::reassert_taskbar_hidden();
            return;
        }
        // Windows opened its own Start menu: the Win key reached it without
        // passing our hook (an elevated app such as Task Manager was in
        // front), or its Start button was clicked. Show ours instead; taking
        // the foreground closes theirs.
        if event == EVENT_SYSTEM_FOREGROUND
            && self.cfg.launcher
            && self.cfg.launcher_win_key
            && !self.launcher.open
            && shell::is_start_menu(hwnd)
        {
            self.open_launcher();
            return;
        }
        if !self.cfg.dock {
            return;
        }
        if event == EVENT_SYSTEM_FOREGROUND {
            if let Some(h) = self.systray.hwnd {
                systray::put_first(h);
            }
        }
        let rescan = match event {
            EVENT_SYSTEM_FOREGROUND => {
                self.dock.foreground = hwnd;
                self.dock.dirty = true;
                !self.dock.tracks(hwnd)
            }
            EVENT_OBJECT_DESTROY => self.dock.tracks(hwnd),
            _ => unsafe { GetAncestor(hwnd, GA_PARENT) == GetDesktopWindow() },
        };
        if rescan {
            unsafe {
                SetTimer(Some(self.dock.hwnd), TIMER_SCAN, 120, None);
            }
        }
    }

    fn right_click_target(&mut self, hwnd: HWND) -> RightClick {
        if hwnd == self.flyout.hwnd {
            if let Some(i) = self.flyout.hovered() {
                self.systray.click(i, true);
            }
            return RightClick::Nothing;
        }
        if hwnd == self.dock.hwnd {
            if let Some(dock::Hot::TrayIcon(i)) = self.dock.hovered {
                self.systray.click(i, true);
                return RightClick::Nothing;
            }
        }
        if hwnd == self.launcher.hwnd {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
                let _ = ScreenToClient(hwnd, &mut p);
            }
            return match self.launcher.target_at(p.x, p.y).and_then(|t| self.launcher.app_target(t)) {
                Some(target) => RightClick::LauncherApp { pinned: self.dock.is_pinned(&target), target },
                None => RightClick::Nothing,
            };
        }
        if self.settings.as_ref().is_some_and(|s| s.hwnd == hwnd) {
            return RightClick::Nothing;
        }
        if hwnd == self.dock.hwnd {
            if let Some(i) = self.dock.hovered_app() {
                let a = &self.dock.apps[i];
                return RightClick::App {
                    id: a.id.clone(),
                    name: self.dock.name(i),
                    pinned: a.pinned,
                    windows: a.windows.len(),
                };
            }
        }
        RightClick::General
    }

    fn menu_state(&self) -> MenuState {
        let update_label = match self.update_available {
            _ if self.updating => "Updating\u{2026}".to_string(),
            Some(b) => format!("Update Cascade (build {b} available)"),
            None if update::current() > 0 => format!("Update Cascade (on build {})", update::current()),
            None => "Update Cascade".to_string(),
        };
        MenuState {
            update_label,
            hide_taskbar_allowed: self.cfg.dock,
            notch: self.cfg.notch,
            dock: self.cfg.dock,
            dock_autohide: self.cfg.dock_autohide,
            hide_taskbar: self.cfg.hide_taskbar,
            autostart: shell::autostart_enabled(),
            flyout_hidden: self.cfg.hide_windows_flyout,
        }
    }

    fn on_menu(&mut self, cmd: MenuCmd) {
        let flip = |slot: &mut bool, key: &str| {
            *slot = !*slot;
            Config::set_bool(key, *slot);
        };
        match cmd {
            MenuCmd::Notch => {
                flip(&mut self.cfg.notch, "notch");
                self.apply_flyout();
                self.retarget();
            }
            MenuCmd::Dock => {
                flip(&mut self.cfg.dock, "dock");
                self.apply_taskbar();
                self.apply_dock();
            }
            MenuCmd::DockAutohide => {
                flip(&mut self.cfg.dock_autohide, "dock_autohide");
                self.apply_dock();
            }
            MenuCmd::HideTaskbar => {
                flip(&mut self.cfg.hide_taskbar, "hide_taskbar");
                self.apply_taskbar();
                self.apply_dock();
            }
            MenuCmd::Autostart => shell::set_autostart(!shell::autostart_enabled()),
            MenuCmd::Flyout => {
                flip(&mut self.cfg.hide_windows_flyout, "hide_windows_flyout");
                self.apply_flyout();
            }
            MenuCmd::OpenConfig => shell::open_file(&config::path()),
            MenuCmd::Update => {
                if !self.updating {
                    self.updating = true;
                    self.notify("Checking for updates\u{2026}".into(), ACCENT, false);
                    update::spawn_install(self.cfg.github_token.clone());
                }
            }
            MenuCmd::Settings => self.open_settings(),
            MenuCmd::Launcher => self.toggle_launcher(),
            MenuCmd::Reload => self.reload_config(),
            MenuCmd::None | MenuCmd::Quit => {}
        }
        if let Some(s) = &self.settings {
            s.invalidate();
        }
    }

    /// Re-reads config.ini and applies everything that may have changed.
    fn reload_config(&mut self) {
        let old = self.cfg.clone();
        self.cfg = Config::load();
        if self.cfg.weather != old.weather
            || self.cfg.weather_location != old.weather_location
            || self.cfg.fahrenheit != old.fahrenheit
        {
            if self.cfg.weather {
                weather::spawn(self.cfg.weather_location.clone(), self.cfg.fahrenheit);
            } else {
                weather::stop();
                self.weather = None;
            }
        }
        self.pomo.configure(self.cfg.focus_minutes, self.cfg.break_minutes);
        self.update_display();
        self.apply_taskbar();
        self.apply_flyout();
        self.apply_dock();
        self.retarget();
        if let Some(s) = &self.settings {
            s.invalidate();
        }
    }

    // --------------------------------------------------- launcher glue --

    fn toggle_launcher(&mut self) {
        if self.launcher.open {
            self.launcher.hide();
        } else {
            self.open_launcher();
        }
    }

    fn open_launcher(&mut self) {
        if !self.cfg.launcher {
            return;
        }
        self.preview.hide();
        self.launcher.show(self.dpi, self.dock.pinned_apps());
    }

    fn launcher_bottom(&self) -> i32 {
        if self.dock.shown {
            self.dock.panel_top()
        } else {
            shell::work_area().bottom
        }
    }

    fn launcher_action(&mut self, action: launcher::Action) {
        match action {
            launcher::Action::Launch(target) => {
                shell::open(&target);
                self.launcher.hide();
            }
            launcher::Action::Close => self.launcher.hide(),
            launcher::Action::Settings => {
                self.launcher.hide();
                self.open_settings();
            }
            launcher::Action::Power => unsafe {
                // Menus run outside the app borrow, so post to ourselves.
                self.launcher_menu = true;
                let _ = PostMessageW(Some(self.launcher.hwnd), msg::POWER_MENU, WPARAM(0), LPARAM(0));
            },
        }
    }

    fn handle_launcher(&mut self, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match m {
            WM_ACTIVATE => {
                if wp.0 & 0xFFFF == 0 && !self.launcher_menu {
                    self.launcher.hide();
                }
                return None;
            }
            WM_CHAR => {
                if let Some(c) = char::from_u32(wp.0 as u32) {
                    self.launcher.on_char(c);
                }
            }
            WM_KEYDOWN => {
                if let Some(a) = self.launcher.on_key(wp.0 as u16) {
                    self.launcher_action(a);
                }
            }
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lp);
                self.launcher.on_move(x, y);
            }
            WM_MOUSELEAVE => self.launcher.on_leave(),
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lp);
                if let Some(a) = self.launcher.on_click(x, y) {
                    self.launcher_action(a);
                }
            }
            WM_MOUSEWHEEL => self.launcher.on_wheel(((wp.0 >> 16) & 0xFFFF) as u16 as i16),
            _ => return None,
        }
        Some(LRESULT(0))
    }

    // --------------------------------------------------- settings glue --

    fn open_settings(&mut self) {
        if let Some(s) = &self.settings {
            unsafe {
                let _ = ShowWindow(s.hwnd, SW_RESTORE);
            }
            launcher::force_foreground(s.hwnd);
            return;
        }
        if let Ok(mut s) = Settings::create(self.instance) {
            let cfg = &self.cfg;
            s.sync_edits(&|k: &str| setting_value(cfg, k));
            self.settings = Some(s);
        }
    }

    fn apply_change(&mut self, change: Change) {
        match change {
            Change::Set("autostart", v) => shell::set_autostart(v == "true"),
            Change::Set("admin_helper", v) => {
                let on = v == "true";
                if on != self.cfg.admin_helper {
                    Config::set_bool("admin_helper", on);
                    self.cfg.admin_helper = on;
                    if on {
                        helper::install();
                    } else {
                        helper::uninstall();
                    }
                }
            }
            Change::Set(key, v) => {
                if self.cfg.get(key) != v {
                    Config::set(key, &v);
                    self.reload_config();
                }
            }
            Change::Action("update_now") => self.on_menu(MenuCmd::Update),
            Change::Action("open_config") => shell::open_file(&config::path()),
            Change::Action("open_folder") => shell::open_file(&config::dir()),
            Change::Action(_) => {}
        }
        if let Some(s) = &self.settings {
            s.invalidate();
        }
    }

    fn handle_settings(&mut self, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let cfg = &self.cfg;
        let get = |k: &str| setting_value(cfg, k);
        let s = self.settings.as_mut()?;
        let change = match m {
            WM_ERASEBKGND => return Some(LRESULT(1)),
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lp);
                s.on_move(x, y, &get);
                None
            }
            WM_MOUSELEAVE => {
                s.on_leave();
                None
            }
            WM_LBUTTONDOWN => {
                // Clicking anywhere commits text boxes and takes focus from them.
                let pending = s.commit_all();
                unsafe {
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(s.hwnd));
                }
                let (x, y) = lparam_xy(lp);
                let change = s.on_down(x, y, &get);
                for p in pending {
                    self.apply_change(p);
                }
                change
            }
            WM_LBUTTONUP => s.on_up(),
            WM_MOUSEWHEEL => {
                s.on_wheel(((wp.0 >> 16) & 0xFFFF) as u16 as i16, &get);
                None
            }
            WM_CTLCOLOREDIT => unsafe {
                let hdc = HDC(wp.0 as *mut _);
                SetTextColor(hdc, COLORREF(0x00F0F0F0));
                SetBkColor(hdc, COLORREF(0x002B2B2B));
                return Some(LRESULT(s.brush.0 as isize));
            },
            WM_COMMAND if (wp.0 >> 16) & 0xFFFF == 0x0200 => s.commit_edit(HWND(lp.0 as *mut _)),
            WM_DPICHANGED => {
                let suggested = unsafe { *(lp.0 as *const RECT) };
                s.apply_dpi(Some(suggested));
                s.sync_edits(&get);
                s.invalidate();
                return Some(LRESULT(0));
            }
            WM_CLOSE => {
                // Save any text box that still has focus, then let it close.
                let changes = s.commit_all();
                for c in changes {
                    self.apply_change(c);
                }
                return None;
            }
            _ => return None,
        };
        if let Some(c) = change {
            self.apply_change(c);
        }
        Some(LRESULT(0))
    }

    fn on_app_cmd(&mut self, id: &str, cmd: AppCmd) {
        let Some(i) = self.dock.apps.iter().position(|a| a.id == id) else { return };
        match cmd {
            AppCmd::NewWindow => self.dock.launch_new(i),
            AppCmd::TogglePin => self.dock.toggle_pin(i),
            AppCmd::CloseAll => {
                self.dock.close_all(i);
                self.preview.hide();
            }
            AppCmd::None => {}
        }
    }

    // ------------------------------------------------------- dock glue --

    fn handle_dock(&mut self, hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let dock = self.dock.hwnd;
        unsafe {
            match m {
                WM_MOUSEMOVE if hwnd == self.dock.trigger => {
                    self.dock.show_now();
                    SetTimer(Some(dock), TIMER_DOCK_HIDE, 1500, None);
                }
                WM_MOUSEMOVE => {
                    let _ = KillTimer(Some(dock), TIMER_DOCK_HIDE);
                    let _ = KillTimer(Some(dock), TIMER_PREVIEW_HIDE);
                    let (x, y) = lparam_xy(lp);
                    if self.dock.on_move(x, y) {
                        self.schedule_preview();
                    }
                }
                WM_MOUSELEAVE => {
                    self.dock.on_leave();
                    SetTimer(Some(dock), TIMER_DOCK_HIDE, 600, None);
                    if self.preview.visible() {
                        SetTimer(Some(dock), TIMER_PREVIEW_HIDE, 300, None);
                    }
                    let _ = KillTimer(Some(dock), TIMER_PREVIEW_SHOW);
                }
                WM_LBUTTONDOWN => self.dock.on_down(lparam_xy(lp).0),
                WM_LBUTTONUP => {
                    if let Some(hot) = self.dock.on_up() {
                        self.preview.hide();
                        self.on_dock_click(hot);
                    }
                }
                WM_MBUTTONUP => {
                    if let Some(i) = self.dock.hovered_app() {
                        self.dock.launch_new(i);
                    }
                }
                WM_TIMER => {
                    let _ = KillTimer(Some(dock), wp.0);
                    match wp.0 {
                        TIMER_SCAN => {
                            if self.systray.prune() {
                                self.dock.dirty = true;
                            }
                            self.dock.scan();
                            self.refresh_preview();
                        }
                        TIMER_DOCK_HIDE => {
                            if self.preview.visible() {
                                SetTimer(Some(dock), TIMER_DOCK_HIDE, 600, None);
                            } else {
                                self.dock.hide_if_idle();
                            }
                        }
                        TIMER_PREVIEW_SHOW => self.show_preview(),
                        TIMER_PREVIEW_HIDE => self.preview.hide(),
                        TIMER_CLOCK => {
                            self.dock.dirty = true;
                            self.arm_clock();
                        }
                        _ => {}
                    }
                }
                _ => return None,
            }
        }
        Some(LRESULT(0))
    }

    fn on_dock_click(&mut self, hot: DockHot) {
        match hot {
            DockHot::Start | DockHot::Search => {
                if self.cfg.launcher {
                    self.toggle_launcher();
                } else {
                    shell::open_start_menu();
                }
            }
            DockHot::App(i) => self.dock.activate(i),
            DockHot::Weather => {
                let q = if self.cfg.weather_location.trim().is_empty() {
                    "weather".to_string()
                } else {
                    format!("weather {}", self.cfg.weather_location.trim())
                };
                shell::open(&format!("https://www.bing.com/search?q={}", q.replace(' ', "+")));
            }
            DockHot::Tray => self.toggle_flyout(),
            DockHot::TrayIcon(i) => self.systray.click(i, false),
            DockHot::Status => shell::open("ms-actioncenter:controlcenter/&showFooter=true"),
            DockHot::Clock => shell::open("ms-actioncenter:"),
        }
    }

    /// Redraws the taskbar clock at the start of each minute (and not more).
    fn arm_clock(&mut self) {
        unsafe {
            if self.cfg.dock && (self.cfg.show_clock || self.cfg.show_status) {
                let t = GetLocalTime();
                let ms = (60 - t.wSecond as u32) * 1000 - t.wMilliseconds as u32 + 20;
                SetTimer(Some(self.dock.hwnd), TIMER_CLOCK, ms.max(100), None);
            } else {
                let _ = KillTimer(Some(self.dock.hwnd), TIMER_CLOCK);
            }
        }
    }

    /// The hidden tray icons, above the ^ button.
    fn toggle_flyout(&mut self) {
        if self.flyout.open {
            self.flyout.hide();
            return;
        }
        let items = self.systray.overflow();
        if items.is_empty() {
            return;
        }
        self.preview.hide();
        let bottom = self.dock.panel_top() - (8.0 * self.dpi) as i32;
        self.flyout.show(items, self.dock.tray_button_x(), bottom, self.dpi);
    }

    fn handle_flyout(&mut self, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match m {
            WM_ACTIVATE => {
                // Clicking anywhere else (or an app opening its menu) closes it.
                if wp.0 & 0xFFFF == 0 {
                    self.flyout.hide();
                }
                return None;
            }
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lp);
                self.flyout.on_move(x, y);
            }
            WM_MOUSELEAVE => self.flyout.on_leave(),
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lp);
                self.flyout.on_move(x, y);
                if let Some(i) = self.flyout.hovered() {
                    self.systray.click(i, false);
                }
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }

    /// Opens (after a short hover) or retargets the preview for the hovered app.
    fn schedule_preview(&mut self) {
        let dock = self.dock.hwnd;
        let has_windows = self.dock.hovered_app().is_some_and(|i| !self.dock.apps[i].windows.is_empty());
        unsafe {
            if has_windows {
                let delay = if self.preview.visible() { 1 } else { 450 };
                SetTimer(Some(dock), TIMER_PREVIEW_SHOW, delay, None);
            } else {
                let _ = KillTimer(Some(dock), TIMER_PREVIEW_SHOW);
                if self.preview.visible() {
                    SetTimer(Some(dock), TIMER_PREVIEW_HIDE, 250, None);
                }
            }
        }
    }

    fn show_preview(&mut self) {
        let Some(i) = self.dock.hovered_app() else { return };
        let app = &self.dock.apps[i];
        if app.windows.is_empty() {
            return;
        }
        let (id, windows) = (app.id.clone(), app.windows.clone());
        let x = self.dock.app_screen_x(i);
        let bottom = self.dock.panel_top() - (6.0 * self.dpi) as i32;
        self.preview.show(&id, &windows, x, bottom, self.monitor, self.dpi);
    }

    /// After a rescan, keeps an open preview in sync with the app's windows.
    fn refresh_preview(&mut self) {
        let Some(id) = self.preview.app.clone() else { return };
        match self.dock.apps.iter().position(|a| a.id == id) {
            Some(i) if !self.dock.apps[i].windows.is_empty() => {
                let windows = self.dock.apps[i].windows.clone();
                let x = self.dock.app_screen_x(i);
                let bottom = self.dock.panel_top() - (6.0 * self.dpi) as i32;
                self.preview.show(&id, &windows, x, bottom, self.monitor, self.dpi);
            }
            _ => self.preview.hide(),
        }
    }

    fn handle_preview(&mut self, m: u32, lp: LPARAM) -> Option<LRESULT> {
        let dock = self.dock.hwnd;
        unsafe {
            match m {
                WM_MOUSEMOVE => {
                    let _ = KillTimer(Some(dock), TIMER_PREVIEW_HIDE);
                    let _ = KillTimer(Some(dock), TIMER_DOCK_HIDE);
                    let (x, y) = lparam_xy(lp);
                    self.preview.on_move(x, y);
                }
                WM_MOUSELEAVE => {
                    self.preview.on_leave();
                    SetTimer(Some(dock), TIMER_PREVIEW_HIDE, 300, None);
                    SetTimer(Some(dock), TIMER_DOCK_HIDE, 700, None);
                }
                WM_LBUTTONUP => {
                    let (x, y) = lparam_xy(lp);
                    match self.preview.on_click(x, y) {
                        Some(preview::Action::Focus(w)) => {
                            self.preview.hide();
                            dock::focus_window(w);
                        }
                        Some(preview::Action::Close(w)) => {
                            let _ = PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0));
                            SetTimer(Some(dock), TIMER_SCAN, 300, None);
                        }
                        None => {}
                    }
                }
                _ => return None,
            }
        }
        Some(LRESULT(0))
    }

    fn paint(&mut self, hwnd: HWND) -> bool {
        if hwnd == self.preview.hwnd {
            let _ = self.preview.paint(&mut self.gfx);
            return true;
        }
        if let Some(s) = self.settings.as_ref().filter(|s| s.hwnd == hwnd) {
            let cfg = &self.cfg;
            let _ = s.paint(&mut self.gfx, &|k: &str| setting_value(cfg, k), update::current());
            return true;
        }
        false
    }

    // ----------------------------------------------------------- frames --

    fn notch_wants(&self) -> bool {
        self.shown
            && (self.dirty
                || !self.w.settled()
                || !self.h.settled()
                || !self.r.settled()
                || !self.fade.settled()
                || !self.level.settled()
                || self.bars.iter().any(|b| !b.settled()))
    }

    fn wants_frame(&self) -> bool {
        self.notch_wants() || self.dock.wants_frame() || self.launcher.wants_frame() || self.flyout.wants_frame()
    }

    /// Steps the springs, draws, and returns how long to wait for the next frame.
    fn frame(&mut self) -> Duration {
        let now = Instant::now();
        let mut dt = (now - self.last_frame).as_secs_f32();
        if dt > 0.1 {
            dt = 1.0 / 60.0;
        }
        self.last_frame = now;

        let mut fast = false;
        if self.notch_wants() {
            fast |= self.w.step(dt);
            fast |= self.h.step(dt);
            fast |= self.r.step(dt);
            self.fade.step(dt);
            self.level.step(dt);
            for b in &mut self.bars {
                b.step(dt);
            }
            self.dirty = false;
            let _ = self.render();
        }
        if self.dock.wants_frame() {
            fast |= self.dock.step(dt);
            let status = dock::Status {
                wifi: self.radios.wifi,
                volume: self.vol.0,
                muted: self.vol.1,
                battery: battery(),
            };
            let weather = self.weather.as_ref().filter(|_| self.cfg.weather);
            let _ = self.dock.render(&mut self.gfx, &status, weather, &mut self.systray);
            if let Some(h) = self.systray.hwnd {
                let r = self.dock.bar_rect();
                if r != self.tray_rect {
                    self.tray_rect = r;
                    systray::set_rect(h, r);
                }
            }
        }
        if self.flyout.wants_frame() {
            fast |= self.flyout.step(dt);
            let _ = self.flyout.render(&mut self.gfx, &mut self.systray);
        }
        if self.launcher.wants_frame() {
            fast |= self.launcher.step(dt);
            let bottom = self.launcher_bottom();
            let anchor = if self.dock.shown { self.dock.start_anchor() } else { None };
            let _ = self.launcher.render(&mut self.gfx, &mut self.dock, self.monitor, bottom, anchor);
        }

        // Full rate while shapes morph; level bars and fades are fine at 60.
        let fps = if fast { self.cfg.max_fps } else { 60 }.min(self.refresh_hz).max(15);
        Duration::from_secs_f64(1.0 / fps as f64).saturating_sub(now.elapsed())
    }

    fn sync_art(&mut self) {
        let want = self.media.as_ref().and_then(|m| m.art.clone());
        let generation = self.gfx.generation;
        match (&want, &self.art) {
            (Some(a), Some((cur, _, g))) if Arc::ptr_eq(a, cur) && *g == generation => {}
            (Some(a), _) => {
                self.art = self.gfx.bitmap(a.width, a.height, &a.pixels).ok().map(|b| (a.clone(), b, generation))
            }
            (None, _) => self.art = None,
        }
    }

    fn accent(&self) -> Color {
        match self.media.as_ref().and_then(|m| m.art.as_ref()) {
            Some(a) => Color(a.accent[0], a.accent[1], a.accent[2], 1.0),
            None => Color::white(0.9),
        }
    }

    fn render(&mut self) -> Result<()> {
        self.sync_art();
        let k = self.k;
        let bw = self.w.value.max(1.0);
        let bh = self.h.value.max(0.0);
        let mon_w = self.monitor.right - self.monitor.left;
        let mut pw = ((bw + 2.0 * EAR) * k).ceil() as i32 + 2;
        if (mon_w - pw) % 2 != 0 {
            pw += 1; // keep the window exactly centred, so it never jitters
        }
        let ph = ((bh * k).ceil() as i32).max(1);
        let pos = POINT { x: self.monitor.left + (mon_w - pw) / 2, y: self.monitor.top };
        let cx = pw as f32 / k / 2.0;
        let body_x = cx - bw / 2.0;
        let (tw, _, _) = self.size_of(self.view);
        self.origin = cx - tw / 2.0;
        self.content_w = tw;

        self.gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = self.gfx.painter();
            p.notch(body_x, bw, bh, self.r.value, EAR, BLACK);
            let alpha = self.fade.value.clamp(0.0, 1.0);
            if alpha > 0.01 && bh > 4.0 {
                p.clip(body_x, 0.0, bw, bh);
                match self.view {
                    View::Idle => {}
                    View::Live => self.draw_live(&p, alpha),
                    View::Peek if matches!(self.peek, Some(PeekKind::Timer | PeekKind::Message)) => {
                        self.draw_timer_peek(&p, alpha)
                    }
                    View::Peek => {
                        let (_, th, _) = self.size_of(View::Peek);
                        self.draw_level_row(&p, self.origin + 16.0, th / 2.0, tw - 32.0, self.peek, alpha);
                    }
                    View::Expanded => self.draw_expanded(&p, alpha),
                }
                p.unclip();
            }
        }
        self.gfx.present(&self.layer, self.hwnd, pos, pw, ph, 255)
    }

    // ---------------------------------------------------------- hit-test --

    /// Clickable rects of the open notch: (what, x, y, w, h), x relative to
    /// the content origin.
    fn hotspots(&self) -> Vec<(Hot, f32, f32, f32, f32)> {
        let mut out = Vec::new();
        if self.view != View::Expanded {
            return out;
        }
        let (tw, th, _) = self.size_of(View::Expanded);
        for i in 0..MODES.len() {
            out.push((Hot::Tab(i), tw / 2.0 + (i as f32 - 1.0) * 16.0 - 8.0, th - 18.0, 16.0, 16.0));
        }
        match self.mode {
            Mode::Media if self.media.is_some() => {
                for (h, dx) in [(Hot::Previous, -60.0), (Hot::PlayPause, 0.0), (Hot::Next, 60.0)] {
                    out.push((h, tw / 2.0 + dx - 20.0, 104.0, 40.0, 40.0));
                }
            }
            Mode::Media => {}
            Mode::Controls => {
                let tile_w = (tw - 36.0 - 16.0) / 3.0;
                for (i, h) in [Hot::Wifi, Hot::Bluetooth, Hot::Notifications].into_iter().enumerate() {
                    out.push((h, 18.0 + i as f32 * (tile_w + 8.0), 14.0, tile_w, 54.0));
                }
                out.push((Hot::VolumeBar, 44.0, 84.0, tw - 98.0, 24.0));
                out.push((Hot::BrightnessBar, 44.0, 118.0, tw - 98.0, 24.0));
            }
            Mode::Calendar => {
                for (h, cx) in [(Hot::PomoReset, 274.0), (Hot::PomoToggle, 314.0), (Hot::PomoSkip, 354.0)] {
                    out.push((h, cx - 17.0, 150.0 - 17.0, 34.0, 34.0));
                }
            }
        }
        out
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hot> {
        let lx = x as f32 / self.k - self.origin;
        let ly = y as f32 / self.k;
        self.hotspots()
            .into_iter()
            .find(|(_, hx, hy, hw, hh)| lx >= *hx && lx < hx + hw && ly >= *hy && ly < hy + hh)
            .map(|(h, ..)| h)
    }

    // ---------------------------------------------------------- drawing --

    fn draw_art(&self, p: &Painter, x: f32, y: f32, s: f32, alpha: f32) {
        match &self.art {
            Some((_, bmp, _)) => p.image(bmp, x, y, s, s, alpha),
            None => p.rrect(x, y, s, s, s * 0.2, Color::white(0.14 * alpha)),
        }
    }

    fn draw_bars(&self, p: &Painter, x: f32, cy: f32, bar: f32, gap: f32, max_h: f32, alpha: f32) {
        let c = self.accent().alpha(alpha);
        for (i, b) in self.bars.iter().enumerate() {
            let h = bar + (max_h - bar) * b.value.clamp(0.0, 1.0);
            p.rrect(x + i as f32 * (bar + gap), cy - h / 2.0, bar, h, bar / 2.0, c);
        }
    }

    fn draw_live(&self, p: &Painter, alpha: f32) {
        let ox = self.origin;
        let th = self.cfg.idle_height;
        let tw = self.content_w;
        if self.playing() {
            let s = (th - 10.0).max(10.0);
            self.draw_art(p, ox + 12.0, (th - s) / 2.0, s, alpha);
            let (bar, gap) = (3.0, 2.2);
            let bars_w = BANDS as f32 * bar + (BANDS - 1) as f32 * gap;
            self.draw_bars(p, ox + tw - 12.0 - bars_w, th / 2.0, bar, gap, th - 12.0, alpha);
        } else {
            let c = if self.pomo.focus { ACCENT } else { GREEN };
            let (cx, cy, r) = (ox + 22.0, th / 2.0, (th / 2.0 - 6.0).max(5.0));
            p.stroke_circle(cx, cy, r, Color::white(0.18 * alpha), 2.5);
            let a = self.pomo.progress().clamp(0.0, 0.999) * std::f32::consts::TAU;
            if a > 0.01 {
                p.arc(cx, cy, r, -std::f32::consts::FRAC_PI_2, a - std::f32::consts::FRAC_PI_2, c.alpha(alpha), 2.5);
            }
            p.text(
                &self.pomo.label(),
                &self.gfx.fonts.num,
                ox + tw - 70.0,
                0.0,
                58.0,
                th,
                Align::Right,
                Color::white(0.95 * alpha),
            );
        }
    }

    fn draw_timer_peek(&self, p: &Painter, alpha: f32) {
        let (tw, th, _) = self.size_of(View::Peek);
        let (text, c) = if self.peek == Some(PeekKind::Message) {
            (self.message.as_str(), self.message_color)
        } else if self.pomo.focus {
            ("Break over \u{2014} time to focus", ACCENT)
        } else {
            ("Focus done \u{2014} take a break", GREEN)
        };
        p.circle(self.origin + 22.0, th / 2.0, 5.0, c.alpha(alpha));
        p.text(
            text,
            &self.gfx.fonts.label,
            self.origin + 34.0,
            0.0,
            tw - 50.0,
            th,
            Align::Left,
            Color::white(0.95 * alpha),
        );
    }

    /// Icon, level bar and percentage for volume or brightness.
    fn draw_level_row(&self, p: &Painter, x: f32, cy: f32, w: f32, kind: Option<PeekKind>, alpha: f32) {
        let white = Color::white(0.95 * alpha);
        let (value, dim, shown) = match kind {
            Some(PeekKind::Brightness) => {
                sun(p, x + 9.0, cy, 18.0, white);
                (self.brightness as f32 / 100.0, !self.brightness_ok, self.brightness as f32 / 100.0)
            }
            _ => {
                speaker(p, x + 9.0, cy, 18.0, self.vol.0, self.vol.1, white);
                (self.vol.0, self.vol.1, self.vol.0)
            }
        };
        let bar_x = x + 32.0;
        let bar_w = w - 32.0 - 40.0;
        p.rrect(bar_x, cy - 3.0, bar_w, 6.0, 3.0, Color::white(0.18 * alpha));
        let level = if kind.is_some() && self.view == View::Peek { self.level.value } else { shown };
        let fill = bar_w * level.clamp(0.0, 1.0);
        if fill > 0.5 {
            let c = Color::white(if dim { 0.35 } else { 0.95 } * alpha);
            p.rrect(bar_x, cy - 3.0, fill.max(6.0), 6.0, 3.0, c);
        }
        let pct = format!("{}", (value * 100.0).round() as i32);
        p.text(&pct, &self.gfx.fonts.num, x + w - 36.0, cy - 10.0, 36.0, 20.0, Align::Right, white);
    }

    fn draw_tabs(&self, p: &Painter, alpha: f32) {
        let (tw, th, _) = self.size_of(View::Expanded);
        for (i, &m) in MODES.iter().enumerate() {
            let cx = self.origin + tw / 2.0 + (i as f32 - 1.0) * 16.0;
            let active = m == self.mode;
            let hovered = self.hovered == Some(Hot::Tab(i));
            let a = if active {
                0.9
            } else if hovered {
                0.55
            } else {
                0.28
            };
            if active {
                p.rrect(cx - 7.0, th - 13.0, 14.0, 6.0, 3.0, Color::white(a * alpha));
            } else {
                p.circle(cx, th - 10.0, 3.0, Color::white(a * alpha));
            }
        }
    }

    fn draw_expanded(&self, p: &Painter, alpha: f32) {
        match self.mode {
            Mode::Media if self.media.is_some() => self.draw_media(p, alpha),
            Mode::Media => self.draw_home(p, alpha),
            Mode::Controls => self.draw_controls(p, alpha),
            Mode::Calendar => self.draw_calendar(p, alpha),
        }
        self.draw_tabs(p, alpha);
    }

    fn draw_media(&self, p: &Painter, alpha: f32) {
        let Some(m) = &self.media else { return };
        let ox = self.origin;
        let tw = self.content_w;
        let fonts = &self.gfx.fonts;
        self.draw_art(p, ox + 18.0, 16.0, 64.0, alpha);
        p.text(
            &m.title,
            &fonts.title,
            ox + 96.0,
            18.0,
            tw - 96.0 - 70.0,
            22.0,
            Align::Left,
            Color::white(0.95 * alpha),
        );
        p.text(&m.artist, &fonts.body, ox + 96.0, 40.0, tw - 96.0 - 70.0, 20.0, Align::Left, Color::white(0.6 * alpha));
        let (bar, gap) = (4.4, 3.0);
        let bars_w = BANDS as f32 * bar + (BANDS - 1) as f32 * gap;
        self.draw_bars(p, ox + tw - 18.0 - bars_w, 38.0, bar, gap, 26.0, alpha);

        if matches!(self.peek, Some(PeekKind::Volume | PeekKind::Brightness)) {
            self.draw_level_row(p, ox + 18.0, 94.0, tw - 36.0, self.peek, alpha);
        } else if m.duration > Duration::ZERO {
            let pos = m.position_now();
            let frac = (pos.as_secs_f32() / m.duration.as_secs_f32()).clamp(0.0, 1.0);
            let dim = Color::white(0.5 * alpha);
            p.text(&fmt_time(pos), &fonts.small, ox + 18.0, 86.0, 40.0, 16.0, Align::Left, dim);
            let remaining = format!("-{}", fmt_time(m.duration.saturating_sub(pos)));
            p.text(&remaining, &fonts.small, ox + tw - 58.0, 86.0, 40.0, 16.0, Align::Right, dim);
            let (bx, bw) = (ox + 62.0, tw - 124.0);
            p.rrect(bx, 92.0, bw, 4.0, 2.0, Color::white(0.18 * alpha));
            if frac > 0.0 {
                p.rrect(bx, 92.0, (bw * frac).max(4.0), 4.0, 2.0, Color::white(0.9 * alpha));
            }
        }

        let cy = 124.0;
        let cx = ox + tw / 2.0;
        let white = Color::white(0.95 * alpha);
        for (b, x) in [(Hot::Previous, cx - 60.0), (Hot::PlayPause, cx), (Hot::Next, cx + 60.0)] {
            if self.hovered == Some(b) {
                p.circle(x, cy, 19.0, Color::white(0.12 * alpha));
            }
            match b {
                Hot::Previous => skip(p, x, cy, 16.0, -1.0, white),
                Hot::Next => skip(p, x, cy, 16.0, 1.0, white),
                _ if m.playing => pause(p, x, cy, white),
                _ => play(p, x, cy, white),
            }
        }
    }

    fn draw_home(&self, p: &Painter, alpha: f32) {
        let ox = self.origin;
        let tw = self.content_w;
        let fonts = &self.gfx.fonts;
        let (time, date) = clock();
        p.text(&time, &fonts.big, ox + 22.0, 10.0, 190.0, 44.0, Align::Left, Color::white(0.95 * alpha));
        if matches!(self.peek, Some(PeekKind::Volume | PeekKind::Brightness)) {
            self.draw_level_row(p, ox + 22.0, 68.0, tw - 44.0, self.peek, alpha);
        } else {
            p.text(&date, &fonts.body, ox + 22.0, 54.0, 200.0, 22.0, Align::Left, Color::white(0.55 * alpha));
        }

        let rx = ox + tw - 22.0 - 150.0;
        if let Some(wx) = &self.weather {
            weather_icon(p, rx + 12.0, 27.0, 24.0, wx.code, wx.day, alpha);
            let unit = if wx.fahrenheit { "F" } else { "C" };
            p.text(
                &format!("{:.0}\u{b0}{unit}", wx.temp),
                &fonts.title,
                rx + 30.0,
                14.0,
                60.0,
                26.0,
                Align::Left,
                Color::white(0.95 * alpha),
            );
            p.text(wx.describe(), &fonts.small, rx + 84.0, 14.0, 66.0, 26.0, Align::Left, Color::white(0.6 * alpha));
            if wx.high.is_finite() && wx.low.is_finite() {
                let hl = format!("H {:.0}\u{b0}  L {:.0}\u{b0}", wx.high, wx.low);
                p.text(&hl, &fonts.small, rx + 30.0, 40.0, 120.0, 16.0, Align::Left, Color::white(0.5 * alpha));
            }
            if !wx.place.is_empty() {
                p.text(&wx.place, &fonts.small, rx + 30.0, 56.0, 120.0, 16.0, Align::Left, Color::white(0.4 * alpha));
            }
        }
        if let Some((pct, charging)) = battery() {
            let (bx, by) = (ox + tw - 22.0 - 28.0, 80.0);
            p.stroke_rrect(bx, by, 25.0, 13.0, 3.5, Color::white(0.5 * alpha), 1.2);
            p.rrect(bx + 26.0, by + 4.0, 2.0, 5.0, 1.0, Color::white(0.5 * alpha));
            let c = if charging {
                GREEN.alpha(alpha)
            } else if pct <= 20 {
                Color(1.0, 0.4, 0.35, alpha)
            } else {
                Color::white(0.9 * alpha)
            };
            p.rrect(bx + 2.0, by + 2.0, 21.0 * pct as f32 / 100.0, 9.0, 2.0, c);
            p.text(
                &format!("{pct}%"),
                &fonts.body,
                bx - 60.0,
                by - 4.0,
                54.0,
                21.0,
                Align::Right,
                Color::white(0.7 * alpha),
            );
        }
    }

    fn draw_controls(&self, p: &Painter, alpha: f32) {
        let ox = self.origin;
        let tw = self.content_w;
        let fonts = &self.gfx.fonts;
        let tile_w = (tw - 36.0 - 16.0) / 3.0;
        let tiles = [
            (Hot::Wifi, "Wi-Fi", self.radios.wifi),
            (Hot::Bluetooth, "Bluetooth", self.radios.bluetooth),
            (Hot::Notifications, "Notifications", None),
        ];
        for (i, (hot, label, state)) in tiles.into_iter().enumerate() {
            let x = ox + 18.0 + i as f32 * (tile_w + 8.0);
            let y = 14.0;
            let hovered = self.hovered == Some(hot);
            p.rrect(x, y, tile_w, 54.0, 14.0, Color::white(if hovered { 0.13 } else { 0.08 } * alpha));
            let on = state == Some(true);
            let (icx, icy) = (x + 24.0, y + 27.0);
            p.circle(icx, icy, 15.0, if on { ACCENT.alpha(alpha) } else { Color::white(0.14 * alpha) });
            let white = Color::white(0.95 * alpha);
            match hot {
                Hot::Wifi => wifi(p, icx, icy, white),
                Hot::Bluetooth => bluetooth(p, icx, icy, white),
                _ => bell(p, icx, icy, white),
            }
            let status = match (hot, state) {
                (Hot::Notifications, _) => "Open",
                (_, Some(true)) => "On",
                (_, Some(false)) => "Off",
                (_, None) => "Unavailable",
            };
            p.text(label, &fonts.label, x + 46.0, y + 9.0, tile_w - 50.0, 18.0, Align::Left, white);
            p.text(
                status,
                &fonts.small,
                x + 46.0,
                y + 27.0,
                tile_w - 50.0,
                16.0,
                Align::Left,
                Color::white(0.55 * alpha),
            );
        }
        // Sliders.
        let rows = [
            (Hot::VolumeBar, 96.0, self.vol.0, self.vol.1),
            (Hot::BrightnessBar, 130.0, self.brightness as f32 / 100.0, !self.brightness_ok),
        ];
        for (hot, cy, value, dim) in rows {
            let white = Color::white(if dim { 0.4 } else { 0.95 } * alpha);
            if hot == Hot::VolumeBar {
                speaker(p, ox + 27.0, cy, 18.0, value, self.vol.1, white);
            } else {
                sun(p, ox + 27.0, cy, 18.0, white);
            }
            let (x0, x1) = (ox + 50.0, ox + tw - 60.0);
            let track_h = if self.hovered == Some(hot) || self.dragging == Some(hot) { 10.0 } else { 8.0 };
            p.rrect(x0, cy - track_h / 2.0, x1 - x0, track_h, track_h / 2.0, Color::white(0.15 * alpha));
            let fill = (x1 - x0) * value.clamp(0.0, 1.0);
            if fill > 0.5 && !(hot == Hot::BrightnessBar && !self.brightness_ok) {
                p.rrect(
                    x0,
                    cy - track_h / 2.0,
                    fill.max(track_h),
                    track_h,
                    track_h / 2.0,
                    Color::white(if dim { 0.35 } else { 0.95 } * alpha),
                );
            }
            let pct = if hot == Hot::BrightnessBar && !self.brightness_ok {
                "\u{2014}".to_string()
            } else {
                format!("{}", (value * 100.0).round() as i32)
            };
            p.text(&pct, &fonts.num, ox + tw - 54.0, cy - 10.0, 36.0, 20.0, Align::Right, white);
        }
    }

    fn draw_calendar(&self, p: &Painter, alpha: f32) {
        let ox = self.origin;
        let fonts = &self.gfx.fonts;
        let st = unsafe { GetLocalTime() };
        let (year, month, today) = (st.wYear as i32, st.wMonth as u32, st.wDay as u32);
        let first_dow = (st.wDayOfWeek as i32 - (today as i32 - 1)).rem_euclid(7);
        let col0 = (first_dow + 6) % 7; // Monday first
        let cell = 27.0;
        let gx = ox + 18.0;

        p.text(
            &date_text(w!("MMMM yyyy")),
            &fonts.label,
            gx + 4.0,
            10.0,
            7.0 * cell,
            20.0,
            Align::Left,
            Color::white(0.95 * alpha),
        );
        for (i, d) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
            p.text(d, &fonts.small, gx + i as f32 * cell, 32.0, cell, 16.0, Align::Center, Color::white(0.4 * alpha));
        }
        for day in 1..=days_in_month(year, month) {
            let idx = col0 as u32 + day - 1;
            let (col, row) = ((idx % 7) as f32, (idx / 7) as f32);
            let cx = gx + col * cell;
            let cy = 50.0 + row * 24.0;
            let is_today = day == today;
            if is_today {
                p.circle(cx + cell / 2.0, cy + 11.0, 11.0, ACCENT.alpha(alpha));
            }
            let weekend = col >= 5.0;
            let a = if is_today {
                1.0
            } else if weekend {
                0.5
            } else {
                0.85
            };
            let font = if is_today { &fonts.label } else { &fonts.small };
            p.text(&day.to_string(), font, cx, cy, cell, 22.0, Align::Center, Color::white(a * alpha));
        }

        // Focus timer.
        let (rx, rw) = (ox + 226.0, 176.0);
        let c = if self.pomo.focus { ACCENT } else { GREEN };
        let phase = if self.pomo.focus { "Focus" } else { "Break" };
        p.text(phase, &fonts.label, rx, 10.0, rw, 20.0, Align::Center, c.alpha(alpha));
        let (cx, cy, r) = (ox + 314.0, 82.0, 40.0);
        p.stroke_circle(cx, cy, r, Color::white(0.12 * alpha), 4.0);
        let a = self.pomo.progress().clamp(0.0, 0.999) * std::f32::consts::TAU;
        if a > 0.01 {
            p.arc(cx, cy, r, -std::f32::consts::FRAC_PI_2, a - std::f32::consts::FRAC_PI_2, c.alpha(alpha), 4.0);
        }
        p.text(
            &self.pomo.label(),
            &fonts.title,
            cx - 40.0,
            cy - 13.0,
            80.0,
            26.0,
            Align::Center,
            Color::white(0.95 * alpha),
        );

        let white = Color::white(0.95 * alpha);
        for (hot, bx) in [(Hot::PomoReset, ox + 274.0), (Hot::PomoToggle, ox + 314.0), (Hot::PomoSkip, ox + 354.0)] {
            let by = 150.0;
            let bg = if self.hovered == Some(hot) { 0.18 } else { 0.09 };
            p.circle(bx, by, 16.0, Color::white(bg * alpha));
            match hot {
                Hot::PomoReset => p.rrect(bx - 5.0, by - 5.0, 10.0, 10.0, 2.0, white),
                Hot::PomoSkip => skip(p, bx, by, 12.0, 1.0, white),
                _ if self.pomo.running() => pause(p, bx, by, white),
                _ => play(p, bx, by, white),
            }
        }
    }
}

// ------------------------------------------------------------------ icons --

/// Double-triangle skip icon; `dir` is 1 for next, -1 for previous.
fn skip(p: &Painter, cx: f32, cy: f32, s: f32, dir: f32, c: Color) {
    let h = s * 0.42;
    let half = s * 0.5;
    for off in [-half, 0.0] {
        let x0 = cx + dir * off;
        let x1 = cx + dir * (off + half);
        p.polygon(&[(x0, cy - h), (x0, cy + h), (x1, cy)], c);
    }
}

fn play(p: &Painter, x: f32, cy: f32, c: Color) {
    p.polygon(&[(x - 6.0, cy - 10.0), (x - 6.0, cy + 10.0), (x + 10.0, cy)], c);
}

fn pause(p: &Painter, x: f32, cy: f32, c: Color) {
    p.rrect(x - 7.0, cy - 9.0, 5.0, 18.0, 1.5, c);
    p.rrect(x + 2.0, cy - 9.0, 5.0, 18.0, 1.5, c);
}

pub(crate) fn speaker(p: &Painter, cx: f32, cy: f32, s: f32, level: f32, muted: bool, c: Color) {
    let x0 = cx - s * 0.5;
    p.polygon(
        &[
            (x0, cy - s * 0.17),
            (x0 + s * 0.22, cy - s * 0.17),
            (x0 + s * 0.47, cy - s * 0.4),
            (x0 + s * 0.47, cy + s * 0.4),
            (x0 + s * 0.22, cy + s * 0.17),
            (x0, cy + s * 0.17),
        ],
        c,
    );
    let ax = x0 + s * 0.47;
    let stroke = s * 0.09;
    if muted {
        let (a, b) = (ax + s * 0.2, ax + s * 0.5);
        p.line(a, cy - s * 0.15, b, cy + s * 0.15, c, stroke);
        p.line(a, cy + s * 0.15, b, cy - s * 0.15, c, stroke);
    } else {
        if level > 0.01 {
            p.arc(ax, cy, s * 0.22, -0.8, 0.8, c, stroke);
        }
        if level > 0.45 {
            p.arc(ax, cy, s * 0.42, -0.8, 0.8, c, stroke);
        }
    }
}

fn sun(p: &Painter, cx: f32, cy: f32, s: f32, c: Color) {
    p.circle(cx, cy, s * 0.2, c);
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::FRAC_PI_4;
        let (dx, dy) = (a.cos(), a.sin());
        p.line(cx + dx * s * 0.32, cy + dy * s * 0.32, cx + dx * s * 0.46, cy + dy * s * 0.46, c, s * 0.09);
    }
}

fn wifi(p: &Painter, cx: f32, cy: f32, c: Color) {
    let base = cy + 5.0;
    let (a0, a1) = (-2.4, -0.74);
    for r in [4.5, 8.5] {
        p.arc(cx, base, r, a0, a1, c, 1.8);
    }
    p.circle(cx, base, 1.6, c);
}

fn bluetooth(p: &Painter, cx: f32, cy: f32, c: Color) {
    let pts = [
        (cx - 4.0, cy - 4.0),
        (cx + 4.0, cy + 4.0),
        (cx, cy + 8.0),
        (cx, cy - 8.0),
        (cx + 4.0, cy - 4.0),
        (cx - 4.0, cy + 4.0),
    ];
    for w in pts.windows(2) {
        p.line(w[0].0, w[0].1, w[1].0, w[1].1, c, 1.6);
    }
}

fn bell(p: &Painter, cx: f32, cy: f32, c: Color) {
    p.rrect(cx - 5.0, cy - 7.0, 10.0, 11.0, 5.0, c);
    p.rrect(cx - 7.0, cy + 2.0, 14.0, 2.5, 1.2, c);
    p.circle(cx, cy + 6.5, 1.8, c);
}

fn cloud(p: &Painter, cx: f32, cy: f32, s: f32, c: Color) {
    p.circle(cx - s * 0.18, cy + s * 0.04, s * 0.2, c);
    p.circle(cx + s * 0.05, cy - s * 0.08, s * 0.26, c);
    p.circle(cx + s * 0.25, cy + s * 0.08, s * 0.17, c);
    p.rrect(cx - s * 0.38, cy + s * 0.04, s * 0.8, s * 0.21, s * 0.1, c);
}

pub(crate) fn weather_icon(p: &Painter, cx: f32, cy: f32, s: f32, code: u32, day: bool, alpha: f32) {
    let sun_c = Color(1.0, 0.8, 0.3, alpha);
    let cloud_c = Color(0.85, 0.87, 0.9, alpha);
    let drop_c = Color(0.45, 0.7, 1.0, alpha);
    let sky = |p: &Painter, x: f32, y: f32, r: f32| {
        if day {
            sun(p, x, y, r * 2.4, sun_c);
        } else {
            p.circle(x, y, r, Color(0.92, 0.92, 0.85, alpha));
            p.circle(x + r * 0.45, y - r * 0.3, r * 0.85, BLACK);
        }
    };
    match code {
        0 | 1 => sky(p, cx, cy, s * 0.3),
        2 => {
            sky(p, cx + s * 0.2, cy - s * 0.18, s * 0.22);
            cloud(p, cx - s * 0.05, cy + s * 0.08, s * 0.8, cloud_c);
        }
        3 | 45 | 48 => cloud(p, cx, cy, s, cloud_c),
        71..=77 | 85 | 86 => {
            cloud(p, cx, cy - s * 0.12, s * 0.9, cloud_c);
            for i in 0..3 {
                p.circle(cx - s * 0.22 + i as f32 * s * 0.22, cy + s * 0.34, s * 0.06, Color::white(alpha));
            }
        }
        95..=99 => {
            cloud(p, cx, cy - s * 0.12, s * 0.9, cloud_c);
            p.polygon(
                &[
                    (cx, cy + s * 0.12),
                    (cx - s * 0.12, cy + s * 0.34),
                    (cx, cy + s * 0.32),
                    (cx - s * 0.06, cy + s * 0.5),
                ],
                sun_c,
            );
        }
        51..=67 | 80..=82 => {
            cloud(p, cx, cy - s * 0.12, s * 0.9, cloud_c);
            for i in 0..3 {
                let x = cx - s * 0.22 + i as f32 * s * 0.22;
                p.line(x, cy + s * 0.24, x - s * 0.06, cy + s * 0.42, drop_c, s * 0.07);
            }
        }
        _ => cloud(p, cx, cy, s, cloud_c),
    }
}
