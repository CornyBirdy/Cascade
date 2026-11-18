//! Night light, Cascade's own: Windows keeps its own in a private format
//! that recent updates changed, and that no other program can switch. This
//! warms the screens instead by bending each one's colour response (its
//! gamma ramp), as night-light programs have long done: on now, or on a
//! schedule, at a chosen strength. Screens in HDR keep their colours
//! (Windows doesn't apply gamma ramps to them).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{CreateDCW, DeleteDC};
use windows::Win32::UI::ColorSystem::SetDeviceGammaRamp;

use crate::config;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Night {
    /// On (now, or on the schedule).
    pub on: bool,
    /// How warm, 0.1 to 1.
    pub strength: f32,
    /// None: on all the time. Else the hours it's on from and to.
    pub hours: Option<(u32, u32)>,
}

impl Default for Night {
    fn default() -> Self {
        Self { on: false, strength: 0.5, hours: None }
    }
}

/// The schedules offered: (from, to) in hours, or always.
pub const SCHEDULES: [Option<(u32, u32)>; 6] =
    [None, Some((19, 7)), Some((20, 7)), Some((21, 7)), Some((22, 6)), Some((23, 7))];

static NIGHT: Mutex<Option<Night>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
/// Whether a tint is on the screens now.
static TINTED: AtomicBool = AtomicBool::new(false);

fn path() -> std::path::PathBuf {
    config::dir().join("nightlight")
}

fn parse(text: &str) -> Night {
    let mut n = Night::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "on" => n.on = v.trim() == "true",
            "strength" => n.strength = v.trim().parse().unwrap_or(0.5_f32).clamp(0.1, 1.0),
            "hours" => n.hours = v.trim().split_once('-').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))),
            _ => {}
        }
    }
    n
}

pub fn settings() -> Night {
    *NIGHT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(|| parse(&std::fs::read_to_string(path()).unwrap_or_default()))
}

/// Changes the settings, saves them and applies them now.
pub fn update(f: impl FnOnce(&mut Night)) {
    let mut n = settings();
    f(&mut n);
    *NIGHT.lock().unwrap_or_else(|e| e.into_inner()) = Some(n);
    let hours = n.hours.map(|(a, b)| format!("{a}-{b}")).unwrap_or_default();
    let _ = std::fs::write(path(), format!("on={}\nstrength={}\nhours={hours}\n", n.on, n.strength));
    apply();
    super::changed();
}

/// Whether hour `h` (0 to 23) falls in the schedule `from`..`to` (which may run past midnight).
fn in_hours(h: u32, (from, to): (u32, u32)) -> bool {
    if from <= to {
        (from..to).contains(&h)
    } else {
        h >= from || h < to
    }
}

/// Whether the tint should be on right now.
fn wanted(n: &Night) -> bool {
    if !n.on {
        return false;
    }
    match n.hours {
        None => true,
        Some(hours) => {
            in_hours(unsafe { windows::Win32::System::SystemInformation::GetLocalTime() }.wHour as u32, hours)
        }
    }
}

/// The ramp for a strength (0: none): red kept, green and blue turned down,
/// blue the most, the way a warm light is.
fn ramp(strength: f32) -> [[u16; 256]; 3] {
    let s = strength.clamp(0.0, 1.0);
    let k = [1.0, 1.0 - 0.22 * s, 1.0 - 0.5 * s];
    let mut r = [[0u16; 256]; 3];
    for (c, row) in r.iter_mut().enumerate() {
        for (i, v) in row.iter_mut().enumerate() {
            *v = ((i as f32 * 257.0) * k[c]).round().clamp(0.0, 65535.0) as u16;
        }
    }
    r
}

/// Sets every screen's ramp. False if Windows refused one.
fn set_ramps(strength: f32) -> bool {
    let r = ramp(strength);
    let mut ok = true;
    for s in super::display::screens() {
        unsafe {
            let dc = CreateDCW(w!("DISPLAY"), &HSTRING::from(s.gdi.as_str()), None, None);
            if dc.is_invalid() {
                continue;
            }
            ok &= SetDeviceGammaRamp(dc, r.as_ptr() as *const _).as_bool();
            let _ = DeleteDC(dc);
        }
    }
    ok
}

/// Puts the tint on or off to match the settings and the time.
fn apply() {
    let n = settings();
    if wanted(&n) {
        let ok = set_ramps(n.strength);
        if !TINTED.swap(true, Ordering::AcqRel) {
            config::log(&format!("night light: on at {:.0}% (took: {ok})", n.strength * 100.0));
        }
    } else if TINTED.swap(false, Ordering::AcqRel) {
        set_ramps(0.0);
        config::log("night light: off");
    }
}

/// Looks every minute (and puts the tint back after a screen change resets it).
pub fn start() {
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(|| {
        while RUNNING.load(Ordering::Acquire) {
            if TINTED.load(Ordering::Acquire) || wanted(&settings()) {
                // A display change resets ramps: set them again every time.
                TINTED.store(false, Ordering::Release);
            }
            apply();
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    });
}

/// Takes the tint off (Cascade is closing).
pub fn stop() {
    RUNNING.store(false, Ordering::Release);
    if TINTED.swap(false, Ordering::AcqRel) {
        set_ramps(0.0);
    }
}

fn schedule_text(h: Option<(u32, u32)>) -> String {
    match h {
        None => "All the time".into(),
        Some((a, b)) => format!("{a}:00 to {b}:00"),
    }
}

pub fn schedule_label() -> String {
    schedule_text(settings().hours)
}

pub fn pick_schedule(owner: HWND) {
    let names: Vec<String> = SCHEDULES.iter().map(|h| schedule_text(*h)).collect();
    let cur = SCHEDULES.iter().position(|h| *h == settings().hours);
    if let Some(i) = super::pick(owner, &names, cur) {
        update(|n| n.hours = SCHEDULES[i]);
    }
}

pub fn desc() -> String {
    let n = settings();
    match (n.on, wanted(&n)) {
        (false, _) => "Warmer colours, easier on the eyes at night. Cascade's own (Windows' needs Explorer).".into(),
        (true, true) => "On now.".into(),
        (true, false) => format!("Comes on at {}:00.", n.hours.map(|h| h.0).unwrap_or(0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_past_midnight() {
        assert!(in_hours(22, (21, 7)));
        assert!(in_hours(3, (21, 7)));
        assert!(!in_hours(7, (21, 7)));
        assert!(!in_hours(12, (21, 7)));
        assert!(in_hours(10, (9, 17)));
        assert!(!in_hours(17, (9, 17)));
    }

    #[test]
    fn reads_its_file() {
        assert_eq!(
            parse("on=true\nstrength=0.7\nhours=21-7\n"),
            Night { on: true, strength: 0.7, hours: Some((21, 7)) }
        );
        assert_eq!(parse(""), Night::default());
        assert_eq!(parse("hours=\n").hours, None);
    }

    #[test]
    fn warm_ramp() {
        let r = ramp(1.0);
        assert_eq!(r[0][255], 65535);
        assert!(r[2][255] < r[1][255] && r[1][255] < r[0][255]);
        assert_eq!(ramp(0.0)[2][255], 65535);
    }
}
