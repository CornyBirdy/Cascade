//! The Cascade launcher (Start menu replacement), laid out like Windows 11
//! with a sidebar: places and power on the left, pinned apps and recent
//! files in the middle, and every app A to Z on the right. Typing searches.
//!
//! The app list comes from the shell's AppsFolder, the same source the
//! Windows Start menu uses, so Store apps are included.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use windows::core::{Result, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, SetFocus, TrackMouseEvent, INPUT, INPUT_MOUSE, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::backdrop::Backdrop;
use crate::config::Config;
use crate::dock::Dock;
use crate::gfx::{Align, Color, Gfx, Layer, Painter};
use crate::msg;
use crate::shell::Power;
use crate::smart::{self, Answer};
use crate::spring::Spring;
use crate::theme;

/// Sidebar width (when shown), and the slim icon rail it shrinks to.
const SIDE: f32 = 184.0;
const RAIL: f32 = 52.0;
const CELL_W: f32 = 90.0;
const CELL_H: f32 = 84.0;
/// All-apps column width (wider when it has the menu to itself).
const RIGHT_W: f32 = 220.0;
const ALONE_W: f32 = 300.0;
const PAD: f32 = 16.0;
const TOP: f32 = 76.0;
const LIST_ROW: f32 = 34.0;
const RESULT_ROW: f32 = 46.0;
const RECENT_ROW: f32 = 46.0;
const SIDE_ROW: f32 = 38.0;

/// Opens Cascade's settings window (listed with the apps).
pub const SETTINGS_TARGET: &str = "cascade:settings";

/// Sidebar places: label, what to open, glyph.
const PLACES: [(&str, &str, Glyph); 8] = [
    ("Settings", "ms-settings:", Glyph::Gear),
    ("File Explorer", "explorer.exe", Glyph::Folder),
    ("Downloads", "shell:Downloads", Glyph::Download),
    ("Documents", "shell:Personal", Glyph::Document),
    ("Pictures", "shell:My Pictures", Glyph::Picture),
    ("Music", "shell:My Music", Glyph::Music),
    ("Videos", "shell:My Video", Glyph::Video),
    ("Network", "ms-settings:network", Glyph::Globe),
];

pub struct AppEntry {
    pub name: String,
    /// Launch / icon target: `shell:AppsFolder\<id>`.
    pub target: String,
}

pub struct RecentFile {
    name: String,
    path: String,
    when: SystemTime,
    /// How long ago, as shown ("5m ago"): worked out when the menu opens.
    ago: String,
}

/// The power menu's entries, in order (a rule is drawn before Restart).
const POWER_ITEMS: [(&str, Power); 5] = [
    ("Lock", Power::Lock),
    ("Sleep", Power::Sleep),
    ("Sign out", Power::SignOut),
    ("Restart", Power::Restart),
    ("Shut down", Power::ShutDown),
];
const POWER_ROW: f32 = 34.0;
const POWER_W: f32 = 156.0;

/// The "go to letter" grid: # then A to Z.
const LETTERS: [char; 27] = [
    '#', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V',
    'W', 'X', 'Y', 'Z',
];

#[derive(Clone, Copy)]
enum Row {
    Letter(char),
    App(usize),
    /// A search answer (see `smart`), by its place in `Launcher::answers`.
    Answer(usize),
}

// ------------------------------------------------------------- app list --

/// Lists every app in the Start menu on a worker thread; posts `msg::APPS`.
pub fn spawn_scan() {
    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let list = scan().unwrap_or_default();
        let ptr = Box::into_raw(Box::new(list));
        if !msg::post(msg::APPS, 0, ptr as isize) {
            drop(Box::from_raw(ptr));
        }
    });
}

unsafe fn take(p: PWSTR) -> String {
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

unsafe fn scan() -> Result<Vec<AppEntry>> {
    let folder: IShellItem = SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)?;
    let items: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems)?;
    let mut out: Vec<AppEntry> = Vec::new();
    loop {
        let mut batch = [None; 1];
        let mut fetched = 0u32;
        if items.Next(&mut batch, Some(&mut fetched)).is_err() || fetched == 0 {
            break;
        }
        let Some(item) = batch[0].take() else { break };
        let (Ok(name), Ok(id)) =
            (item.GetDisplayName(SIGDN_NORMALDISPLAY), item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING))
        else {
            continue;
        };
        let (name, id) = (take(name), take(id));
        let lower = name.to_lowercase();
        let id_lower = id.to_lowercase();
        // Skip uninstallers, readmes and web links that clutter "All apps",
        // and our own settings shortcut (it is built in below).
        if name.is_empty()
            || lower.starts_with("uninstall")
            || lower == "cascade settings"
            || [".txt", ".chm", ".pdf", ".html", ".htm", ".url", ".rtf"].iter().any(|e| id_lower.ends_with(e))
        {
            continue;
        }
        out.push(AppEntry { name, target: format!("shell:AppsFolder\\{id}") });
    }
    out.push(AppEntry { name: "Cascade Settings".into(), target: SETTINGS_TARGET.into() });
    out.sort_by_key(|a| a.name.to_lowercase());
    out.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    Ok(out)
}

/// Reads the recent files on a worker thread; posts `msg::RECENT`. The menu
/// shows the last list it got, so opening it never waits on the folder.
pub fn spawn_recent() {
    std::thread::spawn(|| {
        let ptr = Box::into_raw(Box::new(load_recent(8)));
        if !msg::post(msg::RECENT, 0, ptr as isize) {
            drop(unsafe { Box::from_raw(ptr) });
        }
    });
}

/// "Just now", "5m ago", "3h ago", "2d ago".
fn ago(t: SystemTime) -> String {
    let secs = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..60 => "Just now".to_string(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

/// Recently opened files, newest first (from the shell's Recent folder).
fn load_recent(max: usize) -> Vec<RecentFile> {
    let Some(appdata) = std::env::var_os("APPDATA") else { return Vec::new() };
    let dir = std::path::PathBuf::from(appdata).join("Microsoft\\Windows\\Recent");
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(SystemTime, std::path::PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by_key(|a| std::cmp::Reverse(a.0));
    files
        .into_iter()
        .take(max)
        .map(|(t, p)| {
            let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            RecentFile { name, path: p.to_string_lossy().into_owned(), when: t, ago: ago(t) }
        })
        .collect()
}

// ------------------------------------------------------- launch counts --

/// How often each app was started from the menu, so a search lists the ones
/// you use most first. Kept in `launches.txt` (one `count<TAB>target` a line).
static LAUNCHES: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);
/// A count past this halves them all, so what you used lately outweighs what
/// you used long ago.
const LAUNCH_CAP: u32 = 200;

fn launches_path() -> std::path::PathBuf {
    crate::config::dir().join("launches.txt")
}

fn with_launches<R>(f: impl FnOnce(&mut HashMap<String, u32>) -> R) -> R {
    let mut guard = LAUNCHES.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(|| {
        let text = std::fs::read_to_string(launches_path()).unwrap_or_default();
        text.lines()
            .filter_map(|l| {
                let (n, target) = l.split_once('\t')?;
                Some((target.to_string(), n.parse().ok()?))
            })
            .collect()
    });
    f(map)
}

fn launches_of(target: &str) -> u32 {
    with_launches(|m| m.get(target).copied().unwrap_or(0))
}

fn record_launch(target: &str) {
    let text = with_launches(|m| {
        let n = m.entry(target.to_string()).or_insert(0);
        *n += 1;
        if *n > LAUNCH_CAP {
            for v in m.values_mut() {
                *v /= 2;
            }
            m.retain(|_, v| *v > 0);
        }
        m.iter().map(|(t, n)| format!("{n}\t{t}\n")).collect::<String>()
    });
    let _ = std::fs::create_dir_all(crate::config::dir());
    let _ = std::fs::write(launches_path(), text);
}

/// Lower is better; None = no match.
fn score(name: &str, query: &str) -> Option<u32> {
    let n = name.to_lowercase();
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Some(0);
    }
    if n.starts_with(&q) {
        return Some(0);
    }
    if n.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&q)) {
        return Some(1);
    }
    if n.contains(&q) {
        return Some(2);
    }
    // Letters in order, e.g. "vsc" -> "Visual Studio Code".
    let mut chars = n.chars();
    q.chars().all(|qc| qc == ' ' || chars.any(|c| c == qc)).then_some(3)
}

fn letter_of(name: &str) -> char {
    match name.chars().next() {
        Some(c) if c.is_alphabetic() => c.to_uppercase().next().unwrap_or(c),
        _ => '#',
    }
}

// ------------------------------------------------------------- launcher --

#[derive(Clone, Copy, PartialEq)]
pub enum Target {
    Pinned(usize),
    App(usize),
    Recent(usize),
    Place(usize),
    Profile,
    Settings,
    Power,
    /// A letter heading in All apps (opens the go-to-letter grid).
    Header(char),
    /// A letter in the grid.
    Jump(char),
    /// The grid's backdrop (a click closes it).
    JumpBg,
    PowerItem(usize),
    /// A search answer: a sum, conversion, command or web search.
    Answer(usize),
    /// The power menu's backdrop (a click closes it).
    PowerBg,
    /// The sidebar's expand / collapse button.
    Burger,
    /// The expanded sidebar's backdrop (swallows clicks).
    SideBg,
}

pub enum Action {
    Launch(String),
    /// Put this text on the clipboard (a sum's answer).
    Copy(String),
    Settings,
    Power(Power),
    Close,
}

#[derive(Clone, Copy, PartialEq)]
enum Glyph {
    Gear,
    Folder,
    Download,
    Document,
    Picture,
    Music,
    Video,
    Globe,
    Power,
    Menu,
}

/// Where the menu's parts sit, worked out from the settings.
#[derive(Clone, Copy)]
struct Geo {
    w: f32,
    h: f32,
    /// Sidebar width (0 when hidden).
    side: f32,
    /// Left edge of the middle column.
    mx: f32,
    cols: usize,
    prows: usize,
    /// Left edge of the all-apps column, and its width.
    rx: f32,
    rw: f32,
    /// Whether the middle column (pinned and / or recent) exists at all.
    mid: bool,
    /// The sidebar is a slim rail that expands over the menu.
    compact: bool,
    show_side: bool,
    show_recent: bool,
    show_pinned: bool,
    show_profile: bool,
    show_cascade: bool,
    places: [bool; 8],
    show_all: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Position {
    Auto,
    Center,
    Left,
}

impl Geo {
    fn new(cfg: &Config) -> Self {
        let show_side = cfg.start_show_sidebar;
        let compact = show_side && cfg.start_sidebar_compact;
        let side = match (show_side, compact) {
            (false, _) => 0.0,
            (true, true) => RAIL,
            (true, false) => SIDE,
        };
        let mx = side + 20.0;
        let cols = cfg.start_columns as usize;
        let mid = cfg.start_show_pinned || cfg.start_show_recent;
        let show_all = cfg.start_show_all_apps;
        // With no pinned apps or recent files the middle column goes away and
        // All apps takes its place.
        let (rx, rw) = if mid { (mx + cols as f32 * CELL_W + 16.0, RIGHT_W) } else { (mx, ALONE_W) };
        let w = if show_all { rx + rw + PAD } else { mx + cols as f32 * CELL_W + PAD + 4.0 };
        Geo {
            w,
            h: cfg.start_height,
            side,
            mx,
            cols,
            prows: cfg.start_pinned_rows as usize,
            rx,
            rw,
            mid,
            compact,
            show_side,
            show_recent: cfg.start_show_recent,
            show_pinned: cfg.start_show_pinned,
            show_profile: cfg.start_show_profile,
            show_cascade: cfg.start_show_cascade,
            places: cfg.start_places,
            show_all,
        }
    }
}

pub struct Launcher {
    pub hwnd: HWND,
    geo: Geo,
    position: Position,
    layer: Layer,
    backdrop: Backdrop,
    acrylic: bool,
    /// The Tint slider.
    opacity: f32,
    radius: f32,
    pub open: bool,
    window_visible: bool,
    reveal: Spring,
    apps: Vec<AppEntry>,
    rows: Vec<Row>,
    scanned: Option<Instant>,
    /// Pinned dock apps: (target, name), refreshed on open.
    pinned: Vec<(String, String)>,
    recent: Vec<RecentFile>,
    query: String,
    /// Ctrl+A picked all of `query`: typing replaces it, Backspace clears it.
    all_selected: bool,
    results: Vec<usize>,
    /// What the search answers besides apps: the first `answers_top` go
    /// above the apps, the rest below them.
    answers: Vec<Answer>,
    answers_top: usize,
    /// The web search engine (`start_web_search`).
    web: String,
    selected: usize,
    scroll: f32,
    /// Where the smooth scroll is heading.
    scroll_target: f32,
    /// Fades the scroll bar in while the list moves (1 = just scrolled).
    bar: f32,
    /// Rows slide in when the list opens or the search changes.
    list_anim: Spring,
    /// The expanded sidebar (compact mode).
    side_open: bool,
    side_anim: Spring,
    /// The go-to-letter grid.
    jump_open: bool,
    jump: Spring,
    power_open: bool,
    power_anim: Spring,
    hovered: Option<Target>,
    tracking: bool,
    hits: Vec<(Target, f32, f32, f32, f32)>,
    k: f32,
    exe: String,
    pub dirty: bool,
}

pub fn force_foreground(h: HWND) {
    unsafe {
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        let attach = fg_thread != 0 && fg_thread != me;
        if attach {
            let _ = AttachThreadInput(me, fg_thread, true);
        }
        // An empty mouse event makes this the process that received the
        // last input, which Windows requires before handing over the front.
        let nudge = INPUT { r#type: INPUT_MOUSE, ..Default::default() };
        SendInput(&[nudge], std::mem::size_of::<INPUT>() as i32);
        let _ = SetForegroundWindow(h);
        let _ = BringWindowToTop(h);
        let _ = SetFocus(Some(h));
        if attach {
            let _ = AttachThreadInput(me, fg_thread, false);
        }
    }
}

impl Launcher {
    pub fn new(hwnd: HWND, backdrop: Backdrop) -> Self {
        Self {
            hwnd,
            geo: Geo::new(&Config::default()),
            position: Position::Auto,
            layer: Layer::default(),
            backdrop,
            acrylic: true,
            opacity: 0.55,
            radius: 8.0,
            open: false,
            window_visible: false,
            reveal: Spring::new(0.0, 520.0, 40.0),
            apps: Vec::new(),
            rows: Vec::new(),
            scanned: None,
            pinned: Vec::new(),
            recent: Vec::new(),
            query: String::new(),
            all_selected: false,
            results: Vec::new(),
            answers: Vec::new(),
            answers_top: 0,
            web: "google".into(),
            selected: 0,
            scroll: 0.0,
            scroll_target: 0.0,
            bar: 0.0,
            list_anim: Spring::new(1.0, 170.0, 20.0),
            side_open: false,
            side_anim: Spring::new(0.0, 420.0, 34.0),
            jump_open: false,
            jump: Spring::new(0.0, 380.0, 32.0),
            power_open: false,
            power_anim: Spring::new(0.0, 480.0, 34.0),
            hovered: None,
            tracking: false,
            hits: Vec::new(),
            k: 1.0,
            exe: std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
            dirty: false,
        }
    }

    /// Look and layout, from the settings.
    pub fn configure(&mut self, cfg: &Config) {
        self.acrylic = theme::blurs(cfg.acrylic);
        self.opacity = cfg.start_opacity.clamp(0.0, 1.0);
        self.radius = cfg.corner_radius();
        self.geo = Geo::new(cfg);
        self.web = cfg.start_web_search.clone();
        self.position = match cfg.start_position.as_str() {
            "center" => Position::Center,
            "left" => Position::Left,
            _ => Position::Auto,
        };
        theme::style_backdrop(&self.backdrop, cfg.acrylic, self.opacity, self.radius);
        self.reset_scroll();
        self.close_popups();
        self.dirty = true;
    }

    pub fn set_apps(&mut self, apps: Vec<AppEntry>) {
        self.apps = apps;
        self.scanned = Some(Instant::now());
        self.rows.clear();
        let mut last = None;
        for (i, a) in self.apps.iter().enumerate() {
            let l = letter_of(&a.name);
            if last != Some(l) {
                self.rows.push(Row::Letter(l));
                last = Some(l);
            }
            self.rows.push(Row::App(i));
        }
        self.filter();
        self.dirty = true;
    }

    /// The app behind a target, for pinning (None for files and built-ins).
    pub fn app_target(&self, t: Target) -> Option<String> {
        let s = match t {
            Target::Pinned(i) => self.pinned.get(i).map(|p| p.0.clone()),
            Target::App(i) => self.apps.get(i).map(|a| a.target.clone()),
            _ => None,
        }?;
        (s != SETTINGS_TARGET).then_some(s)
    }

    fn icon_id<'a>(&'a self, target: &'a str) -> &'a str {
        if target == SETTINGS_TARGET {
            &self.exe
        } else {
            target
        }
    }

    pub fn show(&mut self, k: f32, pinned: Vec<(String, String)>) {
        self.k = k;
        self.pinned = pinned;
        for r in &mut self.recent {
            r.ago = ago(r.when);
        }
        if self.recent.is_empty() && self.geo.show_recent {
            // Not read yet (just turned on): they arrive in a moment.
            spawn_recent();
        }
        self.query.clear();
        self.all_selected = false;
        self.filter();
        self.reset_scroll();
        self.close_popups();
        self.side_anim.snap(0.0);
        self.list_anim.snap(0.0);
        self.list_anim.set(1.0);
        self.hovered = None;
        self.open = true;
        // Pick up newly installed apps now and then.
        if self.scanned.is_none_or(|t| t.elapsed().as_secs() > 300) {
            spawn_scan();
        }
        if !self.window_visible {
            self.window_visible = true;
            self.reveal.snap(0.0);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOW);
            }
        }
        force_foreground(self.hwnd);
        if unsafe { GetForegroundWindow() } != self.hwnd {
            // Windows refused (an admin app is in front): the admin helper,
            // if it's on, is allowed to do it.
            crate::helper::request_focus(self.hwnd);
        }
        self.reveal.set(1.0);
        self.dirty = true;
    }

    /// The recent files, read in the background (see `spawn_recent`).
    pub fn set_recent(&mut self, recent: Vec<RecentFile>) {
        self.recent = recent;
        self.dirty = true;
    }

    pub fn hide(&mut self) {
        crate::mousehook::set_rect(0, None);
        self.close_popups();
        if self.open {
            self.open = false;
            // Read them again for next time, now the menu is out of the way.
            if self.geo.show_recent {
                spawn_recent();
            }
            self.reveal.set(0.0);
            self.backdrop.hide();
            self.dirty = true;
        }
    }

    fn filter(&mut self) {
        let searching = self.searching();
        // Names that start a word count as one kind of match; within a kind
        // the apps you open most come first, then the closer matches.
        let mut scored: Vec<(u32, std::cmp::Reverse<u32>, u32, usize)> = self
            .apps
            .iter()
            .enumerate()
            .filter_map(|(i, a)| {
                let s = score(&a.name, &self.query)?;
                let used = if searching { launches_of(&a.target) } else { 0 };
                Some((s.saturating_sub(1), std::cmp::Reverse(used), s, i))
            })
            .collect();
        scored.sort();
        self.results = scored.into_iter().map(|(.., i)| i).collect();
        let (top, bottom) = if self.searching() { smart::answers(&self.query, &self.web) } else { Default::default() };
        self.answers_top = top.len();
        self.answers = top.into_iter().chain(bottom).collect();
        self.selected = 0;
        self.reset_scroll();
        // Results ripple in again.
        self.list_anim.snap(0.0);
        self.list_anim.set(1.0);
    }

    fn reset_scroll(&mut self) {
        self.scroll = 0.0;
        self.scroll_target = 0.0;
    }

    /// Closes the side panel, the letter grid and the power menu.
    fn close_popups(&mut self) {
        self.side_open = false;
        self.jump_open = false;
        self.power_open = false;
        self.side_anim.set(0.0);
        self.jump.set(0.0);
        self.power_anim.set(0.0);
    }

    fn open_jump(&mut self, on: bool) {
        self.jump_open = on;
        self.jump.set(if on { 1.0 } else { 0.0 });
        self.dirty = true;
    }

    fn open_power(&mut self, on: bool) {
        self.power_open = on;
        self.power_anim.set(if on { 1.0 } else { 0.0 });
        self.dirty = true;
    }

    fn open_side(&mut self, on: bool) {
        self.side_open = on;
        self.side_anim.set(if on { 1.0 } else { 0.0 });
        self.dirty = true;
    }

    /// Scrolls the list so a letter's heading is at the top.
    fn jump_to(&mut self, letter: char) {
        if let Some(i) = self.rows.iter().position(|r| matches!(r, Row::Letter(l) if *l == letter)) {
            self.set_scroll(i as f32 * LIST_ROW);
        }
    }

    /// Smoothly scrolls to `y` (clamped to the list).
    fn set_scroll(&mut self, y: f32) {
        let (top, bottom) = self.list_area();
        let rows =
            if self.searching() { self.result_rows() as f32 * RESULT_ROW } else { self.rows.len() as f32 * LIST_ROW };
        let max = (rows - (bottom - top)).max(0.0);
        self.scroll_target = y.clamp(0.0, max);
        self.bar = 1.0;
        self.dirty = true;
    }

    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// Rows while searching: the answers above the apps, the apps, the rest.
    fn result_rows(&self) -> usize {
        self.answers.len() + self.results.len()
    }

    fn result_at(&self, r: usize) -> Option<Row> {
        let top = self.answers_top;
        if r < top {
            return Some(Row::Answer(r));
        }
        match self.results.get(r - top) {
            Some(&i) => Some(Row::App(i)),
            None => (r - self.results.len() < self.answers.len()).then_some(Row::Answer(r - self.results.len())),
        }
    }

    /// What picking answer `j` does.
    fn answer_action(&self, j: usize) -> Option<Action> {
        Some(match &self.answers.get(j)?.run {
            smart::Run::Copy(text) => Action::Copy(text.clone()),
            smart::Run::Open(target) => Action::Launch(target.clone()),
            smart::Run::Power(p) => Action::Power(match *p {
                "lock" => Power::Lock,
                "sleep" => Power::Sleep,
                "signout" => Power::SignOut,
                "restart" => Power::Restart,
                _ => Power::ShutDown,
            }),
        })
    }

    fn launch(&self, target: String) -> Action {
        if target == SETTINGS_TARGET {
            Action::Settings
        } else {
            record_launch(&target);
            Action::Launch(target)
        }
    }

    // -------------------------------------------------------------- input --

    pub fn on_char(&mut self, c: char) {
        match c {
            // Ctrl+A, Ctrl+C, Ctrl+X: the search box is a plain text box too.
            '\u{1}' if !self.query.is_empty() => {
                self.all_selected = true;
                self.dirty = true;
            }
            '\u{3}' | '\u{18}' if !self.query.is_empty() => {
                crate::clipboard::copy(&self.query);
                if c == '\u{18}' {
                    self.query.clear();
                    self.all_selected = false;
                    self.filter();
                    self.dirty = true;
                }
            }
            c if !c.is_control() => {
                self.close_popups();
                if self.all_selected {
                    self.query.clear();
                    self.all_selected = false;
                }
                self.query.push(c);
                self.filter();
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// Returns an action for keys that do something.
    pub fn on_key(&mut self, vk: u16) -> Option<Action> {
        const BACK: u16 = 0x08;
        const DELETE: u16 = 0x2E;
        const ENTER: u16 = 0x0D;
        const ESC: u16 = 0x1B;
        const UP: u16 = 0x26;
        const DOWN: u16 = 0x28;
        match vk {
            ESC if self.power_open => self.open_power(false),
            ESC if self.jump_open => self.open_jump(false),
            ESC if self.side_open => self.open_side(false),
            ESC if self.searching() => {
                self.query.clear();
                self.all_selected = false;
                self.filter();
            }
            ESC => return Some(Action::Close),
            BACK | DELETE => {
                if self.all_selected {
                    self.query.clear();
                    self.all_selected = false;
                } else if vk == BACK {
                    self.query.pop();
                }
                self.filter();
            }
            ENTER => {
                return match self.result_at(self.selected).filter(|_| self.searching()) {
                    Some(Row::App(i)) => Some(self.launch(self.apps[i].target.clone())),
                    Some(Row::Answer(j)) => self.answer_action(j),
                    _ => None,
                };
            }
            UP => self.selected = self.selected.saturating_sub(1),
            DOWN => self.selected = (self.selected + 1).min(self.result_rows().saturating_sub(1)),
            _ => return None,
        }
        self.keep_selected_visible();
        self.dirty = true;
        None
    }

    fn keep_selected_visible(&mut self) {
        if !self.searching() {
            return;
        }
        let (top, bottom) = self.list_area();
        let y = self.selected as f32 * RESULT_ROW;
        let view = bottom - top;
        if y < self.scroll_target {
            self.set_scroll(y);
        } else if y + RESULT_ROW > self.scroll_target + view {
            self.set_scroll(y + RESULT_ROW - view);
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<Target> {
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        self.hits
            .iter()
            .find(|(_, hx, hy, hw, hh)| lx >= *hx && lx < hx + hw && ly >= *hy && ly < hy + hh)
            .map(|(t, ..)| *t)
    }

    pub fn on_move(&mut self, x: i32, y: i32) {
        unsafe {
            if !self.tracking {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                self.tracking = TrackMouseEvent(&mut t).is_ok();
            }
        }
        // The expanded sidebar folds away when the pointer leaves it.
        if self.side_open && x as f32 / self.k > SIDE + 4.0 {
            self.open_side(false);
        }
        let h = self.hit(x, y);
        if h != self.hovered {
            self.hovered = h;
            self.dirty = true;
        }
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        self.hovered = None;
        if self.side_open {
            self.open_side(false);
        }
        self.dirty = true;
    }

    pub fn target_at(&self, x: i32, y: i32) -> Option<Target> {
        self.hit(x, y)
    }

    pub fn on_click(&mut self, x: i32, y: i32) -> Option<Action> {
        let t = self.hit(x, y)?;
        // Anything but the controls of an open popup closes it.
        if self.jump_open && !matches!(t, Target::Jump(_) | Target::JumpBg | Target::Header(_)) {
            self.open_jump(false);
        }
        if !matches!(t, Target::PowerItem(_) | Target::Power) && self.power_open {
            self.open_power(false);
        }
        if matches!(t, Target::Place(_) | Target::Settings | Target::Profile) {
            self.open_side(false);
        }
        match t {
            Target::Header(_) => {
                self.open_jump(!self.jump_open);
                None
            }
            Target::Jump(l) => {
                self.jump_to(l);
                self.open_jump(false);
                None
            }
            Target::JumpBg | Target::PowerBg | Target::SideBg => None,
            Target::Burger => {
                self.open_side(!self.side_open);
                None
            }
            Target::PowerItem(i) => {
                self.open_power(false);
                Some(Action::Power(POWER_ITEMS[i].1))
            }
            Target::Power => {
                self.open_power(!self.power_open);
                None
            }
            Target::Pinned(i) => self.pinned.get(i).map(|p| self.launch(p.0.clone())),
            Target::App(i) => self.apps.get(i).map(|a| self.launch(a.target.clone())),
            Target::Answer(j) => self.answer_action(j),
            Target::Recent(i) => self.recent.get(i).map(|r| Action::Launch(r.path.clone())),
            Target::Place(i) => Some(Action::Launch(PLACES[i].1.to_string())),
            Target::Profile => Some(Action::Launch("ms-settings:yourinfo".into())),
            Target::Settings => Some(Action::Settings),
        }
    }

    pub fn on_wheel(&mut self, delta: i16) {
        if self.jump_open || self.power_open {
            return;
        }
        self.set_scroll(self.scroll_target - delta as f32 / 120.0 * LIST_ROW * 3.0);
    }

    // ------------------------------------------------------------- frames --

    /// Top and bottom of the scrolling list, in layout units.
    fn list_area(&self) -> (f32, f32) {
        (TOP + 30.0, self.geo.h - PAD)
    }

    pub fn wants_frame(&self) -> bool {
        self.window_visible
            && (self.dirty
                || !self.reveal.settled()
                || !self.side_anim.settled()
                || !self.jump.settled()
                || !self.power_anim.settled()
                || !self.list_anim.settled()
                || self.scroll != self.scroll_target
                || self.bar > 0.0)
    }

    pub fn step(&mut self, dt: f32) -> bool {
        let mut moving = self.reveal.step(dt);
        for s in [&mut self.side_anim, &mut self.jump, &mut self.power_anim, &mut self.list_anim] {
            moving |= s.step(dt);
        }
        // Scrolling eases toward its target instead of jumping.
        let d = self.scroll_target - self.scroll;
        if d.abs() > 0.4 {
            self.scroll += d * (1.0 - (-dt * 16.0).exp());
            moving = true;
        } else if d != 0.0 {
            self.scroll = self.scroll_target;
            moving = true;
        }
        if self.bar > 0.0 {
            self.bar = (self.bar - dt * 0.8).max(0.0);
            moving = true;
        }
        moving
    }

    /// Draws the launcher above `bottom` (screen px) on `monitor`: centred,
    /// or with its left edge at `anchor` when the taskbar is left-aligned.
    pub fn render(
        &mut self,
        gfx: &mut Gfx,
        dock: &mut Dock,
        monitor: RECT,
        bottom: i32,
        anchor: Option<i32>,
    ) -> Result<()> {
        self.dirty = false;
        let g = self.geo;
        if !self.window_visible {
            return Ok(());
        }
        if !self.open && self.reveal.value < 0.01 {
            self.window_visible = false;
            self.layer.release();
            self.backdrop.hide();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return Ok(());
        }
        let k = self.k;
        let (pw, ph) = ((g.w * k).ceil() as i32, (g.h * k).ceil() as i32);
        let slide = ((1.0 - self.reveal.value.clamp(0.0, 1.0)) * 28.0 * k) as i32;
        let centred = monitor.left + (monitor.right - monitor.left - pw) / 2;
        let x = match (self.position, anchor) {
            (Position::Center, _) | (Position::Auto, None) => centred,
            (Position::Left, _) => monitor.left + (12.0 * k) as i32,
            (Position::Auto, Some(ax)) => ax.clamp(monitor.left, monitor.right - pw),
        };
        let x = x.clamp(monitor.left, (monitor.right - pw).max(monitor.left));
        let y = (bottom - ph - (12.0 * k) as i32).max(monitor.top);
        let pos = POINT { x, y: y + slide };
        if self.open {
            crate::mousehook::set_rect(0, Some(RECT { left: x, top: y, right: x + pw, bottom: y + ph }));
        }

        // Glass while it slides in and out too, not only once it has landed.
        let glass = self.acrylic && self.reveal.value > theme::GLASS_FROM;
        let screen = RECT { left: pos.x, top: pos.y, right: pos.x + pw, bottom: pos.y + ph };

        // Icons for what is on screen (loaded lazily through the dock's cache).
        let searching = self.searching();
        let (top, list_bottom) = self.list_area();
        let row_h = if searching { RESULT_ROW } else { LIST_ROW };
        let first = (self.scroll / row_h) as usize;
        let count = ((list_bottom - top) / row_h) as usize + 2;
        let visible: Vec<(usize, Row)> = if searching {
            (first..self.result_rows()).take(count).filter_map(|r| self.result_at(r).map(|row| (r, row))).collect()
        } else if g.show_all {
            self.rows.iter().copied().enumerate().skip(first).take(count).collect()
        } else {
            Vec::new()
        };
        // Icons are fetched at exactly the pixel size they're drawn at.
        let px = |s: f32| (s * k).round() as u32;
        let row_px = px(if searching { 30.0 } else { 22.0 });
        let mut row_icons: Vec<Option<ID2D1Bitmap>> = Vec::with_capacity(visible.len());
        for &(_, row) in &visible {
            row_icons.push(match row {
                Row::App(i) => dock.icon(self.icon_id(&self.apps[i].target), gfx, row_px),
                Row::Letter(_) | Row::Answer(_) => None,
            });
        }
        let (pinned_icons, recent_icons): (Vec<_>, Vec<_>) = if searching {
            (Vec::new(), Vec::new())
        } else {
            (
                self.pinned
                    .iter()
                    .take(if g.show_pinned { g.cols * g.prows } else { 0 })
                    .map(|(t, _)| dock.icon(self.icon_id(t), gfx, px(32.0)))
                    .collect(),
                if g.show_recent {
                    self.recent.iter().map(|r| dock.icon(&r.path, gfx, px(28.0))).collect()
                } else {
                    Vec::new()
                },
            )
        };

        self.hits.clear();
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            let r = self.radius;
            // Over the blur only the theme's wash (it also keeps the pixels
            // clickable); without blur a solid panel.
            theme::panel(&p, 0.0, 0.0, g.w, g.h, r, glass, self.opacity, 0.97, 1.0);
            if g.show_side {
                p.clip(1.0, 1.0, g.side - 1.0, g.h - 2.0);
                p.rrect(0.0, 0.0, g.side + r * 2.0, g.h, r, Color(0.0, 0.0, 0.0, 0.16));
                p.unclip();
            }

            // Search box across the two right-hand columns.
            let (sx, sw, sh) = (g.mx, g.w - PAD - g.mx, 38.0);
            p.rrect(sx, 18.0, sw, sh, sh / 2.0, Color::white(0.07));
            p.stroke_rrect(sx + 0.5, 18.5, sw - 1.0, sh - 1.0, sh / 2.0, Color::white(0.08), 1.0);
            p.stroke_circle(sx + 22.0, 36.0, 5.5, Color::white(0.65), 1.5);
            p.line(sx + 26.0, 40.0, sx + 30.0, 44.0, Color::white(0.65), 1.5);
            if self.query.is_empty() {
                p.text("Search for apps", &f.body, sx + 40.0, 18.0, sw - 60.0, sh, Align::Left, Color::white(0.45));
                p.line(sx + 39.0, 28.0, sx + 39.0, 46.0, Color::white(0.8), 1.2);
            } else {
                let tw = gfx.measure(&self.query, &f.body).min(sw - 62.0);
                if self.all_selected {
                    p.rrect(sx + 38.0, 26.0, tw + 4.0, 22.0, 4.0, theme::accent().alpha(0.45));
                }
                p.text(&self.query, &f.body, sx + 40.0, 18.0, sw - 60.0, sh, Align::Left, Color::white(0.95));
                if !self.all_selected {
                    p.line(sx + 40.0 + tw + 1.0, 28.0, sx + 40.0 + tw + 1.0, 46.0, Color::white(0.8), 1.2);
                }
            }

            // Rows ease in one after another when the list opens or changes.
            let ripple = self.list_anim.value;
            let ease = |n: usize| {
                let t = (ripple * 1.9 - n.min(10) as f32 * 0.09).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            };
            // Scroll bar, visible only while the list is moving.
            let bar_alpha = (self.bar * 2.0).min(1.0) * 0.3;
            let draw_bar = |total: f32, x: f32| {
                let view = list_bottom - top;
                if bar_alpha > 0.01 && total > view {
                    let bh = (view * view / total).max(24.0);
                    let by = top + self.scroll / (total - view) * (view - bh);
                    p.rrect(x, by, 3.0, bh, 1.5, Color::white(bar_alpha));
                }
            };

            if searching {
                p.text("Best matches", &f.label, g.mx + 4.0, TOP, 300.0, 22.0, Align::Left, Color::white(0.9));
                p.clip(g.mx - 6.0, top, g.w - PAD - g.mx + 6.0, list_bottom - top);
                for (vi, (&(row, item), icon)) in visible.iter().zip(&row_icons).enumerate() {
                    let e = ease(vi);
                    let y = top + row as f32 * RESULT_ROW - self.scroll + (1.0 - e) * 14.0;
                    let i = match item {
                        Row::App(i) => i,
                        Row::Answer(j) => {
                            // A sum, conversion, command or web search.
                            let Some(a) = self.answers.get(j) else { continue };
                            let t = Target::Answer(j);
                            let selected = row == self.selected;
                            let (x, w) = (g.mx - 6.0, g.w - PAD - g.mx + 6.0);
                            if self.hovered == Some(t) || selected {
                                let fill = if selected { 0.10 } else { 0.06 };
                                p.rrect(x, y + 2.0, w, RESULT_ROW - 4.0, 6.0, Color::white(fill * e));
                            }
                            if selected {
                                p.rrect(x, y + 14.0, 3.0, RESULT_ROW - 28.0, 1.5, theme::accent().alpha(e));
                            }
                            answer_glyph(&p, a.kind, g.mx + 23.0, y + RESULT_ROW / 2.0, Color::white(0.85 * e));
                            let font = if a.kind == smart::Kind::Sum || a.kind == smart::Kind::Unit {
                                &f.title
                            } else {
                                &f.body
                            };
                            let tw = g.w - g.mx - 90.0 - 120.0;
                            p.text(&a.title, font, g.mx + 50.0, y, tw, RESULT_ROW, Align::Left, Color::white(0.95 * e));
                            let dx = g.mx + 50.0 + tw;
                            let c = Color::white(0.5 * e);
                            p.text(&a.detail, &f.small, dx, y, g.w - PAD - dx - 6.0, RESULT_ROW, Align::Right, c);
                            let (vy0, vy1) = (y.max(top), (y + RESULT_ROW).min(list_bottom));
                            if vy1 > vy0 {
                                self.hits.push((t, x, vy0, w, vy1 - vy0));
                            }
                            continue;
                        }
                        Row::Letter(_) => continue,
                    };
                    let t = Target::App(i);
                    let selected = row == self.selected;
                    if self.hovered == Some(t) || selected {
                        let a = if selected { 0.10 } else { 0.06 };
                        p.rrect(
                            g.mx - 6.0,
                            y + 2.0,
                            g.w - PAD - g.mx + 6.0,
                            RESULT_ROW - 4.0,
                            6.0,
                            Color::white(a * e),
                        );
                    }
                    if selected {
                        p.rrect(g.mx - 6.0, y + 14.0, 3.0, RESULT_ROW - 28.0, 1.5, theme::accent().alpha(e));
                    }
                    draw_icon(&p, icon.as_ref(), g.mx + 8.0, y + (RESULT_ROW - 30.0) / 2.0, 30.0, e);
                    let name = &self.apps[i].name;
                    p.text(
                        name,
                        &f.body,
                        g.mx + 50.0,
                        y,
                        g.w - g.mx - 90.0,
                        RESULT_ROW,
                        Align::Left,
                        Color::white(0.95 * e),
                    );
                    let (vy0, vy1) = (y.max(top), (y + RESULT_ROW).min(list_bottom));
                    if vy1 > vy0 {
                        self.hits.push((t, g.mx - 6.0, vy0, g.w - PAD - g.mx + 6.0, vy1 - vy0));
                    }
                }
                if self.result_rows() == 0 {
                    p.text(
                        "No apps found",
                        &f.body,
                        g.mx,
                        top + 10.0,
                        g.w - g.mx - PAD,
                        30.0,
                        Align::Left,
                        Color::white(0.5 * ease(0)),
                    );
                }
                p.unclip();
                draw_bar(self.result_rows() as f32 * RESULT_ROW, g.w - 8.0);
            } else {
                // Pinned.
                let grid_y = TOP + 28.0;
                if g.show_pinned {
                    p.text("Pinned", &f.label, g.mx + 4.0, TOP, 200.0, 22.0, Align::Left, Color::white(0.9));
                    for (i, ((_, name), icon)) in self.pinned.iter().zip(&pinned_icons).enumerate() {
                        let (col, row) = ((i % g.cols) as f32, (i / g.cols) as f32);
                        let (x, y) = (g.mx + col * CELL_W, grid_y + row * CELL_H);
                        let t = Target::Pinned(i);
                        if self.hovered == Some(t) {
                            p.rrect(x + 2.0, y, CELL_W - 4.0, CELL_H - 4.0, 6.0, Color::white(0.07));
                        }
                        draw_icon(&p, icon.as_ref(), x + CELL_W / 2.0 - 16.0, y + 12.0, 32.0, 1.0);
                        p.text(
                            name,
                            &f.small,
                            x + 4.0,
                            y + 52.0,
                            CELL_W - 8.0,
                            18.0,
                            Align::Center,
                            Color::white(0.88),
                        );
                        self.hits.push((t, x + 2.0, y, CELL_W - 4.0, CELL_H - 4.0));
                    }
                    if self.pinned.is_empty() {
                        let hint = "Right-click an app to pin it";
                        p.text(hint, &f.small, g.mx + 4.0, grid_y, 300.0, 20.0, Align::Left, Color::white(0.5));
                    }
                }

                // Recent files.
                let ry = if g.show_pinned {
                    let rows = self.pinned.len().div_ceil(g.cols).clamp(1, g.prows) as f32;
                    grid_y + rows * CELL_H + 10.0
                } else {
                    TOP
                };
                if g.show_recent {
                    p.text("Recent", &f.label, g.mx + 4.0, ry, 200.0, 22.0, Align::Left, Color::white(0.9));
                    // Keep clear of the bottom-left buttons when there's no sidebar.
                    let reserved = if g.show_side { 0.0 } else { 40.0 };
                    let fits = ((g.h - PAD - ry - 28.0 - reserved) / RECENT_ROW).max(0.0) as usize;
                    for (i, (file, icon)) in self.recent.iter().zip(&recent_icons).take(fits).enumerate() {
                        let y = ry + 28.0 + i as f32 * RECENT_ROW;
                        let t = Target::Recent(i);
                        let w = g.cols as f32 * CELL_W;
                        if self.hovered == Some(t) {
                            p.rrect(g.mx, y, w, RECENT_ROW - 4.0, 6.0, Color::white(0.07));
                        }
                        draw_icon(&p, icon.as_ref(), g.mx + 8.0, y + 7.0, 28.0, 1.0);
                        p.text(
                            &file.name,
                            &f.body,
                            g.mx + 46.0,
                            y + 3.0,
                            w - 56.0,
                            20.0,
                            Align::Left,
                            Color::white(0.92),
                        );
                        p.text(
                            &file.ago,
                            &f.small,
                            g.mx + 46.0,
                            y + 21.0,
                            w - 56.0,
                            16.0,
                            Align::Left,
                            Color::white(0.5),
                        );
                        self.hits.push((t, g.mx, y, w, RECENT_ROW - 4.0));
                    }
                    if self.recent.is_empty() {
                        let hint = "Files you open show up here";
                        p.text(hint, &f.small, g.mx + 4.0, ry + 28.0, 300.0, 20.0, Align::Left, Color::white(0.5));
                    }
                }

                if g.show_all {
                    // All apps, A to Z.
                    let rw = g.rw;
                    let mark = self.hits.len();
                    if g.mid {
                        p.line(g.rx - 8.0, TOP, g.rx - 8.0, g.h - PAD, Color::white(0.06), 1.0);
                    }
                    p.text("All apps", &f.label, g.rx + 4.0, TOP, rw, 22.0, Align::Left, Color::white(0.9));
                    p.clip(g.rx - 4.0, top, rw + 4.0, list_bottom - top);
                    for (vi, (&(row, item), icon)) in visible.iter().zip(&row_icons).enumerate() {
                        let e = ease(vi);
                        let y = top + row as f32 * LIST_ROW - self.scroll + (1.0 - e) * 12.0;
                        match item {
                            Row::Letter(l) => {
                                let s = l.to_string();
                                if self.hovered == Some(Target::Header(l)) && !self.jump_open {
                                    p.rrect(g.rx - 4.0, y + 1.0, rw + 4.0, LIST_ROW - 2.0, 5.0, Color::white(0.05 * e));
                                }
                                p.text(
                                    &s,
                                    &f.label,
                                    g.rx + 8.0,
                                    y + 6.0,
                                    rw,
                                    LIST_ROW - 6.0,
                                    Align::Left,
                                    theme::accent().alpha(e),
                                );
                                let (vy0, vy1) = (y.max(top), (y + LIST_ROW).min(list_bottom));
                                if vy1 > vy0 {
                                    self.hits.push((Target::Header(l), g.rx - 4.0, vy0, rw + 4.0, vy1 - vy0));
                                }
                            }
                            Row::Answer(_) => {}
                            Row::App(i) => {
                                let t = Target::App(i);
                                if self.hovered == Some(t) {
                                    p.rrect(g.rx - 4.0, y + 1.0, rw + 4.0, LIST_ROW - 2.0, 5.0, Color::white(0.07 * e));
                                }
                                draw_icon(&p, icon.as_ref(), g.rx + 6.0, y + (LIST_ROW - 22.0) / 2.0, 22.0, e);
                                let name = &self.apps[i].name;
                                p.text(
                                    name,
                                    &f.body,
                                    g.rx + 36.0,
                                    y,
                                    rw - 40.0,
                                    LIST_ROW,
                                    Align::Left,
                                    Color::white(0.92 * e),
                                );
                                let (vy0, vy1) = (y.max(top), (y + LIST_ROW).min(list_bottom));
                                if vy1 > vy0 {
                                    self.hits.push((t, g.rx - 4.0, vy0, rw + 4.0, vy1 - vy0));
                                }
                            }
                        }
                    }
                    if self.apps.is_empty() {
                        p.text("Loading\u{2026}", &f.body, g.rx + 8.0, top, rw, 30.0, Align::Left, Color::white(0.5));
                    }
                    p.unclip();
                    draw_bar(self.rows.len() as f32 * LIST_ROW, g.w - 8.0);

                    // The go-to-letter grid, over the list.
                    let ja = self.jump.value.clamp(0.0, 1.0);
                    if ja > 0.01 {
                        self.hits.truncate(mark);
                        let (ox, oy, ow, oh) = (g.rx - 6.0, top - 4.0, rw + 10.0, list_bottom - top + 4.0);
                        // Same flat panel as the menu, fading in; no outline.
                        p.rrect(ox, oy, ow, oh, 8.0, Color(0.14, 0.14, 0.15, 0.97 * ja));
                        let slide = (1.0 - ja) * 6.0;
                        let pad = 10.0;
                        p.text(
                            "Jump to",
                            &f.label,
                            ox + pad + 2.0,
                            oy + pad + slide,
                            ow,
                            20.0,
                            Align::Left,
                            Color::white(0.55 * ja),
                        );
                        let (gx, gy) = (ox + pad, oy + pad + 28.0 + slide);
                        let (cw, ch) = ((ow - pad * 2.0) / 4.0, ((oh - pad * 2.0 - 28.0) / 7.0).min(56.0));
                        let mut cells = Vec::new();
                        // A big typographic index: no tiles, just the letters.
                        for (n, &l) in LETTERS.iter().enumerate() {
                            let has = self.rows.iter().any(|r| matches!(r, Row::Letter(x) if *x == l));
                            let (cx, cy) = (gx + (n % 4) as f32 * cw, gy + (n / 4) as f32 * ch);
                            let hot = has && self.hovered == Some(Target::Jump(l));
                            if hot {
                                p.rrect(cx + 3.0, cy + 3.0, cw - 6.0, ch - 6.0, 10.0, Color::white(0.06 * ja));
                            }
                            let col = match (has, hot) {
                                (true, true) => theme::accent().alpha(ja),
                                (true, false) => Color::white(0.92 * ja),
                                (false, _) => Color::white(0.13 * ja),
                            };
                            p.text(&l.to_string(), &f.big, cx, cy, cw, ch, Align::Center, col);
                            if has && self.jump_open {
                                cells.push((Target::Jump(l), cx + 3.0, cy + 3.0, cw - 6.0, ch - 6.0));
                            }
                        }
                        self.hits.extend(cells);
                        if self.jump_open {
                            self.hits.push((Target::JumpBg, ox, oy, ow, oh));
                        }
                    }
                }
            }

            // Sidebar, drawn last so it can open over the menu. Its hit areas
            // go first in the list so they win over what's underneath.
            let mut side_hits: Vec<(Target, f32, f32, f32, f32)> = Vec::new();
            let mut power_pos = (g.mx, g.h - PAD - 36.0);
            if g.show_side {
                let ea = if g.compact { self.side_anim.value.clamp(0.0, 1.0) } else { 1.0 };
                let sw = if g.compact { RAIL + (SIDE - RAIL) * ea } else { SIDE };
                // Labels fade in once the panel is mostly open.
                let la = if g.compact { ((ea - 0.4) / 0.6).clamp(0.0, 1.0) } else { 1.0 };
                let iw = sw - 20.0;
                let ih = SIDE_ROW - 4.0;
                if g.compact && ea > 0.01 {
                    p.clip(0.0, 0.0, sw, g.h);
                    p.rrect(0.0, 0.0, sw + r * 2.0, g.h, r, Color(0.085, 0.085, 0.095, 0.97 * (ea * 2.0).min(1.0)));
                    p.unclip();
                    p.line(sw - 0.5, 14.0, sw - 0.5, g.h - 14.0, Color::white(0.06 * ea), 1.0);
                }
                p.clip(0.0, 0.0, sw, g.h);
                let user = std::env::var("USERNAME").unwrap_or_default();
                let mut py = 14.0;
                if g.compact {
                    py = 12.0;
                    if self.hovered == Some(Target::Burger) {
                        p.rrect(10.0, py, iw, ih, 5.0, Color::white(0.07));
                    }
                    draw_glyph(&p, Glyph::Menu, 26.0, py + ih / 2.0, Color::white(0.88));
                    side_hits.push((Target::Burger, 10.0, py, iw, ih));
                    py += SIDE_ROW;
                }
                let mut top = py;
                if g.show_profile {
                    let ph = if g.compact { 40.0 } else { 44.0 };
                    if self.hovered == Some(Target::Profile) {
                        p.rrect(10.0, py, iw, ph, 6.0, Color::white(0.07));
                    }
                    p.circle(26.0, py + ph / 2.0, 14.0, theme::accent());
                    let initial: String = user.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
                    p.text(
                        &initial,
                        &f.label,
                        12.0,
                        py + ph / 2.0 - 15.0,
                        28.0,
                        30.0,
                        Align::Center,
                        Color::white(1.0),
                    );
                    if la > 0.02 {
                        p.text(&user, &f.title, 48.0, py, sw - 60.0, ph, Align::Left, Color::white(0.95 * la));
                    }
                    side_hits.push((Target::Profile, 10.0, py, iw, ph));
                    top = if g.compact { py + ph + 6.0 } else { TOP };
                }

                let side_item = |hits: &mut Vec<_>, t: Target, label: &str, glyph: Glyph, y: f32| {
                    if self.hovered == Some(t) {
                        p.rrect(10.0, y, iw, ih, 5.0, Color::white(0.07));
                    }
                    draw_glyph(&p, glyph, 26.0, y + ih / 2.0, Color::white(0.88));
                    if la > 0.02 {
                        p.text(label, &f.body, 48.0, y, sw - 58.0, ih, Align::Left, Color::white(0.9 * la));
                    }
                    hits.push((t, 10.0, y, iw, ih));
                };
                let shown = PLACES.iter().enumerate().filter(|(i, _)| g.places[*i]);
                for (slot, (i, (label, _, glyph))) in shown.enumerate() {
                    side_item(&mut side_hits, Target::Place(i), label, *glyph, top + slot as f32 * SIDE_ROW);
                }
                let rule_y = if g.show_cascade { g.h - PAD - SIDE_ROW * 2.0 - 6.0 } else { g.h - PAD - SIDE_ROW - 6.0 };
                p.line(18.0, rule_y, sw - 18.0, rule_y, Color::white(0.08), 1.0);
                if g.show_cascade {
                    side_item(&mut side_hits, Target::Settings, "Cascade", Glyph::Gear, g.h - PAD - SIDE_ROW * 2.0);
                }
                side_item(&mut side_hits, Target::Power, "Power", Glyph::Power, g.h - PAD - SIDE_ROW);
                p.unclip();
                if g.compact && ea > 0.01 {
                    side_hits.push((Target::SideBg, 0.0, 0.0, sw, g.h));
                }
                power_pos = (10.0, g.h - PAD - SIDE_ROW - 4.0);
            } else {
                // No sidebar: just Cascade settings and power, bottom left.
                let buttons = [(Target::Settings, Glyph::Gear), (Target::Power, Glyph::Power)];
                let shown = buttons.iter().filter(|(t, _)| g.show_cascade || *t != Target::Settings);
                for (n, &(t, glyph)) in shown.enumerate() {
                    let cx = g.mx + 14.0 + n as f32 * 40.0;
                    let cy = g.h - PAD - 16.0;
                    if self.hovered == Some(t) {
                        p.circle(cx, cy, 16.0, Color::white(0.10));
                    }
                    draw_glyph(&p, glyph, cx, cy, Color::white(0.88));
                    side_hits.push((t, cx - 16.0, cy - 16.0, 32.0, 32.0));
                }
            }
            self.hits.splice(0..0, side_hits);

            // The power menu, drawn by Cascade instead of a Windows popup.
            let pa = self.power_anim.value.clamp(0.0, 1.0);
            if pa > 0.01 {
                let mut pop: Vec<(Target, f32, f32, f32, f32)> = Vec::new();
                let h = POWER_ITEMS.len() as f32 * POWER_ROW + 8.0 + 12.0;
                let (x0, y1) = power_pos;
                let y0 = y1 - h + (1.0 - pa) * 10.0;
                p.rrect(x0 - 1.0, y0 - 1.0, POWER_W + 2.0, h + 2.0, 11.0, Color(0.0, 0.0, 0.0, 0.25 * pa));
                p.rrect(x0, y0, POWER_W, h, 10.0, Color(0.13, 0.13, 0.14, 0.98 * pa));
                p.stroke_rrect(x0 + 0.5, y0 + 0.5, POWER_W - 1.0, h - 1.0, 10.0, Color::white(0.12 * pa), 1.0);
                let mut y = y0 + 6.0;
                for (i, (label, _)) in POWER_ITEMS.iter().enumerate() {
                    if i == 3 {
                        p.line(x0 + 12.0, y + 4.0, x0 + POWER_W - 12.0, y + 4.0, Color::white(0.10 * pa), 1.0);
                        y += 8.0;
                    }
                    let t = Target::PowerItem(i);
                    if self.hovered == Some(t) {
                        p.rrect(x0 + 6.0, y, POWER_W - 12.0, POWER_ROW - 2.0, 6.0, Color::white(0.10 * pa));
                    }
                    p.text(
                        label,
                        &f.body,
                        x0 + 18.0,
                        y,
                        POWER_W - 30.0,
                        POWER_ROW - 2.0,
                        Align::Left,
                        Color::white(0.93 * pa),
                    );
                    if self.power_open {
                        pop.push((t, x0 + 6.0, y, POWER_W - 12.0, POWER_ROW - 2.0));
                    }
                    y += POWER_ROW;
                }
                if self.power_open {
                    pop.push((Target::PowerBg, 0.0, 0.0, g.w, g.h));
                }
                self.hits.splice(0..0, pop);
            }
        }
        let alpha = (self.reveal.value.clamp(0.0, 1.0) * 255.0) as u8;
        let shown = gfx.present(&self.layer, self.hwnd, pos, pw, ph, alpha);
        if !glass {
            self.backdrop.hold(screen);
        } else if self.backdrop.track(screen, None, self.hwnd) {
            self.dirty = true;
        }
        shown
    }
}

/// The little picture beside a search answer, centred on (cx, cy).
fn answer_glyph(p: &Painter, kind: smart::Kind, cx: f32, cy: f32, c: Color) {
    match kind {
        smart::Kind::Sum => {
            // A calculator: its screen and keys.
            p.stroke_rrect(cx - 8.0, cy - 10.0, 16.0, 20.0, 3.0, c, 1.4);
            p.rrect(cx - 5.0, cy - 7.0, 10.0, 4.0, 1.0, c);
            for (dx, dy) in [(-4.0, 1.0), (0.0, 1.0), (4.0, 1.0), (-4.0, 5.0), (0.0, 5.0), (4.0, 5.0)] {
                p.circle(cx + dx, cy + dy, 1.2, c);
            }
        }
        smart::Kind::Unit => {
            // A ruler.
            p.stroke_rrect(cx - 11.0, cy - 4.0, 22.0, 8.0, 1.5, c, 1.4);
            for i in 0..5 {
                let x = cx - 7.0 + i as f32 * 3.5;
                p.line(x, cy - 4.0, x, cy - if i % 2 == 0 { 0.5 } else { 2.0 }, c, 1.2);
            }
        }
        smart::Kind::Command => {
            // A prompt: > and a line.
            p.line(cx - 8.0, cy - 5.0, cx - 3.0, cy, c, 1.6);
            p.line(cx - 3.0, cy, cx - 8.0, cy + 5.0, c, 1.6);
            p.line(cx, cy + 5.0, cx + 8.0, cy + 5.0, c, 1.6);
        }
        smart::Kind::Web => {
            // A globe.
            p.stroke_circle(cx, cy, 9.0, c, 1.4);
            p.line(cx - 9.0, cy, cx + 9.0, cy, c, 1.2);
            p.line(cx, cy - 9.0, cx, cy + 9.0, c, 1.2);
            // Its meridians, as two curves either side of the middle one.
            p.arc(cx + 9.0, cy, 12.6, 2.5, 3.78, c, 1.2);
            p.arc(cx - 9.0, cy, 12.6, -0.64, 0.64, c, 1.2);
        }
    }
}

fn draw_icon(p: &Painter, icon: Option<&ID2D1Bitmap>, x: f32, y: f32, s: f32, a: f32) {
    match icon {
        Some(b) => p.icon(b, x + s / 2.0, y + s / 2.0, a),
        None => p.rrect(x + s * 0.1, y + s * 0.1, s * 0.8, s * 0.8, s * 0.2, Color::white(0.12 * a)),
    }
}

/// Small line icons for the sidebar, drawn around (cx, cy), about 16 units.
fn draw_glyph(p: &Painter, g: Glyph, cx: f32, cy: f32, c: Color) {
    let w = 1.3;
    match g {
        Glyph::Gear => {
            p.stroke_circle(cx, cy, 3.0, c, w);
            p.stroke_circle(cx, cy, 6.0, c, w);
            for j in 0..8 {
                let a = j as f32 * std::f32::consts::FRAC_PI_4;
                p.line(cx + a.cos() * 6.0, cy + a.sin() * 6.0, cx + a.cos() * 8.0, cy + a.sin() * 8.0, c, 2.0);
            }
        }
        Glyph::Folder => {
            p.stroke_rrect(cx - 8.0, cy - 5.0, 16.0, 11.0, 1.5, c, w);
            p.line(cx - 8.0, cy - 5.0, cx - 7.0, cy - 7.0, c, w);
            p.line(cx - 7.0, cy - 7.0, cx - 2.5, cy - 7.0, c, w);
            p.line(cx - 2.5, cy - 7.0, cx - 1.0, cy - 5.0, c, w);
        }
        Glyph::Download => {
            p.line(cx, cy - 7.0, cx, cy + 3.0, c, w);
            p.line(cx - 4.0, cy - 1.0, cx, cy + 3.0, c, w);
            p.line(cx + 4.0, cy - 1.0, cx, cy + 3.0, c, w);
            p.line(cx - 7.0, cy + 7.0, cx + 7.0, cy + 7.0, c, w);
        }
        Glyph::Document => {
            p.stroke_rrect(cx - 6.0, cy - 8.0, 12.0, 16.0, 1.5, c, w);
            for dy in [-3.0, 0.0, 3.0] {
                p.line(cx - 3.0, cy + dy, cx + 3.0, cy + dy, c, w);
            }
        }
        Glyph::Picture => {
            p.stroke_rrect(cx - 8.0, cy - 6.0, 16.0, 12.0, 1.5, c, w);
            p.circle(cx + 3.5, cy - 2.0, 1.5, c);
            p.line(cx - 6.0, cy + 4.0, cx - 2.0, cy - 1.0, c, w);
            p.line(cx - 2.0, cy - 1.0, cx + 2.0, cy + 4.0, c, w);
        }
        Glyph::Music => {
            p.circle(cx - 3.5, cy + 5.0, 2.5, c);
            p.line(cx - 1.2, cy + 5.0, cx - 1.2, cy - 7.0, c, w);
            p.line(cx - 1.2, cy - 7.0, cx + 6.0, cy - 4.5, c, w);
        }
        Glyph::Video => {
            p.stroke_rrect(cx - 8.0, cy - 5.0, 11.0, 10.0, 1.5, c, w);
            p.polygon(&[(cx + 4.0, cy - 1.0), (cx + 8.0, cy - 4.0), (cx + 8.0, cy + 4.0), (cx + 4.0, cy + 1.0)], c);
        }
        Glyph::Globe => {
            p.stroke_circle(cx, cy, 7.5, c, w);
            p.line(cx - 7.5, cy, cx + 7.5, cy, c, w);
            p.stroke_rrect(cx - 3.5, cy - 7.5, 7.0, 15.0, 3.5, c, w);
        }
        Glyph::Menu => {
            for dy in [-5.0, 0.0, 5.0] {
                p.line(cx - 7.0, cy + dy, cx + 7.0, cy + dy, c, w + 0.2);
            }
        }
        Glyph::Power => {
            p.arc(cx, cy + 0.5, 7.0, -1.05, 4.19, c, w + 0.3);
            p.line(cx, cy - 8.0, cx, cy - 1.0, c, w + 0.3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{letter_of, score};

    #[test]
    fn ranks_matches() {
        assert_eq!(score("Visual Studio Code", "vis"), Some(0));
        assert_eq!(score("Visual Studio Code", "code"), Some(1));
        assert_eq!(score("Notepad", "pad"), Some(2));
        assert_eq!(score("Visual Studio Code", "vsc"), Some(3));
        assert_eq!(score("Notepad", "xyz"), None);
    }

    #[test]
    fn groups_by_letter() {
        assert_eq!(letter_of("notepad"), 'N');
        assert_eq!(letter_of("7-Zip"), '#');
        assert_eq!(letter_of(""), '#');
    }
}
