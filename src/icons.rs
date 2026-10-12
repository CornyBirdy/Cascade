//! App icons and names for the dock, loaded off the UI thread through the
//! shell (works for normal .exe apps and Store apps alike).

use std::sync::mpsc::{channel, Sender};

use windows::core::{Interface, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{DeleteObject, HPALETTE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;

use crate::msg;

pub const ICON_SIZE: u32 = 96;

pub struct IconResult {
    pub id: String,
    pub name: String,
    /// Premultiplied BGRA, ICON_SIZE square. Empty if the icon failed.
    pub pixels: Vec<u8>,
}

pub fn spawn() -> Sender<String> {
    let (tx, rx) = channel::<String>();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let wic: Option<IWICImagingFactory> =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok();
        while let Ok(id) = rx.recv() {
            let item: Option<IShellItem> = SHCreateItemFromParsingName(&HSTRING::from(id.as_str()), None).ok();
            let pixels = match (&item, &wic) {
                (Some(item), Some(wic)) => load_icon(item, wic).unwrap_or_default(),
                _ => Vec::new(),
            };
            let name = display_name(&id, item.as_ref());
            let ptr = Box::into_raw(Box::new(IconResult { id, name, pixels }));
            if !msg::post(msg::ICON, 0, ptr as isize) {
                drop(Box::from_raw(ptr));
            }
        }
    });
    tx
}

unsafe fn load_icon(item: &IShellItem, wic: &IWICImagingFactory) -> Result<Vec<u8>> {
    let factory: IShellItemImageFactory = item.cast()?;
    let size = SIZE { cx: ICON_SIZE as i32, cy: ICON_SIZE as i32 };
    let hbmp = factory
        .GetImage(size, SIIGBF(SIIGBF_ICONONLY.0 | SIIGBF_BIGGERSIZEOK.0))
        .or_else(|_| factory.GetImage(size, SIIGBF_BIGGERSIZEOK))?;
    let result = (|| {
        let bmp = wic.CreateBitmapFromHBITMAP(hbmp, HPALETTE::default(), WICBitmapUsePremultipliedAlpha)?;
        let scaler = wic.CreateBitmapScaler()?;
        scaler.Initialize(&bmp, ICON_SIZE, ICON_SIZE, WICBitmapInterpolationModeFant)?;
        let conv = wic.CreateFormatConverter()?;
        conv.Initialize(
            &scaler,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let mut px = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
        conv.CopyPixels(std::ptr::null(), ICON_SIZE * 4, &mut px)?;
        Ok(px)
    })();
    let _ = DeleteObject(hbmp.into());
    result
}

/// The app's friendly name: an exe's "File description", else the shell's name.
unsafe fn display_name(id: &str, item: Option<&IShellItem>) -> String {
    if id.to_ascii_lowercase().ends_with(".exe") {
        if let Some(d) = file_description(id) {
            return d;
        }
    }
    if let Some(item) = item {
        if let Ok(p) = item.GetDisplayName(SIGDN_NORMALDISPLAY) {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            if !s.is_empty() {
                return s.trim_end_matches(".exe").to_string();
            }
        }
    }
    id.rsplit(['\\', '/']).next().unwrap_or(id).trim_end_matches(".exe").to_string()
}

unsafe fn file_description(path: &str) -> Option<String> {
    let path = HSTRING::from(path);
    let size = GetFileVersionInfoSizeW(&path, None);
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    GetFileVersionInfoW(&path, None, size, buf.as_mut_ptr() as *mut _).ok()?;
    let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    let query = |q: &str, ptr: &mut *mut std::ffi::c_void, len: &mut u32| {
        VerQueryValueW(buf.as_ptr() as *const _, &HSTRING::from(q), ptr, len).as_bool()
    };
    let mut langs: Vec<String> = Vec::new();
    if query("\\VarFileInfo\\Translation", &mut ptr, &mut len) && len >= 4 {
        let pairs = std::slice::from_raw_parts(ptr as *const u16, len as usize / 2);
        for p in pairs.chunks(2) {
            if let [lang, cp] = p {
                langs.push(format!("{lang:04x}{cp:04x}"));
            }
        }
    }
    langs.extend(["040904b0".to_string(), "040904e4".to_string()]);
    for l in langs {
        if query(&format!("\\StringFileInfo\\{l}\\FileDescription"), &mut ptr, &mut len) && len > 1 {
            let s = PCWSTR(ptr as *const u16).to_string().ok()?;
            let s = s.trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}
