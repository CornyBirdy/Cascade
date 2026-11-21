//! A pen tablet driver, built in: no vendor driver (their tray apps are slow
//! and fussy) and no OpenTabletDriver needed for the tablets it knows.
//!
//! Cheap tablets (10moon, Vinsa, Gotop's and their rebadges) start as a
//! plain mouse: no pressure, the whole tablet stretched over the screen.
//! Their own drivers (Vinsa's `TabletService.exe` is one, looking for USB
//! 08F2:6811 and 0B57:902E) switch them into tablet mode with a few HID
//! feature reports and then read the pen's position, pressure and buttons
//! from their own reports. Cascade does the same (the reports as
//! OpenTabletDriver has them for the 10moon 1060N), and hands the pen to
//! Windows as a real pen (Windows Ink: pressure in Photoshop, Krita,
//! OneNote, Paint...) through Windows' synthetic pointer input, or as a
//! mouse for apps that only take a mouse.
//!
//! Settings > Bluetooth & devices > Pen tablet: which screen it maps to,
//! keeping the tablet's proportions, left-handed, how firm the pressure is,
//! and what the pen's two buttons do. A tablet Windows already drives as a
//! pen (Wacom and the like) is left alone.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::UI::Controls::{
    CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE, POINTER_FEEDBACK_DEFAULT,
    POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Input::Pointer::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// How a tablet's reports read.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Format {
    /// 10moon 1060N and its relatives (OpenTabletDriver's TenMoon parser).
    TenMoon,
}

/// A tablet Cascade knows.
struct Model {
    name: &'static str,
    vid: u16,
    pid: u16,
    format: Format,
    /// The pen's interface: its input report length.
    pen_report: u16,
    /// The interface that takes the switch into tablet mode: its input report length.
    init_report: u16,
    /// Feature reports that switch it into tablet mode.
    init: &'static [&'static [u8]],
    /// The tablet's size (mm) and its coordinates' range.
    size: (f32, f32),
    max: (f32, f32),
    /// The raw pressure with the pen up (the reading falls as it's pressed).
    pressure_rest: f32,
}

const MODELS: [Model; 2] = [
    Model {
        name: "10moon / Vinsa tablet",
        vid: 0x08F2,
        pid: 0x6811,
        format: Format::TenMoon,
        pen_report: 64,
        init_report: 8,
        init: &[
            &[0x08, 0x04, 0x1D, 0x01, 0xFF, 0xF4, 0x06, 0x83],
            &[0x08, 0x03, 0x00, 0xFF, 0xF0, 0x00, 0xFF, 0xF0],
            &[0x08, 0x03, 0x00, 0xFF, 0xF0, 0x00, 0xFF, 0xF0],
            &[0x08, 0x06, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
        ],
        size: (254.0, 152.4),
        max: (4095.0, 4095.0),
        pressure_rest: 1650.0,
    },
    // The other model Vinsa's driver looks for (Hanvon's chip), read the same way.
    Model {
        name: "Vinsa tablet",
        vid: 0x0B57,
        pid: 0x902E,
        format: Format::TenMoon,
        pen_report: 64,
        init_report: 8,
        init: &[
            &[0x08, 0x04, 0x1D, 0x01, 0xFF, 0xF4, 0x06, 0x83],
            &[0x08, 0x03, 0x00, 0xFF, 0xF0, 0x00, 0xFF, 0xF0],
            &[0x08, 0x03, 0x00, 0xFF, 0xF0, 0x00, 0xFF, 0xF0],
            &[0x08, 0x06, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
        ],
        size: (254.0, 152.4),
        max: (4095.0, 4095.0),
        pressure_rest: 1650.0,
    },
];

/// What the pen's buttons do.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Button {
    Right,
    Middle,
    Eraser,
    Nothing,
}

impl Button {
    fn parse(v: &str) -> Button {
        match v {
            "middle" => Button::Middle,
            "eraser" => Button::Eraser,
            "none" => Button::Nothing,
            _ => Button::Right,
        }
    }
}

/// The settings (from the config).
#[derive(Clone, Debug)]
pub struct Setup {
    pub on: bool,
    /// Windows Ink (a pen, with pressure) rather than a mouse.
    pub ink: bool,
    /// The main screen only (else every screen).
    pub main_screen: bool,
    pub left_handed: bool,
    /// Use a part of the tablet shaped like the screen, so circles stay round.
    pub keep_shape: bool,
    /// How firm the pressure is: 1 as it comes, above 1 needs more press.
    pub firmness: f32,
    pub lower: Button,
    pub upper: Button,
}

static SETUP: Mutex<Option<Setup>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static STATUS: Mutex<String> = Mutex::new(String::new());

fn setup() -> Option<Setup> {
    SETUP.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set_status(s: impl Into<String>) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = s.into();
}

/// What the driver is doing, for Settings.
pub fn status() -> String {
    let s = STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if s.is_empty() {
        "Looking for a tablet\u{2026}".into()
    } else {
        s
    }
}

/// Applies the settings (and starts or stops the driver).
pub fn configure(cfg: &crate::config::Config) {
    let s = Setup {
        on: cfg.tablet,
        ink: cfg.tablet_mode != "mouse",
        main_screen: cfg.tablet_screen == "main",
        left_handed: cfg.tablet_left,
        keep_shape: cfg.tablet_keep_shape,
        firmness: cfg.tablet_firmness.clamp(0.3, 3.0),
        lower: Button::parse(&cfg.tablet_lower),
        upper: Button::parse(&cfg.tablet_upper),
    };
    let on = s.on;
    *SETUP.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
    if on && !RUNNING.swap(true, Ordering::AcqRel) {
        std::thread::spawn(run);
    }
}

// ---------------------------------------------------------------- devices --

/// A HID interface: its path, ids and report lengths.
struct Interface {
    path: Vec<u16>,
    vid: u16,
    pid: u16,
    input: u16,
    feature: u16,
}

/// Every HID interface there is now.
fn interfaces() -> Vec<Interface> {
    let mut out = Vec::new();
    unsafe {
        let guid = HidD_GetHidGuid();
        let Ok(set) = SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)
        else {
            return out;
        };
        let mut i = 0;
        loop {
            let mut ifd = SP_DEVICE_INTERFACE_DATA {
                cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInterfaces(set, None, &guid, i, &mut ifd).is_err() {
                break;
            }
            i += 1;
            let mut need = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &ifd, None, 0, Some(&mut need), None);
            if need < 8 {
                continue;
            }
            let mut buf = vec![0u8; need as usize + 2];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &ifd, Some(detail), need, None, None).is_err() {
                continue;
            }
            let p = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
            let mut path = Vec::new();
            let mut k = 0;
            while *p.add(k) != 0 {
                path.push(*p.add(k));
                k += 1;
            }
            path.push(0);
            if let Some(info) = describe(&path) {
                out.push(info);
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    out
}

fn open(path: &[u16], access: u32) -> Option<HANDLE> {
    unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .ok()
    }
}

/// An interface's ids and report lengths (opened without access, which
/// works even for the ones Windows keeps for itself).
fn describe(path: &[u16]) -> Option<Interface> {
    let h = open(path, 0)?;
    unsafe {
        let mut attrs = HIDD_ATTRIBUTES { Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
        let ok = HidD_GetAttributes(h, &mut attrs);
        let mut caps = HIDP_CAPS::default();
        let mut pre = PHIDP_PREPARSED_DATA::default();
        if HidD_GetPreparsedData(h, &mut pre) {
            let _ = HidP_GetCaps(pre, &mut caps);
            let _ = HidD_FreePreparsedData(pre);
        }
        let _ = CloseHandle(h);
        ok.then(|| Interface {
            path: path.to_vec(),
            vid: attrs.VendorID,
            pid: attrs.ProductID,
            input: caps.InputReportByteLength,
            feature: caps.FeatureReportByteLength,
        })
    }
}

/// Sends the reports that switch the tablet into tablet mode.
fn switch_on(m: &Model, list: &[Interface]) -> bool {
    let mut any = false;
    for i in list.iter().filter(|i| i.vid == m.vid && i.pid == m.pid && i.input == m.init_report) {
        let Some(h) = open(&i.path, (GENERIC_READ | GENERIC_WRITE).0) else { continue };
        for r in m.init {
            let mut buf = r.to_vec();
            if (i.feature as usize) > buf.len() {
                buf.resize(i.feature as usize, 0);
            }
            unsafe {
                any |= HidD_SetFeature(h, buf.as_ptr() as *const _, buf.len() as u32);
            }
        }
        unsafe {
            let _ = CloseHandle(h);
        }
    }
    any
}

// ------------------------------------------------------------------ reports --

/// One reading of the pen.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pen {
    /// 0..1 across and down the tablet.
    x: f32,
    y: f32,
    /// 0..1.
    pressure: f32,
    lower: bool,
    upper: bool,
}

/// A report from the pen interface, if it's a pen report.
fn parse(m: &Model, r: &[u8]) -> Option<Pen> {
    match m.format {
        Format::TenMoon => {
            if r.len() < 12 || r[11] != 0xFF {
                return None;
            }
            let x = ((r[1] as u16) << 8 | r[2] as u16) as f32;
            let y = (((r[3] as u16) << 8 | r[4] as u16) as i16).max(0) as f32;
            let pressed = r[9] & 6 != 0;
            let raw = ((r[5] as u16) << 8 | r[6] as u16) as f32 - if pressed { 50.0 } else { 0.0 };
            let pressure = ((m.pressure_rest - raw) / m.pressure_rest).clamp(0.0, 1.0);
            Some(Pen {
                x: (x / m.max.0).clamp(0.0, 1.0),
                y: (y / m.max.1).clamp(0.0, 1.0),
                pressure,
                lower: r[9] & 4 != 0,
                upper: r[9] & 6 == 6,
            })
        }
    }
}

// ------------------------------------------------------------------- output --

/// Where on the screen a point of the tablet is: the tablet (or the part of
/// it shaped like the screen) stretched over the screen, turned round for
/// the left hand.
fn place(m: &Model, s: &Setup, pen: &Pen) -> POINT {
    let (mut u, mut v) = (pen.x, pen.y);
    if s.left_handed {
        u = 1.0 - u;
        v = 1.0 - v;
    }
    let area = unsafe {
        if s.main_screen {
            RECT { left: 0, top: 0, right: GetSystemMetrics(SM_CXSCREEN), bottom: GetSystemMetrics(SM_CYSCREEN) }
        } else {
            let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
            RECT {
                left: x,
                top: y,
                right: x + GetSystemMetrics(SM_CXVIRTUALSCREEN),
                bottom: y + GetSystemMetrics(SM_CYVIRTUALSCREEN),
            }
        }
    };
    let (sw, sh) = ((area.right - area.left) as f32, (area.bottom - area.top) as f32);
    if s.keep_shape && sw > 0.0 && sh > 0.0 {
        // The middle of the tablet, as wide (or tall) as the screen's shape allows.
        let (tw, th) = m.size;
        let screen = sw / sh;
        if tw / th > screen {
            let used = th * screen / tw;
            u = ((u - (1.0 - used) / 2.0) / used).clamp(0.0, 1.0);
        } else {
            let used = tw / screen / th;
            v = ((v - (1.0 - used) / 2.0) / used).clamp(0.0, 1.0);
        }
    }
    POINT { x: area.left + (u * (sw - 1.0)).round() as i32, y: area.top + (v * (sh - 1.0)).round() as i32 }
}

/// Where the pen was last, for the transitions Windows wants.
#[derive(Default)]
struct Out {
    near: bool,
    down: bool,
    lower: bool,
    upper: bool,
    pen: Option<HSYNTHETICPOINTERDEVICE>,
    frame: u32,
}

fn mouse(flags: MOUSE_EVENT_FLAGS, at: Option<POINT>) {
    unsafe {
        let (mut dx, mut dy) = (0, 0);
        let mut flags = flags;
        if let Some(p) = at {
            let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
            let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2), GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2));
            dx = ((p.x - vx) as i64 * 65535 / (vw - 1) as i64) as i32;
            dy = ((p.y - vy) as i64 * 65535 / (vh - 1) as i64) as i32;
            flags |= MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
        }
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        };
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// Presses or lets go of a button the pen's button stands for (not the eraser).
fn button(b: Button, down: bool, at: POINT) {
    let flags = match (b, down) {
        (Button::Right, true) => MOUSEEVENTF_RIGHTDOWN,
        (Button::Right, false) => MOUSEEVENTF_RIGHTUP,
        (Button::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
        (Button::Middle, false) => MOUSEEVENTF_MIDDLEUP,
        _ => return,
    };
    mouse(flags, Some(at));
}

impl Out {
    /// The pen moved (`Some`), or went out of reach (`None`).
    fn send(&mut self, m: &Model, s: &Setup, pen: Option<Pen>) {
        let Some(pen) = pen else {
            if self.near {
                if self.down {
                    self.contact(s, None, false, 0.0, false);
                }
                self.leave(s);
            }
            return;
        };
        let at = place(m, s, &pen);
        let pressure = pen.pressure.powf(s.firmness);
        let touching = pressure > 0.02;
        let eraser = (pen.lower && s.lower == Button::Eraser) || (pen.upper && s.upper == Button::Eraser);
        // The buttons that stand for mouse buttons.
        for (now, was, b) in [(pen.lower, self.lower, s.lower), (pen.upper, self.upper, s.upper)] {
            if now != was {
                button(b, now, at);
            }
        }
        self.lower = pen.lower;
        self.upper = pen.upper;
        self.contact(s, Some(at), touching, pressure, eraser);
    }

    fn contact(&mut self, s: &Setup, at: Option<POINT>, touching: bool, pressure: f32, eraser: bool) {
        let first = !self.near;
        let (was, now) = (self.down, touching);
        self.near = true;
        self.down = now;
        if !s.ink {
            mouse(MOUSE_EVENT_FLAGS(0), at);
            match (was, now) {
                (false, true) => mouse(MOUSEEVENTF_LEFTDOWN, None),
                (true, false) => mouse(MOUSEEVENTF_LEFTUP, None),
                _ => {}
            }
            return;
        }
        let mut flags = POINTER_FLAG_INRANGE;
        flags |= match (was, now) {
            (false, true) => POINTER_FLAG_DOWN | POINTER_FLAG_INCONTACT | POINTER_FLAG_FIRSTBUTTON,
            (true, true) => POINTER_FLAG_UPDATE | POINTER_FLAG_INCONTACT | POINTER_FLAG_FIRSTBUTTON,
            (true, false) => POINTER_FLAG_UP,
            (false, false) => POINTER_FLAG_UPDATE,
        };
        if first {
            flags |= POINTER_FLAG_NEW;
        }
        self.inject(at, flags, pressure, eraser);
    }

    fn leave(&mut self, s: &Setup) {
        self.near = false;
        self.down = false;
        if s.ink {
            self.inject(None, POINTER_FLAG_UPDATE, 0.0, false);
        }
    }

    fn inject(&mut self, at: Option<POINT>, flags: POINTER_FLAGS, pressure: f32, eraser: bool) {
        unsafe {
            if self.pen.is_none() {
                self.pen = CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_DEFAULT).ok();
            }
            let Some(dev) = self.pen else { return };
            let mut pt = POINT::default();
            match at {
                Some(p) => pt = p,
                None => {
                    let _ = GetCursorPos(&mut pt);
                }
            }
            self.frame = self.frame.wrapping_add(1);
            let info = POINTER_PEN_INFO {
                pointerInfo: POINTER_INFO {
                    pointerType: PT_PEN,
                    pointerId: 0,
                    frameId: self.frame,
                    pointerFlags: flags,
                    ptPixelLocation: pt,
                    ..Default::default()
                },
                penFlags: if eraser { PEN_FLAG_ERASER | PEN_FLAG_INVERTED } else { 0 },
                penMask: PEN_MASK_PRESSURE,
                pressure: (pressure.clamp(0.0, 1.0) * 1024.0) as u32,
                ..Default::default()
            };
            let input = POINTER_TYPE_INFO { r#type: PT_PEN, Anonymous: POINTER_TYPE_INFO_0 { penInfo: info } };
            let _ = InjectSyntheticPointerInput(dev, &[input]);
        }
    }
}

impl Drop for Out {
    fn drop(&mut self) {
        if let Some(d) = self.pen.take() {
            unsafe { DestroySyntheticPointerDevice(d) };
        }
    }
}

// --------------------------------------------------------------------- run --

/// Whether a vendor's own tablet driver is running (it takes the tablet too).
fn vendor_driver() -> Option<&'static str> {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    const KNOWN: [(&str, &str); 3] = [
        ("tabletservice.exe", "the tablet's own driver"),
        ("tstart.exe", "the tablet's own driver"),
        ("opentabletdriver.daemon.exe", "OpenTabletDriver"),
    ];
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        let mut found = None;
        while ok && found.is_none() {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
            let name = String::from_utf16_lossy(&e.szExeFile[..n]).to_ascii_lowercase();
            found = KNOWN.iter().find(|(p, _)| *p == name).map(|k| k.1);
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

fn run() {
    crate::config::log("tablet: driver started");
    while setup().is_some_and(|s| s.on) {
        let list = interfaces();
        let found = MODELS.iter().find_map(|m| {
            list.iter()
                .find(|i| i.vid == m.vid && i.pid == m.pid && i.input == m.pen_report)
                .map(|i| (m, i.path.clone()))
        });
        let Some((m, path)) = found else {
            set_status("No tablet it knows is plugged in (10moon, Vinsa and their relatives).");
            std::thread::sleep(Duration::from_secs(3));
            continue;
        };
        if let Some(other) = vendor_driver() {
            set_status(format!(
                "{} found, but {other} is running and holds it: close it (and stop it starting with Windows).",
                m.name
            ));
            std::thread::sleep(Duration::from_secs(3));
            continue;
        }
        let switched = switch_on(m, &list);
        let Some(h) = open(&path, (GENERIC_READ | GENERIC_WRITE).0).or_else(|| open(&path, GENERIC_READ.0)) else {
            set_status(format!("{} found, but it couldn't be opened.", m.name));
            std::thread::sleep(Duration::from_secs(3));
            continue;
        };
        crate::config::log(&format!("tablet: {} connected (tablet mode: {switched})", m.name));
        set_status(format!("{} connected", m.name));
        read(m, h);
        unsafe {
            let _ = CloseHandle(h);
        }
        set_status(format!("{} disconnected", m.name));
        std::thread::sleep(Duration::from_millis(500));
    }
    RUNNING.store(false, Ordering::Release);
    set_status("Off");
    crate::config::log("tablet: driver stopped");
}

/// Reads the pen until the tablet goes or the driver's turned off. The pen
/// is out of reach when reports stop coming for a moment (a reader thread
/// hands reports over, so that can be told).
fn read(m: &'static Model, h: HANDLE) {
    let (tx, rx) = std::sync::mpsc::channel::<Option<Vec<u8>>>();
    let hv = h.0 as isize;
    let len = m.pen_report as usize;
    std::thread::spawn(move || {
        let h = HANDLE(hv as *mut _);
        let mut buf = vec![0u8; len];
        loop {
            let mut got = 0u32;
            let ok = unsafe { ReadFile(h, Some(&mut buf), Some(&mut got), None).is_ok() };
            if !ok || got == 0 {
                let _ = tx.send(None);
                return;
            }
            if tx.send(Some(buf[..got as usize].to_vec())).is_err() {
                return;
            }
        }
    });
    let mut out = Out::default();
    let mut last = Instant::now();
    loop {
        let Some(s) = setup().filter(|s| s.on) else {
            out.send(m, &Setup { on: false, ..default_setup() }, None);
            unsafe {
                let _ = windows::Win32::System::IO::CancelIoEx(h, None);
            }
            return;
        };
        match rx.recv_timeout(Duration::from_millis(60)) {
            Ok(Some(r)) => {
                if let Some(pen) = parse(m, &r) {
                    last = Instant::now();
                    out.send(m, &s, Some(pen));
                }
            }
            Ok(None) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                out.send(m, &s, None);
                return;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if last.elapsed() > Duration::from_millis(80) {
                    out.send(m, &s, None);
                }
            }
        }
    }
}

fn default_setup() -> Setup {
    Setup {
        on: true,
        ink: true,
        main_screen: false,
        left_handed: false,
        keep_shape: true,
        firmness: 1.0,
        lower: Button::Right,
        upper: Button::Middle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_tenmoon_report() {
        let m = &MODELS[0];
        let mut r = [0u8; 64];
        r[11] = 0xFF;
        r[1] = 0x08; // x 0x0800
        r[3] = 0x04; // y 0x0400
        let rest = m.pressure_rest as u16;
        r[5] = (rest >> 8) as u8;
        r[6] = rest as u8;
        let p = parse(m, &r).unwrap();
        assert!((p.x - 2048.0 / 4095.0).abs() < 1e-4 && (p.y - 1024.0 / 4095.0).abs() < 1e-4);
        assert!(p.pressure < 0.01 && !p.lower && !p.upper);
        // Pressed hard, with the lower button.
        r[5] = 0;
        r[6] = 100;
        r[9] = 4;
        let p = parse(m, &r).unwrap();
        assert!(p.pressure > 0.9 && p.lower && !p.upper);
        // Not a pen report.
        r[11] = 0x33;
        assert!(parse(m, &r).is_none());
    }
}
