//! Lighting (`cascade.exe --rgb`): iCUE without iCUE, for every brand.
//!
//! - **Profiles** down the side: make, rename, duplicate, delete; pick one to
//!   use and edit; link programs to one so it comes on while they're in front.
//! - **Lighting**: the profile's effect layers (top one over the rest), each
//!   with its effect, the devices it covers, colours, speed, size, direction,
//!   opacity and blend, and a live preview of every device.
//! - **Layout**: drag the devices where they sit on your desk (and resize
//!   them), so a wave runs across them all as one; fans as rings; flip one.
//! - **Devices**: every device as a card; open one for its own hardware
//!   modes (colours, speed, brightness, direction, saved to the device) and
//!   how many LEDs each fan or strip header has.
//! - **Settings**: on or off, brightness, frame rate, switching with apps,
//!   off when locked; OpenRGB, and administrator rights for RAM, motherboard
//!   and graphics card lighting (which installs the PawnIO driver too).
//!
//! OpenRGB is set up by itself the first time (fetched, unpacked, started),
//! and a link thread stays connected to it, so devices appear as OpenRGB
//! finds them. Every change is saved as it's made; the engine (see `engine`)
//! picks it up. Every control lights on hover and press and moves smoothly
//! (see `Anim`); pages slide in; Esc, the mouse's back button and the arrow
//! at the top go back.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::effects::{self, Blend, Context, Dir, Kind, Rgb, Sensor};
use super::model::{led_positions, Hardware, Layer, Lighting, Place, Profile, Target};
use super::openrgb::{self, Client, Controller};
use crate::appwin::{self, BG, LINE, PANEL};
use crate::gfx::{Align, Color, Fonts, Gfx, Painter};
use crate::theme;

const SIDE: f32 = 236.0;
const ROW: f32 = 38.0;
const PAD: f32 = 28.0;
/// The link thread has news (devices, or how setting up is going).
const LINK_MSG: u32 = WM_APP + 1;
/// Administrator rights: a step, or done.
const ADMIN_MSG: u32 = WM_APP + 2;
const TICK: usize = 1;
const SAVE: usize = 2;

// Glyphs from Windows' icon font (Segoe Fluent Icons / MDL2 Assets).
const G_BACK: u32 = 0xE72B;
const G_LIGHT: u32 = 0xE706;
const G_LAYOUT: u32 = 0xE8A9;
const G_DEVICES: u32 = 0xE772;
const G_SETTINGS: u32 = 0xE713;
const G_ADD: u32 = 0xE710;
const G_DELETE: u32 = 0xE74D;
const G_UP: u32 = 0xE70E;
const G_DOWN: u32 = 0xE70D;
const G_EYE: u32 = 0xE890;
const G_HIDDEN: u32 = 0xED1A;
const G_MORE: u32 = 0xE712;
const G_CHEVRON: u32 = 0xE70D;
const G_REFRESH: u32 = 0xE72C;
const G_DOWNLOAD: u32 = 0xE896;
const G_SHIELD: u32 = 0xEA18;
const G_WARN: u32 = 0xE7BA;
const G_CHECK: u32 = 0xE73E;
const G_FOLDER: u32 = 0xE8B7;
const G_LINK: u32 = 0xE71B;
const G_KEYBOARD: u32 = 0xE765;
const G_MOUSE: u32 = 0xE962;
const G_HEADSET: u32 = 0xE7F6;
const G_SPEAKER: u32 = 0xE7F5;
const G_GAMEPAD: u32 = 0xE7FC;
const G_CHIP: u32 = 0xE950;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Page {
    Effects,
    Layout,
    Devices,
    Device(usize),
    Settings,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Effects => "Lighting",
            Page::Layout => "Layout",
            Page::Devices | Page::Device(_) => "Devices",
            Page::Settings => "Settings",
        }
    }

    /// Which item in the side lights for it.
    fn nav(self) -> usize {
        match self {
            Page::Effects => 0,
            Page::Layout => 1,
            Page::Devices | Page::Device(_) => 2,
            Page::Settings => 3,
        }
    }
}

const NAV: [(Page, u32); 4] =
    [(Page::Effects, G_LIGHT), (Page::Layout, G_LAYOUT), (Page::Devices, G_DEVICES), (Page::Settings, G_SETTINGS)];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
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

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Act {
    Go(Page),
    Back,
    Profile(usize),
    ProfileMore(usize),
    NewProfile,
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
    Retry,
    ChooseOpenRgb,
    Admin,
    Rescan,
    Canvas,
}

/// What the link thread is doing.
#[derive(Clone, PartialEq, Debug)]
enum Phase {
    Looking,
    Downloading(f32),
    Starting,
    Connected,
    Failed(String),
}

struct Link {
    phase: Phase,
    devices: Option<Vec<Controller>>,
    since: Option<Instant>,
}

static LINK: Mutex<Link> = Mutex::new(Link { phase: Phase::Looking, devices: None, since: None });
static ADMIN_STEP: Mutex<Option<Result<String, String>>> = Mutex::new(None);
/// Asks for the link thread: 1 try setting up again, 2 look for devices again.
static ASK: AtomicU32 = AtomicU32::new(0);
const ASK_RETRY: u32 = 1;
const ASK_RESCAN: u32 = 2;

enum Drag {
    Slider(Sl, f32, f32),
    /// A device on the canvas: its key, where it was grabbed (canvas units), resizing.
    Place(String, f32, f32, bool),
}

type Hit = (f32, f32, f32, f32, Act);

/// Smoothly moving values, keyed by what they belong to: each frame a
/// value moves part of the way to where it's going (fast at first, then
/// settling), so hovers fade, toggles slide and selections glide.
struct Anim {
    values: RefCell<HashMap<u64, f32>>,
    /// Something is still moving to where it's going (frames at 60 a second).
    busy: Cell<bool>,
    /// Something moves for as long as it's shown, gently (a spinner, a pulse: 30 a second).
    live: Cell<bool>,
    dt: Cell<f32>,
}

impl Anim {
    fn to(&self, key: u64, target: f32, rate: f32) -> f32 {
        let mut map = self.values.borrow_mut();
        let v = map.entry(key).or_insert(target);
        let k = 1.0 - (-rate * self.dt.get()).exp();
        *v += (target - *v) * k;
        if (target - *v).abs() < 0.002 {
            *v = target;
        } else {
            self.busy.set(true);
        }
        *v
    }

    /// Starts `key` from `from` (it then moves to wherever it's asked to go).
    fn set(&self, key: u64, from: f32) {
        self.values.borrow_mut().insert(key, from);
    }
}

fn key(what: impl Hash, slot: u8) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    what.hash(&mut h);
    slot.hash(&mut h);
    h.finish()
}

const K_HOVER: u8 = 1;
const K_PRESS: u8 = 2;
const K_ON: u8 = 3;
const K_POS: u8 = 4;

struct State {
    hwnd: HWND,
    gfx: Option<Gfx>,
    l: Lighting,
    devices: Vec<Controller>,
    phase: Phase,
    connected_at: Option<Instant>,
    page: Page,
    history: Vec<Page>,
    page_since: Instant,
    layer: usize,
    device: usize,
    hits: Vec<Hit>,
    hover: Option<Act>,
    hover_dev: Option<usize>,
    pressed: Option<Act>,
    drag: Option<Drag>,
    start: Instant,
    last_frame: Instant,
    rivals: Vec<&'static str>,
    admin_busy: Option<String>,
    admin_error: Option<String>,
    pawnio: bool,
    scroll: f32,
    content_h: Cell<f32>,
    view: Cell<(f32, f32, f32, f32)>,
    canvas: Cell<(f32, f32, f32, f32)>,
    anim: Anim,
    timer: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

fn rgb_of(c: u32) -> Color {
    Color(((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0, 1.0)
}

fn glyph(code: u32) -> String {
    char::from_u32(code).map(String::from).unwrap_or_default()
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t, a.3 + (b.3 - a.3) * t)
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// The icon for a kind of device, as OpenRGB numbers them.
fn device_glyph(kind: i32) -> u32 {
    match kind {
        5 | 18 => G_KEYBOARD,
        6 | 7 => G_MOUSE,
        8 | 9 => G_HEADSET,
        10 => G_GAMEPAD,
        12 => G_SPEAKER,
        4 | 11 => G_LIGHT,
        _ => G_CHIP,
    }
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

// ------------------------------------------------------------- the link --

fn publish(hwnd: isize, f: impl FnOnce(&mut Link)) {
    f(&mut LINK.lock().unwrap_or_else(|e| e.into_inner()));
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), LINK_MSG, WPARAM(0), LPARAM(0));
    }
}

fn set_phase(hwnd: isize, p: Phase) {
    publish(hwnd, |l| l.phase = p);
}

/// Waits up to `ms`, ending early when the window asks for something.
fn nap(ms: u64) -> u32 {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        let a = ASK.swap(0, Ordering::AcqRel);
        if a != 0 {
            return a;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    0
}

/// Sets OpenRGB up when it isn't there, starts it, and stays connected:
/// devices are sent to the window as OpenRGB finds them.
fn link_thread(hwnd: isize) {
    let mut tried_download = false;
    let mut last_start: Option<Instant> = None;
    loop {
        let s = Lighting::load().settings;
        if !super::hw::running(s.port) {
            if super::hw::find(&s.openrgb).is_none() {
                if tried_download {
                    if nap(1000) & ASK_RETRY != 0 {
                        tried_download = false;
                    }
                    continue;
                }
                tried_download = true;
                set_phase(hwnd, Phase::Downloading(0.0));
                let r = super::hw::download(&|t| {
                    LINK.lock().unwrap_or_else(|e| e.into_inner()).phase = Phase::Downloading(t);
                    unsafe {
                        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), LINK_MSG, WPARAM(0), LPARAM(0));
                    }
                });
                if let Err(e) = r {
                    crate::config::log(&format!("lighting: getting OpenRGB failed: {e}"));
                    set_phase(hwnd, Phase::Failed(format!("Couldn't get OpenRGB: {e}.")));
                }
                continue;
            }
            // Started now and then; meanwhile it's given time to come up.
            if last_start.is_none_or(|t| t.elapsed() > Duration::from_secs(20)) {
                if last_start.is_some() {
                    set_phase(
                        hwnd,
                        Phase::Failed("OpenRGB didn't start. Another copy may have its port, or it's blocked.".into()),
                    );
                } else {
                    set_phase(hwnd, Phase::Starting);
                }
                last_start = Some(Instant::now());
                if s.on {
                    super::engine::ensure(true);
                }
                super::hw::start(&s.openrgb, s.port, s.admin);
            }
            if nap(400) & ASK_RETRY != 0 {
                last_start = None;
            }
            continue;
        }
        let mut c = match Client::connect(s.port, "Cascade Lighting") {
            Ok(c) => c,
            Err(e) => {
                set_phase(hwnd, Phase::Failed(format!("Couldn't connect to OpenRGB ({e}).")));
                nap(2000);
                continue;
            }
        };
        if s.on {
            super::engine::ensure(true);
        }
        let mut list = match c.controllers() {
            Ok(l) => l,
            Err(_) => continue,
        };
        publish(hwnd, |l| {
            l.phase = Phase::Connected;
            l.since = Some(Instant::now());
            l.devices = Some(list.clone());
        });
        let mut checked = Instant::now();
        loop {
            let ask = nap(400);
            if ask & ASK_RESCAN != 0 {
                let _ = c.rescan();
            }
            let mut fetch = c.take_list_changed();
            if checked.elapsed() > Duration::from_secs(3) {
                checked = Instant::now();
                match c.count() {
                    Ok(n) => fetch |= n as usize != list.len(),
                    Err(_) => break,
                }
            }
            if fetch {
                match c.controllers() {
                    Ok(l) => {
                        list = l;
                        publish(hwnd, |l| l.devices = Some(list.clone()));
                    }
                    Err(_) => break,
                }
            }
        }
        last_start = None;
        set_phase(hwnd, Phase::Starting);
    }
}

// ------------------------------------------------------------ the state --

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
            SetTimer(Some(self.hwnd), SAVE, 150, None);
        }
        appwin::redraw(self.hwnd);
    }

    fn go(&mut self, page: Page) {
        if page == self.page {
            return;
        }
        self.history.push(self.page);
        if self.history.len() > 20 {
            self.history.remove(0);
        }
        self.show(page);
    }

    fn back(&mut self) {
        if let Some(p) = self.history.pop() {
            self.show(p);
        }
    }

    fn show(&mut self, page: Page) {
        self.page = page;
        self.page_since = Instant::now();
        self.scroll = 0.0;
        self.anim.set(key("scroll", K_POS), 0.0);
        self.hover = None;
        appwin::redraw(self.hwnd);
    }

    fn linked(&mut self) {
        let (phase, devices, since) = {
            let mut l = LINK.lock().unwrap_or_else(|e| e.into_inner());
            (l.phase.clone(), l.devices.take(), l.since)
        };
        self.phase = phase;
        self.connected_at = since;
        if let Some(list) = devices {
            self.l.place_new(&list);
            self.devices = list;
            self.device = self.device.min(self.devices.len().saturating_sub(1));
            if let Page::Device(i) = self.page {
                if i >= self.devices.len() {
                    self.show(Page::Devices);
                }
            }
            self.rivals = super::hw::rivals();
            self.changed();
        }
        appwin::redraw(self.hwnd);
    }

    fn status(&self) -> (Color, String) {
        let n = self.devices.len();
        match &self.phase {
            Phase::Connected if n > 0 => {
                (Color(0.3, 0.85, 0.45, 1.0), format!("{n} device{}", if n == 1 { "" } else { "s" }))
            }
            Phase::Connected => (Color(0.95, 0.7, 0.25, 1.0), "Finding devices\u{2026}".into()),
            Phase::Downloading(t) => (theme::accent(), format!("Setting up\u{2026} {:.0}%", t * 100.0)),
            Phase::Looking | Phase::Starting => (Color(0.95, 0.7, 0.25, 1.0), "Starting OpenRGB\u{2026}".into()),
            Phase::Failed(_) => (Color(0.95, 0.35, 0.35, 1.0), "Not connected".into()),
        }
    }

    // ---------------------------------------------------------- actions --

    fn act(&mut self, a: Act, x: f32, right: bool) {
        use crate::menu::{check, item, show, Entry, Place as MP};
        let owner = self.hwnd;
        match a {
            Act::Go(p) => self.go(p),
            Act::Back => self.back(),
            Act::Profile(i) | Act::ProfileMore(i) if right || matches!(a, Act::ProfileMore(_)) => {
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
                self.layer = self.p().layers.len().saturating_sub(1);
            }
            Act::ProfileMore(_) => {}
            Act::NewProfile => {
                self.l
                    .profiles
                    .push(Profile { name: format!("Profile {}", self.l.profiles.len() + 1), ..Default::default() });
                self.l.active = self.l.profiles.len() - 1;
                self.layer = 0;
                self.go(Page::Effects);
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
                    if let Some(slot) = self.cur_layer_mut().and_then(|l| l.effect.colours.get_mut(i)) {
                        *slot = c;
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
                let Some(key) = self.devices.get(self.device).map(|d| d.key()) else { return };
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
                let dev = self.devices.get(self.device).map(|d| d.key());
                let s = &mut self.l.settings;
                match n {
                    0 => {
                        s.on = !s.on;
                        super::engine::ensure(s.on);
                    }
                    1 => s.auto_switch = !s.auto_switch,
                    2 => s.off_when_idle = !s.off_when_idle,
                    3 | 4 => {
                        if let Some(k) = dev {
                            let p = self.l.layout.entry(k).or_default();
                            if n == 3 {
                                p.ring = !p.ring;
                            } else {
                                p.flip = !p.flip;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Act::Retry => {
                ASK.fetch_or(ASK_RETRY, Ordering::AcqRel);
                self.phase = Phase::Looking;
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
                        ASK.fetch_or(ASK_RETRY, Ordering::AcqRel);
                        unsafe {
                            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), LINK_MSG, WPARAM(1), LPARAM(0));
                        }
                    }
                });
            }
            Act::Admin => {
                if self.admin_busy.is_some() {
                    return;
                }
                let on = !self.l.settings.admin;
                self.admin_error = None;
                self.admin_busy = Some("Starting\u{2026}".into());
                let hwnd = self.hwnd.0 as isize;
                std::thread::spawn(move || {
                    let post = |r: Result<String, String>, done: usize| {
                        *ADMIN_STEP.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                        unsafe {
                            let _ =
                                PostMessageW(Some(HWND(hwnd as *mut _)), ADMIN_MSG, WPARAM(done), LPARAM(on as isize));
                        }
                    };
                    let r = super::hw::set_admin(on, &|step| post(Ok(step), 0));
                    post(r.map(|_| String::new()), 1);
                });
            }
            Act::Rescan => {
                ASK.fetch_or(ASK_RESCAN | ASK_RETRY, Ordering::AcqRel);
                self.rivals = super::hw::rivals();
            }
            Act::Canvas => {}
        }
        self.changed();
    }

    fn admin_news(&mut self, done: bool, on: bool) {
        let step = ADMIN_STEP.lock().unwrap_or_else(|e| e.into_inner()).take();
        match (done, step) {
            (false, Some(Ok(s))) => self.admin_busy = Some(s),
            (true, Some(Ok(_))) => {
                self.admin_busy = None;
                self.l.settings.admin = on;
                self.pawnio = super::hw::pawnio_installed();
                self.l.save();
                ASK.fetch_or(ASK_RETRY, Ordering::AcqRel);
            }
            (true, Some(Err(e))) => {
                self.admin_busy = None;
                self.admin_error = Some(e);
                self.pawnio = super::hw::pawnio_installed();
            }
            _ => {}
        }
        appwin::redraw(self.hwnd);
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
            }
        }
        self.drag = None;
    }

    fn rename(&mut self) {
        let name = self.p().name.clone();
        let (k, i) = (appwin::scale(self.hwnd), self.l.active);
        let y = profiles_top() + i as f32 * ROW;
        unsafe {
            if let Ok(e) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                &HSTRING::from(name),
                WS_CHILD | WS_VISIBLE | WS_BORDER | WINDOW_STYLE(0x80),
                (14.0 * k) as i32,
                ((y + 4.0) * k) as i32,
                ((SIDE - 28.0) * k) as i32,
                ((ROW - 8.0) * k) as i32,
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
        let mut all: Vec<String> = linked.clone();
        for r in running_apps() {
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
        let mut entries = vec![check(1, "Cascade's lighting (the profile's effects)", cur.is_none()), Entry::Separator];
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

    // ------------------------------------------------------------ input --

    fn hit_at(&self, x: f32, y: f32) -> Option<Act> {
        self.hits.iter().rev().find(|h| x >= h.0 && x < h.0 + h.2 && y >= h.1 && y < h.1 + h.3).map(|h| h.4)
    }

    fn on_move(&mut self, x: f32, y: f32) {
        if self.drag.is_some() {
            self.drag_to(x, y);
            return;
        }
        let h = self.hit_at(x, y);
        let dev = if h == Some(Act::Canvas) { self.device_at(x, y).map(|(i, ..)| i) } else { None };
        if h != self.hover || dev != self.hover_dev {
            self.hover = h;
            self.hover_dev = dev;
            appwin::redraw(self.hwnd);
        }
    }

    fn on_down(&mut self, x: f32, y: f32) {
        let hit = self.hit_at(x, y);
        match hit {
            Some(Act::Canvas) => self.canvas_down(x, y, false),
            Some(a @ Act::Slider(_)) => self.act(a, x, false),
            Some(a) => self.pressed = Some(a),
            None => {}
        }
        appwin::redraw(self.hwnd);
    }

    fn on_up(&mut self, x: f32, y: f32) {
        let was = self.drag.is_some();
        self.release();
        if let Some(a) = self.pressed.take() {
            if self.hit_at(x, y) == Some(a) {
                self.act(a, x, false);
            }
        }
        if was {
            self.changed();
        }
        appwin::redraw(self.hwnd);
    }

    fn on_wheel(&mut self, delta: i32) {
        let (_, _, _, vh) = self.view.get();
        let max = (self.content_h.get() - vh).max(0.0);
        self.scroll = (self.scroll - delta as f32 / 120.0 * 72.0).clamp(0.0, max);
        appwin::redraw(self.hwnd);
    }

    fn device_at(&self, x: f32, y: f32) -> Option<(usize, String, Place)> {
        let (cx, cy, cw, ch) = self.canvas.get();
        let (u, v) = ((x - cx) / cw, (y - cy) / ch);
        self.devices.iter().enumerate().rev().find_map(|(i, d)| {
            let p = self.l.layout.get(&d.key()).copied().unwrap_or_default();
            let editing = self.page == Page::Layout;
            ((editing || !p.off) && u >= p.x && u <= p.x + p.w && v >= p.y && v <= p.y + p.h).then(|| (i, d.key(), p))
        })
    }

    /// A press on the canvas: pick up a device (by its corner, to resize), or its menu.
    fn canvas_down(&mut self, x: f32, y: f32, right: bool) {
        let Some((i, key, p)) = self.device_at(x, y) else { return };
        self.device = i;
        if self.page != Page::Layout {
            if right {
                self.go(Page::Device(i));
            }
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
                4 => self.go(Page::Device(i)),
                _ => {}
            }
            self.changed();
            return;
        }
        let (cx, cy, cw, ch) = self.canvas.get();
        let (u, v) = ((x - cx) / cw, (y - cy) / ch);
        let corner = (u - (p.x + p.w)).abs() * cw < 14.0 && (v - (p.y + p.h)).abs() * ch < 14.0;
        self.drag = Some(Drag::Place(key, u - p.x, v - p.y, corner));
        unsafe {
            SetCapture(self.hwnd);
        }
    }

    fn drag_to(&mut self, x: f32, y: f32) {
        match &self.drag {
            Some(Drag::Slider(s, sx, sw)) => {
                let (s, t) = (*s, ((x - sx) / sw).clamp(0.0, 1.0));
                self.slide(s, t);
            }
            Some(Drag::Place(key, gx, gy, resize)) => {
                let (cx, cy, cw, ch) = self.canvas.get();
                let (u, v) = (((x - cx) / cw).clamp(0.0, 1.0), ((y - cy) / ch).clamp(0.0, 1.0));
                let (key, gx, gy, resize) = (key.clone(), *gx, *gy, *resize);
                let p: &mut Place = self.l.layout.entry(key).or_default();
                if resize {
                    p.w = (u - p.x).max(0.03);
                    p.h = (v - p.y).max(0.03);
                } else {
                    p.x = (u - gx).clamp(0.0, 1.0 - p.w);
                    p.y = (v - gy).clamp(0.0, 1.0 - p.h);
                }
                self.changed();
            }
            None => {}
        }
    }

    fn cursor(&self) -> Option<PCWSTRCursor> {
        if let Some(Drag::Place(_, _, _, r)) = &self.drag {
            return Some(if *r { IDC_SIZENWSE } else { IDC_SIZEALL });
        }
        match self.hover {
            Some(Act::Canvas) if self.page == Page::Layout && self.hover_dev.is_some() => Some(IDC_SIZEALL),
            Some(Act::Canvas) => None,
            Some(_) => Some(IDC_HAND),
            None => None,
        }
    }

    // ---------------------------------------------------------- drawing --

    fn paint(&mut self) {
        let Some(mut gfx) = self.gfx.take() else { return };
        let now = Instant::now();
        // A long pause (nothing moving) counts as one frame, so a hover starting
        // after it still fades in rather than jumping.
        self.anim.dt.set(now.duration_since(self.last_frame).as_secs_f32().min(1.0 / 30.0));
        self.last_frame = now;
        self.anim.busy.set(false);
        self.anim.live.set(false);
        let hwnd = self.hwnd;
        let mut hits = Vec::new();
        {
            let me = &*self;
            appwin::paint(hwnd, &mut gfx, |p, f, w, h| {
                let mut ui = Ui { p, f, s: me, hits: &mut hits, clip: None };
                me.draw(&mut ui, w, h);
            });
        }
        self.hits = hits;
        self.gfx = Some(gfx);
        self.schedule();
    }

    /// Keeps a frame timer going while something moves (fast while animating,
    /// at the effects' pace for the live preview), and stops it otherwise.
    fn schedule(&mut self) {
        let preview = matches!(self.page, Page::Effects | Page::Layout)
            && !self.devices.is_empty()
            && self.p().layers.iter().any(|l| l.on && l.effect.kind.moves());
        let want = if self.anim.busy.get() {
            16
        } else if preview || self.anim.live.get() {
            33
        } else {
            0
        };
        if want != self.timer {
            unsafe {
                if want == 0 {
                    let _ = KillTimer(Some(self.hwnd), TICK);
                } else {
                    SetTimer(Some(self.hwnd), TICK, want, None);
                }
            }
            self.timer = want;
        }
    }

    fn draw(&self, ui: &mut Ui, w: f32, h: f32) {
        self.draw_side(ui, h);
        // The page: header, then its content (scrolled), sliding in when it changes.
        let x = SIDE + PAD;
        let cw = w - x - PAD;
        let t = ease_out(self.page_since.elapsed().as_secs_f32() / 0.28);
        if t < 1.0 {
            self.anim.busy.set(true);
        }
        let shift = (1.0 - t) * 18.0;
        let mut hx = x;
        if !self.history.is_empty() {
            ui.icon_button(x - 6.0, 18.0, 36.0, G_BACK, Act::Back, true);
            hx += 38.0;
        }
        let sub = match self.page {
            Page::Effects => format!("{} \u{b7} effects stack, top over the rest", self.p().name),
            Page::Layout => "Where your devices sit, so effects flow across them".into(),
            Page::Devices => "Everything OpenRGB found".into(),
            Page::Device(i) => self.devices.get(i).map_or(String::new(), |d| d.name.clone()),
            Page::Settings => "Lighting, OpenRGB and administrator rights".into(),
        };
        ui.p.text(self.page.title(), &ui.f.big, hx + shift, 14.0, cw, 34.0, Align::Left, Color::white(0.96));
        ui.p.text(&sub, &ui.f.small, hx + shift, 46.0, cw - 40.0, 18.0, Align::Left, Color::white(0.5));
        let top = 82.0;
        let view = (x - 8.0, top, cw + 16.0, h - top);
        self.view.set(view);
        let scroll = self.anim.to(key("scroll", K_POS), self.scroll, 22.0);
        ui.p.clip(view.0, view.1, view.2, view.3);
        ui.clip = Some(view);
        let mut y = top + 4.0 - scroll + shift;
        y = self.draw_banner(ui, x, y, cw);
        let bottom = match self.page {
            Page::Effects => self.draw_effects(ui, x, y, cw, h - y - PAD + scroll),
            Page::Layout => self.draw_layout(ui, x, y, cw, (h - y - PAD).max(320.0)),
            Page::Devices => self.draw_devices(ui, x, y, cw),
            Page::Device(i) => self.draw_device(ui, i, x, y, cw),
            Page::Settings => self.draw_settings(ui, x, y, cw),
        };
        self.content_h.set(bottom + scroll - top + PAD);
        // The page fades in over the old one's place.
        if t < 1.0 {
            ui.p.rrect(view.0, view.1, view.2, view.3, 0.0, BG.alpha(1.0 - t));
        }
        ui.p.unclip();
        ui.clip = None;
        // A scrollbar when the page is taller than the window.
        let ch = self.content_h.get();
        if ch > view.3 + 1.0 {
            let bh = (view.3 / ch * view.3).max(30.0);
            let by = view.1 + (scroll / (ch - view.3)).clamp(0.0, 1.0) * (view.3 - bh);
            ui.p.rrect(w - 8.0, by + 2.0, 4.0, bh - 4.0, 2.0, Color::white(0.18));
        }
    }

    fn draw_side(&self, ui: &mut Ui, h: f32) {
        let p = ui.p;
        let accent = theme::accent();
        p.rrect(0.0, 0.0, SIDE, h, 0.0, Color(0.105, 0.105, 0.11, 1.0));
        p.line(SIDE - 0.5, 0.0, SIDE - 0.5, h, LINE, 1.0);
        // The app and how it's connected.
        let (dot, status) = self.status();
        p.text("Lighting", &ui.f.title, 20.0, 18.0, SIDE - 40.0, 24.0, Align::Left, Color::white(0.95));
        let pulse = if matches!(self.phase, Phase::Connected) && !self.devices.is_empty() {
            1.0
        } else {
            self.anim.live.set(true);
            0.55 + 0.45 * (self.start.elapsed().as_secs_f32() * 3.0).sin().abs()
        };
        p.circle(24.0, 52.0, 4.0, dot.alpha(pulse));
        p.text(&status, &ui.f.small, 34.0, 43.0, SIDE - 50.0, 18.0, Align::Left, Color::white(0.6));
        // The pages.
        let ny = 78.0;
        let sel = self.anim.to(key("nav", K_POS), self.page.nav() as f32, 16.0);
        p.rrect(10.0, ny + sel * ROW + 3.0, SIDE - 20.0, ROW - 6.0, 6.0, Color::white(0.08));
        p.rrect(10.0, ny + sel * ROW + 12.0, 3.0, ROW - 24.0, 1.5, accent);
        for (i, (page, g)) in NAV.iter().enumerate() {
            let y = ny + i as f32 * ROW;
            let a = Act::Go(*page);
            let on = self.page.nav() == i;
            let hv = ui.hov(a);
            if !on {
                p.rrect(10.0, y + 3.0, SIDE - 20.0, ROW - 6.0, 6.0, Color::white(0.05 * hv + 0.04 * ui.prs(a)));
            }
            let c = Color::white(if on { 0.96 } else { 0.68 + 0.25 * hv });
            ui.glyph(*g, 22.0, y + ROW / 2.0 - 9.0, 18.0, c);
            p.text(page.title(), &ui.f.body, 52.0, y, SIDE - 70.0, ROW, Align::Left, c);
            ui.hit(10.0, y + 3.0, SIDE - 20.0, ROW - 6.0, a);
        }
        // Profiles.
        let py = profiles_top();
        p.text("Profiles", &ui.f.label, 20.0, py - 30.0, 120.0, 20.0, Align::Left, Color::white(0.5));
        ui.icon_button(SIDE - 44.0, py - 34.0, 28.0, G_ADD, Act::NewProfile, false);
        let act = self.anim.to(key("profile", K_POS), self.l.active as f32, 16.0);
        p.rrect(10.0, py + act * ROW + 3.0, SIDE - 20.0, ROW - 6.0, 6.0, accent.alpha(0.18));
        for (i, pr) in self.l.profiles.iter().enumerate() {
            let y = py + i as f32 * ROW;
            if y > h - 70.0 - ROW {
                break;
            }
            let a = Act::Profile(i);
            let on = i == self.l.active;
            let hv = ui.hov(a).max(ui.hov(Act::ProfileMore(i)));
            if !on {
                p.rrect(10.0, y + 3.0, SIDE - 20.0, ROW - 6.0, 6.0, Color::white(0.05 * hv + 0.04 * ui.prs(a)));
            }
            p.text(
                &pr.name,
                &ui.f.body,
                22.0,
                y,
                SIDE - 90.0,
                ROW,
                Align::Left,
                Color::white(if on { 0.96 } else { 0.78 }),
            );
            ui.hit(10.0, y + 3.0, SIDE - 20.0, ROW - 6.0, a);
            if !pr.apps.is_empty() {
                ui.glyph(G_LINK, SIDE - 66.0, y + ROW / 2.0 - 7.0, 14.0, Color::white(0.45));
            }
            if hv > 0.01 || on {
                let c = Color::white(0.55 * hv.max(if on { 0.6 } else { 0.0 }));
                let mx = SIDE - 44.0;
                let mh = ui.hov(Act::ProfileMore(i));
                p.rrect(mx, y + 7.0, 26.0, ROW - 14.0, 5.0, Color::white(0.08 * mh));
                ui.glyph(G_MORE, mx + 5.0, y + ROW / 2.0 - 8.0, 16.0, c);
                ui.hit(mx, y + 7.0, 26.0, ROW - 14.0, Act::ProfileMore(i));
            }
        }
        // Everything on or off.
        let by = h - 56.0;
        p.line(16.0, by - 8.0, SIDE - 16.0, by - 8.0, LINE, 1.0);
        p.text("Lighting on", &ui.f.body, 20.0, by, SIDE - 90.0, 40.0, Align::Left, Color::white(0.85));
        ui.toggle(SIDE - 62.0, by + 10.0, self.l.settings.on, Act::Toggle(0));
    }

    /// The card at the top while OpenRGB is being set up, starting, or finding nothing.
    fn draw_banner(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = ui.p;
        let warm = Color(0.95, 0.7, 0.25, 1.0);
        let (glyph_code, tint, title, text, progress, spin, buttons): Banner<'_> =
            match &self.phase {
                Phase::Downloading(t) => (
                    G_DOWNLOAD,
                    theme::accent(),
                    "Setting up OpenRGB",
                    "Cascade drives your RGB through OpenRGB (free and open source), which talks to Corsair, ASUS Aura, RGB Fusion, Razer, Logitech, HyperX and hundreds more. Getting it now (about 22 MB), once.".into(),
                    Some(*t),
                    false,
                    vec![],
                ),
                Phase::Looking | Phase::Starting => (
                    G_LIGHT,
                    warm,
                    "Starting OpenRGB",
                    "It runs out of sight; this takes a moment.".into(),
                    None,
                    true,
                    vec![],
                ),
                Phase::Failed(e) => (
                    G_WARN,
                    Color(0.95, 0.4, 0.35, 1.0),
                    "OpenRGB isn't connected",
                    e.clone(),
                    None,
                    false,
                    vec![("Try again", G_REFRESH, Act::Retry), ("Use my own OpenRGB\u{2026}", G_FOLDER, Act::ChooseOpenRgb)],
                ),
                Phase::Connected if self.devices.is_empty() => {
                    let early = self.connected_at.is_none_or(|t| t.elapsed() < Duration::from_secs(25));
                    if early {
                        self.anim.live.set(true);
                        (G_DEVICES, warm, "Finding your devices", "OpenRGB is looking at every USB device, and at RAM, the motherboard and graphics card when it has administrator rights.".into(), None, true, vec![])
                    } else {
                        let rivals = if self.rivals.is_empty() {
                            String::new()
                        } else {
                            format!(" Running now: {}. Close them first.", self.rivals.join(", "))
                        };
                        (
                            G_WARN,
                            warm,
                            "No devices found",
                            format!("The vendors' own apps (iCUE, Synapse, G Hub, Armoury Crate, RGB Fusion) hold their devices.{rivals} RAM, motherboard and graphics card lighting need administrator rights."),
                            None,
                            false,
                            vec![("Look again", G_REFRESH, Act::Rescan), ("Administrator rights", G_SHIELD, Act::Go(Page::Settings))],
                        )
                    }
                }
                Phase::Connected => return y,
            };
        let h = if buttons.is_empty() { 104.0 } else { 144.0 };
        p.rrect(x, y, w, h, 10.0, mix(PANEL, tint, 0.10));
        p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 10.0, tint.alpha(0.35), 1.0);
        if spin {
            self.anim.live.set(true);
            let a = self.start.elapsed().as_secs_f32() * 300.0;
            p.stroke_circle(x + 34.0, y + 34.0, 12.0, Color::white(0.1), 2.5);
            p.arc(x + 34.0, y + 34.0, 12.0, a, a + 100.0, tint, 2.5);
        } else {
            ui.glyph(glyph_code, x + 22.0, y + 22.0, 24.0, tint);
        }
        p.text(title, &ui.f.title, x + 64.0, y + 14.0, w - 90.0, 24.0, Align::Left, Color::white(0.95));
        p.text_wrap(&text, &ui.f.small, x + 64.0, y + 40.0, w - 90.0, 48.0, Color::white(0.65));
        if let Some(t) = progress {
            let shown = self.anim.to(key("progress", K_POS), t, 8.0);
            let (bx, bw) = (x + 64.0, w - 90.0);
            p.rrect(bx, y + h - 22.0, bw, 6.0, 3.0, Color::white(0.1));
            p.gradient_rrect(bx, y + h - 22.0, (bw * shown).max(6.0), 6.0, 3.0, &[(0.0, tint.alpha(0.7)), (1.0, tint)]);
        }
        let mut bx = x + 64.0;
        for (label, g, a) in buttons {
            let bw = ui.p.measure(label, &ui.f.body) + 56.0;
            ui.button(bx, y + h - 46.0, bw, 32.0, label, Some(g), a, bx == x + 64.0);
            bx += bw + 10.0;
        }
        y + h + 16.0
    }

    fn draw_effects(&self, ui: &mut Ui, x: f32, y: f32, w: f32, h: f32) -> f32 {
        let p = ui.p;
        let accent = theme::accent();
        let pr = self.p();
        let lw = 300.0_f32.min(w * 0.38);
        let h = h.max(560.0);
        // The layers, top first.
        let lh = (h * 0.58).max(300.0);
        ui.card(x, y, lw, lh);
        p.text("Effects", &ui.f.title, x + 16.0, y + 12.0, 120.0, 24.0, Align::Left, Color::white(0.92));
        let n = pr.layers.len();
        let mut bx = x + lw - 12.0 - 4.0 * 32.0;
        for (g, a, ok) in [
            (G_ADD, Act::AddLayer, true),
            (G_UP, Act::LayerUp, self.layer + 1 < n),
            (G_DOWN, Act::LayerDown, self.layer > 0 && n > 0),
            (G_DELETE, Act::LayerDelete, n > 0),
        ] {
            if ok {
                ui.icon_button(bx, y + 10.0, 30.0, g, a, false);
            } else {
                ui.glyph(g, bx + 7.0, y + 17.0, 16.0, Color::white(0.2));
            }
            bx += 32.0;
        }
        let ly0 = y + 50.0;
        let rows = ((lh - 60.0) / ROW).floor().max(1.0) as usize;
        if n > 0 {
            let sel_row = (n - 1 - self.layer.min(n - 1)) as f32;
            let s = self.anim.to(key("layer", K_POS), sel_row, 16.0);
            if s < rows as f32 {
                p.rrect(x + 8.0, ly0 + s * ROW + 2.0, lw - 16.0, ROW - 4.0, 6.0, accent.alpha(0.2));
            }
        }
        for (row, i) in (0..n).rev().enumerate().take(rows) {
            let l = &pr.layers[i];
            let ly = ly0 + row as f32 * ROW;
            let a = Act::Layer(i);
            let hv = ui.hov(a);
            if i != self.layer {
                p.rrect(x + 8.0, ly + 2.0, lw - 16.0, ROW - 4.0, 6.0, Color::white(0.05 * hv + 0.04 * ui.prs(a)));
            }
            ui.hit(x + 44.0, ly + 2.0, lw - 52.0, ROW - 4.0, a);
            ui.icon_button(x + 12.0, ly + 5.0, 28.0, if l.on { G_EYE } else { G_HIDDEN }, Act::LayerEye(i), false);
            let c = Color::white(if l.on { 0.92 } else { 0.42 });
            p.text(&l.name, &ui.f.body, x + 48.0, ly, lw - 130.0, ROW, Align::Left, c);
            // Its colours, as dots.
            for (k, col) in l.effect.colours.iter().take(4).enumerate() {
                p.circle(
                    x + lw - 24.0 - k as f32 * 12.0,
                    ly + ROW / 2.0,
                    4.5,
                    rgb_of(*col).alpha(if l.on { 1.0 } else { 0.4 }),
                );
            }
        }
        if n == 0 {
            p.text_wrap(
                "No effects yet. + adds one; they stack, the top one over the rest.",
                &ui.f.small,
                x + 16.0,
                ly0 + 4.0,
                lw - 32.0,
                40.0,
                Color::white(0.5),
            );
        }
        // The profile itself.
        let py = y + lh + 14.0;
        let ph = 196.0;
        ui.card(x, py, lw, ph);
        p.text("This profile", &ui.f.title, x + 16.0, py + 12.0, lw - 32.0, 24.0, Align::Left, Color::white(0.92));
        ui.slider(
            x + 16.0,
            py + 46.0,
            lw - 32.0,
            "Brightness",
            pr.brightness,
            &format!("{:.0}%", pr.brightness * 100.0),
            Act::Slider(Sl::ProfileBright),
        );
        let apps = if pr.apps.is_empty() {
            "Comes on when you choose it".to_string()
        } else {
            format!("Comes on with {}", pr.apps.join(", "))
        };
        p.text_wrap(&apps, &ui.f.small, x + 16.0, py + 102.0, lw - 32.0, 34.0, Color::white(0.6));
        ui.button(x + 16.0, py + 144.0, lw - 32.0, 34.0, "Linked programs\u{2026}", Some(G_LINK), Act::Apps, false);
        let left_bottom = py + ph;
        // The preview, then the chosen effect's settings.
        let ex = x + lw + 16.0;
        let ew = w - lw - 16.0;
        let prev_h = (h * 0.36).clamp(180.0, 300.0);
        self.draw_canvas(ui, (ex, y, ew, prev_h), false);
        let ey = y + prev_h + 14.0;
        let Some(l) = pr.layers.get(self.layer) else {
            ui.card(ex, ey, ew, 120.0);
            p.text(
                "Pick an effect on the left, or add one.",
                &ui.f.body,
                ex + 20.0,
                ey,
                ew - 40.0,
                120.0,
                Align::Center,
                Color::white(0.5),
            );
            return left_bottom.max(ey + 120.0);
        };
        let e = &l.effect;
        // Measure first (a card behind the rows), then draw.
        let mut rows_h = 20.0 + 48.0 * 2.0;
        let (_, hi) = e.kind.colours();
        if hi > 0 {
            rows_h += 48.0;
        }
        if e.kind.moves() || e.kind == Kind::Gradient {
            rows_h += 60.0 * 2.0 + 48.0;
        }
        if e.kind == Kind::Temperature {
            rows_h += 48.0 + 60.0 * 2.0;
        }
        rows_h += 60.0 + 48.0 + 10.0;
        ui.card(ex, ey, ew, rows_h);
        let lx = ex + 20.0;
        let cx = ex + 140.0;
        let cw = (ew - 160.0).min(420.0);
        let sw = ew - 40.0;
        let mut ry = ey + 16.0;
        let label = |ui: &mut Ui, ry: f32, s: &str| {
            ui.p.text(s, &ui.f.body, lx, ry, 118.0, 34.0, Align::Left, Color::white(0.72));
        };
        label(ui, ry, "Effect");
        ui.select(cx, ry, cw, e.kind.name(), Act::Effect);
        ry += 48.0;
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
        label(ui, ry, "Lights");
        ui.select(cx, ry, cw, &target, Act::Target);
        ry += 48.0;
        if hi > 0 {
            label(ui, ry, "Colours");
            for (i, c) in e.colours.iter().enumerate() {
                ui.swatch(cx + i as f32 * 42.0, ry, rgb_of(*c), Act::Colour(i));
            }
            if e.colours.len() < hi {
                ui.icon_button(cx + e.colours.len() as f32 * 42.0, ry + 1.0, 32.0, G_ADD, Act::AddColour, false);
            }
            ry += 48.0;
        }
        if e.kind.moves() || e.kind == Kind::Gradient {
            ui.slider(lx, ry, sw, "Speed", e.speed, &format!("{:.0}%", e.speed * 100.0), Act::Slider(Sl::Speed));
            ry += 60.0;
            ui.slider(lx, ry, sw, "Size", e.size, &format!("{:.0}%", e.size * 100.0), Act::Slider(Sl::Size));
            ry += 60.0;
            label(ui, ry, "Direction");
            const DIRS: [(Dir, &str); 6] = [
                (Dir::Right, "\u{2192}"),
                (Dir::Left, "\u{2190}"),
                (Dir::Down, "\u{2193}"),
                (Dir::Up, "\u{2191}"),
                (Dir::Out, "Out"),
                (Dir::In, "In"),
            ];
            let cur = DIRS.iter().position(|d| d.0 == e.dir).unwrap_or(0) as f32;
            let seg = ((ew - 160.0) / 6.0).min(50.0);
            p.rrect(cx, ry, seg * 6.0, 34.0, 8.0, Color::white(0.05));
            let s = self.anim.to(key("dir", K_POS), cur, 18.0);
            p.rrect(cx + s * seg + 2.0, ry + 2.0, seg - 4.0, 30.0, 6.0, accent.alpha(0.55));
            for (i, (d, n)) in DIRS.iter().enumerate() {
                let bx = cx + i as f32 * seg;
                let a = Act::Dir(*d);
                let hv = ui.hov(a);
                if e.dir != *d {
                    p.rrect(bx + 2.0, ry + 2.0, seg - 4.0, 30.0, 6.0, Color::white(0.06 * hv));
                }
                p.text(n, &ui.f.body, bx, ry, seg, 34.0, Align::Center, Color::white(0.92));
                ui.hit(bx, ry, seg, 34.0, a);
            }
            ry += 48.0;
        }
        if e.kind == Kind::Temperature {
            label(ui, ry, "Sensor");
            ui.select(cx, ry, cw, if e.sensor == Sensor::Cpu { "CPU" } else { "Graphics card" }, Act::Sensor);
            ry += 48.0;
            let (a, b) = e.temp_range;
            ui.slider(lx, ry, sw, "Cool at", (a - 20.0) / 80.0, &format!("{a:.0} \u{b0}C"), Act::Slider(Sl::TempLo));
            ry += 60.0;
            ui.slider(lx, ry, sw, "Hot at", (b - 20.0) / 80.0, &format!("{b:.0} \u{b0}C"), Act::Slider(Sl::TempHi));
            ry += 60.0;
        }
        ui.slider(lx, ry, sw, "Opacity", l.opacity, &format!("{:.0}%", l.opacity * 100.0), Act::Slider(Sl::Opacity));
        ry += 60.0;
        let blend = match l.blend {
            Blend::Normal => "Normal",
            Blend::Add => "Add",
            Blend::Screen => "Screen",
            Blend::Multiply => "Multiply",
            Blend::Lighten => "Lighten",
        };
        label(ui, ry, "Blend");
        ui.select(cx, ry, cw, blend, Act::Blend);
        left_bottom.max(ey + rows_h)
    }

    fn draw_layout(&self, ui: &mut Ui, x: f32, y: f32, w: f32, h: f32) -> f32 {
        ui.p.text(
            "Drag a device to where it sits; drag its corner to resize. Right-click one to make it a ring (fans), flip it or leave it out.",
            &ui.f.small,
            x,
            y,
            w,
            20.0,
            Align::Left,
            Color::white(0.55),
        );
        self.draw_canvas(ui, (x, y + 28.0, w, h - 28.0), true);
        y + h
    }

    /// The devices on the canvas, lit as the profile lights them now.
    fn draw_canvas(&self, ui: &mut Ui, (x, y, w, h): (f32, f32, f32, f32), editing: bool) {
        let p = ui.p;
        self.canvas.set((x, y, w, h));
        p.rrect(x, y, w, h, 10.0, Color(0.065, 0.065, 0.07, 1.0));
        p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 10.0, LINE, 1.0);
        if editing {
            // A faint grid to line things up against.
            for i in 1..10 {
                let gx = x + w * i as f32 / 10.0;
                let gy = y + h * i as f32 / 10.0;
                p.line(gx, y + 1.0, gx, y + h - 1.0, Color::white(0.025), 1.0);
                p.line(x + 1.0, gy, x + w - 1.0, gy, Color::white(0.025), 1.0);
            }
        }
        ui.hit(x, y, w, h, Act::Canvas);
        let pr = self.p();
        let ctx = Context {
            t: self.start.elapsed().as_secs_f32(),
            cpu_temp: 55.0,
            gpu_temp: 50.0,
            level: 0.5 + 0.3 * (self.start.elapsed().as_secs_f32() * 2.0).sin(),
            bands: (0..16).map(|i| 0.5 + 0.4 * (self.start.elapsed().as_secs_f32() * 3.0 + i as f32).sin()).collect(),
            ..Default::default()
        };
        let bright =
            (self.l.settings.brightness * pr.brightness).clamp(0.0, 1.0) * if self.l.settings.on { 1.0 } else { 0.15 };
        for (di, d) in self.devices.iter().enumerate() {
            let key_ = d.key();
            let place = self.l.layout.get(&key_).copied().unwrap_or_default();
            if place.off && !editing {
                continue;
            }
            let (bx, by, bw, bh) = (x + place.x * w, y + place.y * h, place.w * w, place.h * h);
            let hv = self.anim.to(key(("dev", di), K_HOVER), if self.hover_dev == Some(di) { 1.0 } else { 0.0 }, 18.0);
            let sel = editing && di == self.device;
            if editing {
                p.rrect(bx, by, bw, bh, 8.0, Color::white(if place.off { 0.02 } else { 0.04 + 0.03 * hv }));
                let edge = if sel { theme::accent() } else { Color::white(0.12 + 0.2 * hv) };
                p.stroke_rrect(bx + 0.5, by + 0.5, bw - 1.0, bh - 1.0, 8.0, edge, if sel { 2.0 } else { 1.0 });
                p.text(
                    &d.name,
                    &ui.f.small,
                    bx + 2.0,
                    by - 18.0,
                    bw.max(180.0),
                    16.0,
                    Align::Left,
                    Color::white(0.55 + 0.35 * hv),
                );
                p.rrect(bx + bw - 10.0, by + bh - 10.0, 8.0, 8.0, 2.0, Color::white(0.3 + 0.5 * hv));
            }
            let hw = pr.hardware.get(&key_);
            let dot = (bw.min(bh) / 6.0).clamp(2.0, 6.0);
            for (led, zone) in led_positions(d, &place) {
                let c = if hw.is_some() {
                    Rgb(0.32, 0.32, 0.36)
                } else {
                    let mut c = Rgb::BLACK;
                    for l in pr.layers.iter().filter(|l| l.on && l.covers(&key_, zone)) {
                        c = effects::blend(c, effects::colour(&l.effect, &led, &ctx), l.blend, l.opacity);
                    }
                    c.scale(bright)
                };
                let a = if place.off { 0.2 } else { 1.0 };
                let (lx, ly) = (x + led.x * w, y + led.y * h);
                // A soft glow round each LED, as on the real thing.
                p.circle(lx, ly, dot * 2.4, Color(c.0, c.1, c.2, 0.12 * a));
                p.circle(lx, ly, dot, Color(c.0, c.1, c.2, a));
            }
        }
        if self.devices.is_empty() {
            let (_, s) = self.status();
            p.text(&s, &ui.f.body, x, y + h / 2.0 - 10.0, w, 20.0, Align::Center, Color::white(0.4));
        }
    }

    fn draw_devices(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = ui.p;
        if self.devices.is_empty() {
            return y;
        }
        let cols = ((w + 14.0) / 300.0).floor().max(1.0) as usize;
        let cw = (w - 14.0 * (cols - 1) as f32) / cols as f32;
        let ch = 112.0;
        let mut bottom = y;
        for (i, d) in self.devices.iter().enumerate() {
            let (col, row) = (i % cols, i / cols);
            let (cx, cy) = (x + col as f32 * (cw + 14.0), y + row as f32 * (ch + 14.0));
            let a = Act::Go(Page::Device(i));
            let hv = ui.hov(a);
            let off = self.l.layout.get(&d.key()).is_some_and(|p| p.off);
            let lift = 2.0 * hv;
            p.rrect(cx, cy - lift, cw, ch, 10.0, mix(PANEL, Color::white(1.0), 0.03 * hv + 0.03 * ui.prs(a)));
            p.stroke_rrect(cx + 0.5, cy - lift + 0.5, cw - 1.0, ch - 1.0, 10.0, Color::white(0.05 + 0.08 * hv), 1.0);
            ui.hit(cx, cy - lift, cw, ch, a);
            // Its icon on a tile tinted by what it shows now.
            let tint = d.colors.first().map_or(theme::accent(), |&c| {
                let (r, g, b) = openrgb::unpack(c);
                let col = Color(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0);
                if r as u32 + g as u32 + b as u32 > 60 {
                    col
                } else {
                    theme::accent()
                }
            });
            p.rrect(cx + 16.0, cy - lift + 16.0, 44.0, 44.0, 10.0, tint.alpha(0.16));
            ui.glyph(device_glyph(d.kind), cx + 27.0, cy - lift + 27.0, 22.0, tint);
            let vendor = if d.vendor.is_empty() {
                openrgb::kind_name(d.kind).to_string()
            } else {
                format!("{} \u{b7} {}", d.vendor, openrgb::kind_name(d.kind))
            };
            p.text(
                &d.name,
                &ui.f.body,
                cx + 72.0,
                cy - lift + 14.0,
                cw - 90.0,
                22.0,
                Align::Left,
                Color::white(if off { 0.45 } else { 0.94 }),
            );
            p.text(&vendor, &ui.f.small, cx + 72.0, cy - lift + 36.0, cw - 90.0, 18.0, Align::Left, Color::white(0.5));
            let mode = match self.p().hardware.get(&d.key()) {
                Some(h) => format!("Own mode: {}", h.mode),
                None if off => "Left out".to_string(),
                None => format!("{} LEDs \u{b7} Cascade's effects", d.leds.len()),
            };
            p.text(
                &mode,
                &ui.f.small,
                cx + 16.0,
                cy - lift + ch - 34.0,
                cw - 90.0,
                20.0,
                Align::Left,
                Color::white(0.6),
            );
            ui.toggle(cx + cw - 56.0, cy - lift + ch - 32.0, !off, Act::DeviceOff(i));
            bottom = cy + ch;
        }
        bottom
    }

    fn draw_device(&self, ui: &mut Ui, i: usize, x: f32, y: f32, w: f32) -> f32 {
        let p = ui.p;
        let Some(d) = self.devices.get(i) else { return y };
        let key_ = d.key();
        // What it is.
        ui.card(x, y, w, 76.0);
        ui.glyph(device_glyph(d.kind), x + 22.0, y + 24.0, 28.0, theme::accent());
        let info = format!(
            "{} \u{b7} {} \u{b7} {} LEDs{}",
            openrgb::kind_name(d.kind),
            if d.vendor.is_empty() { "Unknown maker" } else { &d.vendor },
            d.leds.len(),
            if d.location.is_empty() { String::new() } else { format!(" \u{b7} {}", d.location) }
        );
        p.text(&d.name, &ui.f.title, x + 68.0, y + 12.0, w - 90.0, 26.0, Align::Left, Color::white(0.95));
        p.text(&info, &ui.f.small, x + 68.0, y + 40.0, w - 90.0, 20.0, Align::Left, Color::white(0.55));
        // Its lighting.
        let mut ry = y + 90.0;
        let hw = self.p().hardware.get(&key_).cloned();
        let mode = hw.as_ref().and_then(|h| d.modes.iter().find(|m| m.name == h.mode));
        let mut ch = 70.0;
        if let (Some(h), Some(m)) = (&hw, mode) {
            ch += if m.colors_max > 0 { 48.0 } else { 0.0 }
                + if m.flags & openrgb::MODE_HAS_SPEED != 0 { 60.0 } else { 0.0 }
                + if m.flags & openrgb::MODE_HAS_BRIGHTNESS != 0 { 60.0 } else { 0.0 }
                + if m.flags
                    & (openrgb::MODE_HAS_DIRECTION_LR | openrgb::MODE_HAS_DIRECTION_UD | openrgb::MODE_HAS_DIRECTION_HV)
                    != 0
                {
                    48.0
                } else {
                    0.0
                }
                + if m.flags & (openrgb::MODE_MANUAL_SAVE | openrgb::MODE_AUTOMATIC_SAVE) != 0 { 48.0 } else { 0.0 };
            let _ = h;
        } else {
            ch += 34.0;
        }
        ui.card(x, ry, w, ch);
        let (lx, cx, cw, sw) = (x + 20.0, x + 190.0, (w - 210.0).min(420.0), w - 40.0);
        ry += 18.0;
        p.text("Lighting", &ui.f.body, lx, ry, 160.0, 34.0, Align::Left, Color::white(0.72));
        ui.select(cx, ry, cw, hw.as_ref().map_or("Cascade's effects (this profile)", |h| h.mode.as_str()), Act::HwMode);
        ry += 48.0;
        if let (Some(h), Some(m)) = (&hw, mode) {
            if m.colors_max > 0 {
                p.text("Colours", &ui.f.body, lx, ry, 160.0, 34.0, Align::Left, Color::white(0.72));
                let n = (h.colours.len() as u32).clamp(m.colors_min.max(1), m.colors_max) as usize;
                for k in 0..n {
                    ui.swatch(
                        cx + k as f32 * 42.0,
                        ry,
                        rgb_of(h.colours.get(k).copied().unwrap_or(0xFFFFFF)),
                        Act::HwColour(k),
                    );
                }
                if (n as u32) < m.colors_max {
                    ui.icon_button(cx + n as f32 * 42.0, ry + 1.0, 32.0, G_ADD, Act::HwColour(n), false);
                }
                ry += 48.0;
            }
            if m.flags & openrgb::MODE_HAS_SPEED != 0 {
                ui.slider(lx, ry, sw, "Speed", h.speed, &format!("{:.0}%", h.speed * 100.0), Act::Slider(Sl::HwSpeed));
                ry += 60.0;
            }
            if m.flags & openrgb::MODE_HAS_BRIGHTNESS != 0 {
                ui.slider(
                    lx,
                    ry,
                    sw,
                    "Brightness",
                    h.brightness,
                    &format!("{:.0}%", h.brightness * 100.0),
                    Act::Slider(Sl::HwBright),
                );
                ry += 60.0;
            }
            if m.flags
                & (openrgb::MODE_HAS_DIRECTION_LR | openrgb::MODE_HAS_DIRECTION_UD | openrgb::MODE_HAS_DIRECTION_HV)
                != 0
            {
                let names = ["Left", "Right", "Up", "Down", "Horizontal", "Vertical"];
                p.text("Direction", &ui.f.body, lx, ry, 160.0, 34.0, Align::Left, Color::white(0.72));
                ui.select(
                    cx,
                    ry,
                    cw.min(220.0),
                    names.get(h.direction as usize).copied().unwrap_or("Left"),
                    Act::HwDir,
                );
                ry += 48.0;
            }
            if m.flags & (openrgb::MODE_MANUAL_SAVE | openrgb::MODE_AUTOMATIC_SAVE) != 0 {
                p.text(
                    "Save to the device (keeps it with Cascade closed)",
                    &ui.f.body,
                    lx,
                    ry,
                    w - 120.0,
                    34.0,
                    Align::Left,
                    Color::white(0.72),
                );
                ui.toggle(x + w - 64.0, ry + 8.0, h.save, Act::HwSave);
            }
        } else {
            p.text_wrap(
                "Follows the profile's effects. Pick one of the device's own modes to leave it to the device: it runs with no software and can be saved to it.",
                &ui.f.small,
                lx,
                ry,
                sw,
                34.0,
                Color::white(0.55),
            );
        }
        // Zones, and the size of the ones that can be set (fan and strip headers).
        let mut zy = y + 90.0 + ch + 14.0;
        let zh = 50.0 + d.zones.iter().map(|z| if z.leds_max > z.leds_min { 60.0 } else { 30.0 }).sum::<f32>();
        ui.card(x, zy, w, zh);
        p.text("Zones", &ui.f.title, lx, zy + 12.0, 200.0, 24.0, Align::Left, Color::white(0.92));
        zy += 46.0;
        for (k, z) in d.zones.iter().enumerate() {
            if z.leds_max > z.leds_min {
                let t = (z.leds_count.saturating_sub(z.leds_min)) as f32 / (z.leds_max - z.leds_min) as f32;
                ui.slider(
                    lx,
                    zy,
                    sw,
                    &format!("{} (LEDs on the header)", z.name),
                    t,
                    &z.leds_count.to_string(),
                    Act::Slider(Sl::Zone(k)),
                );
                zy += 60.0;
            } else {
                p.text(
                    &format!("{} \u{b7} {} LEDs", z.name, z.leds_count),
                    &ui.f.body,
                    lx,
                    zy,
                    sw,
                    24.0,
                    Align::Left,
                    Color::white(0.8),
                );
                zy += 30.0;
            }
        }
        // Where it sits.
        let place = self.l.layout.get(&key_).copied().unwrap_or_default();
        let py = y + 90.0 + ch + 14.0 + zh + 14.0;
        ui.card(x, py, w, 150.0);
        p.text("On the layout", &ui.f.title, lx, py + 12.0, 200.0, 24.0, Align::Left, Color::white(0.92));
        ui.button(
            x + w - 170.0,
            py + 10.0,
            150.0,
            32.0,
            "Open the layout",
            Some(G_LAYOUT),
            Act::Go(Page::Layout),
            false,
        );
        let rows = [
            ("A ring (fans)", place.ring, Act::Toggle(3)),
            ("Flipped", place.flip, Act::Toggle(4)),
            ("In the lighting", !place.off, Act::DeviceOff(i)),
        ];
        for (k, (label, on, a)) in rows.iter().enumerate() {
            let ry = py + 50.0 + k as f32 * 32.0;
            p.text(label, &ui.f.body, lx, ry, w - 120.0, 30.0, Align::Left, Color::white(0.8));
            ui.toggle(x + w - 64.0, ry + 6.0, *on, *a);
        }
        py + 150.0
    }

    fn draw_settings(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = ui.p;
        let s = &self.l.settings;
        let w = w.min(820.0);
        let row = |ui: &mut Ui, y: f32, title: &str, desc: &str, on: bool, a: Act| -> f32 {
            let h = if desc.is_empty() { 56.0 } else { 68.0 };
            let hv = ui.hov(a);
            ui.p.rrect(x, y, w, h, 8.0, mix(PANEL, Color::white(1.0), 0.025 * hv));
            ui.hit(x, y, w, h, a);
            ui.p.text(
                title,
                &ui.f.body,
                x + 20.0,
                y + if desc.is_empty() { 0.0 } else { 10.0 },
                w - 110.0,
                if desc.is_empty() { h } else { 24.0 },
                Align::Left,
                Color::white(0.9),
            );
            if !desc.is_empty() {
                ui.p.text(desc, &ui.f.small, x + 20.0, y + 34.0, w - 110.0, 20.0, Align::Left, Color::white(0.5));
            }
            ui.toggle(x + w - 64.0, y + h / 2.0 - 9.0, on, a);
            y + h + 4.0
        };
        let heading = |ui: &mut Ui, y: f32, t: &str| -> f32 {
            ui.p.text(t, &ui.f.label, x + 4.0, y, w, 20.0, Align::Left, Color::white(0.55));
            y + 28.0
        };
        let mut ry = heading(ui, y, "Lighting");
        ry = row(
            ui,
            ry,
            "Lighting on",
            "Off leaves every device as it is (or dark, with a saved mode).",
            s.on,
            Act::Toggle(0),
        );
        ry = row(
            ui,
            ry,
            "Switch profiles with the program in front",
            "Profiles with linked programs come on by themselves.",
            s.auto_switch,
            Act::Toggle(1),
        );
        ry = row(ui, ry, "Off while the PC is locked or its screens are off", "", s.off_when_idle, Act::Toggle(2));
        let sliders = [
            ("Brightness", s.brightness, format!("{:.0}%", s.brightness * 100.0), Sl::Bright),
            ("Frames a second (moving effects)", (s.fps as f32 - 5.0) / 115.0, s.fps.to_string(), Sl::Fps),
            (
                "Screen ambience: looks at the screen",
                (s.screen_fps as f32 - 1.0) / 59.0,
                format!("{} a second", s.screen_fps),
                Sl::ScreenFps,
            ),
        ];
        for (label, t, v, sl) in sliders {
            p.rrect(x, ry, w, 72.0, 8.0, PANEL);
            ui.slider(x + 20.0, ry + 12.0, w - 40.0, label, t, &v, Act::Slider(sl));
            ry += 76.0;
        }
        ry += 18.0;
        ry = heading(ui, ry, "OpenRGB");
        let found = super::hw::find(&s.openrgb);
        let (dot, status) = self.status();
        p.rrect(x, ry, w, 124.0, 8.0, PANEL);
        p.circle(x + 26.0, ry + 26.0, 5.0, dot);
        p.text(&status, &ui.f.body, x + 40.0, ry + 12.0, w - 60.0, 28.0, Align::Left, Color::white(0.9));
        let where_ = found
            .as_ref()
            .map_or("Not on this PC yet (Cascade gets it by itself)".to_string(), |p| p.display().to_string());
        p.text(&where_, &ui.f.small, x + 20.0, ry + 42.0, w - 40.0, 20.0, Align::Left, Color::white(0.5));
        let mut bx = x + 20.0;
        for (label, g, a) in [
            ("Look for devices", G_REFRESH, Act::Rescan),
            (if found.is_some() { "Reconnect" } else { "Get OpenRGB" }, G_DOWNLOAD, Act::Retry),
            ("Use my own copy\u{2026}", G_FOLDER, Act::ChooseOpenRgb),
        ] {
            let bw = ui.p.measure(label, &ui.f.body) + 56.0;
            ui.button(bx, ry + 74.0, bw, 34.0, label, Some(g), a, false);
            bx += bw + 10.0;
        }
        ry += 128.0;
        // Administrator rights, which also installs PawnIO.
        let busy = self.admin_busy.as_deref();
        let desc = match (busy, &self.admin_error) {
            (Some(step), _) => step.to_string(),
            (None, Some(e)) => format!("Didn't work: {e}"),
            (None, None) if s.admin && self.pawnio => "On \u{b7} PawnIO driver installed".into(),
            (None, None) => {
                "For RAM, motherboard and graphics card lighting. Installs the PawnIO driver too; Windows asks once."
                    .into()
            }
        };
        let a = Act::Admin;
        let hv = ui.hov(a);
        p.rrect(x, ry, w, 76.0, 8.0, mix(PANEL, Color::white(1.0), 0.025 * hv));
        ui.hit(x, ry, w, 76.0, a);
        ui.glyph(G_SHIELD, x + 18.0, ry + 26.0, 22.0, if s.admin { theme::accent() } else { Color::white(0.6) });
        p.text(
            "Administrator rights",
            &ui.f.body,
            x + 56.0,
            ry + 12.0,
            w - 140.0,
            24.0,
            Align::Left,
            Color::white(0.9),
        );
        let dc =
            if self.admin_error.is_some() && busy.is_none() { Color(1.0, 0.5, 0.45, 1.0) } else { Color::white(0.5) };
        p.text(&desc, &ui.f.small, x + 56.0, ry + 38.0, w - 140.0, 20.0, Align::Left, dc);
        if busy.is_some() {
            self.anim.live.set(true);
            let ang = self.start.elapsed().as_secs_f32() * 300.0;
            p.arc(x + w - 46.0, ry + 38.0, 9.0, ang, ang + 100.0, theme::accent(), 2.5);
        } else {
            ui.toggle(x + w - 64.0, ry + 29.0, s.admin, a);
        }
        ry += 80.0;
        if s.admin && self.pawnio {
            ui.glyph(G_CHECK, x + 6.0, ry + 4.0, 14.0, Color(0.3, 0.85, 0.45, 1.0));
            p.text(
                "RAM, motherboard and graphics card lighting can be reached.",
                &ui.f.small,
                x + 28.0,
                ry,
                w,
                22.0,
                Align::Left,
                Color::white(0.55),
            );
            ry += 28.0;
        }
        if !self.rivals.is_empty() {
            ry += 8.0;
            let warm = Color(0.95, 0.65, 0.2, 1.0);
            p.rrect(x, ry, w, 72.0, 8.0, mix(PANEL, warm, 0.12));
            ui.glyph(G_WARN, x + 18.0, ry + 24.0, 22.0, warm);
            p.text_wrap(
                &format!("Running now: {}. These hold their devices and fight OpenRGB for them: close them, and stop them starting with Windows.", self.rivals.join(", ")),
                &ui.f.small,
                x + 56.0,
                ry + 14.0,
                w - 76.0,
                48.0,
                Color::white(0.85),
            );
            ry += 72.0;
        }
        ry
    }
}

/// The banner's parts: icon, tint, title, text, progress, spinning, buttons.
type Banner<'a> = (u32, Color, &'a str, String, Option<f32>, bool, Vec<(&'a str, u32, Act)>);

fn profiles_top() -> f32 {
    78.0 + NAV.len() as f32 * ROW + 50.0
}

type PCWSTRCursor = windows::core::PCWSTR;

/// Immediate-mode controls: drawn (lit by hover and press, moving smoothly),
/// and their hit areas noted for the mouse.
struct Ui<'a, 'b> {
    p: &'a Painter<'b>,
    f: &'a Fonts,
    s: &'a State,
    hits: &'a mut Vec<Hit>,
    clip: Option<(f32, f32, f32, f32)>,
}

impl Ui<'_, '_> {
    fn hit(&mut self, x: f32, y: f32, w: f32, h: f32, a: Act) {
        let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + h);
        if let Some((cx, cy, cw, ch)) = self.clip {
            x0 = x0.max(cx);
            y0 = y0.max(cy);
            x1 = x1.min(cx + cw);
            y1 = y1.min(cy + ch);
        }
        if x1 > x0 && y1 > y0 {
            self.hits.push((x0, y0, x1 - x0, y1 - y0, a));
        }
    }

    fn hov(&self, a: Act) -> f32 {
        self.s.anim.to(key(a, K_HOVER), if self.s.hover == Some(a) { 1.0 } else { 0.0 }, 20.0)
    }

    fn prs(&self, a: Act) -> f32 {
        let down = self.s.pressed == Some(a) && self.s.hover == Some(a);
        self.s.anim.to(key(a, K_PRESS), if down { 1.0 } else { 0.0 }, 30.0)
    }

    fn glyph(&self, code: u32, x: f32, y: f32, size: f32, c: Color) {
        if let Some(ic) = &self.f.icons {
            self.p.text(&glyph(code), ic, x, y, size, size, Align::Center, c);
        }
    }

    fn card(&self, x: f32, y: f32, w: f32, h: f32) {
        self.p.rrect(x, y, w, h, 10.0, PANEL);
        self.p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 10.0, Color::white(0.04), 1.0);
    }

    #[allow(clippy::too_many_arguments)]
    fn button(&mut self, x: f32, y: f32, w: f32, h: f32, label: &str, icon: Option<u32>, a: Act, primary: bool) {
        let (hv, pr) = (self.hov(a), self.prs(a));
        let base = if primary { theme::accent() } else { Color::white(0.07) };
        let fill = if primary {
            mix(base, Color::white(1.0), 0.12 * hv - 0.1 * pr)
        } else {
            Color::white(0.07 + 0.05 * hv + 0.04 * pr)
        };
        let inset = pr;
        self.p.rrect(x + inset, y + inset, w - 2.0 * inset, h - 2.0 * inset, 7.0, fill);
        self.p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 7.0, Color::white(0.05 + 0.06 * hv), 1.0);
        let c = Color::white(if primary { 1.0 } else { 0.88 + 0.1 * hv });
        let tw = self.p.measure(label, &self.f.body) + if icon.is_some() { 24.0 } else { 0.0 };
        let mut tx = x + (w - tw) / 2.0;
        if let Some(g) = icon {
            self.glyph(g, tx, y + h / 2.0 - 8.0, 16.0, c);
            tx += 24.0;
        }
        self.p.text(label, &self.f.body, tx, y, w, h, Align::Left, c);
        self.hit(x, y, w, h, a);
    }

    /// A round-cornered icon-only button; `strong` shows a faint fill at rest.
    fn icon_button(&mut self, x: f32, y: f32, size: f32, g: u32, a: Act, strong: bool) {
        let (hv, pr) = (self.hov(a), self.prs(a));
        let rest = if strong { 0.05 } else { 0.0 };
        self.p.rrect(x, y, size, size, 7.0, Color::white(rest + 0.08 * hv + 0.06 * pr));
        let gs = (size * 0.5).round();
        self.glyph(g, x + (size - gs) / 2.0, y + (size - gs) / 2.0 + pr, gs, Color::white(0.72 + 0.26 * hv));
        self.hit(x, y, size, size, a);
    }

    /// A field that opens a menu: its value and a chevron.
    fn select(&mut self, x: f32, y: f32, w: f32, value: &str, a: Act) {
        let (hv, pr) = (self.hov(a), self.prs(a));
        self.p.rrect(x, y, w, 34.0, 7.0, Color::white(0.06 + 0.05 * hv + 0.04 * pr));
        self.p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, 33.0, 7.0, Color::white(0.06 + 0.08 * hv), 1.0);
        self.p.text(value, &self.f.body, x + 12.0, y, w - 44.0, 34.0, Align::Left, Color::white(0.92));
        self.glyph(G_CHEVRON, x + w - 28.0, y + 10.0 + 2.0 * pr, 14.0, Color::white(0.55 + 0.35 * hv));
        self.hit(x, y, w, 34.0, a);
    }

    fn swatch(&mut self, x: f32, y: f32, c: Color, a: Act) {
        let hv = self.hov(a);
        let grow = 2.0 * hv;
        self.p.rrect(x - grow, y - grow, 34.0 + 2.0 * grow, 34.0 + 2.0 * grow, 8.0, c);
        self.p.stroke_rrect(
            x - grow + 0.5,
            y - grow + 0.5,
            33.0 + 2.0 * grow,
            33.0 + 2.0 * grow,
            8.0,
            Color::white(0.2 + 0.5 * hv),
            1.0 + hv,
        );
        self.hit(x, y, 34.0, 34.0, a);
    }

    fn toggle(&mut self, x: f32, y: f32, on: bool, a: Act) {
        let (w, h) = (40.0, 20.0);
        let t = self.s.anim.to(key(a, K_ON), if on { 1.0 } else { 0.0 }, 18.0);
        let hv = self.hov(a);
        let off_c = Color::white(0.12 + 0.06 * hv);
        let on_c = mix(theme::accent(), Color::white(1.0), 0.1 * hv);
        self.p.rrect(x, y, w, h, h / 2.0, mix(off_c, on_c, t));
        if t < 0.99 {
            self.p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, h / 2.0, Color::white(0.25 * (1.0 - t)), 1.0);
        }
        let r = h / 2.0 - 4.0 + 1.0 * hv + 1.0 * self.prs(a);
        let kx = x + h / 2.0 + t * (w - h);
        self.p.circle(kx, y + h / 2.0, r, Color::white(0.75 + 0.25 * t));
        self.hit(x - 6.0, y - 6.0, w + 12.0, h + 12.0, a);
    }

    #[allow(clippy::too_many_arguments)]
    fn slider(&mut self, x: f32, y: f32, w: f32, label: &str, t: f32, value: &str, a: Act) {
        self.p.text(label, &self.f.body, x, y, w - 110.0, 22.0, Align::Left, Color::white(0.72));
        self.p.text(value, &self.f.small, x + w - 110.0, y, 110.0, 22.0, Align::Right, Color::white(0.6));
        let dragging = matches!(&self.s.drag, Some(Drag::Slider(s, ..)) if Act::Slider(*s) == a);
        let hv = self.hov(a).max(if dragging { 1.0 } else { 0.0 });
        let ty = y + 34.0;
        let t = self.s.anim.to(key(a, K_POS), t.clamp(0.0, 1.0), if dragging { 40.0 } else { 16.0 });
        self.p.rrect(x, ty - 2.0, w, 4.0, 2.0, Color::white(0.12));
        self.p.rrect(x, ty - 2.0, (w * t).max(4.0), 4.0, 2.0, theme::accent());
        let kx = x + w * t;
        self.p.circle(kx, ty, 9.0 + 2.0 * hv, theme::accent().alpha(0.18 * hv));
        self.p.circle(kx, ty, 7.0, Color::white(0.95));
        self.p.circle(kx, ty, 3.0 + hv, theme::accent());
        self.hit(x - 8.0, ty - 12.0, w + 16.0, 24.0, a);
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
        WM_GETMINMAXINFO => {
            let k = appwin::scale(hwnd);
            let mm = &mut *(lp.0 as *mut MINMAXINFO);
            mm.ptMinTrackSize = POINT { x: (980.0 * k) as i32, y: (640.0 * k) as i32 };
            return LRESULT(0);
        }
        WM_TIMER if wp.0 == TICK => appwin::redraw(hwnd),
        WM_TIMER if wp.0 == SAVE => {
            let _ = KillTimer(Some(hwnd), SAVE);
            with(|s| s.l.save());
        }
        LINK_MSG => {
            with(|s| {
                if wp.0 == 1 {
                    s.l.settings.openrgb = Lighting::load().settings.openrgb;
                }
                s.linked();
            });
        }
        ADMIN_MSG => {
            with(|s| s.admin_news(wp.0 == 1, lp.0 != 0));
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            if let Some(Some(c)) = with(|s| s.cursor()) {
                let _ = SetCursor(LoadCursorW(None, c).ok());
                return LRESULT(1);
            }
        }
        WM_MOUSEMOVE => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| s.on_move(x, y));
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut t);
        }
        0x02A3 => {
            // WM_MOUSELEAVE
            with(|s| {
                if s.drag.is_none() && (s.hover.take().is_some() || s.hover_dev.take().is_some()) {
                    appwin::redraw(hwnd);
                }
            });
        }
        WM_LBUTTONDOWN => {
            let (x, y) = appwin::mouse(hwnd, lp);
            SetCapture(hwnd);
            with(|s| s.on_down(x, y));
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| match s.hit_at(x, y) {
                Some(Act::Profile(i)) => {
                    s.l.active = i;
                    s.rename();
                }
                Some(Act::Canvas) => {
                    if let Some((i, ..)) = s.device_at(x, y) {
                        s.go(Page::Device(i));
                    }
                }
                _ => s.on_down(x, y),
            });
        }
        WM_LBUTTONUP => {
            let (x, y) = appwin::mouse(hwnd, lp);
            let _ = ReleaseCapture();
            with(|s| s.on_up(x, y));
        }
        WM_RBUTTONUP => {
            let (x, y) = appwin::mouse(hwnd, lp);
            with(|s| match s.hit_at(x, y) {
                Some(Act::Canvas) => s.canvas_down(x, y, true),
                Some(a @ (Act::Profile(_) | Act::Colour(_))) => s.act(a, x, true),
                _ => {}
            });
        }
        WM_XBUTTONUP if (wp.0 >> 16) & 0xFFFF == 1 => {
            // The mouse's back button.
            with(|s| s.back());
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
            with(|s| s.on_wheel(delta));
            return LRESULT(0);
        }
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize || wp.0 == VK_BROWSER_BACK.0 as usize => {
            with(|s| s.back());
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

/// `cascade.exe --rgb`.
pub fn run() {
    appwin::init();
    let Ok(gfx) = Gfx::new() else { return };
    let Some(hwnd) = appwin::create(w!("CascadeLighting"), "Lighting", 1280, 840, Some(proc)) else { return };
    let mut l = Lighting::load();
    if l.profiles.is_empty() {
        l.profiles.push(Profile::default());
    }
    let layer = l.profiles[l.active.min(l.profiles.len() - 1)].layers.len().saturating_sub(1);
    let now = Instant::now();
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            hwnd,
            gfx: Some(gfx),
            l,
            devices: Vec::new(),
            phase: Phase::Looking,
            connected_at: None,
            page: Page::Effects,
            history: Vec::new(),
            page_since: now,
            layer,
            device: 0,
            hits: Vec::new(),
            hover: None,
            hover_dev: None,
            pressed: None,
            drag: None,
            start: now,
            last_frame: now,
            rivals: super::hw::rivals(),
            admin_busy: None,
            admin_error: None,
            pawnio: super::hw::pawnio_installed(),
            scroll: 0.0,
            content_h: Cell::new(0.0),
            view: Cell::new((0.0, 0.0, 1.0, 1.0)),
            canvas: Cell::new((0.0, 0.0, 1.0, 1.0)),
            anim: Anim {
                values: RefCell::new(HashMap::new()),
                busy: Cell::new(false),
                live: Cell::new(false),
                dt: Cell::new(0.0),
            },
            timer: 0,
        })
    });
    let h = hwnd.0 as isize;
    std::thread::spawn(move || link_thread(h));
    appwin::run();
}
