//! The Alt+Tab switcher: an acrylic panel with a card per window and live
//! thumbnails, in Cascade's theme.
//!
//! The thumbnails are composited by DWM itself (DwmRegisterThumbnail) onto a
//! blurred backdrop window, so no window pixels are ever copied. A layered
//! window on top draws the cards, titles, icons and selection, with see-through
//! holes where the thumbnails are. Nothing exists, and nothing runs, until
//! Alt+Tab is pressed.
//!
//! Win+Tab opens it as Task View: it stays open (pick a window with a click,
//! the arrow keys and Enter, or Esc to leave), and under the windows a strip
//! of your virtual desktops to switch to, or make a new one. Drag a window's
//! card onto a desktop in the strip to send that window there.
//!
//! Alt+Tab comes in four looks (`alt_tab_style`): cards with live previews,
//! large previews (KDE's thumbnail grid), big app icons with the picked
//! window's title under them (GNOME's app switcher) and a compact list of
//! icons and titles (KDE's). Either way it slides and fades in, and out
//! again once you've picked. Arrow keys move the pick in every direction
//! while Alt is held.

use windows::core::{Result, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::backdrop::Backdrop;
use crate::desktops::Desktops;
use crate::dock::{self, Dock};
use crate::gfx::{Align, Color, Gfx, Layer};
use crate::spring::Spring;
use crate::theme;

const PAD: f32 = 20.0;
/// How far a card rises while it appears, in layout units.
const RISE: f32 = 18.0;
/// How far the whole panel slides up as it opens (and down as it closes).
const SLIDE: f32 = 14.0;
/// The picked window's title, under the icons (Icons style).
const CAPTION_H: f32 = 40.0;

/// How Alt+Tab looks (`alt_tab_style`). Task View always uses the cards.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Style {
    /// Cards with live previews (the default).
    Cards,
    /// Bigger previews, fewer to a row.
    Large,
    /// Big app icons in a row; the picked window's title under them.
    Icons,
    /// A column of icons and titles.
    List,
}

impl Style {
    pub fn parse(v: &str) -> Self {
        match v {
            "large" => Style::Large,
            "icons" => Style::Icons,
            "list" => Style::List,
            _ => Style::Cards,
        }
    }
}

/// The size of one card (or tile, or row) in a style, in layout units.
#[derive(Clone, Copy)]
struct Geom {
    w: f32,
    h: f32,
    /// The title strip at the top of a card (0: no title on the card).
    title: f32,
    gap: f32,
    /// Live previews in the cards.
    thumbs: bool,
    /// The app icon's size.
    icon: f32,
    max_rows: usize,
}
/// Task View's strip of desktops, under the cards.
const STRIP_H: f32 = 64.0;
const DESK_W: f32 = 132.0;
const DESK_H: f32 = 40.0;
const DESK_GAP: f32 = 8.0;
/// The "new desktop" button at the end of the strip.
const PLUS_W: f32 = 44.0;

/// What a click or key in Task View asks for.
pub enum ViewAction {
    Focus(HWND),
    Desktop(usize),
    /// A card dropped on a desktop in the strip: send that window there.
    MoveWindow(HWND, usize),
    NewDesktop,
    Close,
}

/// A box in Task View's strip of desktops: (x, y, width); all are DESK_H tall.
type StripBox = (f32, f32, f32);

#[derive(Clone, Copy, PartialEq)]
enum Spot {
    Card(usize),
    Desk(usize),
    Plus,
}

/// A window card picked up in Task View, on its way to a desktop in the strip.
struct Drag {
    item: usize,
    /// Where the button went down, and where the pointer is now (px).
    from: (i32, i32),
    at: (i32, i32),
    /// Past the wobble of a click: the card is carried along.
    live: bool,
    /// Esc was pressed: nothing is dropped when the button comes up.
    dead: bool,
}
/// How far (px) the pointer goes before a press on a card is a drag.
const DRAG_FROM: i32 = 6;

struct Item {
    window: HWND,
    thumb: isize,
    title: String,
    id: Option<String>,
    /// The thumbnail's box inside the card (layout units).
    fit: (f32, f32, f32, f32),
    /// 0 to 1 as the card appears (rising and fading in).
    appear: Spring,
    /// Seconds before this card starts to appear.
    delay: f32,
    /// 1 while selected / hovered, springing between 0 and 1.
    sel: Spring,
    hov: Spring,
}

pub struct Switcher {
    pub overlay: HWND,
    backdrop: Backdrop,
    layer: Layer,
    /// Alt+Tab is in use (the panel may not be showing yet).
    pub active: bool,
    shown: bool,
    items: Vec<Item>,
    selected: usize,
    hovered: Option<usize>,
    tracking: bool,
    /// The selection ring's top-left corner, sliding between cards.
    ring_x: Spring,
    ring_y: Spring,
    /// The panel's own fade-in.
    open: Spring,
    moving: bool,
    was_moving: bool,
    thumbs_dirty: bool,
    k: f32,
    monitor: RECT,
    radius: f32,
    /// Whether the backdrop blurs (otherwise it is opaque), and the Tint slider.
    blur: bool,
    opacity: f32,
    pub dirty: bool,
    /// Open as Task View (Win+Tab) rather than for Alt+Tab, with the desktops.
    pub view: bool,
    desks: Desktops,
    /// The desktop or button under the pointer in the strip.
    desk_hover: Option<Spot>,
    /// The apps with a window on each desktop (read as Task View opens).
    desk_apps: Vec<Vec<String>>,
    drag: Option<Drag>,
    style: Style,
    /// Fading out after a pick (or Esc): still drawn until `open` reaches 0.
    closing: bool,
}

/// A window of `size` fitted into the box, keeping its shape, centred in it.
fn fit_in(size: SIZE, x: f32, y: f32, w: f32, h: f32) -> (f32, f32, f32, f32) {
    let aspect = size.cx.max(1) as f32 / size.cy.max(1) as f32;
    let (mut fw, mut fh) = (w, w / aspect);
    if fh > h {
        fh = h;
        fw = h * aspect;
    }
    (x + (w - fw) / 2.0, y + (h - fh) / 2.0, fw, fh)
}

unsafe extern "system" fn collect(h: HWND, out: LPARAM) -> BOOL {
    (*(out.0 as *mut Vec<HWND>)).push(h);
    true.into()
}

impl Switcher {
    pub fn new(overlay: HWND, backdrop: Backdrop) -> Self {
        Self {
            overlay,
            backdrop,
            layer: Layer::default(),
            active: false,
            shown: false,
            items: Vec::new(),
            selected: 0,
            hovered: None,
            tracking: false,
            ring_x: Spring::new(0.0, 900.0, 46.0),
            ring_y: Spring::new(0.0, 900.0, 46.0),
            open: Spring::new(0.0, 500.0, 40.0),
            moving: false,
            was_moving: false,
            thumbs_dirty: false,
            k: 1.0,
            monitor: RECT::default(),
            radius: 8.0,
            blur: true,
            opacity: 0.55,
            dirty: false,
            view: false,
            desks: Desktops::default(),
            desk_hover: None,
            desk_apps: Vec::new(),
            drag: None,
            style: Style::Cards,
            closing: false,
        }
    }

    pub fn set_style(&mut self, style: Style) {
        if style != self.style {
            self.hide_now();
            self.style = style;
        }
    }

    fn geom(&self) -> Geom {
        let style = if self.view { Style::Cards } else { self.style };
        match style {
            Style::Cards => Geom { w: 232.0, h: 176.0, title: 38.0, gap: 12.0, thumbs: true, icon: 18.0, max_rows: 3 },
            Style::Large => Geom { w: 340.0, h: 248.0, title: 40.0, gap: 14.0, thumbs: true, icon: 20.0, max_rows: 2 },
            Style::Icons => Geom { w: 92.0, h: 92.0, title: 0.0, gap: 8.0, thumbs: false, icon: 48.0, max_rows: 3 },
            Style::List => {
                // As many rows as fit the screen, within reason.
                let mon_h = (self.monitor.bottom - self.monitor.top) as f32 / self.k;
                let rows = ((mon_h - 2.0 * PAD - 160.0) / 48.0).floor().clamp(4.0, 14.0) as usize;
                Geom { w: 440.0, h: 44.0, title: 0.0, gap: 4.0, thumbs: false, icon: 24.0, max_rows: rows }
            }
        }
    }

    pub fn configure(&mut self, acrylic: bool, opacity: f32, radius: f32) {
        self.radius = radius;
        self.opacity = opacity.clamp(0.0, 1.0);
        // The backdrop also hosts the thumbnails, so it always exists: without
        // blur it is simply opaque.
        self.blur = theme::style_backdrop(&self.backdrop, acrylic, opacity, radius);
        if !self.blur {
            theme::style_opaque(&self.backdrop, radius);
        }
    }

    fn max_cols(&self) -> usize {
        let g = self.geom();
        if !self.view && self.style == Style::List {
            return 1;
        }
        let mon_w = (self.monitor.right - self.monitor.left) as f32 / self.k;
        ((mon_w - 2.0 * PAD - 80.0 + g.gap) / (g.w + g.gap)).floor().max(1.0) as usize
    }

    /// Win+Tab: opens as Task View on `monitor`, with the windows of this
    /// desktop and the strip of `desks`, and takes the keyboard.
    pub fn begin_view(&mut self, k: f32, monitor: RECT, desks: Desktops) {
        if self.active || self.view || self.shown {
            self.hide_now();
        }
        self.view = true;
        self.list(k, monitor);
        self.desks = desks;
        self.desk_hover = None;
        self.desk_apps.clear();
        let fg = unsafe { GetForegroundWindow() };
        self.selected = self.items.iter().position(|i| i.window == fg).unwrap_or(0);
        self.start();
        unsafe {
            // It takes the keyboard while it's Task View.
            let ex = GetWindowLongW(self.overlay, GWL_EXSTYLE) as u32;
            SetWindowLongW(self.overlay, GWL_EXSTYLE, (ex & !WS_EX_NOACTIVATE.0) as i32);
        }
        self.show_now();
        crate::launcher::force_foreground(self.overlay);
    }

    /// What is open on each desktop, for the list that comes up over a desktop.
    pub fn set_desk_apps(&mut self, apps: Vec<Vec<String>>) {
        if self.view && apps != self.desk_apps {
            self.desk_apps = apps;
            self.dirty = true;
        }
    }

    /// New desktops (or a switch) while Task View is open.
    pub fn set_desktops(&mut self, desks: Desktops) {
        if self.view && desks != self.desks {
            self.desks = desks;
            self.dirty = true;
        }
    }

    /// Alt+Tab was pressed: lists the windows and picks the one to switch to.
    pub fn begin(&mut self, k: f32, monitor: RECT) -> bool {
        if self.view || self.shown {
            self.hide_now();
        }
        self.list(k, monitor);
        if self.items.is_empty() {
            self.active = false;
            return false;
        }
        let fg = unsafe { GetForegroundWindow() };
        // The first step moves from the window you're in to the next one (or,
        // if you're not in a listed window, to the first).
        self.selected = if self.items[0].window == fg { 0 } else { self.items.len() - 1 };
        self.active = true;
        self.start();
        true
    }

    /// Lists the windows, most recently used first.
    fn list(&mut self, k: f32, monitor: RECT) {
        self.clear_thumbs();
        self.k = k;
        self.monitor = monitor;
        let own = unsafe { GetCurrentProcessId() };
        let mut all: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
        }
        // Top of the z-order first, which is the order they were last used in.
        let most = self.max_cols() * self.geom().max_rows;
        self.items = all
            .into_iter()
            .filter(|&h| dock::is_app_window(h, own))
            .take(most)
            .map(|h| {
                let mut title = [0u16; 256];
                let n = unsafe { GetWindowTextW(h, &mut title) } as usize;
                Item {
                    window: h,
                    thumb: 0,
                    title: String::from_utf16_lossy(&title[..n]),
                    id: dock::window_app_id(h),
                    fit: (0.0, 0.0, 0.0, 0.0),
                    appear: Spring::new(0.0, 420.0, 27.0),
                    delay: 0.0,
                    sel: Spring::new(0.0, 700.0, 42.0),
                    hov: Spring::new(0.0, 800.0, 48.0),
                }
            })
            .collect();
    }

    /// Readies the cards' animations once the windows are listed.
    fn start(&mut self) {
        self.hovered = None;
        self.closing = false;
        self.open.snap(0.0);
        let (cols, ..) = self.layout();
        let (rx, ry) = self.card(self.selected, cols);
        self.ring_x.snap(rx);
        self.ring_y.snap(ry);
        for (i, it) in self.items.iter_mut().enumerate() {
            it.delay = (i as f32 * 0.03).min(0.15);
            it.sel.snap((i == self.selected) as u8 as f32);
        }
    }

    pub fn step(&mut self, back: bool) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        self.selected = if back { (self.selected + n - 1) % n } else { (self.selected + 1) % n };
        self.dirty = true;
    }

    /// Up or down a row (the arrow keys while Alt is held), wrapping round
    /// to the other end of the column. With a single row, up and down step
    /// back and forward instead.
    pub fn step_row(&mut self, up: bool) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let (cols, rows, ..) = self.layout();
        if rows <= 1 {
            return self.step(up);
        }
        let s = self.selected;
        self.selected = if up {
            if s >= cols {
                s - cols
            } else {
                (((n - 1) / cols) * cols + s).min(n - 1)
            }
        } else if s + cols < n {
            s + cols
        } else {
            s % cols
        };
        self.dirty = true;
    }

    /// Alt was released: hides the panel and returns the window to switch to.
    pub fn commit(&mut self) -> Option<HWND> {
        let target = self.items.get(self.selected).map(|i| i.window);
        self.hide();
        target
    }

    /// Lets go of the keyboard and fades the panel out (it's gone for good
    /// once `open` reaches 0; see `render`).
    pub fn hide(&mut self) {
        if !self.shown {
            return self.hide_now();
        }
        self.release_keys();
        self.active = false;
        self.hovered = None;
        self.desk_hover = None;
        self.drag = None;
        self.closing = true;
        self.dirty = true;
    }

    fn release_keys(&mut self) {
        if self.view {
            unsafe {
                let ex = GetWindowLongW(self.overlay, GWL_EXSTYLE) as u32;
                SetWindowLongW(self.overlay, GWL_EXSTYLE, (ex | WS_EX_NOACTIVATE.0) as i32);
            }
        }
    }

    /// Takes it off the screen at once.
    pub fn hide_now(&mut self) {
        self.release_keys();
        self.view = false;
        self.closing = false;
        self.clear_thumbs();
        self.items.clear();
        self.active = false;
        self.shown = false;
        self.hovered = None;
        self.drag = None;
        self.moving = false;
        self.was_moving = false;
        self.backdrop.hide();
        self.layer.release();
        unsafe {
            let _ = ShowWindow(self.overlay, SW_HIDE);
        }
    }

    fn clear_thumbs(&mut self) {
        for it in &mut self.items {
            if it.thumb != 0 {
                unsafe {
                    let _ = DwmUnregisterThumbnail(it.thumb);
                }
                it.thumb = 0;
            }
        }
    }

    /// Cards in a row, rows, and the panel's size in layout units (with
    /// Task View's strip of desktops under the cards).
    fn layout(&self) -> (usize, usize, f32, f32) {
        let g = self.geom();
        let n = self.items.len().max(1);
        let cols = n.min(self.max_cols());
        let rows = n.div_ceil(cols).min(g.max_rows);
        let mut w = PAD * 2.0 + cols as f32 * g.w + (cols - 1) as f32 * g.gap;
        let mut h = PAD * 2.0 + rows as f32 * g.h + (rows - 1) as f32 * g.gap;
        if !self.view && self.style == Style::Icons {
            // The picked window's title goes under the icons, and long titles
            // need more room than a short row of icons.
            h += CAPTION_H;
            w = w.max(360.0);
        }
        if self.view {
            w = w.max(self.strip_w() + 2.0 * PAD);
            h += STRIP_H;
        }
        (cols, rows, w, h)
    }

    fn strip_w(&self) -> f32 {
        self.desks.names.len() as f32 * (DESK_W + DESK_GAP) + PLUS_W
    }

    /// Each desktop's box in the strip, then the new-desktop button's.
    fn strip(&self) -> (Vec<StripBox>, StripBox) {
        let (_, _, w, h) = self.layout();
        let y = h - PAD - DESK_H;
        let mut x = (w - self.strip_w()) / 2.0;
        let mut desks = Vec::new();
        for _ in &self.desks.names {
            desks.push((x, y, DESK_W));
            x += DESK_W + DESK_GAP;
        }
        (desks, (x, y, PLUS_W))
    }

    fn spot(&self, x: i32, y: i32) -> Option<Spot> {
        if let Some(i) = self.hit(x, y) {
            return Some(Spot::Card(i));
        }
        if !self.view {
            return None;
        }
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        let inside = |(bx, by, bw): (f32, f32, f32)| lx >= bx && lx < bx + bw && ly >= by && ly < by + DESK_H;
        let (desks, plus) = self.strip();
        if let Some(i) = desks.iter().position(|&b| inside(b)) {
            return Some(Spot::Desk(i));
        }
        inside(plus).then_some(Spot::Plus)
    }

    /// Card `i`'s top-left corner (layout units), with the cards (or tiles)
    /// of a row shorter than the panel centred in it.
    fn card(&self, i: usize, cols: usize) -> (f32, f32) {
        let g = self.geom();
        let (_, _, w, _) = self.layout();
        let row_w = cols as f32 * g.w + (cols - 1) as f32 * g.gap;
        let x0 = (w - row_w) / 2.0;
        (x0 + (i % cols) as f32 * (g.w + g.gap), PAD + (i / cols) as f32 * (g.h + g.gap))
    }

    /// The thumbnail's box inside a card.
    fn thumb_box(&self, cx: f32, cy: f32) -> (f32, f32, f32, f32) {
        let g = self.geom();
        (cx + 10.0, cy + g.title, g.w - 20.0, g.h - g.title - 10.0)
    }

    fn screen_pos(&self, pw: i32, ph: i32) -> (i32, i32) {
        let m = self.monitor;
        (m.left + (m.right - m.left - pw) / 2, m.top + (m.bottom - m.top - ph) / 2)
    }

    pub fn wants_frame(&self) -> bool {
        self.shown && (self.dirty || self.moving)
    }

    /// Fading out: it no longer takes clicks or keys.
    pub fn closing(&self) -> bool {
        self.closing
    }

    /// Advances the animations. Returns true while anything is moving.
    pub fn animate(&mut self, dt: f32) -> bool {
        if !self.shown {
            return false;
        }
        let (cols, ..) = self.layout();
        let (rx, ry) = self.card(self.selected, cols);
        self.ring_x.set(rx);
        self.ring_y.set(ry);
        self.open.set(if self.closing { 0.0 } else { 1.0 });
        let mut moving = self.ring_x.step(dt);
        moving |= self.ring_y.step(dt);
        moving |= self.open.step(dt);
        let mut appearing = false;
        for (i, it) in self.items.iter_mut().enumerate() {
            if it.delay > 0.0 {
                it.delay -= dt;
                moving = true;
            } else {
                it.appear.set(1.0);
            }
            appearing |= it.appear.step(dt);
            it.sel.set((i == self.selected) as u8 as f32);
            it.hov.set((self.hovered == Some(i)) as u8 as f32);
            moving |= it.sel.step(dt);
            moving |= it.hov.step(dt);
        }
        moving |= appearing;
        // The thumbnails live in another window: move them along with the
        // cards while those appear and the panel fades (and once more when
        // they land).
        let fading = !self.open.settled();
        self.thumbs_dirty = appearing || fading || self.was_moving;
        self.was_moving = appearing || fading;
        self.moving = moving;
        moving
    }

    /// Puts card `i`'s live thumbnail where its card is drawn right now.
    fn place_thumb(&self, i: usize) {
        let it = &self.items[i];
        if it.thumb == 0 {
            return;
        }
        let a = it.appear.value.clamp(0.0, 1.0) * self.open.value.clamp(0.0, 1.0) * self.carried(i);
        let oy = (1.0 - it.appear.value) * RISE;
        let (fx, fy, fw, fh) = it.fit;
        let k = self.k;
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY,
            rcDestination: RECT {
                left: (fx * k) as i32,
                top: ((fy + oy) * k) as i32,
                right: ((fx + fw) * k) as i32,
                bottom: ((fy + fh + oy) * k) as i32,
            },
            opacity: (a * 255.0) as u8,
            fVisible: (a > 0.01).into(),
            ..Default::default()
        };
        unsafe {
            let _ = DwmUpdateThumbnailProperties(it.thumb, &props);
        }
    }

    /// Puts the panel on screen (a moment after Alt+Tab, so a quick flip
    /// between two windows never flashes it).
    pub fn show_now(&mut self) {
        if self.shown || !(self.active || self.view) || (self.items.is_empty() && !self.view) {
            return;
        }
        self.shown = true;
        let (cols, ..) = self.layout();
        // The glass comes in with the panel, in `render`.
        unsafe {
            let _ = ShowWindow(self.overlay, SW_SHOWNOACTIVATE);
        }
        let thumbs = self.geom().thumbs;
        for i in 0..self.items.len() {
            self.items[i].appear.snap(0.0);
            if !thumbs {
                continue;
            }
            let (cx, cy) = self.card(i, cols);
            let (tx, ty, tw, th) = self.thumb_box(cx, cy);
            let src = self.items[i].window;
            let thumb = unsafe { DwmRegisterThumbnail(self.backdrop.hwnd, src) }.unwrap_or(0);
            self.items[i].thumb = thumb;
            if thumb == 0 {
                continue;
            }
            // Fit the window into the box, keeping its shape.
            let size = unsafe { DwmQueryThumbnailSourceSize(thumb) }.unwrap_or(SIZE { cx: 16, cy: 9 });
            self.items[i].fit = fit_in(size, tx, ty, tw, th);
            self.place_thumb(i);
        }
        self.dirty = true;
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        let (cols, ..) = self.layout();
        let g = self.geom();
        (0..self.items.len()).find(|&i| {
            let (cx, cy) = self.card(i, cols);
            lx >= cx && lx < cx + g.w && ly >= cy && ly < cy + g.h
        })
    }

    pub fn on_move(&mut self, x: i32, y: i32) {
        unsafe {
            if !self.tracking {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.overlay,
                    dwHoverTime: 0,
                };
                self.tracking = TrackMouseEvent(&mut t).is_ok();
            }
        }
        let mut lifted = None;
        if let Some(d) = &mut self.drag {
            d.at = (x, y);
            if !d.live && (x - d.from.0).abs().max((y - d.from.1).abs()) >= DRAG_FROM {
                d.live = true;
                lifted = Some(d.item);
            }
            self.dirty |= d.live;
        }
        if let Some(i) = lifted {
            // The card's own picture fades as the card is carried off.
            self.selected = i;
            self.place_thumb(i);
        }
        // Nothing lights up under a card that is being carried.
        let carrying = self.drag.as_ref().is_some_and(|d| d.live);
        let h = if carrying { None } else { self.hit(x, y) };
        if h != self.hovered {
            self.hovered = h;
            self.dirty = true;
        }
        let spot = self.spot(x, y).filter(|s| !matches!(s, Spot::Card(_)));
        if spot != self.desk_hover {
            self.desk_hover = spot;
            self.dirty = true;
        }
    }

    /// The button went down in Task View. On a card (with a desktop to send
    /// it to) that may become a drag: true if so, and the caller should keep
    /// the mouse until it comes up.
    pub fn on_down(&mut self, x: i32, y: i32) -> bool {
        if !self.view || self.desks.names.len() < 2 {
            return false;
        }
        let Some(item) = self.hit(x, y) else { return false };
        self.drag = Some(Drag { item, from: (x, y), at: (x, y), live: false, dead: false });
        true
    }

    /// The button came up in Task View: a card dropped on a desktop sends its
    /// window there, a press that never became a drag is a click.
    pub fn on_up_view(&mut self, x: i32, y: i32) -> Option<ViewAction> {
        let Some(d) = self.drag.take() else { return self.on_click_view(x, y) };
        if !d.live {
            return if d.dead { None } else { self.on_click_view(x, y) };
        }
        self.place_thumb(d.item);
        self.dirty = true;
        if d.dead {
            return None;
        }
        match self.spot(x, y) {
            Some(Spot::Desk(i)) if i != self.desks.current => {
                self.items.get(d.item).map(|it| ViewAction::MoveWindow(it.window, i))
            }
            _ => None,
        }
    }

    /// The mouse was taken away mid-drag: the card is put back.
    pub fn cancel_drag(&mut self) {
        if let Some(d) = self.drag.take() {
            if d.live {
                self.place_thumb(d.item);
                self.dirty = true;
            }
        }
    }

    /// How much of itself card `i` shows: dimmed while it's being carried.
    fn carried(&self, i: usize) -> f32 {
        match &self.drag {
            Some(d) if d.live && !d.dead && d.item == i => 0.35,
            _ => 1.0,
        }
    }

    /// `window` has gone to another desktop: its card goes, and the rest
    /// close up.
    pub fn window_moved(&mut self, window: HWND) {
        let Some(i) = self.items.iter().position(|it| it.window == window) else { return };
        let gone = self.items.remove(i);
        if gone.thumb != 0 {
            unsafe {
                let _ = DwmUnregisterThumbnail(gone.thumb);
            }
        }
        if self.selected > i || self.selected >= self.items.len() {
            self.selected = self.selected.saturating_sub(1).min(self.items.len().saturating_sub(1));
        }
        self.hovered = None;
        let (cols, ..) = self.layout();
        for j in 0..self.items.len() {
            if self.items[j].thumb == 0 {
                continue;
            }
            let (cx, cy) = self.card(j, cols);
            let (tx, ty, tw, th) = self.thumb_box(cx, cy);
            let size = unsafe { DwmQueryThumbnailSourceSize(self.items[j].thumb) }.unwrap_or(SIZE { cx: 16, cy: 9 });
            self.items[j].fit = fit_in(size, tx, ty, tw, th);
            self.place_thumb(j);
        }
        self.dirty = true;
    }

    /// A click in Task View.
    pub fn on_click_view(&mut self, x: i32, y: i32) -> Option<ViewAction> {
        Some(match self.spot(x, y)? {
            Spot::Card(i) => ViewAction::Focus(self.items.get(i)?.window),
            Spot::Desk(i) => ViewAction::Desktop(i),
            Spot::Plus => ViewAction::NewDesktop,
        })
    }

    /// A key in Task View: arrows move between windows, Enter picks one.
    pub fn on_key_view(&mut self, vk: u16) -> Option<ViewAction> {
        use windows::Win32::UI::Input::KeyboardAndMouse::*;
        let n = self.items.len();
        let (cols, ..) = self.layout();
        let key = VIRTUAL_KEY(vk);
        if key == VK_ESCAPE {
            // Mid-drag, Esc only puts the card back.
            if let Some(d) = self.drag.as_mut().filter(|d| !d.dead) {
                d.dead = true;
                let item = d.item;
                self.place_thumb(item);
                self.dirty = true;
                return None;
            }
            return Some(ViewAction::Close);
        }
        if n == 0 {
            return None;
        }
        let s = self.selected;
        self.selected = match key {
            VK_RETURN | VK_SPACE => return self.items.get(s).map(|i| ViewAction::Focus(i.window)),
            VK_RIGHT | VK_TAB => (s + 1) % n,
            VK_LEFT => (s + n - 1) % n,
            VK_DOWN if s + cols < n => s + cols,
            VK_UP if s >= cols => s - cols,
            VK_HOME => 0,
            VK_END => n - 1,
            _ => return None,
        };
        self.dirty = true;
        None
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        self.hovered = None;
        self.desk_hover = None;
        self.dirty = true;
    }

    /// A click on a card: hides the panel and returns that window.
    pub fn on_click(&mut self, x: i32, y: i32) -> Option<HWND> {
        let i = self.hit(x, y)?;
        self.selected = i;
        self.commit()
    }

    pub fn render(&mut self, gfx: &mut Gfx, dock: &mut Dock) -> Result<()> {
        self.dirty = false;
        if !self.shown {
            return Ok(());
        }
        if self.closing && self.open.value < 0.02 {
            self.hide_now();
            return Ok(());
        }
        let k = self.k;
        let g = self.geom();
        let style = if self.view { Style::Cards } else { self.style };
        let (cols, rows, w, h) = self.layout();
        let (pw, ph) = ((w * k).ceil() as i32, (h * k).ceil() as i32);
        let (x, y) = self.screen_pos(pw, ph);
        let open = self.open.value.clamp(0.0, 1.0);
        // It rises into place as it fades in, and sinks as it fades out.
        let y = y + ((1.0 - open) * SLIDE * k).round() as i32;
        let icon_px = (g.icon * k).round() as u32;
        let icons: Vec<Option<ID2D1Bitmap>> =
            self.items.iter().map(|it| it.id.as_ref().and_then(|id| dock.icon(id, gfx, icon_px))).collect();

        if self.thumbs_dirty {
            for i in 0..self.items.len() {
                self.place_thumb(i);
            }
            self.thumbs_dirty = false;
        }
        // The backdrop (the glass, or an opaque panel without blur; it also
        // hosts the previews) follows the panel once it's shown enough to look
        // right. Before that the panel is painted solid.
        let backdrop = open > theme::GLASS_FROM;
        let glass = self.blur && backdrop;
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            let r = self.radius.max(6.0);
            let carrying = self.drag.as_ref().is_some_and(|d| d.live && !d.dead);
            // Over the opaque backdrop (no blur) only a trace on top: it keeps
            // the pixels clickable.
            let solid = if !self.blur && backdrop { 0.02 } else { 0.97 };
            theme::panel(&p, 0.0, 0.0, w, h, self.radius, glass, self.opacity, solid, 1.0);
            for (i, it) in self.items.iter().enumerate() {
                let (cx, cy0) = self.card(i, cols);
                let a = it.appear.value.clamp(0.0, 1.0) * self.carried(i);
                let cy = cy0 + (1.0 - it.appear.value) * RISE;
                let sel = it.sel.value.clamp(0.0, 1.0);
                let hov = it.hov.value.clamp(0.0, 1.0) * (1.0 - sel);
                let title = if it.title.is_empty() { "Window" } else { &it.title };
                match style {
                    Style::Cards | Style::Large => {
                        let fill = 0.05 + 0.07 * sel + 0.03 * hov;
                        p.rrect(cx, cy, g.w, g.h, r, Color::white(fill * a));
                        p.stroke_rrect(
                            cx + 0.5,
                            cy + 0.5,
                            g.w - 1.0,
                            g.h - 1.0,
                            r,
                            Color::white(0.07 * a * (1.0 - sel)),
                            1.0,
                        );
                        match &icons[i] {
                            Some(bmp) => p.icon(bmp, cx + 22.0, cy + g.title / 2.0, a),
                            None => {
                                p.rrect(cx + 14.0, cy + g.title / 2.0 - 8.0, 16.0, 16.0, 4.0, Color::white(0.15 * a))
                            }
                        }
                        let font = if style == Style::Large { &f.body } else { &f.label };
                        let c = Color::white(0.93 * a);
                        p.text(title, font, cx + 40.0, cy + 4.0, g.w - 52.0, g.title - 8.0, Align::Left, c);
                        let (tx, ty, tw, th) = self.thumb_box(cx, cy);
                        p.hole(tx, ty, tw, th);
                        if it.thumb == 0 {
                            // No live preview (e.g. an admin window): its icon, big.
                            p.rrect(tx, ty, tw, th, 6.0, Color::white(0.06 * a));
                            if let Some(bmp) = &icons[i] {
                                p.icon(bmp, tx + tw / 2.0, ty + th / 2.0, a);
                            }
                        }
                    }
                    Style::Icons => {
                        let fill = 0.10 * sel + 0.06 * hov;
                        if fill > 0.0 {
                            p.rrect(cx, cy, g.w, g.h, r + 4.0, Color::white(fill * a));
                        }
                        match &icons[i] {
                            Some(bmp) => p.icon(bmp, cx + g.w / 2.0, cy + g.h / 2.0, a),
                            None => p.rrect(cx + 22.0, cy + 22.0, 48.0, 48.0, 10.0, Color::white(0.15 * a)),
                        }
                    }
                    Style::List => {
                        let fill = 0.11 * sel + 0.05 * hov;
                        if fill > 0.0 {
                            p.rrect(cx, cy, g.w, g.h, r, Color::white(fill * a));
                        }
                        match &icons[i] {
                            Some(bmp) => p.icon(bmp, cx + 26.0, cy + g.h / 2.0, a),
                            None => p.rrect(cx + 14.0, cy + g.h / 2.0 - 12.0, 24.0, 24.0, 6.0, Color::white(0.15 * a)),
                        }
                        let c = Color::white((0.78 + 0.18 * sel) * a);
                        p.text(title, &f.body, cx + 50.0, cy, g.w - 62.0, g.h, Align::Left, c);
                    }
                }
            }
            if style == Style::Icons {
                // The picked window's title under the icons.
                if let Some(it) = self.items.get(self.selected) {
                    let title = if it.title.is_empty() { "Window" } else { &it.title };
                    let top = PAD + rows as f32 * (g.h + g.gap) - g.gap + 6.0;
                    let c = Color::white(0.93);
                    p.text(title, &f.body, PAD, top, w - 2.0 * PAD, CAPTION_H - 6.0, Align::Center, c);
                }
            }
            if self.view {
                if self.items.is_empty() {
                    let (cw, ch) = (w - 2.0 * PAD, g.h);
                    let c = Color::white(0.55);
                    p.text("No windows on this desktop", &f.body, PAD, PAD, cw, ch, Align::Center, c);
                }
                // The desktops: the one you're on lit, and a button for a new one.
                let (desks, plus) = self.strip();
                for (i, &(bx, by, bw)) in desks.iter().enumerate() {
                    let current = i == self.desks.current;
                    let hot = self.desk_hover == Some(Spot::Desk(i));
                    // A card being carried: the desktop it would land on lights up.
                    let target = hot && !current && carrying;
                    let fill = if target {
                        0.20
                    } else if current {
                        0.14
                    } else if hot {
                        0.10
                    } else {
                        0.05
                    };
                    p.rrect(bx, by, bw, DESK_H, r, Color::white(fill));
                    if current {
                        p.stroke_rrect(bx + 1.0, by + 1.0, bw - 2.0, DESK_H - 2.0, r, theme::accent(), 2.0);
                    } else if target {
                        p.stroke_rrect(bx + 1.0, by + 1.0, bw - 2.0, DESK_H - 2.0, r, theme::accent().alpha(0.9), 2.0);
                    }
                    let name = &self.desks.names[i];
                    p.text(name, &f.label, bx + 8.0, by, bw - 16.0, DESK_H, Align::Center, Color::white(0.92));
                }
                let (bx, by, bw) = plus;
                let hot = self.desk_hover == Some(Spot::Plus);
                p.rrect(bx, by, bw, DESK_H, r, Color::white(if hot { 0.12 } else { 0.06 }));
                let (cx, cy, c) = (bx + bw / 2.0, by + DESK_H / 2.0, Color::white(0.85));
                p.line(cx - 6.0, cy, cx + 6.0, cy, c, 1.6);
                p.line(cx, cy - 6.0, cx, cy + 6.0, c, 1.6);
            }
            // The selection ring glides from card to card (the list and the
            // icons show the pick by its lighter fill instead).
            if matches!(style, Style::Cards | Style::Large) {
                if let Some(it) = self.items.get(self.selected) {
                    let a = it.appear.value.clamp(0.0, 1.0);
                    let oy = (1.0 - it.appear.value) * RISE;
                    let (x, y) = (self.ring_x.value, self.ring_y.value + oy);
                    p.stroke_rrect(x + 1.0, y + 1.0, g.w - 2.0, g.h - 2.0, r, theme::accent().alpha(a), 2.0);
                }
            } else if let Some(it) = self.items.get(self.selected) {
                // A small accent mark on the picked one, so it reads at a glance.
                let a = it.appear.value.clamp(0.0, 1.0);
                let oy = (1.0 - it.appear.value) * RISE;
                let (x, y) = (self.ring_x.value, self.ring_y.value + oy);
                if style == Style::List {
                    p.rrect(x + 3.0, y + g.h * 0.25, 3.0, g.h * 0.5, 1.5, theme::accent().alpha(a));
                } else {
                    p.rrect(x + g.w * 0.35, y + g.h - 5.0, g.w * 0.3, 3.0, 1.5, theme::accent().alpha(a));
                }
            }
            // Over a desktop: the apps that are open on it.
            if let (true, Some(Spot::Desk(i))) = (self.drag.is_none(), self.desk_hover) {
                let apps = self.desk_apps.get(i).map(Vec::as_slice).unwrap_or(&[]);
                if let (Some(&(bx, by, bw)), false) = (self.strip().0.get(i), apps.is_empty()) {
                    const ROW: f32 = 22.0;
                    const MAX: usize = 6;
                    let shown = apps.len().min(MAX);
                    let rows = shown + (apps.len() > MAX) as usize;
                    let (pw, ph) = (190.0, rows as f32 * ROW + 14.0);
                    let px = (bx + bw / 2.0 - pw / 2.0).clamp(4.0, (w - pw - 4.0).max(4.0));
                    let py = by - ph - 6.0;
                    p.rrect(px + 1.0, py + 3.0, pw, ph, r, Color(0.0, 0.0, 0.0, 0.25));
                    p.rrect(px, py, pw, ph, r, Color(0.17, 0.18, 0.21, 0.97));
                    p.stroke_rrect(px + 0.5, py + 0.5, pw - 1.0, ph - 1.0, r, Color::white(0.14), 1.0);
                    for (n, name) in apps.iter().take(MAX).enumerate() {
                        let ty = py + 7.0 + n as f32 * ROW;
                        p.text(name, &f.label, px + 12.0, ty, pw - 24.0, ROW, Align::Left, Color::white(0.93));
                    }
                    if apps.len() > MAX {
                        let more = format!("+ {} more", apps.len() - MAX);
                        let ty = py + 7.0 + MAX as f32 * ROW;
                        p.text(&more, &f.label, px + 12.0, ty, pw - 24.0, ROW, Align::Left, Color::white(0.55));
                    }
                }
            }
            // The card being carried to a desktop follows the pointer.
            if let Some(d) = self.drag.as_ref().filter(|d| d.live && !d.dead) {
                if let Some(it) = self.items.get(d.item) {
                    let (gw, gh) = (184.0, 52.0);
                    let (gx, gy) = (d.at.0 as f32 / k - gw / 2.0, d.at.1 as f32 / k - gh / 2.0);
                    p.rrect(gx + 1.0, gy + 3.0, gw, gh, r, Color(0.0, 0.0, 0.0, 0.25));
                    p.rrect(gx, gy, gw, gh, r, Color(0.17, 0.18, 0.21, 0.97));
                    p.stroke_rrect(gx + 0.5, gy + 0.5, gw - 1.0, gh - 1.0, r, theme::accent().alpha(0.8), 1.5);
                    match &icons[d.item] {
                        Some(bmp) => p.icon(bmp, gx + 26.0, gy + gh / 2.0, 1.0),
                        None => p.rrect(gx + 18.0, gy + gh / 2.0 - 8.0, 16.0, 16.0, 4.0, Color::white(0.15)),
                    }
                    let title = if it.title.is_empty() { "Window" } else { &it.title };
                    p.text(title, &f.label, gx + 46.0, gy, gw - 58.0, gh, Align::Left, Color::white(0.93));
                }
            }
        }
        let alpha = (open * 255.0).round() as u8;
        let shown = gfx.present(&self.layer, self.overlay, POINT { x, y }, pw, ph, alpha);
        let screen = RECT { left: x, top: y, right: x + pw, bottom: y + ph };
        if !backdrop {
            self.backdrop.hold(screen);
        } else if self.backdrop.track(screen, None, self.overlay) {
            self.dirty = true;
        }
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_fit_their_box_keeping_their_shape() {
        // A wide window fills the width and sits in the middle of the height.
        let (x, y, w, h) = fit_in(SIZE { cx: 1600, cy: 900 }, 10.0, 20.0, 160.0, 160.0);
        assert_eq!((x, w, h), (10.0, 160.0, 90.0));
        assert!((y - 55.0).abs() < 1e-3);
        // A tall one fills the height and sits in the middle of the width.
        let (x, y, w, h) = fit_in(SIZE { cx: 450, cy: 900 }, 0.0, 0.0, 160.0, 160.0);
        assert_eq!((y, w, h), (0.0, 80.0, 160.0));
        assert!((x - 40.0).abs() < 1e-3);
    }
}
