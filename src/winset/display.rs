//! Display: the screens, their resolution, refresh rate, scale, rotation and
//! HDR, how several are used (extend, duplicate, one only), and which is the
//! main one. As Windows' Settings > System > Display, through the same
//! display-configuration calls. A change of mode asks whether to keep it,
//! and goes back by itself after 15 seconds without an answer.

use std::sync::atomic::{AtomicUsize, Ordering};

use windows::core::HSTRING;
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;

use super::{changed, pick as menu};
use crate::config;

/// The screen the page is showing (an index into `screens`).
static SCREEN: AtomicUsize = AtomicUsize::new(0);

/// An active screen.
#[derive(Clone)]
pub struct Screen {
    /// GDI's name for it (\\.\DISPLAY1).
    pub gdi: String,
    /// What it's called ("DELL U2720Q"), or "Screen n".
    pub name: String,
    adapter: LUID,
    source: u32,
    target: u32,
}

fn wide(s: &[u16]) -> String {
    let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..n])
}

/// The active screens, the main one first.
pub fn screens() -> Vec<Screen> {
    let mut out = Vec::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return out;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None)
            != ERROR_SUCCESS
        {
            return out;
        }
        paths.truncate(np as usize);
        for (i, p) in paths.iter().enumerate() {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            let mut tgt = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            let name = if DisplayConfigGetDeviceInfo(&mut tgt.header) == 0 {
                wide(&tgt.monitorFriendlyDeviceName)
            } else {
                String::new()
            };
            let name = if name.trim().is_empty() { format!("Screen {}", i + 1) } else { name };
            out.push(Screen {
                gdi: wide(&src.viewGdiDeviceName),
                name,
                adapter: p.sourceInfo.adapterId,
                source: p.sourceInfo.id,
                target: p.targetInfo.id,
            });
        }
    }
    // The main screen (at 0,0) first.
    out.sort_by_key(|s| {
        let m = current_mode(&s.gdi);
        let (x, y) = m
            .map(|m| unsafe { (m.Anonymous1.Anonymous2.dmPosition.x, m.Anonymous1.Anonymous2.dmPosition.y) })
            .unwrap_or((1, 1));
        !(x == 0 && y == 0)
    });
    out
}

pub fn count() -> usize {
    screens().len()
}

fn chosen() -> Option<Screen> {
    let all = screens();
    let i = SCREEN.load(Ordering::Relaxed).min(all.len().saturating_sub(1));
    all.get(i).cloned()
}

fn current_mode(gdi: &str) -> Option<DEVMODEW> {
    let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
    unsafe { EnumDisplaySettingsW(&HSTRING::from(gdi), ENUM_CURRENT_SETTINGS, &mut dm).as_bool().then_some(dm) }
}

/// Every mode the screen offers: (width, height, refresh rate).
fn modes(gdi: &str) -> Vec<(u32, u32, u32)> {
    let mut out = Vec::new();
    let name = HSTRING::from(gdi);
    for i in 0.. {
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        if !unsafe { EnumDisplaySettingsW(&name, ENUM_DISPLAY_SETTINGS_MODE(i), &mut dm) }.as_bool() {
            break;
        }
        if dm.dmBitsPerPel >= 32 || dm.dmBitsPerPel == 0 {
            let m = (dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency);
            if !out.contains(&m) {
                out.push(m);
            }
        }
    }
    out
}

/// The resolutions offered, biggest first. Rotated sideways, a mode's width
/// and height are the other way round on the screen, but listed as the
/// screen's own.
fn resolutions(gdi: &str) -> Vec<(u32, u32)> {
    let mut r: Vec<(u32, u32)> = modes(gdi).into_iter().map(|(w, h, _)| (w, h)).collect();
    r.sort_by_key(|&(w, h)| std::cmp::Reverse((w as u64 * h as u64, w)));
    r.dedup();
    r
}

/// Rotation: Windows' four, as named in its Settings.
const ORIENTATIONS: [&str; 4] = ["Landscape", "Portrait", "Landscape (flipped)", "Portrait (flipped)"];
/// Scale steps Windows offers, in percent.
const SCALES: [u32; 12] = [100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500];

/// The layout of several screens, as Win+P offers it.
pub const LAYOUTS: [(&str, SET_DISPLAY_CONFIG_FLAGS); 4] = [
    ("Extend these screens", SDC_TOPOLOGY_EXTEND),
    ("Duplicate these screens", SDC_TOPOLOGY_CLONE),
    ("Show only on the main screen", SDC_TOPOLOGY_INTERNAL),
    ("Show only on the second screen", SDC_TOPOLOGY_EXTERNAL),
];

/// The layout in use now (an index into `LAYOUTS`).
pub fn layout() -> Option<usize> {
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        GetDisplayConfigBufferSizes(QDC_DATABASE_CURRENT, &mut np, &mut nm).ok().ok()?;
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        let mut topology = DISPLAYCONFIG_TOPOLOGY_ID(0);
        QueryDisplayConfig(
            QDC_DATABASE_CURRENT,
            &mut np,
            paths.as_mut_ptr(),
            &mut nm,
            modes.as_mut_ptr(),
            Some(&mut topology),
        )
        .ok()
        .ok()?;
        match topology {
            DISPLAYCONFIG_TOPOLOGY_EXTEND => Some(0),
            DISPLAYCONFIG_TOPOLOGY_CLONE => Some(1),
            DISPLAYCONFIG_TOPOLOGY_INTERNAL => Some(2),
            DISPLAYCONFIG_TOPOLOGY_EXTERNAL => Some(3),
            _ => None,
        }
    }
}

/// Uses several screens as `LAYOUTS[i]` says (Win+P).
pub fn set_layout(i: usize) {
    let Some((name, flag)) = LAYOUTS.get(i) else { return };
    let r = unsafe { SetDisplayConfig(None, None, SDC_APPLY | *flag) };
    config::log(&format!("display: {name} (result {r})"));
    STALE.store(true, Ordering::Release);
    changed();
}

// --------------------------------------------------------------- scale --

#[repr(C)]
struct DpiGet {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    min: i32,
    cur: i32,
    max: i32,
}

#[repr(C)]
struct DpiSet {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    rel: i32,
}

/// The scale steps the screen allows, the one in use, and the recommended
/// one (indexes into `SCALES`). Windows has no public call for this; these
/// are the private ones its own Settings page uses (asked for with -3, set
/// with -4), which keep scale relative to the recommended step.
fn scale(s: &Screen) -> Option<(Vec<usize>, usize, usize)> {
    let mut g = DpiGet { header: DISPLAYCONFIG_DEVICE_INFO_HEADER::default(), min: 0, cur: 0, max: 0 };
    g.header.r#type = DISPLAYCONFIG_DEVICE_INFO_TYPE(-3);
    g.header.size = std::mem::size_of::<DpiGet>() as u32;
    g.header.adapterId = s.adapter;
    g.header.id = s.source;
    if unsafe { DisplayConfigGetDeviceInfo(&mut g.header) } != 0 {
        return None;
    }
    let rec = g.min.unsigned_abs() as usize;
    let lo = rec as i32 + g.min;
    let hi = (rec as i32 + g.max).min(SCALES.len() as i32 - 1);
    let cur = (rec as i32 + g.cur).clamp(0, SCALES.len() as i32 - 1) as usize;
    Some(((lo.max(0)..=hi.max(0)).map(|i| i as usize).collect(), cur, rec))
}

fn set_scale(s: &Screen, index: usize, rec: usize) {
    let mut p = DpiSet { header: DISPLAYCONFIG_DEVICE_INFO_HEADER::default(), rel: index as i32 - rec as i32 };
    p.header.r#type = DISPLAYCONFIG_DEVICE_INFO_TYPE(-4);
    p.header.size = std::mem::size_of::<DpiSet>() as u32;
    p.header.adapterId = s.adapter;
    p.header.id = s.source;
    let r = unsafe { DisplayConfigSetDeviceInfo(&p.header) };
    config::log(&format!("display: {} scale {}% (result {r})", s.name, SCALES[index]));
}

// ----------------------------------------------------------------- HDR --

#[repr(C)]
struct ColourInfo {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
    encoding: i32,
    bits: u32,
}

#[repr(C)]
struct ColourSet {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
}

/// Whether the chosen screen can do HDR, and whether it's on. None if it can't.
pub fn hdr() -> Option<(bool, bool)> {
    let s = chosen()?;
    let mut c = ColourInfo { header: DISPLAYCONFIG_DEVICE_INFO_HEADER::default(), value: 0, encoding: 0, bits: 0 };
    c.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO;
    c.header.size = std::mem::size_of::<ColourInfo>() as u32;
    c.header.adapterId = s.adapter;
    c.header.id = s.target;
    if unsafe { DisplayConfigGetDeviceInfo(&mut c.header) } != 0 {
        return None;
    }
    let supported = c.value & 1 != 0;
    supported.then_some((true, c.value & 2 != 0))
}

pub fn set_hdr(on: bool) {
    let Some(s) = chosen() else { return };
    let mut c = ColourSet { header: DISPLAYCONFIG_DEVICE_INFO_HEADER::default(), value: on as u32 };
    c.header.r#type = DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE;
    c.header.size = std::mem::size_of::<ColourSet>() as u32;
    c.header.adapterId = s.adapter;
    c.header.id = s.target;
    let r = unsafe { DisplayConfigSetDeviceInfo(&c.header) };
    config::log(&format!("display: HDR {} on {} (result {r})", if on { "on" } else { "off" }, s.name));
    changed();
}

// --------------------------------------------------------------- modes --

/// Changes the chosen screen's mode, then asks whether to keep it (going
/// back after 15 seconds with no answer), as Windows' Settings does.
fn change_mode(edit: impl FnOnce(&mut DEVMODEW)) {
    let Some(s) = chosen() else { return };
    let Some(before) = current_mode(&s.gdi) else { return };
    let mut dm = before;
    edit(&mut dm);
    std::thread::spawn(move || unsafe {
        let name = HSTRING::from(s.gdi.as_str());
        let r = ChangeDisplaySettingsExW(&name, Some(&dm), None, CDS_UPDATEREGISTRY, None);
        config::log(&format!(
            "display: {} to {}x{} at {} Hz (result {})",
            s.name, dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency, r.0
        ));
        changed();
        if r != DISP_CHANGE_SUCCESSFUL {
            super::tell(format!("Windows wouldn't change {} to that (error {}).", s.name, r.0));
            return;
        }
        if !keep_changes() {
            let mut back = before;
            back.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY | DM_DISPLAYORIENTATION;
            let _ = ChangeDisplaySettingsExW(&name, Some(&back), None, CDS_UPDATEREGISTRY, None);
            config::log("display: not kept; put back");
            changed();
        }
    });
}

/// "Keep these display settings?", answering itself (no) after 15 seconds.
fn keep_changes() -> bool {
    use windows::core::{w, PCSTR, PCWSTR};
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::Win32::UI::WindowsAndMessaging::*;
    type MessageBoxTimeout = unsafe extern "system" fn(HWND, PCWSTR, PCWSTR, u32, u16, u32) -> i32;
    unsafe {
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return true };
        let Some(f) = GetProcAddress(user32, PCSTR(c"MessageBoxTimeoutW".as_ptr() as *const u8)) else { return true };
        let f = std::mem::transmute::<unsafe extern "system" fn() -> isize, MessageBoxTimeout>(f);
        let style = MB_YESNO | MB_ICONQUESTION | MB_TOPMOST | MB_SETFOREGROUND | MB_DEFBUTTON2;
        f(
            HWND::default(),
            w!("Keep these display settings?\n\nThey go back in 15 seconds if you don't say yes."),
            w!("Cascade"),
            style.0,
            0,
            15_000,
        ) == IDYES.0
    }
}

/// Makes the chosen screen the main one: it moves to 0,0 and the others with it.
pub fn make_primary() {
    let Some(s) = chosen() else { return };
    let Some(m) = current_mode(&s.gdi) else { return };
    let (dx, dy) = unsafe { (m.Anonymous1.Anonymous2.dmPosition.x, m.Anonymous1.Anonymous2.dmPosition.y) };
    unsafe {
        for other in screens() {
            let Some(mut dm) = current_mode(&other.gdi) else { continue };
            dm.Anonymous1.Anonymous2.dmPosition.x -= dx;
            dm.Anonymous1.Anonymous2.dmPosition.y -= dy;
            dm.dmFields = DM_POSITION;
            let mut flags = CDS_UPDATEREGISTRY | CDS_NORESET;
            if other.gdi == s.gdi {
                flags |= CDS_SET_PRIMARY;
            }
            let _ = ChangeDisplaySettingsExW(&HSTRING::from(other.gdi.as_str()), Some(&dm), None, flags, None);
        }
        let r = ChangeDisplaySettingsExW(windows::core::PCWSTR::null(), None, None, CDS_TYPE(0), None);
        config::log(&format!("display: {} is now the main screen (result {})", s.name, r.0));
    }
    SCREEN.store(0, Ordering::Relaxed);
    STALE.store(true, Ordering::Release);
    changed();
}

// ------------------------------------------------------------- the page --

fn res_text((w, h): (u32, u32)) -> String {
    format!("{w} \u{d7} {h}")
}

/// What a picker row shows.
pub fn label(key: &str) -> String {
    let Some(s) = chosen() else { return String::new() };
    let m = current_mode(&s.gdi);
    match key {
        "win_screen" => s.name.clone(),
        "win_layout" => layout().map(|i| LAYOUTS[i].0.to_string()).unwrap_or_default(),
        "win_resolution" => m.map(|m| res_text((m.dmPelsWidth, m.dmPelsHeight))).unwrap_or_default(),
        "win_refresh" => m.map(|m| format!("{} Hz", m.dmDisplayFrequency)).unwrap_or_default(),
        "win_scale" => scale(&s)
            .map(
                |(_, cur, rec)| {
                    if cur == rec {
                        format!("{}% (recommended)", SCALES[cur])
                    } else {
                        format!("{}%", SCALES[cur])
                    }
                },
            )
            .unwrap_or_default(),
        "win_orientation" => m
            .map(|m| unsafe { m.Anonymous1.Anonymous2.dmDisplayOrientation.0 as usize })
            .and_then(|o| ORIENTATIONS.get(o))
            .map(|o| o.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// The chosen screen in a line: its mode, and whether it's the main one.
pub fn screen_desc() -> String {
    let all = screens();
    let i = SCREEN.load(Ordering::Relaxed).min(all.len().saturating_sub(1));
    match all.get(i) {
        Some(s) if i == 0 => format!("The settings below are for {}, the main screen.", s.name),
        Some(s) => format!("The settings below are for {}.", s.name),
        None => String::new(),
    }
}

/// A picker row was clicked.
pub fn pick(key: &str, owner: HWND) {
    let Some(s) = chosen() else { return };
    let m = current_mode(&s.gdi);
    match key {
        "win_screen" => {
            let all = screens();
            let names: Vec<String> = all
                .iter()
                .enumerate()
                .map(|(i, s)| if i == 0 { format!("{} (main)", s.name) } else { s.name.clone() })
                .collect();
            if let Some(i) = menu(owner, &names, Some(SCREEN.load(Ordering::Relaxed))) {
                SCREEN.store(i, Ordering::Relaxed);
                changed();
            }
        }
        "win_layout" => {
            let names: Vec<String> = LAYOUTS.iter().map(|(n, _)| n.to_string()).collect();
            if let Some(i) = menu(owner, &names, layout()) {
                set_layout(i);
            }
        }
        "win_resolution" => {
            let list = resolutions(&s.gdi);
            let now = m.map(|m| (m.dmPelsWidth, m.dmPelsHeight));
            let names: Vec<String> = list.iter().map(|r| res_text(*r)).collect();
            let cur = list.iter().position(|r| Some(*r) == now);
            if let Some(i) = menu(owner, &names, cur) {
                let (w, h) = list[i];
                // The fastest refresh rate that resolution has.
                let hz = modes(&s.gdi).iter().filter(|(mw, mh, _)| (*mw, *mh) == (w, h)).map(|m| m.2).max();
                change_mode(|dm| {
                    dm.dmPelsWidth = w;
                    dm.dmPelsHeight = h;
                    dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
                    if let Some(hz) = hz {
                        dm.dmDisplayFrequency = hz;
                        dm.dmFields |= DM_DISPLAYFREQUENCY;
                    }
                });
            }
        }
        "win_refresh" => {
            let Some(now) = m else { return };
            let mut rates: Vec<u32> = modes(&s.gdi)
                .into_iter()
                .filter(|(w, h, _)| (*w, *h) == (now.dmPelsWidth, now.dmPelsHeight))
                .map(|m| m.2)
                .collect();
            rates.sort_unstable_by(|a, b| b.cmp(a));
            rates.dedup();
            let names: Vec<String> = rates.iter().map(|r| format!("{r} Hz")).collect();
            let cur = rates.iter().position(|r| *r == now.dmDisplayFrequency);
            if let Some(i) = menu(owner, &names, cur) {
                let hz = rates[i];
                change_mode(|dm| {
                    dm.dmDisplayFrequency = hz;
                    dm.dmFields = DM_DISPLAYFREQUENCY;
                });
            }
        }
        "win_scale" => {
            let Some((steps, cur, rec)) = scale(&s) else { return };
            let names: Vec<String> = steps
                .iter()
                .map(|&i| if i == rec { format!("{}% (recommended)", SCALES[i]) } else { format!("{}%", SCALES[i]) })
                .collect();
            if let Some(i) = menu(owner, &names, steps.iter().position(|&x| x == cur)) {
                set_scale(&s, steps[i], rec);
                changed();
            }
        }
        "win_orientation" => {
            let Some(now) = m else { return };
            let cur = unsafe { now.Anonymous1.Anonymous2.dmDisplayOrientation.0 as usize };
            let names: Vec<String> = ORIENTATIONS.iter().map(|s| s.to_string()).collect();
            if let Some(i) = menu(owner, &names, Some(cur)) {
                if i == cur {
                    return;
                }
                change_mode(|dm| {
                    // A quarter turn swaps width and height.
                    if (i % 2) != (cur % 2) {
                        std::mem::swap(&mut dm.dmPelsWidth, &mut dm.dmPelsHeight);
                    }
                    dm.Anonymous1.Anonymous2.dmDisplayOrientation = DEVMODE_DISPLAY_ORIENTATION(i as u32);
                    dm.dmFields = DM_DISPLAYORIENTATION | DM_PELSWIDTH | DM_PELSHEIGHT;
                });
            }
        }
        _ => {}
    }
}

// ------------------------------------------------------------ arranging --

/// Where each screen sits on the desktop now (desktop pixels), in
/// `screens` order (the main one first), and which one the page is on.
/// Read at most once a second (the page asks on every frame).
pub fn arrangement() -> (Vec<(String, RECT)>, usize) {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    type Arrangement = Vec<(String, RECT)>;
    static CACHE: Mutex<Option<(Instant, Arrangement)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let fresh =
        c.as_ref().is_some_and(|(t, _)| t.elapsed() < Duration::from_secs(1)) && !STALE.swap(false, Ordering::AcqRel);
    if !fresh {
        let list = screens()
            .into_iter()
            .filter_map(|s| {
                let m = current_mode(&s.gdi)?;
                let (x, y) = unsafe { (m.Anonymous1.Anonymous2.dmPosition.x, m.Anonymous1.Anonymous2.dmPosition.y) };
                let r = RECT { left: x, top: y, right: x + m.dmPelsWidth as i32, bottom: y + m.dmPelsHeight as i32 };
                Some((s.name, r))
            })
            .collect();
        *c = Some((Instant::now(), list));
    }
    let list = c.as_ref().map(|(_, l)| l.clone()).unwrap_or_default();
    let i = SCREEN.load(Ordering::Relaxed).min(list.len().saturating_sub(1));
    (list, i)
}

/// Set when Cascade moved the screens: the next `arrangement` reads them again.
static STALE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The page now shows screen `i` (a click on it in the arrangement).
pub fn select(i: usize) {
    SCREEN.store(i, Ordering::Relaxed);
    changed();
}

fn overlaps(a: &RECT, b: &RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// Where screen `i` lands when dropped at `left, top`: touching another
/// screen along an edge (Windows needs them joined), lined up with its edge
/// when that's close, overlapping none. None if there's nowhere near.
pub fn snap(rects: &[RECT], i: usize, left: i32, top: i32) -> Option<(i32, i32)> {
    let me = rects.get(i)?;
    let (w, h) = (me.right - me.left, me.bottom - me.top);
    // Lining up edges within this far (a tenth of the screen).
    let (near_x, near_y) = (w / 10, h / 10);
    let align = |v: i32, targets: [i32; 3], near: i32| {
        targets.into_iter().filter(|t| (v - t).abs() <= near).min_by_key(|t| (v - t).abs()).unwrap_or(v)
    };
    let mut best: Option<((i32, i32), i64)> = None;
    for (j, o) in rects.iter().enumerate() {
        if j == i {
            continue;
        }
        // Beside it (left or right), sliding along its side; above or below it, along its top or bottom.
        let ys =
            align(top.clamp(o.top - h + 1, o.bottom - 1), [o.top, o.bottom - h, (o.top + o.bottom - h) / 2], near_y);
        let xs =
            align(left.clamp(o.left - w + 1, o.right - 1), [o.left, o.right - w, (o.left + o.right - w) / 2], near_x);
        for (x, y) in [(o.left - w, ys), (o.right, ys), (xs, o.top - h), (xs, o.bottom)] {
            let r = RECT { left: x, top: y, right: x + w, bottom: y + h };
            if rects.iter().enumerate().any(|(k, other)| k != i && overlaps(&r, other)) {
                continue;
            }
            let d = (x - left) as i64 * (x - left) as i64 + (y - top) as i64 * (y - top) as i64;
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some(((x, y), d));
            }
        }
    }
    best.map(|(p, _)| p)
}

/// Moves screen `i` to `left, top` (desktop pixels, snapped first), the
/// others staying where they are; the main screen stays at 0,0, as Windows
/// needs, so moving it moves the rest the other way.
pub fn move_screen(i: usize, left: i32, top: i32) {
    let all = screens();
    let mut rects: Vec<RECT> = arrangement().0.into_iter().map(|(_, r)| r).collect();
    if rects.len() != all.len() || i >= rects.len() {
        return;
    }
    let Some((x, y)) = snap(&rects, i, left, top) else { return };
    let r = &mut rects[i];
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    *r = RECT { left: x, top: y, right: x + w, bottom: y + h };
    let (ox, oy) = (rects[0].left, rects[0].top);
    unsafe {
        for (s, r) in all.iter().zip(&rects) {
            let Some(mut dm) = current_mode(&s.gdi) else { continue };
            dm.Anonymous1.Anonymous2.dmPosition.x = r.left - ox;
            dm.Anonymous1.Anonymous2.dmPosition.y = r.top - oy;
            dm.dmFields = DM_POSITION;
            let _ = ChangeDisplaySettingsExW(
                &HSTRING::from(s.gdi.as_str()),
                Some(&dm),
                None,
                CDS_UPDATEREGISTRY | CDS_NORESET,
                None,
            );
        }
        let r = ChangeDisplaySettingsExW(windows::core::PCWSTR::null(), None, None, CDS_TYPE(0), None);
        config::log(&format!("display: moved {} to {}, {} (result {})", all[i].name, x - ox, y - oy, r.0));
    }
    STALE.store(true, Ordering::Release);
    changed();
}

/// Identify: each screen's number, big, in its middle for a few seconds.
pub fn identify() {
    use windows::core::w;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe extern "system" fn proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match m {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(h, &mut ps);
                let mut rc = RECT::default();
                let _ = GetClientRect(h, &mut rc);
                let bg = CreateSolidBrush(COLORREF(0x00302820));
                FillRect(dc, &rc, bg);
                let _ = DeleteObject(bg.into());
                let mut buf = [0u16; 8];
                let n = GetWindowTextW(h, &mut buf) as usize;
                let font = CreateFontW(
                    rc.bottom * 3 / 4,
                    0,
                    0,
                    0,
                    600,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    0,
                    w!("Segoe UI Variable Display"),
                );
                let old = SelectObject(dc, font.into());
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, COLORREF(0x00FFFFFF));
                DrawTextW(dc, &mut buf[..n], &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                SelectObject(dc, old);
                let _ = DeleteObject(font.into());
                let _ = EndPaint(h, &ps);
                LRESULT(0)
            }
            WM_TIMER | WM_LBUTTONDOWN => {
                let _ = DestroyWindow(h);
                LRESULT(0)
            }
            _ => DefWindowProcW(h, m, wp, lp),
        }
    }
    let (list, _) = arrangement();
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let instance = HINSTANCE(module.0);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: w!("CascadeIdentify"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        for (n, (_, r)) in list.iter().enumerate() {
            let size = ((r.bottom - r.top) / 4).clamp(120, 360);
            let (x, y) = (r.left + 40, r.bottom - size - 80);
            let Ok(h) = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("CascadeIdentify"),
                &HSTRING::from((n + 1).to_string()),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                size,
                size,
                None,
                None,
                Some(instance),
                None,
            ) else {
                continue;
            };
            let rgn = CreateRoundRectRgn(0, 0, size + 1, size + 1, size / 6, size / 6);
            SetWindowRgn(h, Some(rgn), true);
            SetTimer(Some(h), 1, 3000, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(l: i32, t: i32, w: i32, h: i32) -> RECT {
        RECT { left: l, top: t, right: l + w, bottom: t + h }
    }

    #[test]
    fn snaps_beside_and_lines_up() {
        let rects = [r(0, 0, 1920, 1080), r(1920, 0, 1920, 1080)];
        // Dragged a little up and away to the left: lands left of the main screen, tops lined up.
        assert_eq!(snap(&rects, 1, -1980, -60), Some((-1920, 0)));
        // Dropped on top of the main screen: pushed out beside it, never overlapping.
        let (x, y) = snap(&rects, 1, 300, 50).unwrap();
        assert!(!overlaps(&r(x, y, 1920, 1080), &rects[0]));
        // Above it, centred when close to centred.
        assert_eq!(snap(&rects, 1, 30, -1100), Some((0, -1080)));
    }
}
