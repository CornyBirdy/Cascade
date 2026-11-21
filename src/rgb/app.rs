//! Lighting: iCUE without iCUE, for every brand, as a page of Cascade's
//! settings (Settings > Personalisation > Lighting). `embed` puts it in the
//! settings window as a child pane; `cascade.exe --rgb` opens that page in
//! the running Cascade (or, with none running, a window of its own).
//!
//! - **Profiles** and **devices** down the side: make, rename, duplicate,
//!   delete profiles, pick one to use and edit, link programs to one so it
//!   comes on while they're in front; each device on or left out.
//! - **Lighting**: the profile's effect layers (top one over the rest), each
//!   with its effect, the devices it covers, colours, speed, size, direction,
//!   opacity and blend, and a live preview of every device.
//! - **Layout**: drag the devices where they sit on your desk (and resize
//!   them), so a wave runs across them all as one; fans as rings; flip one.
//! - **Devices**: each device on Cascade's effects or one of its own hardware
//!   modes (colours, speed, brightness, direction, saved to the device), and
//!   how many LEDs each fan or strip header has.
//! - **Settings**: on or off, brightness, frame rate, switching with apps,
//!   off when locked; OpenRGB, and administrator rights for RAM, motherboard
//!   and graphics card lighting (which installs the PawnIO driver too).
//!
//! OpenRGB is set up by itself the first time (fetched, unpacked, started),
//! and a link thread stays connected to it, so devices appear as OpenRGB
//! finds them. Every change is saved as it's made; the engine (see `engine`)
//! picks it up. Controls light on hover and press and move smoothly (`Anim`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::effects::{self, Blend, Context, Dir, Kind, Rgb, Sensor};
use super::model::{led_positions, Hardware, Layer, Lighting, Place, Profile, Target};
use super::openrgb::{self, Client, Controller};
use crate::appwin::{self, LINE};
use crate::gfx::{Align, Color, Fonts, Gfx, Painter};
use crate::theme;

/// The pane's background: the settings window's.
const BG: Color = Color(0.125, 0.125, 0.13, 1.0);
const PANEL: Color = Color(0.16, 0.16, 0.165, 1.0);
const SIDE: f32 = 224.0;
const TOP: f32 = 52.0;
const ROW: f32 = 34.0;
/// The link thread has news (devices, or how setting up is going).
const LINK_MSG: u32 = WM_APP + 1;
/// Administrator rights: a step, or done.
const ADMIN_MSG: u32 = WM_APP + 2;
const TICK: usize = 1;
const SAVE: usize = 2;

// Glyphs from Windows' icon font (Segoe Fluent Icons / MDL2 Assets).
const G_ADD: u32 = 0xE710;
const G_DELETE: u32 = 0xE74D;
const G_UP: u32 = 0xE70E;
const G_DOWN: u32 = 0xE70D;
const G_CHEVRON: u32 = 0xE70D;
const G_REFRESH: u32 = 0xE72C;
const G_DOWNLOAD: u32 = 0xE896;
const G_FOLDER: u32 = 0xE8B7;
const G_LINK: u32 = 0xE71B;
const G_WARN: u32 = 0xE7BA;
const G_CHECK: u32 = 0xE73E;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Tab {
    Lighting,
    Layout,
    Devices,
    Settings,
}

const TABS: [(Tab, &str); 4] =
    [(Tab::Lighting, "Lighting"), (Tab::Layout, "Layout"), (Tab::Devices, "Devices"), (Tab::Settings, "Settings")];

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
/// Bumped when the pane closes, so its link thread ends.
static GEN: AtomicU32 = AtomicU32::new(0);

enum Drag {
    Slider(Sl, f32, f32),
    /// A device on the canvas: its key, where it was grabbed (canvas units), resizing.
    Place(String, f32, f32, bool),
}

type Hit = (f32, f32, f32, f32, Act);

/// Smoothly moving values, keyed by what they belong to: each frame a
/// value moves part of the way to where it's going, so hovers fade,
/// toggles slide and selections glide.
struct Anim {
    values: RefCell<HashMap<u64, f32>>,
    /// Something is still on its way (frames at 60 a second).
    busy: Cell<bool>,
    /// Something moves for as long as it's shown (a spinner: 30 a second).
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
    /// In the settings window (not a window of its own).
    embedded: bool,
    gfx: Option<Gfx>,
    l: Lighting,
    devices: Vec<Controller>,
    phase: Phase,
    connected_at: Option<Instant>,
    tab: Tab,
    tab_since: Instant,
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

fn wake(hwnd: isize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), LINK_MSG, WPARAM(0), LPARAM(0));
    }
}

fn publish(hwnd: isize, f: impl FnOnce(&mut Link)) {
    f(&mut LINK.lock().unwrap_or_else(|e| e.into_inner()));
    wake(hwnd);
}

fn set_phase(hwnd: isize, p: Phase) {
    publish(hwnd, |l| l.phase = p);
}

/// Waits up to `ms`, ending early when the pane asks for something; None
/// once the pane has closed.
fn nap(ms: u64, gen: u32) -> Option<u32> {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        if GEN.load(Ordering::Acquire) != gen {
            return None;
        }
        let a = ASK.swap(0, Ordering::AcqRel);
        if a != 0 {
            return Some(a);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Some(0)
}

/// Sets OpenRGB up when it isn't there, starts it, and stays connected:
/// devices are sent to the pane as OpenRGB finds them. Ends with the pane.
fn link_thread(hwnd: isize, gen: u32) {
    let mut tried_download = false;
    let mut last_start: Option<Instant> = None;
    loop {
        if GEN.load(Ordering::Acquire) != gen {
            return;
        }
        let s = Lighting::load().settings;
        if !super::hw::running(s.port) {
            if super::hw::find(&s.openrgb).is_none() {
                if tried_download {
                    match nap(1000, gen) {
                        None => return,
                        Some(a) if a & ASK_RETRY != 0 => tried_download = false,
                        _ => {}
                    }
                    continue;
                }
                tried_download = true;
                set_phase(hwnd, Phase::Downloading(0.0));
                let r = super::hw::download(&|t| {
                    LINK.lock().unwrap_or_else(|e| e.into_inner()).phase = Phase::Downloading(t);
                    wake(hwnd);
                });
                if let Err(e) = r {
                    crate::config::log(&format!("lighting: getting OpenRGB failed: {e}"));
                    set_phase(hwnd, Phase::Failed(format!("Couldn't get OpenRGB: {e}.")));
                }
                continue;
            }
            // Started now and then; meanwhile it's given time to come up.
            if last_start.is_none_or(|t| t.elapsed() > Duration::from_secs(20)) {
                set_phase(
                    hwnd,
                    if last_start.is_some() {
                        Phase::Failed("OpenRGB didn't start: another copy may have its port, or it's blocked.".into())
                    } else {
                        Phase::Starting
                    },
                );
                last_start = Some(Instant::now());
                if s.on {
                    super::engine::ensure(true);
                }
                super::hw::start(&s.openrgb, s.port, s.admin);
            }
            match nap(400, gen) {
                None => return,
                Some(a) if a & ASK_RETRY != 0 => last_start = None,
                _ => {}
            }
            continue;
        }
        let mut c = match Client::connect(s.port, "Cascade Lighting") {
            Ok(c) => c,
            Err(e) => {
                set_phase(hwnd, Phase::Failed(format!("Couldn't connect to OpenRGB ({e}).")));
                if nap(2000, gen).is_none() {
                    return;
                }
                continue;
            }
        };
        if s.on {
            super::engine::ensure(true);
        }
        let Ok(mut list) = c.controllers() else { continue };
        publish(hwnd, |l| {
            l.phase = Phase::Connected;
            l.since = Some(Instant::now());
            l.devices = Some(list.clone());
        });
        let mut checked = Instant::now();
        loop {
            let Some(ask) = nap(400, gen) else { return };
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

    /// The top-level window, for dialogs and menus.
    fn owner(&self) -> HWND {
        unsafe { GetAncestor(self.hwnd, GA_ROOT) }
    }

    fn changed(&mut self) {
        // Saved a moment after the last change (a slider drag is many changes).
        unsafe {
            SetTimer(Some(self.hwnd), SAVE, 150, None);
        }
        appwin::redraw(self.hwnd);
    }

    fn set_tab(&mut self, t: Tab) {
        if t != self.tab {
            self.tab = t;
            self.tab_since = Instant::now();
            self.hover = None;
        }
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
            self.rivals = super::hw::rivals();
            self.changed();
        }
        appwin::redraw(self.hwnd);
    }

    fn status(&self) -> (Color, String) {
        let n = self.devices.len();
        match &self.phase {
            Phase::Connected if n > 0 => {
                (Color(0.3, 0.85, 0.45, 1.0), format!("{n} device{} through OpenRGB", if n == 1 { "" } else { "s" }))
            }
            Phase::Connected if self.connected_at.is_none_or(|t| t.elapsed() < Duration::from_secs(25)) => {
                (Color(0.95, 0.7, 0.25, 1.0), "OpenRGB is finding your devices\u{2026}".into())
            }
            Phase::Connected => (Color(0.95, 0.7, 0.25, 1.0), "No devices found: see Settings".into()),
            Phase::Downloading(t) => (theme::accent(), format!("Setting up OpenRGB\u{2026} {:.0}%", t * 100.0)),
            Phase::Looking | Phase::Starting => (Color(0.95, 0.7, 0.25, 1.0), "Starting OpenRGB\u{2026}".into()),
            Phase::Failed(_) => (Color(0.95, 0.35, 0.35, 1.0), "OpenRGB isn't connected: see Settings".into()),
        }
    }

    // ---------------------------------------------------------- actions --

    fn act(&mut self, a: Act, x: f32, right: bool) {
        use crate::menu::{check, item, show, Entry, Place as MP};
        let owner = self.owner();
        match a {
            Act::Tab(t) => self.set_tab(t),
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
                self.layer = self.p().layers.len().saturating_sub(1);
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
                if matches!(self.tab, Tab::Lighting | Tab::Settings) {
                    self.set_tab(Tab::Devices);
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
                    let (sx, sw) = (sx + 8.0, sw - 16.0);
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
        let y = profile_y(i);
        unsafe {
            if let Ok(e) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                &HSTRING::from(name),
                WS_CHILD | WS_VISIBLE | WS_BORDER | WINDOW_STYLE(0x80),
                (12.0 * k) as i32,
                ((y + 3.0) * k) as i32,
                ((SIDE - 24.0) * k) as i32,
                ((ROW - 6.0) * k) as i32,
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
        match show(self.owner(), entries, MP::Cursor) {
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
        match show(self.owner(), entries, MP::Cursor) {
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
        match self.hit_at(x, y) {
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

    fn device_at(&self, x: f32, y: f32) -> Option<(usize, String, Place)> {
        let (cx, cy, cw, ch) = self.canvas.get();
        let (u, v) = ((x - cx) / cw, (y - cy) / ch);
        let editing = self.tab == Tab::Layout;
        self.devices.iter().enumerate().rev().find_map(|(i, d)| {
            let p = self.l.layout.get(&d.key()).copied().unwrap_or_default();
            ((editing || !p.off) && u >= p.x && u <= p.x + p.w && v >= p.y && v <= p.y + p.h).then(|| (i, d.key(), p))
        })
    }

    /// A press on the canvas: pick up a device (by its corner, to resize), or its menu.
    fn canvas_down(&mut self, x: f32, y: f32, right: bool) {
        let Some((i, key, p)) = self.device_at(x, y) else { return };
        self.device = i;
        if self.tab != Tab::Layout {
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
            match show(self.owner(), entries, MP::Cursor) {
                1 => self.l.layout.entry(key).or_default().ring = !p.ring,
                2 => self.l.layout.entry(key).or_default().flip = !p.flip,
                3 => self.l.layout.entry(key).or_default().off = !p.off,
                4 => self.set_tab(Tab::Devices),
                _ => {}
            }
            self.changed();
            return;
        }
        let (cx, cy, cw, ch) = self.canvas.get();
        let (u, v) = ((x - cx) / cw, (y - cy) / ch);
        let corner = (u - (p.x + p.w)).abs() * cw < 14.0 && (v - (p.y + p.h)).abs() * ch < 14.0;
        self.drag = Some(Drag::Place(key, u - p.x, v - p.y, corner));
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

    fn cursor(&self) -> Option<PCWSTR> {
        if let Some(Drag::Place(_, _, _, r)) = &self.drag {
            return Some(if *r { IDC_SIZENWSE } else { IDC_SIZEALL });
        }
        match self.hover {
            Some(Act::Canvas) if self.tab == Tab::Layout && self.hover_dev.is_some() => Some(IDC_SIZEALL),
            Some(Act::Canvas) | None => None,
            Some(_) => Some(IDC_HAND),
        }
    }

    // ---------------------------------------------------------- drawing --

    fn paint(&mut self) {
        let Some(mut gfx) = self.gfx.take() else { return };
        let now = Instant::now();
        // A long pause (nothing moving) counts as one frame, so a hover
        // starting after it still fades in rather than jumping.
        self.anim.dt.set(now.duration_since(self.last_frame).as_secs_f32().min(1.0 / 30.0));
        self.last_frame = now;
        self.anim.busy.set(false);
        self.anim.live.set(false);
        let hwnd = self.hwnd;
        let mut hits = Vec::new();
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut r = RECT::default();
            let _ = GetClientRect(hwnd, &mut r);
            let k = appwin::scale(hwnd);
            let (pw, ph) = (r.right - r.left, r.bottom - r.top);
            if pw > 0 && ph > 0 && gfx.begin_dc(dc, pw, ph, k, BG).is_ok() {
                {
                    let p = gfx.painter();
                    let mut ui = Ui { p: &p, f: &gfx.fonts, s: self, hits: &mut hits };
                    self.draw(&mut ui, pw as f32 / k, ph as f32 / k);
                }
                let _ = gfx.end();
            }
            let _ = EndPaint(hwnd, &ps);
        }
        self.hits = hits;
        self.gfx = Some(gfx);
        self.schedule();
    }

    /// Keeps a frame timer going while something moves (fast while animating,
    /// at the effects' pace for the live preview), and none while hidden.
    fn schedule(&mut self) {
        let shown = unsafe { IsWindowVisible(self.hwnd).as_bool() };
        let preview = matches!(self.tab, Tab::Lighting | Tab::Layout)
            && !self.devices.is_empty()
            && self.p().layers.iter().any(|l| l.on && l.effect.kind.moves());
        let want = if !shown {
            0
        } else if self.anim.busy.get() {
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
        let p = ui.p;
        let accent = theme::accent();
        // The side: profiles, then devices.
        p.rrect(0.0, 0.0, SIDE, h, 10.0, Color(0.105, 0.105, 0.11, 1.0));
        p.text("Profiles", &ui.f.label, 16.0, 14.0, 150.0, 20.0, Align::Left, Color::white(0.55));
        ui.icon_button(SIDE - 40.0, 10.0, 28.0, G_ADD, Act::NewProfile);
        let act = self.anim.to(key("profile", K_POS), self.l.active as f32, 16.0);
        p.rrect(8.0, profile_y(0) + act * ROW + 2.0, SIDE - 16.0, ROW - 4.0, 6.0, Color::white(0.08));
        p.rrect(8.0, profile_y(0) + act * ROW + 10.0, 3.0, ROW - 20.0, 1.5, accent);
        for (i, pr) in self.l.profiles.iter().enumerate() {
            let y = profile_y(i);
            let a = Act::Profile(i);
            let hv = ui.hov(a);
            if i != self.l.active {
                p.rrect(8.0, y + 2.0, SIDE - 16.0, ROW - 4.0, 6.0, Color::white(0.05 * hv + 0.04 * ui.prs(a)));
            }
            p.text(&pr.name, &ui.f.body, 20.0, y, SIDE - 56.0, ROW, Align::Left, Color::white(0.92));
            if !pr.apps.is_empty() {
                ui.glyph(G_LINK, SIDE - 34.0, y + ROW / 2.0 - 7.0, 14.0, Color::white(0.45));
            }
            ui.hit(8.0, y + 2.0, SIDE - 16.0, ROW - 4.0, a);
        }
        let dy = profile_y(self.l.profiles.len()) + 18.0;
        p.text("Devices", &ui.f.label, 16.0, dy, 150.0, 20.0, Align::Left, Color::white(0.55));
        for (i, d) in self.devices.iter().enumerate() {
            let y = dy + 26.0 + i as f32 * ROW;
            if y > h - ROW {
                break;
            }
            let off = self.l.layout.get(&d.key()).is_some_and(|p| p.off);
            let a = Act::Device(i);
            let hv = ui.hov(a);
            let sel = i == self.device && self.tab == Tab::Devices;
            let fill = if sel { 0.08 } else { 0.05 * hv + 0.04 * ui.prs(a) };
            p.rrect(8.0, y + 2.0, SIDE - 16.0, ROW - 4.0, 6.0, Color::white(fill));
            p.text(
                &d.name,
                &ui.f.body,
                16.0,
                y,
                SIDE - 76.0,
                ROW,
                Align::Left,
                Color::white(if off { 0.4 } else { 0.9 }),
            );
            ui.hit(8.0, y + 2.0, SIDE - 64.0, ROW - 4.0, a);
            ui.toggle(SIDE - 52.0, y + 8.0, !off, Act::DeviceOff(i));
        }
        if self.devices.is_empty() {
            let (_, s) = self.status();
            p.text_wrap(&s, &ui.f.small, 16.0, dy + 26.0, SIDE - 30.0, 60.0, Color::white(0.5));
        }
        // Tabs and how it's connected.
        let x0 = SIDE + 18.0;
        let tw = 92.0;
        let cur = TABS.iter().position(|t| t.0 == self.tab).unwrap_or(0) as f32;
        let sel = self.anim.to(key("tab", K_POS), cur, 18.0);
        for (i, (t, name)) in TABS.iter().enumerate() {
            let x = x0 + i as f32 * (tw + 6.0);
            let a = Act::Tab(*t);
            let on = self.tab == *t;
            let hv = ui.hov(a);
            let fill = if on { 0.09 } else { 0.05 * hv + 0.04 * ui.prs(a) };
            p.rrect(x, 6.0, tw, 32.0, 6.0, Color::white(fill));
            let c = Color::white(if on { 0.96 } else { 0.62 + 0.3 * hv });
            p.text(name, &ui.f.title, x, 6.0, tw, 32.0, Align::Center, c);
            ui.hit(x, 6.0, tw, 32.0, a);
        }
        p.rrect(x0 + sel * (tw + 6.0) + 28.0, 35.0, tw - 56.0, 3.0, 1.5, accent);
        let (dot, status) = self.status();
        let sx = x0 + 4.0 * (tw + 6.0) + 10.0;
        let busy = !matches!(self.phase, Phase::Connected) || self.devices.is_empty();
        let pulse = if busy {
            self.anim.live.set(true);
            0.5 + 0.5 * (self.start.elapsed().as_secs_f32() * 3.0).sin().abs()
        } else {
            1.0
        };
        let tx = p.measure(&status, &ui.f.small);
        let sw = (w - sx - 8.0).max(0.0);
        p.text(&status, &ui.f.small, sx, 6.0, sw - 4.0, 32.0, Align::Right, Color::white(0.55));
        if sw > tx + 20.0 {
            p.circle(sx + sw - tx - 16.0, 22.0, 4.0, dot.alpha(pulse));
        }
        if let Phase::Downloading(t) = self.phase {
            let shown = self.anim.to(key("progress", K_POS), t, 8.0);
            p.rrect(x0, 44.0, w - x0, 3.0, 1.5, Color::white(0.08));
            p.rrect(x0, 44.0, (w - x0) * shown, 3.0, 1.5, accent);
        }
        // The tab, sliding in when it changes.
        let t = ease_out(self.tab_since.elapsed().as_secs_f32() / 0.24);
        if t < 1.0 {
            self.anim.busy.set(true);
        }
        let shift = (1.0 - t) * 14.0;
        let (cx, cy, cw, ch) = (x0, TOP + shift, w - x0, h - TOP - 4.0);
        match self.tab {
            Tab::Lighting => self.draw_lighting(ui, cx, cy, cw, ch),
            Tab::Layout => {
                p.text(
                    "Drag a device to where it sits on your desk; drag its corner to resize. Right-click one for more.",
                    &ui.f.small,
                    cx,
                    cy,
                    cw,
                    22.0,
                    Align::Left,
                    Color::white(0.55),
                );
                self.draw_canvas(ui, (cx, cy + 30.0, cw, ch - 30.0), true);
            }
            Tab::Devices => self.draw_devices(ui, cx, cy, cw),
            Tab::Settings => self.draw_settings(ui, cx, cy, cw),
        }
        if t < 1.0 {
            p.rrect(x0 - 2.0, TOP, w - x0 + 2.0, h - TOP, 0.0, BG.alpha(1.0 - t));
        }
    }

    fn draw_lighting(&self, ui: &mut Ui, x: f32, y: f32, w: f32, h: f32) {
        let p = ui.p;
        let accent = theme::accent();
        let pr = self.p();
        // Layers, top first.
        let lw = (w * 0.36).clamp(230.0, 290.0);
        let lh = h * 0.55;
        p.rrect(x, y, lw, lh, 8.0, PANEL);
        p.text("Layers", &ui.f.label, x + 14.0, y + 8.0, 100.0, 22.0, Align::Left, Color::white(0.55));
        let n = pr.layers.len();
        let mut bx = x + lw - 8.0 - 4.0 * 30.0;
        for (g, a, ok) in [
            (G_ADD, Act::AddLayer, true),
            (G_UP, Act::LayerUp, self.layer + 1 < n),
            (G_DOWN, Act::LayerDown, self.layer > 0 && n > 0),
            (G_DELETE, Act::LayerDelete, n > 0),
        ] {
            if ok {
                ui.icon_button(bx, y + 5.0, 28.0, g, a);
            } else {
                ui.glyph(g, bx + 7.0, y + 12.0, 14.0, Color::white(0.2));
            }
            bx += 30.0;
        }
        let ly0 = y + 40.0;
        let rows = ((lh - 46.0) / ROW).floor().max(1.0) as usize;
        if n > 0 {
            let sel_row = (n - 1 - self.layer.min(n - 1)) as f32;
            let s = self.anim.to(key("layer", K_POS), sel_row, 16.0);
            if s < rows as f32 {
                p.rrect(x + 6.0, ly0 + s * ROW, lw - 12.0, ROW - 4.0, 5.0, accent.alpha(0.22));
            }
        }
        for (row, i) in (0..n).rev().enumerate().take(rows) {
            let l = &pr.layers[i];
            let ly = ly0 + row as f32 * ROW;
            let a = Act::Layer(i);
            let hv = ui.hov(a);
            if i != self.layer {
                p.rrect(x + 6.0, ly, lw - 12.0, ROW - 4.0, 5.0, Color::white(0.05 * hv + 0.04 * ui.prs(a)));
            }
            ui.hit(x + 54.0, ly, lw - 60.0, ROW - 4.0, a);
            ui.toggle(x + 12.0, ly + 6.0, l.on, Act::LayerEye(i));
            let c = Color::white(if l.on { 0.92 } else { 0.45 });
            p.text(&l.name, &ui.f.body, x + 60.0, ly, lw - 110.0, ROW - 4.0, Align::Left, c);
            for (k, col) in l.effect.colours.iter().take(3).enumerate() {
                let dc = rgb_of(*col).alpha(if l.on { 1.0 } else { 0.4 });
                p.circle(x + lw - 20.0 - k as f32 * 11.0, ly + (ROW - 4.0) / 2.0, 4.0, dc);
            }
        }
        if n == 0 {
            p.text(
                "No layers: + adds an effect",
                &ui.f.small,
                x + 14.0,
                y + 44.0,
                lw - 28.0,
                20.0,
                Align::Left,
                Color::white(0.45),
            );
        }
        // The profile itself.
        let py = y + lh + 10.0;
        p.rrect(x, py, lw, h - lh - 10.0, 8.0, PANEL);
        p.text("This profile", &ui.f.label, x + 14.0, py + 8.0, 200.0, 22.0, Align::Left, Color::white(0.55));
        let b = pr.brightness;
        ui.slider(
            x + 14.0,
            py + 36.0,
            lw - 28.0,
            "Brightness",
            b,
            &format!("{:.0}%", b * 100.0),
            Act::Slider(Sl::ProfileBright),
        );
        let apps =
            if pr.apps.is_empty() { "None: it's used whenever it's chosen".to_string() } else { pr.apps.join(", ") };
        p.text("Comes on with", &ui.f.small, x + 14.0, py + 92.0, lw - 28.0, 16.0, Align::Left, Color::white(0.55));
        p.text_wrap(&apps, &ui.f.body, x + 14.0, py + 110.0, lw - 28.0, 40.0, Color::white(0.85));
        ui.button(x + 14.0, py + 154.0, lw - 28.0, 30.0, "Linked programs\u{2026}", Act::Apps);
        // The chosen layer's settings.
        let ex = x + lw + 14.0;
        let ew = w - lw - 14.0;
        let top_h = (h - 210.0).max(250.0);
        p.rrect(ex, y, ew, top_h, 8.0, PANEL);
        let preview = (ex, y + top_h + 10.0, ew, h - top_h - 10.0);
        let Some(l) = pr.layers.get(self.layer) else {
            let c = Color::white(0.5);
            p.text(
                "Pick a layer, or add one with +.",
                &ui.f.body,
                ex + 16.0,
                y + 16.0,
                ew - 32.0,
                24.0,
                Align::Left,
                c,
            );
            self.draw_canvas(ui, preview, false);
            return;
        };
        let e = &l.effect;
        let lx = ex + 16.0;
        let cx = ex + 130.0;
        let cw = (ew - 146.0).min(320.0);
        let sw = (ew - 32.0).min(500.0);
        let mut ry = y + 12.0;
        let label = |ui: &mut Ui, ry: f32, s: &str| {
            ui.p.text(s, &ui.f.body, lx, ry, 110.0, 30.0, Align::Left, Color::white(0.75));
        };
        label(ui, ry, "Effect");
        ui.select(cx, ry, cw, e.kind.name(), Act::Effect);
        ry += 40.0;
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
        ry += 40.0;
        let (_, hi) = e.kind.colours();
        if hi > 0 {
            label(ui, ry, "Colours");
            for (i, c) in e.colours.iter().enumerate() {
                ui.swatch(cx + i as f32 * 38.0, ry, rgb_of(*c), Act::Colour(i));
            }
            if e.colours.len() < hi {
                ui.icon_button(cx + e.colours.len() as f32 * 38.0, ry, 30.0, G_ADD, Act::AddColour);
            }
            ry += 40.0;
        }
        if e.kind.moves() || e.kind == Kind::Gradient {
            ui.slider(lx, ry, sw, "Speed", e.speed, &format!("{:.0}%", e.speed * 100.0), Act::Slider(Sl::Speed));
            ry += 52.0;
            ui.slider(lx, ry, sw, "Size", e.size, &format!("{:.0}%", e.size * 100.0), Act::Slider(Sl::Size));
            ry += 52.0;
            label(ui, ry, "Direction");
            const DIRS: [(Dir, &str); 6] = [
                (Dir::Right, "\u{2192}"),
                (Dir::Left, "\u{2190}"),
                (Dir::Down, "\u{2193}"),
                (Dir::Up, "\u{2191}"),
                (Dir::Out, "Out"),
                (Dir::In, "In"),
            ];
            let seg = ((ew - 146.0) / 6.0).min(46.0);
            let cur = DIRS.iter().position(|d| d.0 == e.dir).unwrap_or(0) as f32;
            let s = self.anim.to(key("dir", K_POS), cur, 18.0);
            p.rrect(cx + s * seg, ry, seg - 4.0, 30.0, 6.0, accent.alpha(0.5));
            for (i, (d, n)) in DIRS.iter().enumerate() {
                let bx = cx + i as f32 * seg;
                let a = Act::Dir(*d);
                if e.dir != *d {
                    let fill = 0.07 + 0.06 * ui.hov(a) + 0.04 * ui.prs(a);
                    p.rrect(bx, ry, seg - 4.0, 30.0, 6.0, Color::white(fill));
                }
                p.text(n, &ui.f.body, bx, ry, seg - 4.0, 30.0, Align::Center, Color::white(0.9));
                ui.hit(bx, ry, seg - 4.0, 30.0, a);
            }
            ry += 40.0;
        }
        if e.kind == Kind::Temperature {
            label(ui, ry, "Sensor");
            ui.select(cx, ry, cw, if e.sensor == Sensor::Cpu { "CPU" } else { "Graphics card" }, Act::Sensor);
            ry += 40.0;
            let (a, b) = e.temp_range;
            ui.slider(lx, ry, sw, "Cool at", (a - 20.0) / 80.0, &format!("{a:.0} \u{b0}C"), Act::Slider(Sl::TempLo));
            ry += 52.0;
            ui.slider(lx, ry, sw, "Hot at", (b - 20.0) / 80.0, &format!("{b:.0} \u{b0}C"), Act::Slider(Sl::TempHi));
            ry += 52.0;
        }
        ui.slider(lx, ry, sw, "Opacity", l.opacity, &format!("{:.0}%", l.opacity * 100.0), Act::Slider(Sl::Opacity));
        ry += 52.0;
        let blend = match l.blend {
            Blend::Normal => "Normal",
            Blend::Add => "Add",
            Blend::Screen => "Screen",
            Blend::Multiply => "Multiply",
            Blend::Lighten => "Lighten",
        };
        label(ui, ry, "Blend");
        ui.select(cx, ry, cw, blend, Act::Blend);
        // The preview: every device as laid out, lit live.
        self.draw_canvas(ui, preview, false);
    }

    /// The devices on the canvas, lit as the profile lights them now.
    fn draw_canvas(&self, ui: &mut Ui, (x, y, w, h): (f32, f32, f32, f32), editing: bool) {
        let p = ui.p;
        self.canvas.set((x, y, w, h));
        p.rrect(x, y, w, h, 8.0, Color(0.07, 0.07, 0.075, 1.0));
        p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 8.0, LINE, 1.0);
        ui.hit(x, y, w, h, Act::Canvas);
        let pr = self.p();
        let now = self.start.elapsed().as_secs_f32();
        let ctx = Context {
            t: now,
            cpu_temp: 55.0,
            gpu_temp: 50.0,
            level: 0.5 + 0.3 * (now * 2.0).sin(),
            bands: (0..16).map(|i| 0.5 + 0.4 * (now * 3.0 + i as f32).sin()).collect(),
            ..Default::default()
        };
        let on = if self.l.settings.on { 1.0 } else { 0.15 };
        let bright = (self.l.settings.brightness * pr.brightness).clamp(0.0, 1.0) * on;
        for (di, d) in self.devices.iter().enumerate() {
            let dk = d.key();
            let place = self.l.layout.get(&dk).copied().unwrap_or_default();
            if place.off && !editing {
                continue;
            }
            let (bx, by, bw, bh) = (x + place.x * w, y + place.y * h, place.w * w, place.h * h);
            let lit = if self.hover_dev == Some(di) { 1.0 } else { 0.0 };
            let hv = self.anim.to(key(("dev", di), K_HOVER), lit, 18.0);
            let sel = editing && di == self.device;
            p.rrect(bx, by, bw, bh, 6.0, Color::white(if place.off { 0.02 } else { 0.05 + 0.03 * hv }));
            let edge = if sel { theme::accent() } else { Color::white(0.15 + 0.2 * hv) };
            p.stroke_rrect(bx + 0.5, by + 0.5, bw - 1.0, bh - 1.0, 6.0, edge, if sel { 2.0 } else { 1.0 });
            if editing {
                let c = Color::white(0.6 + 0.3 * hv);
                p.text(&d.name, &ui.f.small, bx + 4.0, by - 16.0, bw.max(160.0), 14.0, Align::Left, c);
                p.rrect(bx + bw - 8.0, by + bh - 8.0, 8.0, 8.0, 2.0, Color::white(0.4 + 0.5 * hv));
            }
            let hw = pr.hardware.get(&dk);
            let dot = (bw.min(bh) / 6.0).clamp(2.0, 6.0);
            for (led, zone) in led_positions(d, &place) {
                let c = if hw.is_some() {
                    Rgb(0.35, 0.35, 0.38)
                } else {
                    let mut c = Rgb::BLACK;
                    for l in pr.layers.iter().filter(|l| l.on && l.covers(&dk, zone)) {
                        c = effects::blend(c, effects::colour(&l.effect, &led, &ctx), l.blend, l.opacity);
                    }
                    c.scale(bright)
                };
                let a = if place.off { 0.2 } else { 1.0 };
                let (lx, ly) = (x + led.x * w, y + led.y * h);
                p.circle(lx, ly, dot * 2.2, Color(c.0, c.1, c.2, 0.1 * a));
                p.circle(lx, ly, dot, Color(c.0, c.1, c.2, a));
            }
        }
        if self.devices.is_empty() {
            let (_, s) = self.status();
            p.text(&s, &ui.f.body, x, y + h / 2.0 - 10.0, w, 20.0, Align::Center, Color::white(0.45));
        }
    }

    fn draw_devices(&self, ui: &mut Ui, x: f32, y: f32, w: f32) {
        let p = ui.p;
        let Some(d) = self.devices.get(self.device) else {
            let (_, s) = self.status();
            p.text(&format!("No devices yet. {s}"), &ui.f.body, x, y, w, 24.0, Align::Left, Color::white(0.5));
            return;
        };
        let key_ = d.key();
        p.text(&d.name, &ui.f.big, x, y, w, 40.0, Align::Left, Color::white(0.95));
        let info = format!(
            "{} \u{b7} {} \u{b7} {} LEDs{}",
            openrgb::kind_name(d.kind),
            if d.vendor.is_empty() { "Unknown maker" } else { &d.vendor },
            d.leds.len(),
            if d.location.is_empty() { String::new() } else { format!(" \u{b7} {}", d.location) }
        );
        p.text(&info, &ui.f.small, x, y + 44.0, w, 18.0, Align::Left, Color::white(0.55));
        let mut ry = y + 76.0;
        let hw = self.p().hardware.get(&key_).cloned();
        let mode = hw.as_ref().and_then(|h| d.modes.iter().find(|m| m.name == h.mode));
        const DIRECTIONS: u32 =
            openrgb::MODE_HAS_DIRECTION_LR | openrgb::MODE_HAS_DIRECTION_UD | openrgb::MODE_HAS_DIRECTION_HV;
        const SAVES: u32 = openrgb::MODE_MANUAL_SAVE | openrgb::MODE_AUTOMATIC_SAVE;
        let mut ph = 64.0;
        if let Some(m) = mode {
            let has = |f: u32| m.flags & f != 0;
            let gap = |b: bool, h: f32| if b { h } else { 0.0 };
            ph += gap(m.colors_max > 0, 40.0)
                + gap(has(openrgb::MODE_HAS_SPEED), 52.0)
                + gap(has(openrgb::MODE_HAS_BRIGHTNESS), 52.0)
                + gap(has(DIRECTIONS), 40.0)
                + gap(has(SAVES), 40.0);
        } else {
            ph += 30.0;
        }
        p.rrect(x, ry, w, ph, 8.0, PANEL);
        let (lx, cx, cw, sw) = (x + 16.0, x + 150.0, (w - 166.0).min(360.0), (w - 32.0).min(500.0));
        ry += 14.0;
        p.text("Lighting", &ui.f.body, lx, ry, 130.0, 30.0, Align::Left, Color::white(0.75));
        let current = hw.as_ref().map_or("Cascade's lighting (the profile's layers)", |h| h.mode.as_str());
        ui.select(cx, ry, cw, current, Act::HwMode);
        ry += 40.0;
        if let (Some(h), Some(m)) = (&hw, mode) {
            if m.colors_max > 0 {
                p.text("Colours", &ui.f.body, lx, ry, 130.0, 30.0, Align::Left, Color::white(0.75));
                let n = (h.colours.len() as u32).clamp(m.colors_min.max(1), m.colors_max) as usize;
                for i in 0..n {
                    let c = rgb_of(h.colours.get(i).copied().unwrap_or(0xFFFFFF));
                    ui.swatch(cx + i as f32 * 38.0, ry, c, Act::HwColour(i));
                }
                if (n as u32) < m.colors_max {
                    ui.icon_button(cx + n as f32 * 38.0, ry, 30.0, G_ADD, Act::HwColour(n));
                }
                ry += 40.0;
            }
            if m.flags & openrgb::MODE_HAS_SPEED != 0 {
                let v = format!("{:.0}%", h.speed * 100.0);
                ui.slider(lx, ry, sw, "Speed", h.speed, &v, Act::Slider(Sl::HwSpeed));
                ry += 52.0;
            }
            if m.flags & openrgb::MODE_HAS_BRIGHTNESS != 0 {
                let v = format!("{:.0}%", h.brightness * 100.0);
                ui.slider(lx, ry, sw, "Brightness", h.brightness, &v, Act::Slider(Sl::HwBright));
                ry += 52.0;
            }
            if m.flags & DIRECTIONS != 0 {
                let names = ["Left", "Right", "Up", "Down", "Horizontal", "Vertical"];
                p.text("Direction", &ui.f.body, lx, ry, 130.0, 30.0, Align::Left, Color::white(0.75));
                let v = names.get(h.direction as usize).copied().unwrap_or("Left");
                ui.select(cx, ry, cw.min(200.0), v, Act::HwDir);
                ry += 40.0;
            }
            if m.flags & SAVES != 0 {
                let t = "Save to the device (keeps it with Cascade closed)";
                p.text(t, &ui.f.body, lx, ry, w - 100.0, 30.0, Align::Left, Color::white(0.75));
                ui.toggle(x + w - 60.0, ry + 6.0, h.save, Act::HwSave);
            }
        } else {
            p.text_wrap(
                "The device follows the profile's layers. Pick one of its own modes above to leave it to the device (runs with no software, can be saved to it).",
                &ui.f.small,
                lx,
                ry,
                w - 32.0,
                36.0,
                Color::white(0.55),
            );
        }
        // Zones, and the size of the ones that can be set (fan and strip headers).
        let mut zy = y + 76.0 + ph + 16.0;
        p.text("Zones", &ui.f.label, x, zy, 200.0, 20.0, Align::Left, Color::white(0.55));
        zy += 24.0;
        for (i, z) in d.zones.iter().enumerate() {
            if z.leds_max > z.leds_min {
                let t = z.leds_count.saturating_sub(z.leds_min) as f32 / (z.leds_max - z.leds_min) as f32;
                let label = format!("{} (LEDs)", z.name);
                ui.slider(x, zy, (w - 16.0).min(500.0), &label, t, &z.leds_count.to_string(), Act::Slider(Sl::Zone(i)));
                zy += 52.0;
            } else {
                let t = format!("{} \u{b7} {} LEDs", z.name, z.leds_count);
                p.text(&t, &ui.f.body, x, zy, w, 24.0, Align::Left, Color::white(0.8));
                zy += 28.0;
            }
        }
        let place = self.l.layout.get(&key_).copied().unwrap_or_default();
        p.text("Lay out as a ring (fans)", &ui.f.body, x, zy + 8.0, 300.0, 24.0, Align::Left, Color::white(0.8));
        ui.toggle(x + 320.0, zy + 10.0, place.ring, Act::Toggle(3));
        p.text("Flip", &ui.f.body, x, zy + 40.0, 300.0, 24.0, Align::Left, Color::white(0.8));
        ui.toggle(x + 320.0, zy + 42.0, place.flip, Act::Toggle(4));
    }

    fn draw_settings(&self, ui: &mut Ui, x: f32, y: f32, w: f32) {
        let p = ui.p;
        let s = &self.l.settings;
        let w = w.min(720.0);
        let mut ry = y;
        let line = |ui: &mut Ui, ry: &mut f32, label: &str, on: bool, a: Act| {
            let hv = ui.hov(a);
            ui.p.rrect(x - 8.0, *ry - 3.0, w + 8.0, 36.0, 6.0, Color::white(0.03 * hv));
            ui.p.text(label, &ui.f.body, x, *ry, w - 80.0, 30.0, Align::Left, Color::white(0.85));
            ui.toggle(x + w - 50.0, *ry + 6.0, on, a);
            ui.hit(x - 8.0, *ry - 3.0, w + 8.0, 36.0, a);
            *ry += 38.0;
        };
        line(ui, &mut ry, "Lighting on", s.on, Act::Toggle(0));
        let b = s.brightness;
        ui.slider(x, ry, w, "Brightness", b, &format!("{:.0}%", b * 100.0), Act::Slider(Sl::Bright));
        ry += 52.0;
        let fps = (s.fps as f32 - 5.0) / 115.0;
        ui.slider(x, ry, w, "Frames a second (moving effects)", fps, &s.fps.to_string(), Act::Slider(Sl::Fps));
        ry += 52.0;
        let sf = (s.screen_fps as f32 - 1.0) / 59.0;
        let sv = format!("{} a second", s.screen_fps);
        ui.slider(x, ry, w, "Screen ambience: looks at the screen", sf, &sv, Act::Slider(Sl::ScreenFps));
        ry += 52.0;
        line(ui, &mut ry, "Switch profiles with the program in front", s.auto_switch, Act::Toggle(1));
        line(ui, &mut ry, "Lights off while the PC is locked or its screens are off", s.off_when_idle, Act::Toggle(2));
        ry += 8.0;
        p.text("OpenRGB", &ui.f.title, x, ry, 300.0, 24.0, Align::Left, Color::white(0.95));
        ry += 28.0;
        let (dot, status) = self.status();
        p.circle(x + 5.0, ry + 10.0, 4.0, dot);
        p.text(&status, &ui.f.body, x + 16.0, ry, w - 16.0, 20.0, Align::Left, Color::white(0.85));
        ry += 24.0;
        let found = super::hw::find(&s.openrgb);
        let detail = match (&self.phase, &found) {
            (Phase::Failed(e), _) => e.clone(),
            (_, Some(p)) => p.display().to_string(),
            (_, None) => "Not on this PC yet: Cascade gets it by itself.".into(),
        };
        let failed = matches!(self.phase, Phase::Failed(_));
        let dc = if failed { Color(1.0, 0.55, 0.5, 1.0) } else { Color::white(0.5) };
        p.text_wrap(&detail, &ui.f.small, x, ry, w, 34.0, dc);
        ry += 36.0;
        p.text_wrap(
            "Cascade drives your devices through OpenRGB (free and open source), which talks to each one directly: Corsair, ASUS Aura, Gigabyte RGB Fusion, Razer, Logitech, HyperX, MSI, NZXT, SteelSeries and hundreds more. It runs out of sight.",
            &ui.f.small,
            x,
            ry,
            w,
            48.0,
            Color::white(0.45),
        );
        ry += 50.0;
        let mut bx = x;
        for (label, g, a) in [
            ("Look for devices", G_REFRESH, Act::Rescan),
            (if found.is_some() { "Reconnect" } else { "Get OpenRGB" }, G_DOWNLOAD, Act::Retry),
            ("Use another copy\u{2026}", G_FOLDER, Act::ChooseOpenRgb),
        ] {
            let bw = p.measure(label, &ui.f.body) + 52.0;
            ui.icon_text_button(bx, ry, bw, 32.0, label, g, a);
            bx += bw + 8.0;
        }
        ry += 46.0;
        // Administrator rights, which installs PawnIO too.
        let busy = self.admin_busy.as_deref();
        let a = Act::Admin;
        let hv = ui.hov(a);
        p.rrect(x - 8.0, ry - 3.0, w + 8.0, 52.0, 6.0, Color::white(0.03 * hv));
        ui.hit(x - 8.0, ry - 3.0, w + 8.0, 52.0, a);
        let t = "Administrator rights (RAM, motherboard and graphics card lighting)";
        p.text(t, &ui.f.body, x, ry, w - 80.0, 24.0, Align::Left, Color::white(0.85));
        let (desc, dc) = match (busy, &self.admin_error) {
            (Some(step), _) => (step.to_string(), Color::white(0.55)),
            (None, Some(e)) => (format!("Didn't work: {e}"), Color(1.0, 0.55, 0.5, 1.0)),
            (None, None) if s.admin && self.pawnio => {
                ("On \u{b7} the PawnIO driver is installed".into(), Color::white(0.5))
            }
            (None, None) => ("Also installs the PawnIO driver they need; Windows asks once.".into(), Color::white(0.5)),
        };
        p.text(&desc, &ui.f.small, x, ry + 24.0, w - 80.0, 20.0, Align::Left, dc);
        if busy.is_some() {
            self.anim.live.set(true);
            let ang = self.start.elapsed().as_secs_f32() * 300.0;
            p.stroke_circle(x + w - 32.0, ry + 22.0, 8.0, Color::white(0.1), 2.0);
            p.arc(x + w - 32.0, ry + 22.0, 8.0, ang, ang + 100.0, theme::accent(), 2.0);
        } else {
            ui.toggle(x + w - 50.0, ry + 13.0, s.admin, a);
        }
        ry += 58.0;
        if s.admin && self.pawnio {
            ui.glyph(G_CHECK, x, ry + 3.0, 14.0, Color(0.3, 0.85, 0.45, 1.0));
            let t = "RAM, motherboard and graphics card lighting can be reached.";
            p.text(t, &ui.f.small, x + 20.0, ry, w, 20.0, Align::Left, Color::white(0.55));
            ry += 28.0;
        }
        if !self.rivals.is_empty() {
            p.rrect(x - 8.0, ry, w + 8.0, 64.0, 8.0, Color(0.45, 0.25, 0.05, 0.5));
            ui.glyph(G_WARN, x + 4.0, ry + 22.0, 18.0, Color(1.0, 0.75, 0.3, 1.0));
            p.text_wrap(
                &format!(
                    "Running now: {}. These hold their devices and fight Cascade (and OpenRGB) for them: close them and stop them starting with Windows (or uninstall them).",
                    self.rivals.join(", ")
                ),
                &ui.f.small,
                x + 32.0,
                ry + 8.0,
                w - 44.0,
                50.0,
                Color::white(0.9),
            );
        }
    }
}

fn profile_y(i: usize) -> f32 {
    42.0 + i as f32 * ROW
}

/// Immediate-mode controls: drawn (lit by hover and press, moving smoothly),
/// and their hit areas noted for the mouse.
struct Ui<'a, 'b> {
    p: &'a Painter<'b>,
    f: &'a Fonts,
    s: &'a State,
    hits: &'a mut Vec<Hit>,
}

impl Ui<'_, '_> {
    fn hit(&mut self, x: f32, y: f32, w: f32, h: f32, a: Act) {
        self.hits.push((x, y, w, h, a));
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

    /// A button's plate, lit by hover and pressed in a little.
    fn plate(&mut self, x: f32, y: f32, w: f32, h: f32, a: Act) -> (f32, f32) {
        let (hv, pr) = (self.hov(a), self.prs(a));
        let fill = Color::white(0.08 + 0.05 * hv + 0.03 * pr);
        self.p.rrect(x + pr, y + pr, w - 2.0 * pr, h - 2.0 * pr, 6.0, fill);
        self.p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 6.0, Color::white(0.06 + 0.07 * hv), 1.0);
        self.hit(x, y, w, h, a);
        (hv, pr)
    }

    fn button(&mut self, x: f32, y: f32, w: f32, h: f32, label: &str, a: Act) {
        let (hv, _) = self.plate(x, y, w, h, a);
        self.p.text(label, &self.f.body, x + 8.0, y, w - 16.0, h, Align::Center, Color::white(0.9 + 0.1 * hv));
    }

    #[allow(clippy::too_many_arguments)]
    fn icon_text_button(&mut self, x: f32, y: f32, w: f32, h: f32, label: &str, g: u32, a: Act) {
        let (hv, _) = self.plate(x, y, w, h, a);
        let c = Color::white(0.88 + 0.12 * hv);
        self.glyph(g, x + 12.0, y + h / 2.0 - 7.0, 14.0, c);
        self.p.text(label, &self.f.body, x + 34.0, y, w - 40.0, h, Align::Left, c);
    }

    fn icon_button(&mut self, x: f32, y: f32, size: f32, g: u32, a: Act) {
        let (hv, pr) = self.plate(x, y, size, size, a);
        let gs = (size * 0.5).round();
        self.glyph(g, x + (size - gs) / 2.0, y + (size - gs) / 2.0 + pr, gs, Color::white(0.75 + 0.25 * hv));
    }

    /// A field that opens a menu: its value and a chevron.
    fn select(&mut self, x: f32, y: f32, w: f32, value: &str, a: Act) {
        let (hv, pr) = self.plate(x, y, w, 30.0, a);
        self.p.text(value, &self.f.body, x + 10.0, y, w - 40.0, 30.0, Align::Left, Color::white(0.92));
        self.glyph(G_CHEVRON, x + w - 24.0, y + 9.0 + pr, 12.0, Color::white(0.5 + 0.4 * hv));
    }

    fn swatch(&mut self, x: f32, y: f32, c: Color, a: Act) {
        let hv = self.hov(a);
        let g = 2.0 * hv - self.prs(a);
        self.p.rrect(x - g, y - g, 32.0 + 2.0 * g, 30.0 + 2.0 * g, 6.0, c);
        let edge = Color::white(0.25 + 0.5 * hv);
        self.p.stroke_rrect(x - g + 0.5, y - g + 0.5, 31.0 + 2.0 * g, 29.0 + 2.0 * g, 6.0, edge, 1.0);
        self.hit(x, y, 32.0, 30.0, a);
    }

    fn toggle(&mut self, x: f32, y: f32, on: bool, a: Act) {
        let (w, h) = (36.0, 18.0);
        let t = self.s.anim.to(key(a, K_ON), if on { 1.0 } else { 0.0 }, 18.0);
        let hv = self.hov(a);
        let off_c = Color::white(0.12 + 0.06 * hv);
        let on_c = mix(theme::accent(), Color::white(1.0), 0.12 * hv);
        self.p.rrect(x, y, w, h, h / 2.0, mix(off_c, on_c, t));
        let r = h / 2.0 - 3.0 + 0.8 * hv - 0.8 * self.prs(a);
        let kx = x + h / 2.0 + t * (w - h);
        self.p.circle(kx, y + h / 2.0, r, Color::white(0.95));
        self.hit(x - 4.0, y - 4.0, w + 8.0, h + 8.0, a);
    }

    #[allow(clippy::too_many_arguments)]
    fn slider(&mut self, x: f32, y: f32, w: f32, label: &str, t: f32, value: &str, a: Act) {
        self.p.text(label, &self.f.body, x, y, w - 100.0, 20.0, Align::Left, Color::white(0.75));
        self.p.text(value, &self.f.small, x + w - 100.0, y, 100.0, 20.0, Align::Right, Color::white(0.6));
        let dragging = matches!(&self.s.drag, Some(Drag::Slider(s, ..)) if Act::Slider(*s) == a);
        let hv = self.hov(a).max(if dragging { 1.0 } else { 0.0 });
        let ty = y + 32.0;
        let t = self.s.anim.to(key(a, K_POS), t.clamp(0.0, 1.0), if dragging { 40.0 } else { 16.0 });
        self.p.rrect(x, ty - 2.0, w, 4.0, 2.0, Color::white(0.12));
        self.p.rrect(x, ty - 2.0, (w * t).max(4.0), 4.0, 2.0, theme::accent());
        let kx = x + w * t;
        self.p.circle(kx, ty, 7.0 + 4.0 * hv, theme::accent().alpha(0.2 * hv));
        self.p.circle(kx, ty, 7.0, Color::white(0.95));
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
        WM_SIZE | WM_SHOWWINDOW => appwin::redraw(hwnd),
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
                        s.device = i;
                        s.set_tab(Tab::Devices);
                        appwin::redraw(hwnd);
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
        WM_DESTROY => {
            // Saved, and its link thread let go; a window of its own also ends the process.
            GEN.fetch_add(1, Ordering::AcqRel);
            let alone = STATE.with(|s| {
                let mut s = s.borrow_mut();
                let alone = s.as_ref().is_some_and(|s| !s.embedded);
                if let Some(st) = s.take() {
                    st.l.save();
                }
                alone
            });
            if alone {
                PostQuitMessage(0);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, m, wp, lp)
}

fn state(hwnd: HWND, embedded: bool) -> Option<State> {
    let gfx = Gfx::new().ok()?;
    let mut l = Lighting::load();
    if l.profiles.is_empty() {
        l.profiles.push(Profile::default());
    }
    let layer = l.profiles[l.active.min(l.profiles.len() - 1)].layers.len().saturating_sub(1);
    let now = Instant::now();
    Some(State {
        hwnd,
        embedded,
        gfx: Some(gfx),
        l,
        devices: Vec::new(),
        phase: Phase::Looking,
        connected_at: None,
        tab: Tab::Lighting,
        tab_since: now.checked_sub(Duration::from_secs(1)).unwrap_or(now),
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
        canvas: Cell::new((0.0, 0.0, 1.0, 1.0)),
        anim: Anim {
            values: RefCell::new(HashMap::new()),
            busy: Cell::new(false),
            live: Cell::new(false),
            dt: Cell::new(0.0),
        },
        timer: 0,
    })
}

fn begin(hwnd: HWND, embedded: bool) -> bool {
    let Some(st) = state(hwnd, embedded) else { return false };
    STATE.with(|s| *s.borrow_mut() = Some(st));
    let gen = GEN.fetch_add(1, Ordering::AcqRel) + 1;
    let h = hwnd.0 as isize;
    std::thread::spawn(move || link_thread(h, gen));
    true
}

/// Makes the Lighting pane, a child of `parent` (the settings window),
/// hidden: the settings window places and shows it (see `settings`).
pub fn embed(parent: HWND) -> Option<HWND> {
    unsafe {
        let inst = appwin::instance();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(proc),
            hInstance: inst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("CascadeLightingPane"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("CascadeLightingPane"),
            w!("Lighting"),
            WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
            0,
            0,
            10,
            10,
            Some(parent),
            None,
            Some(inst),
            None,
        )
        .ok()?;
        if !begin(hwnd, true) {
            let _ = DestroyWindow(hwnd);
            return None;
        }
        Some(hwnd)
    }
}

/// `cascade.exe --rgb`: opens Settings > Lighting in the running Cascade, or
/// (with none running) a Lighting window of its own.
pub fn run() {
    if let Some(main) = crate::helper::main_window() {
        unsafe {
            let _ = PostMessageW(
                Some(main),
                crate::msg::OPEN_SETTINGS_AT,
                WPARAM(crate::settings::LIGHTING + 1),
                LPARAM(0),
            );
        }
        return;
    }
    appwin::init();
    let Some(hwnd) = appwin::create(w!("CascadeLighting"), "Lighting", 1240, 820, Some(proc)) else { return };
    if !begin(hwnd, false) {
        return;
    }
    appwin::run();
}
