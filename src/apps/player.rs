//! Media Player (`cascade.exe --player [file]`), as Windows' own: video and
//! music (whatever Windows can play: MP4, MKV, MOV, AVI, MP3, M4A, FLAC,
//! WAV...), with play and pause, a seek bar, the volume, the folder's other
//! files as a playlist (previous, next, and on to the next when one ends),
//! repeat, and fullscreen (double-click the picture, or F11). Space plays
//! and pauses, the arrows seek and set the volume.

use std::path::{Path, PathBuf};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::{App, Ctx, Kit, Look};
use crate::gfx::{Align, Color, Fonts, Painter};

const BAR: f32 = 96.0;
/// From the picture: repaint it, toggle play, toggle fullscreen.
const VIDEO_PAINT: u32 = WM_APP + 5;
const VIDEO_CLICK: u32 = WM_APP + 6;
const VIDEO_DOUBLE: u32 = WM_APP + 7;

const EXTS: [&str; 16] =
    ["mp4", "mkv", "mov", "avi", "wmv", "webm", "m4v", "mp3", "m4a", "flac", "wav", "wma", "aac", "ogg", "opus", "3gp"];

fn is_media(p: &Path) -> bool {
    p.extension().is_some_and(|e| EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

struct Player {
    hwnd: HWND,
    video: HWND,
    mf: Option<IMFPMediaPlayer>,
    list: Vec<PathBuf>,
    at: usize,
    volume: f32,
    repeat: bool,
    full: bool,
    placement: WINDOWPLACEMENT,
    pos: f64,
    len: f64,
    playing: bool,
    seeking: bool,
    error: Option<String>,
}

unsafe extern "system" fn video_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let parent = GetParent(hwnd).unwrap_or_default();
    match m {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            FillRect(dc, &ps.rcPaint, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            let _ = EndPaint(hwnd, &ps);
            let _ = PostMessageW(Some(parent), VIDEO_PAINT, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONUP => {
            let _ = PostMessageW(Some(parent), VIDEO_CLICK, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let _ = PostMessageW(Some(parent), VIDEO_DOUBLE, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

fn hms(s: f64) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

impl Player {
    fn place(&self) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
            let k = crate::appwin::scale(self.hwnd);
            let bar = if self.full { 0 } else { (BAR * k) as i32 };
            let _ = MoveWindow(self.video, 0, 0, r.right, (r.bottom - bar).max(1), true);
            if let Some(mf) = &self.mf {
                let _ = mf.UpdateVideo();
            }
        }
    }

    fn load(&mut self, i: usize) {
        let Some(path) = self.list.get(i).cloned() else { return };
        self.at = i;
        self.error = None;
        unsafe {
            if let Some(old) = self.mf.take() {
                let _ = old.Shutdown();
            }
            let mut mf: Option<IMFPMediaPlayer> = None;
            if MFPCreateMediaPlayer(
                PCWSTR::null(),
                false,
                MFP_CREATION_OPTIONS(0),
                None,
                Some(self.video),
                Some(&mut mf),
            )
            .is_err()
            {
                self.error = Some("Windows' media player couldn't start".into());
                return;
            }
            let Some(mf) = mf else { return };
            let mut item: Option<IMFPMediaItem> = None;
            let url = HSTRING::from(path.as_os_str());
            if mf.CreateMediaItemFromURL(&url, true, 0, Some(&mut item)).is_err() || item.is_none() {
                self.error = Some(format!(
                    "Couldn't play {}",
                    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                ));
                return;
            }
            let _ = mf.SetMediaItem(item.as_ref());
            let _ = mf.SetVolume(self.volume);
            let _ = mf.Play();
            self.playing = true;
            self.mf = Some(mf);
            let title = format!(
                "{} \u{2014} Media Player",
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            );
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(title));
        }
        self.place();
    }

    fn open_path(&mut self, path: PathBuf) {
        let mut list: Vec<PathBuf> = path
            .parent()
            .and_then(|d| std::fs::read_dir(d).ok())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_media(p))
            .collect();
        list.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
        let at = list.iter().position(|p| *p == path).unwrap_or_else(|| {
            list.insert(0, path.clone());
            0
        });
        self.list = list;
        self.load(at);
    }

    fn toggle(&mut self) {
        let Some(mf) = &self.mf else { return };
        unsafe {
            if self.playing {
                let _ = mf.Pause();
            } else {
                let _ = mf.Play();
            }
        }
        self.playing = !self.playing;
    }

    fn seek(&mut self, secs: f64) {
        let Some(mf) = &self.mf else { return };
        let t = (secs.clamp(0.0, self.len.max(0.0)) * 10_000_000.0) as i64;
        unsafe {
            let _ = mf.SetPosition(&MFP_POSITIONTYPE_100NS, &PROPVARIANT::from(t));
        }
        self.pos = secs;
    }

    fn fullscreen(&mut self) {
        unsafe {
            let style = GetWindowLongW(self.hwnd, GWL_STYLE) as u32;
            if !self.full {
                let _ = GetWindowPlacement(self.hwnd, &mut self.placement);
                let mon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
                let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(mon, &mut mi);
                SetWindowLongW(self.hwnd, GWL_STYLE, (style & !WS_OVERLAPPEDWINDOW.0) as i32);
                let r = mi.rcMonitor;
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOP),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_FRAMECHANGED,
                );
            } else {
                SetWindowLongW(self.hwnd, GWL_STYLE, (style | WS_OVERLAPPEDWINDOW.0) as i32);
                let _ = SetWindowPlacement(self.hwnd, &self.placement);
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            }
        }
        self.full = !self.full;
        self.place();
    }

    fn next(&mut self, d: isize) {
        if self.list.is_empty() {
            return;
        }
        let n = self.list.len() as isize;
        let i = (self.at as isize + d).rem_euclid(n) as usize;
        self.load(i);
    }
}

const OPEN: u32 = 1;
const PLAY: u32 = 2;
const PREV: u32 = 3;
const NEXT: u32 = 4;
const REPEAT: u32 = 5;
const FULL: u32 = 6;
const SEEK: u32 = 7;
const VOL: u32 = 8;

impl App for Player {
    fn draw(&mut self, p: &Painter, f: &Fonts, w: f32, h: f32, kit: &mut Kit) {
        if self.full {
            return;
        }
        let accent = crate::theme::accent();
        let y0 = h - BAR;
        p.rrect(0.0, y0, w, BAR, 0.0, Color(0.1, 0.1, 0.105, 1.0));
        // The seek bar, with the times either side.
        let (sx, sw) = (70.0, w - 140.0);
        let frac = if self.len > 0.0 { (self.pos / self.len) as f32 } else { 0.0 };
        p.text(&hms(self.pos), &f.small, 8.0, y0 + 6.0, 56.0, 20.0, Align::Right, Color::white(0.7));
        p.text(&hms(self.len), &f.small, w - 64.0, y0 + 6.0, 56.0, 20.0, Align::Left, Color::white(0.7));
        p.rrect(sx, y0 + 14.0, sw, 4.0, 2.0, Color::white(0.15));
        p.rrect(sx, y0 + 14.0, sw * frac.clamp(0.0, 1.0), 4.0, 2.0, accent);
        let (hot, _) = kit.lit(SEEK);
        p.circle(
            sx + sw * frac.clamp(0.0, 1.0),
            y0 + 16.0,
            if hot || self.seeking { 8.0 } else { 6.0 },
            Color::white(0.95),
        );
        kit.hit(sx - 6.0, y0 + 4.0, sw + 12.0, 24.0, SEEK);
        // What's playing.
        let name = self.list.get(self.at).and_then(|p| p.file_stem()).map(|n| n.to_string_lossy().into_owned());
        let title = self.error.clone().or(name).unwrap_or_else(|| "Open something to play".into());
        p.text(&title, &f.body, 20.0, y0 + 36.0, w / 2.0 - 140.0, 48.0, Align::Left, Color::white(0.9));
        // The buttons, in the middle.
        let cx = w / 2.0;
        kit.round(p, f, cx - 56.0, y0 + 60.0, 18.0, 0xE892, Look::Plain, PREV);
        kit.round(p, f, cx, y0 + 60.0, 24.0, if self.playing { 0xE769 } else { 0xE768 }, Look::Accent, PLAY);
        kit.round(p, f, cx + 56.0, y0 + 60.0, 18.0, 0xE893, Look::Plain, NEXT);
        // Repeat, the volume, open and fullscreen on the right.
        let rx = w - 300.0;
        kit.round(
            p,
            f,
            rx,
            y0 + 60.0,
            16.0,
            if self.repeat { 0xE8ED } else { 0xE8EE },
            if self.repeat { Look::Accent } else { Look::Plain },
            REPEAT,
        );
        if let (Some(font), Some(c)) = (&f.icons, char::from_u32(0xE767)) {
            p.text(&c.to_string(), font, rx + 26.0, y0 + 52.0, 16.0, 16.0, Align::Center, Color::white(0.8));
        }
        let (vx, vw) = (rx + 48.0, 110.0);
        p.rrect(vx, y0 + 58.0, vw, 4.0, 2.0, Color::white(0.15));
        p.rrect(vx, y0 + 58.0, vw * self.volume, 4.0, 2.0, accent);
        p.circle(vx + vw * self.volume, y0 + 60.0, 6.0, Color::white(0.95));
        kit.hit(vx - 6.0, y0 + 48.0, vw + 12.0, 24.0, VOL);
        kit.round(p, f, w - 88.0, y0 + 60.0, 16.0, 0xE8E5, Look::Plain, OPEN);
        kit.round(p, f, w - 44.0, y0 + 60.0, 16.0, 0xE740, Look::Plain, FULL);
    }

    fn press(&mut self, id: u32, ctx: &Ctx) {
        match id {
            OPEN => {
                let filter = EXTS.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";");
                if let Some(p) = super::pick_file("Open media", &[("Media", &filter), ("All files", "*.*")]) {
                    self.open_path(p);
                }
            }
            PLAY => self.toggle(),
            PREV => self.next(-1),
            NEXT => self.next(1),
            REPEAT => self.repeat = !self.repeat,
            FULL => self.fullscreen(),
            _ => {}
        }
        ctx.redraw();
    }

    fn mouse(&mut self, ev: super::Mouse, x: f32, _: f32, ctx: &Ctx) -> bool {
        let (w, _) = ctx.size();
        // The seek bar and volume take drags (they're hit areas the frame
        // doesn't press: a press there is handled as it goes).
        let _ = (ev, x, w);
        false
    }

    fn tick_ms(&self) -> u32 {
        // Quick enough to follow a drag on the seek bar or the volume.
        80
    }

    fn tick(&mut self, ctx: &Ctx) -> bool {
        let Some(mf) = self.mf.clone() else { return false };
        unsafe {
            if !self.seeking {
                if let Ok(v) = mf.GetPosition(&MFP_POSITIONTYPE_100NS) {
                    self.pos = u64::try_from(&v)
                        .map(|t| t as f64 / 1e7)
                        .or_else(|_| i64::try_from(&v).map(|t| t as f64 / 1e7))
                        .unwrap_or(self.pos);
                }
            }
            if let Ok(v) = mf.GetDuration(&MFP_POSITIONTYPE_100NS) {
                self.len = u64::try_from(&v)
                    .map(|t| t as f64 / 1e7)
                    .or_else(|_| i64::try_from(&v).map(|t| t as f64 / 1e7))
                    .unwrap_or(self.len);
            }
            // At the end: round again, or on to the next.
            if self.playing && self.len > 0.0 && self.pos >= self.len - 0.3 {
                if self.repeat {
                    self.seek(0.0);
                    let _ = mf.Play();
                } else if self.at + 1 < self.list.len() {
                    self.next(1);
                } else {
                    self.playing = false;
                }
            }
            // Dragging the seek bar or the volume (the button's held over them).
            let held = GetKeyState(VK_LBUTTON.0 as i32) < 0;
            if held {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(ctx.hwnd, &mut pt);
                let (w, h) = ctx.size();
                let (x, y) = (pt.x as f32 / ctx.k, pt.y as f32 / ctx.k);
                let y0 = h - BAR;
                if (y0..y0 + 30.0).contains(&y) && (64.0..w - 64.0).contains(&x) {
                    let t = ((x - 70.0) / (w - 140.0)).clamp(0.0, 1.0) as f64;
                    self.seek(t * self.len);
                } else if (y0 + 46.0..y0 + 74.0).contains(&y) {
                    let vx = w - 300.0 + 48.0;
                    if (vx - 8.0..vx + 118.0).contains(&x) {
                        self.volume = ((x - vx) / 110.0).clamp(0.0, 1.0);
                        if let Some(mf) = &self.mf {
                            let _ = mf.SetVolume(self.volume);
                        }
                    }
                }
            }
        }
        true
    }

    fn key(&mut self, vk: u16, ctx: &Ctx) -> bool {
        match VIRTUAL_KEY(vk) {
            VK_SPACE => self.toggle(),
            VK_RIGHT => self.seek(self.pos + 5.0),
            VK_LEFT => self.seek(self.pos - 5.0),
            VK_UP | VK_DOWN => {
                let d = if VIRTUAL_KEY(vk) == VK_UP { 0.05 } else { -0.05 };
                self.volume = (self.volume + d).clamp(0.0, 1.0);
                if let Some(mf) = &self.mf {
                    unsafe {
                        let _ = mf.SetVolume(self.volume);
                    }
                }
            }
            VK_F11 => self.fullscreen(),
            VK_ESCAPE if self.full => self.fullscreen(),
            _ => return false,
        }
        ctx.redraw();
        true
    }

    fn size(&mut self, _: &Ctx) {
        self.place();
    }

    fn message(&mut self, m: u32, _: WPARAM, _: LPARAM, _: &Ctx) -> bool {
        match m {
            VIDEO_PAINT => {
                if let Some(mf) = &self.mf {
                    unsafe {
                        let _ = mf.UpdateVideo();
                    }
                }
            }
            VIDEO_CLICK => self.toggle(),
            VIDEO_DOUBLE => self.fullscreen(),
            _ => return false,
        }
        true
    }

    fn closing(&mut self, _: &Ctx) -> bool {
        if let Some(mf) = self.mf.take() {
            unsafe {
                let _ = mf.Shutdown();
            }
        }
        true
    }
}

/// `cascade.exe --player [file]`.
pub fn run(file: Option<String>) {
    super::run(w!("CascadeMediaPlayer"), "Media Player", "Cascade.MediaPlayer", 1100, 720, |ctx| unsafe {
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(video_proc),
            hInstance: crate::appwin::instance(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("CascadeVideoArea"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let video = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("CascadeVideoArea"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            0,
            0,
            10,
            10,
            Some(ctx.hwnd),
            None,
            Some(crate::appwin::instance()),
            None,
        )
        .unwrap_or_default();
        let mut p = Player {
            hwnd: ctx.hwnd,
            video,
            mf: None,
            list: Vec::new(),
            at: 0,
            volume: 0.8,
            repeat: false,
            full: false,
            placement: WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() },
            pos: 0.0,
            len: 0.0,
            playing: false,
            seeking: false,
            error: None,
        };
        p.place();
        if let Some(f) = file.filter(|f| !f.is_empty()) {
            p.open_path(PathBuf::from(f));
        }
        Box::new(p)
    });
}
