//! The wallpaper on shell mode's desktop (see `shellmode`). Explorer draws it
//! when it's the shell; without Explorer, Cascade does.
//!
//! The picture is Windows' own setting (Control Panel\Desktop's WallPaper,
//! WallpaperStyle and TileWallpaper), or failing that the copy Windows keeps
//! of the picture last on screen (TranscodedWallpaper). It's decoded and
//! fitted to each screen once, on a thread of its own, and painted from
//! memory after that.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use windows::core::HSTRING;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config;

const DESKTOP_KEY: &str = "Control Panel\\Desktop";

/// How the picture fills a screen, as Windows' Personalisation page offers.
#[derive(Clone, Copy, PartialEq, Debug)]
#[allow(clippy::enum_variant_names)] // Windows' own names
pub enum Fit {
    Fill,
    Fit,
    Stretch,
    Center,
    Tile,
    /// One picture across every screen.
    Span,
}

impl Fit {
    pub const ALL: [(Fit, &'static str); 6] = [
        (Fit::Fill, "Fill"),
        (Fit::Fit, "Fit"),
        (Fit::Stretch, "Stretch"),
        (Fit::Center, "Centre"),
        (Fit::Tile, "Tile"),
        (Fit::Span, "Span"),
    ];

    /// From the registry's WallpaperStyle and TileWallpaper.
    fn from_registry(style: &str, tile: &str) -> Fit {
        match style.trim() {
            "6" => Fit::Fit,
            "2" => Fit::Stretch,
            "22" => Fit::Span,
            "0" if tile.trim() == "1" => Fit::Tile,
            "0" => Fit::Center,
            _ => Fit::Fill,
        }
    }

    /// WallpaperStyle and TileWallpaper for this fit.
    fn registry(self) -> (&'static str, &'static str) {
        match self {
            Fit::Fill => ("10", "0"),
            Fit::Fit => ("6", "0"),
            Fit::Stretch => ("2", "0"),
            Fit::Center => ("0", "0"),
            Fit::Tile => ("0", "1"),
            Fit::Span => ("22", "0"),
        }
    }
}

fn reg_text(key: &str, value: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    } == ERROR_SUCCESS;
    ok.then(|| {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..n])
    })
}

fn set_reg_text(key: &str, value: &str, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    unsafe {
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(value),
            REG_SZ.0,
            Some(wide.as_ptr() as *const c_void),
            (wide.len() * 2) as u32,
        );
    }
}

/// The fit Windows is set to.
pub fn fit() -> Fit {
    Fit::from_registry(
        &reg_text(DESKTOP_KEY, "WallpaperStyle").unwrap_or_default(),
        &reg_text(DESKTOP_KEY, "TileWallpaper").unwrap_or_default(),
    )
}

/// Changes the fit (Windows' own setting, so Explorer uses it too).
pub fn set_fit(f: Fit) {
    let (style, tile) = f.registry();
    set_reg_text(DESKTOP_KEY, "WallpaperStyle", style);
    set_reg_text(DESKTOP_KEY, "TileWallpaper", tile);
}

/// The picture to show: Windows' setting, or the copy of the last picture
/// on screen when the setting names a file that's gone (or names none at
/// all). None for a plain colour.
fn picture() -> Option<PathBuf> {
    let set = reg_text(DESKTOP_KEY, "WallPaper");
    if let Some(p) = &set {
        if p.trim().is_empty() {
            return None; // "Solid colour"
        }
        let p = PathBuf::from(p.trim());
        if p.is_file() {
            return Some(p);
        }
    }
    let copy = std::env::var_os("APPDATA")
        .map(|a| PathBuf::from(a).join("Microsoft\\Windows\\Themes\\TranscodedWallpaper"))?;
    copy.is_file().then_some(copy)
}

/// Makes `path` the wallpaper (Windows' own setting, which every window is told of).
pub fn set_picture(path: &Path) -> bool {
    let mut wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect();
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(wide.as_mut_ptr() as *mut c_void),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        )
    }
    .is_ok();
    // Windows writes the setting itself, but not always where it's read back from.
    if ok {
        set_reg_text(DESKTOP_KEY, "WallPaper", &path.to_string_lossy());
    }
    config::log(&format!("wallpaper: set to {} (took: {ok})", path.display()));
    ok
}

/// Asks for a picture with Windows' own Open box and makes it the
/// wallpaper, then `done` (to redraw the desktop). All on a thread of its
/// own: the Open box is slow to load without Explorer, and setting the
/// wallpaper tells (and waits on) every window.
pub fn choose_and_set(done: fn()) {
    crate::shell::off_thread(move || {
        let filters =
            [("Pictures", "*.jpg;*.jpeg;*.png;*.bmp;*.gif;*.tif;*.tiff;*.webp;*.jxr;*.heic"), ("All files", "*.*")];
        if let Some(path) = crate::shell::choose_file("Choose a wallpaper", &filters) {
            if set_picture(&path) {
                done();
            }
        }
    });
}

// ------------------------------------------------------------- drawing --

/// Where the picture goes in a `w`×`h` box: scaled to (width, height) and
/// placed at (x, y) from the box's top left (negative: cut off at that edge).
fn place(iw: u32, ih: u32, w: i32, h: i32, fit: Fit) -> (i32, i32, i32, i32) {
    let (fw, fh) = (iw.max(1) as f64, ih.max(1) as f64);
    let (bw, bh) = (w.max(1) as f64, h.max(1) as f64);
    let (sw, sh) = match fit {
        Fit::Fill | Fit::Span => {
            let k = (bw / fw).max(bh / fh);
            (fw * k, fh * k)
        }
        Fit::Fit => {
            let k = (bw / fw).min(bh / fh);
            (fw * k, fh * k)
        }
        Fit::Stretch => (bw, bh),
        Fit::Center | Fit::Tile => (fw, fh),
    };
    let (sw, sh) = ((sw.round() as i32).max(1), (sh.round() as i32).max(1));
    (sw, sh, (w - sw) / 2, (h - sh) / 2)
}

/// One screen's picture: where it goes in the desktop window, and the bitmap.
struct Painted {
    at: RECT,
    bitmap: HBITMAP,
}

// Bitmaps are plain GDI objects, usable from any thread.
unsafe impl Send for Painted {}

static PAINTED: Mutex<Vec<Painted>> = Mutex::new(Vec::new());
/// Bumped for every load, so an older one that finishes late is dropped.
static GENERATION: AtomicU32 = AtomicU32::new(0);

fn painted() -> std::sync::MutexGuard<'static, Vec<Painted>> {
    PAINTED.lock().unwrap_or_else(|e| e.into_inner())
}

unsafe extern "system" fn collect_screens(mon: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> windows::core::BOOL {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if GetMonitorInfoW(mon, &mut mi).as_bool() {
        (*(out.0 as *mut Vec<RECT>)).push(mi.rcMonitor);
    }
    true.into()
}

fn screens() -> Vec<RECT> {
    let mut list: Vec<RECT> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect_screens), LPARAM(&mut list as *mut _ as isize));
    }
    list
}

/// The colour behind the picture (Windows' desktop colour), as a DIB pixel.
fn background() -> u32 {
    let c = unsafe { GetSysColor(COLOR_BACKGROUND) };
    let (r, g, b) = (c & 0xFF, (c >> 8) & 0xFF, (c >> 16) & 0xFF);
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

/// A top-down 32-bit bitmap `w`×`h` and its pixels.
unsafe fn dib(w: i32, h: i32) -> Option<(HBITMAP, &'static mut [u32])> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    if bits.is_null() {
        let _ = DeleteObject(bitmap.into());
        return None;
    }
    Some((bitmap, std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize)))
}

/// The decoded picture, ready to be scaled.
struct Picture {
    wic: IWICImagingFactory,
    source: IWICBitmapSource,
    w: u32,
    h: u32,
    /// Scaled copies made so far: (size, source).
    scaled: Vec<((i32, i32), IWICBitmapSource)>,
}

impl Picture {
    unsafe fn open(path: &Path, cache: bool) -> Option<Picture> {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let decoder = wic
            .CreateDecoderFromFilename(
                &HSTRING::from(path.as_os_str()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let convert = wic.CreateFormatConverter().ok()?;
        convert
            .Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .ok()?;
        let source: IWICBitmapSource = if cache {
            wic.CreateBitmapFromSource(&convert, WICBitmapCacheOnLoad).ok()?.into()
        } else {
            convert.into()
        };
        let (mut w, mut h) = (0u32, 0u32);
        source.GetSize(&mut w, &mut h).ok()?;
        (w > 0 && h > 0).then_some(Picture { wic, source, w, h, scaled: Vec::new() })
    }

    /// The picture at `sw`×`sh`.
    unsafe fn at(&mut self, sw: i32, sh: i32) -> Option<IWICBitmapSource> {
        if (sw as u32, sh as u32) == (self.w, self.h) {
            return Some(self.source.clone());
        }
        if let Some((_, s)) = self.scaled.iter().find(|(size, _)| *size == (sw, sh)) {
            return Some(s.clone());
        }
        let scaler = self.wic.CreateBitmapScaler().ok()?;
        scaler.Initialize(&self.source, sw as u32, sh as u32, WICBitmapInterpolationModeHighQualityCubic).ok()?;
        let s: IWICBitmapSource = scaler.into();
        self.scaled.push(((sw, sh), s.clone()));
        Some(s)
    }

    /// Draws the picture, scaled to `sw`×`sh`, at (x, y) in the `w`×`h` pixels `out`.
    unsafe fn draw(&mut self, out: &mut [u32], w: i32, h: i32, (sw, sh, x, y): (i32, i32, i32, i32)) {
        let (x0, y0, x1, y1) = (x.max(0), y.max(0), (x + sw).min(w), (y + sh).min(h));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let Some(source) = self.at(sw, sh) else { return };
        let rect = WICRect { X: x0 - x, Y: y0 - y, Width: x1 - x0, Height: y1 - y0 };
        let start = (y0 * w + x0) as usize;
        let bytes = std::slice::from_raw_parts_mut(out[start..].as_mut_ptr() as *mut u8, (out.len() - start) * 4);
        let _ = source.CopyPixels(&rect, (w * 4) as u32, bytes);
    }
}

/// What the wallpaper is now (its file and when it changed), to tell
/// whether a `thumbnail` made earlier is still it.
pub fn picture_key() -> String {
    match picture() {
        Some(p) => {
            let at = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
            format!("{}|{at:?}", p.display())
        }
        None => format!("colour {:08X}", background()),
    }
}

/// The wallpaper as a small `w`×`h` picture filling it (cropped, not
/// squashed), premultiplied BGRA: Task View's pictures of the desktops sit
/// on it. A plain colour for "Solid colour". Decodes the file, so keep it
/// (see `picture_key`) rather than asking each time.
pub fn thumbnail(w: i32, h: i32) -> Option<Vec<u8>> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut out = vec![background(); (w * h) as usize];
    unsafe {
        if let Some(mut pic) = picture().and_then(|p| Picture::open(&p, false)) {
            let s = (w as f32 / pic.w as f32).max(h as f32 / pic.h as f32);
            let (sw, sh) = ((pic.w as f32 * s).ceil() as i32, (pic.h as f32 * s).ceil() as i32);
            pic.draw(&mut out, w, h, (sw, sh, (w - sw) / 2, (h - sh) / 2));
        }
    }
    for px in &mut out {
        *px |= 0xFF00_0000;
    }
    Some(out.iter().flat_map(|p| p.to_le_bytes()).collect())
}

/// Every screen's picture, in desktop-window coordinates (`origin` is the
/// window's top left on the screen).
unsafe fn render(origin: (i32, i32)) -> Vec<Painted> {
    let bg = background();
    let fit = fit();
    let all = screens();
    let mut picture = picture().and_then(|p| {
        let opened = Picture::open(&p, fit == Fit::Tile);
        if opened.is_none() {
            config::log(&format!("wallpaper: couldn't read {}", p.display()));
        }
        opened
    });
    // Span: one picture over the box round every screen.
    let span = all.iter().fold(None::<RECT>, |acc, r| {
        Some(match acc {
            None => *r,
            Some(a) => RECT {
                left: a.left.min(r.left),
                top: a.top.min(r.top),
                right: a.right.max(r.right),
                bottom: a.bottom.max(r.bottom),
            },
        })
    });
    let mut out = Vec::new();
    for screen in all {
        let (w, h) = (screen.right - screen.left, screen.bottom - screen.top);
        if w <= 0 || h <= 0 {
            continue;
        }
        let Some((bitmap, pixels)) = dib(w, h) else { continue };
        pixels.fill(bg);
        if let Some(p) = picture.as_mut() {
            match fit {
                Fit::Tile => {
                    let (tw, th) = (p.w as i32, p.h as i32);
                    for ty in (0..h).step_by(th.max(1) as usize) {
                        for tx in (0..w).step_by(tw.max(1) as usize) {
                            p.draw(pixels, w, h, (tw, th, tx, ty));
                        }
                    }
                }
                Fit::Span => {
                    let s = span.unwrap_or(screen);
                    let (sw, sh, x, y) = place(p.w, p.h, s.right - s.left, s.bottom - s.top, Fit::Span);
                    p.draw(pixels, w, h, (sw, sh, x - (screen.left - s.left), y - (screen.top - s.top)));
                }
                _ => {
                    let at = place(p.w, p.h, w, h, fit);
                    p.draw(pixels, w, h, at);
                }
            }
        }
        let at = RECT {
            left: screen.left - origin.0,
            top: screen.top - origin.1,
            right: screen.right - origin.0,
            bottom: screen.bottom - origin.1,
        };
        out.push(Painted { at, bitmap });
    }
    out
}

/// (Re)loads the picture on a thread of its own, then has `window` (whose
/// top left is at `origin` on the screen) repaint.
pub fn reload(window: HWND, origin: (i32, i32)) {
    let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    let window = window.0 as isize;
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let fresh = render(origin);
        if GENERATION.load(Ordering::Acquire) != generation {
            for p in fresh {
                let _ = DeleteObject(p.bitmap.into());
            }
            return;
        }
        let old = std::mem::replace(&mut *painted(), fresh);
        for p in old {
            let _ = DeleteObject(p.bitmap.into());
        }
        let _ = InvalidateRect(Some(HWND(window as *mut c_void)), None, false);
    });
}

/// Frees the pictures (the desktop is going away).
pub fn release() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    for p in painted().drain(..) {
        unsafe {
            let _ = DeleteObject(p.bitmap.into());
        }
    }
}

/// Paints the desktop window's WM_PAINT: each screen's picture, and the
/// desktop colour anywhere no screen is.
pub unsafe fn paint(window: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(window, &mut ps);
    {
        let list = painted();
        let mem = CreateCompatibleDC(Some(dc));
        for p in list.iter() {
            let old = SelectObject(mem, p.bitmap.into());
            let (w, h) = (p.at.right - p.at.left, p.at.bottom - p.at.top);
            let _ = BitBlt(dc, p.at.left, p.at.top, w, h, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old);
            let _ = ExcludeClipRect(dc, p.at.left, p.at.top, p.at.right, p.at.bottom);
        }
        let _ = DeleteDC(mem);
    }
    let brush = GetSysColorBrush(COLOR_BACKGROUND);
    FillRect(dc, &ps.rcPaint, brush);
    let _ = EndPaint(window, &ps);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_the_picture() {
        // A 2:1 picture on a 16:9 screen.
        assert_eq!(place(2000, 1000, 1920, 1080, Fit::Fill), (2160, 1080, -120, 0));
        assert_eq!(place(2000, 1000, 1920, 1080, Fit::Fit), (1920, 960, 0, 60));
        assert_eq!(place(2000, 1000, 1920, 1080, Fit::Stretch), (1920, 1080, 0, 0));
        assert_eq!(place(800, 600, 1920, 1080, Fit::Center), (800, 600, 560, 240));
        // Bigger than the screen at its own size: cut off evenly.
        assert_eq!(place(4000, 2000, 1920, 1080, Fit::Center), (4000, 2000, -1040, -460));
    }

    #[test]
    fn reads_windows_fit_setting() {
        assert_eq!(Fit::from_registry("10", "0"), Fit::Fill);
        assert_eq!(Fit::from_registry("6", "0"), Fit::Fit);
        assert_eq!(Fit::from_registry("2", "0"), Fit::Stretch);
        assert_eq!(Fit::from_registry("0", "0"), Fit::Center);
        assert_eq!(Fit::from_registry("0", "1"), Fit::Tile);
        assert_eq!(Fit::from_registry("22", "0"), Fit::Span);
        assert_eq!(Fit::from_registry("", ""), Fit::Fill);
        for (f, _) in Fit::ALL {
            let (style, tile) = f.registry();
            assert_eq!(Fit::from_registry(style, tile), f);
        }
    }
}
