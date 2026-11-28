//! Cascade's own popup menus, in place of Windows' (the plain grey lists
//! `TrackPopupMenu` draws). Drawn like the rest of Cascade: a rounded,
//! shadowed panel that fades and slides in, a highlight that glides to the
//! item under the pointer, ticks, greyed items, separators, headings,
//! submenus that open beside their item, and the keys a menu takes (up,
//! down, Home, End, Page up/down, Enter, Esc, right and left for submenus).
//! A long list scrolls, and from 15 items on has a filter box: type to
//! narrow it down.
//!
//! `show` waits for a pick, as `TrackPopupMenu` does, and returns its id (0
//! for none). `track` shows a menu built with Windows' menu functions, so
//! the shell's own menus (a file's Open with, Send to...) come out the same.

use std::cell::{Cell, RefCell};
use std::sync::Once;
use std::time::Instant;

use windows::core::{w, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gfx::{Align, Color, Gfx, Layer};
use crate::theme;

/// One line of a menu.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Item(Item),
    Separator,
    /// A small grey title over the items after it.
    Header(String),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Item {
    /// What `show` returns when it's picked (never 0).
    pub id: usize,
    pub text: String,
    /// A shortcut shown on the right, greyed.
    pub hint: String,
    pub checked: bool,
    /// An icon (a glyph of the icon font), or 0 for the one that goes with
    /// the item's words (Open, Copy, Delete...), if any (see `style_of`).
    pub icon: u32,
    pub enabled: bool,
    /// The default command (bold), as Open in a file's menu.
    pub bold: bool,
    pub sub: Vec<Entry>,
}

/// An item: `id` and `text`, enabled.
pub fn item(id: usize, text: impl Into<String>) -> Entry {
    Entry::Item(Item { id, text: text.into(), enabled: true, ..Default::default() })
}

/// An item with a submenu.
pub fn sub(text: impl Into<String>, entries: Vec<Entry>) -> Entry {
    Entry::Item(Item { text: text.into(), enabled: true, sub: entries, ..Default::default() })
}

/// An item with a tick when `on`.
pub fn check(id: usize, text: impl Into<String>, on: bool) -> Entry {
    Entry::Item(Item { id, text: text.into(), enabled: true, checked: on, ..Default::default() })
}

/// Where a menu opens.
#[derive(Clone, Copy, Debug)]
pub enum Place {
    /// At the pointer (above or to the left of it when there's no room).
    Cursor,
    /// Centred on a point (the Alt+F4 menu).
    Centre(POINT),
    /// Under a control (or over it when there's no room below), at least as wide.
    Below(RECT),
}

thread_local! {
    /// Set just before a picker runs: the control it belongs to (screen pixels).
    static ANCHOR: Cell<Option<(RECT, Instant)>> = const { Cell::new(None) };
}

/// The next menu shown at the pointer opens under `r` instead, as a
/// drop-down list (the settings window sets it for its pickers).
pub fn set_anchor(r: RECT) {
    ANCHOR.with(|a| a.set(Some((r, Instant::now()))));
}

// ------------------------------------------------------------- layout --

const SHADOW: f32 = 16.0;
const PAD: f32 = 4.0;
const ROW: f32 = 28.0;
const SEP: f32 = 7.0;
const HEAD: f32 = 26.0;
/// The most columns a long list is spread over.
const MAX_COLS: usize = 3;
/// What an icon takes in front of an item's text.
const ICON_W: f32 = 26.0;
const FILTER_H: f32 = 40.0;
/// Past this many items a list is one scrolling column, not several.
const COLS_UP_TO: usize = 80;
/// From this many items, a filter box.
const FILTER_FROM: usize = 15;
const MIN_W: f32 = 180.0;
const MAX_W: f32 = 560.0;
const MAX_H: f32 = 520.0;
const RADIUS: f32 = 8.0;
/// How long the fade and slide in take.
const OPEN_S: f32 = 0.14;

static ANIMATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Whether menus animate (`menu_animation`): fade and slide in, and a
/// highlight that glides. Off, they open at once and the highlight jumps.
pub fn configure(on: bool) {
    ANIMATE.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn animate() -> bool {
    ANIMATE.load(std::sync::atomic::Ordering::Relaxed)
}
/// How long the pointer rests on an item before its submenu opens.
const SUB_DELAY_S: f32 = 0.28;
const TIMER: usize = 1;

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn row_h(e: &Entry) -> f32 {
    match e {
        Entry::Item(_) => ROW,
        Entry::Separator => SEP,
        Entry::Header(_) => HEAD,
    }
}

/// Which entries show with `filter` typed: items whose text has it (without
/// separators and headings), or everything when nothing is typed.
fn visible(entries: &[Entry], filter: &str) -> Vec<usize> {
    let f = filter.to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| match e {
            _ if f.is_empty() => true,
            Entry::Item(it) => it.text.to_lowercase().contains(&f),
            _ => false,
        })
        .map(|(i, _)| i)
        .collect()
}

/// An icon (a glyph of the icon font) and its colour for what an item says,
/// so the usual commands are recognised at a glance wherever they come from.
fn style_of(text: &str) -> Option<(u32, Color)> {
    let t = text.replace(['&', '\u{2026}'], "").trim().to_lowercase();
    let t = t.as_str();
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    let start = |words: &[&str]| words.iter().any(|w| t.starts_with(w));
    let red = Color(0.97, 0.48, 0.45, 1.0);
    let green = Color(0.48, 0.84, 0.58, 1.0);
    let amber = Color(1.0, 0.78, 0.40, 1.0);
    let purple = Color(0.74, 0.64, 1.0, 1.0);
    let blue = Color(0.45, 0.72, 1.0, 1.0);
    let accent = theme::accent();
    Some(match () {
        _ if start(&["open in new tab", "open in new window", "new tab", "new window"]) => (0xE8A7, accent),
        _ if has(&["open file location", "show in folder", "reveal"]) => (0xE838, amber),
        _ if start(&["run as administrator"]) => (0xE7EF, amber),
        _ if start(&["open"]) => (0xE8E5, accent),
        _ if start(&["properties", "info"]) => (0xE946, amber),
        _ if start(&["refresh", "reload"]) => (0xE72C, accent),
        _ if start(&["paste"]) => (0xE77F, blue),
        _ if start(&["cut"]) => (0xE8C6, blue),
        _ if start(&["copy", "duplicate"]) => (0xE8C8, blue),
        _ if start(&["rename"]) => (0xE8AC, amber),
        _ if start(&["unpin"]) => (0xE77A, red),
        _ if start(&["delete", "remove", "clear", "empty"]) => (0xE74D, red),
        _ if start(&["close", "quit", "exit"]) => (0xE711, red),
        _ if start(&["new folder"]) => (0xE8F4, green),
        _ if start(&["new", "add", "create"]) => (0xE710, green),
        _ if start(&["pin"]) => (0xE718, green),
        _ if start(&["share", "send"]) => (0xE72D, accent),
        _ if start(&["sort", "order"]) => (0xE8CB, purple),
        _ if start(&["view", "details", "icons", "group"]) => (0xE8FD, purple),
        _ if start(&["select all"]) => (0xE8B3, purple),
        _ if has(&["terminal", "command prompt", "powershell"]) => (0xE756, green),
        _ if start(&["restore"]) => (0xE777, amber),
        _ if start(&["move up"]) => (0xE74A, accent),
        _ if start(&["move down"]) => (0xE74B, accent),
        _ if start(&["save"]) => (0xE74E, blue),
        _ if start(&["print"]) => (0xE749, blue),
        _ if start(&["edit"]) => (0xE70F, amber),
        _ if start(&["play"]) => (0xE768, green),
        _ if start(&["download", "install"]) => (0xE896, green),
        _ if has(&["settings", "personali"]) => (0xE713, accent),
        _ => return None,
    })
}

/// The icon and colour of `it`: its own, or what its words say.
fn item_style(it: &Item) -> Option<(u32, Color)> {
    if it.icon != 0 {
        return Some((it.icon, style_of(&it.text).map_or(theme::accent(), |s| s.1)));
    }
    style_of(&it.text)
}

fn pickable(e: &Entry) -> bool {
    matches!(e, Entry::Item(it) if it.enabled)
}

/// A shown menu: the root, or a submenu beside its item.
struct Panel {
    hwnd: HWND,
    layer: Layer,
    entries: Vec<Entry>,
    filtering: bool,
    filter: String,
    shown: Vec<usize>,
    /// Content size, logical (without the shadow).
    w: f32,
    h: f32,
    /// Where the window is (physical pixels, shadow included).
    x: i32,
    y: i32,
    k: f32,
    scroll: f32,
    hover: Option<usize>,
    /// The highlight's drawn place, gliding to the hovered row.
    glide: Option<f32>,
    /// When the pointer came to rest on the hovered item (for its submenu).
    hover_since: Instant,
    opened: Instant,
    /// Opened upwards (slides down into place) or downwards.
    up: bool,
    /// Which entry its open submenu belongs to.
    child_of: Option<usize>,
    /// A long list runs down a column and on into the next: how many, each
    /// one's width, and where each row of `shown` goes (column, top, height).
    cols: usize,
    col_w: f32,
    lay: Vec<(usize, f32, f32)>,
    /// Some item has an icon: all the text is moved over for them.
    icons: bool,
}

impl Panel {
    fn top(&self) -> f32 {
        if self.filtering {
            FILTER_H
        } else {
            PAD
        }
    }

    fn content_h(&self) -> f32 {
        self.lay.iter().map(|&(_, y, h)| y + h).fold(0.0, f32::max) + PAD
    }

    /// Lays `shown` out over the columns, each about the same height.
    fn relayout(&mut self) {
        let hs: Vec<f32> = self.shown.iter().map(|&i| row_h(&self.entries[i])).collect();
        let cols = self.cols.max(1);
        let target = hs.iter().sum::<f32>() / cols as f32;
        self.lay.clear();
        let (mut col, mut y) = (0usize, 0.0f32);
        for (n, &h) in hs.iter().enumerate() {
            if col + 1 < cols && y > 0.0 && y + h / 2.0 > target {
                col += 1;
                y = 0.0;
            }
            // A line at the top of a column has nothing to divide.
            let h = if y == 0.0 && matches!(self.entries[self.shown[n]], Entry::Separator) { 0.0 } else { h };
            self.lay.push((col, y, h));
            y += h;
        }
    }

    /// Which column row `n` of `shown` is in.
    fn col_of(&self, n: usize) -> usize {
        self.lay.get(n).map_or(0, |l| l.0)
    }

    fn max_scroll(&self) -> f32 {
        (self.top() + self.content_h() - self.h).max(0.0)
    }

    /// Row `n` of `shown`: its top (scrolled) and height.
    fn row(&self, n: usize) -> (f32, f32) {
        let (_, y, h) = self.lay[n];
        (self.top() + y - self.scroll, h)
    }

    /// The entry at a point (logical, relative to the panel's content).
    fn hit(&self, lx: f32, ly: f32) -> Option<usize> {
        if lx < 0.0 || lx >= self.w || ly < self.top() || ly >= self.h {
            return None;
        }
        let col = (lx / self.col_w) as usize;
        (0..self.shown.len()).find_map(|n| {
            let (y, h) = self.row(n);
            (self.col_of(n) == col && ly >= y && ly < y + h).then_some(self.shown[n])
        })
    }

    /// Scrolls just enough to show entry `i`.
    fn reveal(&mut self, i: usize) {
        let Some(n) = self.shown.iter().position(|&s| s == i) else { return };
        let (y, h) = self.row(n);
        let (top, bottom) = (self.top(), self.h - PAD);
        if y < top {
            self.scroll -= top - y;
        } else if y + h > bottom {
            self.scroll += y + h - bottom;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    fn contains(&self, p: POINT) -> bool {
        let m = (SHADOW * self.k) as i32;
        let (x0, y0) = (self.x + m, self.y + m);
        p.x >= x0 && p.y >= y0 && p.x < x0 + (self.w * self.k) as i32 && p.y < y0 + (self.h * self.k) as i32
    }

    fn local(&self, p: POINT) -> (f32, f32) {
        let m = SHADOW * self.k;
        ((p.x - self.x) as f32 / self.k - m / self.k, (p.y - self.y) as f32 / self.k - m / self.k)
    }

    fn animating(&self) -> bool {
        let opening = animate() && self.opened.elapsed().as_secs_f32() < OPEN_S;
        let gliding = animate()
            && match (self.glide, self.hover.and_then(|h| self.shown.iter().position(|&s| s == h))) {
                (Some(g), Some(n)) => (g - (self.row(n).0 + self.scroll)).abs() > 0.3,
                _ => false,
            };
        opening || gliding
    }
}

struct State {
    panels: Vec<Panel>,
    gfx: Gfx,
    done: bool,
    picked: usize,
    instance: HINSTANCE,
    owner: HWND,
}

fn register(instance: HINSTANCE) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("CascadeMenu"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    });
}

unsafe extern "system" fn proc_(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        // Submenus never take the focus from the menu they came from.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// The work area and scale of the screen at `p`.
fn screen_at(p: POINT) -> (RECT, f32) {
    unsafe {
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        (mi.rcWork, dx.max(96) as f32 / 96.0)
    }
}

impl State {
    /// Makes a panel for `entries` and places it: at `place`, or (for a
    /// submenu) beside `beside` (the item's row, screen pixels).
    fn open(&mut self, entries: Vec<Entry>, place: Place, beside: Option<RECT>, min_w: f32) {
        let at = match (place, beside) {
            (_, Some(r)) => POINT { x: r.right, y: r.top },
            (Place::Cursor, _) => {
                let mut p = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut p);
                }
                p
            }
            (Place::Centre(p), _) => p,
            (Place::Below(r), _) => POINT { x: r.left, y: r.bottom },
        };
        let (work, k) = screen_at(at);
        let items = entries.iter().filter(|e| matches!(e, Entry::Item(_))).count();
        let filtering = beside.is_none() && items >= FILTER_FROM;
        // As wide as the longest item (with room for ticks, hints and arrows).
        let f = &self.gfx.fonts;
        let checks = entries.iter().any(|e| matches!(e, Entry::Item(it) if it.checked));
        let widest = entries
            .iter()
            .map(|e| match e {
                Entry::Item(it) => {
                    let font = if it.bold { &f.num } else { &f.body };
                    let hint = if it.hint.is_empty() { 0.0 } else { self.gfx.measure(&it.hint, &f.small) + 24.0 };
                    let arrow = if it.sub.is_empty() { 0.0 } else { 20.0 };
                    self.gfx.measure(&it.text, font) + hint + arrow
                }
                Entry::Header(t) => self.gfx.measure(t, &f.small),
                Entry::Separator => 0.0,
            })
            .fold(0.0, f32::max);
        let lead = if checks { 34.0 } else { 14.0 };
        let icons = entries.iter().any(|e| matches!(e, Entry::Item(it) if item_style(it).is_some()));
        let icon_w = if icons { ICON_W } else { 0.0 };
        let w = (widest + lead + icon_w + 22.0 + PAD * 2.0).max(MIN_W).max(min_w).min(MAX_W).ceil();
        let shown: Vec<usize> = (0..entries.len()).collect();
        let mut panel = Panel {
            hwnd: HWND::default(),
            layer: Layer::default(),
            entries,
            filtering,
            filter: String::new(),
            shown,
            w,
            h: 0.0,
            x: 0,
            y: 0,
            k,
            scroll: 0.0,
            hover: None,
            glide: None,
            hover_since: Instant::now(),
            opened: Instant::now(),
            up: false,
            child_of: None,
            cols: 1,
            col_w: w,
            lay: Vec::new(),
            icons,
        };
        panel.relayout();
        let full = panel.top() + panel.content_h();
        let room = ((work.bottom - work.top) as f32 / k - 16.0).max(ROW * 3.0);
        let limit = MAX_H.min(room);
        // Too tall for the screen: the list runs on into more columns beside
        // (not for a drop-down under its control, which scrolls).
        // (A long list of choices, as the apps to uninstall, scrolls in one column, with its filter.)
        if min_w == 0.0 && full > limit && items <= COLS_UP_TO {
            let fit = (((work.right - work.left) as f32 / k - 24.0) / w).floor().max(1.0) as usize;
            panel.cols = ((full / limit).ceil() as usize).clamp(1, MAX_COLS).min(fit);
            panel.w = w * panel.cols as f32;
            panel.relayout();
        }
        let w = panel.w;
        let full = panel.top() + panel.content_h();
        panel.h = full.min(limit).ceil();
        // Placed so it fits on the screen.
        let (pw, ph) = ((w * k) as i32, (panel.h * k) as i32);
        let m = (SHADOW * k) as i32;
        let (mut x, mut y) = match (place, beside) {
            (_, Some(r)) => (r.right - (2.0 * k) as i32, r.top - (PAD * k) as i32),
            (Place::Centre(p), _) => (p.x - pw / 2, p.y - ph / 2),
            (Place::Below(r), _) => (r.left, r.bottom + (4.0 * k) as i32),
            (Place::Cursor, _) => (at.x, at.y),
        };
        if x + pw > work.right {
            x = match beside {
                // A submenu goes to the left of its menu instead.
                Some(r) => {
                    let parent_left = self.panels.last().map_or(r.left, |p| p.x + m);
                    parent_left - pw + (2.0 * k) as i32
                }
                None => match place {
                    Place::Cursor => at.x - pw,
                    _ => work.right - pw,
                },
            };
        }
        if y + ph > work.bottom {
            panel.up = true;
            y = match (place, beside) {
                (Place::Below(r), None) if r.top - ph - (4.0 * k) as i32 >= work.top => r.top - ph - (4.0 * k) as i32,
                (Place::Cursor, None) if at.y - ph >= work.top => at.y - ph,
                _ => work.bottom - ph,
            };
        }
        x = x.max(work.left);
        y = y.max(work.top);
        panel.x = x - m;
        panel.y = y - m;
        let root = self.panels.is_empty();
        let mut ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
        if !root {
            ex |= WS_EX_NOACTIVATE;
        }
        let owner = self.panels.first().map_or(self.owner, |p| p.hwnd);
        panel.hwnd = unsafe {
            CreateWindowExW(
                ex,
                w!("CascadeMenu"),
                w!(""),
                WS_POPUP,
                panel.x,
                panel.y,
                1,
                1,
                (!owner.is_invalid()).then_some(owner),
                None,
                Some(self.instance),
                None,
            )
            .unwrap_or_default()
        };
        // The first enabled item is highlighted when a submenu opens from the keyboard.
        self.panels.push(panel);
        let n = self.panels.len() - 1;
        self.render(n);
        unsafe {
            let hwnd = self.panels[n].hwnd;
            let _ = ShowWindow(hwnd, if root { SW_SHOW } else { SW_SHOWNOACTIVATE });
            if root {
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(hwnd));
            }
            SetTimer(Some(self.panels[0].hwnd), TIMER, 15, None);
        }
    }

    /// Closes the submenus deeper than panel `n`.
    fn close_after(&mut self, n: usize) {
        while self.panels.len() > n + 1 {
            if let Some(p) = self.panels.pop() {
                unsafe {
                    let _ = DestroyWindow(p.hwnd);
                }
            }
        }
        if let Some(p) = self.panels.get_mut(n) {
            p.child_of = None;
        }
    }

    /// The screen rectangle of entry `i`'s row in panel `n`.
    fn row_rect(&self, n: usize, i: usize) -> Option<RECT> {
        let p = &self.panels[n];
        let row = p.shown.iter().position(|&s| s == i)?;
        let (y, h) = p.row(row);
        let k = p.k;
        let (ox, oy) = (p.x + (SHADOW * k) as i32, p.y + (SHADOW * k) as i32);
        let left = ox + ((p.col_of(row) as f32 * p.col_w) * k) as i32;
        Some(RECT {
            left,
            top: oy + (y * k) as i32,
            right: left + (p.col_w * k) as i32,
            bottom: oy + ((y + h) * k) as i32,
        })
    }

    /// Opens entry `i` of panel `n`: its submenu, or picks it.
    fn activate(&mut self, n: usize, i: usize, keyboard: bool) {
        let Some(Entry::Item(it)) = self.panels[n].entries.get(i).cloned() else { return };
        if !it.enabled {
            return;
        }
        if it.sub.is_empty() {
            self.picked = it.id;
            self.done = true;
            return;
        }
        self.open_sub(n, i);
        if keyboard {
            let last = self.panels.len() - 1;
            let first = self.panels[last].entries.iter().position(pickable);
            self.panels[last].hover = first;
            self.render(last);
        }
    }

    fn open_sub(&mut self, n: usize, i: usize) {
        if self.panels[n].child_of == Some(i) {
            return;
        }
        self.close_after(n);
        let Some(Entry::Item(it)) = self.panels[n].entries.get(i).cloned() else { return };
        if it.sub.is_empty() || !it.enabled {
            return;
        }
        let Some(r) = self.row_rect(n, i) else { return };
        self.panels[n].child_of = Some(i);
        self.open(it.sub, Place::Cursor, Some(r), 0.0);
    }

    fn panel_at(&self, p: POINT) -> Option<usize> {
        (0..self.panels.len()).rev().find(|&n| self.panels[n].contains(p))
    }

    fn on_move(&mut self, p: POINT) {
        let Some(n) = self.panel_at(p) else { return };
        let (lx, ly) = self.panels[n].local(p);
        let hit = self.panels[n].hit(lx, ly).filter(|&i| !matches!(self.panels[n].entries[i], Entry::Separator));
        let panel = &mut self.panels[n];
        if hit != panel.hover {
            panel.hover = hit;
            panel.hover_since = Instant::now();
            self.render(n);
        }
    }

    fn on_click(&mut self, p: POINT) {
        let Some(n) = self.panel_at(p) else { return };
        let (lx, ly) = self.panels[n].local(p);
        if let Some(i) = self.panels[n].hit(lx, ly) {
            self.activate(n, i, false);
        }
    }

    fn on_wheel(&mut self, p: POINT, delta: i16) {
        let n = self.panel_at(p).unwrap_or(0);
        let panel = &mut self.panels[n];
        panel.scroll = (panel.scroll - delta as f32 / 120.0 * ROW * 3.0).clamp(0.0, panel.max_scroll());
        let mut c = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut c);
        }
        self.render(n);
        self.on_move(c);
    }

    /// Moves the highlight `by` pickable items (or to the first/last for ±∞).
    fn step(&mut self, by: i32) {
        let n = self.panels.len() - 1;
        let panel = &mut self.panels[n];
        let order: Vec<usize> = panel.shown.iter().copied().filter(|&i| pickable(&panel.entries[i])).collect();
        if order.is_empty() {
            return;
        }
        let at = panel.hover.and_then(|h| order.iter().position(|&o| o == h));
        let next = match (at, by) {
            (_, i32::MIN) => 0,
            (_, i32::MAX) => order.len() - 1,
            (None, b) if b > 0 => 0,
            (None, _) => order.len() - 1,
            (Some(a), b) => (a as i32 + b).clamp(0, order.len() as i32 - 1) as usize,
        };
        panel.hover = Some(order[next]);
        panel.hover_since = Instant::now();
        panel.reveal(order[next]);
        self.render(n);
    }

    fn on_key(&mut self, vk: u16) {
        let n = self.panels.len() - 1;
        let page = (self.panels[n].h / ROW).floor().max(1.0) as i32;
        match VIRTUAL_KEY(vk) {
            VK_DOWN => self.step(1),
            VK_UP => self.step(-1),
            VK_NEXT => self.step(page),
            VK_PRIOR => self.step(-page),
            VK_HOME if !self.panels[n].filtering => self.step(i32::MIN),
            VK_END if !self.panels[n].filtering => self.step(i32::MAX),
            VK_RETURN | VK_SPACE if !(vk == VK_SPACE.0 && self.panels[n].filtering) => {
                if let Some(i) = self.panels[n].hover {
                    self.activate(n, i, true);
                } else if self.panels[n].filtering {
                    // Enter with one match left picks it.
                    let p = &self.panels[n];
                    let left: Vec<usize> = p.shown.iter().copied().filter(|&i| pickable(&p.entries[i])).collect();
                    if left.len() == 1 {
                        self.activate(n, left[0], true);
                    }
                }
            }
            VK_RIGHT => {
                if let Some(i) = self.panels[n].hover {
                    if matches!(&self.panels[n].entries[i], Entry::Item(it) if !it.sub.is_empty()) {
                        self.activate(n, i, true);
                    }
                }
            }
            VK_LEFT if n > 0 => self.close_after(n - 1),
            VK_ESCAPE => {
                if n > 0 {
                    self.close_after(n - 1);
                } else if !self.panels[0].filter.is_empty() {
                    self.set_filter(String::new());
                } else {
                    self.done = true;
                }
            }
            VK_BACK if self.panels[0].filtering => {
                let mut f = self.panels[0].filter.clone();
                f.pop();
                self.set_filter(f);
            }
            _ => {}
        }
    }

    fn on_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        if self.panels[0].filtering && self.panels.len() == 1 {
            let f = format!("{}{c}", self.panels[0].filter);
            self.set_filter(f);
            return;
        }
        // Without a filter box, a letter goes to the next item starting with it.
        let n = self.panels.len() - 1;
        let p = &self.panels[n];
        let lc = c.to_lowercase().next().unwrap_or(c);
        let start = p.hover.unwrap_or(usize::MAX);
        let order: Vec<usize> = p.shown.clone();
        let from = order.iter().position(|&i| i == start).map_or(0, |a| a + 1);
        let found = (0..order.len()).map(|d| order[(from + d) % order.len()]).find(|&i| {
            matches!(&p.entries[i], Entry::Item(it) if it.enabled
                && it.text.trim_start().to_lowercase().starts_with(lc))
        });
        if let Some(i) = found {
            let p = &mut self.panels[n];
            p.hover = Some(i);
            p.reveal(i);
            self.render(n);
        }
    }

    fn set_filter(&mut self, f: String) {
        let p = &mut self.panels[0];
        p.shown = visible(&p.entries, &f);
        p.relayout();
        p.filter = f;
        p.scroll = 0.0;
        p.hover = p.shown.iter().copied().find(|&i| pickable(&p.entries[i]));
        p.glide = None;
        self.render(0);
    }

    /// The timer: animations, and submenus opening once the pointer rests.
    fn tick(&mut self) {
        for n in 0..self.panels.len() {
            let p = &self.panels[n];
            if let Some(i) = p.hover {
                let has_sub = matches!(&p.entries[i], Entry::Item(it) if !it.sub.is_empty() && it.enabled);
                let rested = p.hover_since.elapsed().as_secs_f32() >= SUB_DELAY_S;
                if rested
                    && p.child_of != Some(i)
                    && (has_sub || p.child_of.is_some())
                    && n + 1 >= self.panels.len() - 1
                {
                    if has_sub {
                        self.open_sub(n, i);
                    } else {
                        self.close_after(n);
                    }
                    return;
                }
            }
        }
        for n in 0..self.panels.len() {
            if self.panels[n].animating() {
                self.render(n);
            }
        }
    }

    fn render(&mut self, n: usize) {
        let gfx = &mut self.gfx;
        let p = &mut self.panels[n];
        let k = p.k;
        let (tw, th) = (p.w + SHADOW * 2.0, p.h + SHADOW * 2.0);
        let (pw, ph) = ((tw * k).ceil() as i32, (th * k).ceil() as i32);
        let t = if animate() { ease(p.opened.elapsed().as_secs_f32() / OPEN_S) } else { 1.0 };
        let slide = (1.0 - t) * 8.0 * if p.up { 1.0 } else { -1.0 };
        // The highlight glides to the hovered row.
        let target = p.hover.and_then(|h| p.shown.iter().position(|&s| s == h)).map(|row| p.row(row).0 + p.scroll);
        p.glide = match (p.glide, target) {
            (Some(g), Some(tg)) if animate() => Some(g + (tg - g) * 0.45),
            (_, tg) => tg,
        };
        if gfx.begin(&mut p.layer, pw, ph, k).is_err() {
            return;
        }
        {
            let d = gfx.painter();
            let f = &gfx.fonts;
            let (x0, y0) = (SHADOW, SHADOW + slide);
            // A soft shadow under the panel.
            for s in 1..=6 {
                let g = s as f32 * 2.0;
                d.rrect(
                    x0 - g + 2.0,
                    y0 - g + 6.0,
                    p.w + g * 2.0 - 4.0,
                    p.h + g * 2.0 - 4.0,
                    RADIUS + g,
                    Color(0.0, 0.0, 0.0, 0.035),
                );
            }
            d.rrect(x0, y0, p.w, p.h, RADIUS, Color(0.165, 0.165, 0.172, 0.985));
            d.stroke_rrect(x0 + 0.5, y0 + 0.5, p.w - 1.0, p.h - 1.0, RADIUS, Color::white(0.09), 1.0);
            if p.filtering {
                let (fx, fy, fw, fh) = (x0 + 6.0, y0 + 6.0, p.w - 12.0, FILTER_H - 10.0);
                d.rrect(fx, fy, fw, fh, 5.0, Color::white(0.06));
                d.rrect(
                    fx,
                    fy + fh - 2.0,
                    fw,
                    2.0,
                    1.0,
                    if p.filter.is_empty() { Color::white(0.2) } else { theme::accent() },
                );
                d.stroke_circle(fx + 15.0, fy + fh / 2.0 - 1.0, 4.5, Color::white(0.55), 1.3);
                d.line(fx + 18.2, fy + fh / 2.0 + 2.2, fx + 21.5, fy + fh / 2.0 + 5.5, Color::white(0.55), 1.3);
                let (text, c) = if p.filter.is_empty() {
                    ("Type to filter".to_string(), Color::white(0.4))
                } else {
                    (format!("{}\u{2502}", p.filter), Color::white(0.92))
                };
                d.text(&text, &f.body, fx + 30.0, fy, fw - 36.0, fh - 2.0, Align::Left, c);
            }
            let top = p.top();
            d.clip(x0, y0 + top, p.w, p.h - top);
            if let (Some(g), Some(_)) = (p.glide, p.hover) {
                let gy = y0 + g - p.scroll;
                let hot = p.hover.and_then(|h| p.shown.iter().position(|&s| s == h)).map_or(0, |n| p.col_of(n));
                let gx = x0 + hot as f32 * p.col_w;
                d.rrect(gx + PAD, gy + 2.0, p.col_w - PAD * 2.0, ROW - 4.0, 4.0, Color::white(0.075));
            }
            let checks = p.entries.iter().any(|e| matches!(e, Entry::Item(it) if it.checked));
            let lead = if checks { 34.0 } else { 14.0 };
            let icon_w = if p.icons { ICON_W } else { 0.0 };
            for (row, &i) in p.shown.iter().enumerate() {
                let (ry, rh) = p.row(row);
                if ry + rh < top - 2.0 || ry > p.h {
                    continue;
                }
                let ry = y0 + ry;
                let (cx0, cwid) = (x0 + p.col_of(row) as f32 * p.col_w, p.col_w);
                match &p.entries[i] {
                    Entry::Separator => {
                        d.rrect(cx0 + PAD + 8.0, ry + SEP / 2.0, cwid - PAD * 2.0 - 16.0, 1.0, 0.0, Color::white(0.09));
                    }
                    Entry::Header(text) => {
                        d.text(
                            text,
                            &f.small,
                            cx0 + 14.0,
                            ry + 6.0,
                            cwid - 28.0,
                            rh - 6.0,
                            Align::Left,
                            Color::white(0.5),
                        );
                    }
                    Entry::Item(it) => {
                        let a = if it.enabled { 0.93 } else { 0.38 };
                        if it.checked {
                            let (cx, cy) = (cx0 + 18.0, ry + rh / 2.0);
                            let c = theme::accent();
                            d.line(cx - 4.5, cy, cx - 1.3, cy + 3.5, c, 1.7);
                            d.line(cx - 1.3, cy + 3.5, cx + 5.0, cy - 3.5, c, 1.7);
                        }
                        let right = if it.sub.is_empty() { 14.0 } else { 30.0 };
                        let hint_w = if it.hint.is_empty() { 0.0 } else { gfx.measure(&it.hint, &f.small) + 16.0 };
                        let font = if it.bold { &f.num } else { &f.body };
                        let tw = cwid - lead - icon_w - right - hint_w;
                        // Its icon, in the colour that goes with what it does.
                        if let (Some((glyph, tint)), Some(font), true) = (item_style(it), &f.icons, p.icons) {
                            if let Some(ch) = char::from_u32(glyph) {
                                let c = tint.alpha(if it.enabled { 0.95 } else { 0.35 });
                                d.text(&ch.to_string(), font, cx0 + lead, ry, 16.0, rh, Align::Center, c);
                            }
                        }
                        d.text(&it.text, font, cx0 + lead + icon_w, ry, tw, rh, Align::Left, Color::white(a));
                        if !it.hint.is_empty() {
                            let hx = cx0 + cwid - right - hint_w;
                            d.text(&it.hint, &f.small, hx, ry, hint_w, rh, Align::Right, Color::white(a * 0.55));
                        }
                        if !it.sub.is_empty() {
                            let (ax, ay) = (cx0 + cwid - 18.0, ry + rh / 2.0);
                            d.line(ax - 2.0, ay - 4.0, ax + 2.0, ay, Color::white(a * 0.8), 1.4);
                            d.line(ax + 2.0, ay, ax - 2.0, ay + 4.0, Color::white(a * 0.8), 1.4);
                        }
                    }
                }
            }
            d.unclip();
            // A scroll bar while the list is longer than the panel.
            let max = p.max_scroll();
            if max > 0.0 {
                let track = p.h - top - PAD * 2.0;
                let bar = (track * (p.h - top) / (p.h - top + max)).max(24.0);
                let by = y0 + top + PAD + (track - bar) * (p.scroll / max);
                d.rrect(x0 + p.w - 6.0, by, 3.0, bar, 1.5, Color::white(0.3));
            }
            if p.filtering && p.shown.is_empty() {
                d.text("Nothing matches", &f.body, x0, y0 + top + 4.0, p.w, ROW, Align::Center, Color::white(0.45));
            }
        }
        let alpha = (t * 255.0) as u8;
        let _ = gfx.present(&p.layer, p.hwnd, POINT { x: p.x, y: p.y }, pw, ph, alpha.max(1));
    }
}

thread_local! {
    /// A menu is up (a second one waits for it to go).
    static OPEN: Cell<bool> = const { Cell::new(false) };
    static LAST: RefCell<Option<Instant>> = const { RefCell::new(None) };
}

/// Shows `entries` and waits for a pick: its id, or 0 when the menu was
/// left (Esc, a click elsewhere, another window taking over).
pub fn show(owner: HWND, entries: Vec<Entry>, place: Place) -> usize {
    if entries.is_empty() || OPEN.with(|o| o.replace(true)) {
        return 0;
    }
    let picked = run(owner, entries, place);
    OPEN.with(|o| o.set(false));
    LAST.with(|l| *l.borrow_mut() = Some(Instant::now()));
    picked
}

fn run(owner: HWND, entries: Vec<Entry>, place: Place) -> usize {
    let Ok(gfx) = Gfx::new() else { return 0 };
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).unwrap_or_default() };
    register(instance);
    // A picker opened from the settings window drops down from its control.
    // (Only just set: a picker that showed no list leaves it behind.)
    let anchor = ANCHOR.with(|a| a.take()).filter(|(_, t)| t.elapsed().as_secs_f32() < 2.0).map(|(r, _)| r);
    let (place, min_w) = match (place, anchor) {
        (Place::Cursor, Some(r)) => {
            let (_, k) = screen_at(POINT { x: r.left, y: r.top });
            (Place::Below(r), (r.right - r.left) as f32 / k)
        }
        (p @ Place::Below(r), _) => (p, (r.right - r.left) as f32 / screen_at(POINT { x: r.left, y: r.top }).1),
        (p, _) => (p, 0.0),
    };
    let mut st = State { panels: Vec::new(), gfx, done: false, picked: 0, instance, owner };
    st.open(entries, place, None, min_w);
    // A drop-down list starts on what's picked now.
    if let Some(i) = st.panels[0].entries.iter().position(|e| matches!(e, Entry::Item(it) if it.checked)) {
        st.panels[0].reveal(i);
        st.panels[0].hover = Some(i);
        st.panels[0].glide = None;
        st.render(0);
    }
    let root = st.panels[0].hwnd;
    let focused = unsafe { GetForegroundWindow() } == root;
    let mut quit = None;
    let mut m = MSG::default();
    while !st.done {
        unsafe {
            if GetMessageW(&mut m, None, 0, 0).0 <= 0 {
                quit = Some(m.wParam.0 as i32);
                break;
            }
        }
        let ours = st.panels.iter().any(|p| p.hwnd == m.hwnd);
        let screen = || {
            let mut p =
                POINT { x: (m.lParam.0 & 0xFFFF) as i16 as i32, y: ((m.lParam.0 >> 16) & 0xFFFF) as i16 as i32 };
            unsafe {
                let _ = ClientToScreen(m.hwnd, &mut p);
            }
            p
        };
        match m.message {
            WM_MOUSEMOVE if ours => st.on_move(screen()),
            WM_LBUTTONUP | WM_RBUTTONUP if ours => st.on_click(screen()),
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_LBUTTONDBLCLK if ours => {}
            // A click anywhere else leaves the menu (and goes no further, as with Windows' menus).
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN => st.done = true,
            WM_MOUSEWHEEL => {
                let p =
                    POINT { x: (m.lParam.0 & 0xFFFF) as i16 as i32, y: ((m.lParam.0 >> 16) & 0xFFFF) as i16 as i32 };
                st.on_wheel(p, ((m.wParam.0 >> 16) & 0xFFFF) as u16 as i16);
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                if m.wParam.0 as u16 == VK_MENU.0 || m.wParam.0 as u16 == VK_F10.0 {
                    st.done = true;
                } else {
                    st.on_key(m.wParam.0 as u16);
                }
            }
            WM_CHAR => {
                if let Some(c) = char::from_u32(m.wParam.0 as u32) {
                    st.on_char(c);
                }
            }
            WM_KEYUP | WM_SYSKEYUP | WM_SYSCHAR => {}
            WM_TIMER if ours => st.tick(),
            _ => unsafe {
                let _ = TranslateMessage(&m);
                DispatchMessageW(&m);
            },
        }
        // Another window took over (Alt+Tab, a click on another app).
        if focused && !st.done && unsafe { GetForegroundWindow() } != root {
            st.done = true;
        }
    }
    let picked = st.picked;
    st.close_after(0);
    if let Some(p) = st.panels.pop() {
        unsafe {
            let _ = KillTimer(Some(p.hwnd), TIMER);
            let _ = DestroyWindow(p.hwnd);
        }
    }
    if let Some(code) = quit {
        unsafe { PostQuitMessage(code) };
    }
    picked
}

// ------------------------------------------------- Windows' own menus --

/// What a menu item built with Windows' functions says: its text without
/// the `&` that marks its key, and the shortcut after a tab.
fn split_label(raw: &str) -> (String, String) {
    let (text, hint) = raw.split_once('\t').unwrap_or((raw, ""));
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            if chars.peek() == Some(&'&') {
                out.push('&');
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    (out.trim().to_string(), hint.trim().to_string())
}

/// Reads a menu made with Windows' functions. `init` is called on each
/// submenu before it's read (the shell fills Open with and Send to then).
pub fn from_hmenu(menu: HMENU, init: &mut dyn FnMut(HMENU, u32)) -> Vec<Entry> {
    let mut out = Vec::new();
    unsafe {
        let n = GetMenuItemCount(Some(menu));
        for i in 0..n.max(0) as u32 {
            let mut buf = vec![0u16; 512];
            let mut mi = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_SUBMENU | MIIM_STRING,
                dwTypeData: PWSTR(buf.as_mut_ptr()),
                cch: buf.len() as u32 - 1,
                ..Default::default()
            };
            if GetMenuItemInfoW(menu, i, true, &mut mi).is_err() {
                continue;
            }
            if mi.fType.0 & MFT_SEPARATOR.0 != 0 {
                if !matches!(out.last(), None | Some(Entry::Separator)) {
                    out.push(Entry::Separator);
                }
                continue;
            }
            let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
            let (text, hint) = split_label(&String::from_utf16_lossy(&buf[..len]));
            if text.is_empty() {
                continue;
            }
            let sub = if mi.hSubMenu.is_invalid() {
                Vec::new()
            } else {
                init(mi.hSubMenu, i);
                from_hmenu(mi.hSubMenu, init)
            };
            out.push(Entry::Item(Item {
                id: mi.wID as usize,
                text,
                hint,
                checked: mi.fState.0 & MFS_CHECKED.0 != 0,
                enabled: mi.fState.0 & MFS_DISABLED.0 == 0,
                bold: mi.fState.0 & MFS_DEFAULT.0 != 0,
                sub,
                icon: 0,
            }));
        }
    }
    while matches!(out.last(), Some(Entry::Separator)) {
        out.pop();
    }
    // A greyed first item over a separator is the menu's title.
    if let [Entry::Item(it), Entry::Separator, ..] = out.as_slice() {
        if !it.enabled && it.sub.is_empty() {
            let title = it.text.clone();
            out.splice(0..2, [Entry::Header(title)]);
        }
    }
    out
}

/// A file's menu, shortened as Windows 11's is: the everyday commands stay,
/// and what other apps added (7-Zip, WinRAR, Resize, Share...) goes under
/// "More options". Ids are kept, so a pick is carried out as before.
pub fn compact(entries: Vec<Entry>) -> Vec<Entry> {
    const KEEP: [&str; 14] = [
        "open",
        "open with",
        "open with\u{2026}",
        "edit with photos",
        "set as desktop background",
        "send to",
        "cut",
        "copy",
        "paste",
        "copy as path",
        "create shortcut",
        "rename",
        "delete",
        "properties",
    ];
    let keeps = |e: &Entry| match e {
        Entry::Item(i) => KEEP.contains(&i.text.to_lowercase().as_str()),
        _ => false,
    };
    let extra: Vec<Entry> = entries.iter().filter(|e| matches!(e, Entry::Item(_)) && !keeps(e)).cloned().collect();
    if extra.len() < 3 || !entries.iter().any(keeps) {
        return entries;
    }
    let mut out: Vec<Entry> = Vec::new();
    for e in entries {
        match e {
            Entry::Item(_) if keeps(&e) => out.push(e),
            Entry::Separator if !matches!(out.last(), None | Some(Entry::Separator)) => out.push(e),
            _ => {}
        }
    }
    while matches!(out.last(), Some(Entry::Separator)) {
        out.pop();
    }
    // Between the commands and Properties, so it's where the eye lands.
    let at = out
        .iter()
        .rposition(|e| matches!(e, Entry::Item(i) if i.text.eq_ignore_ascii_case("properties")))
        .unwrap_or(out.len());
    let more = Entry::Item(Item {
        id: 0x7FFF_FFF0,
        text: "More options".into(),
        enabled: true,
        sub: extra,
        ..Default::default()
    });
    out.splice(at..at, [more, Entry::Separator]);
    out
}

/// Shows a menu made with Windows' functions as one of Cascade's, and
/// returns the picked item's id (0 for none).
pub fn track(owner: HWND, menu: HMENU, place: Place) -> usize {
    show(owner, from_hmenu(menu, &mut |_, _| {}), place)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_lose_their_ampersands() {
        assert_eq!(split_label("&Open"), ("Open".into(), String::new()));
        assert_eq!(split_label("Save && close\tCtrl+S"), ("Save & close".into(), "Ctrl+S".into()));
    }

    #[test]
    fn filtering_keeps_matching_items() {
        let e = vec![
            item(1, "Discord"),
            Entry::Separator,
            item(2, "Docker Desktop"),
            Entry::Header("x".into()),
            item(3, "Git"),
        ];
        assert_eq!(visible(&e, ""), vec![0, 1, 2, 3, 4]);
        assert_eq!(visible(&e, "d"), vec![0, 2]);
        assert_eq!(visible(&e, "GIT"), vec![4]);
    }
}
