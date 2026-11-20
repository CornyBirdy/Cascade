//! Lighting (`cascade.exe --rgb`): iCUE without iCUE, for every brand.
//!
//! - **Profiles** down the side: make, rename, duplicate, delete; pick one to
//!   use and edit; link programs to one so it comes on while they're in front.
//! - **Lighting**: the profile's effect layers (top one over the rest), each
//!   with its effect, the devices it covers, colours, speed, size, direction,
//!   opacity and blend, and a live preview of every device.
//! - **Layout**: drag the devices where they sit on your desk (and resize
//!   them), so a wave runs across them all as one; fans as rings; flip one.
//! - **Devices**: each device on Cascade's effects or one of its own hardware
//!   modes (colours, speed, brightness, direction, saved to the device), and
//!   how many LEDs each fan or strip header has.
//! - **Settings**: on or off, brightness, frame rate, switching with apps,
//!   off when locked; OpenRGB (get it, admin rights for RAM, motherboard and
//!   graphics card lighting) and the vendors' apps that would fight it.
//!
//! Every change is saved as it's made; the engine (see `engine`) picks it up.

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::Instant;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::effects::{self, Blend, Context, Dir, Kind, Rgb, Sensor};
use super::model::{led_positions, Hardware, Layer, Lighting, Place, Profile, Target};
use super::openrgb::{self, Client, Controller};
use crate::appwin::{self, LINE, PANEL};
use crate::gfx::{Align, Color, Fonts, Gfx, Painter};
use crate::theme;

const SIDE: f32 = 250.0;
const TOP: f32 = 56.0;
const ROW: f32 = 34.0;
const DEVICES: u32 = WM_APP + 1;
const TICK: usize = 1;
const SAVE: usize = 2;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab {
    Lighting,
    Layout,
    Devices,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Sl {
    Speed,
    Size,
    Opacity,
    ProfileBright,
    Bright,
    Fps,
    ScreenFps,
    TempLo,
    TempHi,
    HwSpeed,
    HwBright,
    Zone(usize),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Tab(Tab),
    Profile(usize),
    NewProfile,
    Device(usize),
    DeviceOff(usize),
    Layer(usize),
    LayerEye(usize),
    AddLayer,
    LayerUp,
    LayerDown,
    LayerDelete,
    Effect,
    Target,
    Colour(usize),
    AddColour,
    Slider(Sl),
    Dir(Dir),
    Blend,
    Sensor,
    Apps,
    HwMode,
    HwColour(usize),
    HwDir,
    HwSave,
    Toggle(u8),
    GetOpenRgb,
    ChooseOpenRgb,
    Admin,
    Rescan,
    PawnIo,
    Canvas,
}

enum Drag {
    Slider(Sl, f32, f32),
    /// A device on the canvas: its key, where it was grabbed (canvas units), resizing.
    Place(String, f32, f32, bool),
}

struct State {
    hwnd: HWND,
    gfx: Option<Gfx>,
    l: Lighting,
    devices: Vec<Controller>,
    status: String,
    tab: Tab,
    layer: usize,
    device: usize,
    hits: Vec<(f32, f32, f32, f32, Act)>,
    drag: Option<Drag>,
    start: Instant,
    rivals: Vec<&'static str>,
    busy: bool,
    canvas: (f32, f32, f32, f32),
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

static FETCHED: Mutex<Option<Result<Vec<Controller>, String>>> = Mutex::new(None);

fn rgb_of(c: u32) -> Color {
    Color(((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0, 1.0)
}

/// Windows' colour picker, starting at `c` (0xRRGGBB).
fn pick_colour(owner: HWND, c: u32) -> Option<u32> {
    use windows::Win32::UI::Controls::Dialogs::*;
    static CUSTOM: Mutex<[u32; 16]> = Mutex::new([0x00FFFFFF; 16]);
    let mut custom = CUSTOM.lock().unwrap_or_else(|e| e.into_inner());
    let bgr = |c: u32| ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF);
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: COLORREF(bgr(c)),
        lpCustColors: custom.as_mut_ptr() as *mut COLORREF,
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    unsafe { ChooseColorW(&mut cc).as_bool() }.then(|| bgr(cc.rgbResult.0))
}

/// Programs with a window open now (exe names), for linking to a profile.
fn running_apps() -> Vec<String> {
    let mut list: Vec<HWND> = Vec::new();
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> windows::core::BOOL {
        let v = &mut *(lp.0 as *mut Vec<HWND>);
        if IsWindowVisible(h).as_bool() && GetWindowTextLengthW(h) > 0 {
            v.push(h);
        }
        windows::core::BOOL(1)
    }
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut list as *mut _ as isize));
    }
    let mut out: Vec<String> = list
        .into_iter()
        .filter_map(|h| {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
            let p = crate::dock::process_path(pid)?;
            Some(p.rsplit('\\').next().unwrap_or(&p).to_ascii_lowercase())
        })
        .filter(|e| e != "cascade.exe" && e != "explorer.exe" && e != "applicationframehost.exe")
        .collect();
    out.sort();
    out.dedup();
    out
}

impl State {
    fn profile(&mut self) -> &mut Profile {
        let i = self.l.active.min(self.l.profiles.len() - 1);
        &mut self.l.profiles[i]
    }

    fn p(&self) -> &Profile {
        &self.l.profiles[self.l.active.min(self.l.profiles.len() - 1)]
    }

    fn changed(&mut self) {
        // Saved a moment after the last change (a slider drag is many changes).
        unsafe {
            SetTimer(Some(self.hwnd), SAVE, 120, None);
        }
        appwin::redraw(self.hwnd);
    }

    fn fetch(&mut self) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = "Looking for devices\u{2026}".into();
        let (port, hwnd) = (self.l.settings.port, self.hwnd.0 as isize);
        // The engine starts OpenRGB if it isn't running.
        super::engine::ensure(self.l.settings.on);
        std::thread::spawn(move || {
            let mut result = Err("OpenRGB isn't running".to_string());
            for _ in 0..20 {
                if super::hw::running(port) {
                    result = Client::connect(port, "Cascade Lighting")
                        .and_then(|mut c| c.controllers())
                        .map_err(|e| e.to_string());
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            *FETCHED.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), DEVICES, WPARAM(0), LPARAM(0));
            }
        });
    }

    fn fetched(&mut self) {
        self.busy = false;
        match FETCHED.lock().unwrap_or_else(|e| e.into_inner()).take() {
            Some(Ok(list)) => {
                self.status =
                    format!("{} device{} through OpenRGB", list.len(), if list.len() == 1 { "" } else { "s" });
                self.l.place_new(&list);
                self.devices = list;
                self.device = self.device.min(self.devices.len().saturating_sub(1));
                self.changed();
            }
            Some(Err(e)) => {
                self.status = if super::hw::find(&self.l.settings.openrgb).is_none() {
                    "OpenRGB isn't on this PC yet: Settings > Get OpenRGB".into()
                } else {
                    format!("Couldn't reach OpenRGB ({e})")
                };
            }
            None => {}
        }
        self.rivals = super::hw::rivals();
        appwin::redraw(self.hwnd);
    }

    // ---------------------------------------------------------- actions --

    fn act(&mut self, a: Act, x: f32, right: bool) {
        use crate::menu::{check, item, show, Entry, Place as MP};
        let owner = self.hwnd;
        match a {
            Act::Tab(t) => self.tab = t,
            Act::Profile(i) if right => {
                self.l.active = i;
                let entries = vec![
                    item(1, "Rename"),
                    item(2, "Duplicate"),
                    item(3, "Linked programs\u{2026}"),
                    Entry::Separator,
                    item(4, "Delete"),
                ];
                match show(owner, entries, MP::Cursor) {
                    1 => self.rename(),
                    2 => {
                        let mut p = self.p().clone();
                        p.name = format!("{} copy", p.name);
                        p.apps.clear();
                        self.l.profiles.push(p);
                        self.l.active = self.l.profiles.len() - 1;
                    }
                    3 => self.apps_menu(),
                    4 if self.l.profiles.len() > 1
                        && crate::winset::confirm(
                            owner,
                            &format!("Delete the profile \u{201c}{}\u{201d}?", self.p().name),
                        ) =>
                    {
                        self.l.profiles.remove(i);
                        self.l.active = 0;
                    }
                    _ => {}
                }
            }
            Act::Profile(i) => {
                self.l.active = i;
                self.layer = 0;
            }
            Act::NewProfile => {
                self.l
                    .profiles
                    .push(Profile { name: format!("Profile {}", self.l.profiles.len() + 1), ..Default::default() });
                self.l.active = self.l.profiles.len() - 1;
                self.layer = 0;
            }
            Act::Device(i) => {
                self.device = i;
                if self.tab == Tab::Lighting || self.tab == Tab::Settings {
                    self.tab = Tab::Devices;
                }
            }
            Act::DeviceOff(i) => {
                if let Some(d) = self.devices.get(i) {
                    let p = self.l.layout.entry(d.key()).or_default();
                    p.off = !p.off;
                }
            }
            Act::Layer(i) => self.layer = i,
            Act::LayerEye(i) => {
                if let Some(l) = self.profile().layers.get_mut(i) {
                    l.on = !l.on;
                }
            }
            Act::AddLayer => {
                let entries = Kind::ALL.iter().enumerate().map(|(i, k)| item(i + 1, k.name())).collect();
                if let Some(i) = show(owner, entries, MP::Cursor).checked_sub(1) {
                    self.profile().layers.push(Layer::new(Kind::ALL[i]));
                    self.layer = self.p().layers.len() - 1;
                }
            }
            Act::LayerUp => {
                let n = self.p().layers.len();
                if self.layer + 1 < n {
                    let l = self.layer;
                    self.profile().layers.swap(l, l + 1);
                    self.layer += 1;
                }
            }
            Act::LayerDown => {
                if self.layer > 0 {
                    let l = self.layer;
                    self.profile().layers.swap(l, l - 1);
                    self.layer -= 1;
                }
            }
            Act::LayerDelete => {
                let l = self.layer;
                if l < self.p().layers.len() {
                    self.profile().layers.remove(l);
                    self.layer = self.layer.saturating_sub(1);
                }
            }
            Act::Effect => {
                let cur = self.cur_layer().map(|l| l.effect.kind);
                let entries =
                    Kind::ALL.iter().enumerate().map(|(i, k)| check(i + 1, k.name(), Some(*k) == cur)).collect();
                if let Some(i) = show(owner, entries, MP::Cursor).checked_sub(1) {
                    let k = Kind::ALL[i];
                    if let Some(l) = self.cur_layer_mut() {
                        if l.name == l.effect.kind.name() {
                            l.name = k.name().into();
                        }
                        l.effect.kind = k;
                        let (lo, hi) = k.colours();
                        if l.effect.colours.len() < lo || l.effect.colours.len() > hi.max(lo) {
                            l.effect.colours = k.default_colours();
                        }
                    }
                }
            }
            Act::Target => {
                let devices: Vec<(String, String)> = self.devices.iter().map(|d| (d.key(), d.name.clone())).collect();
                let Some(l) = self.cur_layer() else { return };
                let chosen: Vec<String> = match &l.target {
                    Target::Devices(d) => d.clone(),
                    _ => Vec::new(),
                };
                let all = matches!(l.target, Target::All);
                let mut entries = vec![check(1, "All devices", all), Entry::Separator];
                for (i, (k, n)) in devices.iter().enumerate() {
                    entries.push(check(i + 10, n.as_str(), !all && chosen.contains(k)));
                }
                match show(owner, entries, MP::Cursor) {
                    1 => {
                        if let Some(l) = self.cur_layer_mut() {
                            l.target = Target::All;
                        }
                    }
                    n if n >= 10 => {
                        let key = devices[n - 10].0.clone();
                        if let Some(l) = self.cur_layer_mut() {
                            let mut list = match &l.target {
                                Target::Devices(d) => d.clone(),
                                _ => Vec::new(),
                            };
                            if let Some(i) = list.iter().position(|k| *k == key) {
                                list.remove(i);
                            } else {
                                list.push(key);
                            }
                            l.target = if list.is_empty() { Target::All } else { Target::Devices(list) };
                        }
                    }
                    _ => {}
                }
            }
            Act::Colour(i) => {
                let Some(l) = self.cur_layer() else { return };
                let (lo, _) = l.effect.kind.colours();
                let n = l.effect.colours.len();
                if right && n > lo.max(1) {
                    if let Some(l) = self.cur_layer_mut() {
                        l.effect.colours.remove(i);
                    }
                } else if let Some(c) = pick_colour(owner, l.effect.colours.get(i).copied().unwrap_or(0xFFFFFF)) {
                    if let Some(l) = self.cur_layer_mut() {
                        if let Some(slot) = l.effect.colours.get_mut(i) {
                            *slot = c;
                        }
                    }
                }
            }
            Act::AddColour => {
                if let Some(c) = pick_colour(owner, 0xFFFFFF) {
                    if let Some(l) = self.cur_layer_mut() {
                        l.effect.colours.push(c);
                    }
                }
            }
            Act::Slider(s) => {
                // Pressed on the track: jump there and drag from it.
                if let Some(&(sx, _, sw, _, _)) = self.hits.iter().find(|h| h.4 == a) {
                    self.drag = Some(Drag::Slider(s, sx, sw));
                    self.slide(s, ((x - sx) / sw).clamp(0.0, 1.0));
                }
            }
            Act::Dir(d) => {
                if let Some(l) = self.cur_layer_mut() {
                    l.effect.dir = d;
                }
            }
            Act::Blend => {
                const BLENDS: [(Blend, &str); 5] = [
                    (Blend::Normal, "Normal"),
                    (Blend::Add, "Add"),
                    (Blend::Screen, "Screen"),
                    (Blend::Multiply, "Multiply"),
                    (Blend::Lighten, "Lighten"),
                ];
                let cur = self.cur_layer().map(|l| l.blend);
                let entries = BLENDS.iter().enumerate().map(|(i, (b, n))| check(i + 1, *n, Some(*b) == cur)).collect();
                if let Some(i) = show(owner, entries, MP::Cursor).checked_sub(1) {
                    if let Some(l) = self.cur_layer_mut() {
                        l.blend = BLENDS[i].0;
                    }
                }
            }
            Act::Sensor => {
                if let Some(l) = self.cur_layer_mut() {
                    l.effect.sensor = if l.effect.sensor == Sensor::Cpu { Sensor::Gpu } else { Sensor::Cpu };
                }
            }
            Act::Apps => self.apps_menu(),
            Act::HwMode => self.hardware_menu(),
            Act::HwColour(i) => {
                let Some(key) = self.devices.get(self.device).map(|d| d.key()) else { return };
                let cur = self.p().hardware.get(&key).and_then(|h| h.colours.get(i).copied()).unwrap_or(0xFFFFFF);
                if let Some(c) = pick_colour(owner, cur) {
                    if let Some(h) = self.profile().hardware.get_mut(&key) {
                        if i < h.colours.len() {
                            h.colours[i] = c;
                        } else {
                            h.colours.push(c);
                        }
                    }
                }
            }
            Act::HwDir => {
                let Some(d) = self.devices.get(self.device).cloned() else { return };
                let key = d.key();
                let names = ["Left", "Right", "Up", "Down", "Horizontal", "Vertical"];
                let cur = self.p().hardware.get(&key).map(|h| h.direction as usize);
                let entries = names.iter().enumerate().map(|(i, n)| check(i + 1, *n, Some(i) == cur)).collect();
                if let Some(i) = show(owner, entries, MP::Cursor).checked_sub(1) {
                    if let Some(h) = self.profile().hardware.get_mut(&key) {
                        h.direction = i as u32;
                    }
                }
            }
            Act::HwSave => {
                let Some(key) = self.devices.get(self.device).map(|d| d.key()) else { return };
                if let Some(h) = self.profile().hardware.get_mut(&key) {
                    h.save = !h.save;
                }
            }
            Act::Toggle(n) => {
                let s = &mut self.l.settings;
                match n {
                    0 => {
                        s.on = !s.on;
                        super::engine::ensure(s.on);
                    }
                    1 => s.auto_switch = !s.auto_switch,
                    2 => s.off_when_idle = !s.off_when_idle,
                    3 => {
                        if let Some(k) = self.devices.get(self.device).map(|d| d.key()) {
                            let p = self.l.layout.entry(k).or_default();
                            p.ring = !p.ring;
                        }
                    }
                    4 => {
                        if let Some(k) = self.devices.get(self.device).map(|d| d.key()) {
                            let p = self.l.layout.entry(k).or_default();
                            p.flip = !p.flip;
                        }
                    }
                    _ => {}
                }
            }
            Act::GetOpenRgb => {
                if self.busy {
                    return;
                }
                self.busy = true;
                self.status = "Getting OpenRGB from codeberg.org\u{2026}".into();
                let hwnd = self.hwnd.0 as isize;
                std::thread::spawn(move || {
                    let r = super::hw::download();
                    crate::config::log(&format!("lighting: OpenRGB download: {r:?}"));
                    *FETCHED.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(r.map(|_| Vec::new()).map_err(|e| format!("getting OpenRGB: {e}")));
                    unsafe {
                        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), DEVICES, WPARAM(1), LPARAM(0));
                    }
                });
            }
            Act::ChooseOpenRgb => {
                let hwnd = self.hwnd.0 as isize;
                crate::shell::off_thread(move || {
                    if let Some(p) = crate::shell::choose_file(
                        "Where's OpenRGB.exe?",
                        &[("OpenRGB", "OpenRGB.exe"), ("Programs", "*.exe")],
                    ) {
                        let mut l = Lighting::load();
                        l.settings.openrgb = p.to_string_lossy().into_owned();
                        l.save();
                        unsafe {
                            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), DEVICES, WPARAM(2), LPARAM(0));
                        }
                    }
                });
            }
            Act::Admin => {
                let on = !self.l.settings.admin;
                std::thread::spawn(move || super::hw::set_admin(on));
                self.l.settings.admin = on;
            }
            Act::Rescan => {
                let port = self.l.settings.port;
                std::thread::spawn(move || {
                    if let Ok(mut c) = Client::connect(port, "Cascade Lighting") {
                        let _ = c.rescan();
                    }
                });
                self.fetch();
            }
            Act::PawnIo => crate::shell::open(super::hw::PAWNIO),
            Act::Canvas => {}
        }
        self.changed();
    }

    fn cur_layer(&self) -> Option<&Layer> {
        self.p().layers.get(self.layer)
    }

    fn cur_layer_mut(&mut self) -> Option<&mut Layer> {
        let i = self.layer;
        self.profile().layers.get_mut(i)
    }

    fn slide(&mut self, s: Sl, t: f32) {
        let key = self.devices.get(self.device).map(|d| d.key()).unwrap_or_default();
        match s {
            Sl::Speed => self.cur_layer_mut().map(|l| l.effect.speed = t).unwrap_or(()),
            Sl::Size => self.cur_layer_mut().map(|l| l.effect.size = t).unwrap_or(()),
            Sl::Opacity => self.cur_layer_mut().map(|l| l.opacity = t).unwrap_or(()),
            Sl::TempLo => self.cur_layer_mut().map(|l| l.effect.temp_range.0 = 20.0 + t * 80.0).unwrap_or(()),
            Sl::TempHi => self.cur_layer_mut().map(|l| l.effect.temp_range.1 = 20.0 + t * 80.0).unwrap_or(()),
            Sl::ProfileBright => self.profile().brightness = t,
            Sl::Bright => self.l.settings.brightness = t,
            Sl::Fps => self.l.settings.fps = (5.0 + t * 115.0).round() as u32,
            Sl::ScreenFps => self.l.settings.screen_fps = (1.0 + t * 59.0).round() as u32,
            Sl::HwSpeed => {
                if let Some(h) = self.profile().hardware.get_mut(&key) {
                    h.speed = t;
                }
            }
            Sl::HwBright => {
                if let Some(h) = self.profile().hardware.get_mut(&key) {
                    h.brightness = t;
                }
            }
            Sl::Zone(z) => {
                // Shown as it's dragged; set on the device when let go (see `release`).
                if let Some(zone) = self.devices.get_mut(self.device).and_then(|d| d.zones.get_mut(z)) {
                    zone.leds_count =
                        (zone.leds_min as f32 + t * (zone.leds_max - zone.leds_min) as f32).round() as u32;
                }
            }
        }
        self.changed();
    }

    fn release(&mut self) {
        if let Some(Drag::Slider(Sl::Zone(z), ..)) = self.drag {
            if let Some(d) = self.devices.get(self.device) {
                let (index, size, port) = (d.index, d.zones[z].leds_count, self.l.settings.port);
                std::thread::spawn(move || {
                    if let Ok(mut c) = Client::connect(port, "Cascade Lighting") {
                        let _ = c.resize_zone(index, z, size);
                    }
                });
                // The engine reads the devices again once OpenRGB says the list changed.
            }
        }
        self.drag = None;
    }

    fn rename(&mut self) {
        let name = self.p().name.clone();
        let (k, i) = (appwin::scale(self.hwnd), self.l.active);
        let y = TOP + i as f32 * ROW;
        unsafe {
            if let Ok(e) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                &HSTRING::from(name),
                WS_CHILD | WS_VISIBLE | WS_BORDER | WINDOW_STYLE(0x80),
                (14.0 * k) as i32,
                (y * k) as i32,
                ((SIDE - 28.0) * k) as i32,
                (28.0 * k) as i32,
                Some(self.hwnd),
                None,
                Some(appwin::instance()),
                None,
            ) {
                let _ = windows::Win32::UI::Shell::SetWindowSubclass(e, Some(rename_proc), 1, 0);
                let _ = SetFocus(Some(e));
                SendMessageW(e, 0x00B1, Some(WPARAM(0)), Some(LPARAM(-1)));
            }
        }
    }

    fn apps_menu(&mut self) {
        use crate::menu::{check, item, show, Entry, Place as MP};
        let linked = self.p().apps.clone();
        let running = running_apps();
        let mut all: Vec<String> = linked.clone();
        for r in running {
            if !all.contains(&r) {
                all.push(r);
            }
        }
        let mut entries = vec![Entry::Header("Comes on while these are in front".into())];
        for (i, a) in all.iter().enumerate() {
            entries.push(check(i + 10, a.as_str(), linked.contains(a)));
        }
        entries.push(Entry::Separator);
        entries.push(item(1, "Browse for a program\u{2026}"));
        match show(self.hwnd, entries, MP::Cursor) {
            1 => {
                if let Some(p) = crate::shell::choose_file("A program for this profile", &[("Programs", "*.exe")]) {
                    let exe = p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                    if !exe.is_empty() && !self.p().apps.contains(&exe) {
                        self.profile().apps.push(exe);
                    }
                }
            }
            n if n >= 10 => {
                let a = all[n - 10].clone();
                let apps = &mut self.profile().apps;
                if let Some(i) = apps.iter().position(|x| *x == a) {
                    apps.remove(i);
                } else {
                    apps.push(a);
                }
            }
            _ => {}
        }
    }

    fn hardware_menu(&mut self) {
        use crate::menu::{check, show, Entry, Place as MP};
        let Some(d) = self.devices.get(self.device).cloned() else { return };
        let key = d.key();
        let cur = self.p().hardware.get(&key).map(|h| h.mode.clone());
        let mut entries = vec![check(1, "Cascade's lighting (the profile's layers)", cur.is_none()), Entry::Separator];
        for (i, m) in d.modes.iter().enumerate() {
            entries.push(check(i + 10, m.name.as_str(), cur.as_deref() == Some(m.name.as_str())));
        }
        match show(self.hwnd, entries, MP::Cursor) {
            1 => {
                self.profile().hardware.remove(&key);
            }
            n if n >= 10 => {
                let m = &d.modes[n - 10];
                let colours = m.colors.iter().map(|&c| {
                    let (r, g, b) = openrgb::unpack(c);
                    (r as u32) << 16 | (g as u32) << 8 | b as u32
                });
                let hw = Hardware {
                    mode: m.name.clone(),
                    colours: colours.collect(),
                    speed: 0.5,
                    brightness: 1.0,
                    direction: m.direction,
                    save: false,
                };
                self.profile().hardware.insert(key, hw);
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------- drawing --

    fn paint(&mut self) {
        let Some(mut gfx) = self.gfx.take() else { return };
        let hwnd = self.hwnd;
        let mut hits = Vec::new();
        let mut canvas = self.canvas;
        appwin::paint(hwnd, &mut gfx, |p, f, w, h| {
            let mut ui = Ui { p, f, hits: &mut hits };
            self.draw(&mut ui, w, h, &mut canvas);
        });
        self.hits = hits;
        self.canvas = canvas;
        self.gfx = Some(gfx);
    }

    fn draw(&self, ui: &mut Ui, w: f32, h: f32, canvas: &mut (f32, f32, f32, f32)) {
        let (p, f) = (ui.p, ui.f);
        let accent = theme::accent();
        // Sidebar: profiles, then devices.
        p.rrect(0.0, 0.0, SIDE, h, 0.0, Color(0.11, 0.11, 0.115, 1.0));
        p.text("Profiles", &f.label, 18.0, 16.0, 150.0, 20.0, Align::Left, Color::white(0.55));
        ui.button(SIDE - 46.0, 12.0, 30.0, 26.0, "+", Act::NewProfile);
        for (i, pr) in self.l.profiles.iter().enumerate() {
            let y = TOP + i as f32 * ROW;
            let on = i == self.l.active;
            if on {
                p.rrect(8.0, y, SIDE - 16.0, ROW - 4.0, 5.0, Color::white(0.08));
                p.rrect(8.0, y + 8.0, 3.0, ROW - 20.0, 1.5, accent);
            }
            let apps = if pr.apps.is_empty() {
                String::new()
            } else {
                format!("  \u{b7} {} app{}", pr.apps.len(), if pr.apps.len() == 1 { "" } else { "s" })
            };
            p.text(
                &format!("{}{apps}", pr.name),
                &f.body,
                22.0,
                y,
                SIDE - 40.0,
                ROW - 4.0,
                Align::Left,
                Color::white(0.92),
            );
            ui.hits.push((8.0, y, SIDE - 16.0, ROW - 4.0, Act::Profile(i)));
        }
        let dy = TOP + 20.0 + self.l.profiles.len() as f32 * ROW + 20.0;
        p.text("Devices", &f.label, 18.0, dy, 150.0, 20.0, Align::Left, Color::white(0.55));
        for (i, d) in self.devices.iter().enumerate() {
            let y = dy + 26.0 + i as f32 * ROW;
            if y > h - ROW {
                break;
            }
            let off = self.l.layout.get(&d.key()).is_some_and(|p| p.off);
            if i == self.device && self.tab == Tab::Devices {
                p.rrect(8.0, y, SIDE - 16.0, ROW - 4.0, 5.0, Color::white(0.08));
            }
            p.text(
                &d.name,
                &f.body,
                18.0,
                y,
                SIDE - 74.0,
                ROW - 4.0,
                Align::Left,
                Color::white(if off { 0.4 } else { 0.9 }),
            );
            ui.hits.push((8.0, y, SIDE - 70.0, ROW - 4.0, Act::Device(i)));
            ui.toggle(SIDE - 54.0, y + 6.0, !off, Act::DeviceOff(i));
        }
        if self.devices.is_empty() {
            p.text_wrap(&self.status, &f.small, 18.0, dy + 26.0, SIDE - 30.0, 60.0, Color::white(0.5));
        }
        // Tabs and status.
        let x0 = SIDE + 20.0;
        for (i, (t, name)) in [
            (Tab::Lighting, "Lighting"),
            (Tab::Layout, "Layout"),
            (Tab::Devices, "Devices"),
            (Tab::Settings, "Settings"),
        ]
        .iter()
        .enumerate()
        {
            let x = x0 + i as f32 * 104.0;
            let on = self.tab == *t;
            p.rrect(x, 12.0, 96.0, 32.0, 6.0, if on { Color::white(0.10) } else { Color::white(0.0) });
            p.text(name, &f.title, x, 12.0, 96.0, 32.0, Align::Center, Color::white(if on { 0.95 } else { 0.6 }));
            if on {
                p.rrect(x + 30.0, 41.0, 36.0, 3.0, 1.5, accent);
            }
            ui.hits.push((x, 12.0, 96.0, 32.0, Act::Tab(*t)));
        }
        p.text(&self.status, &f.small, x0 + 430.0, 12.0, w - x0 - 450.0, 32.0, Align::Right, Color::white(0.5));
        let (cx, cy, cw, ch) = (x0, TOP + 12.0, w - x0 - 20.0, h - TOP - 24.0);
        match self.tab {
            Tab::Lighting => self.draw_lighting(ui, cx, cy, cw, ch, canvas),
            Tab::Layout => {
                *canvas = (cx, cy + 30.0, cw, ch - 30.0);
                p.text(
                    "Drag a device to where it sits on your desk; drag its corner to resize. Right-click one for more.",
                    &f.small,
                    cx,
                    cy,
                    cw,
                    22.0,
                    Align::Left,
                    Color::white(0.55),
                );
                self.draw_canvas(ui, *canvas, true);
            }
            Tab::Devices => self.draw_devices(ui, cx, cy, cw, ch),
            Tab::Settings => self.draw_settings(ui, cx, cy, cw),
        }
    }

    fn draw_lighting(&self, ui: &mut Ui, x: f32, y: f32, w: f32, h: f32, canvas: &mut (f32, f32, f32, f32)) {
        let (p, f) = (ui.p, ui.f);
        let accent = theme::accent();
        let pr = self.p();
        // Layers, top first.
        let lw = 280.0;
        p.rrect(x, y, lw, h * 0.55, 8.0, PANEL);
        p.text("Layers", &f.label, x + 14.0, y + 8.0, 100.0, 22.0, Align::Left, Color::white(0.55));
        ui.button(x + lw - 140.0, y + 6.0, 30.0, 26.0, "+", Act::AddLayer);
        ui.button(x + lw - 106.0, y + 6.0, 30.0, 26.0, "\u{2191}", Act::LayerUp);
        ui.button(x + lw - 72.0, y + 6.0, 30.0, 26.0, "\u{2193}", Act::LayerDown);
        ui.button(x + lw - 38.0, y + 6.0, 30.0, 26.0, "\u{d7}", Act::LayerDelete);
        for (row, i) in (0..pr.layers.len()).rev().enumerate() {
            let l = &pr.layers[i];
            let ly = y + 40.0 + row as f32 * ROW;
            if ly + ROW > y + h * 0.55 {
                break;
            }
            if i == self.layer {
                p.rrect(x + 6.0, ly, lw - 12.0, ROW - 4.0, 5.0, accent.alpha(0.22));
            }
            ui.toggle(x + 12.0, ly + 6.0, l.on, Act::LayerEye(i));
            p.text(
                &l.name,
                &f.body,
                x + 60.0,
                ly,
                lw - 70.0,
                ROW - 4.0,
                Align::Left,
                Color::white(if l.on { 0.92 } else { 0.45 }),
            );
            ui.hits.push((x + 54.0, ly, lw - 60.0, ROW - 4.0, Act::Layer(i)));
        }
        if pr.layers.is_empty() {
            p.text(
                "No layers: + adds an effect",
                &f.small,
                x + 14.0,
                y + 44.0,
                lw - 28.0,
                20.0,
                Align::Left,
                Color::white(0.45),
            );
        }
        // The profile itself.
        let py = y + h * 0.55 + 10.0;
        p.rrect(x, py, lw, h * 0.45 - 10.0, 8.0, PANEL);
        p.text("This profile", &f.label, x + 14.0, py + 8.0, 200.0, 22.0, Align::Left, Color::white(0.55));
        ui.slider(
            x + 14.0,
            py + 36.0,
            lw - 28.0,
            "Brightness",
            pr.brightness,
            &format!("{:.0}%", pr.brightness * 100.0),
            Act::Slider(Sl::ProfileBright),
        );
        let apps =
            if pr.apps.is_empty() { "None: it's used whenever it's chosen".to_string() } else { pr.apps.join(", ") };
        p.text("Comes on with", &f.small, x + 14.0, py + 88.0, lw - 28.0, 16.0, Align::Left, Color::white(0.55));
        p.text_wrap(&apps, &f.body, x + 14.0, py + 106.0, lw - 28.0, 40.0, Color::white(0.85));
        ui.button(x + 14.0, py + 150.0, 150.0, 30.0, "Linked programs\u{2026}", Act::Apps);
        // The chosen layer's settings.
        let ex = x + lw + 16.0;
        let ew = w - lw - 16.0;
        let top_h = (h - 220.0).max(260.0);
        p.rrect(ex, y, ew, top_h, 8.0, PANEL);
        if let Some(l) = pr.layers.get(self.layer) {
            let e = &l.effect;
            let mut ry = y + 12.0;
            let row = |ui: &mut Ui, ry: &mut f32, label: &str, value: &str, a: Act| {
                ui.p.text(label, &ui.f.body, ex + 16.0, *ry, 160.0, 30.0, Align::Left, Color::white(0.75));
                ui.button(ex + 180.0, *ry, (ew - 200.0).min(320.0), 30.0, value, a);
                *ry += 40.0;
            };
            row(ui, &mut ry, "Effect", e.kind.name(), Act::Effect);
            let target = match &l.target {
                Target::All => "All devices".to_string(),
                Target::Devices(d) => {
                    let names: Vec<String> = d
                        .iter()
                        .map(|k| {
                            self.devices
                                .iter()
                                .find(|x| x.key() == *k)
                                .map_or(k.split('|').next().unwrap_or(k).to_string(), |x| x.name.clone())
                        })
                        .collect();
                    names.join(", ")
                }
                Target::Zones(z) => format!("{} zones", z.len()),
            };
            row(ui, &mut ry, "Lights", &target, Act::Target);
            let (lo, hi) = e.kind.colours();
            if hi > 0 {
                p.text("Colours", &f.body, ex + 16.0, ry, 160.0, 30.0, Align::Left, Color::white(0.75));
                for (i, c) in e.colours.iter().enumerate() {
                    let sx = ex + 180.0 + i as f32 * 40.0;
                    p.rrect(sx, ry, 32.0, 30.0, 6.0, rgb_of(*c));
                    p.stroke_rrect(sx + 0.5, ry + 0.5, 31.0, 29.0, 6.0, Color::white(0.25), 1.0);
                    ui.hits.push((sx, ry, 32.0, 30.0, Act::Colour(i)));
                }
                if e.colours.len() < hi {
                    let sx = ex + 180.0 + e.colours.len() as f32 * 40.0;
                    ui.button(sx, ry, 32.0, 30.0, "+", Act::AddColour);
                }
                let _ = lo;
                ry += 40.0;
            }
            if e.kind.moves() || e.kind == Kind::Gradient {
                ui.slider(
                    ex + 16.0,
                    ry,
                    (ew - 32.0).min(500.0),
                    "Speed",
                    e.speed,
                    &format!("{:.0}%", e.speed * 100.0),
                    Act::Slider(Sl::Speed),
                );
                ry += 52.0;
                ui.slider(
                    ex + 16.0,
                    ry,
                    (ew - 32.0).min(500.0),
                    "Size",
                    e.size,
                    &format!("{:.0}%", e.size * 100.0),
                    Act::Slider(Sl::Size),
                );
                ry += 52.0;
                p.text("Direction", &f.body, ex + 16.0, ry, 160.0, 30.0, Align::Left, Color::white(0.75));
                for (i, (d, n)) in [
                    (Dir::Right, "\u{2192}"),
                    (Dir::Left, "\u{2190}"),
                    (Dir::Down, "\u{2193}"),
                    (Dir::Up, "\u{2191}"),
                    (Dir::Out, "Out"),
                    (Dir::In, "In"),
                ]
                .iter()
                .enumerate()
                {
                    let bx = ex + 180.0 + i as f32 * 48.0;
                    let on = e.dir == *d;
                    p.rrect(bx, ry, 44.0, 30.0, 6.0, if on { accent.alpha(0.5) } else { Color::white(0.07) });
                    p.text(n, &f.body, bx, ry, 44.0, 30.0, Align::Center, Color::white(0.9));
                    ui.hits.push((bx, ry, 44.0, 30.0, Act::Dir(*d)));
                }
                ry += 40.0;
            }
            if e.kind == Kind::Temperature {
                row(ui, &mut ry, "Sensor", if e.sensor == Sensor::Cpu { "CPU" } else { "Graphics card" }, Act::Sensor);
                let (a, b) = e.temp_range;
                ui.slider(
                    ex + 16.0,
                    ry,
                    (ew - 32.0).min(500.0),
                    "Cool at",
                    (a - 20.0) / 80.0,
                    &format!("{a:.0} \u{b0}C"),
                    Act::Slider(Sl::TempLo),
                );
                ry += 52.0;
                ui.slider(
                    ex + 16.0,
                    ry,
                    (ew - 32.0).min(500.0),
                    "Hot at",
                    (b - 20.0) / 80.0,
                    &format!("{b:.0} \u{b0}C"),
                    Act::Slider(Sl::TempHi),
                );
                ry += 52.0;
            }
            ui.slider(
                ex + 16.0,
                ry,
                (ew - 32.0).min(500.0),
                "Opacity",
                l.opacity,
                &format!("{:.0}%", l.opacity * 100.0),
                Act::Slider(Sl::Opacity),
            );
            ry += 52.0;
            let blend = match l.blend {
                Blend::Normal => "Normal",
                Blend::Add => "Add",
                Blend::Screen => "Screen",
                Blend::Multiply => "Multiply",
                Blend::Lighten => "Lighten",
            };
            row(ui, &mut ry, "Over the layers below", blend, Act::Blend);
        } else {
            p.text(
                "Pick a layer, or add one with +.",
                &f.body,
                ex + 16.0,
                y + 16.0,
                ew - 32.0,
                24.0,
                Align::Left,
                Color::white(0.5),
            );
        }
        // The preview: every device as laid out, lit live.
        let prev = (ex, y + top_h + 10.0, ew, h - top_h - 10.0);
        *canvas = prev;
        self.draw_canvas(ui, prev, false);
    }

    /// The devices on the canvas, lit as the profile lights them now.
    fn draw_canvas(&self, ui: &mut Ui, (x, y, w, h): (f32, f32, f32, f32), editing: bool) {
        let p = ui.p;
        p.rrect(x, y, w, h, 8.0, Color(0.07, 0.07, 0.075, 1.0));
        p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 8.0, LINE, 1.0);
        ui.hits.push((x, y, w, h, Act::Canvas));
        let pr = self.p();
        let ctx = Context {
            t: self.start.elapsed().as_secs_f32(),
            cpu_temp: 55.0,
            gpu_temp: 50.0,
            level: 0.5,
            bands: vec![0.4; 16],
            ..Default::default()
        };
        let bright = (self.l.settings.brightness * pr.brightness).clamp(0.0, 1.0);
        for (di, d) in self.devices.iter().enumerate() {
            let key = d.key();
            let place = self.l.layout.get(&key).copied().unwrap_or_default();
            if place.off && !editing {
                continue;
            }
            let (bx, by, bw, bh) = (x + place.x * w, y + place.y * h, place.w * w, place.h * h);
            let sel = editing && di == self.device;
            p.rrect(bx, by, bw, bh, 6.0, Color::white(if place.off { 0.02 } else { 0.05 }));
            p.stroke_rrect(
                bx + 0.5,
                by + 0.5,
                bw - 1.0,
                bh - 1.0,
                6.0,
                if sel { theme::accent() } else { Color::white(0.15) },
                if sel { 2.0 } else { 1.0 },
            );
            if editing {
                p.text(&d.name, &ui.f.small, bx + 4.0, by - 16.0, bw.max(160.0), 14.0, Align::Left, Color::white(0.6));
                p.rrect(bx + bw - 8.0, by + bh - 8.0, 8.0, 8.0, 2.0, Color::white(0.5));
            }
            let hw = pr.hardware.get(&key);
            let dot = (bw.min(bh) / 6.0).clamp(2.0, 6.0);
            for (led, zone) in led_positions(d, &place) {
                let c = if hw.is_some() {
                    Rgb(0.35, 0.35, 0.38)
                } else {
                    let mut c = Rgb::BLACK;
                    for l in pr.layers.iter().filter(|l| l.on && l.covers(&key, zone)) {
                        c = effects::blend(c, effects::colour(&l.effect, &led, &ctx), l.blend, l.opacity);
                    }
                    c.scale(bright)
                };
                let (lx, ly) = (x + led.x * w, y + led.y * h);
                p.circle(lx, ly, dot, Color(c.0, c.1, c.2, if place.off { 0.2 } else { 1.0 }));
            }
        }
        if self.devices.is_empty() {
            p.text(&self.status, &ui.f.body, x, y + h / 2.0 - 10.0, w, 20.0, Align::Center, Color::white(0.45));
        }
    }

    fn draw_devices(&self, ui: &mut Ui, x: f32, y: f32, w: f32, _h: f32) {
        let (p, f) = (ui.p, ui.f);
        let Some(d) = self.devices.get(self.device) else {
            p.text("No devices yet.", &f.body, x, y, w, 24.0, Align::Left, Color::white(0.5));
            return;
        };
        let key = d.key();
        p.text(&d.name, &f.big, x, y, w, 40.0, Align::Left, Color::white(0.95));
        let info = format!(
            "{} \u{b7} {} \u{b7} {} LEDs \u{b7} {}",
            openrgb::kind_name(d.kind),
            if d.vendor.is_empty() { "Unknown maker" } else { &d.vendor },
            d.leds.len(),
            d.location
        );
        p.text(&info, &f.small, x, y + 44.0, w, 18.0, Align::Left, Color::white(0.55));
        let mut ry = y + 76.0;
        p.rrect(x, ry, w, 300.0, 8.0, PANEL);
        ry += 12.0;
        let hw = self.p().hardware.get(&key).cloned();
        p.text("Lighting", &f.body, x + 16.0, ry, 160.0, 30.0, Align::Left, Color::white(0.75));
        ui.button(
            x + 180.0,
            ry,
            340.0,
            30.0,
            hw.as_ref().map_or("Cascade's lighting (the profile's layers)", |h| h.mode.as_str()),
            Act::HwMode,
        );
        ry += 40.0;
        if let Some(h) = &hw {
            if let Some(m) = d.modes.iter().find(|m| m.name == h.mode) {
                if m.colors_max > 0 {
                    p.text("Colours", &f.body, x + 16.0, ry, 160.0, 30.0, Align::Left, Color::white(0.75));
                    let n = (h.colours.len() as u32).clamp(m.colors_min.max(1), m.colors_max) as usize;
                    for i in 0..n {
                        let sx = x + 180.0 + i as f32 * 40.0;
                        p.rrect(sx, ry, 32.0, 30.0, 6.0, rgb_of(h.colours.get(i).copied().unwrap_or(0xFFFFFF)));
                        ui.hits.push((sx, ry, 32.0, 30.0, Act::HwColour(i)));
                    }
                    if (n as u32) < m.colors_max {
                        ui.button(x + 180.0 + n as f32 * 40.0, ry, 32.0, 30.0, "+", Act::HwColour(n));
                    }
                    ry += 40.0;
                }
                if m.flags & openrgb::MODE_HAS_SPEED != 0 {
                    ui.slider(
                        x + 16.0,
                        ry,
                        (w - 32.0).min(500.0),
                        "Speed",
                        h.speed,
                        &format!("{:.0}%", h.speed * 100.0),
                        Act::Slider(Sl::HwSpeed),
                    );
                    ry += 52.0;
                }
                if m.flags & openrgb::MODE_HAS_BRIGHTNESS != 0 {
                    ui.slider(
                        x + 16.0,
                        ry,
                        (w - 32.0).min(500.0),
                        "Brightness",
                        h.brightness,
                        &format!("{:.0}%", h.brightness * 100.0),
                        Act::Slider(Sl::HwBright),
                    );
                    ry += 52.0;
                }
                if m.flags
                    & (openrgb::MODE_HAS_DIRECTION_LR | openrgb::MODE_HAS_DIRECTION_UD | openrgb::MODE_HAS_DIRECTION_HV)
                    != 0
                {
                    let names = ["Left", "Right", "Up", "Down", "Horizontal", "Vertical"];
                    p.text("Direction", &f.body, x + 16.0, ry, 160.0, 30.0, Align::Left, Color::white(0.75));
                    ui.button(
                        x + 180.0,
                        ry,
                        200.0,
                        30.0,
                        names.get(h.direction as usize).copied().unwrap_or("Left"),
                        Act::HwDir,
                    );
                    ry += 40.0;
                }
                if m.flags & (openrgb::MODE_MANUAL_SAVE | openrgb::MODE_AUTOMATIC_SAVE) != 0 {
                    p.text(
                        "Save to the device (keeps it with Cascade closed)",
                        &f.body,
                        x + 16.0,
                        ry,
                        420.0,
                        30.0,
                        Align::Left,
                        Color::white(0.75),
                    );
                    ui.toggle(x + 440.0, ry + 6.0, h.save, Act::HwSave);
                }
            }
        } else {
            p.text_wrap("The device follows the profile's layers. Pick one of its own modes above to leave it to the device (runs with no software, can be saved to it).", &f.small, x + 16.0, ry, w - 32.0, 40.0, Color::white(0.55));
        }
        // Zones, and the size of the ones that can be set (fan and strip headers).
        let mut zy = y + 390.0;
        p.text("Zones", &f.label, x, zy, 200.0, 20.0, Align::Left, Color::white(0.55));
        zy += 24.0;
        for (i, z) in d.zones.iter().enumerate() {
            if z.leds_max > z.leds_min {
                let t = (z.leds_count - z.leds_min) as f32 / (z.leds_max - z.leds_min) as f32;
                ui.slider(
                    x,
                    zy,
                    (w - 16.0).min(500.0),
                    &format!("{} (LEDs)", z.name),
                    t,
                    &z.leds_count.to_string(),
                    Act::Slider(Sl::Zone(i)),
                );
                zy += 52.0;
            } else {
                p.text(
                    &format!("{} \u{b7} {} LEDs", z.name, z.leds_count),
                    &f.body,
                    x,
                    zy,
                    w,
                    24.0,
                    Align::Left,
                    Color::white(0.8),
                );
                zy += 28.0;
            }
        }
        let place = self.l.layout.get(&key).copied().unwrap_or_default();
        p.text("Lay out as a ring (fans)", &f.body, x, zy + 8.0, 300.0, 24.0, Align::Left, Color::white(0.8));
        ui.toggle(x + 320.0, zy + 10.0, place.ring, Act::Toggle(3));
        p.text("Flip", &f.body, x, zy + 40.0, 300.0, 24.0, Align::Left, Color::white(0.8));
        ui.toggle(x + 320.0, zy + 42.0, place.flip, Act::Toggle(4));
    }

    fn draw_settings(&self, ui: &mut Ui, x: f32, y: f32, w: f32) {
        let (p, f) = (ui.p, ui.f);
        let s = &self.l.settings;
        let mut ry = y;
        let line = |ui: &mut Ui, ry: &mut f32, label: &str, on: bool, a: Act| {
            ui.p.text(label, &ui.f.body, x, *ry, w - 80.0, 30.0, Align::Left, Color::white(0.85));
            ui.toggle(x + (w - 60.0).min(560.0), *ry + 6.0, on, a);
            *ry += 40.0;
        };
        line(ui, &mut ry, "Lighting on", s.on, Act::Toggle(0));
        ui.slider(
            x,
            ry,
            (w - 20.0).min(600.0),
            "Brightness",
            s.brightness,
            &format!("{:.0}%", s.brightness * 100.0),
            Act::Slider(Sl::Bright),
        );
        ry += 52.0;
        ui.slider(
            x,
            ry,
            (w - 20.0).min(600.0),
            "Frames a second (moving effects)",
            (s.fps as f32 - 5.0) / 115.0,
            &s.fps.to_string(),
            Act::Slider(Sl::Fps),
        );
        ry += 52.0;
        ui.slider(
            x,
            ry,
            (w - 20.0).min(600.0),
            "Screen ambience: looks at the screen",
            (s.screen_fps as f32 - 1.0) / 59.0,
            &format!("{} a second", s.screen_fps),
            Act::Slider(Sl::ScreenFps),
        );
        ry += 52.0;
        line(ui, &mut ry, "Switch profiles with the program in front", s.auto_switch, Act::Toggle(1));
        line(ui, &mut ry, "Lights off while the PC is locked or its screens are off", s.off_when_idle, Act::Toggle(2));
        ry += 10.0;
        p.text("OpenRGB", &f.title, x, ry, 300.0, 24.0, Align::Left, Color::white(0.95));
        ry += 30.0;
        let found = super::hw::find(&s.openrgb);
        let where_ = found.as_ref().map_or("Not on this PC yet".to_string(), |p| p.display().to_string());
        p.text_wrap(
            &format!("Cascade drives your devices through OpenRGB (free and open source, openrgb.org), which talks to each one directly: Corsair, ASUS Aura, Gigabyte RGB Fusion, Razer, Logitech, HyperX, MSI, NZXT, SteelSeries and hundreds more. It runs out of sight. Where: {where_}"),
            &f.small,
            x,
            ry,
            (w - 20.0).min(700.0),
            48.0,
            Color::white(0.6),
        );
        ry += 54.0;
        ui.button(x, ry, 170.0, 30.0, if found.is_some() { "Get it again" } else { "Get OpenRGB" }, Act::GetOpenRgb);
        ui.button(x + 180.0, ry, 190.0, 30.0, "Use another copy\u{2026}", Act::ChooseOpenRgb);
        ui.button(x + 380.0, ry, 160.0, 30.0, "Look for devices", Act::Rescan);
        ry += 44.0;
        line(ui, &mut ry, "Administrator rights (RAM, motherboard and graphics card lighting)", s.admin, Act::Admin);
        p.text(
            "Those also need the PawnIO driver:",
            &f.small,
            x,
            ry - 6.0,
            260.0,
            20.0,
            Align::Left,
            Color::white(0.55),
        );
        ui.button(x + 250.0, ry - 10.0, 120.0, 26.0, "pawnio.eu", Act::PawnIo);
        ry += 30.0;
        if !self.rivals.is_empty() {
            p.rrect(x, ry, (w - 20.0).min(700.0), 64.0, 8.0, Color(0.45, 0.25, 0.05, 0.5));
            p.text_wrap(
                &format!(
                    "Running now: {}. These hold their devices and fight Cascade (and OpenRGB) for them: close them and stop them starting with Windows (or uninstall them).",
                    self.rivals.join(", ")
                ),
                &f.small,
                x + 12.0,
                ry + 8.0,
                (w - 44.0).min(676.0),
                50.0,
                Color::white(0.9),
            );
        }
    }
}

/// Immediate-mode controls: drawn, and their hit areas noted for the click.
struct Ui<'a, 'b> {
    p: &'a Painter<'b>,
    f: &'a Fonts,
    hits: &'a mut Vec<(f32, f32, f32, f32, Act)>,
}

impl Ui<'_, '_> {
    fn button(&mut self, x: f32, y: f32, w: f32, h: f32, label: &str, a: Act) {
        self.p.rrect(x, y, w, h, 6.0, Color::white(0.08));
        self.p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 6.0, Color::white(0.06), 1.0);
        self.p.text(label, &self.f.body, x + 8.0, y, w - 16.0, h, Align::Center, Color::white(0.9));
        self.hits.push((x, y, w, h, a));
    }

    fn toggle(&mut self, x: f32, y: f32, on: bool, a: Act) {
        let (w, h) = (36.0, 18.0);
        self.p.rrect(x, y, w, h, h / 2.0, if on { theme::accent() } else { Color::white(0.12) });
        let kx = if on { x + w - h / 2.0 } else { x + h / 2.0 };
        self.p.circle(kx, y + h / 2.0, h / 2.0 - 3.0, Color::white(0.95));
        self.hits.push((x - 4.0, y - 4.0, w + 8.0, h + 8.0, a));
    }

    fn slider(&mut self, x: f32, y: f32, w: f32, label: &str, t: f32, value: &str, a: Act) {
        self.p.text(label, &self.f.body, x, y, w - 100.0, 20.0, Align::Left, Color::white(0.75));
        self.p.text(value, &self.f.small, x + w - 100.0, y, 100.0, 20.0, Align::Right, Color::white(0.6));
        let ty = y + 30.0;
        let t = t.clamp(0.0, 1.0);
        self.p.rrect(x, ty, w, 4.0, 2.0, Color::white(0.12));
        self.p.rrect(x, ty, w * t, 4.0, 2.0, theme::accent());
        self.p.circle(x + w * t, ty + 2.0, 7.0, Color::white(0.95));
        self.hits.push((x, ty - 10.0, w, 24.0, a));
    }
}

unsafe extern "system" fn rename_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM, _: usize, _: usize) -> LRESULT {
    let done = |keep: bool| {
        let mut buf = [0u16; 128];
        let n = GetWindowTextW(hwnd, &mut buf) as usize;
        let name = String::from_utf16_lossy(&buf[..n]).trim().to_string();
        let parent = GetParent(hwnd).ok();
        let _ = DestroyWindow(hwnd);
        if keep && !name.is_empty() {
            with(|s| {
                s.profile().name = name;
                s.changed();
            });
        }
        if let Some(p) = parent {
            let _ = SetFocus(Some(p));
        }
    };
    match m {
        WM_KEYDOWN if wp.0 == 0x0D || wp.0 == 0x1B => {
            done(wp.0 == 0x0D);
            return LRESULT(0);
        }
        WM_CHAR if wp.0 == 0x0D || wp.0 == 0x1B => return LRESULT(0),
        WM_KILLFOCUS => {
            done(true);
            return LRESULT(0);
        }
        _ => {}
    }
    windows::Win32::UI::Shell::DefSubclassProc(hwnd, m, wp, lp)
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
        WM_TIMER if wp.0 == TICK => {
            // The preview moves with the effects.
            with(|s| {
                if matches!(s.tab, Tab::Lighting | Tab::Layout)
                    && s.p().layers.iter().any(|l| l.on && l.effect.kind.moves())
                {
                    appwin::redraw(hwnd);
                }
            });
        }
        WM_TIMER if wp.0 == SAVE => {
            let _ = KillTimer(Some(hwnd), SAVE);
            with(|s| s.l.save());
        }
        DEVICES => {
            with(|s| {
                if wp.0 != 0 {
                    // OpenRGB was just got or picked: start it and look again.
                    s.l.settings.openrgb = Lighting::load().settings.openrgb;
                    s.busy = false;
                    let _ = FETCHED.lock().map(|mut f| f.take());
                    s.fetch();
                } else {
                    s.fetched();
                }
            });
        }
        WM_LBUTTONDOWN | WM_RBUTTONUP => {
            let (x, y) = appwin::mouse(hwnd, lp);
            let right = m == WM_RBUTTONUP;
            let hit = with(|s| {
                s.hits.iter().rev().find(|h| x >= h.0 && x < h.0 + h.2 && y >= h.1 && y < h.1 + h.3).map(|h| h.4)
            })
            .flatten();
            match hit {
                Some(Act::Canvas) => {
                    with(|s| s.canvas_down(x, y, right));
                }
                Some(a) => {
                    with(|s| s.act(a, x, right));
                }
                None => {}
            }
            if !right && with(|s| s.drag.is_some()) == Some(true) {
                SetCapture(hwnd);
            }
        }
        WM_MOUSEMOVE => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| s.drag_to(x, y));
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            with(|s| {
                s.release();
                s.changed();
            });
        }
        WM_DESTROY => {
            with(|s| s.l.save());
            PostQuitMessage(0);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, m, wp, lp)
}

impl State {
    /// A press on the canvas: pick up a device (by its corner, to resize), or its menu.
    fn canvas_down(&mut self, x: f32, y: f32, right: bool) {
        let (cx, cy, cw, ch) = self.canvas;
        let (u, v) = ((x - cx) / cw, (y - cy) / ch);
        let editing = self.tab == Tab::Layout;
        let found = self.devices.iter().enumerate().rev().find_map(|(i, d)| {
            let p = self.l.layout.get(&d.key()).copied().unwrap_or_default();
            (u >= p.x && u <= p.x + p.w && v >= p.y && v <= p.y + p.h).then(|| (i, d.key(), p))
        });
        let Some((i, key, p)) = found else { return };
        self.device = i;
        if !editing {
            return;
        }
        if right {
            use crate::menu::{check, item, show, Entry, Place as MP};
            let entries = vec![
                check(1, "Ring (a fan)", p.ring),
                check(2, "Flipped", p.flip),
                check(3, "Left out of the lighting", p.off),
                Entry::Separator,
                item(4, "Settings for this device"),
            ];
            match show(self.hwnd, entries, MP::Cursor) {
                1 => self.l.layout.entry(key).or_default().ring = !p.ring,
                2 => self.l.layout.entry(key).or_default().flip = !p.flip,
                3 => self.l.layout.entry(key).or_default().off = !p.off,
                4 => self.tab = Tab::Devices,
                _ => {}
            }
            self.changed();
            return;
        }
        let corner = (u - (p.x + p.w)).abs() * cw < 12.0 && (v - (p.y + p.h)).abs() * ch < 12.0;
        self.drag = Some(Drag::Place(key, u - p.x, v - p.y, corner));
        appwin::redraw(self.hwnd);
    }

    fn drag_to(&mut self, x: f32, y: f32) {
        match &self.drag {
            Some(Drag::Slider(s, sx, sw)) => {
                let (s, t) = (*s, ((x - sx) / sw).clamp(0.0, 1.0));
                self.slide(s, t);
            }
            Some(Drag::Place(key, gx, gy, resize)) => {
                let (cx, cy, cw, ch) = self.canvas;
                let (u, v) = (((x - cx) / cw).clamp(0.0, 1.0), ((y - cy) / ch).clamp(0.0, 1.0));
                let (key, gx, gy, resize) = (key.clone(), *gx, *gy, *resize);
                let p: &mut Place = self.l.layout.entry(key).or_default();
                if resize {
                    p.w = (u - p.x).max(0.02);
                    p.h = (v - p.y).max(0.02);
                } else {
                    p.x = (u - gx).clamp(0.0, 1.0 - p.w);
                    p.y = (v - gy).clamp(0.0, 1.0 - p.h);
                }
                self.changed();
            }
            None => {}
        }
    }
}

/// `cascade.exe --rgb`.
pub fn run() {
    appwin::init();
    let Ok(gfx) = Gfx::new() else { return };
    let Some(hwnd) = appwin::create(w!("CascadeLighting"), "Lighting", 1240, 820, Some(proc)) else { return };
    let mut l = Lighting::load();
    if l.profiles.is_empty() {
        l.profiles.push(Profile::default());
    }
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            hwnd,
            gfx: Some(gfx),
            l,
            devices: Vec::new(),
            status: String::new(),
            tab: Tab::Lighting,
            layer: 0,
            device: 0,
            hits: Vec::new(),
            drag: None,
            start: Instant::now(),
            rivals: Vec::new(),
            busy: false,
            canvas: (0.0, 0.0, 1.0, 1.0),
        })
    });
    unsafe {
        SetTimer(Some(hwnd), TICK, 33, None);
    }
    with(|s| {
        s.layer = s.p().layers.len().saturating_sub(1);
        s.fetch();
    });
    appwin::run();
}
