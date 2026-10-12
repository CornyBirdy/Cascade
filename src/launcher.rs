//! The Cascade launcher (Start menu replacement): search, pinned apps, all
//! apps and power options. The app list comes from the shell's AppsFolder,
//! the same source the Windows Start menu uses, so Store apps are included.

use std::time::Instant;

use windows::core::{Result, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::dock::Dock;
use crate::gfx::{Align, Color, Gfx, Layer, Painter};
use crate::msg;
use crate::spring::Spring;

const W: f32 = 620.0;
const H: f32 = 600.0;
const ACCENT: Color = Color(0.27, 0.55, 1.0, 1.0);
const COLS: usize = 6;
const PINNED_ROWS: usize = 2;
const LIST_ROW: f32 = 38.0;
const RESULT_ROW: f32 = 46.0;

pub struct AppEntry {
    pub name: String,
    /// Launch / icon target: `shell:AppsFolder\<id>`.
    pub target: String,
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
        // Skip uninstallers, readmes and web links that clutter "All apps".
        if name.is_empty()
            || lower.starts_with("uninstall")
            || [".txt", ".chm", ".pdf", ".html", ".htm", ".url", ".rtf"].iter().any(|e| id_lower.ends_with(e))
        {
            continue;
        }
        out.push(AppEntry { name, target: format!("shell:AppsFolder\\{id}") });
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    Ok(out)
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

// ------------------------------------------------------------- launcher --

#[derive(Clone, Copy, PartialEq)]
pub enum Target {
    Pinned(usize),
    App(usize),
    Settings,
    Power,
}

pub enum Action {
    Launch(String),
    Settings,
    Power,
    Close,
}

pub struct Launcher {
    pub hwnd: HWND,
    layer: Layer,
    pub open: bool,
    window_visible: bool,
    reveal: Spring,
    apps: Vec<AppEntry>,
    scanned: Option<Instant>,
    /// Pinned dock apps: (target, name), refreshed on open.
    pinned: Vec<(String, String)>,
    query: String,
    results: Vec<usize>,
    selected: usize,
    scroll: f32,
    hovered: Option<Target>,
    tracking: bool,
    hits: Vec<(Target, f32, f32, f32, f32)>,
    k: f32,
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
        let _ = SetForegroundWindow(h);
        let _ = BringWindowToTop(h);
        let _ = SetFocus(Some(h));
        if attach {
            let _ = AttachThreadInput(me, fg_thread, false);
        }
    }
}

impl Launcher {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            layer: Layer::default(),
            open: false,
            window_visible: false,
            reveal: Spring::new(0.0, 420.0, 34.0),
            apps: Vec::new(),
            scanned: None,
            pinned: Vec::new(),
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            scroll: 0.0,
            hovered: None,
            tracking: false,
            hits: Vec::new(),
            k: 1.0,
            dirty: false,
        }
    }

    pub fn set_apps(&mut self, apps: Vec<AppEntry>) {
        self.apps = apps;
        self.scanned = Some(Instant::now());
        self.filter();
        self.dirty = true;
    }

    pub fn app_target(&self, t: Target) -> Option<String> {
        match t {
            Target::Pinned(i) => self.pinned.get(i).map(|p| p.0.clone()),
            Target::App(i) => self.apps.get(i).map(|a| a.target.clone()),
            _ => None,
        }
    }

    pub fn show(&mut self, k: f32, pinned: Vec<(String, String)>) {
        self.k = k;
        self.pinned = pinned;
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
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOW);
            }
        }
        force_foreground(self.hwnd);
        self.reveal.set(1.0);
        self.dirty = true;
    }

    pub fn hide(&mut self) {
        if self.open {
            self.open = false;
            self.reveal.set(0.0);
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
            ESC => return Some(Action::Close),
            BACK => {
                self.query.pop();
                self.filter();
            }
            ENTER => {
                let i = if self.query.trim().is_empty() { None } else { self.results.get(self.selected).copied() };
                return i.map(|i| Action::Launch(self.apps[i].target.clone()));
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
        if self.query.trim().is_empty() {
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
            t @ (Target::Pinned(_) | Target::App(_)) => self.app_target(t).map(Action::Launch),
            Target::Settings => Some(Action::Settings),
            Target::Power => Some(Action::Power),
        }
    }

    pub fn on_wheel(&mut self, delta: i16) {
        let (top, bottom) = self.list_area();
        let rows = if self.query.trim().is_empty() {
            self.apps.len() as f32 * LIST_ROW
        } else {
            self.results.len() as f32 * RESULT_ROW
        };
        let max = (rows - (bottom - top)).max(0.0);
        self.scroll = (self.scroll - delta as f32 / 120.0 * LIST_ROW * 3.0).clamp(0.0, max);
        self.dirty = true;
    }

    // ------------------------------------------------------------- frames --

    fn pinned_rows(&self) -> usize {
        self.pinned.len().div_ceil(COLS).min(PINNED_ROWS)
    }

    /// Top and bottom of the scrolling list, in layout units.
    fn list_area(&self) -> (f32, f32) {
        if self.query.trim().is_empty() {
            let rows = self.pinned_rows() as f32;
            let label = if rows > 0.0 { 104.0 + rows * 86.0 + 6.0 } else { 78.0 };
            (label + 28.0, H - 68.0)
        } else {
            (78.0, H - 68.0)
        }
    }

    pub fn wants_frame(&self) -> bool {
        self.window_visible && (self.dirty || !self.reveal.settled())
    }

    pub fn step(&mut self, dt: f32) -> bool {
        self.reveal.step(dt)
    }

    /// Draws the launcher centred above `bottom` (screen px) on `monitor`.
    pub fn render(&mut self, gfx: &mut Gfx, dock: &mut Dock, monitor: RECT, bottom: i32) -> Result<()> {
        self.dirty = false;
        if !self.window_visible {
            return Ok(());
        }
        if !self.open && self.reveal.value < 0.01 {
            self.window_visible = false;
            self.layer.release();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return Ok(());
        }
        let k = self.k;
        let (pw, ph) = ((W * k).ceil() as i32, (H * k).ceil() as i32);
        let slide = ((1.0 - self.reveal.value.clamp(0.0, 1.0)) * 24.0 * k) as i32;
        let pos = POINT {
            x: monitor.left + (monitor.right - monitor.left - pw) / 2,
            y: (bottom - ph - (12.0 * k) as i32 + slide).max(monitor.top),
        };

        // Icons for what is on screen (requested lazily from the dock's cache).
        let empty = self.query.trim().is_empty();
        let (top, list_bottom) = self.list_area();
        let row_h = if empty { LIST_ROW } else { RESULT_ROW };
        let first = (self.scroll / row_h) as usize;
        let count = ((list_bottom - top) / row_h) as usize + 2;
        let visible: Vec<usize> = if empty {
            (first..(first + count).min(self.apps.len())).collect()
        } else {
            self.results.iter().copied().skip(first).take(count).collect()
        };
        let mut icons = Vec::new();
        for &i in &visible {
            icons.push((i, dock.icon(&self.apps[i].target, gfx)));
        }
        let pinned_icons: Vec<_> = if empty {
            self.pinned.iter().take(COLS * PINNED_ROWS).map(|(t, _)| dock.icon(t, gfx)).collect()
        } else {
            Vec::new()
        };

        self.hits.clear();
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            p.rrect(0.0, 0.0, W, H, 22.0, Color(0.09, 0.09, 0.10, 0.96));
            p.stroke_rrect(0.5, 0.5, W - 1.0, H - 1.0, 21.5, Color::white(0.09), 1.0);

            // Search box.
            p.rrect(20.0, 20.0, W - 40.0, 42.0, 21.0, Color::white(0.08));
            p.stroke_circle(42.0, 39.0, 6.0, Color::white(0.6), 1.6);
            p.line(46.5, 43.5, 51.0, 48.0, Color::white(0.6), 1.6);
            if self.query.is_empty() {
                p.text("Type to search apps", &f.body, 62.0, 20.0, W - 100.0, 42.0, Align::Left, Color::white(0.4));
                p.line(61.0, 31.0, 61.0, 50.0, Color::white(0.8), 1.2);
            } else {
                p.text(&self.query, &f.body, 62.0, 20.0, W - 100.0, 42.0, Align::Left, Color::white(0.95));
                let caret = 62.0 + gfx.measure(&self.query, &f.body).min(W - 102.0) + 1.0;
                p.line(caret, 31.0, caret, 50.0, Color::white(0.8), 1.2);
            }

            if empty {
                if !self.pinned.is_empty() {
                    p.text("Pinned", &f.label, 26.0, 74.0, 200.0, 24.0, Align::Left, Color::white(0.9));
                    let cell_w = (W - 40.0) / COLS as f32;
                    for (i, ((_, name), icon)) in self.pinned.iter().zip(&pinned_icons).enumerate() {
                        let (col, row) = ((i % COLS) as f32, (i / COLS) as f32);
                        let (x, y) = (20.0 + col * cell_w, 104.0 + row * 86.0);
                        let t = Target::Pinned(i);
                        if self.hovered == Some(t) {
                            p.rrect(x + 4.0, y, cell_w - 8.0, 80.0, 10.0, Color::white(0.07));
                        }
                        draw_icon(&p, icon.as_ref(), x + cell_w / 2.0 - 20.0, y + 8.0, 40.0);
                        p.text(
                            name,
                            &f.small,
                            x + 4.0,
                            y + 54.0,
                            cell_w - 8.0,
                            18.0,
                            Align::Center,
                            Color::white(0.85),
                        );
                        self.hits.push((t, x + 4.0, y, cell_w - 8.0, 80.0));
                    }
                }
                p.text("All apps", &f.label, 26.0, top - 28.0, 200.0, 24.0, Align::Left, Color::white(0.9));
            }

            p.clip(0.0, top, W, list_bottom - top);
            for (i, icon) in icons {
                let row = if empty { i } else { self.results.iter().position(|&r| r == i).unwrap_or(0) };
                let y = top + row as f32 * row_h - self.scroll;
                let t = Target::App(i);
                let selected = !empty && row == self.selected;
                if self.hovered == Some(t) || selected {
                    p.rrect(
                        16.0,
                        y + 2.0,
                        W - 32.0,
                        row_h - 4.0,
                        10.0,
                        Color::white(if selected { 0.10 } else { 0.06 }),
                    );
                }
                let s = if empty { 26.0 } else { 32.0 };
                draw_icon(&p, icon.as_ref(), 30.0, y + (row_h - s) / 2.0, s);
                let font = if empty { &f.body } else { &f.label };
                p.text(&self.apps[i].name, font, 30.0 + s + 14.0, y, W - 120.0, row_h, Align::Left, Color::white(0.92));
                let (vy0, vy1) = (y.max(top), (y + row_h).min(list_bottom));
                if vy1 > vy0 {
                    self.hits.push((t, 16.0, vy0, W - 32.0, vy1 - vy0));
                }
            }
            if !empty && self.results.is_empty() {
                p.text("No apps found", &f.body, 0.0, top + 20.0, W, 30.0, Align::Center, Color::white(0.5));
            }
            p.unclip();

            // Footer: user, Cascade settings, power.
            p.line(20.0, H - 64.0, W - 20.0, H - 64.0, Color::white(0.07), 1.0);
            let user = std::env::var("USERNAME").unwrap_or_default();
            p.circle(40.0, H - 32.0, 15.0, ACCENT);
            let initial: String = user.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            p.text(&initial, &f.label, 25.0, H - 47.0, 30.0, 30.0, Align::Center, Color::white(1.0));
            p.text(&user, &f.body, 64.0, H - 50.0, 300.0, 36.0, Align::Left, Color::white(0.9));
            for (t, cx) in [(Target::Settings, W - 82.0), (Target::Power, W - 40.0)] {
                let cy = H - 32.0;
                if self.hovered == Some(t) {
                    p.circle(cx, cy, 17.0, Color::white(0.10));
                }
                let c = Color::white(0.9);
                if t == Target::Settings {
                    p.stroke_circle(cx, cy, 5.0, c, 1.6);
                    for j in 0..8 {
                        let a = j as f32 * std::f32::consts::FRAC_PI_4;
                        p.line(cx + a.cos() * 7.0, cy + a.sin() * 7.0, cx + a.cos() * 9.5, cy + a.sin() * 9.5, c, 2.0);
                    }
                } else {
                    p.arc(cx, cy, 8.0, -1.05, 4.19, c, 1.8);
                    p.line(cx, cy - 10.0, cx, cy - 2.0, c, 1.8);
                }
                self.hits.push((t, cx - 17.0, cy - 17.0, 34.0, 34.0));
            }
        }
        let alpha = (self.reveal.value.clamp(0.0, 1.0) * 255.0) as u8;
        gfx.present(&self.layer, self.hwnd, pos, pw, ph, alpha)
    }
}

fn draw_icon(p: &Painter, icon: Option<&windows::Win32::Graphics::Direct2D::ID2D1Bitmap>, x: f32, y: f32, s: f32) {
    match icon {
        Some(b) => p.image(b, x, y, s, s, 1.0),
        None => p.rrect(x + s * 0.1, y + s * 0.1, s * 0.8, s * 0.8, s * 0.2, Color::white(0.12)),
    }
}

#[cfg(test)]
mod tests {
    use super::score;

    #[test]
    fn ranks_matches() {
        assert_eq!(score("Visual Studio Code", "vis"), Some(0));
        assert_eq!(score("Visual Studio Code", "code"), Some(1));
        assert_eq!(score("Notepad", "pad"), Some(2));
        assert_eq!(score("Visual Studio Code", "vsc"), Some(3));
        assert_eq!(score("Notepad", "xyz"), None);
    }
}
