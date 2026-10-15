//! The Win+X menu in Cascade's theme: Windows' quick links (Device Manager,
//! Terminal, Task Manager, the power options...) in the same order and with
//! the same access keys, above the Start button. Opened with Win+X or a
//! right-click on Start.
//!
//! Like Windows' own, it works from the keyboard: arrow keys and Enter, or
//! the letter shown beside an item (Win+X, then U, then R restarts).

use windows::core::Result;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::backdrop::Backdrop;
use crate::gfx::{Align, Color, Gfx, Layer};
use crate::shell::Power;
use crate::spring::Spring;
use crate::theme;

const W: f32 = 252.0;
const ROW: f32 = 32.0;
const RULE: f32 = 9.0;
const PAD: f32 = 6.0;
/// How far power options are indented under "Shut down or sign out".
const INDENT: f32 = 14.0;

#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    /// Something to open: a settings page, a console, a program.
    Open(&'static str),
    /// Windows Terminal (PowerShell where it isn't installed).
    Terminal {
        admin: bool,
    },
    Search,
    Run,
    Desktop,
    Power(Power),
}

#[derive(Clone, Copy, PartialEq)]
enum Row {
    Item(&'static str, char, Action),
    Rule,
    /// "Shut down or sign out", which opens the power options under it.
    PowerHead,
    /// One of those options.
    PowerItem(&'static str, char, Power),
}

/// Windows 11's Win+X menu, in order. Mobility Center only shows on a laptop.
const MOBILITY: Row = Row::Item("Mobility Center", 'B', Action::Open("mblctr.exe"));
const TOP: [Row; 11] = [
    Row::Item("Installed apps", 'F', Action::Open("ms-settings:appsfeatures")),
    Row::Item("Power Options", 'O', Action::Open("ms-settings:powersleep")),
    Row::Item("Event Viewer", 'V', Action::Open("eventvwr.msc")),
    Row::Item("System", 'Y', Action::Open("ms-settings:about")),
    Row::Item("Device Manager", 'M', Action::Open("devmgmt.msc")),
    Row::Item("Network Connections", 'W', Action::Open("ms-settings:network")),
    Row::Item("Disk Management", 'K', Action::Open("diskmgmt.msc")),
    Row::Item("Computer Management", 'G', Action::Open("compmgmt.msc")),
    Row::Item("Terminal", 'I', Action::Terminal { admin: false }),
    Row::Item("Terminal (Admin)", 'A', Action::Terminal { admin: true }),
    Row::Rule,
];
const MIDDLE: [Row; 6] = [
    Row::Item("Task Manager", 'T', Action::Open("taskmgr.exe")),
    Row::Item("Settings", 'N', Action::Open("ms-settings:")),
    Row::Item("File Explorer", 'E', Action::Open("explorer.exe")),
    Row::Item("Search", 'S', Action::Search),
    Row::Item("Run", 'R', Action::Run),
    Row::Rule,
];
const POWER: [Row; 4] = [
    Row::PowerItem("Sign out", 'I', Power::SignOut),
    Row::PowerItem("Sleep", 'S', Power::Sleep),
    Row::PowerItem("Shut down", 'U', Power::ShutDown),
    Row::PowerItem("Restart", 'R', Power::Restart),
];
const DESKTOP: Row = Row::Item("Desktop", 'D', Action::Desktop);

/// The rows shown: with Mobility Center on a laptop, and the power options
/// when they're open.
fn rows(laptop: bool, power_open: bool) -> Vec<Row> {
    let mut v = Vec::with_capacity(24);
    v.push(TOP[0]);
    if laptop {
        v.push(MOBILITY);
    }
    v.extend_from_slice(&TOP[1..]);
    v.extend_from_slice(&MIDDLE);
    v.push(Row::PowerHead);
    if power_open {
        v.extend_from_slice(&POWER);
    }
    v.push(DESKTOP);
    v
}

fn row_h(r: &Row) -> f32 {
    if *r == Row::Rule {
        RULE
    } else {
        ROW
    }
}

fn selectable(r: &Row) -> bool {
    *r != Row::Rule
}

/// What a key press did.
pub enum Key {
    Nothing,
    Close,
    Run(Action),
}

/// Which rows show and which is highlighted: the menu's keyboard and
/// pointer state, apart from its window.
#[derive(Default)]
struct Nav {
    laptop: bool,
    power_open: bool,
    /// The highlighted row (pointer or keyboard).
    sel: Option<usize>,
}

impl Nav {
    fn rows(&self) -> Vec<Row> {
        rows(self.laptop, self.power_open)
    }

    fn activate(&mut self, i: usize) -> Option<Action> {
        match self.rows().get(i)? {
            Row::Item(_, _, a) => Some(*a),
            Row::PowerItem(_, _, p) => Some(Action::Power(*p)),
            Row::PowerHead => {
                self.set_power(!self.power_open, false);
                None
            }
            Row::Rule => None,
        }
    }

    /// Opens or closes the power options; `select_first` moves the
    /// highlight onto the first of them.
    fn set_power(&mut self, open: bool, select_first: bool) {
        self.power_open = open;
        let head = self.rows().iter().position(|r| *r == Row::PowerHead);
        self.sel = if open && select_first { head.map(|h| h + 1) } else { head };
    }

    /// Moves the highlight `step` selectable rows along, wrapping round.
    fn step_sel(&mut self, step: i32) {
        let rows = self.rows();
        let n = rows.len() as i32;
        let mut i = match self.sel {
            Some(i) => i as i32,
            None if step > 0 => -1,
            None => n,
        };
        for _ in 0..n {
            i = (i + step).rem_euclid(n);
            if selectable(&rows[i as usize]) {
                self.sel = Some(i as usize);
                break;
            }
        }
    }

    fn on_key(&mut self, vk: u16) -> Key {
        let rows = self.rows();
        let in_power = self.sel.and_then(|i| rows.get(i)).is_some_and(|r| matches!(r, Row::PowerItem(..)));
        match VIRTUAL_KEY(vk) {
            VK_ESCAPE if self.power_open => self.set_power(false, false),
            VK_ESCAPE => return Key::Close,
            VK_UP => self.step_sel(-1),
            VK_DOWN | VK_TAB => self.step_sel(1),
            VK_HOME => {
                self.sel = None;
                self.step_sel(1);
            }
            VK_END => {
                self.sel = None;
                self.step_sel(-1);
            }
            VK_RIGHT if self.sel.and_then(|i| rows.get(i)) == Some(&Row::PowerHead) => self.set_power(true, true),
            VK_LEFT if in_power => self.set_power(false, false),
            VK_RETURN | VK_SPACE => {
                let sel = self.sel;
                if let Some(a) = sel.and_then(|i| self.activate(i)) {
                    return Key::Run(a);
                }
            }
            _ => {
                // An access key: the open power options' first, then the rest.
                let Some(c) = char::from_u32(vk as u32).filter(|c| c.is_ascii_alphanumeric()) else {
                    return Key::Nothing;
                };
                let found = |power: bool| {
                    rows.iter().position(|r| match r {
                        Row::PowerItem(_, key, _) => power && *key == c,
                        Row::Item(_, key, _) => !power && *key == c,
                        Row::PowerHead => !power && c == 'U',
                        Row::Rule => false,
                    })
                };
                let hit = if self.power_open { found(true).or_else(|| found(false)) } else { found(false) };
                if let Some(i) = hit {
                    self.sel = Some(i);
                    if rows[i] == Row::PowerHead {
                        self.set_power(true, true);
                    } else if let Some(a) = self.activate(i) {
                        return Key::Run(a);
                    }
                }
            }
        }
        Key::Nothing
    }
}

pub struct QuickMenu {
    pub hwnd: HWND,
    layer: Layer,
    backdrop: Backdrop,
    pub open: bool,
    visible: bool,
    reveal: Spring,
    nav: Nav,
    tracking: bool,
    /// Screen x of the menu's left edge and y of its bottom edge, and the
    /// screen it has to stay on.
    anchor: (i32, i32),
    bounds: RECT,
    k: f32,
    acrylic: bool,
    /// The Tint slider.
    opacity: f32,
    radius: f32,
    pub dirty: bool,
}

impl QuickMenu {
    pub fn new(hwnd: HWND, backdrop: Backdrop) -> Self {
        Self {
            hwnd,
            layer: Layer::default(),
            backdrop,
            open: false,
            visible: false,
            reveal: Spring::new(0.0, 600.0, 44.0),
            nav: Nav::default(),
            tracking: false,
            anchor: (0, 0),
            bounds: RECT::default(),
            k: 1.0,
            acrylic: true,
            opacity: 0.55,
            radius: 8.0,
            dirty: false,
        }
    }

    pub fn configure(&mut self, acrylic: bool, opacity: f32, radius: f32) {
        self.acrylic = theme::style_backdrop(&self.backdrop, acrylic, opacity, radius);
        self.opacity = opacity.clamp(0.0, 1.0);
        self.radius = radius;
        self.dirty = true;
    }

    /// Opens with its left edge at screen x `left` and its bottom at
    /// `bottom`, kept on `bounds`. `keyboard` highlights the first item
    /// (opened with Win+X rather than a click).
    pub fn show(&mut self, left: i32, bottom: i32, bounds: RECT, laptop: bool, keyboard: bool, k: f32) {
        self.anchor = (left, bottom);
        self.bounds = bounds;
        self.nav = Nav { laptop, power_open: false, sel: keyboard.then_some(0) };
        self.k = k;
        self.open = true;
        if !self.visible {
            self.visible = true;
            self.reveal.snap(0.0);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOW);
            }
        }
        crate::launcher::force_foreground(self.hwnd);
        self.reveal.set(1.0);
        self.dirty = true;
    }

    pub fn hide(&mut self) {
        crate::mousehook::set_rect(4, None);
        if self.open {
            self.open = false;
            self.reveal.set(0.0);
            self.backdrop.hide();
            self.dirty = true;
        }
    }

    /// Each row's top, relative to the menu.
    fn tops(rows: &[Row]) -> Vec<f32> {
        let mut y = PAD;
        rows.iter()
            .map(|r| {
                let top = y;
                y += row_h(r);
                top
            })
            .collect()
    }

    fn height(rows: &[Row]) -> f32 {
        PAD * 2.0 + rows.iter().map(row_h).sum::<f32>()
    }

    /// The row under client point (x, y), in pixels.
    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        if !(0.0..W).contains(&lx) {
            return None;
        }
        let rows = self.nav.rows();
        let tops = Self::tops(&rows);
        rows.iter().zip(tops).position(|(r, top)| selectable(r) && ly >= top && ly < top + row_h(r))
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
        if let Some(i) = self.hit(x, y) {
            if self.nav.sel != Some(i) {
                self.nav.sel = Some(i);
                self.dirty = true;
            }
        }
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
    }

    /// A click: runs the item (or opens the power options).
    pub fn on_click(&mut self, x: i32, y: i32) -> Option<Action> {
        let i = self.hit(x, y)?;
        self.nav.sel = Some(i);
        self.dirty = true;
        self.nav.activate(i)
    }

    pub fn on_key(&mut self, vk: u16) -> Key {
        self.dirty = true;
        self.nav.on_key(vk)
    }

    pub fn wants_frame(&self) -> bool {
        self.visible && (self.dirty || !self.reveal.settled())
    }

    pub fn step(&mut self, dt: f32) -> bool {
        self.reveal.step(dt)
    }

    pub fn render(&mut self, gfx: &mut Gfx) -> Result<()> {
        self.dirty = false;
        if !self.visible {
            return Ok(());
        }
        if !self.open && self.reveal.value < 0.01 {
            self.visible = false;
            self.layer.release();
            self.backdrop.hide();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return Ok(());
        }
        let k = self.k;
        let rows = self.nav.rows();
        let tops = Self::tops(&rows);
        let h = Self::height(&rows);
        let (pw, ph) = ((W * k).ceil() as i32, (h * k).ceil() as i32);
        let b = self.bounds;
        let edge = (8.0 * k) as i32;
        let x = self.anchor.0.min(b.right - pw - edge).max(b.left + edge);
        let y = (self.anchor.1 - ph).max(b.top + edge);
        let slide = ((1.0 - self.reveal.value.clamp(0.0, 1.0)) * 10.0 * k) as i32;
        let pos = POINT { x, y: y + slide };
        if self.open {
            crate::mousehook::set_rect(4, Some(RECT { left: x, top: y, right: x + pw, bottom: y + ph }));
        }
        // Glass while it slides in and out too (placed once the frame is out).
        let glass = self.acrylic && self.reveal.value > theme::GLASS_FROM;
        let screen = RECT { left: pos.x, top: pos.y, right: pos.x + pw, bottom: pos.y + ph };
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            theme::panel(&p, 0.0, 0.0, W, h, self.radius, glass, self.opacity, 0.97, 1.0);
            for (i, (r, &top)) in rows.iter().zip(&tops).enumerate() {
                if *r == Row::Rule {
                    p.rrect(PAD + 8.0, top + RULE / 2.0, W - 2.0 * (PAD + 8.0), 1.0, 0.0, Color::white(0.10));
                    continue;
                }
                let (label, key, indent) = match r {
                    Row::Item(l, k, _) => (*l, *k, 0.0),
                    Row::PowerItem(l, k, _) => (*l, *k, INDENT),
                    _ => ("Shut down or sign out", 'U', 0.0),
                };
                if self.nav.sel == Some(i) {
                    p.rrect(PAD, top + 1.0, W - 2.0 * PAD, ROW - 2.0, 6.0, Color::white(0.09));
                }
                let tx = PAD + 10.0 + indent;
                p.text(label, &f.body, tx, top, W - tx - 52.0, ROW, Align::Left, Color::white(0.92));
                // Its access key, in a small keycap on the right.
                let (kx, kw) = (W - PAD - 8.0 - 18.0, 18.0);
                let ky = top + (ROW - kw) / 2.0;
                p.rrect(kx, ky, kw, kw, 4.0, Color::white(0.07));
                p.text(&key.to_string(), &f.small, kx, ky, kw, kw, Align::Center, Color::white(0.55));
                if *r == Row::PowerHead {
                    // A chevron: right when closed, down when open.
                    let (cx, cy, c) = (kx - 14.0, top + ROW / 2.0, Color::white(0.6));
                    if self.nav.power_open {
                        p.line(cx - 4.0, cy - 2.0, cx, cy + 2.0, c, 1.4);
                        p.line(cx, cy + 2.0, cx + 4.0, cy - 2.0, c, 1.4);
                    } else {
                        p.line(cx - 2.0, cy - 4.0, cx + 2.0, cy, c, 1.4);
                        p.line(cx + 2.0, cy, cx - 2.0, cy + 4.0, c, 1.4);
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_keys_are_unique() {
        for (laptop, power) in [(false, false), (true, true)] {
            let rows = rows(laptop, power);
            let top: Vec<char> = rows
                .iter()
                .filter_map(|r| match r {
                    Row::Item(_, k, _) => Some(*k),
                    Row::PowerHead => Some('U'),
                    _ => None,
                })
                .collect();
            let mut sorted = top.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), top.len(), "{top:?}");
        }
    }

    #[test]
    fn keyboard_walks_items_and_opens_power() {
        let mut m = Nav { sel: Some(0), ..Nav::default() };
        m.step_sel(-1);
        // Wraps round to the last row, Desktop.
        assert!(m.rows()[m.sel.unwrap()] == DESKTOP);
        assert!(matches!(m.on_key(b'U' as u16), Key::Nothing));
        assert!(m.power_open);
        assert!(matches!(m.on_key(b'R' as u16), Key::Run(Action::Power(Power::Restart))));
    }
}
