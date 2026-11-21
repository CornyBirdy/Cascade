//! Photos (`cascade.exe --view <picture>`): a quick picture viewer. Windows'
//! Photos app is a Store app, which can't open without Explorer, so in shell
//! mode pictures open here (and anywhere, with `photo_viewer = cascade`).
//! It decodes with Windows' own image codecs (WIC), so whatever Windows can
//! read it shows: JPEG, PNG, GIF, BMP, TIFF, WebP, HEIC and RAW with their
//! extensions installed.
//!
//! Wheel or +/- to zoom (at the pointer), drag to move around, double-click
//! or 0 / 1 for fit and 100%; Left / Right (or Page Up / Down, Home, End)
//! for the other pictures in the folder; R / Shift+R to rotate; F11 for
//! fullscreen; Delete to recycle; Ctrl+C to copy the picture; right-click
//! for Open with, Show in Files, Set as wallpaper, Save rotation and more.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::appwin::{self, size_text};
use crate::gfx::{Align, Color, Gfx};

const BAR_H: f32 = 34.0;
/// The biggest side a picture is kept at (Direct2D's limit, and memory).
const MAX_SIDE: u32 = 8192;
const EXTS: [&str; 16] = [
    ".jpg", ".jpeg", ".jpe", ".jfif", ".png", ".gif", ".bmp", ".dib", ".tif", ".tiff", ".webp", ".heic", ".heif",
    ".avif", ".ico", ".jxr",
];

pub fn is_picture(path: &str) -> bool {
    let e = crate::filesio::ext_of(path);
    EXTS.contains(&e.as_str())
}

/// Whether Cascade's viewer opens `path` rather than its usual app: a
/// picture, in shell mode (Photos can't open there) or when asked for.
pub fn should_handle(path: &str) -> bool {
    if !is_picture(path) {
        return false;
    }
    match crate::config::Config::load().photo_viewer.as_str() {
        "cascade" => true,
        "windows" => false,
        _ => crate::shellmode::active(),
    }
}

struct Picture {
    pixels: Vec<u8>,
    w: u32,
    h: u32,
    /// Its real size (before any shrinking to fit memory).
    full: (u32, u32),
}

/// A picture file as BGRA pixels and its size (for Paint).
pub(crate) fn load_pixels(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    decode(path).map(|p| (p.pixels, p.w, p.h))
}

/// Decodes `path` to premultiplied BGRA (on the calling thread).
fn decode(path: &Path) -> Option<Picture> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let dec = wic
            .CreateDecoderFromFilename(
                &HSTRING::from(path.as_os_str()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        let full = (w, h);
        // The orientation the camera saved (EXIF), applied as Windows' own viewers do.
        let source: IWICBitmapSource = match orientation(&frame) {
            Some(t) if t != WICBitmapTransformRotate0 => {
                let r = wic.CreateBitmapFlipRotator().ok()?;
                r.Initialize(&frame, t).ok()?;
                r.cast().ok()?
            }
            _ => frame.cast().ok()?,
        };
        let (mut w, mut h) = (0u32, 0u32);
        source.GetSize(&mut w, &mut h).ok()?;
        let source: IWICBitmapSource = if w.max(h) > MAX_SIDE {
            let k = MAX_SIDE as f64 / w.max(h) as f64;
            let (nw, nh) = (((w as f64 * k) as u32).max(1), ((h as f64 * k) as u32).max(1));
            let s = wic.CreateBitmapScaler().ok()?;
            s.Initialize(&source, nw, nh, WICBitmapInterpolationModeFant).ok()?;
            w = nw;
            h = nh;
            s.cast().ok()?
        } else {
            source
        };
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(
            &source,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .ok()?;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut pixels).ok()?;
        Some(Picture { pixels, w, h, full })
    }
}

use windows::core::Interface;

/// The EXIF orientation of a frame, as a rotation to apply.
unsafe fn orientation(frame: &IWICBitmapFrameDecode) -> Option<WICBitmapTransformOptions> {
    let reader = frame.GetMetadataQueryReader().ok()?;
    let mut v = windows::Win32::System::Com::StructuredStorage::PROPVARIANT::default();
    for q in [w!("/app1/ifd/{ushort=274}"), w!("/ifd/{ushort=274}")] {
        if reader.GetMetadataByName(q, &mut v).is_ok() {
            let o = u16::try_from(&v).unwrap_or(1);
            return Some(match o {
                3 => WICBitmapTransformRotate180,
                6 => WICBitmapTransformRotate90,
                8 => WICBitmapTransformRotate270,
                _ => WICBitmapTransformRotate0,
            });
        }
    }
    None
}

struct State {
    hwnd: HWND,
    gfx: Option<Gfx>,
    files: Vec<PathBuf>,
    at: usize,
    pic: Option<Picture>,
    bmp: Option<ID2D1Bitmap>,
    failed: bool,
    /// Zoom (None: fit the window), where the picture's centre is from the window's, and turns of 90°.
    zoom: Option<f32>,
    pan: (f32, f32),
    turns: u8,
    drag: Option<(f32, f32, (f32, f32))>,
    fullscreen: Option<(WINDOW_STYLE, RECT)>,
    bytes: u64,
    generation: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// The pictures in `path`'s folder, in Explorer's order.
fn siblings(path: &Path) -> Vec<PathBuf> {
    let Some(dir) = path.parent() else { return vec![path.to_path_buf()] };
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.path()).filter(|p| is_picture(&p.to_string_lossy())).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| {
        let (wa, wb): (Vec<u16>, Vec<u16>) = (a.as_os_str().encode_wide_null(), b.as_os_str().encode_wide_null());
        unsafe {
            windows::Win32::UI::Shell::StrCmpLogicalW(
                windows::core::PCWSTR(wa.as_ptr()),
                windows::core::PCWSTR(wb.as_ptr()),
            )
        }
        .cmp(&0)
    });
    if !v.iter().any(|p| p == path) {
        v.push(path.to_path_buf());
    }
    v
}

trait WideNull {
    fn encode_wide_null(&self) -> Vec<u16>;
}

impl WideNull for std::ffi::OsStr {
    fn encode_wide_null(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain([0]).collect()
    }
}

const LOADED: u32 = WM_APP + 1;
static DECODED: std::sync::Mutex<Option<(u32, Option<Picture>)>> = std::sync::Mutex::new(None);

impl State {
    fn path(&self) -> Option<&PathBuf> {
        self.files.get(self.at)
    }

    fn show(&mut self, at: usize) {
        if self.files.is_empty() {
            return;
        }
        self.at = at.min(self.files.len() - 1);
        self.zoom = None;
        self.pan = (0.0, 0.0);
        self.turns = 0;
        self.failed = false;
        self.generation = self.generation.wrapping_add(1);
        let Some(path) = self.path().cloned() else { return };
        self.bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(format!("{name} \u{2014} Photos")));
        }
        // Decoded on a thread of its own: a big picture never freezes the window.
        let (generation, hwnd) = (self.generation, self.hwnd.0 as isize);
        std::thread::spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let pic = decode(&path);
            *DECODED.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, pic));
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), LOADED, WPARAM(0), LPARAM(0));
        });
        appwin::redraw(self.hwnd);
    }

    fn decoded(&mut self) {
        let Some((generation, pic)) = DECODED.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        if generation != self.generation {
            return;
        }
        self.failed = pic.is_none();
        self.bmp = pic.as_ref().and_then(|p| self.gfx.as_ref()?.bitmap(p.w, p.h, &p.pixels).ok());
        self.pic = pic;
        appwin::redraw(self.hwnd);
    }

    /// How big the picture is drawn (window pixels per picture pixel).
    fn scale(&self, w: f32, h: f32) -> f32 {
        let Some(p) = &self.pic else { return 1.0 };
        let (pw, ph) = if self.turns % 2 == 1 { (p.h as f32, p.w as f32) } else { (p.w as f32, p.h as f32) };
        // Shrunk to fit, never blown up past 100% when fitting.
        let fit = (w / pw).min((h - BAR_H) / ph).min(1.0);
        let k = appwin::scale(self.hwnd);
        // 100% is one picture pixel per screen pixel.
        let real = |z: f32| z / k;
        self.zoom.map_or(fit, real)
    }

    fn zoom_at(&mut self, factor: f32, cx: f32, cy: f32) {
        let (w, h) = appwin::client(self.hwnd);
        let k = appwin::scale(self.hwnd);
        let old = self.scale(w, h);
        let new = (old * factor).clamp(0.02, 40.0);
        // Keep the point under the pointer where it is.
        let (mx, my) = (w / 2.0 + self.pan.0, (h - BAR_H) / 2.0 + self.pan.1);
        let r = new / old;
        self.pan.0 += (cx - mx) * (1.0 - r);
        self.pan.1 += (cy - my) * (1.0 - r);
        self.zoom = Some(new * k);
        appwin::redraw(self.hwnd);
    }

    fn step(&mut self, by: isize) {
        let n = self.files.len() as isize;
        if n > 0 {
            self.show(((self.at as isize + by).rem_euclid(n)) as usize);
        }
    }

    fn delete(&mut self) {
        let Some(path) = self.path().cloned() else { return };
        crate::deskicons::delete(self.hwnd, &[path.to_string_lossy().into_owned()], false);
        self.files.remove(self.at);
        if self.files.is_empty() {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
            return;
        }
        let at = self.at.min(self.files.len() - 1);
        self.show(at);
    }

    fn copy(&self) {
        let Some(p) = &self.pic else { return };
        // Straight alpha, as the clipboard's pictures are.
        let px: Vec<u32> = p
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                let a = c[3] as u32;
                let un = |v: u8| (v as u32 * 255 + a / 2).checked_div(a).unwrap_or(0).min(255);
                (a << 24) | (un(c[2]) << 16) | (un(c[1]) << 8) | un(c[0])
            })
            .collect();
        unsafe {
            crate::snip::copy_image(Some(self.hwnd), &px, p.w as i32, p.h as i32);
        }
    }

    fn toggle_fullscreen(&mut self) {
        unsafe {
            match self.fullscreen.take() {
                Some((style, r)) => {
                    SetWindowLongW(self.hwnd, GWL_STYLE, style.0 as i32);
                    let _ = SetWindowPos(
                        self.hwnd,
                        Some(HWND_NOTOPMOST),
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_FRAMECHANGED,
                    );
                }
                None => {
                    let style = WINDOW_STYLE(GetWindowLongW(self.hwnd, GWL_STYLE) as u32);
                    let mut r = RECT::default();
                    let _ = GetWindowRect(self.hwnd, &mut r);
                    self.fullscreen = Some((style, r));
                    let mut mi =
                        MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                    let _ = GetMonitorInfoW(MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST), &mut mi);
                    let m = mi.rcMonitor;
                    SetWindowLongW(self.hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as i32);
                    let _ = SetWindowPos(
                        self.hwnd,
                        Some(HWND_TOP),
                        m.left,
                        m.top,
                        m.right - m.left,
                        m.bottom - m.top,
                        SWP_FRAMECHANGED,
                    );
                }
            }
        }
        self.zoom = None;
        self.pan = (0.0, 0.0);
    }

    /// Saves the picture turned as it's shown (re-encoded in its own format).
    fn save_rotation(&mut self) {
        let Some(path) = self.path().cloned() else { return };
        let turns = self.turns % 4;
        if turns == 0 {
            return;
        }
        let ok = unsafe { save_rotated(&path, turns).is_some() };
        crate::config::log(&format!("photos: saved {} turned {turns}: {ok}", path.display()));
        if ok {
            let at = self.at;
            self.show(at);
        } else {
            crate::winset::tell("Windows couldn't save the picture turned (its format may not be writable).".into());
        }
    }

    fn menu(&mut self) {
        use crate::menu::{item, show, Entry, Place};
        let Some(path) = self.path().cloned() else { return };
        let p = path.to_string_lossy().into_owned();
        let entries = vec![
            item(1, "Open with\u{2026}"),
            item(2, "Show in Files"),
            Entry::Separator,
            item(3, "Rotate left"),
            item(4, "Rotate right"),
            item(5, "Save rotation"),
            Entry::Separator,
            item(6, "Copy"),
            item(7, "Copy path"),
            item(8, "Set as wallpaper"),
            Entry::Separator,
            item(9, "Delete"),
            item(10, "Properties"),
        ];
        match show(self.hwnd, entries, Place::Cursor) {
            1 => unsafe {
                let _ = windows::Win32::UI::Shell::ShellExecuteW(
                    Some(self.hwnd),
                    w!("openas"),
                    &HSTRING::from(p.as_str()),
                    None,
                    None,
                    SW_SHOWNORMAL,
                );
            },
            2 => {
                let dir = path.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
                appwin::launch(&["--files", &dir]);
            }
            3 => self.turns = (self.turns + 3) % 4,
            4 => self.turns = (self.turns + 1) % 4,
            5 => self.save_rotation(),
            6 => self.copy(),
            7 => crate::shell::copy_text(&p),
            8 => {
                crate::wallpaper::set_picture(&path);
            }
            9 => self.delete(),
            10 => unsafe {
                let _ = windows::Win32::UI::Shell::SHObjectProperties(
                    Some(self.hwnd),
                    windows::Win32::UI::Shell::SHOP_FILEPATH,
                    &HSTRING::from(p.as_str()),
                    windows::core::PCWSTR::null(),
                );
            },
            _ => {}
        }
        appwin::redraw(self.hwnd);
    }

    fn paint(&mut self) {
        let Some(mut gfx) = self.gfx.take() else { return };
        let hwnd = self.hwnd;
        appwin::paint(hwnd, &mut gfx, |p, f, w, h| {
            p.rrect(0.0, 0.0, w, h, 0.0, Color(0.06, 0.06, 0.065, 1.0));
            let full = self.fullscreen.is_some();
            let area_h = if full { h } else { h - BAR_H };
            match (&self.bmp, &self.pic) {
                (Some(b), Some(pic)) => {
                    let s = self.scale(w, if full { h + BAR_H } else { h });
                    let (dw, dh) = (pic.w as f32 * s, pic.h as f32 * s);
                    let (cx, cy) = (w / 2.0 + self.pan.0, area_h / 2.0 + self.pan.1);
                    p.rotated(self.turns as f32 * 90.0, cx, cy, || {
                        p.image(b, cx - dw / 2.0, cy - dh / 2.0, dw, dh, 1.0)
                    });
                }
                _ => {
                    let msg =
                        if self.failed { "Windows can't read this picture (a codec may be missing)." } else { "" };
                    p.text(msg, &f.body, 0.0, area_h / 2.0 - 10.0, w, 20.0, Align::Center, Color::white(0.6));
                }
            }
            if full {
                return;
            }
            // The bar: name, size, where in the folder, zoom.
            p.rrect(0.0, h - BAR_H, w, BAR_H, 0.0, Color(0.11, 0.11, 0.115, 1.0));
            let name =
                self.path().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let dims = self.pic.as_ref().map(|p| format!("{} \u{d7} {}", p.full.0, p.full.1)).unwrap_or_default();
            let zoom = format!("{:.0}%", self.scale(w, h) * appwin::scale(self.hwnd) * 100.0);
            let info =
                format!("{name}    {dims}    {}    {} of {}", size_text(self.bytes), self.at + 1, self.files.len());
            p.text(&info, &f.small, 14.0, h - BAR_H, w - 120.0, BAR_H, Align::Left, Color::white(0.7));
            p.text(&zoom, &f.small, w - 100.0, h - BAR_H, 86.0, BAR_H, Align::Right, Color::white(0.7));
        });
        self.gfx = Some(gfx);
    }
}

unsafe fn save_rotated(path: &Path, turns: u8) -> Option<()> {
    let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
    let dec = wic
        .CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnLoad)
        .ok()?;
    let format = dec.GetContainerFormat().ok()?;
    let frame = dec.GetFrame(0).ok()?;
    let rot = wic.CreateBitmapFlipRotator().ok()?;
    let t = match turns {
        1 => WICBitmapTransformRotate90,
        2 => WICBitmapTransformRotate180,
        _ => WICBitmapTransformRotate270,
    };
    rot.Initialize(&frame, t).ok()?;
    // Into memory first: the file is still open for reading.
    let bmp = wic.CreateBitmapFromSource(&rot, WICBitmapCacheOnLoad).ok()?;
    drop(frame);
    drop(dec);
    let tmp = path.with_extension("cascade-tmp");
    let stream = wic.CreateStream().ok()?;
    stream.InitializeFromFilename(&HSTRING::from(tmp.as_os_str()), GENERIC_WRITE.0).ok()?;
    let enc = wic.CreateEncoder(&format, std::ptr::null()).ok()?;
    enc.Initialize(&stream, WICBitmapEncoderNoCache).ok()?;
    let mut fe = None;
    enc.CreateNewFrame(&mut fe, std::ptr::null_mut()).ok()?;
    let fe = fe?;
    fe.Initialize(None).ok()?;
    let (mut w, mut h) = (0u32, 0u32);
    bmp.GetSize(&mut w, &mut h).ok()?;
    fe.SetSize(w, h).ok()?;
    let mut pf = GUID_WICPixelFormat32bppBGRA;
    bmp.GetPixelFormat().map(|f| pf = f).ok();
    fe.SetPixelFormat(&mut pf).ok()?;
    fe.WriteSource(&bmp, std::ptr::null()).ok()?;
    fe.Commit().ok()?;
    enc.Commit().ok()?;
    drop(stream);
    std::fs::rename(&tmp, path).ok()
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_PAINT => {
            if with(|s| s.paint()).is_none() {
                return DefWindowProcW(hwnd, m, wp, lp);
            }
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE => appwin::redraw(hwnd),
        LOADED => {
            with(|s| s.decoded());
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
            let mut p = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut p);
            let k = appwin::scale(hwnd);
            with(|s| s.zoom_at(1.2f32.powf(delta), p.x as f32 / k, p.y as f32 / k));
        }
        WM_LBUTTONDOWN => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| s.drag = Some((x, y, s.pan)));
            SetCapture(hwnd);
        }
        WM_MOUSEMOVE => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| {
                if let Some((sx, sy, (px, py))) = s.drag {
                    s.pan = (px + x - sx, py + y - sy);
                    appwin::redraw(hwnd);
                }
            });
        }
        WM_LBUTTONUP => {
            with(|s| s.drag = None);
            let _ = ReleaseCapture();
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| {
                if s.zoom.is_some() {
                    s.zoom = None;
                    s.pan = (0.0, 0.0);
                } else {
                    let (w, h) = appwin::client(hwnd);
                    let k = appwin::scale(hwnd);
                    let factor = 1.0 / (s.scale(w, h) * k);
                    s.zoom_at(factor, x, y);
                }
                appwin::redraw(hwnd);
            });
        }
        WM_RBUTTONUP => {
            with(|s| s.menu());
        }
        WM_XBUTTONUP => {
            let back = (wp.0 >> 16) & 0xFFFF == 1;
            with(|s| s.step(if back { -1 } else { 1 }));
        }
        WM_KEYDOWN => {
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
            let (w, h) = appwin::client(hwnd);
            with(|s| match VIRTUAL_KEY(wp.0 as u16) {
                VK_LEFT | VK_PRIOR => s.step(-1),
                VK_RIGHT | VK_NEXT | VK_SPACE => s.step(1),
                VK_HOME => s.show(0),
                VK_END => {
                    let n = s.files.len();
                    s.show(n.saturating_sub(1));
                }
                VK_DELETE => s.delete(),
                VK_F11 => s.toggle_fullscreen(),
                VK_ESCAPE => {
                    if s.fullscreen.is_some() {
                        s.toggle_fullscreen();
                    } else {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                VK_ADD | VK_OEM_PLUS => s.zoom_at(1.25, w / 2.0, (h - BAR_H) / 2.0),
                VK_SUBTRACT | VK_OEM_MINUS => s.zoom_at(0.8, w / 2.0, (h - BAR_H) / 2.0),
                k if k.0 == b'0' as u16 => {
                    s.zoom = None;
                    s.pan = (0.0, 0.0);
                }
                k if k.0 == b'1' as u16 => {
                    s.zoom = Some(1.0);
                    s.pan = (0.0, 0.0);
                }
                k if k.0 == b'R' as u16 => s.turns = (s.turns + if shift { 3 } else { 1 }) % 4,
                k if k.0 == b'F' as u16 => s.toggle_fullscreen(),
                k if k.0 == b'C' as u16 && ctrl => s.copy(),
                _ => {}
            });
            appwin::redraw(hwnd);
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, m, wp, lp)
}

/// `cascade.exe --view <picture>`.
pub fn run(path: Option<String>) {
    appwin::init();
    let Some(path) = path.map(PathBuf::from) else { return };
    let Ok(gfx) = Gfx::new() else { return };
    let Some(hwnd) = appwin::create(w!("CascadePhotos"), "Photos", 1100, 760, Some(proc)) else { return };
    appwin::identity(hwnd, "Cascade.Photos");
    let files = siblings(&path);
    let at = files.iter().position(|p| *p == path).unwrap_or(0);
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            hwnd,
            gfx: Some(gfx),
            files,
            at,
            pic: None,
            bmp: None,
            failed: false,
            zoom: None,
            pan: (0.0, 0.0),
            turns: 0,
            drag: None,
            fullscreen: None,
            bytes: 0,
            generation: 0,
        })
    });
    with(|s| s.show(at));
    appwin::run();
}
