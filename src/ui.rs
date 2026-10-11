//! The notch window: state, layout, input and the frame loop.
//!
//! Idle cost is zero: the thread blocks in GetMessage until something
//! happens (hover, media/volume event, timer). Frames are only produced
//! while a spring is moving, paced by a high-resolution waitable timer.

use std::cell::RefCell;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, ENUM_DATE_FORMATS_FLAGS, TIME_NOSECONDS};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::ABN_FULLSCREENAPP;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::audio::{self, Visualizer, Volume, BANDS};
use crate::brightness;
use crate::config::{self, Config};
use crate::gfx::{Align, Color, Gfx, Painter};
use crate::media::{self, Art, MediaCmd, MediaInfo};
use crate::msg;
use crate::shell::{self, MenuCmd, MenuState, Tray};
use crate::spring::Spring;

/// Width of the concave flares joining the notch to the screen edge.
const EAR: f32 = 9.0;
const PEEK_MS: u32 = 1600;

const TIMER_EXPAND: usize = 1;
const TIMER_COLLAPSE: usize = 2;
const TIMER_PEEK: usize = 3;
const TIMER_TICK: usize = 4;
const TIMER_FLYOUT: usize = 5;

/// Not exported by the `windows` crate features we enable.
const WM_MOUSELEAVE: u32 = 0x02A3;

const BLACK: Color = Color(0.0, 0.0, 0.0, 1.0);

#[derive(Clone, Copy, PartialEq, Debug)]
enum View {
    Idle,
    Live,
    Peek,
    Expanded,
}

#[derive(Clone, Copy, PartialEq)]
enum PeekKind {
    Volume,
    Brightness,
}

#[derive(Clone, Copy, PartialEq)]
enum Button {
    Previous,
    PlayPause,
    Next,
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
    refresh_hz: u32,

    w: Spring,
    h: Spring,
    r: Spring,
    fade: Spring,
    level: Spring,
    bars: [Spring; BANDS],
    last_frame: Instant,
    dirty: bool,

    view: View,
    expanded: bool,
    expand_pending: bool,
    tracking: bool,
    hovered: Option<Button>,
    peek: Option<PeekKind>,
    fullscreen: bool,
    shown: bool,

    media: Option<MediaInfo>,
    art: Option<(Arc<Art>, ID2D1Bitmap)>,
    media_tx: Sender<MediaCmd>,
    volume: Option<Volume>,
    vol: (f32, bool),
    brightness: u32,
    visualizer: Visualizer,
    flyout_hidden: bool,

    /// Content origin and width of the last frame, for hit-testing.
    origin: f32,
    content_w: f32,
}

pub fn run() -> Result<()> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("CascadeNotch");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("Cascade"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance),
            None,
        )?;
        msg::set_target(hwnd);
        let app = App::new(hwnd)?;
        APP.with(|c| *c.borrow_mut() = Some(app));
        with_app(|a| a.retarget());

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

fn show_menu(hwnd: HWND) {
    let Some(state) = with_app(|a| a.menu_state()) else { return };
    let cmd = shell::show_menu(hwnd, state);
    if cmd == MenuCmd::Quit {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    } else {
        with_app(|a| a.on_menu(cmd));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_RBUTTONUP => {
            show_menu(hwnd);
            return LRESULT(0);
        }
        msg::TRAY => {
            let event = (lp.0 & 0xFFFF) as u32;
            if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
                show_menu(hwnd);
            }
            return LRESULT(0);
        }
        msg::MEDIA => {
            // Always reclaim the box, even if the app is busy or gone.
            let info = Box::from_raw(lp.0 as *mut Option<MediaInfo>);
            with_app(move |a| a.on_media(*info));
            return LRESULT(0);
        }
        WM_DESTROY => {
            with_app(|a| a.shutdown());
            PostQuitMessage(0);
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
    match with_app(|a| a.handle(m, wp, lp)).flatten() {
        Some(r) => r,
        None => DefWindowProcW(hwnd, m, wp, lp),
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

fn clock() -> (String, String) {
    let mut t = [0u16; 64];
    let mut d = [0u16; 128];
    unsafe {
        let nt = GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, None, PCWSTR::null(), Some(&mut t));
        let nd = GetDateFormatEx(
            PCWSTR::null(),
            ENUM_DATE_FORMATS_FLAGS(0),
            None,
            w!("dddd d MMMM"),
            Some(&mut d),
            PCWSTR::null(),
        );
        let s = |b: &[u16], n: i32| String::from_utf16_lossy(&b[..(n.max(1) - 1) as usize]);
        (s(&t, nt), s(&d, nd))
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

impl App {
    fn new(hwnd: HWND) -> Result<Self> {
        let cfg = Config::load();
        let gfx = Gfx::new()?;
        let volume = Volume::new().ok();
        let vol = volume.as_ref().and_then(|v| v.get()).unwrap_or((0.0, false));
        let media_tx = media::spawn();
        brightness::spawn();
        shell::register_appbar(hwnd);
        let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        let size = Spring::new(0.0, 420.0, 32.0);
        let mut app = Self {
            hwnd,
            gfx,
            tray: Some(Tray::new(hwnd)),
            taskbar_created,
            monitor: RECT::default(),
            k: 1.0,
            refresh_hz: 60,
            // Start small and flat so the notch grows in on launch.
            w: {
                let mut s = size;
                s.value = cfg.idle_width * 0.4;
                s
            },
            h: size,
            r: Spring::new(0.0, 420.0, 34.0),
            fade: Spring::new(0.0, 260.0, 30.0),
            level: Spring::new(vol.0, 500.0, 40.0),
            bars: [Spring::new(0.0, 700.0, 30.0); BANDS],
            last_frame: Instant::now(),
            dirty: true,
            view: View::Idle,
            expanded: false,
            expand_pending: false,
            tracking: false,
            hovered: None,
            peek: None,
            fullscreen: shell::fullscreen_now(),
            shown: false,
            media: None,
            art: None,
            media_tx,
            volume,
            vol,
            brightness: 0,
            visualizer: Visualizer::default(),
            flyout_hidden: false,
            origin: 0.0,
            content_w: 0.0,
            cfg,
        };
        app.update_display();
        app.apply_flyout();
        Ok(app)
    }

    fn shutdown(&mut self) {
        self.visualizer.set(false);
        shell::unregister_appbar(self.hwnd);
        self.tray = None;
        if self.flyout_hidden {
            shell::set_flyout_hidden(false);
            self.flyout_hidden = false;
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
            self.k = dx as f32 / 96.0 * self.cfg.scale;
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
        let want = self.cfg.hide_windows_flyout && (self.cfg.volume_osd || self.cfg.brightness_osd);
        // Also run when `want` is false: that restores a flyout left hidden
        // by a previous run that did not exit cleanly.
        shell::set_flyout_hidden(want);
        self.flyout_hidden = want;
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
        } else if self.cfg.live_activity && self.playing() {
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
            View::Peek => (iw + 110.0, ih + 12.0, (ih + 12.0) * 0.4),
            View::Expanded if self.media.is_some() => (420.0, 148.0, 30.0),
            View::Expanded => (300.0, 92.0, 26.0),
        }
    }

    /// Re-derives the view and animation targets after any state change.
    fn retarget(&mut self) {
        let visible = !(self.cfg.hide_in_fullscreen && self.fullscreen);
        if visible != self.shown {
            self.shown = visible;
            unsafe {
                let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
            if !visible {
                self.expanded = false;
                self.peek = None;
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

        let visualize =
            self.shown && self.cfg.visualizer && self.playing() && matches!(view, View::Live | View::Expanded);
        self.visualizer.set(visualize);
        if !visualize {
            for b in &mut self.bars {
                b.set(0.0);
            }
        }
        self.dirty = true;
    }

    fn show_peek(&mut self, kind: PeekKind) {
        if !self.shown {
            return;
        }
        let value = match kind {
            PeekKind::Volume => self.vol.0,
            PeekKind::Brightness => self.brightness as f32 / 100.0,
        };
        if self.peek != Some(kind) {
            self.level.snap(value);
        }
        self.level.set(value);
        self.peek = Some(kind);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_PEEK, PEEK_MS, None);
            if self.flyout_hidden {
                // The flyout host can be created lazily on first use.
                shell::set_flyout_hidden(true);
                SetTimer(Some(self.hwnd), TIMER_FLYOUT, 60, None);
            }
        }
        self.retarget();
    }

    // ----------------------------------------------------------- events --

    fn handle(&mut self, m: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match m {
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lp);
                self.on_mouse_move(x, y);
            }
            WM_MOUSELEAVE => self.on_mouse_leave(),
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lp);
                self.on_click(x, y);
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16;
                if self.cfg.scroll_volume {
                    if let Some(v) = &self.volume {
                        v.nudge(if delta > 0 { 0.02 } else { -0.02 });
                    }
                }
            }
            WM_TIMER => self.on_timer(wp.0),
            msg::VOLUME => {
                self.vol = (wp.0 as f32 / 1000.0, lp.0 != 0);
                if self.cfg.volume_osd {
                    self.show_peek(PeekKind::Volume);
                }
            }
            msg::BRIGHTNESS => {
                self.brightness = wp.0 as u32;
                if self.cfg.brightness_osd {
                    self.show_peek(PeekKind::Brightness);
                }
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
            msg::APPBAR => {
                if wp.0 as u32 == ABN_FULLSCREENAPP {
                    self.fullscreen = lp.0 != 0;
                    self.retarget();
                }
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                self.update_display();
                self.retarget();
            }
            _ if m == self.taskbar_created && m != 0 => {
                if let Some(t) = &self.tray {
                    t.add();
                }
                shell::register_appbar(self.hwnd);
            }
            _ => return None,
        }
        Some(LRESULT(0))
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
        let hit = self.hit(x, y);
        if hit != self.hovered {
            self.hovered = hit;
            self.dirty = true;
        }
    }

    fn on_mouse_leave(&mut self) {
        self.tracking = false;
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

    fn on_click(&mut self, x: i32, y: i32) {
        if !self.expanded {
            self.expanded = true;
            self.expand_pending = false;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_EXPAND);
            }
            self.retarget();
            return;
        }
        let Some(button) = self.hit(x, y) else { return };
        let cmd = match button {
            Button::Previous => MediaCmd::Previous,
            Button::Next => MediaCmd::Next,
            Button::PlayPause => {
                // Flip right away; the real state follows from the player.
                if let Some(m) = &mut self.media {
                    m.position = m.position_now();
                    m.stamp = Instant::now();
                    m.playing = !m.playing;
                }
                MediaCmd::PlayPause
            }
        };
        let _ = self.media_tx.send(cmd);
        self.retarget();
    }

    fn on_timer(&mut self, id: usize) {
        unsafe {
            if id != TIMER_TICK {
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
                self.retarget();
            }
            TIMER_PEEK => {
                self.peek = None;
                self.retarget();
            }
            TIMER_TICK => self.dirty = true,
            TIMER_FLYOUT => shell::set_flyout_hidden(self.flyout_hidden),
            _ => {}
        }
    }

    fn menu_state(&self) -> MenuState {
        MenuState { autostart: shell::autostart_enabled(), flyout_hidden: self.cfg.hide_windows_flyout }
    }

    fn on_menu(&mut self, cmd: MenuCmd) {
        match cmd {
            MenuCmd::Autostart => shell::set_autostart(!shell::autostart_enabled()),
            MenuCmd::Flyout => {
                self.cfg.hide_windows_flyout = !self.cfg.hide_windows_flyout;
                Config::set("hide_windows_flyout", if self.cfg.hide_windows_flyout { "true" } else { "false" });
                self.apply_flyout();
            }
            MenuCmd::OpenConfig => shell::open_file(&config::path()),
            MenuCmd::Reload => {
                self.cfg = Config::load();
                self.update_display();
                self.apply_flyout();
                self.retarget();
            }
            MenuCmd::None | MenuCmd::Quit => {}
        }
    }

    /// Which media button is under a client-area pixel, if any.
    fn hit(&self, x: i32, y: i32) -> Option<Button> {
        if self.view != View::Expanded || self.media.is_none() {
            return None;
        }
        let lx = x as f32 / self.k - self.origin;
        let ly = y as f32 / self.k;
        let cx = self.content_w / 2.0;
        [(Button::Previous, cx - 60.0), (Button::PlayPause, cx), (Button::Next, cx + 60.0)]
            .into_iter()
            .find(|(_, bx)| (lx - bx).powi(2) + (ly - 124.0).powi(2) < 21.0 * 21.0)
            .map(|(b, _)| b)
    }

    // ----------------------------------------------------------- frames --

    fn wants_frame(&self) -> bool {
        self.shown
            && (self.dirty
                || !self.w.settled()
                || !self.h.settled()
                || !self.r.settled()
                || !self.fade.settled()
                || !self.level.settled()
                || self.bars.iter().any(|b| !b.settled()))
    }

    /// Steps the springs, draws, and returns how long to wait for the next frame.
    fn frame(&mut self) -> Duration {
        let now = Instant::now();
        let mut dt = (now - self.last_frame).as_secs_f32();
        if dt > 0.1 {
            dt = 1.0 / 60.0;
        }
        self.last_frame = now;

        let mut shape = self.w.step(dt);
        shape |= self.h.step(dt);
        shape |= self.r.step(dt);
        self.fade.step(dt);
        self.level.step(dt);
        for b in &mut self.bars {
            b.step(dt);
        }
        self.dirty = false;
        let _ = self.render();

        // Full rate while the shape morphs; level bars are fine at 60.
        let fps = if shape { self.cfg.max_fps } else { 60 }.min(self.refresh_hz).max(15);
        Duration::from_secs_f64(1.0 / fps as f64).saturating_sub(now.elapsed())
    }

    fn sync_art(&mut self) {
        let want = self.media.as_ref().and_then(|m| m.art.clone());
        match (&want, &self.art) {
            (Some(a), Some((cur, _))) if Arc::ptr_eq(a, cur) => {}
            (Some(a), _) => self.art = self.gfx.bitmap(a.width, a.height, &a.pixels).ok().map(|b| (a.clone(), b)),
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

        self.gfx.begin(pw, ph, k)?;
        {
            let p = self.gfx.painter();
            p.notch(body_x, bw, bh, self.r.value, EAR, BLACK);
            let alpha = self.fade.value.clamp(0.0, 1.0);
            if alpha > 0.01 && bh > 4.0 {
                p.clip(body_x, 0.0, bw, bh);
                match self.view {
                    View::Idle => {}
                    View::Live => self.draw_live(&p, alpha),
                    View::Peek => {
                        let (_, th, _) = self.size_of(View::Peek);
                        self.draw_level_row(&p, self.origin + 16.0, th / 2.0, tw - 32.0, alpha);
                    }
                    View::Expanded => self.draw_expanded(&p, alpha),
                }
                p.unclip();
            }
        }
        self.gfx.present(self.hwnd, pos, pw, ph)
    }

    // ---------------------------------------------------------- drawing --

    fn draw_art(&self, p: &Painter, x: f32, y: f32, s: f32, alpha: f32) {
        match &self.art {
            Some((_, bmp)) => p.image(bmp, x, y, s, s, alpha),
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
        let s = (th - 10.0).max(10.0);
        self.draw_art(p, ox + 12.0, (th - s) / 2.0, s, alpha);
        let (bar, gap) = (3.0, 2.2);
        let bars_w = BANDS as f32 * bar + (BANDS - 1) as f32 * gap;
        self.draw_bars(p, ox + self.content_w - 12.0 - bars_w, th / 2.0, bar, gap, th - 12.0, alpha);
    }

    /// Icon, level bar and percentage for the volume/brightness peek.
    fn draw_level_row(&self, p: &Painter, x: f32, cy: f32, w: f32, alpha: f32) {
        let white = Color::white(0.95 * alpha);
        let (value, dim) = match self.peek {
            Some(PeekKind::Brightness) => {
                sun(p, x + 9.0, cy, 18.0, white);
                (self.brightness as f32 / 100.0, false)
            }
            _ => {
                speaker(p, x + 9.0, cy, 18.0, self.vol.0, self.vol.1, white);
                (self.vol.0, self.vol.1)
            }
        };
        let bar_x = x + 32.0;
        let bar_w = w - 32.0 - 40.0;
        p.rrect(bar_x, cy - 3.0, bar_w, 6.0, 3.0, Color::white(0.18 * alpha));
        let fill = bar_w * self.level.value.clamp(0.0, 1.0);
        if fill > 0.5 {
            let c = Color::white(if dim { 0.35 } else { 0.95 } * alpha);
            p.rrect(bar_x, cy - 3.0, fill.max(6.0), 6.0, 3.0, c);
        }
        let pct = format!("{}", (value * 100.0).round() as i32);
        p.text(&pct, &self.gfx.fonts.num, x + w - 36.0, cy - 10.0, 36.0, 20.0, Align::Right, white);
    }

    fn draw_expanded(&self, p: &Painter, alpha: f32) {
        let ox = self.origin;
        let tw = self.content_w;
        let fonts = &self.gfx.fonts;
        let Some(m) = &self.media else {
            let (time, date) = clock();
            p.text(&time, &fonts.big, ox + 22.0, 10.0, tw - 44.0, 44.0, Align::Left, Color::white(0.95 * alpha));
            if self.peek.is_some() {
                self.draw_level_row(p, ox + 22.0, 66.0, tw - 44.0, alpha);
            } else {
                p.text(&date, &fonts.body, ox + 22.0, 54.0, tw - 44.0, 22.0, Align::Left, Color::white(0.55 * alpha));
            }
            if let Some((pct, charging)) = battery() {
                let (bx, by) = (ox + tw - 22.0 - 28.0, 24.0);
                p.stroke_rrect(bx, by, 25.0, 13.0, 3.5, Color::white(0.5 * alpha), 1.2);
                p.rrect(bx + 26.0, by + 4.0, 2.0, 5.0, 1.0, Color::white(0.5 * alpha));
                let c = if charging {
                    Color(0.45, 0.9, 0.5, alpha)
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
            return;
        };

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

        if self.peek.is_some() {
            self.draw_level_row(p, ox + 18.0, 94.0, tw - 36.0, alpha);
        } else if m.duration > Duration::ZERO {
            let pos = m.position_now();
            let frac = (pos.as_secs_f32() / m.duration.as_secs_f32()).clamp(0.0, 1.0);
            let dim = Color::white(0.5 * alpha);
            p.text(&fmt_time(pos), &fonts.small, ox + 18.0, 86.0, 40.0, 16.0, Align::Left, dim);
            p.text(
                &format!("-{}", fmt_time(m.duration.saturating_sub(pos))),
                &fonts.small,
                ox + tw - 58.0,
                86.0,
                40.0,
                16.0,
                Align::Right,
                dim,
            );
            let (bx, bw) = (ox + 62.0, tw - 124.0);
            p.rrect(bx, 92.0, bw, 4.0, 2.0, Color::white(0.18 * alpha));
            if frac > 0.0 {
                p.rrect(bx, 92.0, (bw * frac).max(4.0), 4.0, 2.0, Color::white(0.9 * alpha));
            }
        }

        let cy = 124.0;
        let cx = ox + tw / 2.0;
        let white = Color::white(0.95 * alpha);
        for (b, x) in [(Button::Previous, cx - 60.0), (Button::PlayPause, cx), (Button::Next, cx + 60.0)] {
            if self.hovered == Some(b) {
                p.circle(x, cy, 19.0, Color::white(0.12 * alpha));
            }
            match b {
                Button::Previous => skip(p, x, cy, 16.0, -1.0, white),
                Button::Next => skip(p, x, cy, 16.0, 1.0, white),
                Button::PlayPause if m.playing => {
                    p.rrect(x - 7.0, cy - 9.0, 5.0, 18.0, 1.5, white);
                    p.rrect(x + 2.0, cy - 9.0, 5.0, 18.0, 1.5, white);
                }
                Button::PlayPause => p.polygon(&[(x - 6.0, cy - 10.0), (x - 6.0, cy + 10.0), (x + 10.0, cy)], white),
            }
        }
    }
}

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

fn speaker(p: &Painter, cx: f32, cy: f32, s: f32, level: f32, muted: bool, c: Color) {
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
