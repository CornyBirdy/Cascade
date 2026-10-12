//! The settings window ("Cascade Settings" in the Start menu). Drawn with
//! Direct2D like the rest of Cascade; only the text boxes are native EDIT
//! controls. Every change is written to config.ini and applied at once.

use windows::core::{w, Result, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;
use crate::gfx::{Align, Color, Gfx};

const CW: f32 = 900.0;
const CH: f32 = 620.0;
const SIDEBAR: f32 = 220.0;
const ROW_H: f32 = 58.0;
const ROW_GAP: f32 = 8.0;
const TOP: f32 = 84.0;
const ACCENT: Color = Color(0.27, 0.55, 1.0, 1.0);
const BG: Color = Color(0.11, 0.11, 0.12, 1.0);
const CHOICE_W: f32 = 84.0;

pub const SECTIONS: [&str; 8] =
    ["General", "Notch", "Taskbar", "Start menu", "Widgets", "Focus & weather", "Updates", "About"];

#[derive(Clone, Copy)]
enum Unit {
    Fps,
    Px,
    Ms,
    Min,
    Times,
    Percent,
    Count,
    Seconds,
}

enum Ctl {
    Toggle,
    Slider { min: f32, max: f32, step: f32, unit: Unit },
    Text { secret: bool },
    /// Segmented buttons: (config value, label).
    Choice(&'static [(&'static str, &'static str)]),
    Button(&'static str),
    Note,
}

struct Row {
    key: &'static str,
    title: &'static str,
    desc: &'static str,
    ctl: Ctl,
}

const fn row(key: &'static str, title: &'static str, desc: &'static str, ctl: Ctl) -> Row {
    Row { key, title, desc, ctl }
}

fn slider(min: f32, max: f32, step: f32, unit: Unit) -> Ctl {
    Ctl::Slider { min, max, step, unit }
}

fn rows(section: usize) -> Vec<Row> {
    use Ctl::*;
    match section {
        0 => vec![
            row("autostart", "Start with Windows", "Launch Cascade when you sign in.", Toggle),
            row("hide_in_fullscreen", "Hide in fullscreen", "Get out of the way of games and videos.", Toggle),
            row(
                "max_fps",
                "Animation frame cap",
                "Also limited to your monitor's refresh rate.",
                slider(30.0, 240.0, 10.0, Unit::Fps),
            ),
        ],
        1 => vec![
            row("notch", "Show the notch", "The island at the top of your main screen.", Toggle),
            row("live_activity", "Live activity", "Grow with album art or the focus timer.", Toggle),
            row("visualizer", "Music visualizer", "Bars that move with whatever is playing.", Toggle),
            row("volume_osd", "Volume pop-up", "Show volume changes in the notch.", Toggle),
            row("brightness_osd", "Brightness pop-up", "Show brightness changes in the notch.", Toggle),
            row(
                "hide_windows_flyout",
                "Hide the Windows flyout",
                "Hide the built-in volume / brightness flyout.",
                Toggle,
            ),
            row("scroll_volume", "Scroll to change volume", "Scroll over the closed notch.", Toggle),
            row("scale", "Size", "Scales the whole notch.", slider(0.6, 2.0, 0.05, Unit::Times)),
            row("idle_width", "Resting width", "Width of the closed notch.", slider(100.0, 320.0, 2.0, Unit::Px)),
            row(
                "expand_delay_ms",
                "Hover delay",
                "How long to hover before it opens.",
                slider(0.0, 800.0, 10.0, Unit::Ms),
            ),
        ],
        2 => vec![
            row("dock", "Show the taskbar", "Cascade's taskbar along the bottom.", Toggle),
            row("hide_taskbar", "Replace the Windows taskbar", "Hide the Windows one while Cascade's is on.", Toggle),
            row("dock_autohide", "Auto-hide", "Push the mouse to the bottom edge to show it.", Toggle),
            row(
                "taskbar_style",
                "Style",
                "One bar, or separate floating islands.",
                Choice(&[("full", "Full width"), ("floating", "Floating")]),
            ),
            row("taskbar_align", "Alignment", "Where Start and your apps sit.", Choice(&[("center", "Centre"), ("left", "Left")])),
            row("dock_icon_size", "Icon size", "Size of the app icons.", slider(16.0, 48.0, 1.0, Unit::Px)),
            row(
                "taskbar_tile",
                "Icon padding",
                "Space between an icon and the edge of its highlight.",
                slider(0.0, 16.0, 1.0, Unit::Px),
            ),
            row(
                "taskbar_pad",
                "Bar padding",
                "Gap from the bar's edge to its buttons, the same on all four sides.",
                slider(0.0, 24.0, 1.0, Unit::Px),
            ),
            row(
                "taskbar_gap",
                "Gap from the screen edge",
                "Set it equal to Bar padding for an even look. 0 = flush.",
                slider(0.0, 32.0, 1.0, Unit::Px),
            ),
            row(
                "taskbar_corners",
                "Corners",
                "Also used by the Start menu.",
                Choice(&[("round", "Round"), ("small", "Small"), ("square", "Square")]),
            ),
            row("acrylic", "Acrylic blur", "Blur what's behind the taskbar and Start menu.", Toggle),
            row("taskbar_opacity", "Tint", "How dark the glass is.", slider(0.0, 1.0, 0.05, Unit::Percent)),
            row("show_search", "Search box", "Next to the Start button.", Toggle),
            row("show_status", "Network, volume and battery", "Opens quick settings when clicked.", Toggle),
            row("show_clock", "Clock", "Opens notifications when clicked.", Toggle),
            row(
                "tray_icons",
                "App icons by the clock",
                "Tray icons. Which show is set in Windows' taskbar settings.",
                Toggle,
            ),
            row(
                "dock_magnify",
                "Magnification",
                "Grow icons under the cursor (0 for the Windows look).",
                slider(0.0, 1.0, 0.05, Unit::Percent),
            ),
        ],
        3 => vec![
            row("launcher", "Cascade Start menu", "Opened by the Start button on the taskbar.", Toggle),
            row("launcher_win_key", "Open with the Win key", "Win+E, Win+R and other shortcuts keep working.", Toggle),
            row(
                "admin_helper",
                "Work over admin apps",
                "So the Win key works over Task Manager. Asks for permission once.",
                Toggle,
            ),
            row(
                "start_position",
                "Position",
                "Where it opens. Auto follows the taskbar's alignment.",
                Choice(&[("auto", "Auto"), ("center", "Centre"), ("left", "Left")]),
            ),
            row("start_height", "Height", "How tall the menu is.", slider(500.0, 800.0, 10.0, Unit::Px)),
            row("start_columns", "Pinned columns", "Apps per row in Pinned.", slider(3.0, 6.0, 1.0, Unit::Count)),
            row("start_pinned_rows", "Pinned rows", "Rows of pinned apps.", slider(1.0, 4.0, 1.0, Unit::Count)),
            row("start_show_sidebar", "Sidebar", "Account, folders, settings and power.", Toggle),
            row("start_show_recent", "Recent files", "Under the pinned apps.", Toggle),
            row("start_show_all_apps", "All apps list", "Every app A to Z on the right.", Toggle),
            row("start_opacity", "Tint", "How dark the glass is.", slider(0.0, 1.0, 0.05, Unit::Percent)),
        ],
        4 => vec![
            row("widgets_button", "Widgets button", "A button at the right of the taskbar that opens the panel.", Toggle),
            row("chip_music", "Music", "What's playing, with a play / pause button.", Toggle),
            row("chip_cpu", "CPU", "Processor load on the taskbar.", Toggle),
            row("chip_ram", "Memory", "Memory in use on the taskbar.", Toggle),
            row("chip_gpu", "GPU", "Graphics load on the taskbar.", Toggle),
            row("chip_net", "Network", "Download and upload speed on the taskbar.", Toggle),
            row(
                "widgets_panel",
                "Panel cards",
                "In order: media, system, weather, calendar. Remove one to hide it.",
                Text { secret: false },
            ),
            row("widgets_columns", "Panel columns", "Cards side by side.", Choice(&[("1", "One"), ("2", "Two")])),
            row(
                "stats_seconds",
                "Stats refresh",
                "Seconds between readings (only while one is on screen).",
                slider(1.0, 10.0, 1.0, Unit::Seconds),
            ),
        ],
        5 => vec![
            row("focus_minutes", "Focus length", "Length of a focus session.", slider(5.0, 90.0, 5.0, Unit::Min)),
            row("break_minutes", "Break length", "Length of a break.", slider(1.0, 30.0, 1.0, Unit::Min)),
            row("weather", "Show weather", "On the taskbar, and in the notch when nothing plays.", Toggle),
            row(
                "weather_location",
                "Location",
                "City, or lat,lon. Empty = detect from your IP.",
                Text { secret: false },
            ),
            row("fahrenheit", "Use Fahrenheit", "Otherwise Celsius.", Toggle),
        ],
        6 => vec![
            row("update_now", "Update Cascade", "Download and install the newest build.", Button("Update now")),
            row("update_check", "Check automatically", "Look for a new build every few hours.", Toggle),
            row("github_token", "GitHub token", "Only needed while the repo is private.", Text { secret: true }),
        ],
        _ => vec![
            row("about", "Cascade", "A tiny native notch, dock and launcher for Windows.", Note),
            row("open_config", "Config file", "Every setting, as plain text.", Button("Open")),
            row("open_folder", "Data folder", "Config, pinned apps and logs.", Button("Open")),
        ],
    }
}

fn fmt(v: f32, unit: Unit) -> String {
    match unit {
        Unit::Fps => format!("{v:.0} fps"),
        Unit::Px => format!("{v:.0} px"),
        Unit::Ms => format!("{v:.0} ms"),
        Unit::Min => format!("{v:.0} min"),
        Unit::Times => format!("{v:.2}\u{d7}"),
        Unit::Percent => format!("{:.0}%", v * 100.0),
        Unit::Count => format!("{v:.0}"),
        Unit::Seconds => format!("{v:.0} s"),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Nav(usize),
    Row(usize),
    /// The "reset to default" button of a row.
    Reset(usize),
}

/// The default for `key`, if the row should offer a reset button.
fn default_of(key: &str) -> Option<String> {
    if matches!(key, "autostart" | "github_token" | "admin_helper") {
        return None;
    }
    let d = Config::default().get(key);
    (!d.is_empty() || key == "weather_location").then_some(d)
}

/// Whether `value` differs from the default (sliders compare as numbers).
fn changed(r: &Row, value: &str) -> Option<String> {
    let d = default_of(r.key)?;
    let differs = match r.ctl {
        Ctl::Slider { step, .. } => match (value.parse::<f32>(), d.parse::<f32>()) {
            (Ok(a), Ok(b)) => (a - b).abs() > step / 2.0,
            _ => value != d,
        },
        Ctl::Toggle | Ctl::Choice(_) | Ctl::Text { .. } => value != d,
        _ => false,
    };
    differs.then_some(d)
}

pub enum Change {
    Set(&'static str, String),
    Action(&'static str),
}

pub struct Settings {
    pub hwnd: HWND,
    k: f32,
    section: usize,
    scroll: f32,
    hovered: Option<Hit>,
    tracking: bool,
    drag: Option<(usize, f32)>,
    edits: Vec<(HWND, usize)>,
    font: HFONT,
    pub brush: HBRUSH,
}

fn edit_text(h: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(h, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

impl Settings {
    /// Opens the window (class `CascadeSettings`, registered by the app).
    pub fn create(instance: HINSTANCE) -> Result<Self> {
        unsafe {
            let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
            let hwnd = CreateWindowExW(
                WS_EX_APPWINDOW,
                w!("CascadeSettings"),
                w!("Cascade Settings"),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                100,
                100,
                None,
                None,
                Some(instance),
                None,
            )?;
            let dark = BOOL(1);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
            // Caption colour matching the content (Windows 11).
            let caption = COLORREF(0x001F1C1C);
            let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(35), &caption as *const _ as *const _, 4);
            let mut s = Self {
                hwnd,
                k: 1.0,
                section: 0,
                scroll: 0.0,
                hovered: None,
                tracking: false,
                drag: None,
                edits: Vec::new(),
                font: HFONT::default(),
                brush: CreateSolidBrush(COLORREF(0x002B2B2B)),
            };
            s.apply_dpi(None);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            Ok(s)
        }
    }

    /// Sizes the window for its DPI (centred, or at `suggested`).
    pub fn apply_dpi(&mut self, suggested: Option<RECT>) {
        unsafe {
            self.k = GetDpiForWindow(self.hwnd).max(96) as f32 / 96.0;
            let (cw, ch) = ((CW * self.k) as i32, (CH * self.k) as i32);
            let mut r = RECT { left: 0, top: 0, right: cw, bottom: ch };
            let _ = AdjustWindowRectEx(
                &mut r,
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                false,
                WS_EX_APPWINDOW,
            );
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let (x, y) = match suggested {
                Some(s) => (s.left, s.top),
                None => {
                    let mon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTOPRIMARY);
                    let mut mi =
                        MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                    let _ = GetMonitorInfoW(mon, &mut mi);
                    let wa = mi.rcWork;
                    (wa.left + (wa.right - wa.left - w) / 2, wa.top + (wa.bottom - wa.top - h) / 2)
                }
            };
            let _ = SetWindowPos(self.hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font.into());
            }
            self.font = CreateFontW(
                -(14.0 * self.k) as i32,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
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
        }
    }

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn content_w(&self) -> f32 {
        CW - SIDEBAR - 24.0 - 28.0
    }

    fn row_y(&self, i: usize) -> f32 {
        TOP + i as f32 * (ROW_H + ROW_GAP) - self.scroll
    }

    /// Control rect (x, y, w, h) of row `i`.
    fn ctl_rect(&self, i: usize, ctl: &Ctl) -> (f32, f32, f32, f32) {
        let x1 = SIDEBAR + 24.0 + self.content_w() - 18.0;
        let y = self.row_y(i);
        match ctl {
            Ctl::Toggle => (x1 - 44.0, y + (ROW_H - 22.0) / 2.0, 44.0, 22.0),
            Ctl::Slider { .. } => (x1 - 170.0, y + 14.0, 170.0, ROW_H - 28.0),
            Ctl::Text { .. } => (x1 - 250.0, y + (ROW_H - 30.0) / 2.0, 250.0, 30.0),
            Ctl::Choice(opts) => {
                let w = opts.len() as f32 * CHOICE_W;
                (x1 - w, y + (ROW_H - 32.0) / 2.0, w, 32.0)
            }
            Ctl::Button(_) => (x1 - 130.0, y + (ROW_H - 32.0) / 2.0, 130.0, 32.0),
            Ctl::Note => (x1, y, 0.0, 0.0),
        }
    }

    /// The reset button of row `i` (x, y, size).
    fn reset_rect(&self, i: usize, ctl: &Ctl) -> (f32, f32, f32) {
        let (cx, _, _, _) = self.ctl_rect(i, ctl);
        let x = match ctl {
            Ctl::Slider { .. } => cx - 112.0,
            _ => cx - 36.0,
        };
        (x, self.row_y(i) + (ROW_H - 26.0) / 2.0, 26.0)
    }

    /// Creates the text boxes for the current section.
    pub fn sync_edits(&mut self, get: &dyn Fn(&str) -> String) {
        unsafe {
            for (h, _) in self.edits.drain(..) {
                let _ = DestroyWindow(h);
            }
            let instance = GetWindowLongPtrW(self.hwnd, GWLP_HINSTANCE);
            for (i, r) in rows(self.section).iter().enumerate() {
                let Ctl::Text { secret } = r.ctl else { continue };
                let (x, y, w, h) = self.ctl_rect(i, &r.ctl);
                let k = self.k;
                let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(0x80); // ES_AUTOHSCROLL
                if secret {
                    style |= WINDOW_STYLE(0x20); // ES_PASSWORD
                }
                let pad = (6.0 * k) as i32;
                let Ok(e) = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("EDIT"),
                    &windows::core::HSTRING::from(get(r.key)),
                    style,
                    (x * k) as i32 + pad,
                    (y * k) as i32 + pad,
                    (w * k) as i32 - pad * 2,
                    (h * k) as i32 - pad * 2,
                    Some(self.hwnd),
                    Some(HMENU((1000 + i) as *mut _)),
                    Some(HINSTANCE(instance as *mut _)),
                    None,
                ) else {
                    continue;
                };
                SendMessageW(e, WM_SETFONT, Some(WPARAM(self.font.0 as usize)), Some(LPARAM(1)));
                self.edits.push((e, i));
            }
        }
    }

    /// A text box lost focus: returns its new value.
    pub fn commit_edit(&self, edit: HWND) -> Option<Change> {
        let (_, i) = self.edits.iter().find(|(h, _)| *h == edit)?;
        let r = rows(self.section).into_iter().nth(*i)?;
        Some(Change::Set(r.key, edit_text(edit).trim().to_string()))
    }

    /// All text boxes' values (when the window closes).
    pub fn commit_all(&self) -> Vec<Change> {
        self.edits.iter().filter_map(|(h, _)| self.commit_edit(*h)).collect()
    }

    fn hit(&self, x: i32, y: i32, get: &dyn Fn(&str) -> String) -> Option<Hit> {
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        if lx < SIDEBAR {
            let i = ((ly - 80.0) / 40.0).floor();
            return (ly >= 80.0 && (i as usize) < SECTIONS.len()).then_some(Hit::Nav(i as usize));
        }
        for (i, r) in rows(self.section).iter().enumerate() {
            let (rx, ry, rs) = self.reset_rect(i, &r.ctl);
            if lx >= rx && lx < rx + rs && ly >= ry && ly < ry + rs && changed(r, &get(r.key)).is_some() {
                return Some(Hit::Reset(i));
            }
        }
        for (i, r) in rows(self.section).iter().enumerate() {
            let (cx, cy, cw, ch) = self.ctl_rect(i, &r.ctl);
            let (cx, cw) = if matches!(r.ctl, Ctl::Slider { .. }) { (cx - 8.0, cw + 16.0) } else { (cx, cw) };
            if lx >= cx && lx < cx + cw && ly >= cy && ly < cy + ch && ly >= TOP - 8.0 {
                return Some(Hit::Row(i));
            }
        }
        None
    }

    fn slider_value(&self, i: usize, x: i32) -> Option<f32> {
        let rs = rows(self.section);
        let Ctl::Slider { min, max, step, .. } = rs.get(i)?.ctl else { return None };
        let (cx, _, cw, _) = self.ctl_rect(i, &rs[i].ctl);
        let t = ((x as f32 / self.k - cx) / cw).clamp(0.0, 1.0);
        let v = min + (max - min) * t;
        Some(((v - min) / step).round() * step + min)
    }

    pub fn on_move(&mut self, x: i32, y: i32, get: &dyn Fn(&str) -> String) {
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
        if let Some((i, _)) = self.drag {
            if let Some(v) = self.slider_value(i, x) {
                self.drag = Some((i, v));
                self.invalidate();
            }
            return;
        }
        let h = self.hit(x, y, get);
        if h != self.hovered {
            self.hovered = h;
            self.invalidate();
        }
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        self.hovered = None;
        self.invalidate();
    }

    pub fn on_down(&mut self, x: i32, y: i32, get: &dyn Fn(&str) -> String) -> Option<Change> {
        let hit = self.hit(x, y, get)?;
        match hit {
            Hit::Reset(i) => {
                let r = rows(self.section).into_iter().nth(i)?;
                let d = changed(&r, &get(r.key))?;
                self.hovered = None;
                if matches!(r.ctl, Ctl::Text { .. }) {
                    // Show the default in the text box straight away.
                    if let Some((h, _)) = self.edits.iter().find(|(_, j)| *j == i) {
                        unsafe {
                            let _ = SetWindowTextW(*h, &windows::core::HSTRING::from(d.as_str()));
                        }
                    }
                }
                self.invalidate();
                Some(Change::Set(r.key, d))
            }
            Hit::Nav(i) => {
                if i != self.section {
                    self.section = i;
                    self.scroll = 0.0;
                    self.hovered = None;
                    self.sync_edits(get);
                    self.invalidate();
                }
                None
            }
            Hit::Row(i) => {
                let r = rows(self.section).into_iter().nth(i)?;
                match r.ctl {
                    Ctl::Toggle => {
                        let on = get(r.key) == "true";
                        self.invalidate();
                        Some(Change::Set(r.key, (!on).to_string()))
                    }
                    Ctl::Slider { .. } => {
                        self.drag = Some((i, self.slider_value(i, x)?));
                        unsafe {
                            SetCapture(self.hwnd);
                        }
                        self.invalidate();
                        None
                    }
                    Ctl::Choice(opts) => {
                        let (cx, ..) = self.ctl_rect(i, &r.ctl);
                        let j = ((x as f32 / self.k - cx) / CHOICE_W).floor().clamp(0.0, opts.len() as f32 - 1.0);
                        let value = opts[j as usize].0;
                        (get(r.key) != value).then(|| Change::Set(r.key, value.to_string()))
                    }
                    Ctl::Button(_) => Some(Change::Action(r.key)),
                    _ => None,
                }
            }
        }
    }

    pub fn on_up(&mut self) -> Option<Change> {
        let (i, v) = self.drag.take()?;
        unsafe {
            let _ = ReleaseCapture();
        }
        self.invalidate();
        let r = rows(self.section).into_iter().nth(i)?;
        Some(Change::Set(r.key, format!("{v}")))
    }

    pub fn on_wheel(&mut self, delta: i16, get: &dyn Fn(&str) -> String) {
        let n = rows(self.section).len() as f32;
        let content = TOP + n * (ROW_H + ROW_GAP) + 16.0;
        let max = (content - CH).max(0.0);
        let before = self.scroll;
        self.scroll = (self.scroll - delta as f32 / 120.0 * 60.0).clamp(0.0, max);
        if self.scroll != before {
            self.sync_edits(get);
            self.invalidate();
        }
    }

    pub fn paint(&self, gfx: &mut Gfx, get: &dyn Fn(&str) -> String, build: u32) -> Result<()> {
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        let (pw, ph) = ((CW * self.k) as i32, (CH * self.k) as i32);
        let r = (|| {
            gfx.begin_dc(hdc, pw, ph, self.k, BG)?;
            {
                let p = gfx.painter();
                let f = &gfx.fonts;
                // Sidebar.
                p.rrect(0.0, 0.0, SIDEBAR, CH, 0.0, Color(0.09, 0.09, 0.10, 1.0));
                p.rrect(24.0, 26.0, 26.0, 26.0, 7.0, Color(0.16, 0.19, 0.30, 1.0));
                p.rrect(31.0, 26.0, 12.0, 7.0, 3.0, Color::white(0.95));
                p.rrect(29.0, 44.0, 16.0, 3.0, 1.5, ACCENT);
                p.text("Cascade", &f.title, 60.0, 24.0, 150.0, 30.0, Align::Left, Color::white(0.95));
                for (i, name) in SECTIONS.iter().enumerate() {
                    let y = 80.0 + i as f32 * 40.0;
                    let active = i == self.section;
                    if active || self.hovered == Some(Hit::Nav(i)) {
                        p.rrect(
                            12.0,
                            y + 2.0,
                            SIDEBAR - 24.0,
                            36.0,
                            8.0,
                            Color::white(if active { 0.09 } else { 0.05 }),
                        );
                    }
                    if active {
                        p.rrect(12.0, y + 12.0, 3.0, 16.0, 1.5, ACCENT);
                    }
                    p.text(
                        name,
                        &f.body,
                        28.0,
                        y + 2.0,
                        SIDEBAR - 40.0,
                        36.0,
                        Align::Left,
                        Color::white(if active { 0.95 } else { 0.7 }),
                    );
                }
                if build > 0 {
                    p.text(
                        &format!("Build {build}"),
                        &f.small,
                        24.0,
                        CH - 40.0,
                        160.0,
                        20.0,
                        Align::Left,
                        Color::white(0.35),
                    );
                }

                // Content.
                let x0 = SIDEBAR + 24.0;
                let cw = self.content_w();
                p.clip(SIDEBAR, 0.0, CW - SIDEBAR, CH);
                p.text(
                    SECTIONS[self.section],
                    &f.big,
                    x0,
                    22.0 - self.scroll,
                    cw,
                    44.0,
                    Align::Left,
                    Color::white(0.95),
                );
                for (i, r) in rows(self.section).iter().enumerate() {
                    let y = self.row_y(i);
                    if y + ROW_H < 0.0 || y > CH {
                        continue;
                    }
                    p.rrect(x0, y, cw, ROW_H, 8.0, Color::white(0.045));
                    let text_w = cw - 330.0;
                    let desc = if r.key == "about" && build > 0 {
                        format!("{} Build {build}.", r.desc)
                    } else if r.key == "admin_helper" && get(r.key) == "true" {
                        // Live status, so it's clear whether it's working.
                        if crate::helper::running() {
                            "Running.".to_string()
                        } else {
                            "Not running. Details in cascade.log in the data folder.".to_string()
                        }
                    } else {
                        r.desc.to_string()
                    };
                    p.text(r.title, &f.label, x0 + 18.0, y + 9.0, text_w, 20.0, Align::Left, Color::white(0.95));
                    p.text(&desc, &f.small, x0 + 18.0, y + 30.0, text_w, 18.0, Align::Left, Color::white(0.55));
                    let (cx, cy, cwid, ch) = self.ctl_rect(i, &r.ctl);
                    let hovered = self.hovered == Some(Hit::Row(i));
                    match r.ctl {
                        Ctl::Toggle => {
                            let on = get(r.key) == "true";
                            if on {
                                p.rrect(
                                    cx,
                                    cy,
                                    cwid,
                                    ch,
                                    ch / 2.0,
                                    if hovered { Color(0.36, 0.62, 1.0, 1.0) } else { ACCENT },
                                );
                                p.circle(cx + cwid - ch / 2.0, cy + ch / 2.0, ch / 2.0 - 4.0, Color::white(1.0));
                            } else {
                                p.stroke_rrect(
                                    cx + 0.5,
                                    cy + 0.5,
                                    cwid - 1.0,
                                    ch - 1.0,
                                    ch / 2.0,
                                    Color::white(if hovered { 0.75 } else { 0.55 }),
                                    1.2,
                                );
                                p.circle(
                                    cx + ch / 2.0,
                                    cy + ch / 2.0,
                                    ch / 2.0 - 5.5,
                                    Color::white(if hovered { 0.85 } else { 0.7 }),
                                );
                            }
                        }
                        Ctl::Slider { min, max, unit, .. } => {
                            let v = match self.drag {
                                Some((d, v)) if d == i => v,
                                _ => get(r.key).parse::<f32>().unwrap_or(min),
                            };
                            let t = ((v - min) / (max - min)).clamp(0.0, 1.0);
                            let ty = cy + ch / 2.0;
                            p.rrect(cx, ty - 2.0, cwid, 4.0, 2.0, Color::white(0.18));
                            p.rrect(cx, ty - 2.0, (cwid * t).max(4.0), 4.0, 2.0, ACCENT);
                            let knob = if hovered || self.drag.is_some_and(|(d, _)| d == i) { 9.0 } else { 7.5 };
                            p.circle(cx + cwid * t, ty, knob, Color::white(1.0));
                            p.text(&fmt(v, unit), &f.num, cx - 84.0, y, 70.0, ROW_H, Align::Right, Color::white(0.8));
                        }
                        Ctl::Text { .. } => {
                            p.rrect(cx, cy, cwid, ch, 6.0, Color(0.17, 0.17, 0.17, 1.0));
                            p.stroke_rrect(cx + 0.5, cy + 0.5, cwid - 1.0, ch - 1.0, 6.0, Color::white(0.12), 1.0);
                        }
                        Ctl::Choice(opts) => {
                            let current = get(r.key);
                            p.rrect(cx, cy, cwid, ch, 6.0, Color::white(0.06));
                            p.stroke_rrect(cx + 0.5, cy + 0.5, cwid - 1.0, ch - 1.0, 6.0, Color::white(0.10), 1.0);
                            for (j, (value, label)) in opts.iter().enumerate() {
                                let sx = cx + j as f32 * CHOICE_W;
                                let on = current == *value;
                                if on {
                                    p.rrect(sx + 3.0, cy + 3.0, CHOICE_W - 6.0, ch - 6.0, 4.0, ACCENT);
                                }
                                let c = Color::white(if on { 1.0 } else { 0.75 });
                                p.text(label, &f.body, sx, cy, CHOICE_W, ch, Align::Center, c);
                            }
                        }
                        Ctl::Button(label) => {
                            p.rrect(cx, cy, cwid, ch, 6.0, if hovered { Color(0.36, 0.62, 1.0, 1.0) } else { ACCENT });
                            p.text(label, &f.label, cx, cy, cwid, ch, Align::Center, Color::white(1.0));
                        }
                        Ctl::Note => {}
                    }
                    let value = match self.drag {
                        Some((d, v)) if d == i => format!("{v}"),
                        _ => get(r.key),
                    };
                    if changed(r, &value).is_some() {
                        let (rx, ry, rs) = self.reset_rect(i, &r.ctl);
                        let (cx, cy) = (rx + rs / 2.0, ry + rs / 2.0);
                        if self.hovered == Some(Hit::Reset(i)) {
                            p.circle(cx, cy, rs / 2.0, Color::white(0.10));
                        }
                        // A circular "undo" arrow.
                        let c = Color::white(0.75);
                        p.arc(cx, cy, 6.0, -0.6, 4.1, c, 1.5);
                        let (ax, ay) = (cx + 6.0 * (-0.6f32).cos(), cy + 6.0 * (-0.6f32).sin());
                        p.polygon(&[(ax - 3.5, ay - 2.0), (ax + 2.5, ay - 3.5), (ax + 1.0, ay + 2.5)], c);
                    }
                }
                p.unclip();
            }
            gfx.end()
        })();
        unsafe {
            let _ = EndPaint(self.hwnd, &ps);
        }
        r
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font.into());
            }
            let _ = DeleteObject(self.brush.into());
        }
    }
}
