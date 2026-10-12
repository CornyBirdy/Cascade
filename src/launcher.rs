//! The Cascade launcher (Start menu replacement), laid out like Windows 11
//! with a sidebar: places and power on the left, pinned apps and recent
//! files in the middle, and every app A to Z on the right. Typing searches.
//!
//! The app list comes from the shell's AppsFolder, the same source the
//! Windows Start menu uses, so Store apps are included.

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
use crate::dock::{Dock, ACCENT};
use crate::gfx::{Align, Color, Gfx, Layer, Painter};
use crate::msg;
use crate::spring::Spring;

/// Sidebar width (when shown).
const SIDE: f32 = 184.0;
const CELL_W: f32 = 90.0;
const CELL_H: f32 = 84.0;
/// All-apps column width.
const RIGHT_W: f32 = 220.0;
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

struct RecentFile {
    name: String,
    path: String,
    ago: String,
}

#[derive(Clone, Copy)]
enum Row {
    Letter(char),
    App(usize),
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
    let now = SystemTime::now();
    files
        .into_iter()
        .take(max)
        .map(|(t, p)| {
            let secs = now.duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
            let ago = match secs {
                0..60 => "Just now".to_string(),
                60..3600 => format!("{}m ago", secs / 60),
                3600..86400 => format!("{}h ago", secs / 3600),
                _ => format!("{}d ago", secs / 86400),
            };
            let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            RecentFile { name, path: p.to_string_lossy().into_owned(), ago }
        })
        .collect()
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
}

pub enum Action {
    Launch(String),
    Settings,
    Power,
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
    /// Left edge of the all-apps column.
    rx: f32,
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
        let side = if show_side { SIDE } else { 0.0 };
        let mx = side + 20.0;
        let cols = cfg.start_columns as usize;
        let rx = mx + cols as f32 * CELL_W + 16.0;
        let show_all = cfg.start_show_all_apps;
        let w = if show_all { rx + RIGHT_W + PAD } else { mx + cols as f32 * CELL_W + PAD + 4.0 };
        Geo {
            w,
            h: cfg.start_height,
            side,
            mx,
            cols,
            prows: cfg.start_pinned_rows as usize,
            rx,
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
    results: Vec<usize>,
    selected: usize,
    scroll: f32,
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
            results: Vec::new(),
            selected: 0,
            scroll: 0.0,
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
        self.acrylic = cfg.acrylic;
        self.radius = cfg.corner_radius();
        self.geo = Geo::new(cfg);
        self.position = match cfg.start_position.as_str() {
            "center" => Position::Center,
            "left" => Position::Left,
            _ => Position::Auto,
        };
        let tint = ((cfg.start_opacity.clamp(0.0, 1.0) * 255.0) as u32) << 24 | 0x00_1C1C1E;
        self.backdrop.style(cfg.acrylic, tint, self.radius);
        self.scroll = 0.0;
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
        self.recent = load_recent(8);
        self.query.clear();
        self.filter();
        self.scroll = 0.0;
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

    pub fn hide(&mut self) {
        crate::mousehook::set_rect(0, None);
        if self.open {
            self.open = false;
            self.reveal.set(0.0);
            self.backdrop.hide();
            self.dirty = true;
        }
    }

    fn filter(&mut self) {
        let mut scored: Vec<(u32, usize)> =
            self.apps.iter().enumerate().filter_map(|(i, a)| score(&a.name, &self.query).map(|s| (s, i))).collect();
        scored.sort_by_key(|(s, i)| (*s, *i));
        self.results = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = 0;
        self.scroll = 0.0;
    }

    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    fn launch(&self, target: String) -> Action {
        if target == SETTINGS_TARGET {
            Action::Settings
        } else {
            Action::Launch(target)
        }
    }

    // -------------------------------------------------------------- input --

    pub fn on_char(&mut self, c: char) {
        if !c.is_control() {
            self.query.push(c);
            self.filter();
            self.dirty = true;
        }
    }

    /// Returns an action for keys that do something.
    pub fn on_key(&mut self, vk: u16) -> Option<Action> {
        const BACK: u16 = 0x08;
        const ENTER: u16 = 0x0D;
        const ESC: u16 = 0x1B;
        const UP: u16 = 0x26;
        const DOWN: u16 = 0x28;
        match vk {
            ESC if self.searching() => {
                self.query.clear();
                self.filter();
            }
            ESC => return Some(Action::Close),
            BACK => {
                self.query.pop();
                self.filter();
            }
            ENTER => {
                let i = if self.searching() { self.results.get(self.selected).copied() } else { None };
                return i.map(|i| self.launch(self.apps[i].target.clone()));
            }
            UP => self.selected = self.selected.saturating_sub(1),
            DOWN => self.selected = (self.selected + 1).min(self.results.len().saturating_sub(1)),
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
        if y < self.scroll {
            self.scroll = y;
        } else if y + RESULT_ROW > self.scroll + view {
            self.scroll = y + RESULT_ROW - view;
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
        let h = self.hit(x, y);
        if h != self.hovered {
            self.hovered = h;
            self.dirty = true;
        }
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        self.hovered = None;
        self.dirty = true;
    }

    pub fn target_at(&self, x: i32, y: i32) -> Option<Target> {
        self.hit(x, y)
    }

    pub fn on_click(&mut self, x: i32, y: i32) -> Option<Action> {
        match self.hit(x, y)? {
            Target::Pinned(i) => self.pinned.get(i).map(|p| self.launch(p.0.clone())),
            Target::App(i) => self.apps.get(i).map(|a| self.launch(a.target.clone())),
            Target::Recent(i) => self.recent.get(i).map(|r| Action::Launch(r.path.clone())),
            Target::Place(i) => Some(Action::Launch(PLACES[i].1.to_string())),
            Target::Profile => Some(Action::Launch("ms-settings:yourinfo".into())),
            Target::Settings => Some(Action::Settings),
            Target::Power => Some(Action::Power),
        }
    }

    pub fn on_wheel(&mut self, delta: i16) {
        let (top, bottom) = self.list_area();
        let rows =
            if self.searching() { self.results.len() as f32 * RESULT_ROW } else { self.rows.len() as f32 * LIST_ROW };
        let max = (rows - (bottom - top)).max(0.0);
        self.scroll = (self.scroll - delta as f32 / 120.0 * LIST_ROW * 3.0).clamp(0.0, max);
        self.dirty = true;
    }

    // ------------------------------------------------------------- frames --

    /// Top and bottom of the scrolling list, in layout units.
    fn list_area(&self) -> (f32, f32) {
        (TOP + 30.0, self.geo.h - PAD)
    }

    pub fn wants_frame(&self) -> bool {
        self.window_visible && (self.dirty || !self.reveal.settled())
    }

    pub fn step(&mut self, dt: f32) -> bool {
        self.reveal.step(dt)
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

        let settled = self.open && self.acrylic && self.reveal.value > 0.97;
        if settled {
            self.backdrop.place(RECT { left: x, top: y, right: x + pw, bottom: y + ph }, self.hwnd);
        } else {
            self.backdrop.hide();
        }

        // Icons for what is on screen (loaded lazily through the dock's cache).
        let searching = self.searching();
        let (top, list_bottom) = self.list_area();
        let row_h = if searching { RESULT_ROW } else { LIST_ROW };
        let first = (self.scroll / row_h) as usize;
        let count = ((list_bottom - top) / row_h) as usize + 2;
        let visible: Vec<(usize, Row)> = if searching {
            self.results.iter().enumerate().skip(first).take(count).map(|(r, &i)| (r, Row::App(i))).collect()
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
                Row::Letter(_) => None,
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
            // Over the blur only a faint wash (it also keeps the pixels
            // clickable); while animating or without acrylic, a solid panel.
            let fill = if settled { Color::white(0.015) } else { Color(0.13, 0.13, 0.14, 0.97) };
            p.rrect(0.0, 0.0, g.w, g.h, r, fill);
            if g.show_side {
                p.clip(0.0, 0.0, g.side, g.h);
                p.rrect(0.0, 0.0, g.side + r * 2.0, g.h, r, Color(0.0, 0.0, 0.0, 0.16));
                p.unclip();
            }
            p.stroke_rrect(0.5, 0.5, g.w - 1.0, g.h - 1.0, r, Color::white(0.10), 1.0);

            if g.show_side {
                // Sidebar: profile, places, Cascade settings and power.
                let user = std::env::var("USERNAME").unwrap_or_default();
                let (px, py) = (10.0, 14.0);
                if g.show_profile {
                    if self.hovered == Some(Target::Profile) {
                        p.rrect(px, py, g.side - 20.0, 44.0, 6.0, Color::white(0.07));
                    }
                    p.circle(px + 22.0, py + 22.0, 15.0, ACCENT);
                    let initial: String = user.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
                    p.text(&initial, &f.label, px + 7.0, py + 7.0, 30.0, 30.0, Align::Center, Color::white(1.0));
                    p.text(&user, &f.title, px + 44.0, py, g.side - 64.0, 44.0, Align::Left, Color::white(0.95));
                    self.hits.push((Target::Profile, px, py, g.side - 20.0, 44.0));
                }

                let side_item = |hits: &mut Vec<_>, t: Target, label: &str, glyph: Glyph, y: f32| {
                    if self.hovered == Some(t) {
                        p.rrect(10.0, y, g.side - 20.0, SIDE_ROW - 4.0, 5.0, Color::white(0.07));
                    }
                    draw_glyph(&p, glyph, 32.0, y + (SIDE_ROW - 4.0) / 2.0, Color::white(0.88));
                    p.text(label, &f.body, 54.0, y, g.side - 64.0, SIDE_ROW - 4.0, Align::Left, Color::white(0.9));
                    hits.push((t, 10.0, y, g.side - 20.0, SIDE_ROW - 4.0));
                };
                let mut slot = 0;
                for (i, (label, _, glyph)) in PLACES.iter().enumerate().filter(|(i, _)| g.places[*i]) {
                    let top = if g.show_profile { TOP } else { py };
                    side_item(&mut self.hits, Target::Place(i), label, *glyph, top + slot as f32 * SIDE_ROW);
                    slot += 1;
                }
                let rule_y = if g.show_cascade { g.h - PAD - SIDE_ROW * 2.0 - 6.0 } else { g.h - PAD - SIDE_ROW - 6.0 };
                p.line(18.0, rule_y, g.side - 18.0, rule_y, Color::white(0.08), 1.0);
                if g.show_cascade {
                    side_item(&mut self.hits, Target::Settings, "Cascade", Glyph::Gear, g.h - PAD - SIDE_ROW * 2.0);
                }
                side_item(&mut self.hits, Target::Power, "Power", Glyph::Power, g.h - PAD - SIDE_ROW);
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
                    self.hits.push((t, cx - 16.0, cy - 16.0, 32.0, 32.0));
                }
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
                p.text(&self.query, &f.body, sx + 40.0, 18.0, sw - 60.0, sh, Align::Left, Color::white(0.95));
                let caret = sx + 40.0 + gfx.measure(&self.query, &f.body).min(sw - 62.0) + 1.0;
                p.line(caret, 28.0, caret, 46.0, Color::white(0.8), 1.2);
            }

            if searching {
                p.text("Best matches", &f.label, g.mx + 4.0, TOP, 300.0, 22.0, Align::Left, Color::white(0.9));
                p.clip(g.mx - 6.0, top, g.w - PAD - g.mx + 6.0, list_bottom - top);
                for (&(row, item), icon) in visible.iter().zip(&row_icons) {
                    let Row::App(i) = item else { continue };
                    let y = top + row as f32 * RESULT_ROW - self.scroll;
                    let t = Target::App(i);
                    let selected = row == self.selected;
                    if self.hovered == Some(t) || selected {
                        let a = if selected { 0.10 } else { 0.06 };
                        p.rrect(g.mx - 6.0, y + 2.0, g.w - PAD - g.mx + 6.0, RESULT_ROW - 4.0, 6.0, Color::white(a));
                    }
                    if selected {
                        p.rrect(g.mx - 6.0, y + 14.0, 3.0, RESULT_ROW - 28.0, 1.5, ACCENT);
                    }
                    draw_icon(&p, icon.as_ref(), g.mx + 8.0, y + (RESULT_ROW - 30.0) / 2.0, 30.0);
                    let name = &self.apps[i].name;
                    p.text(
                        name,
                        &f.body,
                        g.mx + 50.0,
                        y,
                        g.w - g.mx - 90.0,
                        RESULT_ROW,
                        Align::Left,
                        Color::white(0.95),
                    );
                    let (vy0, vy1) = (y.max(top), (y + RESULT_ROW).min(list_bottom));
                    if vy1 > vy0 {
                        self.hits.push((t, g.mx - 6.0, vy0, g.w - PAD - g.mx + 6.0, vy1 - vy0));
                    }
                }
                if self.results.is_empty() {
                    p.text(
                        "No apps found",
                        &f.body,
                        g.mx,
                        top + 10.0,
                        g.w - g.mx - PAD,
                        30.0,
                        Align::Left,
                        Color::white(0.5),
                    );
                }
                p.unclip();
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
                        draw_icon(&p, icon.as_ref(), x + CELL_W / 2.0 - 16.0, y + 12.0, 32.0);
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
                        draw_icon(&p, icon.as_ref(), g.mx + 8.0, y + 7.0, 28.0);
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
                    let rw = g.w - PAD - g.rx;
                    p.line(g.rx - 8.0, TOP, g.rx - 8.0, g.h - PAD, Color::white(0.06), 1.0);
                    p.text("All apps", &f.label, g.rx + 4.0, TOP, rw, 22.0, Align::Left, Color::white(0.9));
                    p.clip(g.rx - 4.0, top, rw + 4.0, list_bottom - top);
                    for (&(row, item), icon) in visible.iter().zip(&row_icons) {
                        let y = top + row as f32 * LIST_ROW - self.scroll;
                        match item {
                            Row::Letter(l) => {
                                let s = l.to_string();
                                p.text(&s, &f.label, g.rx + 8.0, y + 6.0, rw, LIST_ROW - 6.0, Align::Left, ACCENT);
                            }
                            Row::App(i) => {
                                let t = Target::App(i);
                                if self.hovered == Some(t) {
                                    p.rrect(g.rx - 4.0, y + 1.0, rw + 4.0, LIST_ROW - 2.0, 5.0, Color::white(0.07));
                                }
                                draw_icon(&p, icon.as_ref(), g.rx + 6.0, y + (LIST_ROW - 22.0) / 2.0, 22.0);
                                let name = &self.apps[i].name;
                                p.text(
                                    name,
                                    &f.body,
                                    g.rx + 36.0,
                                    y,
                                    rw - 40.0,
                                    LIST_ROW,
                                    Align::Left,
                                    Color::white(0.92),
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
                }
            }
        }
        let alpha = (self.reveal.value.clamp(0.0, 1.0) * 255.0) as u8;
        gfx.present(&self.layer, self.hwnd, pos, pw, ph, alpha)
    }
}

fn draw_icon(p: &Painter, icon: Option<&ID2D1Bitmap>, x: f32, y: f32, s: f32) {
    match icon {
        Some(b) => p.icon(b, x + s / 2.0, y + s / 2.0, 1.0),
        None => p.rrect(x + s * 0.1, y + s * 0.1, s * 0.8, s * 0.8, s * 0.2, Color::white(0.12)),
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
