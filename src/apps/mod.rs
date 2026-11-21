//! Cascade's own versions of Windows' everyday apps, for shell mode (where
//! Store apps don't run) and for anyone who'd rather: Calculator, Camera,
//! Notepad, Clock, Media Player, Sound Recorder and Paint. Each runs as
//! `cascade.exe --<app>`, a process of its own, in Cascade's look.
//!
//! They share the frame here: a window (see `appwin`), Direct2D drawing,
//! buttons that light under the pointer and press in, and the plumbing of
//! keys, timers and the mouse. An app only says what it draws and what its
//! buttons do (`App`).

pub mod calc;
pub mod camera;
pub mod clock;
pub mod notepad;
pub mod paint;
pub mod player;
pub mod soundrec;

use std::cell::RefCell;
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::appwin;
use crate::gfx::{Align, Color, Fonts, Gfx, Painter};

pub const BG: Color = Color(0.125, 0.125, 0.13, 1.0);
pub const PANEL: Color = Color(0.16, 0.16, 0.165, 1.0);

/// What an app knows about its window.
pub struct Ctx {
    pub hwnd: HWND,
    pub k: f32,
}

impl Ctx {
    pub fn redraw(&self) {
        appwin::redraw(self.hwnd);
    }

    /// The client area in logical units.
    pub fn size(&self) -> (f32, f32) {
        appwin::client(self.hwnd)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mouse {
    Down,
    Move,
    Up,
    RightDown,
}

/// An app: what it draws and what it does.
#[allow(unused_variables)]
pub trait App {
    fn draw(&mut self, p: &Painter, f: &Fonts, w: f32, h: f32, kit: &mut Kit);
    /// A button (by the id it was drawn with) was clicked.
    fn press(&mut self, id: u32, ctx: &Ctx) {}
    /// A key went down; true if it was used.
    fn key(&mut self, vk: u16, ctx: &Ctx) -> bool {
        false
    }
    fn char(&mut self, c: char, ctx: &Ctx) -> bool {
        false
    }
    /// How often to call `tick` (ms; 0 for never).
    fn tick_ms(&self) -> u32 {
        0
    }
    /// True to redraw.
    fn tick(&mut self, ctx: &Ctx) -> bool {
        false
    }
    /// The mouse outside any button (a canvas); true to redraw.
    fn mouse(&mut self, ev: Mouse, x: f32, y: f32, ctx: &Ctx) -> bool {
        false
    }
    fn wheel(&mut self, delta: i32, ctx: &Ctx) -> bool {
        false
    }
    /// A child control's notification (WM_COMMAND).
    fn command(&mut self, id: u16, code: u16, ctx: &Ctx) {}
    fn size(&mut self, ctx: &Ctx) {}
    /// Messages of its own (WM_APP and up); true if handled.
    fn message(&mut self, m: u32, wp: WPARAM, lp: LPARAM, ctx: &Ctx) -> bool {
        false
    }
    /// The window is closing: false keeps it open.
    fn closing(&mut self, ctx: &Ctx) -> bool {
        true
    }
    /// A text box's colours (WM_CTLCOLOREDIT), if it has one.
    fn edit_brush(&self) -> Option<windows::Win32::Graphics::Gdi::HBRUSH> {
        None
    }
}

/// Buttons drawn this frame and where, which one is under the pointer and
/// which is held down.
#[derive(Default)]
pub struct Kit {
    hits: Vec<(f32, f32, f32, f32, u32)>,
    pub hover: Option<u32>,
    pub pressed: Option<u32>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Look {
    /// A quiet button that lights under the pointer.
    Plain,
    /// The accent colour (the main thing to do).
    Accent,
    /// A filled grey key (Calculator's).
    Key,
    /// Red (to stop or delete).
    Danger,
}

impl Kit {
    pub fn hit(&mut self, x: f32, y: f32, w: f32, h: f32, id: u32) {
        self.hits.push((x, y, w, h, id));
    }

    pub fn at(&self, x: f32, y: f32) -> Option<u32> {
        self.hits.iter().rev().find(|h| x >= h.0 && x < h.0 + h.2 && y >= h.1 && y < h.1 + h.3).map(|h| h.4)
    }

    pub fn lit(&self, id: u32) -> (bool, bool) {
        (self.hover == Some(id), self.pressed == Some(id) && self.hover == Some(id))
    }

    /// A button: its plate, then `label` (and an icon glyph, if any) on it.
    #[allow(clippy::too_many_arguments)]
    pub fn button(
        &mut self,
        p: &Painter,
        f: &Fonts,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        label: &str,
        icon: Option<u32>,
        look: Look,
        id: u32,
    ) {
        let (hot, down) = self.lit(id);
        let accent = crate::theme::accent();
        let r = (h / 2.0).min(8.0);
        let fill = match look {
            Look::Plain => Color::white(if down {
                0.12
            } else if hot {
                0.08
            } else {
                0.0
            }),
            Look::Key => Color::white(if down {
                0.08
            } else if hot {
                0.14
            } else {
                0.1
            }),
            Look::Accent => crate::theme::lighten(
                accent,
                if down {
                    -0.0
                } else if hot {
                    0.12
                } else {
                    0.0
                },
            )
            .alpha(if down { 0.8 } else { 0.95 }),
            Look::Danger => Color(
                0.82,
                0.2,
                0.17,
                if down {
                    0.8
                } else if hot {
                    1.0
                } else {
                    0.9
                },
            ),
        };
        let inset = if down { 1.0 } else { 0.0 };
        p.rrect(x + inset, y + inset, w - 2.0 * inset, h - 2.0 * inset, r, fill);
        if matches!(look, Look::Key | Look::Plain) && hot {
            p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, r, Color::white(0.08), 1.0);
        }
        let ink = Color::white(0.95);
        let tw = if label.is_empty() { 0.0 } else { p.measure(label, &f.body) };
        let iw = if icon.is_some() { 16.0 } else { 0.0 };
        let gap = if icon.is_some() && !label.is_empty() { 8.0 } else { 0.0 };
        let mut lx = x + (w - tw - iw - gap) / 2.0;
        if let (Some(g), Some(font)) = (icon, &f.icons) {
            if let Some(c) = char::from_u32(g) {
                p.text(&c.to_string(), font, lx, y, 16.0, h, Align::Center, ink);
            }
            lx += iw + gap;
        }
        if !label.is_empty() {
            p.text(label, &f.body, lx, y, tw + 4.0, h, Align::Left, ink);
        }
        self.hit(x, y, w, h, id);
    }

    /// A round icon-only button.
    #[allow(clippy::too_many_arguments)]
    pub fn round(&mut self, p: &Painter, f: &Fonts, cx: f32, cy: f32, r: f32, icon: u32, look: Look, id: u32) {
        let (hot, down) = self.lit(id);
        let fill = match look {
            Look::Accent => crate::theme::accent().alpha(if down { 0.8 } else { 0.95 }),
            Look::Danger => Color(0.82, 0.2, 0.17, if hot { 1.0 } else { 0.9 }),
            _ => Color::white(if down {
                0.14
            } else if hot {
                0.12
            } else {
                0.07
            }),
        };
        p.circle(cx, cy, r - if down { 1.0 } else { 0.0 }, fill);
        if let (Some(font), Some(c)) = (&f.icons, char::from_u32(icon)) {
            let s = (r * 0.9).round();
            p.text(&c.to_string(), font, cx - s / 2.0, cy - s / 2.0, s, s, Align::Center, Color::white(0.95));
        }
        self.hit(cx - r, cy - r, 2.0 * r, 2.0 * r, id);
    }
}

struct Host {
    app: Box<dyn App>,
    kit: Kit,
    gfx: Option<Gfx>,
    timer: u32,
    last_click: Option<(u32, Instant)>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

fn ctx(hwnd: HWND) -> Ctx {
    Ctx { hwnd, k: appwin::scale(hwnd) }
}

fn with<R>(f: impl FnOnce(&mut Host) -> R) -> Option<R> {
    HOST.with(|h| h.try_borrow_mut().ok()?.as_mut().map(f))
}

fn paint(hwnd: HWND) {
    with(|host| {
        let Some(mut gfx) = host.gfx.take() else { return };
        host.kit.hits.clear();
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut r = RECT::default();
            let _ = GetClientRect(hwnd, &mut r);
            let k = appwin::scale(hwnd);
            let (pw, ph) = (r.right - r.left, r.bottom - r.top);
            if pw > 0 && ph > 0 && gfx.begin_dc(dc, pw, ph, k, BG).is_ok() {
                {
                    let p = gfx.painter();
                    host.app.draw(&p, &gfx.fonts, pw as f32 / k, ph as f32 / k, &mut host.kit);
                }
                let _ = gfx.end();
            }
            let _ = EndPaint(hwnd, &ps);
        }
        host.gfx = Some(gfx);
        // The app's own pace for its timer.
        let want = host.app.tick_ms();
        if want != host.timer {
            unsafe {
                if want == 0 {
                    let _ = KillTimer(Some(hwnd), 1);
                } else {
                    SetTimer(Some(hwnd), 1, want, None);
                }
            }
            host.timer = want;
        }
    });
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let c = ctx(hwnd);
    match m {
        WM_PAINT => {
            paint(hwnd);
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE => {
            with(|h| h.app.size(&c));
            appwin::redraw(hwnd);
        }
        WM_TIMER if wp.0 == 1 => {
            if with(|h| h.app.tick(&c)) == Some(true) {
                appwin::redraw(hwnd);
            }
        }
        WM_MOUSEMOVE => {
            let (x, y) = appwin::mouse(hwnd, lp);
            let redraw = with(|h| {
                let now = h.kit.at(x, y);
                let changed = now != h.kit.hover;
                h.kit.hover = now;
                let used = h.app.mouse(Mouse::Move, x, y, &c);
                changed || used
            });
            if redraw == Some(true) {
                appwin::redraw(hwnd);
            }
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut t);
        }
        0x02A3 => {
            // WM_MOUSELEAVE
            if with(|h| h.kit.hover.take().is_some()) == Some(true) {
                appwin::redraw(hwnd);
            }
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let (x, y) = appwin::mouse(hwnd, lp);
            let _ = SetFocus(Some(hwnd));
            SetCapture(hwnd);
            let redraw = with(|h| match h.kit.at(x, y) {
                Some(id) => {
                    h.kit.pressed = Some(id);
                    true
                }
                None => h.app.mouse(Mouse::Down, x, y, &c),
            });
            if redraw == Some(true) {
                appwin::redraw(hwnd);
            }
        }
        WM_LBUTTONUP => {
            let (x, y) = appwin::mouse(hwnd, lp);
            let _ = ReleaseCapture();
            let press = with(|h| {
                let pressed = h.kit.pressed.take();
                match pressed {
                    Some(id) if h.kit.at(x, y) == Some(id) => Some(id),
                    Some(_) => None,
                    None => {
                        h.app.mouse(Mouse::Up, x, y, &c);
                        None
                    }
                }
            })
            .flatten();
            if let Some(id) = press {
                with(|h| {
                    h.last_click = Some((id, Instant::now()));
                    h.app.press(id, &c);
                });
            }
            appwin::redraw(hwnd);
        }
        WM_RBUTTONDOWN => {
            let (x, y) = appwin::mouse(hwnd, lp);
            if with(|h| h.app.mouse(Mouse::RightDown, x, y, &c)) == Some(true) {
                appwin::redraw(hwnd);
            }
        }
        WM_MOUSEWHEEL => {
            let d = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
            if with(|h| h.app.wheel(d, &c)) == Some(true) {
                appwin::redraw(hwnd);
            }
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            if with(|h| h.app.key(wp.0 as u16, &c)) == Some(true) {
                appwin::redraw(hwnd);
                return LRESULT(0);
            }
        }
        WM_CHAR => {
            if let Some(ch) = char::from_u32(wp.0 as u32) {
                if with(|h| h.app.char(ch, &c)) == Some(true) {
                    appwin::redraw(hwnd);
                    return LRESULT(0);
                }
            }
        }
        WM_COMMAND => {
            let (id, code) = ((wp.0 & 0xFFFF) as u16, ((wp.0 >> 16) & 0xFFFF) as u16);
            with(|h| h.app.command(id, code, &c));
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
            if let Some(Some(b)) = with(|h| h.app.edit_brush()) {
                let dc = windows::Win32::Graphics::Gdi::HDC(wp.0 as *mut _);
                windows::Win32::Graphics::Gdi::SetTextColor(dc, COLORREF(0x00F0F0F0));
                windows::Win32::Graphics::Gdi::SetBkColor(dc, COLORREF(0x00222020));
                return LRESULT(b.0 as isize);
            }
        }
        WM_CLOSE => {
            if with(|h| h.app.closing(&c)) == Some(false) {
                return LRESULT(0);
            }
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        m if m >= WM_APP && with(|h| h.app.message(m, wp, lp, &c)) == Some(true) => {
            appwin::redraw(hwnd);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, m, wp, lp)
}

/// Runs an app in a window of its own until it's closed: `make` builds it
/// once the window exists.
pub fn run(class: PCWSTR, title: &str, aumid: &str, w: i32, h: i32, make: impl FnOnce(&Ctx) -> Box<dyn App>) {
    appwin::init();
    let Ok(gfx) = Gfx::new() else { return };
    let Some(hwnd) = appwin::create(class, title, w, h, Some(proc)) else { return };
    appwin::identity(hwnd, aumid);
    let app = make(&ctx(hwnd));
    HOST.with(|h| {
        *h.borrow_mut() = Some(Host { app, kit: Kit::default(), gfx: Some(gfx), timer: 0, last_click: None })
    });
    appwin::redraw(hwnd);
    appwin::run();
    HOST.with(|h| h.borrow_mut().take());
}

/// A file picker for opening, run on its own thread so the window keeps drawing.
pub fn pick_file(title: &str, filters: &[(&str, &str)]) -> Option<std::path::PathBuf> {
    let title = title.to_string();
    let filters: Vec<(String, String)> = filters.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    std::thread::spawn(move || {
        let f: Vec<(&str, &str)> = filters.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        crate::shell::choose_file(&title, &f)
    })
    .join()
    .ok()
    .flatten()
}

/// Where to save, asked with Windows' Save dialog (`ext` without the dot).
pub fn save_path(owner: HWND, suggested: &str, filters: &[(&str, &str)], ext: &str) -> Option<std::path::PathBuf> {
    use windows::core::HSTRING;
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileSaveDialog, IFileSaveDialog, SIGDN_FILESYSPATH};
    unsafe {
        let dlg: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let names: Vec<(HSTRING, HSTRING)> =
            filters.iter().map(|(a, b)| (HSTRING::from(*a), HSTRING::from(*b))).collect();
        let specs: Vec<COMDLG_FILTERSPEC> = names
            .iter()
            .map(|(a, b)| COMDLG_FILTERSPEC { pszName: PCWSTR(a.as_ptr()), pszSpec: PCWSTR(b.as_ptr()) })
            .collect();
        let _ = dlg.SetFileTypes(&specs);
        let _ = dlg.SetFileName(&HSTRING::from(suggested));
        let _ = dlg.SetDefaultExtension(&HSTRING::from(ext));
        dlg.Show(Some(owner)).ok()?;
        let item = dlg.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(std::path::PathBuf::from)
    }
}

/// A known folder's path (Pictures, Music...).
pub fn known_folder(id: &windows::core::GUID) -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(std::path::PathBuf::from)
    }
}

/// A file name for now: "Prefix 2026-11-21 153042".
pub fn stamped(prefix: &str, ext: &str) -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{prefix} {:04}-{:02}-{:02} {:02}{:02}{:02}.{ext}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}
