//! Screen snips (Win+Shift+S) in shell mode: Windows' Snipping Tool is a
//! Store app, which only Explorer can open. The screens are captured the
//! moment it starts and shown dimmed; drag over the part you want (or click
//! for the whole screen under the pointer, Esc or a right-click to leave it).
//! The snip goes on the clipboard and into Pictures\Screenshots.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::Shell::{FOLDERID_Screenshots, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{config, msg};

const CF_DIB: u32 = 8;

/// The capture of every screen, and the snip being made.
struct Snip {
    window: HWND,
    /// Where the capture starts on the screen (the virtual screen's top left).
    origin: (i32, i32),
    w: i32,
    h: i32,
    /// The screens as they were, and dimmed, as bitmaps to paint from.
    bright: HBITMAP,
    dim: HBITMAP,
    /// The pixels of `bright` (top-down BGRA).
    pixels: &'static mut [u32],
    /// Where the drag started and where it is now, in the window.
    from: Option<POINT>,
    to: POINT,
}

// Only touched on the UI thread; the bitmaps are plain GDI objects.
unsafe impl Send for Snip {}

static SNIP: Mutex<Option<Snip>> = Mutex::new(None);

fn snip() -> std::sync::MutexGuard<'static, Option<Snip>> {
    SNIP.lock().unwrap_or_else(|e| e.into_inner())
}

/// A top-down 32-bit bitmap and its pixels.
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

/// The selection, in window pixels (left, top, right, bottom), if there's one worth keeping.
fn selection(s: &Snip) -> Option<RECT> {
    let from = s.from?;
    let r = RECT {
        left: from.x.min(s.to.x).max(0),
        top: from.y.min(s.to.y).max(0),
        right: from.x.max(s.to.x).min(s.w),
        bottom: from.y.max(s.to.y).min(s.h),
    };
    (r.right - r.left > 3 && r.bottom - r.top > 3).then_some(r)
}

/// The whole screen under `p` (window pixels).
fn screen_at(s: &Snip, p: POINT) -> RECT {
    unsafe {
        let at = POINT { x: p.x + s.origin.0, y: p.y + s.origin.1 };
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST), &mut mi);
        let m = mi.rcMonitor;
        RECT {
            left: m.left - s.origin.0,
            top: m.top - s.origin.1,
            right: m.right - s.origin.0,
            bottom: m.bottom - s.origin.1,
        }
    }
}

/// Starts a snip: captures the screens and covers them with the snip window.
pub fn start(instance: HINSTANCE) {
    if snip().is_some() {
        return;
    }
    unsafe {
        let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (w, h) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        let (Some((bright, pixels)), Some((dim, dim_px))) = (dib(w, h), dib(w, h)) else { return };
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let old = SelectObject(mem, bright.into());
        let _ = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT);
        SelectObject(mem, old);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        let _ = GdiFlush();
        for (d, b) in dim_px.iter_mut().zip(pixels.iter_mut()) {
            *b |= 0xFF00_0000;
            *d = 0xFF00_0000 | ((*b >> 1) & 0x007F_7F7F);
        }
        let class = w!("CascadeSnip");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let Ok(window) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("Snip"),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(instance),
            None,
        ) else {
            let _ = DeleteObject(bright.into());
            let _ = DeleteObject(dim.into());
            return;
        };
        *snip() = Some(Snip { window, origin: (x, y), w, h, bright, dim, pixels, from: None, to: POINT::default() });
        let _ = ShowWindow(window, SW_SHOW);
        crate::launcher::force_foreground(window);
        let _ = SetFocus(Some(window));
    }
}

/// Closes the snip window and frees the capture.
unsafe fn finish() {
    let Some(s) = snip().take() else { return };
    let _ = ReleaseCapture();
    let _ = DestroyWindow(s.window);
    let _ = DeleteObject(s.bright.into());
    let _ = DeleteObject(s.dim.into());
}

/// Keeps `r` of the capture: on the clipboard and as a file. Says how it went.
unsafe fn keep(r: RECT) {
    let picture: Option<(Vec<u32>, i32, i32)> = snip().as_ref().map(|s| {
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in r.top..r.bottom {
            let start = (y * s.w + r.left) as usize;
            out.extend_from_slice(&s.pixels[start..start + w as usize]);
        }
        (out, w, h)
    });
    let window = snip().as_ref().map(|s| s.window);
    finish();
    let Some((pixels, w, h)) = picture else { return };
    let copied = to_clipboard(window, &pixels, w, h);
    let saved = save_png(&pixels, w, h);
    config::log(&format!("snip: {w}x{h}, copied: {copied}, saved: {saved:?}"));
    msg::note(match (copied, saved) {
        (true, Some(_)) => "Snip copied, and saved in Pictures\\Screenshots".to_string(),
        (true, None) => "Snip copied".to_string(),
        (false, Some(_)) => "Snip saved in Pictures\\Screenshots".to_string(),
        (false, None) => "The snip couldn't be kept".to_string(),
    });
}

/// Puts the pixels on the clipboard as a picture.
unsafe fn to_clipboard(owner: Option<HWND>, pixels: &[u32], w: i32, h: i32) -> bool {
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w,
        // Bottom-up, the way every program reads one from the clipboard.
        biHeight: h,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: (w * h * 4) as u32,
        ..Default::default()
    };
    let size = std::mem::size_of::<BITMAPINFOHEADER>() + pixels.len() * 4;
    let Ok(global) = GlobalAlloc(GMEM_MOVEABLE, size) else { return false };
    let at = GlobalLock(global) as *mut u8;
    if at.is_null() {
        return false;
    }
    std::ptr::copy_nonoverlapping(
        &header as *const BITMAPINFOHEADER as *const u8,
        at,
        std::mem::size_of::<BITMAPINFOHEADER>(),
    );
    let rows = at.add(std::mem::size_of::<BITMAPINFOHEADER>()) as *mut u32;
    for y in 0..h as usize {
        let src = &pixels[y * w as usize..(y + 1) * w as usize];
        std::ptr::copy_nonoverlapping(src.as_ptr(), rows.add((h as usize - 1 - y) * w as usize), w as usize);
    }
    let _ = GlobalUnlock(global);
    if OpenClipboard(owner).is_err() {
        return false;
    }
    let _ = EmptyClipboard();
    let ok = SetClipboardData(CF_DIB, Some(HANDLE(global.0))).is_ok();
    let _ = CloseClipboard();
    ok
}

/// Saves the pixels as a PNG in the Screenshots folder; where, if it did.
unsafe fn save_png(pixels: &[u32], w: i32, h: i32) -> Option<PathBuf> {
    let dir = match SHGetKnownFolderPath(&FOLDERID_Screenshots, KF_FLAG_DEFAULT, None) {
        Ok(p) => {
            let path = p.to_string().ok().map(PathBuf::from);
            CoTaskMemFree(Some(p.0 as *const c_void));
            path
        }
        Err(_) => None,
    }
    .or_else(|| std::env::var_os("USERPROFILE").map(|u| PathBuf::from(u).join("Pictures\\Screenshots")))?;
    let _ = std::fs::create_dir_all(&dir);
    let t = windows::Win32::System::SystemInformation::GetLocalTime();
    let name = format!(
        "Screenshot {:04}-{:02}-{:02} {:02}{:02}{:02}.png",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    );
    let path = dir.join(name);
    let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
    let stream = wic.CreateStream().ok()?;
    stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0).ok()?;
    let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).ok()?;
    encoder.Initialize(&stream, WICBitmapEncoderNoCache).ok()?;
    let mut frame: Option<IWICBitmapFrameEncode> = None;
    encoder.CreateNewFrame(&mut frame, std::ptr::null_mut()).ok()?;
    let frame = frame?;
    frame.Initialize(None).ok()?;
    frame.SetSize(w as u32, h as u32).ok()?;
    let mut format = GUID_WICPixelFormat32bppBGRA;
    frame.SetPixelFormat(&mut format).ok()?;
    let bytes = std::slice::from_raw_parts(pixels.as_ptr() as *const u8, pixels.len() * 4);
    frame.WritePixels(h as u32, (w * 4) as u32, bytes).ok()?;
    frame.Commit().ok()?;
    encoder.Commit().ok()?;
    Some(path)
}

fn point(lp: LPARAM) -> POINT {
    POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 }
}

/// Redraws the parts that change between `a` and `b` (selections, with their edge).
unsafe fn invalidate(window: HWND, a: Option<RECT>, b: Option<RECT>) {
    for r in [a, b].into_iter().flatten() {
        let grown = RECT { left: r.left - 3, top: r.top - 3, right: r.right + 3, bottom: r.bottom + 3 };
        let _ = InvalidateRect(Some(window), Some(&grown), false);
    }
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            if let Some(s) = snip().as_ref() {
                let mem = CreateCompatibleDC(Some(dc));
                let p = ps.rcPaint;
                let old = SelectObject(mem, s.dim.into());
                let _ =
                    BitBlt(dc, p.left, p.top, p.right - p.left, p.bottom - p.top, Some(mem), p.left, p.top, SRCCOPY);
                if let Some(r) = selection(s) {
                    SelectObject(mem, s.bright.into());
                    let _ = BitBlt(
                        dc,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        Some(mem),
                        r.left,
                        r.top,
                        SRCCOPY,
                    );
                    let edge = CreateSolidBrush(COLORREF(0x00FF_FFFF));
                    let _ = FrameRect(
                        dc,
                        &RECT { left: r.left - 1, top: r.top - 1, right: r.right + 1, bottom: r.bottom + 1 },
                        edge,
                    );
                    let _ = DeleteObject(edge.into());
                }
                SelectObject(mem, old);
                let _ = DeleteDC(mem);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(s) = snip().as_mut() {
                s.from = Some(point(lp));
                s.to = point(lp);
            }
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let mut guard = snip();
            if let Some(s) = guard.as_mut().filter(|s| s.from.is_some()) {
                let before = selection(s);
                s.to = point(lp);
                let after = selection(s);
                drop(guard);
                invalidate(hwnd, before, after);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let chosen = snip().as_mut().map(|s| {
                s.to = point(lp);
                // A click without a drag: the whole screen under the pointer.
                selection(s).unwrap_or_else(|| screen_at(s, point(lp)))
            });
            if let Some(r) = chosen {
                keep(r);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            finish();
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 as u16 == VK_ESCAPE.0 => {
            finish();
            LRESULT(0)
        }
        // Enter: every screen.
        WM_KEYDOWN if wp.0 as u16 == VK_RETURN.0 => {
            let all = snip().as_ref().map(|s| RECT { left: 0, top: 0, right: s.w, bottom: s.h });
            if let Some(r) = all {
                keep(r);
            }
            LRESULT(0)
        }
        // Clicked away (Alt+Tab, another window took the front): leave it.
        WM_ACTIVATE if wp.0 & 0xFFFF == 0 => {
            if snip().as_ref().is_some_and(|s| s.window == hwnd) {
                finish();
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}
