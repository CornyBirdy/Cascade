//! App icons and names for the dock, loaded off the UI thread through the
//! shell (works for normal .exe apps and Store apps alike).

use std::sync::mpsc::{channel, Sender};

use windows::core::{Interface, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{DeleteObject, HPALETTE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, FILE_FLAGS_AND_ATTRIBUTES,
};
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

use crate::msg;

pub const ICON_SIZE: u32 = 96;

/// System.Link.TargetParsingPath: the program behind a Start menu entry.
const PKEY_LINK_TARGET: PROPERTYKEY =
    PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0xB9B4B3FC_2B51_4A42_B5D8_324146AFCF25), pid: 2 };

#[derive(Clone)]
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
        // Cascade's own exe is drawn from its embedded icon: the shell's icon
        // cache keeps showing the old picture after an update swaps the file.
        let own = std::env::current_exe().ok().map(|p| p.to_string_lossy().to_lowercase());
        while let Ok(id) = rx.recv() {
            let item: Option<IShellItem> = SHCreateItemFromParsingName(&HSTRING::from(id.as_str()), None).ok();
            let is_own = own.as_deref() == Some(id.to_lowercase().as_str());
            let pixels = match (&item, &wic) {
                (_, Some(wic)) if is_own => own_icon(wic).or_else(|_| load_icon(item.as_ref().ok_or(E_FAIL)?, wic)),
                (Some(item), Some(wic)) => small_icon(item, wic).map(Ok).unwrap_or_else(|| load_icon(item, wic)),
                _ => Ok(Vec::new()),
            }
            .unwrap_or_default();
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
    let result = wic
        .CreateBitmapFromHBITMAP(hbmp, HPALETTE::default(), WICBitmapUsePremultipliedAlpha)
        .and_then(|bmp| to_pixels(wic, &bmp));
    let _ = DeleteObject(hbmp.into());
    result
}

/// An icon drawn only in small sizes (older apps, like 7-Zip's), scaled up
/// to fill the slot. Asked for big, the shell shows these small inside a
/// frame, which looks like an outline round the app. None for icons that
/// come in big sizes (or whose file can't be read): the shell draws those.
unsafe fn small_icon(item: &IShellItem, wic: &IWICImagingFactory) -> Option<Vec<u8>> {
    for (file, index) in icon_locations(item) {
        let Some(largest) = largest_icon(&file, index) else { continue };
        if largest >= ICON_SIZE {
            return None;
        }
        let mut icon = HICON::default();
        SHDefExtractIconW(&HSTRING::from(file.as_str()), index, 0, Some(&mut icon as *mut _), None, largest)
            .ok()
            .ok()?;
        let result = wic.CreateBitmapFromHICON(icon).and_then(|bmp| to_pixels(wic, &bmp));
        let _ = DestroyIcon(icon);
        return result.ok();
    }
    None
}

/// Where an item's icon may come from, best first: (file, index as for
/// ExtractIcon). The shell's own answer, then a Start entry's program.
unsafe fn icon_locations(item: &IShellItem) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    if let Ok(pidl) = SHGetIDListFromObject(item) {
        let mut info = SHFILEINFOW::default();
        let found = SHGetFileInfoW(
            PCWSTR(pidl as *const u16),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info as *mut _),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICONLOCATION | SHGFI_PIDL,
        );
        CoTaskMemFree(Some(pidl as *const _));
        let name = &info.szDisplayName;
        let file = String::from_utf16_lossy(&name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())]);
        if found != 0 && !file.is_empty() {
            out.push((file, info.iIcon));
        }
    }
    if let Ok(item) = item.cast::<IShellItem2>() {
        if let Ok(p) = item.GetString(&PKEY_LINK_TARGET) {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            if !s.is_empty() {
                out.push((s, 0));
            }
        }
    }
    out
}

/// The largest image (in pixels, 256 for "256 or more") in the icon at
/// `index` in an .exe, .dll or .ico file. `index` counts icons as
/// ExtractIcon does: from 0 in resource order, or minus a resource id.
fn largest_icon(path: &str, index: i32) -> Option<u32> {
    largest_in(path, index).or_else(|| {
        // Windows' own DLLs keep their icons in SystemResources\<name>.mun.
        let name = path.rsplit(['\\', '/']).next()?;
        let windir = std::env::var("WINDIR").ok()?;
        largest_in(&format!(r"{windir}\SystemResources\{name}.mun"), index)
    })
}

fn largest_in(path: &str, index: i32) -> Option<u32> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut read = |at: u64, len: usize| -> Option<Vec<u8>> {
        let mut b = vec![0u8; len];
        f.seek(SeekFrom::Start(at)).ok()?;
        f.read_exact(&mut b).ok()?;
        Some(b)
    };
    let u16_at = |b: &[u8], i: usize| Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?));
    let u32_at = |b: &[u8], i: usize| Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?));
    // An icon directory: a count at 4, then entries starting with the width.
    let widest = |dir: &[u8], entry: usize| {
        let count = u16_at(dir, 4)? as usize;
        (0..count).map(|i| dir.get(6 + i * entry).map(|&w| if w == 0 { 256 } else { w as u32 })).max()?
    };
    let head = read(0, 64)?;
    if path.to_ascii_lowercase().ends_with(".ico") {
        let count = u16_at(&head, 4)? as usize;
        return widest(&read(0, 6 + count * 16)?, 16);
    }
    if head.get(..2)? != b"MZ" {
        return None;
    }
    // PE headers: where the resources are, then the section holding them.
    let pe = u32_at(&head, 0x3c)? as u64;
    let nt = read(pe, 24)?;
    if nt.get(..4)? != b"PE\0\0" {
        return None;
    }
    let sections = u16_at(&nt, 6)? as usize;
    let opt_len = u16_at(&nt, 20)? as usize;
    let opt = read(pe + 24, opt_len)?;
    let dirs = match u16_at(&opt, 0)? {
        0x10b => 96,
        0x20b => 112,
        _ => return None,
    };
    let (res_rva, res_len) = (u32_at(&opt, dirs + 16)?, u32_at(&opt, dirs + 20)?);
    if res_rva == 0 || res_len == 0 || res_len > 64 << 20 {
        return None;
    }
    let table = read(pe + 24 + opt_len as u64, sections * 40)?;
    // (file offset, bytes of it stored in the file) for the resources.
    let (file_at, stored) = table.as_chunks::<40>().0.iter().find_map(|s| {
        let s = s.as_slice();
        let (va, size, raw_len, raw) = (u32_at(s, 12)?, u32_at(s, 8)?, u32_at(s, 16)?, u32_at(s, 20)?);
        let into = res_rva.checked_sub(va)?;
        (into < size.max(raw_len)).then(|| ((raw + into) as u64, raw_len.saturating_sub(into)))
    })?;
    let res = read(file_at, res_len.min(stored) as usize)?;
    // A resource directory's entries, named ones first: (name or id, offset).
    let entries = |at: usize| -> Option<Vec<(u32, u32)>> {
        let n = u16_at(&res, at + 12)? as usize + u16_at(&res, at + 14)? as usize;
        (0..n).map(|i| Some((u32_at(&res, at + 16 + i * 8)?, u32_at(&res, at + 20 + i * 8)?))).collect()
    };
    const SUBDIR: u32 = 0x8000_0000;
    const RT_GROUP_ICON: u32 = 14;
    let (_, groups) = entries(0)?.into_iter().find(|&(id, off)| id == RT_GROUP_ICON && off & SUBDIR != 0)?;
    let groups = entries((groups & !SUBDIR) as usize)?;
    let (_, mut off) = if index >= 0 {
        *groups.get(index as usize)?
    } else {
        *groups.iter().find(|&&(id, _)| id == index.unsigned_abs())?
    };
    // Down through the group's first language to its data.
    for _ in 0..4 {
        if off & SUBDIR == 0 {
            break;
        }
        off = entries((off & !SUBDIR) as usize)?.first()?.1;
    }
    let (data_rva, data_len) = (u32_at(&res, off as usize)?, u32_at(&res, off as usize + 4)? as usize);
    let at = data_rva.checked_sub(res_rva)? as usize;
    widest(res.get(at..at.checked_add(data_len)?)?, 14)
}

/// Cascade's own icon, straight from the exe's resources.
unsafe fn own_icon(wic: &IWICImagingFactory) -> Result<Vec<u8>> {
    let icon = crate::shell::app_icon(ICON_SIZE as i32).ok_or(E_FAIL)?;
    let result = wic.CreateBitmapFromHICON(icon).and_then(|bmp| to_pixels(wic, &bmp));
    let _ = DestroyIcon(icon);
    result
}

/// Scales a bitmap to ICON_SIZE square, premultiplied BGRA.
unsafe fn to_pixels(wic: &IWICImagingFactory, bmp: &IWICBitmap) -> Result<Vec<u8>> {
    let (mut w, mut h) = (0u32, 0u32);
    bmp.GetSize(&mut w, &mut h)?;
    // Fant shrinks best; small icons grow smoothly with a cubic.
    let mode = if w < ICON_SIZE { WICBitmapInterpolationModeHighQualityCubic } else { WICBitmapInterpolationModeFant };
    let scaler = wic.CreateBitmapScaler()?;
    scaler.Initialize(bmp, ICON_SIZE, ICON_SIZE, mode)?;
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

/// An exe's "File description" ("Google Chrome"), if it has one.
pub fn exe_description(path: &str) -> Option<String> {
    unsafe { file_description(path) }
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

#[cfg(test)]
mod tests {
    use super::largest_icon;

    #[test]
    fn reads_icon_sizes_from_a_dll() {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        // Its icons live in SystemResources\shell32.dll.mun on current Windows.
        assert_eq!(largest_icon(&format!(r"{windir}\System32\shell32.dll"), 0), Some(256));
        assert_eq!(largest_icon(&format!(r"{windir}\System32\shell32.dll"), 100_000), None);
    }
}
