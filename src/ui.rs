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

use windows::core::{w, Result, BOOL, PCWSTR};
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
    GetAsyncKeyState, GetCapture, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_LBUTTON,
    VK_MENU, VK_RBUTTON,
};
use windows::Win32::UI::Shell::{ABN_FULLSCREENAPP, ABN_POSCHANGED};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::audio::{self, Visualizer, Volume, BANDS};
use crate::backdrop::{self, Backdrop};
use crate::brightness;
use crate::bt;
use crate::calendar;
use crate::clipboard::{self, ClipCmd, ClipMenu};
use crate::config::{self, Config};
use crate::deskosd::{self, DeskOsd};
use crate::desktops::{self, Desktops};
use crate::devices::{self, Device};
use crate::dock::{self, Dock, Hot as DockHot};
use crate::gfx::{Align, Color, Edge, Gfx, Layer, Painter};
use crate::helper;
use crate::icons::{self, IconResult};
use crate::ics::{self, DAY};
use crate::keyhook;
use crate::launcher::{self, AppEntry, Launcher};
use crate::media::{self, Art, MediaCmd, MediaInfo};
use crate::mixer::Mixer;
use crate::mouseedge;
use crate::msg;
use crate::notifications::{self, NotifCmd};
use crate::pomodoro::Pomodoro;
use crate::preview::{self, Preview};
use crate::quickmenu::{self, QuickMenu};
use crate::radios::{self, Kind, RadioCmd, Radios};
use crate::settings::{Change, Settings};
use crate::shell::{self, AppCmd, MenuCmd, MenuState, Tray};
use crate::spring::Spring;
use crate::switcher::Switcher;
use crate::sysmon::{self, Readings, StatsCmd};
use crate::systray::{self, Flyout, Systray};
use crate::theme;
use crate::update;
use crate::weather::{self, Weather};
use crate::widgets::{self, Widgets};
use crate::wifi;

/// Width of the concave flares joining the notch to the screen edge.
const EAR: f32 = 9.0;
const PEEK_MS: u32 = 1600;
/// How long the notch stays open from the status icons if the pointer never
/// moves onto it.
const PINNED_MS: u32 = 6000;

// Notch timers.
const TIMER_EXPAND: usize = 1;
const TIMER_COLLAPSE: usize = 2;
const TIMER_PEEK: usize = 3;
const TIMER_TICK: usize = 4;
const TIMER_FLYOUT: usize = 5;
const TIMER_POMODORO: usize = 6;
const TIMER_KEYFIX: usize = 7;
const TIMER_SWITCH: usize = 9;
// Dock timers (on the dock window).
const TIMER_SCAN: usize = 10;
const TIMER_DOCK_HIDE: usize = 11;
const TIMER_PREVIEW_SHOW: usize = 12;
const TIMER_PREVIEW_HIDE: usize = 13;
const TIMER_CLOCK: usize = 14;
const TIMER_SCAN2: usize = 16;
const TIMER_TRAYSYNC: usize = 17;
const TIMER_NOTIFS: usize = 18;
/// Refreshes clear glass's bent edges (see `lens`).
const TIMER_LENS: usize = 19;
/// Pastes a clipboard item once the app it's for is in front again.
const TIMER_PASTE: usize = 20;
/// The next moment the calendar needs a look: a countdown to show or step,
/// or an event starting.
const TIMER_CAL: usize = 21;
/// Started at sign-in before Explorer: looks for its taskbar until it's up.
const TIMER_SHELL: usize = 22;
/// While something on a taskbar is hovered: checks the pointer is really
/// still there (see `check_hover`).
const TIMER_HOVER: usize = 23;

/// Not exported by the `windows` crate features we enable.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// The Windows accent colour changed (Tinted Glass follows it).
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;
const WM_POWERBROADCAST: u32 = 0x0218;
/// In a `WM_POWERBROADCAST`: the charger was plugged in or out, or the
/// battery's level changed.
const PBT_APMPOWERSTATUSCHANGE: usize = 0x000A;
const WM_DEVICECHANGE: u32 = 0x0219;

const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_SYSTEM_MOVESIZEEND: u32 = 0x000B;
const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;
const EVENT_OBJECT_SHOW: u32 = 0x8002;
const EVENT_OBJECT_CLOAKED: u32 = 0x8017;
const EVENT_OBJECT_UNCLOAKED: u32 = 0x8018;

const BLACK: Color = Color(0.0, 0.0, 0.0, 1.0);
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
    /// A USB device came or went (`App::device`); clicking opens its app.
    Device,
    /// The charger was plugged in or out (`App::power`).
    Power,
}

/// The surfaces `perf_log` times.
#[derive(Clone, Copy)]
enum Surface {
    Notch,
    Dock,
    /// The other screens' taskbars, together.
    Screens,
    Launcher,
    Flyout,
    Quick,
    Widgets,
    Switcher,
    Clipboard,
}

const SURFACES: [&str; 9] =
    ["notch", "dock", "screens", "launcher", "flyout", "quick", "widgets", "switcher", "clipboard"];

/// Each surface's render time and frame count, written to the log about once
/// a second while `perf_log` is on (and nothing at all while it's off).
struct Perf {
    on: bool,
    since: Instant,
    total: [Duration; SURFACES.len()],
    frames: [u32; SURFACES.len()],
}

impl Perf {
    fn new(on: bool) -> Self {
        Self { on, since: Instant::now(), total: [Duration::ZERO; SURFACES.len()], frames: [0; SURFACES.len()] }
    }

    fn start(&self) -> Option<Instant> {
        self.on.then(Instant::now)
    }

    fn end(&mut self, s: Surface, started: Option<Instant>) {
        if let Some(t) = started {
            self.total[s as usize] += t.elapsed();
            self.frames[s as usize] += 1;
        }
    }

    /// "perf: notch 0.42 ms 60 fps | dock 1.80 ms 30 fps", for the surfaces drawn.
    fn flush(&mut self) {
        if !self.on {
            return;
        }
        let secs = self.since.elapsed().as_secs_f64();
        if secs < 1.0 {
            return;
        }
        let parts: Vec<String> = (0..SURFACES.len())
            .filter(|&i| self.frames[i] > 0)
            .map(|i| {
                let ms = self.total[i].as_secs_f64() * 1000.0 / self.frames[i] as f64;
                format!("{} {ms:.2} ms {:.0} fps", SURFACES[i], self.frames[i] as f64 / secs)
            })
            .collect();
        if !parts.is_empty() {
            config::log(&format!("perf: {}", parts.join(" | ")));
        }
        *self = Self::new(true);
    }
}

/// A USB device being announced in the notch.
struct DeviceNote {
    device: Device,
    /// The serial port it got, once Windows says.
    port: Option<String>,
    /// It was unplugged rather than plugged in.
    removed: bool,
    at: Instant,
    /// The pointer is on the pop-up.
    hot: bool,
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
    /// The mixer: the output device (opens the list of them), one of them,
    /// an app's name (mutes it) and its volume.
    Output,
    OutputItem(usize),
    AppMute(usize),
    AppBar(usize),
    PomoToggle,
    PomoReset,
    PomoSkip,
    /// The Wi-Fi list (Controls): the chevron on the Wi-Fi tile that opens
    /// it, its back button, a network, and the ways on to Windows' own.
    WifiMore,
    WifiBack,
    WifiNet(usize),
    WifiSettings,
    WifiQuick,
    /// The Bluetooth list (Controls): the chevron on the Bluetooth tile, its
    /// back button, a device, and Windows' Bluetooth settings.
    BtMore,
    BtBack,
    BtDev(usize),
    BtSettings,
    /// Media: switch to the next app (or phone) with media.
    MediaSource,
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
    /// The main screen less taskbars (the notch sits against its edges).
    area: RECT,
    /// Physical pixels per layout unit (DPI scale x config scale).
    k: f32,
    /// DPI scale alone (the dock ignores the notch's `scale`).
    dpi: f32,
    refresh_hz: u32,

    layer: Layer,
    /// The blur behind the lip (and the notch pulled down from it).
    notch_backdrop: Backdrop,
    notch_blur: bool,
    w: Spring,
    h: Spring,
    r: Spring,
    fade: Spring,
    /// The open notch's tab bar: where its highlight sits (the tab's index)
    /// and its fade, which unlike `fade` doesn't restart on a tab switch.
    tab: Spring,
    tabs_fade: Spring,
    level: Spring,
    bars: [Spring; BANDS],
    /// A bar jumped up (see `msg::SPECTRUM`): draw it even though no spring moves.
    bars_fresh: bool,
    /// Whether the notch (or the lip) shows the visualizer's bars.
    notch_bars: bool,
    /// Whether the music widget or the panel's music card shows them.
    widget_bars: bool,
    /// Whether the taskbar is up with a music widget on it.
    dock_bars: bool,
    /// When the taskbar was last redrawn for the bars (it is wide, so they
    /// move on it at a gentler rate).
    dock_bars_at: Instant,
    last_frame: Instant,
    /// The last frame ran at the screen's refresh rate: pace the next one
    /// to the compositor.
    vsync: bool,
    dirty: bool,

    view: View,
    mode: Mode,
    expanded: bool,
    expand_pending: bool,
    /// Opened from the taskbar's status icons rather than by hovering: stays
    /// open until a click elsewhere, or a while if the pointer never comes.
    pinned: bool,
    /// With the notch turned off, it's brought up for Controls alone (from
    /// the taskbar's status icons or Win+A) and goes again once it closes.
    summoned: bool,
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
    notif_tx: Sender<NotifCmd>,
    /// Windows' notifications, for the widgets panel.
    notifs: notifications::Snapshot,
    volume: Option<Volume>,
    vol: (f32, bool),
    brightness: u32,
    brightness_ok: bool,
    brightness_tx: Sender<u32>,
    radios: Radios,
    radio_tx: Sender<RadioCmd>,
    /// The Wi-Fi networks in range, shown in Controls' Wi-Fi list while it's open.
    wifi_tx: Sender<wifi::Cmd>,
    wifi: wifi::Report,
    wifi_open: bool,
    /// The paired Bluetooth devices, shown in Controls' Bluetooth list while it's open.
    bt_tx: Sender<bt::Cmd>,
    bt: bt::Report,
    bt_open: bool,
    pomo: Pomodoro,
    weather: Option<Weather>,
    visualizer: Visualizer,
    flyout_hidden: bool,

    /// Content origin and width of the last frame, for hit-testing.
    origin: f32,
    origin_y: f32,
    content_w: f32,

    dock: Dock,
    /// Taskbars on the other screens, left to right.
    screens: Vec<Dock>,
    /// The taskbar (0: the main one, then `screens`) the open pop-up belongs
    /// to: the Start menu, widgets panel or Win+X menu.
    pop: usize,
    /// The taskbar the window previews are open over.
    preview_dock: usize,
    /// The taskbar an app's right-click menu came from.
    menu_dock: usize,
    preview: Preview,
    hooks: Vec<HWINEVENTHOOK>,
    taskbar_hidden: bool,
    instance: HINSTANCE,
    launcher: Launcher,
    launcher_menu: bool,
    settings: Option<Settings>,
    /// Tray icons from apps, and the flyout for the hidden ones.
    systray: Systray,
    switcher: Switcher,
    flyout: Flyout,
    /// The Win+X menu.
    quick: QuickMenu,
    /// The pop-up showing your desktops as you switch between them.
    deskosd: DeskOsd,
    /// The Win+V menu (made the first time it's opened), its reader, and the
    /// window it pastes into.
    clips: Option<ClipMenu>,
    clip_tx: Sender<ClipCmd>,
    clip_target: HWND,
    /// Last rect given to the hidden tray window.
    tray_rect: RECT,
    /// System stats, sampled only while a stats widget is on screen.
    /// The latest stats readings, from the reader thread (see `sysmon`).
    sysmon: Readings,
    /// Which NVIDIA card the shown readings start at (see `gpu_cards`).
    gpu_first: usize,
    stats_tx: Sender<StatsCmd>,
    /// How the reader was last started (its pace in ms, and whether it reads
    /// the GPU and disk counters), or None while it's stopped.
    stats_on: Option<(u32, bool)>,
    widgets: Widgets,
    widgets_seen: bool,
    /// Window changes come from the shell's own notifications (few) rather
    /// than from every window event in the system (many).
    shell_msg: u32,
    shell_hooked: bool,
    /// Watches Explorer alone, to catch it re-showing the taskbar we hid.
    taskbar_hook: Option<HWINEVENTHOOK>,
    /// The settings the pointer-matching strips were last built for.
    edge_key: String,
    /// Whether the taskbar is being held in view for an open pop-up.
    dock_held: bool,
    /// When the Win key last closed the menu.
    win_closed_at: Option<Instant>,
    /// When Cascade last asked apps to re-add their tray icons, and how many
    /// of the follow-up asks after a start are still to come.
    tray_announced: Option<Instant>,
    tray_syncs: u8,
    /// When a click outside last closed the Start menu or tray flyout, so the
    /// same click on its button doesn't open it again.
    outside_close: Option<Instant>,

    message: String,
    message_color: Color,
    /// Render times, logged with `perf_log`.
    perf: Perf,
    /// The Controls tab's volume mixer (read as Controls opens), and whether
    /// its list of outputs is open.
    mixer: Mixer,
    outputs_open: bool,
    /// Whether the theme bends light, and whether its refresh timer runs.
    lens_wanted: bool,
    lens_on: bool,
    /// The USB device the notch is announcing.
    device: Option<DeviceNote>,
    /// Files being dragged over a taskbar app: (taskbar, app, since when,
    /// whether its window was brought up).
    drag: Option<(usize, usize, Instant, bool)>,
    /// Windows' virtual desktops (see `desktops`).
    desks: Desktops,
    /// Your calendar's events (last month to about three months ahead), the
    /// reader, the link it was last given, and what's been announced:
    /// (event start, 0 coming up / 1 starting).
    events: Vec<ics::Event>,
    cal_tx: Sender<String>,
    cal_url: String,
    cal_said: Vec<(i64, u8)>,
    update_available: Option<u32>,
    updating: bool,
    /// The battery's level and whether it's plugged in, as last read (None
    /// without a battery), so plugging in or out can be told apart from the
    /// level moving.
    power: Option<(u8, bool)>,
    /// Hover checks in a row that found the pointer off the taskbar and its
    /// previews (see `check_hover`).
    hover_misses: u8,
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
        mouseedge::register(instance);
        // Blur panels: up to three taskbar islands, plus the launcher.
        let dock_backdrops = (0..4).filter_map(|_| Backdrop::create(instance).ok()).collect();
        let launcher_backdrop = Backdrop::create(instance)?;
        let flyout_hwnd = create_window(class, instance, WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW)?;
        let flyout = (flyout_hwnd, Backdrop::create(instance)?);
        let widgets_hwnd = create_window(class, instance, WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW)?;
        let widgets = (widgets_hwnd, Backdrop::create(instance)?);
        let switcher_hwnd =
            create_window(class, instance, WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE)?;
        let switcher = (switcher_hwnd, Backdrop::create(instance)?);
        let quick_hwnd = create_window(class, instance, WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW)?;
        let quick = (quick_hwnd, Backdrop::create(instance)?);
        let osd_hwnd =
            create_window(class, instance, WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE)?;
        let osd = (osd_hwnd, Backdrop::create(instance)?);
        let notch_backdrop = Backdrop::create(instance)?;

        let app = App::new(
            hwnd,
            notch_backdrop,
            dock_hwnd,
            trigger,
            preview_hwnd,
            (launcher_hwnd, launcher_backdrop),
            dock_backdrops,
            flyout,
            widgets,
            switcher,
            quick,
            osd,
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
                // Animating at the screen's rate: wait for the compositor's
                // next frame, so motion steps evenly with the refresh instead
                // of drifting against it. If it comes back at once (nothing
                // for it to compose) the timer below paces it instead.
                if with_app(|a| a.vsync).unwrap_or(false) {
                    let t = Instant::now();
                    let _ = windows::Win32::Graphics::Dwm::DwmFlush();
                    if t.elapsed() >= Duration::from_millis(1) {
                        continue;
                    }
                }
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

/// Files dragged over taskbar `di` at screen point (x, y) (see `drop`).
pub(crate) fn drag_over(di: usize, x: i32, y: i32) -> bool {
    with_app(|a| a.on_drag(di, x, y)).unwrap_or(false)
}

pub(crate) fn drag_leave(di: usize) {
    with_app(|a| a.on_drag_leave(di));
}

/// Files dropped on taskbar `di`; true if an app took them.
pub(crate) fn drop_files(di: usize, x: i32, y: i32, files: Vec<String>) -> bool {
    with_app(move |a| a.on_drop(di, x, y, files)).unwrap_or(false)
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
        msg::SWITCH => {
            with_app(|a| a.on_switch(wp.0));
            return LRESULT(0);
        }
        msg::OUTSIDE_CLICK => {
            with_app(|a| a.on_outside_click());
            return LRESULT(0);
        }
        msg::KEY_CHECK => {
            with_app(|a| unsafe {
                SetTimer(Some(a.hwnd), TIMER_KEYFIX, 150, None);
            });
            return LRESULT(0);
        }
        msg::OPEN_LAUNCHER => {
            with_app(|a| a.toggle_launcher());
            return LRESULT(0);
        }
        msg::SHORTCUT => {
            with_app(|a| a.on_shortcut(wp.0 as u8));
            return LRESULT(0);
        }
        msg::OPEN_SETTINGS => {
            with_app(|a| a.open_settings());
            return LRESULT(0);
        }
        msg::AUTOSTART => {
            with_app(|a| a.on_late_autostart());
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
                for d in &mut a.screens {
                    d.on_icon((*icon).clone());
                }
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
        msg::DESKTOPS => {
            with_app(|a| a.on_desktops());
            return LRESULT(0);
        }
        msg::CALENDAR => {
            let events = Box::from_raw(lp.0 as *mut Vec<ics::Event>);
            with_app(move |a| a.on_events(*events));
            return LRESULT(0);
        }
        msg::CLIPS => {
            let c = Box::from_raw(lp.0 as *mut clipboard::Clips);
            with_app(move |a| {
                if let Some(m) = &mut a.clips {
                    m.set(*c);
                }
            });
            return LRESULT(0);
        }
        msg::RECENT => {
            let list = Box::from_raw(lp.0 as *mut Vec<launcher::RecentFile>);
            with_app(move |a| a.launcher.set_recent(*list));
            return LRESULT(0);
        }
        msg::STATS => {
            let r = Box::from_raw(lp.0 as *mut Readings);
            with_app(move |a| a.on_stats(*r));
            return LRESULT(0);
        }
        msg::WIFI => {
            let report = Box::from_raw(lp.0 as *mut wifi::Report);
            with_app(move |a| {
                a.wifi = *report;
                if a.wifi_open {
                    a.retarget();
                    a.dirty = true;
                }
            });
            return LRESULT(0);
        }
        msg::BLUETOOTH => {
            let report = Box::from_raw(lp.0 as *mut bt::Report);
            with_app(move |a| {
                a.bt = *report;
                a.dirty = true;
                if a.bt_open {
                    a.retarget();
                }
            });
            return LRESULT(0);
        }
        msg::NOTIFS => {
            let n = Box::from_raw(lp.0 as *mut notifications::Snapshot);
            with_app(move |a| {
                a.notifs = *n;
                a.widgets.dirty = true;
            });
            return LRESULT(0);
        }
        msg::WEATHER => {
            // No payload: only the location lookup's status changed.
            let w = if lp.0 != 0 { Some(Box::from_raw(lp.0 as *mut Weather)) } else { None };
            with_app(move |a| {
                if let Some(w) = w {
                    a.weather = Some(*w);
                    a.dirty = true;
                    a.dock.dirty = true;
                }
                if let Some(s) = &a.settings {
                    s.invalidate();
                }
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
    match key {
        "autostart" => shell::autostart_enabled().to_string(),
        "autostart_late" => shell::autostart_late().to_string(),
        // Not a setting: whether there's a battery (see `settings::shown`).
        "battery" => battery().is_some().to_string(),
        _ => cfg.get(key),
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

/// Local-seconds time `t` (see `ics`) in a date `format` ("ddd", "d MMM").
fn date_at(t: i64, format: PCWSTR) -> String {
    let st = calendar::system_time(t);
    let mut d = [0u16; 64];
    unsafe {
        let n = GetDateFormatEx(
            PCWSTR::null(),
            ENUM_DATE_FORMATS_FLAGS(0),
            Some(&st as *const SYSTEMTIME),
            format,
            Some(&mut d),
            PCWSTR::null(),
        );
        String::from_utf16_lossy(&d[..(n.max(1) - 1) as usize])
    }
}

/// The time of day of `t`, as the clock shows it ("10:30", "10:30 am").
pub(crate) fn time_at(t: i64) -> String {
    let st = calendar::system_time(t);
    let mut b = [0u16; 32];
    unsafe {
        let n = GetTimeFormatEx(
            PCWSTR::null(),
            TIME_NOSECONDS,
            Some(&st as *const SYSTEMTIME),
            PCWSTR::null(),
            Some(&mut b),
        );
        String::from_utf16_lossy(&b[..(n.max(1) - 1) as usize])
    }
}

/// When an event is, for the agenda: "Now", "Today 10:30", "Tomorrow",
/// "Fri 9:00".
fn when(e: &ics::Event, now: i64) -> String {
    if e.start <= now && e.end > now && !e.all_day {
        return "Now".into();
    }
    let days = e.start.div_euclid(DAY) - now.div_euclid(DAY);
    let day = match days {
        ..=0 => "Today".to_string(),
        1 => "Tomorrow".to_string(),
        _ => date_at(e.start, w!("ddd")),
    };
    if e.all_day {
        day
    } else {
        format!("{day} {}", time_at(e.start))
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

unsafe extern "system" fn collect_monitor(h: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
    (*(out.0 as *mut Vec<HMONITOR>)).push(h);
    true.into()
}

/// Every screen but the main one, left to right: (its rect, its DPI scale).
fn other_screens() -> Vec<(RECT, f32)> {
    let mut mons: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect_monitor), LPARAM(&mut mons as *mut _ as isize));
    }
    let main = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let mut out: Vec<(RECT, f32)> = mons
        .into_iter()
        .filter(|&m| m != main)
        .filter_map(|m| unsafe {
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(m, &mut mi).as_bool() {
                return None;
            }
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            Some((mi.rcMonitor, dx.max(48) as f32 / 96.0))
        })
        .collect();
    out.sort_by_key(|(r, _)| (r.left, r.top));
    out
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
        notch_backdrop: Backdrop,
        dock_hwnd: HWND,
        trigger: HWND,
        preview_hwnd: HWND,
        (launcher_hwnd, launcher_backdrop): (HWND, Backdrop),
        dock_backdrops: Vec<Backdrop>,
        (flyout_hwnd, flyout_backdrop): (HWND, Backdrop),
        (widgets_hwnd, widgets_backdrop): (HWND, Backdrop),
        (switcher_hwnd, switcher_backdrop): (HWND, Backdrop),
        (quick_hwnd, quick_backdrop): (HWND, Backdrop),
        (osd_hwnd, osd_backdrop): (HWND, Backdrop),
        instance: HINSTANCE,
    ) -> Result<Self> {
        let cfg = Config::load();
        theme::set(&cfg.theme);
        theme::set_colour(&cfg.theme_color, cfg.theme_hue, cfg.theme_intensity);
        theme::set_edge(cfg.theme_edge);
        theme::set_refraction(cfg.refraction);
        let gfx = Gfx::new()?;
        let volume = Volume::new().ok();
        let vol = volume.as_ref().and_then(|v| v.get()).unwrap_or((0.0, false));
        let media_tx = media::spawn();
        brightness::spawn_watcher();
        brightness::listen(hwnd);
        devices::listen(hwnd);
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
            area: RECT::default(),
            k: 1.0,
            dpi: 1.0,
            refresh_hz: 60,
            layer: Layer::default(),
            notch_backdrop,
            notch_blur: false,
            // Start small and flat so the notch grows in on launch.
            w: start_w,
            h: size,
            r: Spring::new(0.0, 420.0, 34.0),
            fade: Spring::new(0.0, 260.0, 30.0),
            tab: Spring::new(0.0, 520.0, 42.0),
            tabs_fade: Spring::new(0.0, 260.0, 30.0),
            // Stiff and close to critically damped, so the bar keeps up with
            // a held volume key instead of trailing behind it.
            level: Spring::new(vol.0, 1600.0, 78.0),
            bars: [Spring::new(0.0, 700.0, 30.0); BANDS],
            bars_fresh: false,
            notch_bars: false,
            widget_bars: false,
            dock_bars: false,
            dock_bars_at: Instant::now(),
            last_frame: Instant::now(),
            vsync: false,
            dirty: true,
            view: View::Idle,
            mode: Mode::Media,
            expanded: false,
            expand_pending: false,
            pinned: false,
            summoned: false,
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
            notif_tx: notifications::spawn(),
            notifs: notifications::Snapshot::default(),
            volume,
            vol,
            brightness: 0,
            brightness_ok: false,
            brightness_tx,
            radios: Radios::default(),
            radio_tx,
            wifi_tx: wifi::spawn(),
            wifi: wifi::Report::default(),
            wifi_open: false,
            bt_tx: bt::spawn(),
            bt: bt::Report::default(),
            bt_open: false,
            pomo: Pomodoro::new(cfg.focus_minutes, cfg.break_minutes),
            weather: None,
            visualizer: Visualizer::default(),
            flyout_hidden: false,
            origin: 0.0,
            origin_y: 0.0,
            content_w: 0.0,
            dock: Dock::new(dock_hwnd, trigger, icons::spawn(), dock_backdrops),
            screens: Vec::new(),
            pop: 0,
            preview_dock: 0,
            menu_dock: 0,
            preview: Preview::new(preview_hwnd),
            hooks: Vec::new(),
            taskbar_hidden: false,
            instance,
            launcher: Launcher::new(launcher_hwnd, launcher_backdrop),
            launcher_menu: false,
            settings: None,
            systray: Systray::default(),
            switcher: Switcher::new(switcher_hwnd, switcher_backdrop),
            flyout: Flyout::new(flyout_hwnd, flyout_backdrop),
            quick: QuickMenu::new(quick_hwnd, quick_backdrop),
            deskosd: DeskOsd::new(osd_hwnd, osd_backdrop),
            clips: None,
            clip_tx: clipboard::spawn(),
            clip_target: HWND::default(),
            tray_rect: RECT::default(),
            outside_close: None,
            shell_msg: unsafe { RegisterWindowMessageW(w!("SHELLHOOK")) },
            shell_hooked: false,
            taskbar_hook: None,
            edge_key: String::new(),
            sysmon: Readings::default(),
            gpu_first: 0,
            stats_tx: sysmon::spawn(),
            stats_on: None,
            widgets: Widgets::new(widgets_hwnd, widgets_backdrop),
            widgets_seen: false,
            dock_held: false,
            win_closed_at: None,
            tray_announced: None,
            tray_syncs: 0,
            message: String::new(),
            message_color: theme::accent(),
            device: None,
            drag: None,
            desks: desktops::read(),
            events: Vec::new(),
            cal_tx: calendar::spawn(),
            cal_url: String::new(),
            cal_said: Vec::new(),
            perf: Perf::new(cfg.perf_log),
            mixer: Mixer::default(),
            outputs_open: false,
            lens_wanted: false,
            lens_on: false,
            update_available: None,
            updating: false,
            power: battery(),
            hover_misses: 0,
            cfg,
        })
    }

    fn start(&mut self) {
        let ready = shell::shell_ready();
        config::log(&format!(
            "started {} s after Windows (Explorer's taskbar up: {ready}; starts with Windows: {})",
            shell::uptime_secs(),
            shell::autostart_kind()
        ));
        shell::hint_late_autostart();
        if !ready {
            // Signed in a moment ago: Explorer is still on its way. Its
            // TaskbarCreated says when it's up; this is in case that's missed.
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_SHELL, 1000, None);
            }
        }
        shell::ensure_shortcuts();
        launcher::spawn_scan();
        crate::drop::register(self.dock.hwnd, 0);
        self.apply_calendar();
        desktops::watch();
        if self.cfg.launcher && self.cfg.start_show_recent {
            launcher::spawn_recent();
        }
        if self.cfg.update_check {
            update::spawn_checker(self.cfg.github_token.clone());
        }
        self.apply_taskbar();
        self.update_display();
        self.apply_flyout();
        self.apply_dock();
        self.retarget();
        self.arm_lens();
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
        keyhook::configure(false, false, 0);
        self.settings = None;
        self.visualizer.set(false);
        weather::stop();
        for h in self.hooks.drain(..).chain(self.taskbar_hook.take()) {
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
        if self.shell_hooked {
            unsafe {
                let _ = DeregisterShellHookWindow(self.hwnd);
            }
            self.shell_hooked = false;
        }
        self.preview.hide();
        crate::drop::revoke(self.dock.hwnd);
        self.dock.shutdown();
        for d in &mut self.screens {
            d.shutdown();
        }
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
            let work = mi.rcWork;
            self.area = if work.right > work.left && work.bottom > work.top { work } else { mi.rcMonitor };
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
        self.hook_taskbar();
    }

    fn apply_dock(&mut self) {
        // The launcher also watches the foreground window, to take over when
        // Windows opens its own Start menu (see `on_win_event`).
        let hooks_wanted = self.cfg.dock || (self.cfg.launcher && self.cfg.launcher_win_key);
        if hooks_wanted && self.hooks.is_empty() {
            let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
            let ranges = [
                (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
                // A window dragged to another screen (its taskbar shows it).
                (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
                (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
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
        // Windows appearing and going: the shell tells us about top-level
        // windows only.
        if hooks_wanted && !self.shell_hooked {
            self.shell_hooked = unsafe { RegisterShellHookWindow(self.hwnd) }.as_bool();
        } else if !hooks_wanted && self.shell_hooked {
            unsafe {
                let _ = DeregisterShellHookWindow(self.hwnd);
            }
            self.shell_hooked = false;
        }
        keyhook::configure(
            self.cfg.launcher && self.cfg.launcher_win_key,
            self.cfg.alt_tab,
            keyhook::shortcuts(&self.cfg),
        );
        if !self.cfg.shortcut_win_x {
            self.quick.hide();
        }
        if !self.cfg.alt_tab {
            self.switcher.hide_now();
        }
        self.switcher.set_style(crate::switcher::Style::parse(&self.cfg.alt_tab_style));
        self.preview.peek = self.cfg.taskbar_peek;
        if !self.cfg.launcher {
            self.launcher.hide();
        }
        self.configure_docks();
        self.show_docks();
        self.launcher.configure(&self.cfg);
        self.flyout.configure(self.cfg.acrylic, self.cfg.taskbar_opacity, self.cfg.corner_radius());
        self.quick.configure(self.cfg.acrylic, self.cfg.taskbar_opacity, self.cfg.corner_radius());
        self.deskosd.configure(self.cfg.acrylic, self.cfg.taskbar_opacity, self.cfg.corner_radius());
        if let Some(c) = &mut self.clips {
            c.configure(self.cfg.acrylic, self.cfg.taskbar_opacity, self.cfg.corner_radius());
            if !self.cfg.shortcut_win_v {
                c.hide();
            }
        }
        self.widgets.configure(self.cfg.acrylic, self.cfg.taskbar_opacity, self.cfg.corner_radius());
        self.switcher.configure(self.cfg.acrylic, self.cfg.taskbar_opacity, self.cfg.corner_radius());
        self.notch_blur = theme::style_backdrop(
            &self.notch_backdrop,
            self.cfg.acrylic,
            self.cfg.taskbar_opacity,
            self.cfg.corner_radius(),
        );
        self.arm_stats();
        self.apply_edges(false);
        mouseedge::raise();
        let want_tray = self.cfg.dock && self.cfg.tray_icons;
        match (want_tray, self.systray.hwnd) {
            (true, None) => self.start_systray(),
            (false, Some(h)) => {
                systray::stop(h);
                self.systray = Systray::default();
                self.flyout.hide();
            }
            _ => {}
        }
        if self.cfg.dock {
            self.scan_docks();
        } else {
            self.preview.hide();
        }
        self.arm_clock();
    }

    /// Puts Cascade's hidden tray window in front of Explorer's. Not before
    /// Explorer's taskbar is up: a window of its class already there at
    /// sign-in can stop Explorer starting as the desktop, and it would have
    /// nothing to pass the icons on to anyway. TaskbarCreated tries again.
    fn start_systray(&mut self) {
        if self.systray.hwnd.is_some() || !shell::shell_ready() {
            return;
        }
        self.systray.hwnd = systray::start(self.instance);
        self.tray_rect = RECT::default();
        // Apps still starting (or Explorer, at sign-in) miss the first
        // announcement: ask again a few times.
        self.tray_announced = Some(Instant::now());
        self.tray_syncs = 3;
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_TRAYSYNC, 4000, None);
        }
    }

    /// A fullscreen app came to the front (true) or went.
    fn set_fullscreen(&mut self, on: bool) {
        self.fullscreen = on;
        self.retarget();
        self.show_docks();
        self.sync_visualizer();
        if self.fullscreen {
            self.preview.hide();
        }
        mouseedge::pause(self.fullscreen);
        mouseedge::raise();
        self.arm_stats();
    }

    /// Explorer made its taskbar: at sign-in, or after it restarted.
    fn on_shell_ready(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SHELL);
        }
        config::log(&format!("Explorer's taskbar is up, {} s after Windows started", shell::uptime_secs()));
        if let Some(t) = &self.tray {
            t.add();
        }
        // App bars registered before Explorer was up went nowhere, so the
        // shell never says when fullscreen apps come and go: register again,
        // and don't stay hidden for a fullscreen app that was never there.
        shell::register_appbar(self.hwnd);
        let fullscreen = shell::fullscreen_now();
        if fullscreen != self.fullscreen {
            self.set_fullscreen(fullscreen);
        }
        if self.taskbar_hidden {
            shell::hide_taskbar();
        }
        self.hook_taskbar();
        self.update_display();
        self.configure_docks();
        self.show_docks();
        if self.cfg.dock && self.cfg.tray_icons {
            self.start_systray();
        }
        self.apply_flyout();
        self.retarget();
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
        } else if self.cfg.live_activity && (self.playing() || self.pomo.running() || self.countdown().is_some()) {
            View::Live
        } else {
            View::Idle
        }
    }

    fn notch_edge(&self) -> Edge {
        Edge::parse(&self.cfg.notch_edge)
    }

    /// Whether the resting notch is a slim strip that doesn't grow for music
    /// or the focus timer (a line in it lights up instead): the lip, and the
    /// classic notch on a side edge.
    fn flat_rest(&self) -> bool {
        self.cfg.lip() || self.notch_edge().side()
    }

    /// Corner radius of the lip and the notch pulled down from it: the same
    /// as the panels' (and their blur's), whatever the notch's own scale.
    fn lip_radius(&self) -> f32 {
        self.cfg.corner_radius() / self.cfg.scale.max(0.1)
    }

    /// Width, height and corner radius of a view, in layout units.
    fn size_of(&self, view: View) -> (f32, f32, f32) {
        let (iw, ih) = (self.cfg.idle_width, self.cfg.idle_height);
        let lip = self.cfg.lip();
        if matches!(view, View::Idle | View::Live) && self.flat_rest() {
            // The lip is sized in pixels at 100%, apart from the notch's scale.
            let s = self.cfg.scale.max(0.1);
            let (along, depth, r) = if lip {
                (self.cfg.lip_width / s, self.cfg.lip_height / s, self.lip_radius())
            } else {
                (iw, ih, ih * 0.36)
            };
            // Against a side edge the strip stands upright.
            return if self.notch_edge().side() { (depth, along, r) } else { (along, depth, r) };
        }
        let (w, h, r) = match view {
            View::Idle => (iw, ih, ih * 0.36),
            View::Live => (iw + 104.0, ih, ih * 0.42),
            View::Peek if self.peek == Some(PeekKind::Timer) => (iw + 170.0, ih + 12.0, (ih + 12.0) * 0.4),
            View::Peek if matches!(self.peek, Some(PeekKind::Message | PeekKind::Power)) => {
                (iw + 190.0, ih + 12.0, (ih + 12.0) * 0.4)
            }
            View::Peek if self.peek == Some(PeekKind::Device) => (iw + 250.0, ih + 12.0, (ih + 12.0) * 0.4),
            View::Peek => (iw + 110.0, ih + 12.0, (ih + 12.0) * 0.4),
            // Every tab is as wide, so the tab bar under them holds still.
            View::Expanded => match self.mode {
                Mode::Media if self.media.is_some() => (420.0, 148.0 + TAB_BAR, 30.0),
                Mode::Media => (420.0, 102.0 + TAB_BAR, 28.0),
                Mode::Controls if self.wifi_open => (420.0, self.wifi_layout().total + TAB_BAR, 30.0),
                Mode::Controls if self.bt_open => (420.0, self.bt_layout().total + TAB_BAR, 30.0),
                Mode::Controls => (420.0, 154.0 + TAB_BAR + self.mixer_h(), 30.0),
                Mode::Calendar => (420.0, 194.0 + TAB_BAR + self.agenda_h(), 30.0),
            },
        };
        (w, h, if lip { self.lip_radius() } else { r })
    }

    /// Re-derives the view and animation targets after any state change.
    fn retarget(&mut self) {
        // The Wi-Fi and Bluetooth lists close with the notch.
        if !self.expanded {
            self.wifi_open = false;
            self.bt_open = false;
        }
        let visible = (self.cfg.notch || self.summoned) && !(self.cfg.hide_in_fullscreen && self.fullscreen);
        if visible != self.shown {
            self.shown = visible;
            unsafe {
                let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
            if !visible {
                self.expanded = false;
                self.peek = None;
                self.layer.release();
                self.notch_backdrop.hide();
            }
        }

        if self.pinned && !self.expanded {
            self.pinned = false;
            crate::mousehook::set_rect(3, None);
        }

        let view = self.compute_view();
        if view != self.view {
            self.view = view;
            self.fade.snap(0.0);
            self.fade.set(1.0);
            self.tabs_fade.snap(0.0);
            self.tabs_fade.set(1.0);
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

        self.sync_visualizer();
        self.dirty = true;
    }

    /// Runs the music visualizer while something on screen shows its bars:
    /// the notch (or the lip's line), the taskbar's music widget or the
    /// widgets panel's music card.
    fn sync_visualizer(&mut self) {
        let playing = self.playing();
        self.notch_bars = self.shown
            && playing
            && match self.view {
                View::Live if self.flat_rest() => self.cfg.lip_visualizer,
                View::Live => self.cfg.visualizer,
                View::Expanded => self.mode == Mode::Media && self.cfg.visualizer,
                _ => false,
            };
        let chip = [&self.cfg.taskbar_left, &self.cfg.taskbar_right]
            .iter()
            .any(|l| l.split(',').any(|c| c.trim().eq_ignore_ascii_case("music")));
        let widget_bars = playing && self.cfg.widget_visualizer && (self.widgets.open || (chip && self.dock.visible()));
        self.dock_bars = chip && self.dock.visible();
        if widget_bars != self.widget_bars {
            // Draw them in, or take them away.
            self.widget_bars = widget_bars;
            self.dock.dirty = true;
            self.widgets.dirty = true;
        }
        let on = self.notch_bars || self.widget_bars;
        self.visualizer.set(on);
        if !on {
            for b in &mut self.bars {
                b.set(0.0);
            }
        }
    }

    /// The visualizer's bands as they are on screen now.
    fn levels(&self) -> [f32; BANDS] {
        std::array::from_fn(|i| self.bars[i].value)
    }

    fn set_mode(&mut self, mode: Mode) {
        if mode != self.mode {
            self.mode = mode;
            self.wifi_open = false;
            self.bt_open = false;
            if mode == Mode::Controls {
                self.read_mixer();
            }
            self.tab.set(MODES.iter().position(|&m| m == mode).unwrap_or(0) as f32);
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
            PeekKind::Timer | PeekKind::Message | PeekKind::Device | PeekKind::Power => 0.0,
        };
        let again = self.peek == Some(kind) && self.view == View::Peek;
        if self.peek != Some(kind) {
            self.level.snap(value);
        }
        self.level.set(value);
        self.peek = Some(kind);
        unsafe {
            let ms = match kind {
                PeekKind::Timer | PeekKind::Message | PeekKind::Power => 4000,
                PeekKind::Device => 6000,
                _ => PEEK_MS,
            };
            SetTimer(Some(self.hwnd), TIMER_PEEK, ms, None);
            if self.flyout_hidden && matches!(kind, PeekKind::Volume | PeekKind::Brightness) {
                // The flyout host can be created lazily on first use. Looking
                // for it walks Explorer's windows, so only once per pop-up;
                // the timer catches one made after that.
                if !again {
                    shell::set_flyout_hidden(true);
                }
                SetTimer(Some(self.hwnd), TIMER_FLYOUT, 60, None);
            }
        }
        if again {
            // Already open at the right size: only the level moves.
            self.dirty = true;
        } else {
            self.retarget();
        }
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
        if hwnd == self.quick.hwnd {
            return self.handle_quick(m, wp, lp);
        }
        if hwnd == self.deskosd.hwnd {
            if m == WM_TIMER && wp.0 == deskosd::TIMER_HIDE {
                self.deskosd.on_timer();
                return Some(LRESULT(0));
            }
            return None;
        }
        if self.clips.as_ref().is_some_and(|c| c.hwnd == hwnd) {
            return self.handle_clips(m, wp, lp);
        }
        if hwnd == self.widgets.hwnd {
            return self.handle_widgets(m, wp, lp);
        }
        if hwnd == self.switcher.overlay {
            return self.handle_switcher(m, wp, lp);
        }
        if m == msg::APPBAR {
            if let Some(i) = self.screens.iter().position(|d| d.hwnd == hwnd) {
                // Another screen's taskbar: only that screen is affected.
                match wp.0 as u32 {
                    ABN_FULLSCREENAPP => {
                        self.screens[i].fullscreen = lp.0 != 0;
                        self.show_docks();
                    }
                    ABN_POSCHANGED => self.screens[i].reposition(),
                    _ => {}
                }
                return Some(LRESULT(0));
            }
            match wp.0 as u32 {
                ABN_FULLSCREENAPP => self.set_fullscreen(lp.0 != 0),
                ABN_POSCHANGED if hwnd == self.dock.hwnd => {
                    self.dock.reposition();
                    mouseedge::raise();
                }
                _ => {}
            }
            return Some(LRESULT(0));
        }
        if m == self.taskbar_created && m != 0 {
            // Explorer made its taskbar (sign-in, or it restarted): its tray
            // window now sits in front of ours, so take the front back and
            // have the apps add their icons here. Our own announcement comes
            // back to us too, hence the pause.
            let echo = self.tray_announced.is_some_and(|t| t.elapsed() < Duration::from_secs(2));
            if self.systray.hwnd.is_some() && !echo {
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_TRAYSYNC, 500, None);
                }
            }
            if !echo {
                self.on_shell_ready();
            }
            return Some(LRESULT(0));
        }
        if m == self.shell_msg && m != 0 {
            self.on_shell(wp.0 as u32, HWND(lp.0 as *mut _));
            return Some(LRESULT(0));
        }
        match m {
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                self.update_display();
                self.apply_edges(true);
                self.configure_docks();
                self.show_docks();
                if self.cfg.dock {
                    self.scan_docks();
                }
                self.retarget();
                return Some(LRESULT(0));
            }
            WM_DWMCOLORIZATIONCOLORCHANGED if hwnd == self.hwnd => {
                theme::refresh_accent();
                if theme::follows_accent(&self.cfg.theme) {
                    self.apply_dock();
                    self.restyle();
                }
                return Some(LRESULT(0));
            }
            WM_SETTINGCHANGE if wp.0 as u32 == SPI_SETWORKAREA.0 => {
                self.dock.refresh_bottom();
                for d in &mut self.screens {
                    d.refresh_bottom();
                }
                if hwnd == self.hwnd {
                    // A taskbar came or went: the notch keeps clear of it.
                    self.update_display();
                }
                return None;
            }
            _ => {}
        }
        if let Some(i) = self.dock_index(hwnd) {
            return self.handle_dock(i, hwnd, m, wp, lp);
        }
        if hwnd == self.preview.hwnd {
            return self.handle_preview(m, wp, lp);
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
            // A sideways swipe on a touchpad (or tilting the wheel) flips tabs
            // the way it moves.
            WM_MOUSEHWHEEL if self.expanded => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16;
                self.on_wheel(delta.saturating_neg());
            }
            WM_TIMER => self.on_timer(wp.0),
            msg::VOLUME => {
                let vol = (wp.0 as f32 / 1000.0, lp.0 != 0);
                // The taskbar's speaker only changes at a few levels: redraw
                // the whole bar for those, not for every step of the volume.
                if speaker_tier(vol.0, vol.1) != speaker_tier(self.vol.0, self.vol.1) {
                    self.dock.dirty = true;
                }
                self.vol = vol;
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
            // Windows' brightness setting changes as soon as the keys are
            // pressed, well before the WMI event comes in.
            WM_POWERBROADCAST => {
                if wp.0 == PBT_APMPOWERSTATUSCHANGE {
                    self.on_power();
                }
                if let Some(level) = unsafe { brightness::power_setting_level(wp, lp) } {
                    let changed = level != self.brightness;
                    self.brightness = level;
                    let in_controls = self.expanded && self.mode == Mode::Controls;
                    if in_controls {
                        self.dirty = true;
                    } else if changed && self.brightness_ok && self.cfg.brightness_osd {
                        self.show_peek(PeekKind::Brightness);
                    }
                }
                return Some(LRESULT(1));
            }
            msg::BRIGHTNESS_LEVEL => {
                self.brightness = wp.0 as u32;
                self.brightness_ok = lp.0 != 0;
                self.dirty = true;
            }
            msg::UPDATE => self.on_update(wp.0, lp.0 as u32),
            msg::CLIP_PASTE => {
                // The item is on the clipboard: back to the app, then paste.
                if wp.0 == 1 && !self.clip_target.is_invalid() {
                    dock::focus_window(self.clip_target);
                    unsafe {
                        SetTimer(Some(self.hwnd), TIMER_PASTE, 60, None);
                    }
                }
            }
            WM_DEVICECHANGE => {
                if let Some(c) = unsafe { devices::parse(wp, lp) } {
                    self.on_device(c);
                }
                return Some(LRESULT(1));
            }
            msg::RADIOS => {
                let was = self.radios.wifi;
                let bt_was = self.radios.bluetooth;
                self.radios = Radios::unpack(wp.0);
                // Bluetooth came on with its list open: the devices can connect now.
                if self.bt_open && self.radios.bluetooth == Some(true) && bt_was != Some(true) {
                    let _ = self.bt_tx.send(bt::Cmd::List);
                }
                // Wi-Fi just came on with the list open: see what's there.
                if self.wifi_open && self.radios.wifi == Some(true) && was != Some(true) {
                    let _ = self.wifi_tx.send(wifi::Cmd::Scan);
                }
                self.dirty = true;
                self.dock.dirty = true;
            }
            msg::SPECTRUM => {
                if self.visualizer.running() {
                    for (b, v) in self.bars.iter_mut().zip(audio::unpack(wp.0)) {
                        // A beat shows the moment it's heard: bars jump up and
                        // only ease back down (springing up lagged ~70 ms).
                        if v > b.value {
                            b.snap(v);
                            self.bars_fresh = true;
                        } else {
                            b.set(v);
                        }
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

    /// Windows says the power changed: the charger went in or out, or the
    /// battery's level moved. The battery icons follow it straight away, and
    /// plugging in or out shows in the notch (`charge_osd`).
    fn on_power(&mut self) {
        let now = battery();
        let before = std::mem::replace(&mut self.power, now);
        self.dirty = true;
        self.dock.dirty = true;
        if let (Some((_, was)), Some((_, plugged))) = (before, now) {
            if was != plugged && self.cfg.charge_osd {
                self.show_peek(PeekKind::Power);
            }
        }
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

    /// A USB device or serial port came or went (see `devices`).
    fn on_device(&mut self, change: devices::Change) {
        let all = match self.cfg.usb_popups.as_str() {
            "off" => return,
            mode => mode == "all",
        };
        match change {
            devices::Change::Arrived(path) => {
                if let Some(device) = devices::describe(&path, all) {
                    self.device =
                        Some(DeviceNote { device, port: None, removed: false, at: Instant::now(), hot: false });
                    self.show_peek(PeekKind::Device);
                }
            }
            devices::Change::Removed(path) if self.cfg.usb_unplug => {
                if let Some(device) = devices::describe(&path, all) {
                    self.device =
                        Some(DeviceNote { device, port: None, removed: true, at: Instant::now(), hot: false });
                    self.show_peek(PeekKind::Device);
                    unsafe {
                        SetTimer(Some(self.hwnd), TIMER_PEEK, 2500, None);
                    }
                }
            }
            devices::Change::Removed(_) => {}
            // The serial port the device just plugged in got (it comes a moment later).
            devices::Change::Port(port) => {
                let fresh = |d: &DeviceNote| {
                    d.device.serial && !d.removed && d.port.is_none() && d.at.elapsed() < Duration::from_secs(10)
                };
                if let Some(d) = self.device.as_mut().filter(|d| fresh(d)) {
                    d.port = Some(port);
                    if self.peek == Some(PeekKind::Device) {
                        self.dirty = true;
                    } else {
                        self.show_peek(PeekKind::Device);
                    }
                }
            }
        }
    }

    /// Hands the calendar reader its link when it changed (empty stops it).
    fn apply_calendar(&mut self) {
        if self.cfg.calendar_url.trim() != self.cal_url {
            self.cal_url = self.cfg.calendar_url.trim().to_string();
            let _ = self.cal_tx.send(self.cal_url.clone());
            if self.cal_url.is_empty() {
                self.events.clear();
                self.retarget();
                self.arm_calendar();
            }
        }
    }

    fn on_events(&mut self, events: Vec<ics::Event>) {
        if self.cal_url.is_empty() {
            return; // read before the link was taken away
        }
        self.events = events;
        self.widgets.dirty = true;
        self.retarget();
        self.arm_calendar();
    }

    /// The event counted down to on the closed notch: the next one starting
    /// within `calendar_soon` minutes, and how far through that wait it is.
    fn countdown(&self) -> Option<(&ics::Event, f32)> {
        let soon = self.cfg.calendar_soon as i64 * 60;
        if soon == 0 || self.cal_url.is_empty() {
            return None;
        }
        let now = calendar::now();
        let e = self.events.iter().find(|e| !e.all_day && e.start > now && e.start - now <= soon)?;
        Some((e, 1.0 - (e.start - now) as f32 / soon as f32))
    }

    /// Says in the notch when an event is coming up, and when it starts.
    fn announce_events(&mut self) {
        let now = calendar::now();
        let soon = self.cfg.calendar_soon as i64 * 60;
        let mut say = None;
        for e in self.events.iter().filter(|e| !e.all_day) {
            let key = |k: u8| (e.start, k);
            if e.start <= now && now - e.start < 120 && !self.cal_said.contains(&key(1)) {
                say = Some((key(1), format!("{} \u{2014} now", e.title)));
                break;
            }
            if soon > 0 && e.start > now && e.start - now <= soon && !self.cal_said.contains(&key(0)) {
                let mins = ((e.start - now) as f32 / 60.0).round().max(1.0);
                say = Some((key(0), format!("{} in {mins:.0} min", e.title)));
                break;
            }
        }
        if let Some((key, text)) = say {
            self.cal_said.push(key);
            // Only today's are worth remembering.
            self.cal_said.retain(|(t, _)| now - t < DAY);
            self.notify(text, theme::accent(), false);
        }
    }

    /// Wakes up at the calendar's next moment: every 30 s while counting
    /// down (to move the line), else when the next countdown or event starts.
    fn arm_calendar(&mut self) {
        let now = calendar::now();
        let soon = self.cfg.calendar_soon as i64 * 60;
        let next = self
            .events
            .iter()
            .filter(|e| !e.all_day)
            .flat_map(|e| [e.start - soon, e.start])
            .filter(|&t| t > now)
            .min();
        unsafe {
            match next {
                _ if self.countdown().is_some() => {
                    SetTimer(Some(self.hwnd), TIMER_CAL, 30_000, None);
                }
                Some(t) => {
                    let ms = ((t - now).clamp(1, 3600) * 1000) as u32;
                    SetTimer(Some(self.hwnd), TIMER_CAL, ms, None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), TIMER_CAL);
                }
            }
        }
    }

    /// The next few events, for the Calendar tab.
    fn agenda(&self) -> Vec<&ics::Event> {
        let now = calendar::now();
        let until = (now.div_euclid(DAY) + 7) * DAY;
        calendar::upcoming(&self.events, now).filter(|e| e.start < until).take(3).collect()
    }

    /// Room for the agenda under the month.
    fn agenda_h(&self) -> f32 {
        match self.agenda().len() {
            0 => 0.0,
            n => 10.0 + n as f32 * 22.0,
        }
    }

    fn device_hot(&self) -> bool {
        self.device.as_ref().is_some_and(|d| d.hot)
    }

    fn device_removed(&self) -> bool {
        self.device.as_ref().is_some_and(|d| d.removed)
    }

    /// Cascade started late with Windows (see `shell::hint_late_autostart`).
    fn on_late_autostart(&mut self) {
        let text = "Cascade can start sooner \u{2014} right-click the taskbar";
        self.notify(text.into(), theme::accent(), false);
    }

    fn on_update(&mut self, outcome: usize, build: u32) {
        match outcome {
            update::AVAILABLE => {
                if self.update_available != Some(build) {
                    self.update_available = Some(build);
                    self.notify(
                        format!("Update available (build {build}) \u{2014} right-click to install"),
                        theme::accent(),
                        false,
                    );
                }
            }
            update::UP_TO_DATE => {
                self.updating = false;
                self.notify(format!("Cascade is up to date (build {build})"), GREEN, true);
            }
            update::INSTALLING => self.notify(format!("Downloading build {build}\u{2026}"), theme::accent(), false),
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
        if let Some(m) = &info {
            // The bars follow the app that's playing, not everything.
            self.visualizer.set_source(&m.source);
        }
        self.media = info;
        // A new song, its art, or play / pause on the music widget.
        self.dock.dirty = true;
        self.widgets.dirty = true;
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
            if self.peek == Some(PeekKind::Device) && !self.expanded {
                // A device pop-up stays while pointed at, to be clicked.
                let _ = KillTimer(Some(self.hwnd), TIMER_PEEK);
                if !self.device_hot() {
                    if let Some(d) = &mut self.device {
                        d.hot = true;
                        self.dirty = true;
                    }
                }
                return;
            }
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
        if self.peek == Some(PeekKind::Device) && !self.expanded {
            if let Some(d) = &mut self.device {
                d.hot = false;
            }
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_PEEK, 2500, None);
            }
            self.dirty = true;
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
        if let Some(bar @ (Hot::VolumeBar | Hot::BrightnessBar | Hot::AppBar(_))) = self.hit(x, y) {
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

    /// The same for an app's slider in the mixer.
    fn app_slider_value(&self, x: i32) -> f32 {
        let (x0, x1) = (MIX_SLIDER_X, self.content_w - 60.0);
        ((x as f32 / self.k - self.origin - x0) / (x1 - x0)).clamp(0.0, 1.0)
    }

    /// Reads the mixer (the apps playing and the outputs) for Controls.
    fn read_mixer(&mut self) {
        self.outputs_open = false;
        self.mixer = if self.cfg.controls_mixer { Mixer::read() } else { Mixer::default() };
        // And the Bluetooth tile names what's connected.
        if self.radios.bluetooth == Some(true) {
            let _ = self.bt_tx.send(bt::Cmd::List);
        }
    }

    /// The mixer's rows: the output (and the list of them when open), then
    /// up to `MIX_APPS` apps.
    fn mixer_h(&self) -> f32 {
        if self.mixer.outputs.is_empty() {
            return 0.0;
        }
        let list = if self.outputs_open { self.mixer.outputs.len() as f32 * MIX_ITEM_H } else { 0.0 };
        MIX_GAP + MIX_ROW + list + self.mixer.apps.len().min(MIX_APPS) as f32 * MIX_ROW
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
            Hot::AppBar(i) => {
                let v = self.app_slider_value(x);
                self.mixer.set_level(i, v);
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
        if self.peek == Some(PeekKind::Device) && !self.expanded {
            // Opens what the device goes with (Betaflight for a flight controller).
            if let Some(open) =
                self.device.as_ref().and_then(|d| d.device.open.clone()).filter(|_| !self.device_removed())
            {
                shell::open(&open);
            }
            self.device = None;
            self.peek = None;
            self.retarget();
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
                // Cascade's widgets panel holds the notifications when its
                // taskbar is on; without one there's nowhere to anchor it.
                if self.cfg.dock {
                    self.expanded = false;
                    self.toggle_widgets();
                } else {
                    shell::open("ms-actioncenter:");
                    self.expanded = false;
                }
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
            Hot::Output => self.outputs_open = !self.outputs_open && self.mixer.outputs.len() > 1,
            Hot::OutputItem(i) => {
                if i != self.mixer.current && self.mixer.set_output(i) {
                    // The apps now play through the new device.
                    self.read_mixer();
                }
                self.outputs_open = false;
            }
            Hot::AppMute(i) => self.mixer.toggle_mute(i),
            Hot::MediaSource => {
                let _ = self.media_tx.send(MediaCmd::NextSource);
            }
            Hot::WifiMore => {
                self.wifi_open = true;
                self.wifi.note = None;
                let _ = self.wifi_tx.send(wifi::Cmd::Scan);
            }
            Hot::WifiBack => self.wifi_open = false,
            Hot::BtMore => {
                self.bt_open = true;
                self.bt.note = None;
                let _ = self.bt_tx.send(bt::Cmd::List);
            }
            Hot::BtBack => self.bt_open = false,
            Hot::BtDev(i) => match self.bt.devices.get(i) {
                Some(d) if d.audio && self.radios.bluetooth != Some(false) => {
                    let _ = self.bt_tx.send(bt::Cmd::Set(d.name.clone(), !d.connected));
                }
                // Mice, keyboards and the like connect by themselves.
                Some(_) => {
                    shell::open("ms-settings:bluetooth");
                    self.expanded = false;
                }
                None => {}
            },
            Hot::BtSettings => {
                shell::open("ms-settings:bluetooth");
                self.expanded = false;
            }
            Hot::WifiNet(i) => {
                if let Some(n) = self.wifi.nets.get(i).filter(|n| !n.connected) {
                    self.wifi.note = Some(format!("Joining {}\u{2026}", n.ssid));
                    let _ = self.wifi_tx.send(wifi::Cmd::Connect(n.clone()));
                }
            }
            Hot::WifiSettings => {
                shell::open("ms-settings:network-wifi");
                self.expanded = false;
            }
            Hot::WifiQuick => {
                // Windows' own quick settings, for anything Controls doesn't do
                // (Cascade lets an injected Win+A through even when it takes the key).
                shell::win_combo(b'A' as u16);
                self.expanded = false;
            }
            Hot::VolumeBar | Hot::BrightnessBar | Hot::AppBar(_) => {}
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
            if ![TIMER_TICK, TIMER_POMODORO, TIMER_SWITCH, TIMER_LENS].contains(&id) {
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
                self.tab.snap(0.0);
                self.retarget();
            }
            TIMER_PEEK => {
                if self.peek == Some(PeekKind::Device) {
                    self.device = None;
                }
                self.peek = None;
                self.retarget();
            }
            TIMER_TICK => self.dirty = true,
            TIMER_PASTE => shell::paste(),
            TIMER_CAL => {
                self.announce_events();
                self.retarget();
                self.arm_calendar();
            }
            TIMER_LENS => {
                // Nothing left to refresh (taskbar hidden for a game, notch off...): stop.
                if !crate::lens::refresh() {
                    self.lens_on = false;
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), TIMER_LENS);
                    }
                }
            }
            TIMER_FLYOUT => shell::set_flyout_hidden(self.flyout_hidden),
            TIMER_SHELL => {
                if shell::shell_ready() {
                    self.on_shell_ready();
                }
            }
            TIMER_KEYFIX => keyhook::fix_stuck(),
            TIMER_NOTIFS => {
                if self.widgets.open && (self.cfg.widgets_notifications || self.widgets.shows_phone()) {
                    let _ = self.notif_tx.send(NotifCmd::Refresh);
                    unsafe {
                        SetTimer(Some(self.hwnd), TIMER_NOTIFS, 3000, None);
                    }
                }
            }
            TIMER_TRAYSYNC => {
                if let Some(h) = self.systray.hwnd {
                    self.tray_announced = Some(Instant::now());
                    systray::announce(h);
                    if self.tray_syncs > 0 {
                        self.tray_syncs -= 1;
                        unsafe {
                            SetTimer(Some(self.hwnd), TIMER_TRAYSYNC, 8000, None);
                        }
                    }
                }
            }
            TIMER_SWITCH => {
                self.switcher.show_now();
                // Alt let go without the hook seeing it (an admin window took the
                // keys): finish the switch rather than leave the panel up.
                if self.switcher.active && unsafe { GetAsyncKeyState(VK_MENU.0 as i32) } as u16 & 0x8000 == 0 {
                    self.finish_switch();
                }
            }
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
        if event == EVENT_OBJECT_SHOW {
            return; // only watched for the taskbar above
        }
        // Windows opened its own Start menu: the Win key reached it without
        // passing our hook (an elevated app such as Task Manager was in
        // front), or its Start button was clicked. Show ours instead; taking
        // the foreground closes theirs.
        // Pressing Win to close our menu can let that press through to Windows
        // as well (it opens theirs over ours): shut theirs rather than bring
        // ours back, so the menu closes.
        if event == EVENT_SYSTEM_FOREGROUND
            && self.cfg.launcher
            && self.cfg.launcher_win_key
            && self.win_closed_at.is_some_and(|t| t.elapsed() < Duration::from_millis(800))
            && shell::is_start_menu(hwnd)
        {
            config::log("start: the Windows Start menu opened as Cascade's closed; closing it");
            self.launcher.hide();
            shell::close_start_menu();
            return;
        }
        // Windows' menu came up over ours (the Win key reached both): take the
        // foreground back, which closes theirs and leaves ours alone.
        if event == EVENT_SYSTEM_FOREGROUND && self.cfg.launcher && self.launcher.open && shell::is_start_menu(hwnd) {
            config::log("start: the Windows Start menu opened over Cascade's; closing it");
            launcher::force_foreground(self.launcher.hwnd);
            return;
        }
        if event == EVENT_SYSTEM_FOREGROUND
            && self.cfg.launcher
            && self.cfg.launcher_win_key
            && !self.launcher.open
            && shell::is_start_menu(hwnd)
        {
            config::log("start: the Windows Start menu opened anyway; showing Cascade's");
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
                let mut known = false;
                let mut moved = false;
                for d in std::iter::once(&mut self.dock).chain(self.screens.iter_mut()) {
                    d.foreground = hwnd;
                    d.dirty = true;
                    known |= d.tracks(hwnd);
                    moved |= d.misplaced(hwnd);
                }
                !known || moved
            }
            EVENT_SYSTEM_MOVESIZEEND => {
                std::iter::once(&self.dock).chain(self.screens.iter()).any(|d| d.misplaced(hwnd))
                    || (self.cfg.taskbar_screens == "all" && self.cfg.taskbar_screen_apps == "screen")
            }
            _ => unsafe { GetAncestor(hwnd, GA_PARENT) == GetDesktopWindow() },
        };
        if rescan {
            unsafe {
                SetTimer(Some(self.dock.hwnd), TIMER_SCAN, 120, None);
            }
        }
    }

    /// A top-level window was created or destroyed (the shell's notification).
    fn on_shell(&mut self, code: u32, hwnd: HWND) {
        const HSHELL_WINDOWCREATED: u32 = 1;
        const HSHELL_WINDOWDESTROYED: u32 = 2;
        if !self.cfg.dock {
            return;
        }
        let dock = self.dock.hwnd;
        unsafe {
            match code {
                // Many windows are created hidden and shown a moment later.
                HSHELL_WINDOWCREATED => {
                    SetTimer(Some(dock), TIMER_SCAN, 300, None);
                    SetTimer(Some(dock), TIMER_SCAN2, 1500, None);
                }
                HSHELL_WINDOWDESTROYED
                    if std::iter::once(&self.dock).chain(self.screens.iter()).any(|d| d.tracks(hwnd)) =>
                {
                    SetTimer(Some(dock), TIMER_SCAN, 120, None);
                }
                _ => {}
            }
        }
    }

    /// Watches Explorer's own windows, to hide the taskbar again if it shows
    /// it. Scoped to one process, so it hears almost nothing.
    fn hook_taskbar(&mut self) {
        if let Some(h) = self.taskbar_hook.take() {
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
        if !self.taskbar_hidden {
            return;
        }
        let Some(tray) = shell::explorer_tray() else { return };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(tray, Some(&mut pid)) };
        if pid == 0 {
            return;
        }
        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        let h = unsafe { SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, None, Some(win_event), pid, 0, flags) };
        if !h.is_invalid() {
            self.taskbar_hook = Some(h);
        }
    }

    /// (Re)builds the pointer-matching strips when their settings, or the
    /// screens, changed.
    fn apply_edges(&mut self, force: bool) {
        let key = format!("{}|{}|{}", self.cfg.mouse_match, self.cfg.mouse_align, self.cfg.screen_inches);
        if force || key != self.edge_key {
            self.edge_key = key;
            mouseedge::configure(&self.cfg);
        }
    }

    fn right_click_target(&mut self, hwnd: HWND) -> RightClick {
        if hwnd == self.widgets.hwnd || hwnd == self.switcher.overlay {
            return RightClick::Nothing;
        }
        if self.clips.as_ref().is_some_and(|c| c.hwnd == hwnd) {
            return RightClick::Nothing;
        }
        if hwnd == self.flyout.hwnd {
            if let Some(i) = self.flyout.hovered() {
                self.systray.click(i, true);
            }
            return RightClick::Nothing;
        }
        let on_dock = self.screens.iter().position(|d| d.hwnd == hwnd).map(|i| i + 1);
        let on_dock = if hwnd == self.dock.hwnd { Some(0) } else { on_dock };
        if let Some(di) = on_dock {
            let hovered = self.dock_ref(di).hovered;
            if let Some(dock::Hot::TrayIcon(i)) = hovered {
                self.systray.click(i, true);
                return RightClick::Nothing;
            }
            // Right-clicking Start opens the Win+X menu, as in Windows.
            if hovered == Some(dock::Hot::Start) && self.cfg.shortcut_win_x {
                self.open_quick(false);
                return RightClick::Nothing;
            }
        }
        if hwnd == self.quick.hwnd {
            return RightClick::Nothing;
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
        if let Some(di) = on_dock {
            let d = self.dock_ref(di);
            if let Some(i) = d.hovered_app() {
                let a = &d.apps[i];
                let target =
                    RightClick::App { id: a.id.clone(), name: d.name(i), pinned: a.pinned, windows: a.windows.len() };
                self.menu_dock = di;
                return target;
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
            autostart_late: shell::autostart_late(),
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
            // Sets up the sign-in task (it asks for permission once), which
            // then takes over from the startup list.
            MenuCmd::AutostartSooner => shell::set_autostart(true),
            MenuCmd::Flyout => {
                flip(&mut self.cfg.hide_windows_flyout, "hide_windows_flyout");
                self.apply_flyout();
            }
            MenuCmd::OpenConfig => shell::open_file(&config::path()),
            MenuCmd::Update => {
                if !self.updating {
                    self.updating = true;
                    self.notify("Checking for updates\u{2026}".into(), theme::accent(), false);
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
        theme::set(&self.cfg.theme);
        theme::set_colour(&self.cfg.theme_color, self.cfg.theme_hue, self.cfg.theme_intensity);
        theme::set_edge(self.cfg.theme_edge);
        theme::set_refraction(self.cfg.refraction);
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
        self.perf = Perf::new(self.cfg.perf_log);
        self.apply_calendar();
        self.update_display();
        self.apply_taskbar();
        self.apply_flyout();
        self.apply_dock();
        self.retarget();
        self.restyle();
        if let Some(s) = &self.settings {
            s.invalidate();
        }
    }

    /// Keeps clear glass's bent edges up to date with what's behind them, a
    /// few times a second, while the theme bends light and a ring is on
    /// screen (`frame` starts it again when one comes back).
    fn arm_lens(&mut self) {
        self.lens_wanted = theme::refracts(self.cfg.acrylic);
        let on = self.lens_wanted && crate::lens::active();
        if on != self.lens_on {
            self.lens_on = on;
            unsafe {
                if on {
                    SetTimer(Some(self.hwnd), TIMER_LENS, 50, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), TIMER_LENS);
                }
            }
        }
    }

    /// Redraws every surface, e.g. after the theme changed.
    fn restyle(&mut self) {
        self.gfx.clear_cache();
        self.arm_lens();
        self.dirty = true;
        self.dock.dirty = true;
        self.launcher.dirty = true;
        self.flyout.dirty = true;
        self.widgets.dirty = true;
    }

    // --------------------------------------------------- launcher glue --

    /// A click landed outside every open pop-up: close them.
    fn on_outside_click(&mut self) {
        let mut closed = false;
        if self.pinned {
            self.on_timer(TIMER_COLLAPSE);
            closed = true;
        }
        // While a right-click menu is up, its clicks are "outside" too.
        if self.launcher.open && !self.launcher_menu {
            self.launcher.hide();
            closed = true;
        }
        if self.flyout.open {
            self.flyout.hide();
            closed = true;
        }
        if self.quick.open {
            self.quick.hide();
            closed = true;
        }
        if let Some(c) = self.clips.as_mut().filter(|c| c.open) {
            c.hide();
            closed = true;
        }
        if self.widgets.open {
            self.widgets.hide();
            closed = true;
        }
        if closed {
            self.outside_close = Some(Instant::now());
        }
    }

    fn just_closed(&self) -> bool {
        self.outside_close.is_some_and(|t| t.elapsed() < Duration::from_millis(400))
    }

    fn toggle_launcher(&mut self) {
        if self.launcher.open {
            self.win_closed_at = Some(Instant::now());
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
        // On the screen the pointer is on, over its taskbar.
        self.pop = self.dock_under_cursor();
        // The taskbar comes up with the menu, even if it was hidden for a
        // fullscreen app or is set to auto-hide.
        self.pd_mut().set_visible(true);
        self.dock_held = true;
        self.pd_mut().set_held(true);
        let k = self.pd().k();
        self.launcher.show(k, self.dock.pinned_apps());
    }

    fn launcher_bottom(&self) -> i32 {
        let d = self.pd();
        if d.shown {
            d.panel_top()
        } else if self.pop == 0 {
            shell::work_area().bottom
        } else {
            shell::work_area_of(d.monitor()).bottom
        }
    }

    fn launcher_action(&mut self, action: launcher::Action) {
        match action {
            launcher::Action::Launch(target) => {
                shell::open(&target);
                self.launcher.hide();
            }
            launcher::Action::Close => self.launcher.hide(),
            launcher::Action::Copy(text) => {
                clipboard::copy(&text);
                self.launcher.hide();
            }
            launcher::Action::Settings => {
                self.launcher.hide();
                self.open_settings();
            }
            launcher::Action::Power(p) => {
                self.launcher.hide();
                shell::power(p);
            }
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
            Change::Many(values) => {
                if values.iter().any(|(k, v)| self.cfg.get(k) != *v) {
                    for (k, v) in &values {
                        Config::set(k, v);
                    }
                    self.reload_config();
                }
            }
            Change::Set(key, v) if key.starts_with("place_") => {
                let (left, right) = self.cfg.with_place(&key["place_".len()..], &v);
                Config::set("taskbar_left", &left);
                Config::set("taskbar_right", &right);
                self.reload_config();
                // The order boxes on this page now hold old text; reload them.
                if let Some(s) = &mut self.settings {
                    let cfg = &self.cfg;
                    s.sync_edits(&|k: &str| setting_value(cfg, k));
                }
            }
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
            Change::Action("edit_devices") => devices::edit_list(),
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
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lp);
                s.on_up(x, y, &get)
            }
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
        let di = self.menu_dock;
        let d = self.dock_mut(di);
        let Some(i) = d.apps.iter().position(|a| a.id == id) else { return };
        match cmd {
            AppCmd::NewWindow => d.launch_new(i),
            AppCmd::TogglePin => d.toggle_pin(i),
            AppCmd::CloseAll => {
                d.close_all(i);
                self.preview.hide();
            }
            AppCmd::ForceClose => {
                d.force_close(i);
                self.preview.hide();
            }
            AppCmd::None => {}
        }
    }

    // ------------------------------------------------------- dock glue --

    /// Events for taskbar `di` (0: the main one, then `screens`).
    fn handle_dock(&mut self, di: usize, hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let dock = self.dock_ref(di).hwnd;
        unsafe {
            match m {
                WM_MOUSEMOVE if hwnd == self.dock_ref(di).trigger => {
                    self.dock_mut(di).show_now();
                    SetTimer(Some(dock), TIMER_DOCK_HIDE, 1500, None);
                }
                WM_MOUSEMOVE => {
                    let _ = KillTimer(Some(dock), TIMER_DOCK_HIDE);
                    let _ = KillTimer(Some(self.dock_ref(self.preview_dock).hwnd), TIMER_PREVIEW_HIDE);
                    let (x, y) = lparam_xy(lp);
                    if self.dock_mut(di).on_move(x, y) {
                        self.schedule_preview(di);
                    }
                    self.hover_misses = 0;
                    SetTimer(Some(dock), TIMER_HOVER, 250, None);
                }
                WM_MOUSELEAVE => {
                    self.dock_mut(di).on_leave();
                    SetTimer(Some(dock), TIMER_DOCK_HIDE, 600, None);
                    if self.preview.visible() {
                        SetTimer(Some(dock), TIMER_PREVIEW_HIDE, 300, None);
                    }
                    let _ = KillTimer(Some(dock), TIMER_PREVIEW_SHOW);
                }
                WM_LBUTTONDOWN => {
                    let (x, y) = lparam_xy(lp);
                    self.dock_mut(di).on_down(x, y);
                }
                WM_LBUTTONUP => {
                    let (x, y) = lparam_xy(lp);
                    if let Some(hot) = self.dock_mut(di).on_up(x, y) {
                        self.preview.hide();
                        self.on_dock_click(di, hot);
                    }
                    self.save_chips(di);
                }
                // Another window took the mouse mid-press: no release will come.
                WM_CAPTURECHANGED => {
                    self.dock_mut(di).on_capture_lost();
                    self.save_chips(di);
                }
                // Scrolling over the desktops widget goes to the next or previous desktop.
                WM_MOUSEWHEEL => {
                    if self.dock_ref(di).hovered != Some(DockHot::Chip(dock::Chip::Desktops)) {
                        return None;
                    }
                    let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16;
                    let (n, cur) = (self.desks.names.len(), self.desks.current);
                    let to = if delta < 0 { (cur + 1).min(n.saturating_sub(1)) } else { cur.saturating_sub(1) };
                    desktops::switch(cur, to);
                }
                WM_MBUTTONUP => {
                    let d = self.dock_mut(di);
                    if let Some(i) = d.hovered_app() {
                        d.launch_new(i);
                    }
                }
                WM_TIMER => {
                    let _ = KillTimer(Some(dock), wp.0);
                    match wp.0 {
                        TIMER_SCAN | TIMER_SCAN2 => {
                            if self.systray.prune() {
                                self.dock.dirty = true;
                            }
                            self.scan_docks();
                            self.refresh_preview();
                        }
                        TIMER_DOCK_HIDE => {
                            if self.preview.visible() && self.preview_dock == di {
                                SetTimer(Some(dock), TIMER_DOCK_HIDE, 600, None);
                            } else {
                                self.dock_mut(di).hide_if_idle();
                            }
                        }
                        TIMER_PREVIEW_SHOW => self.show_preview(),
                        TIMER_PREVIEW_HIDE => self.preview.hide(),
                        TIMER_HOVER => self.check_hover(di),
                        TIMER_CLOCK => {
                            self.dock.dirty = true;
                            for d in &mut self.screens {
                                d.dirty = true;
                            }
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

    // ----------------------------------------------- dropping files --

    /// The app under screen point (x, y) on taskbar `di`, lit as if hovered,
    /// and its front window.
    fn app_at(&mut self, di: usize, x: i32, y: i32) -> Option<(usize, Option<HWND>)> {
        let d = self.dock_mut(di);
        let mut p = POINT { x, y };
        unsafe {
            let _ = ScreenToClient(d.hwnd, &mut p);
        }
        d.on_move(p.x, p.y);
        let i = d.hovered_app()?;
        Some((i, d.apps[i].windows.first().copied()))
    }

    /// Files dragged over taskbar `di`: true over an app (which takes them).
    /// Held over an app with windows for a moment, its window comes up.
    fn on_drag(&mut self, di: usize, x: i32, y: i32) -> bool {
        let Some((i, window)) = self.app_at(di, x, y) else {
            self.drag = None;
            return false;
        };
        match &mut self.drag {
            Some((d, a, since, raised)) if *d == di && *a == i => {
                if !*raised && since.elapsed() > Duration::from_millis(700) {
                    *raised = true;
                    if let Some(w) = window {
                        dock::focus_window(w);
                    }
                }
            }
            _ => self.drag = Some((di, i, Instant::now(), false)),
        }
        true
    }

    fn on_drag_leave(&mut self, di: usize) {
        self.drag = None;
        self.dock_mut(di).on_leave();
    }

    /// Files dropped on taskbar `di`: opened with the app they're on.
    fn on_drop(&mut self, di: usize, x: i32, y: i32, files: Vec<String>) -> bool {
        self.drag = None;
        let app = self.app_at(di, x, y);
        self.dock_mut(di).on_leave();
        let Some((i, _)) = app else { return false };
        let id = self.dock_ref(di).apps[i].id.clone();
        if id.to_ascii_lowercase().ends_with(".exe") {
            let args: Vec<String> = files.iter().map(|f| format!("\"{f}\"")).collect();
            shell::launch(&id, &args.join(" "), false);
        } else {
            // A Store app can't be handed files this way: open them as usual.
            for f in &files {
                shell::open(f);
            }
        }
        true
    }

    // ------------------------------------------------- other screens --

    fn dock_ref(&self, di: usize) -> &Dock {
        match di.checked_sub(1) {
            Some(i) if i < self.screens.len() => &self.screens[i],
            _ => &self.dock,
        }
    }

    fn dock_mut(&mut self, di: usize) -> &mut Dock {
        match di.checked_sub(1) {
            Some(i) if i < self.screens.len() => &mut self.screens[i],
            _ => &mut self.dock,
        }
    }

    /// Which taskbar `hwnd` is (or its auto-hide strip): 0 the main one.
    fn dock_index(&self, hwnd: HWND) -> Option<usize> {
        if self.dock.is(hwnd) {
            return Some(0);
        }
        self.screens.iter().position(|d| d.is(hwnd)).map(|i| i + 1)
    }

    /// The taskbar the open pop-up belongs to.
    fn pd(&self) -> &Dock {
        self.dock_ref(self.pop)
    }

    fn pd_mut(&mut self) -> &mut Dock {
        let i = self.pop;
        self.dock_mut(i)
    }

    /// The taskbar on the screen the pointer is on (the main one if that
    /// screen has none).
    fn dock_under_cursor(&self) -> usize {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        let inside = |r: RECT| p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom;
        self.screens.iter().position(|d| d.shown && inside(d.monitor())).map_or(0, |i| i + 1)
    }

    /// Applies the config to the main taskbar, and makes, fits or takes away
    /// the other screens' ones to match the screens there are.
    fn configure_docks(&mut self) {
        self.dock.configure(&self.cfg, self.monitor, self.dpi);
        let want = if self.cfg.dock && self.cfg.taskbar_screens == "all" { other_screens() } else { Vec::new() };
        let before = self.screens.len();
        if self.screens.len() > want.len() {
            for d in self.screens.drain(want.len()..) {
                d.destroy();
            }
        }
        while self.screens.len() < want.len() {
            match self.make_screen_dock() {
                Some(d) => {
                    crate::drop::register(d.hwnd, self.screens.len() + 1);
                    self.screens.push(d);
                }
                None => break,
            }
        }
        for (d, &(monitor, dpi)) in self.screens.iter_mut().zip(&want) {
            d.configure(&self.cfg, monitor, dpi);
        }
        if self.screens.len() != before {
            self.pop = 0;
            self.preview_dock = 0;
            self.menu_dock = 0;
            if self.screens.len() > before {
                self.scan_docks();
            }
        }
    }

    fn make_screen_dock(&self) -> Option<Dock> {
        let class = w!("CascadeWindow");
        let layered = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let hwnd = create_window(class, self.instance, layered).ok()?;
        let Ok(trigger) = create_window(class, self.instance, layered) else {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return None;
        };
        let backdrops = (0..4).filter_map(|_| Backdrop::create(self.instance).ok()).collect();
        Some(Dock::new_secondary(hwnd, trigger, self.dock.icon_sender(), backdrops))
    }

    /// Shows each taskbar unless a fullscreen app is in front on its screen
    /// (or a pop-up from it is open).
    fn show_docks(&mut self) {
        let hide = self.cfg.hide_in_fullscreen;
        let held = self.launcher.open || self.widgets.open || self.quick.open;
        let pop = self.pop;
        let main = !(hide && self.fullscreen) || (held && pop == 0);
        self.dock.set_visible(main);
        for (i, d) in self.screens.iter_mut().enumerate() {
            let on = !(hide && d.fullscreen) || (held && pop == i + 1);
            d.set_visible(on);
        }
    }

    fn scan_docks(&mut self) {
        self.dock.scan();
        for d in &mut self.screens {
            d.scan();
        }
    }

    /// Pinning, unpinning or reordering on one taskbar does the same on the others.
    fn sync_pins(&mut self) {
        let from = if self.dock.take_pins_changed() {
            Some(0)
        } else {
            self.screens.iter_mut().position(|d| d.take_pins_changed()).map(|i| i + 1)
        };
        let Some(from) = from else { return };
        let ids = self.dock_ref(from).pinned_ids();
        for (i, d) in std::iter::once(&mut self.dock).chain(self.screens.iter_mut()).enumerate() {
            if i != from {
                d.adopt_pins(&ids);
                d.take_pins_changed();
            }
        }
    }

    fn on_dock_click(&mut self, di: usize, hot: DockHot) {
        match hot {
            DockHot::Start | DockHot::Search => {
                if self.just_closed() {
                    // That click already closed the open menu.
                } else if self.cfg.launcher {
                    self.toggle_launcher();
                } else {
                    shell::open_start_menu();
                }
            }
            DockHot::App(i) => self.dock_mut(di).activate(i),
            DockHot::Weather => {
                let q = if self.cfg.weather_location.trim().is_empty() {
                    "weather".to_string()
                } else {
                    format!("weather {}", self.cfg.weather_location.trim())
                };
                let q = q.replace(' ', "+");
                match self.cfg.weather_click.as_str() {
                    "panel" => self.toggle_widgets(),
                    "google" => shell::open(&format!("https://www.google.com/search?q={q}")),
                    "bing" => shell::open(&format!("https://www.bing.com/search?q={q}")),
                    "custom" if !self.cfg.weather_url.trim().is_empty() => {
                        shell::open(&self.cfg.weather_url.trim().replace("{q}", &q))
                    }
                    _ => {}
                }
            }
            DockHot::Tray => self.toggle_flyout(),
            DockHot::TrayIcon(i) => self.systray.click(i, false),
            DockHot::Chip(dock::Chip::Desktops) => self.toggle_task_view(),
            DockHot::Widgets | DockHot::Chip(_) => self.toggle_widgets(),
            DockHot::MusicToggle => self.toggle_play(),
            DockHot::Status => self.on_status_click(),
            // Notifications live in the widgets panel now.
            DockHot::Clock => self.toggle_widgets(),
        }
    }

    /// The taskbar's network / volume / battery icons: the notch's Controls,
    /// or Windows' quick settings when there's no notch to open.
    fn on_status_click(&mut self) {
        if self.just_closed() {
            return; // that click already closed the notch
        }
        if self.cfg.status_click != "notch" || !self.open_controls() {
            shell::open("ms-actioncenter:controlcenter/&showFooter=true");
        }
    }

    /// Opens the notch on its Controls; with the notch turned off it comes
    /// down for them alone. False if it can't show (a fullscreen app).
    fn open_controls(&mut self) -> bool {
        if !self.shown && !self.cfg.notch {
            self.summoned = true;
            self.retarget();
        }
        if !self.shown {
            self.summoned = false;
            return false;
        }
        self.expanded = true;
        self.expand_pending = false;
        self.pinned = true;
        self.mode = Mode::Controls;
        self.read_mixer();
        self.tab.snap(1.0);
        self.hovered = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_EXPAND);
            // Moving onto the notch cancels this; leaving it starts the usual
            // collapse delay.
            SetTimer(Some(self.hwnd), TIMER_COLLAPSE, PINNED_MS, None);
        }
        self.retarget();
        true
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

    /// Alt+Tab: 0 next, 1 previous, 2 Alt released, 3 cancelled, 4 up a
    /// row, 5 down a row.
    fn on_switch(&mut self, op: usize) {
        if !self.cfg.alt_tab {
            return;
        }
        match op {
            0 | 1 => {
                if !self.switcher.active {
                    if !self.switcher.begin(self.dpi, self.monitor) {
                        return;
                    }
                    // The panel appears after a moment, so a quick flip between
                    // two windows never flashes it. The timer also guards
                    // against a missed Alt release.
                    unsafe {
                        SetTimer(Some(self.hwnd), TIMER_SWITCH, 100, None);
                    }
                }
                self.switcher.step(op == 1);
            }
            2 => self.finish_switch(),
            4 | 5 => {
                if self.switcher.active {
                    self.switcher.step_row(op == 4);
                }
            }
            _ => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_SWITCH);
                }
                self.switcher.hide();
            }
        }
    }

    fn finish_switch(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SWITCH);
        }
        if let Some(h) = self.switcher.commit() {
            dock::focus_window(h);
        }
    }

    fn handle_switcher(&mut self, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        // Fading out after a pick: it's done with input.
        if self.switcher.closing() {
            return None;
        }
        match m {
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lp);
                self.switcher.on_move(x, y);
            }
            WM_MOUSELEAVE => self.switcher.on_leave(),
            // Task View: clicking anywhere else closes it.
            WM_ACTIVATE if self.switcher.view => {
                if wp.0 & 0xFFFF == 0 {
                    self.switcher.hide();
                }
                return None;
            }
            WM_KEYDOWN | WM_SYSKEYDOWN if self.switcher.view => {
                if let Some(a) = self.switcher.on_key_view(wp.0 as u16) {
                    self.run_view(a);
                }
            }
            WM_LBUTTONUP if self.switcher.view => {
                let (x, y) = lparam_xy(lp);
                if let Some(a) = self.switcher.on_click_view(x, y) {
                    self.run_view(a);
                }
            }
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lp);
                if let Some(h) = self.switcher.on_click(x, y) {
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), TIMER_SWITCH);
                    }
                    dock::focus_window(h);
                }
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }

    /// Win+Tab: Task View on the screen the pointer is on.
    fn toggle_task_view(&mut self) {
        if self.switcher.view {
            self.switcher.hide();
            return;
        }
        let d = self.dock_ref(self.dock_under_cursor());
        let (k, monitor) = (d.k(), d.monitor());
        let (k, monitor) = if monitor.right > monitor.left { (k, monitor) } else { (self.dpi, self.monitor) };
        self.preview.hide();
        self.launcher.hide();
        self.desks = desktops::read();
        self.switcher.begin_view(k, monitor, self.desks.clone());
    }

    fn run_view(&mut self, action: crate::switcher::ViewAction) {
        use crate::switcher::ViewAction;
        self.switcher.hide();
        match action {
            ViewAction::Focus(h) => dock::focus_window(h),
            ViewAction::Desktop(i) => desktops::switch(self.desks.current, i),
            ViewAction::NewDesktop => desktops::create(),
            ViewAction::Close => {}
        }
    }

    /// The desktops changed (one added, closed, renamed or switched to).
    fn on_desktops(&mut self) {
        let desks = desktops::read();
        if desks != self.desks {
            // A switch (not a rename, a new or a closed desktop) shows the
            // pop-up, unless Task View is up showing them already.
            if self.cfg.desktop_osd
                && desks.current != self.desks.current
                && desks.names.len() == self.desks.names.len()
                && !self.switcher.view
            {
                let d = self.dock_ref(self.dock_under_cursor());
                let (k, monitor) = (d.k(), d.monitor());
                let (k, monitor) = if monitor.right > monitor.left { (k, monitor) } else { (self.dpi, self.monitor) };
                self.deskosd.switched(&self.desks, &desks, monitor, k);
            }
            self.desks = desks;
            self.dock.dirty = true;
            self.switcher.set_desktops(self.desks.clone());
        }
    }

    fn toggle_play(&mut self) {
        // Flip right away; the real state follows from the player.
        if let Some(m) = &mut self.media {
            m.position = m.position_now();
            m.stamp = Instant::now();
            m.playing = !m.playing;
        }
        let _ = self.media_tx.send(MediaCmd::PlayPause);
        self.dock.dirty = true;
        self.widgets.dirty = true;
    }

    fn toggle_widgets(&mut self) {
        if self.just_closed() {
            return;
        }
        if self.widgets.open {
            self.widgets.hide();
            return;
        }
        self.preview.hide();
        self.pop = self.dock_under_cursor();
        self.dock_held = true;
        self.pd_mut().set_held(true);
        let d = self.pd();
        let k = d.k();
        let right = d.bar_rect().right;
        let bottom = d.panel_top() - (8.0 * k) as i32;
        let max_h = bottom - d.monitor().top - (8.0 * k) as i32;
        let notifications = self.cfg.widgets_notifications;
        let (panel, columns) = (&self.cfg.widgets_panel, self.cfg.widgets_columns);
        self.widgets.show(panel, notifications, columns, right, bottom, max_h, k);
        self.arm_stats();
        // The Phone card lists the phone's notifications, so it reads them too.
        if notifications || self.widgets.shows_phone() {
            // Read now, then every few seconds while the panel stays open.
            let _ = self.notif_tx.send(NotifCmd::Refresh);
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_NOTIFS, 3000, None);
            }
        }
    }

    fn handle_widgets(&mut self, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match m {
            WM_ACTIVATE => {
                if wp.0 & 0xFFFF == 0 {
                    self.widgets.hide();
                }
                return None;
            }
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lp);
                self.widgets.on_move(x, y);
            }
            WM_MOUSELEAVE => self.widgets.on_leave(),
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lp);
                self.widgets.on_move(x, y);
                match self.widgets.on_click(x, y) {
                    Some(widgets::Action::Previous) => {
                        let _ = self.media_tx.send(MediaCmd::Previous);
                    }
                    Some(widgets::Action::Next) => {
                        let _ = self.media_tx.send(MediaCmd::Next);
                    }
                    Some(widgets::Action::PlayPause) => self.toggle_play(),
                    Some(widgets::Action::Customise) => {
                        self.widgets.hide();
                        self.open_settings();
                    }
                    Some(widgets::Action::ClearNotifications) => {
                        self.notifs.items.clear();
                        let _ = self.notif_tx.send(NotifCmd::Clear);
                    }
                    Some(widgets::Action::OpenNotification(id)) => {
                        // Like clicking it in Windows' own list: open the app
                        // and the notification goes away.
                        let aumid = self.notifs.items.iter().find(|n| n.id == id).map(|n| n.aumid.clone());
                        self.widgets.hide();
                        match aumid.filter(|a| !a.is_empty()) {
                            Some(a) => {
                                shell::open(&format!("shell:AppsFolder\\{a}"));
                                self.notifs.items.retain(|n| n.id != id);
                                let _ = self.notif_tx.send(NotifCmd::Remove(id));
                            }
                            None => shell::open("ms-actioncenter:"),
                        }
                    }
                    Some(widgets::Action::RemoveNotification(id)) => {
                        // Gone right away; the next read confirms it.
                        self.notifs.items.retain(|n| n.id != id);
                        self.widgets.hovered_clear();
                        let _ = self.notif_tx.send(NotifCmd::Remove(id));
                    }
                    Some(widgets::Action::NotificationSettings) => {
                        self.widgets.hide();
                        shell::open("ms-settings:privacy-notifications");
                    }
                    Some(widgets::Action::OpenPhoneLink) => {
                        self.widgets.hide();
                        shell::open("shell:AppsFolder\\Microsoft.YourPhone_8wekyb3d8bbwe!App");
                    }
                    Some(widgets::Action::TaskManager) => {
                        self.widgets.hide();
                        shell::open("taskmgr.exe");
                    }
                    None => {}
                }
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }

    /// Starts or stops the stats timer: it runs only while a stats widget
    /// is on screen (taskbar chips or the open panel).
    fn arm_stats(&mut self) {
        let chips = ["cpu", "ram", "gpu", "net"].iter().any(|n| self.cfg.placed(n));
        let want = (self.cfg.dock && self.dock.shown && chips) || self.widgets.open;
        // GPU and disk load cost a bit more: only when they are shown.
        let pace = want.then(|| {
            let ms = if self.widgets.open { 1000 } else { self.cfg.stats_seconds.max(1) * 1000 };
            (ms, self.cfg.placed("gpu") || self.widgets.open)
        });
        if pace != self.stats_on {
            self.stats_on = pace;
            let cmd = match pace {
                Some((ms, counters)) => StatsCmd::Start { every: Duration::from_millis(ms as u64), counters },
                None => StatsCmd::Stop,
            };
            let _ = self.stats_tx.send(cmd);
        }
    }

    /// A reading from the stats thread.
    fn on_stats(&mut self, readings: Readings) {
        if self.stats_on.is_none() {
            return; // one still on its way when the reader was stopped
        }
        let before = self.sysmon.signature();
        self.sysmon = readings;
        // Only the card picked in Settings (`gpu_cards`), on the taskbar and in the panel.
        self.gpu_first = 0;
        if let Some(n) = self.cfg.gpu_cards.parse::<usize>().ok().filter(|&n| n >= 1) {
            if self.sysmon.keep_gpu(n - 1) {
                self.gpu_first = n - 1;
            }
        }
        if self.sysmon.signature() != before {
            self.dock.dirty = true;
        }
        if self.widgets.open {
            self.widgets.dirty = true;
        }
    }

    /// The hidden tray icons, above the ^ button.
    fn toggle_flyout(&mut self) {
        if self.just_closed() {
            return;
        }
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

    /// A Win+key shortcut Cascade took from Windows (see `keyhook`).
    fn on_shortcut(&mut self, key: u8) {
        match key {
            b'X' if self.quick.open => self.quick.hide(),
            b'X' => self.open_quick(true),
            b'N' => self.toggle_widgets(),
            b'A' if self.expanded && self.mode == Mode::Controls => self.on_timer(TIMER_COLLAPSE),
            b'A' => {
                self.open_controls();
            }
            b'\t' => self.toggle_task_view(),
            b'V' => match &mut self.clips {
                Some(c) if c.open => c.hide(),
                _ => self.open_clips(),
            },
            _ => {}
        }
    }

    /// The Win+X menu, above the Start button (or at the bottom left of the
    /// screen without Cascade's taskbar). `keyboard`: opened with Win+X.
    fn open_quick(&mut self, keyboard: bool) {
        self.preview.hide();
        self.launcher.hide();
        self.flyout.hide();
        self.widgets.hide();
        self.pop = self.dock_under_cursor();
        let d = self.pd();
        let k = d.k();
        let gap = (8.0 * k) as i32;
        let (left, bottom, monitor) = if self.cfg.dock && d.visible() {
            (d.start_x(), d.panel_top() - gap, d.monitor())
        } else {
            let work = shell::work_area();
            (work.left + gap, work.bottom - gap, self.monitor)
        };
        let laptop = battery().is_some();
        self.quick.show(left, bottom, monitor, laptop, keyboard, k);
    }

    /// The Win+V menu, by the text cursor in the app in front (or the pointer
    /// when the app doesn't say where its cursor is).
    fn open_clips(&mut self) {
        let fg = unsafe { GetForegroundWindow() };
        if self.clips.as_ref().is_none_or(|c| c.hwnd != fg) {
            self.clip_target = fg;
        }
        if self.clips.is_none() {
            let ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
            let (Ok(hwnd), Ok(backdrop)) =
                (create_window(w!("CascadeWindow"), self.instance, ex), Backdrop::create(self.instance))
            else {
                return;
            };
            let mut menu = ClipMenu::new(hwnd, backdrop);
            menu.configure(self.cfg.acrylic, self.cfg.taskbar_opacity, self.cfg.corner_radius());
            self.clips = Some(menu);
        }
        let at = clipboard::caret().unwrap_or_else(|| {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
            }
            p
        });
        let bounds = shell::work_area_of(RECT { left: at.x, top: at.y, right: at.x + 1, bottom: at.y + 1 });
        let k = unsafe {
            let mon = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            dx.max(48) as f32 / 96.0
        };
        self.preview.hide();
        self.launcher.hide();
        self.quick.hide();
        if let Some(c) = &mut self.clips {
            c.show(at.x, at.y, bounds, k);
        }
        // Read once it's in front: Windows only hands the history to the app in front.
        let _ = self.clip_tx.send(ClipCmd::Refresh);
    }

    fn run_clip(&mut self, action: clipboard::Action) {
        use clipboard::Action;
        let close = |a: &mut Self| {
            if let Some(c) = &mut a.clips {
                c.hide();
            }
        };
        match action {
            Action::Paste(id) => {
                close(self);
                let _ = self.clip_tx.send(ClipCmd::Paste(id));
            }
            Action::Delete(id) => {
                let _ = self.clip_tx.send(ClipCmd::Delete(id));
            }
            Action::Clear => {
                let _ = self.clip_tx.send(ClipCmd::Clear);
            }
            Action::Settings => {
                close(self);
                shell::open("ms-settings:clipboard");
            }
            Action::Windows => {
                close(self);
                shell::win_combo(b'V' as u16);
            }
            Action::Close => close(self),
        }
    }

    fn handle_clips(&mut self, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let c = self.clips.as_mut()?;
        let action = match m {
            WM_ACTIVATE => {
                // Clicking anywhere else closes it.
                if wp.0 & 0xFFFF == 0 {
                    c.hide();
                }
                return None;
            }
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lp);
                c.on_move(x, y);
                None
            }
            WM_MOUSELEAVE => {
                c.on_leave();
                None
            }
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lp);
                c.on_click(x, y)
            }
            WM_MOUSEWHEEL => {
                c.on_wheel(((wp.0 >> 16) & 0xFFFF) as u16 as i16);
                None
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => c.on_key(wp.0 as u16),
            _ => return None,
        };
        if let Some(a) = action {
            self.run_clip(a);
        }
        Some(LRESULT(0))
    }

    fn run_quick(&mut self, action: quickmenu::Action) {
        use quickmenu::Action;
        self.quick.hide();
        match action {
            Action::Open(target) => shell::open(target),
            Action::Terminal { admin } => {
                if !shell::launch("wt.exe", "", admin) {
                    shell::launch("powershell.exe", "", admin);
                }
            }
            Action::Search if self.cfg.launcher => self.open_launcher(),
            Action::Search => shell::win_combo(b'S' as u16),
            Action::Run => shell::open("shell:::{2559a1f3-21d7-11d4-bdaf-00c04f60b9f0}"),
            Action::Desktop => shell::win_combo(b'D' as u16),
            Action::Power(p) => shell::power(p),
        }
    }

    fn handle_quick(&mut self, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match m {
            WM_ACTIVATE => {
                // Clicking anywhere else closes it.
                if wp.0 & 0xFFFF == 0 {
                    self.quick.hide();
                }
                return None;
            }
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lp);
                self.quick.on_move(x, y);
            }
            WM_MOUSELEAVE => self.quick.on_leave(),
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lp);
                if let Some(a) = self.quick.on_click(x, y) {
                    self.run_quick(a);
                }
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => match self.quick.on_key(wp.0 as u16) {
                quickmenu::Key::Close => self.quick.hide(),
                quickmenu::Key::Run(a) => self.run_quick(a),
                quickmenu::Key::Nothing => {}
            },
            _ => return None,
        }
        Some(LRESULT(0))
    }

    /// A safety net under the taskbar's hover. Windows doesn't always say
    /// the pointer left (a menu or another window took the mouse, or the
    /// taskbar hid or moved from under it), and then an app's name and its
    /// previews would stay up. While something is hovered this looks at what
    /// is really under the pointer every quarter second; off the taskbar and
    /// the previews twice in a row, with no button held, it lets go of them.
    fn check_hover(&mut self, di: usize) {
        let dock = self.dock_ref(di).hwnd;
        let previews = self.preview.visible() && self.preview_dock == di;
        if self.dock_ref(di).hovered.is_none() && !previews {
            self.hover_misses = 0;
            return;
        }
        let under = unsafe {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            GetAncestor(WindowFromPoint(p), GA_ROOT)
        };
        let held =
            [VK_LBUTTON, VK_RBUTTON].iter().any(|k| unsafe { GetAsyncKeyState(k.0 as i32) } as u16 & 0x8000 != 0);
        if under == dock || under == self.preview.hwnd || held {
            self.hover_misses = 0;
        } else {
            self.hover_misses += 1;
            if self.hover_misses >= 2 {
                self.hover_misses = 0;
                unsafe {
                    if GetCapture() == dock {
                        let _ = ReleaseCapture();
                    }
                    let _ = KillTimer(Some(dock), TIMER_PREVIEW_SHOW);
                }
                self.dock_mut(di).on_pointer_gone();
                self.save_chips(di);
                if previews {
                    self.preview.hide();
                }
                return;
            }
        }
        unsafe {
            SetTimer(Some(dock), TIMER_HOVER, 250, None);
        }
    }

    /// A widget was dragged to a new place on taskbar `di`: the config keeps it.
    fn save_chips(&mut self, di: usize) {
        let Some((left, right)) = self.dock_mut(di).take_chips() else { return };
        self.apply_change(Change::Many(vec![("taskbar_left", left), ("taskbar_right", right)]));
        // Settings' widget lanes show the new places.
        if let Some(s) = &mut self.settings {
            let cfg = &self.cfg;
            s.sync_edits(&|k: &str| setting_value(cfg, k));
        }
    }

    /// Opens (after a short hover) or retargets the preview for the hovered app.
    fn schedule_preview(&mut self, di: usize) {
        let d = self.dock_ref(di);
        let dock = d.hwnd;
        let has_windows = d.hovered_app().is_some_and(|i| !d.apps[i].windows.is_empty());
        unsafe {
            if has_windows {
                if self.preview_dock != di {
                    let _ = KillTimer(Some(self.dock_ref(self.preview_dock).hwnd), TIMER_PREVIEW_SHOW);
                    self.preview_dock = di;
                }
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
        let d = self.dock_ref(self.preview_dock);
        let Some(i) = d.hovered_app() else { return };
        let app = &d.apps[i];
        if app.windows.is_empty() {
            return;
        }
        let (id, windows) = (app.id.clone(), app.windows.clone());
        let x = d.app_screen_x(i);
        let bottom = d.panel_top() - (6.0 * d.k()) as i32;
        let (monitor, k) = (d.monitor(), d.k());
        self.preview.show(&id, &windows, x, bottom, monitor, k);
    }

    /// After a rescan, keeps an open preview in sync with the app's windows.
    fn refresh_preview(&mut self) {
        let Some(id) = self.preview.app.clone() else { return };
        let d = self.dock_ref(self.preview_dock);
        match d.apps.iter().position(|a| a.id == id) {
            Some(i) if !d.apps[i].windows.is_empty() => {
                let windows = d.apps[i].windows.clone();
                let x = d.app_screen_x(i);
                let bottom = d.panel_top() - (6.0 * d.k()) as i32;
                let (monitor, k) = (d.monitor(), d.k());
                self.preview.show(&id, &windows, x, bottom, monitor, k);
            }
            _ => self.preview.hide(),
        }
    }

    fn handle_preview(&mut self, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let dock = self.dock_ref(self.preview_dock).hwnd;
        unsafe {
            match m {
                WM_TIMER if wp.0 == preview::TIMER_PEEK => self.preview.on_timer(),
                WM_MOUSEMOVE => {
                    let _ = KillTimer(Some(dock), TIMER_PREVIEW_HIDE);
                    let _ = KillTimer(Some(dock), TIMER_DOCK_HIDE);
                    let (x, y) = lparam_xy(lp);
                    self.preview.on_move(x, y);
                    self.hover_misses = 0;
                    SetTimer(Some(dock), TIMER_HOVER, 250, None);
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
                            SetTimer(Some(self.dock.hwnd), TIMER_SCAN, 300, None);
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
                || !self.tab.settled()
                || !self.tabs_fade.settled()
                || !self.level.settled()
                // (Bars only the widgets show don't redraw the notch.)
                || ((self.notch_bars || !self.widget_bars) && self.bars_moving()))
    }

    fn bars_moving(&self) -> bool {
        self.bars_fresh || self.bars.iter().any(|b| !b.settled())
    }

    fn wants_frame(&self) -> bool {
        self.notch_wants()
            || (self.widget_bars && self.bars_moving())
            || self.dock.wants_frame()
            || self.screens.iter().any(|d| d.wants_frame())
            || self.launcher.wants_frame()
            || self.flyout.wants_frame()
            || self.quick.wants_frame()
            || self.deskosd.wants_frame()
            || self.clips.as_ref().is_some_and(|c| c.wants_frame())
            || self.widgets.wants_frame()
            || self.switcher.wants_frame()
    }

    /// Steps the springs, draws, and returns how long to wait for the next frame.
    fn frame(&mut self) -> Duration {
        let now = Instant::now();
        let mut dt = (now - self.last_frame).as_secs_f32();
        if dt > 0.1 {
            dt = 1.0 / 60.0;
        }
        self.last_frame = now;

        // The visualizer's bars, wherever they show.
        if self.bars_moving() {
            self.bars_fresh = false;
            for b in &mut self.bars {
                b.step(dt);
            }
            if self.widget_bars {
                if self.widgets.open {
                    self.widgets.dirty = true;
                }
                if self.dock_bars && now.duration_since(self.dock_bars_at) >= Duration::from_millis(33) {
                    self.dock_bars_at = now;
                    // Only the music widget is redrawn for them.
                    self.dock.bars_dirty = true;
                }
            }
        }

        self.sync_pins();
        let mut fast = false;
        if self.notch_wants() {
            fast |= self.w.step(dt);
            fast |= self.h.step(dt);
            fast |= self.r.step(dt);
            self.fade.step(dt);
            fast |= self.tab.step(dt);
            self.tabs_fade.step(dt);
            fast |= self.level.step(dt);
            self.dirty = false;
            let t = self.perf.start();
            let _ = self.render();
            self.perf.end(Surface::Notch, t);
            // Brought up for Controls with the notch off: gone once it has closed.
            if self.summoned && !self.expanded && self.w.settled() && self.h.settled() {
                self.summoned = false;
                self.retarget();
            }
        }
        if self.dock.wants_frame() {
            fast |= self.dock.step(dt);
            let status =
                dock::Status { wifi: self.radios.wifi, volume: self.vol.0, muted: self.vol.1, battery: battery() };
            self.sync_art();
            let weather = self.weather.as_ref().filter(|_| self.cfg.weather);
            let art = self.art.as_ref().map(|(_, b, _)| b.clone());
            let bars = (self.widget_bars && self.dock_bars).then(|| (self.levels(), self.accent()));
            let data = dock::WidgetData {
                stats: &self.sysmon.stats,
                gpus: &self.sysmon.gpus,
                gpu_first: self.gpu_first,
                desks: (self.desks.names.len(), self.desks.current),
                media: self.media.as_ref(),
                art,
                bars,
            };
            let t = self.perf.start();
            let _ = self.dock.render(&mut self.gfx, &status, weather, &mut self.systray, &data);
            self.perf.end(Surface::Dock, t);
            if let Some(h) = self.systray.hwnd {
                let r = self.dock.bar_rect();
                if r != self.tray_rect {
                    self.tray_rect = r;
                    systray::set_rect(h, r);
                }
            }
        }
        if self.screens.iter().any(|d| d.wants_frame()) {
            // The other screens' taskbars: apps and the clock, no widgets.
            let status = dock::Status::default();
            let data = dock::WidgetData {
                stats: &self.sysmon.stats,
                gpus: &[],
                gpu_first: 0,
                desks: (0, 0),
                media: None,
                art: None,
                bars: None,
            };
            for d in &mut self.screens {
                if d.wants_frame() {
                    fast |= d.step(dt);
                    let t = self.perf.start();
                    let _ = d.render(&mut self.gfx, &status, None, &mut self.systray, &data);
                    self.perf.end(Surface::Screens, t);
                }
            }
        }
        if self.flyout.wants_frame() {
            fast |= self.flyout.step(dt);
            let t = self.perf.start();
            let _ = self.flyout.render(&mut self.gfx, &mut self.systray);
            self.perf.end(Surface::Flyout, t);
        }
        if self.quick.wants_frame() {
            fast |= self.quick.step(dt);
            let t = self.perf.start();
            let _ = self.quick.render(&mut self.gfx);
            self.perf.end(Surface::Quick, t);
        }
        if self.deskosd.wants_frame() {
            fast |= self.deskosd.step(dt);
            let _ = self.deskosd.render(&mut self.gfx);
        }
        if let Some(c) = self.clips.as_mut().filter(|c| c.wants_frame()) {
            fast |= c.step(dt);
            let t = self.perf.start();
            let _ = c.render(&mut self.gfx);
            self.perf.end(Surface::Clipboard, t);
        }
        if self.switcher.wants_frame() {
            fast |= self.switcher.animate(dt);
            let t = self.perf.start();
            let _ = self.switcher.render(&mut self.gfx, &mut self.dock);
            self.perf.end(Surface::Switcher, t);
        }
        if self.widgets.wants_frame() {
            fast |= self.widgets.step(dt);
            self.sync_art();
            let art = self.art.as_ref().map(|(_, b, _)| b.clone());
            let bars = self.widget_bars.then(|| (self.levels(), self.accent()));
            let ctx = widgets::Ctx {
                bars,
                sampler: &self.sysmon,
                media: self.media.as_ref(),
                art,
                weather: self.weather.as_ref(),
                notifs: &self.notifs,
                events: &self.events,
            };
            let t = self.perf.start();
            let _ = self.widgets.render(&mut self.gfx, &ctx);
            self.perf.end(Surface::Widgets, t);
        }
        // Stats are read faster while the panel is open.
        if self.widgets.open != self.widgets_seen {
            self.widgets_seen = self.widgets.open;
            self.arm_stats();
            self.sync_visualizer();
        }
        // Keep an auto-hidden taskbar up while the menu or flyout is open,
        // and let it go again shortly after they close.
        let hold = self.launcher.open || self.flyout.open || self.widgets.open || self.quick.open;
        if hold != self.dock_held {
            self.dock_held = hold;
            if !hold {
                // Back to hidden if a fullscreen app is still in front.
                self.show_docks();
            }
            if hold {
                self.pd_mut().set_held(true);
            } else {
                for d in std::iter::once(&mut self.dock).chain(self.screens.iter_mut()) {
                    if d.set_held(false) {
                        unsafe {
                            SetTimer(Some(d.hwnd), TIMER_DOCK_HIDE, 500, None);
                        }
                    }
                }
            }
        }
        if self.launcher.wants_frame() {
            fast |= self.launcher.step(dt);
            let bottom = self.launcher_bottom();
            let d = self.pd();
            let anchor = if d.shown { d.start_anchor() } else { None };
            let monitor = if self.pop == 0 { self.monitor } else { d.monitor() };
            let t = self.perf.start();
            let _ = self.launcher.render(&mut self.gfx, &mut self.dock, monitor, bottom, anchor);
            self.perf.end(Surface::Launcher, t);
        }
        self.perf.flush();
        // A ring of bent light came on screen (or went): refresh it, or stop.
        if self.lens_wanted && crate::lens::active() != self.lens_on {
            self.arm_lens();
        }

        // Full rate while shapes morph or a level moves; the visualizer and
        // fades are fine at 60.
        let fps = if fast { self.cfg.max_fps } else { 60 }.min(self.refresh_hz).max(15);
        // At the screen's own rate, wait for the compositor's next frame
        // rather than a timer, so each step lands on a refresh (see `run`).
        self.vsync = fps >= self.refresh_hz;
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
        let edge = self.notch_edge();
        let side = edge.side();
        // The lip and the notch pulled out of it are a plain panel, flush with
        // the edge: no ears.
        let lip = self.cfg.lip();
        let ear = if lip { 0.0 } else { EAR };
        // The panel stays upright; along a side edge its height runs along it.
        let (along, depth) = if side { (bh, bw) } else { (bw, bh) };
        let mut pa = ((along + 2.0 * ear) * k).ceil() as i32 + 2;
        if pa % 2 != 0 {
            pa += 1; // an even size about a whole-pixel centre never jitters
        }
        let pd = ((depth * k).ceil() as i32).max(1);
        let area = self.area;
        let (m0, m1) = if side { (area.top, area.bottom) } else { (area.left, area.right) };
        let half = pa / 2;
        let centre = (m0 as f32 + self.cfg.notch_position * (m1 - m0) as f32).round() as i32;
        // Kept on the screen: near an end it grows away from the corner.
        let centre = centre.clamp(m0 + half, (m1 - half).max(m0 + half));
        let (pos, pw, ph) = match edge {
            Edge::Top => (POINT { x: centre - half, y: area.top }, pa, pd),
            Edge::Bottom => (POINT { x: centre - half, y: area.bottom - pd }, pa, pd),
            Edge::Left => (POINT { x: area.left, y: centre - half }, pd, pa),
            Edge::Right => (POINT { x: area.right - pd, y: centre - half }, pd, pa),
        };
        if self.pinned {
            let r = RECT { left: pos.x, top: pos.y, right: pos.x + pw, bottom: pos.y + ph };
            crate::mousehook::set_rect(3, Some(r));
        }
        let (ww, wh) = (pw as f32 / k, ph as f32 / k);
        let (tw, th, _) = self.size_of(self.view);
        // The body against its edge, and where the content starts: laid out at
        // its full size out from the edge, so the panel growing uncovers it.
        let (bx, by, ox, oy) = match edge {
            Edge::Top => (ww / 2.0 - bw / 2.0, 0.0, ww / 2.0 - tw / 2.0, 0.0),
            Edge::Bottom => (ww / 2.0 - bw / 2.0, wh - bh, ww / 2.0 - tw / 2.0, wh - th),
            Edge::Left => (0.0, wh / 2.0 - bh / 2.0, 0.0, wh / 2.0 - th / 2.0),
            Edge::Right => (ww - bw, wh / 2.0 - bh / 2.0, ww - tw, wh / 2.0 - th / 2.0),
        };
        self.origin = ox;
        self.origin_y = (oy * k).round() / k;
        self.content_w = tw;
        // The edge's line in the window, and where the body starts along it.
        let (base, a0) = match edge {
            Edge::Top => (0.0, bx),
            Edge::Bottom => (wh, bx),
            Edge::Left => (0.0, by),
            Edge::Right => (ww, by),
        };

        // The panel runs past the edge by more than its corner radius, so only
        // its free corners are round (the blur's too).
        let r = self.r.value;
        let above = r + 2.0;
        let (fx, fy, fw, fh) = match edge {
            Edge::Top => (bx, by - above, bw, bh + above),
            Edge::Bottom => (bx, by, bw, bh + above),
            Edge::Left => (bx - above, by, bw + above, bh),
            Edge::Right => (bx, by, bw + above, bh),
        };
        // Glass the whole time, also while the notch opens and closes (the
        // blur follows the panel frame by frame; see `Backdrop::track`).
        let blurred = lip && self.notch_blur;
        let glass = blurred.then(|| {
            let px = |v: f32| (v * k).round() as i32;
            let body = RECT {
                left: pos.x + px(bx),
                top: pos.y + px(by),
                right: pos.x + px(bx + bw),
                bottom: pos.y + px(by + bh),
            };
            let full = RECT {
                left: pos.x + px(fx),
                top: pos.y + px(fy),
                right: pos.x + px(fx + fw),
                bottom: pos.y + px(fy + fh),
            };
            // Past the edge is normally off every screen. If it lands on one
            // (another screen there, or a taskbar), the blur keeps to the body
            // and is cut to the panel's shape instead.
            let strip = match edge {
                Edge::Top => RECT { bottom: body.top, ..full },
                Edge::Bottom => RECT { top: body.bottom, ..full },
                Edge::Left => RECT { right: body.left, ..full },
                Edge::Right => RECT { left: body.right, ..full },
            };
            let covered = !unsafe { MonitorFromRect(&strip, MONITOR_DEFAULTTONULL) }.is_invalid();
            if covered {
                let shape = RECT {
                    left: full.left - body.left,
                    top: full.top - body.top,
                    right: full.right - body.left,
                    bottom: full.bottom - body.top,
                };
                (body, Some((shape, px(r))))
            } else {
                (full, None)
            }
        });
        if glass.is_none() {
            self.notch_backdrop.hide();
        }

        self.gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = self.gfx.painter();
            if lip {
                theme::panel(&p, fx, fy, fw, fh, r, blurred, self.cfg.taskbar_opacity, 0.97, 1.0);
            } else {
                p.notch(edge, base, a0, along, depth, r, EAR, BLACK);
            }
            let alpha = self.fade.value.clamp(0.0, 1.0);
            if matches!(self.view, View::Idle | View::Live) && self.flat_rest() {
                if self.view == View::Live && alpha > 0.01 {
                    self.draw_lip_glow(&p, edge, base, a0, along, depth, alpha);
                }
            } else if (alpha > 0.01 || self.view == View::Expanded) && bh > 4.0 && bw > 4.0 {
                // (The open notch's tab bar has a fade of its own, so it stays
                // put while a tab switch fades the rest in.)
                p.clip(bx, by, bw, bh);
                p.translate(0.0, self.origin_y);
                match self.view {
                    View::Idle => {}
                    View::Live => self.draw_live(&p, alpha),
                    View::Peek if matches!(self.peek, Some(PeekKind::Timer | PeekKind::Message)) => {
                        self.draw_timer_peek(&p, alpha)
                    }
                    View::Peek if self.peek == Some(PeekKind::Device) => self.draw_device_peek(&p, alpha),
                    View::Peek if self.peek == Some(PeekKind::Power) => self.draw_power_peek(&p, alpha),
                    View::Peek => {
                        self.draw_level_row(&p, self.origin + 16.0, th / 2.0, tw - 32.0, self.peek, alpha);
                    }
                    View::Expanded => self.draw_expanded(&p, alpha),
                }
                p.translate(0.0, 0.0);
                p.unclip();
            }
        }
        let shown = self.gfx.present(&self.layer, self.hwnd, pos, pw, ph, 255);
        // After the frame is on its way, so the two windows move together.
        if let Some((rect, shape)) = glass {
            if self.notch_backdrop.track(rect, shape, self.hwnd) {
                self.dirty = true;
            }
        }
        shown
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
        let (tx, ty, seg) = tab_bar(tw, th);
        for i in 0..MODES.len() {
            out.push((Hot::Tab(i), tx + i as f32 * seg, ty, seg, TAB_H));
        }
        match self.mode {
            Mode::Media if self.media.is_some() => {
                for (h, dx) in [(Hot::Previous, -60.0), (Hot::PlayPause, 0.0), (Hot::Next, 60.0)] {
                    out.push((h, tw / 2.0 + dx - 20.0, 104.0, 40.0, 40.0));
                }
                if self.media.as_ref().is_some_and(|m| m.sources > 1) {
                    out.push((Hot::MediaSource, tw - 18.0 - 34.0, 107.0, 34.0, 34.0));
                }
            }
            Mode::Media => {}
            Mode::Controls if self.bt_open => {
                let l = self.bt_layout();
                out.push((Hot::BtBack, 14.0, 14.0, 36.0, 34.0));
                out.push((Hot::Bluetooth, tw - 18.0 - WIFI_SWITCH_W, 17.0, WIFI_SWITCH_W, 28.0));
                for i in 0..l.rows {
                    out.push((Hot::BtDev(i), 14.0, l.top + i as f32 * WIFI_ROW, tw - 28.0, WIFI_ROW));
                }
                let bw = (tw - 36.0 - 8.0) / 2.0;
                out.push((Hot::BtSettings, 18.0, l.foot, bw, WIFI_BTN_H));
                out.push((Hot::WifiQuick, 18.0 + bw + 8.0, l.foot, bw, WIFI_BTN_H));
            }
            Mode::Controls if self.wifi_open => {
                let l = self.wifi_layout();
                out.push((Hot::WifiBack, 14.0, 14.0, 36.0, 34.0));
                out.push((Hot::Wifi, tw - 18.0 - WIFI_SWITCH_W, 17.0, WIFI_SWITCH_W, 28.0));
                for i in 0..l.rows {
                    out.push((Hot::WifiNet(i), 14.0, l.top + i as f32 * WIFI_ROW, tw - 28.0, WIFI_ROW));
                }
                let bw = (tw - 36.0 - 8.0) / 2.0;
                out.push((Hot::WifiSettings, 18.0, l.foot, bw, WIFI_BTN_H));
                out.push((Hot::WifiQuick, 18.0 + bw + 8.0, l.foot, bw, WIFI_BTN_H));
            }
            Mode::Controls => {
                let tile_w = (tw - 36.0 - 16.0) / 3.0;
                // The Wi-Fi tile's chevron (opens the list) before the tile itself.
                if self.radios.wifi.is_some() {
                    out.push((Hot::WifiMore, 18.0 + tile_w - WIFI_MORE_W, 14.0, WIFI_MORE_W, 54.0));
                }
                if self.radios.bluetooth.is_some() {
                    let x = 18.0 + tile_w + 8.0 + tile_w - WIFI_MORE_W;
                    out.push((Hot::BtMore, x, 14.0, WIFI_MORE_W, 54.0));
                }
                for (i, h) in [Hot::Wifi, Hot::Bluetooth, Hot::Notifications].into_iter().enumerate() {
                    out.push((h, 18.0 + i as f32 * (tile_w + 8.0), 14.0, tile_w, 54.0));
                }
                out.push((Hot::VolumeBar, 44.0, 84.0, tw - 98.0, 24.0));
                out.push((Hot::BrightnessBar, 44.0, 118.0, tw - 98.0, 24.0));
                for (hot, y, h) in self.mixer_rows() {
                    let (x, w) = match hot {
                        Hot::AppMute(_) => (18.0, MIX_SLIDER_X - 26.0),
                        Hot::AppBar(_) => (MIX_SLIDER_X - 6.0, tw - 60.0 - MIX_SLIDER_X + 12.0),
                        _ => (18.0, tw - 36.0),
                    };
                    out.push((hot, x, y, w, h));
                }
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
        let ly = y as f32 / self.k - self.origin_y;
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
        } else if let Some((e, _)) = self.countdown().filter(|_| !self.pomo.running()) {
            // The event coming up, and how long until it starts.
            let mins = ((e.start - calendar::now()) as f32 / 60.0).ceil().max(1.0);
            p.circle(ox + 16.0, th / 2.0, 4.0, theme::accent().alpha(alpha));
            let fonts = &self.gfx.fonts;
            let left = format!("{mins:.0} min");
            p.text(
                &e.title,
                &fonts.label,
                ox + 26.0,
                0.0,
                tw - 26.0 - 62.0,
                th,
                Align::Left,
                Color::white(0.95 * alpha),
            );
            p.text(&left, &fonts.num, ox + tw - 60.0, 0.0, 48.0, th, Align::Right, Color::white(0.7 * alpha));
        } else {
            let c = if self.pomo.focus { theme::accent() } else { GREEN };
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

    /// The resting strip while music plays or the focus timer runs: a thin
    /// line along it, in the album art's colour or showing the timer's
    /// progress. `base`, `a`, `len` and `depth` place the strip as in
    /// `Painter::notch`.
    fn draw_lip_glow(&self, p: &Painter, edge: Edge, base: f32, a: f32, len: f32, depth: f32, alpha: f32) {
        let inset = self.r.value.min(len / 4.0) + 3.0;
        let lw = (len - 2.0 * inset).max(0.0);
        let t = (depth * 0.4).clamp(1.0, 2.5);
        // Near the free side, away from the screen edge.
        let v = (depth - t - 1.0).max(0.0);
        let line = |l: f32, c: Color| {
            let (x, y, w, h) = edge.rect(base, a + inset, v, l, t);
            p.rrect(x, y, w, h, t / 2.0, c);
        };
        if self.playing() && self.cfg.lip_visualizer {
            // Solid lines reaching out from the middle with the music, over a
            // faint one the whole length: the whole mix, and the bass on top.
            let c = self.accent();
            let levels = self.levels();
            let all = levels.iter().sum::<f32>() / BANDS as f32;
            let bass = levels[0].max(levels[1]);
            let centred = |l: f32, c: Color| {
                let l = l.clamp(t * 2.0, lw);
                let (x, y, w, h) = edge.rect(base, a + inset + (lw - l) / 2.0, v, l, t);
                p.rrect(x, y, w, h, t / 2.0, c);
            };
            line(lw, c.alpha(0.22 * alpha));
            centred(lw * (all * 1.6).min(1.0), c.alpha(0.5 * alpha));
            centred(lw * bass, c.alpha(0.95 * alpha));
        } else if self.playing() {
            line(lw, self.accent().alpha(0.9 * alpha));
        } else if let Some((_, frac)) = self.countdown().filter(|_| !self.pomo.running()) {
            // Fills up over the minutes before the event starts.
            line(lw, Color::white(0.14 * alpha));
            let done = lw * frac.clamp(0.0, 1.0);
            if done > 0.5 {
                line(done.max(t), theme::accent().alpha(alpha));
            }
        } else {
            let c = if self.pomo.focus { theme::accent() } else { GREEN };
            line(lw, Color::white(0.14 * alpha));
            let done = lw * self.pomo.progress().clamp(0.0, 1.0);
            if done > 0.5 {
                line(done.max(t), c.alpha(alpha));
            }
        }
    }

    fn draw_timer_peek(&self, p: &Painter, alpha: f32) {
        let (tw, th, _) = self.size_of(View::Peek);
        let (text, c) = if self.peek == Some(PeekKind::Message) {
            (self.message.as_str(), self.message_color)
        } else if self.pomo.focus {
            ("Break over \u{2014} time to focus", theme::accent())
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

    /// The charger plugged in or out: the battery (with its bolt while it's
    /// plugged in) and how full it is.
    fn draw_power_peek(&self, p: &Painter, alpha: f32) {
        let Some((pct, plugged)) = self.power else { return };
        let (tw, th, _) = self.size_of(View::Peek);
        let (bx, cy) = (self.origin + 16.0, th / 2.0);
        let (w, h) = (22.0, 12.0);
        let line = Color::white(0.6 * alpha);
        p.stroke_rrect(bx, cy - h / 2.0, w, h, 3.0, line, 1.2);
        p.rrect(bx + w + 1.0, cy - 2.5, 2.0, 5.0, 1.0, line);
        let c = if plugged {
            GREEN.alpha(alpha)
        } else if pct <= 20 {
            Color(1.0, 0.4, 0.35, alpha)
        } else {
            Color::white(0.9 * alpha)
        };
        p.rrect(bx + 2.0, cy - h / 2.0 + 2.0, (w - 4.0) * pct as f32 / 100.0, h - 4.0, 1.5, c);
        if plugged {
            bolt(p, bx + w / 2.0, cy, 15.0, alpha);
        }
        let text = match (plugged, pct) {
            (true, n) if n >= 100 => "Plugged in \u{b7} fully charged".to_string(),
            (true, _) => format!("Charging \u{b7} {pct}%"),
            (false, _) => format!("On battery \u{b7} {pct}%"),
        };
        let x = bx + w + 12.0;
        let tw = tw - (x - self.origin) - 14.0;
        p.text(&text, &self.gfx.fonts.label, x, 0.0, tw, th, Align::Left, Color::white(0.95 * alpha));
    }

    /// A USB device plugged in: a plug, its name (and serial port), and what
    /// clicking opens; or that it was unplugged.
    fn draw_device_peek(&self, p: &Painter, alpha: f32) {
        let Some(d) = &self.device else { return };
        let (tw, th, _) = self.size_of(View::Peek);
        let ox = self.origin;
        let cy = th / 2.0;
        let c = if d.removed { Color::white(0.5 * alpha) } else { theme::accent().alpha(alpha) };
        // A plug: its body, two prongs and the cable.
        let px = ox + 22.0;
        p.rrect(px - 5.0, cy - 2.0, 10.0, 8.0, 2.0, c);
        p.rrect(px - 3.5, cy - 7.0, 2.0, 5.0, 0.8, c);
        p.rrect(px + 1.5, cy - 7.0, 2.0, 5.0, 0.8, c);
        p.rrect(px - 1.0, cy + 6.0, 2.0, 4.0, 0.8, c);
        let fonts = &self.gfx.fonts;
        let open = d.device.open_label.as_ref().filter(|_| !d.removed && d.device.open.is_some());
        let action = open.map(|l| format!("Open {l}"));
        let chip_w = action.as_ref().map_or(0.0, |a| self.gfx.measure(a, &fonts.small) + 22.0);
        let mut text = d.device.name.clone();
        if d.removed {
            text += " unplugged";
        } else if let Some(port) = &d.port {
            text += &format!(" on {port}");
        }
        let text_w = tw - 34.0 - 14.0 - if chip_w > 0.0 { chip_w + 8.0 } else { 0.0 };
        let white = Color::white(if d.removed { 0.6 } else { 0.95 } * alpha);
        p.text(&text, &fonts.label, ox + 36.0, 0.0, text_w, th, Align::Left, white);
        if let Some(a) = action {
            let (cx, ch) = (ox + tw - 14.0 - chip_w, 24.0);
            let bg = if d.hot { theme::accent_hover() } else { theme::accent() };
            p.rrect(cx, cy - ch / 2.0, chip_w, ch, ch / 2.0, bg.alpha(alpha));
            p.text(&a, &fonts.small, cx, cy - ch / 2.0, chip_w, ch, Align::Center, Color::white(alpha));
        }
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

    /// The tab bar along the bottom of the open notch: a track with a tab
    /// (icon and name) per mode, and a highlight that slides to the open one.
    fn draw_tabs(&self, p: &Painter) {
        let alpha = self.tabs_fade.value.clamp(0.0, 1.0);
        if alpha < 0.01 {
            return;
        }
        let (tw, th, _) = self.size_of(View::Expanded);
        let (tx, ty, seg) = tab_bar(tw, th);
        let x0 = self.origin + tx;
        p.rrect(x0, ty, seg * MODES.len() as f32, TAB_H, TAB_H / 2.0, Color::white(0.06 * alpha));
        let hx = x0 + self.tab.value * seg;
        p.rrect(hx + 2.0, ty + 2.0, seg - 4.0, TAB_H - 4.0, (TAB_H - 4.0) / 2.0, Color::white(0.15 * alpha));
        let font = &self.gfx.fonts.label;
        for (i, &m) in MODES.iter().enumerate() {
            let sx = x0 + i as f32 * seg;
            let active = m == self.mode;
            let hovered = self.hovered == Some(Hot::Tab(i));
            if hovered && !active {
                p.rrect(sx + 2.0, ty + 2.0, seg - 4.0, TAB_H - 4.0, (TAB_H - 4.0) / 2.0, Color::white(0.07 * alpha));
            }
            let label = match m {
                Mode::Media if self.media.is_some() => "Media",
                Mode::Media => "Home",
                Mode::Controls => "Controls",
                Mode::Calendar => "Calendar",
            };
            let c = Color::white(
                if active {
                    0.95
                } else if hovered {
                    0.8
                } else {
                    0.5
                } * alpha,
            );
            let lw = self.gfx.measure(label, font);
            let start = sx + (seg - (14.0 + 6.0 + lw)) / 2.0;
            let cy = ty + TAB_H / 2.0;
            tab_icon(p, m, self.media.is_some(), start + 7.0, cy, c);
            p.text(label, font, start + 20.0, ty, lw + 4.0, TAB_H, Align::Left, c);
        }
    }

    fn draw_expanded(&self, p: &Painter, alpha: f32) {
        match self.mode {
            Mode::Media if self.media.is_some() => self.draw_media(p, alpha),
            Mode::Media => self.draw_home(p, alpha),
            Mode::Controls => self.draw_controls(p, alpha),
            Mode::Calendar => self.draw_calendar(p, alpha),
        }
        self.draw_tabs(p);
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
        // More than one thing with media (your phone and a browser, say): a
        // button to switch between them, at the right of the controls.
        if m.sources > 1 {
            let (sx, sy) = (ox + tw - 18.0 - 17.0, cy);
            if self.hovered == Some(Hot::MediaSource) {
                p.circle(sx, sy, 16.0, Color::white(0.10 * alpha));
            }
            let c = Color::white(0.75 * alpha);
            // Two arrows passing each other.
            p.line(sx - 7.0, sy - 3.5, sx + 6.0, sy - 3.5, c, 1.5);
            p.line(sx + 3.0, sy - 6.5, sx + 6.0, sy - 3.5, c, 1.5);
            p.line(sx + 7.0, sy + 3.5, sx - 6.0, sy + 3.5, c, 1.5);
            p.line(sx - 3.0, sy + 6.5, sx - 6.0, sy + 3.5, c, 1.5);
        }
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
            if charging {
                bolt(p, bx + 12.5, by + 6.5, 15.0, alpha);
            }
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

    /// Where the Wi-Fi list's parts are (content-relative y).
    fn wifi_layout(&self) -> WifiLayout {
        let top = 60.0;
        let rows = self.wifi.nets.len().min(WIFI_ROWS);
        let note = top + rows as f32 * WIFI_ROW + 4.0;
        // A line for what's going on: scanning, nothing found, a pick's result.
        let note_h = if self.wifi_note().is_some() { 24.0 } else { 0.0 };
        let foot = note + note_h + 6.0;
        WifiLayout { top, rows, note, foot, total: foot + WIFI_BTN_H + 14.0 }
    }

    /// The line under the Wi-Fi list, if there's something to say.
    fn wifi_note(&self) -> Option<String> {
        if self.radios.wifi == Some(false) {
            return Some("Wi-Fi is off.".into());
        }
        if self.wifi.missing {
            return Some("No Wi-Fi adapter found.".into());
        }
        if let Some(n) = &self.wifi.note {
            return Some(n.clone());
        }
        if self.wifi.scanning {
            return Some("Looking for networks\u{2026}".into());
        }
        self.wifi.nets.is_empty().then(|| "No networks in range.".into())
    }

    /// Controls' Wi-Fi list: back, the Wi-Fi switch, the networks in range
    /// (the one you're on first), then Windows' own Wi-Fi settings and quick
    /// settings for anything this doesn't do.
    fn draw_wifi(&self, p: &Painter, alpha: f32) {
        let ox = self.origin;
        let tw = self.content_w;
        let fonts = &self.gfx.fonts;
        let white = Color::white(0.95 * alpha);
        let dim = Color::white(0.55 * alpha);
        let l = self.wifi_layout();
        // Header: a back chevron, the title, the switch.
        if self.hovered == Some(Hot::WifiBack) {
            p.rrect(ox + 14.0, 14.0, 36.0, 34.0, 10.0, Color::white(0.08 * alpha));
        }
        let (bx, by) = (ox + 34.0, 31.0);
        p.line(bx + 3.0, by - 7.0, bx - 4.0, by, white, 1.8);
        p.line(bx - 4.0, by, bx + 3.0, by + 7.0, white, 1.8);
        p.text("Wi-Fi", &fonts.title, ox + 58.0, 14.0, tw - 160.0, 34.0, Align::Left, white);
        let on = self.radios.wifi == Some(true);
        let (sx, sy) = (ox + tw - 18.0 - WIFI_SWITCH_W, 21.0);
        p.rrect(
            sx,
            sy,
            WIFI_SWITCH_W,
            20.0,
            10.0,
            if on { theme::accent().alpha(alpha) } else { Color::white(0.18 * alpha) },
        );
        let knob = if on { sx + WIFI_SWITCH_W - 10.0 } else { sx + 10.0 };
        p.circle(knob, sy + 10.0, 7.0, white);
        // The networks.
        for (i, n) in self.wifi.nets.iter().take(l.rows).enumerate() {
            let y = l.top + i as f32 * WIFI_ROW;
            if self.hovered == Some(Hot::WifiNet(i)) && !n.connected {
                p.rrect(ox + 14.0, y + 1.0, tw - 28.0, WIFI_ROW - 2.0, 9.0, Color::white(0.07 * alpha));
            }
            let cy = y + WIFI_ROW / 2.0;
            signal_bars(
                p,
                ox + 30.0,
                cy,
                wifi::bars(n.signal),
                if n.connected { theme::accent().alpha(alpha) } else { white },
                alpha,
            );
            let name_w = tw - 150.0;
            p.text(&n.ssid, &fonts.label, ox + 52.0, y, name_w, WIFI_ROW, Align::Left, white);
            let mut right = ox + tw - 26.0;
            if n.secure {
                padlock(p, right - 5.0, cy, dim);
                right -= 20.0;
            }
            let status = if n.connected {
                "Connected"
            } else if n.profile.is_some() {
                "Saved"
            } else {
                ""
            };
            if !status.is_empty() {
                let c = if n.connected { theme::accent().alpha(alpha) } else { dim };
                p.text(status, &fonts.small, right - 80.0, y, 76.0, WIFI_ROW, Align::Right, c);
            }
        }
        if let Some(note) = self.wifi_note() {
            p.text(&note, &fonts.small, ox + 20.0, l.note, tw - 40.0, 22.0, Align::Left, dim);
        }
        // Ways on to Windows' own.
        let bw = (tw - 36.0 - 8.0) / 2.0;
        for (i, (hot, label)) in
            [(Hot::WifiSettings, "More Wi-Fi settings"), (Hot::WifiQuick, "Windows quick settings")]
                .into_iter()
                .enumerate()
        {
            let x = ox + 18.0 + i as f32 * (bw + 8.0);
            let fill = if self.hovered == Some(hot) { 0.13 } else { 0.08 };
            p.rrect(x, l.foot, bw, WIFI_BTN_H, 10.0, Color::white(fill * alpha));
            p.text(label, &fonts.small, x, l.foot, bw, WIFI_BTN_H, Align::Center, white);
        }
    }

    /// Where the Bluetooth list's parts are (content-relative y), laid out
    /// like the Wi-Fi list.
    fn bt_layout(&self) -> WifiLayout {
        let top = 60.0;
        let rows = self.bt.devices.len().min(WIFI_ROWS);
        let note = top + rows as f32 * WIFI_ROW + 4.0;
        let note_h = if self.bt_note().is_some() { 24.0 } else { 0.0 };
        let foot = note + note_h + 6.0;
        WifiLayout { top, rows, note, foot, total: foot + WIFI_BTN_H + 14.0 }
    }

    /// The line under the Bluetooth list, if there's something to say.
    fn bt_note(&self) -> Option<String> {
        if self.radios.bluetooth == Some(false) {
            return Some("Bluetooth is off.".into());
        }
        if let Some(n) = &self.bt.note {
            return Some(n.clone());
        }
        if self.bt.loading && self.bt.devices.is_empty() {
            return Some("Looking for your devices\u{2026}".into());
        }
        self.bt.devices.is_empty().then(|| "Nothing paired yet: add devices in Bluetooth settings.".into())
    }

    /// The name of the first connected Bluetooth device, for its tile.
    fn bt_connected(&self) -> Option<&str> {
        self.bt.devices.iter().find(|d| d.connected).map(|d| d.name.as_str())
    }

    /// Controls' Bluetooth list: back, the Bluetooth switch, your paired
    /// devices (connected first; click one that plays sound to connect or
    /// disconnect it), then Windows' own settings.
    fn draw_bt(&self, p: &Painter, alpha: f32) {
        let ox = self.origin;
        let tw = self.content_w;
        let fonts = &self.gfx.fonts;
        let white = Color::white(0.95 * alpha);
        let dim = Color::white(0.55 * alpha);
        let l = self.bt_layout();
        if self.hovered == Some(Hot::BtBack) {
            p.rrect(ox + 14.0, 14.0, 36.0, 34.0, 10.0, Color::white(0.08 * alpha));
        }
        let (bx, by) = (ox + 34.0, 31.0);
        p.line(bx + 3.0, by - 7.0, bx - 4.0, by, white, 1.8);
        p.line(bx - 4.0, by, bx + 3.0, by + 7.0, white, 1.8);
        p.text("Bluetooth", &fonts.title, ox + 58.0, 14.0, tw - 160.0, 34.0, Align::Left, white);
        let on = self.radios.bluetooth == Some(true);
        let (sx, sy) = (ox + tw - 18.0 - WIFI_SWITCH_W, 21.0);
        p.rrect(
            sx,
            sy,
            WIFI_SWITCH_W,
            20.0,
            10.0,
            if on { theme::accent().alpha(alpha) } else { Color::white(0.18 * alpha) },
        );
        let knob = if on { sx + WIFI_SWITCH_W - 10.0 } else { sx + 10.0 };
        p.circle(knob, sy + 10.0, 7.0, white);
        for (i, d) in self.bt.devices.iter().take(l.rows).enumerate() {
            let y = l.top + i as f32 * WIFI_ROW;
            if self.hovered == Some(Hot::BtDev(i)) {
                p.rrect(ox + 14.0, y + 1.0, tw - 28.0, WIFI_ROW - 2.0, 9.0, Color::white(0.07 * alpha));
            }
            let cy = y + WIFI_ROW / 2.0;
            let c = if d.connected { theme::accent().alpha(alpha) } else { white };
            device_glyph(p, ox + 32.0, cy, d.kind, c);
            p.text(&d.name, &fonts.label, ox + 52.0, y, tw - 170.0, WIFI_ROW, Align::Left, white);
            let hovered = self.hovered == Some(Hot::BtDev(i));
            let status = match (d.connected, d.audio && hovered && on) {
                (true, true) => "Disconnect",
                (false, true) => "Connect",
                (true, false) => "Connected",
                (false, false) => "",
            };
            if !status.is_empty() {
                let c = if d.connected && !hovered { theme::accent().alpha(alpha) } else { dim };
                p.text(status, &fonts.small, ox + tw - 26.0 - 96.0, y, 96.0, WIFI_ROW, Align::Right, c);
            }
        }
        if let Some(note) = self.bt_note() {
            p.text(&note, &fonts.small, ox + 20.0, l.note, tw - 40.0, 22.0, Align::Left, dim);
        }
        let bw = (tw - 36.0 - 8.0) / 2.0;
        for (i, (hot, label)) in
            [(Hot::BtSettings, "More Bluetooth settings"), (Hot::WifiQuick, "Windows quick settings")]
                .into_iter()
                .enumerate()
        {
            let x = ox + 18.0 + i as f32 * (bw + 8.0);
            let fill = if self.hovered == Some(hot) { 0.13 } else { 0.08 };
            p.rrect(x, l.foot, bw, WIFI_BTN_H, 10.0, Color::white(fill * alpha));
            p.text(label, &fonts.small, x, l.foot, bw, WIFI_BTN_H, Align::Center, white);
        }
    }

    fn draw_controls(&self, p: &Painter, alpha: f32) {
        if self.wifi_open {
            return self.draw_wifi(p, alpha);
        }
        if self.bt_open {
            return self.draw_bt(p, alpha);
        }
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
            p.circle(icx, icy, 15.0, if on { theme::accent().alpha(alpha) } else { Color::white(0.14 * alpha) });
            let white = Color::white(0.95 * alpha);
            match hot {
                Hot::Wifi => wifi(p, icx, icy, white),
                Hot::Bluetooth => bluetooth(p, icx, icy, white),
                _ => bell(p, icx, icy, white),
            }
            // The Wi-Fi tile names the network you're on, and has a chevron
            // that opens the list of them.
            let joined = (hot == Hot::Wifi && state == Some(true))
                .then(|| self.wifi.nets.iter().find(|n| n.connected).map(|n| n.ssid.as_str()))
                .flatten();
            let paired = (hot == Hot::Bluetooth && state == Some(true)).then(|| self.bt_connected()).flatten();
            let status = match (hot, state) {
                (Hot::Notifications, _) => "Open",
                (Hot::Wifi, Some(true)) if joined.is_some() => joined.unwrap_or("On"),
                (Hot::Bluetooth, Some(true)) if paired.is_some() => paired.unwrap_or("On"),
                (_, Some(true)) => "On",
                (_, Some(false)) => "Off",
                (_, None) => "Unavailable",
            };
            // The Wi-Fi and Bluetooth tiles have a chevron that opens their list.
            let more = match hot {
                Hot::Wifi if state.is_some() => Some(Hot::WifiMore),
                Hot::Bluetooth if state.is_some() => Some(Hot::BtMore),
                _ => None,
            };
            let text_w = if more.is_some() { tile_w - 50.0 - WIFI_MORE_W + 8.0 } else { tile_w - 50.0 };
            p.text(label, &fonts.label, x + 46.0, y + 9.0, text_w, 18.0, Align::Left, white);
            p.text(status, &fonts.small, x + 46.0, y + 27.0, text_w, 16.0, Align::Left, Color::white(0.55 * alpha));
            if let Some(more) = more {
                let mx = x + tile_w - WIFI_MORE_W;
                if self.hovered == Some(more) {
                    p.rrect(mx + 2.0, y + 6.0, WIFI_MORE_W - 6.0, 42.0, 10.0, Color::white(0.10 * alpha));
                }
                let (cx, cy, c) = (mx + WIFI_MORE_W / 2.0 - 1.0, y + 27.0, Color::white(0.75 * alpha));
                p.line(cx - 2.5, cy - 5.0, cx + 2.5, cy, c, 1.6);
                p.line(cx + 2.5, cy, cx - 2.5, cy + 5.0, c, 1.6);
            }
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
        self.draw_mixer(p, alpha);
    }

    /// Where the mixer's rows are: (what, top, height). An app's row is its
    /// name (to mute it) and its slider.
    fn mixer_rows(&self) -> Vec<(Hot, f32, f32)> {
        let mut out = Vec::new();
        if self.mixer.outputs.is_empty() {
            return out;
        }
        let mut y = MIX_TOP + MIX_GAP;
        out.push((Hot::Output, y, MIX_ROW));
        y += MIX_ROW;
        if self.outputs_open {
            for i in 0..self.mixer.outputs.len() {
                out.push((Hot::OutputItem(i), y, MIX_ITEM_H));
                y += MIX_ITEM_H;
            }
        }
        for i in 0..self.mixer.apps.len().min(MIX_APPS) {
            out.push((Hot::AppMute(i), y, MIX_ROW));
            out.push((Hot::AppBar(i), y, MIX_ROW));
            y += MIX_ROW;
        }
        out
    }

    /// The mixer under the sliders: the output device (a list of them to
    /// pick from when open), then each app with its own volume.
    fn draw_mixer(&self, p: &Painter, alpha: f32) {
        let ox = self.origin;
        let tw = self.content_w;
        let fonts = &self.gfx.fonts;
        let white = Color::white(0.95 * alpha);
        let dim = Color::white(0.55 * alpha);
        if !self.mixer.outputs.is_empty() {
            p.rrect(ox + 18.0, MIX_TOP + MIX_GAP / 2.0, tw - 36.0, 1.0, 0.0, Color::white(0.08 * alpha));
        }
        for (hot, y, h) in self.mixer_rows() {
            let cy = y + h / 2.0;
            let hovered = self.hovered == Some(hot);
            match hot {
                Hot::Output => {
                    if hovered && self.mixer.outputs.len() > 1 {
                        p.rrect(ox + 14.0, y + 1.0, tw - 28.0, h - 2.0, 7.0, Color::white(0.07 * alpha));
                    }
                    speaker(p, ox + 27.0, cy, 15.0, 1.0, false, dim);
                    let name = self.mixer.outputs.get(self.mixer.current).map_or("Output", |o| o.name.as_str());
                    p.text(name, &fonts.label, ox + 46.0, y, tw - 46.0 - 44.0, h, Align::Left, white);
                    if self.mixer.outputs.len() > 1 {
                        // Down when the list is closed, up when it's open.
                        let (cx, s) = (ox + tw - 30.0, if self.outputs_open { -1.0 } else { 1.0 });
                        p.line(cx - 4.0, cy - 2.0 * s, cx, cy + 2.0 * s, dim, 1.4);
                        p.line(cx, cy + 2.0 * s, cx + 4.0, cy - 2.0 * s, dim, 1.4);
                    }
                }
                Hot::OutputItem(i) => {
                    if hovered {
                        p.rrect(ox + 30.0, y + 1.0, tw - 48.0, h - 2.0, 6.0, Color::white(0.08 * alpha));
                    }
                    if i == self.mixer.current {
                        p.circle(ox + 40.0, cy, 3.0, theme::accent().alpha(alpha));
                    }
                    let name = &self.mixer.outputs[i].name;
                    p.text(name, &fonts.small, ox + 52.0, y, tw - 52.0 - 24.0, h, Align::Left, white);
                }
                Hot::AppMute(i) => {
                    let a = &self.mixer.apps[i];
                    if hovered {
                        p.rrect(ox + 14.0, y + 2.0, MIX_SLIDER_X - 22.0, h - 4.0, 6.0, Color::white(0.07 * alpha));
                    }
                    let c = if a.muted { dim } else { white };
                    speaker(p, ox + 27.0, cy, 13.0, a.level, a.muted, c);
                    p.text(&a.name, &fonts.small, ox + 42.0, y, MIX_SLIDER_X - 54.0, h, Align::Left, c);
                }
                Hot::AppBar(i) => {
                    let a = &self.mixer.apps[i];
                    let (x0, x1) = (ox + MIX_SLIDER_X, ox + tw - 60.0);
                    let track = if hovered || self.dragging == Some(hot) { 8.0 } else { 6.0 };
                    p.rrect(x0, cy - track / 2.0, x1 - x0, track, track / 2.0, Color::white(0.15 * alpha));
                    let fill = (x1 - x0) * a.level.clamp(0.0, 1.0);
                    if fill > 0.5 {
                        let c = Color::white(if a.muted { 0.35 } else { 0.9 } * alpha);
                        p.rrect(x0, cy - track / 2.0, fill.max(track), track, track / 2.0, c);
                    }
                    let pct = format!("{}", (a.level * 100.0).round() as i32);
                    p.text(&pct, &fonts.small, ox + tw - 54.0, y, 36.0, h, Align::Right, dim);
                }
                _ => {}
            }
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
        // Days this month with something on.
        let month_start = ics::days_from_civil(year, month, 1);
        let busy: Vec<u32> = self
            .events
            .iter()
            .flat_map(|e| {
                let last = if e.end > e.start { (e.end - 1).div_euclid(DAY) } else { e.start.div_euclid(DAY) };
                e.start.div_euclid(DAY)..=last
            })
            .filter_map(|d| (d >= month_start).then_some((d - month_start + 1) as u32))
            .collect();

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
                p.circle(cx + cell / 2.0, cy + 11.0, 11.0, theme::accent().alpha(alpha));
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
            if busy.contains(&day) {
                let c = if is_today { Color::white(alpha) } else { theme::accent().alpha(alpha) };
                p.circle(cx + cell / 2.0, cy + 19.5, 1.6, c);
            }
        }

        // What's coming up: the next few events this week.
        let agenda = self.agenda();
        if !agenda.is_empty() {
            let tw = self.content_w;
            let now = calendar::now();
            p.rrect(gx, 198.0, tw - 36.0, 1.0, 0.0, Color::white(0.08 * alpha));
            for (i, e) in agenda.iter().enumerate() {
                let y = 202.0 + i as f32 * 22.0;
                let on = e.start <= now && e.end > now;
                let c = if on { theme::accent() } else { Color::white(0.6) };
                p.text(&when(e, now), &fonts.small, gx + 4.0, y, 112.0, 22.0, Align::Left, c.alpha(alpha));
                let what =
                    if e.location.is_empty() { e.title.clone() } else { format!("{} \u{b7} {}", e.title, e.location) };
                let x = gx + 120.0;
                p.text(&what, &fonts.small, x, y, ox + tw - 18.0 - x, 22.0, Align::Left, Color::white(0.92 * alpha));
            }
        }

        // Focus timer.
        let (rx, rw) = (ox + 226.0, 176.0);
        let c = if self.pomo.focus { theme::accent() } else { GREEN };
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

// --------------------------------------------------------------- tab bar --

/// The mixer on the Controls tab: where it starts (under the sliders), the
/// gap above it, its rows (the output, each app), the rows of the list of
/// outputs, where the apps' sliders start, and the most apps it lists.
/// Controls' Wi-Fi list: a network's row, how many show, the buttons under
/// them, the switch in its header and the chevron on the Wi-Fi tile.
const WIFI_ROW: f32 = 36.0;
const WIFI_ROWS: usize = 6;
const WIFI_BTN_H: f32 = 32.0;
const WIFI_SWITCH_W: f32 = 40.0;
const WIFI_MORE_W: f32 = 26.0;

/// Where the parts of Controls' Wi-Fi list are (see `App::wifi_layout`).
struct WifiLayout {
    top: f32,
    rows: usize,
    note: f32,
    foot: f32,
    total: f32,
}

const MIX_TOP: f32 = 148.0;
const MIX_GAP: f32 = 10.0;
const MIX_ROW: f32 = 28.0;
const MIX_ITEM_H: f32 = 26.0;
const MIX_SLIDER_X: f32 = 160.0;
const MIX_APPS: usize = 5;

/// Height of the open notch's tab bar, and the room it takes at the bottom.
const TAB_H: f32 = 30.0;
const TAB_BAR: f32 = TAB_H + 12.0 + 6.0;

/// Left edge and top of the tab bar in an open notch `tw` x `th`, and the
/// width of one tab.
fn tab_bar(tw: f32, th: f32) -> (f32, f32, f32) {
    let (x0, x1) = (16.0, tw - 16.0);
    (x0, th - TAB_H - 12.0, (x1 - x0) / MODES.len() as f32)
}

/// The little picture on a tab.
fn tab_icon(p: &Painter, mode: Mode, media: bool, cx: f32, cy: f32, c: Color) {
    match mode {
        Mode::Media if media => {
            // A quaver.
            p.circle(cx - 2.2, cy + 3.6, 2.6, c);
            p.line(cx + 0.2, cy + 3.6, cx + 0.2, cy - 5.6, c, 1.4);
            p.line(cx + 0.2, cy - 5.6, cx + 4.6, cy - 3.4, c, 1.4);
        }
        Mode::Media => {
            // A clock.
            p.stroke_circle(cx, cy, 5.8, c, 1.3);
            p.line(cx, cy, cx, cy - 3.4, c, 1.3);
            p.line(cx, cy, cx + 2.6, cy, c, 1.3);
        }
        Mode::Controls => {
            // Two sliders.
            p.line(cx - 6.0, cy - 2.8, cx + 6.0, cy - 2.8, c, 1.3);
            p.circle(cx - 2.0, cy - 2.8, 2.0, c);
            p.line(cx - 6.0, cy + 2.8, cx + 6.0, cy + 2.8, c, 1.3);
            p.circle(cx + 2.6, cy + 2.8, 2.0, c);
        }
        Mode::Calendar => {
            p.stroke_rrect(cx - 5.6, cy - 4.4, 11.2, 10.4, 2.2, c, 1.2);
            p.line(cx - 5.6, cy - 1.2, cx + 5.6, cy - 1.2, c, 1.2);
            p.line(cx - 2.6, cy - 6.4, cx - 2.6, cy - 3.4, c, 1.2);
            p.line(cx + 2.6, cy - 6.4, cx + 2.6, cy - 3.4, c, 1.2);
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

/// The visualizer's level for bar `i` of `n` in a row, from the bass on the
/// left to the treble on the right, blended between the bands.
fn band_at(levels: &[f32; BANDS], i: usize, n: usize) -> f32 {
    let pos = (if n > 1 { i as f32 / (n - 1) as f32 } else { 0.0 }) * (BANDS - 1) as f32;
    let k = (pos.floor() as usize).min(BANDS - 1);
    let next = levels[(k + 1).min(BANDS - 1)];
    (levels[k] + (next - levels[k]) * (pos - k as f32)).clamp(0.0, 1.0)
}

/// An equaliser: `n` bars across `w` from `x`, standing on a faint baseline
/// at `base` and up to `max_h` tall, the bass on the left and the treble
/// on the right.
pub(crate) fn spectrum(p: &Painter, levels: &[f32; BANDS], x: f32, base: f32, w: f32, n: usize, max_h: f32, c: Color) {
    if n == 0 || w <= 0.0 {
        return;
    }
    let line = p.px(1.0);
    p.rrect(x, base - line, w, line, 0.0, c.alpha(0.3));
    let slot = w / n as f32;
    let bar = (slot * 0.62).max(1.0);
    for i in 0..n {
        let h = (max_h * band_at(levels, i, n)).max(line * 2.0).min(max_h);
        let r = (bar / 2.0).min(h / 2.0).min(1.5);
        p.rrect(x + i as f32 * slot + (slot - bar) / 2.0, base - line - h, bar, h, r, c);
    }
}

/// Which of the speaker icon's looks a volume gets (muted, silent, one wave,
/// two waves): see `speaker`.
fn speaker_tier(level: f32, muted: bool) -> u8 {
    if muted {
        0
    } else if level <= 0.01 {
        1
    } else if level <= 0.45 {
        2
    } else {
        3
    }
}

pub(crate) fn speaker(p: &Painter, cx: f32, cy: f32, s: f32, level: f32, muted: bool, c: Color) {
    let (cx, cy) = (p.snap(cx), p.snap(cy));
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
    let stroke = p.px(s * 0.09);
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

/// A lightning bolt `h` tall centred on (cx, cy), on a battery that's
/// plugged in. Edged in dark glass, so it reads over the battery's fill and
/// over an empty one alike.
pub(crate) fn bolt(p: &Painter, cx: f32, cy: f32, h: f32, alpha: f32) {
    const SHAPE: [(f32, f32); 6] =
        [(0.14, -0.5), (-0.3, 0.08), (-0.03, 0.08), (-0.14, 0.5), (0.3, -0.08), (0.03, -0.08)];
    let pts: Vec<(f32, f32)> = SHAPE.iter().map(|&(x, y)| (cx + x * h, cy + y * h)).collect();
    p.polygon_edged(&pts, Color::white(0.97 * alpha), Color(0.0, 0.0, 0.0, 0.55 * alpha), h * 0.18);
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

/// Signal strength: four bars rising to the right, `n` of them lit.
fn signal_bars(p: &Painter, x: f32, cy: f32, n: u32, c: Color, alpha: f32) {
    for i in 0..4u32 {
        let h = 4.0 + i as f32 * 3.5;
        let lit = i < n;
        let col = if lit { c } else { Color::white(0.18 * alpha) };
        p.rrect(x - 8.0 + i as f32 * 4.5, cy + 7.0 - h, 3.0, h, 1.0, col);
    }
}

/// A small padlock (a secured network).
fn padlock(p: &Painter, cx: f32, cy: f32, c: Color) {
    p.rrect(cx - 4.5, cy - 1.0, 9.0, 7.0, 1.5, c);
    p.arc(cx, cy - 1.0, 3.0, std::f32::consts::PI, std::f32::consts::TAU, c, 1.4);
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

/// A small picture of a Bluetooth device's kind: headphones, a mouse, a phone,
/// or the Bluetooth mark.
fn device_glyph(p: &Painter, cx: f32, cy: f32, kind: bt::Kind, c: Color) {
    match kind {
        bt::Kind::Audio => {
            // A headband over two ear cups.
            let pts: Vec<(f32, f32)> = (0..=8)
                .map(|i| std::f32::consts::PI * (1.0 + i as f32 / 8.0))
                .map(|t| (cx + 6.5 * t.cos(), cy + 1.0 + 7.0 * t.sin()))
                .collect();
            for w in pts.windows(2) {
                p.line(w[0].0, w[0].1, w[1].0, w[1].1, c, 1.5);
            }
            p.rrect(cx - 8.0, cy + 1.0, 4.0, 6.5, 1.5, c);
            p.rrect(cx + 4.0, cy + 1.0, 4.0, 6.5, 1.5, c);
        }
        bt::Kind::Input => {
            p.stroke_rrect(cx - 4.5, cy - 7.0, 9.0, 14.0, 4.5, c, 1.4);
            p.line(cx, cy - 7.0, cx, cy - 2.5, c, 1.2);
        }
        bt::Kind::Phone => {
            p.stroke_rrect(cx - 4.5, cy - 7.5, 9.0, 15.0, 2.0, c, 1.4);
            p.line(cx - 1.5, cy + 5.0, cx + 1.5, cy + 5.0, c, 1.2);
        }
        bt::Kind::Other => bluetooth(p, cx, cy, c),
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
