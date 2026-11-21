//! The picture on Start's account button: one picked in Settings (Start
//! menu > Profile picture), or else the Windows account's own picture, cut
//! to a circle. Without either, Start shows the name's first letter.

use std::path::PathBuf;

use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The signed-in user's SID, as text ("S-1-5-21-...").
fn user_sid() -> Option<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut need = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut need);
        let mut buf = vec![0u8; need.max(64) as usize];
        let got = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as *mut _), need, &mut need);
        let _ = CloseHandle(token);
        got.ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let sid = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0 as *mut _)));
        sid
    }
}

/// Where Windows keeps the account's picture (the biggest it lists).
fn windows_picture() -> Option<PathBuf> {
    let sid = user_sid()?;
    let key = HSTRING::from(format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\AccountPicture\\Users\\{sid}"));
    for name in ["Image448", "Image240", "Image192", "Image96"] {
        let mut buf = [0u16; 520];
        let mut size = (buf.len() * 2) as u32;
        let ok = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &key,
                &HSTRING::from(name),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut size),
            )
            .is_ok()
        };
        if ok {
            let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
            let path = PathBuf::from(String::from_utf16_lossy(&buf[..n]));
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

/// The picture to show: `chosen` (the config's `profile_picture`) if it's
/// there, else the Windows account's.
pub fn source(chosen: &str) -> Option<PathBuf> {
    let chosen = chosen.trim();
    if !chosen.is_empty() && std::path::Path::new(chosen).exists() {
        return Some(PathBuf::from(chosen));
    }
    windows_picture()
}

/// The picture at `path` as a `size`×`size` circle: premultiplied BGRA,
/// cropped from the middle to a square and smoothly scaled.
pub fn circle(path: &std::path::Path, size: u32) -> Option<Vec<u8>> {
    let (px, w, h) = crate::viewer::load_pixels(path)?;
    if w == 0 || h == 0 || size == 0 {
        return None;
    }
    let side = w.min(h) as f32;
    let (ox, oy) = ((w as f32 - side) / 2.0, (h as f32 - side) / 2.0);
    let n = size as usize;
    let mut out = vec![0u8; n * n * 4];
    let step = side / size as f32;
    // Each output pixel averages the source pixels under it (a box filter),
    // so a big photo shrinks smoothly.
    let taps = (step.ceil() as usize).clamp(1, 8);
    for y in 0..n {
        for x in 0..n {
            let mut acc = [0f32; 4];
            for ty in 0..taps {
                for tx in 0..taps {
                    let sx = (ox + (x as f32 + (tx as f32 + 0.5) / taps as f32) * step).min(w as f32 - 1.0) as usize;
                    let sy = (oy + (y as f32 + (ty as f32 + 0.5) / taps as f32) * step).min(h as f32 - 1.0) as usize;
                    let s = (sy * w as usize + sx) * 4;
                    for c in 0..4 {
                        acc[c] += px[s + c] as f32;
                    }
                }
            }
            let k = (taps * taps) as f32;
            // The circle's edge, smoothed over a pixel.
            let r = n as f32 / 2.0;
            let d = ((x as f32 + 0.5 - r).hypot(y as f32 + 0.5 - r) - (r - 0.5)).clamp(0.0, 1.0);
            let cover = 1.0 - d;
            let o = (y * n + x) * 4;
            for c in 0..4 {
                out[o + c] = (acc[c] / k * cover).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Some(out)
}
