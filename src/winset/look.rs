//! Personalisation: light or dark mode (for apps, and for Windows itself),
//! transparency effects, the accent colour (Windows' own 48) and whether title
//! bars show it, and the wallpaper. As Windows' Settings > Personalisation >
//! Colours and Background: the same registry settings, then every window is
//! told, so apps follow straight away.

use windows::core::{w, PCSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::HKEY_CURRENT_USER;

use super::{broadcast, reg_dword, set_reg_binary, set_reg_dword};
use crate::config;

const PERSONALIZE: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
const DWM: &str = "Software\\Microsoft\\Windows\\DWM";
const ACCENT: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent";

pub fn flag(key: &str) -> bool {
    let hk = HKEY_CURRENT_USER;
    match key {
        "win_dark_apps" => reg_dword(hk, PERSONALIZE, "AppsUseLightTheme") == Some(0),
        "win_dark_windows" => reg_dword(hk, PERSONALIZE, "SystemUsesLightTheme") == Some(0),
        "win_transparency" => reg_dword(hk, PERSONALIZE, "EnableTransparency") != Some(0),
        "win_accent_bars" => reg_dword(hk, DWM, "ColorPrevalence") == Some(1),
        _ => false,
    }
}

pub fn set_flag(key: &str, on: bool) {
    let hk = HKEY_CURRENT_USER;
    match key {
        "win_dark_apps" => set_reg_dword(hk, PERSONALIZE, "AppsUseLightTheme", !on as u32),
        "win_dark_windows" => set_reg_dword(hk, PERSONALIZE, "SystemUsesLightTheme", !on as u32),
        "win_transparency" => set_reg_dword(hk, PERSONALIZE, "EnableTransparency", on as u32),
        "win_accent_bars" => set_reg_dword(hk, DWM, "ColorPrevalence", on as u32),
        _ => return,
    };
    broadcast("ImmersiveColorSet");
    if key == "win_accent_bars" {
        refresh_dwm(current_accent().unwrap_or(0x0078D4));
    }
    super::changed();
}

/// The accent colour as RGB, from Windows' setting (kept as ABGR).
fn current_accent() -> Option<u32> {
    let abgr = reg_dword(HKEY_CURRENT_USER, DWM, "AccentColor")?;
    Some(((abgr & 0xFF) << 16) | (abgr & 0xFF00) | ((abgr >> 16) & 0xFF))
}

/// `rgb` lightened (`k` > 0) or darkened (`k` < 0) by a share.
fn shade(rgb: u32, k: f32) -> u32 {
    let ch = |s: u32| -> u32 {
        let c = ((rgb >> s) & 0xFF) as f32;
        let v = if k >= 0.0 { c + (255.0 - c) * k } else { c * (1.0 + k) };
        (v.round().clamp(0.0, 255.0) as u32) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// RGB as Windows keeps it in these settings: ABGR, opaque.
fn abgr(rgb: u32) -> u32 {
    0xFF00_0000 | ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF)
}

/// The eight shades Windows keeps for an accent (light 3 to dark 3, as it
/// lays them out), each as R, G, B, A.
fn palette_bytes(rgb: u32) -> [u8; 32] {
    let shades = [0.6, 0.4, 0.2, 0.0, -0.25, -0.45, -0.65, -0.3];
    let mut out = [0u8; 32];
    for (i, k) in shades.iter().enumerate() {
        let c = shade(rgb, *k);
        out[i * 4] = (c >> 16) as u8;
        out[i * 4 + 1] = (c >> 8) as u8;
        out[i * 4 + 2] = c as u8;
        out[i * 4 + 3] = 0xFF;
    }
    out
}

/// Windows' accent colour as #RRGGBB (empty if it has none set).
pub fn accent_hex() -> String {
    current_accent().map(|rgb| format!("#{rgb:06X}")).unwrap_or_default()
}

/// Makes `rgb` the accent colour, everywhere Windows keeps it.
pub fn set_accent(rgb: u32) {
    let hk = HKEY_CURRENT_USER;
    set_reg_binary(hk, ACCENT, "AccentPalette", &palette_bytes(rgb));
    set_reg_dword(hk, ACCENT, "AccentColorMenu", abgr(shade(rgb, -0.25)));
    set_reg_dword(hk, ACCENT, "StartColorMenu", abgr(shade(rgb, -0.45)));
    set_reg_dword(hk, DWM, "AccentColor", abgr(rgb));
    let argb = 0xC400_0000 | rgb;
    set_reg_dword(hk, DWM, "ColorizationColor", argb);
    set_reg_dword(hk, DWM, "ColorizationAfterglow", argb);
    refresh_dwm(rgb);
    broadcast("ImmersiveColorSet");
    config::log(&format!("look: accent colour #{rgb:06X}"));
    super::changed();
}

/// Tells the window manager the colour now (title bars and borders), the
/// way Windows' Settings does: a private call in dwmapi (number 131).
fn refresh_dwm(rgb: u32) {
    #[repr(C)]
    struct Params {
        color: u32,
        afterglow: u32,
        color_balance: u32,
        afterglow_balance: u32,
        blur_balance: u32,
        glass_reflection: u32,
        opaque_blend: u32,
    }
    type SetParams = unsafe extern "system" fn(*const Params, u32) -> i32;
    unsafe {
        let Ok(dwm) = LoadLibraryW(w!("dwmapi.dll")) else { return };
        let Some(f) = GetProcAddress(dwm, PCSTR(std::ptr::without_provenance(131))) else { return };
        let f = std::mem::transmute::<unsafe extern "system" fn() -> isize, SetParams>(f);
        let argb = 0xC400_0000 | rgb;
        let p = Params {
            color: argb,
            afterglow: argb,
            color_balance: 0x59,
            afterglow_balance: 0x0A,
            blur_balance: 0x57,
            glass_reflection: 0x32,
            opaque_blend: 0,
        };
        f(&p, 0);
    }
}

pub fn fit_label() -> String {
    let f = crate::wallpaper::fit();
    crate::wallpaper::Fit::ALL.iter().find(|(x, _)| *x == f).map(|(_, n)| n.to_string()).unwrap_or_default()
}

pub fn pick_fit(owner: HWND) {
    use crate::wallpaper::Fit;
    let names: Vec<String> = Fit::ALL.iter().map(|(_, n)| n.to_string()).collect();
    let cur = Fit::ALL.iter().position(|(f, _)| *f == crate::wallpaper::fit());
    if let Some(i) = super::pick(owner, &names, cur) {
        crate::wallpaper::set_fit(Fit::ALL[i].0);
        // The desktop redraws with it (it hears desktop-setting changes).
        broadcast("Control Panel\\Desktop");
        super::changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_shades_and_order() {
        assert_eq!(abgr(0x0078D4), 0xFFD47800);
        assert_eq!(shade(0x808080, 0.0), 0x808080);
        assert_eq!(shade(0x000000, 1.0), 0xFFFFFF);
        assert_eq!(shade(0xFFFFFF, -1.0), 0x000000);
        let p = palette_bytes(0x0078D4);
        // The accent itself sits fourth, as R, G, B, A.
        assert_eq!(&p[12..16], &[0x00, 0x78, 0xD4, 0xFF]);
    }
}
