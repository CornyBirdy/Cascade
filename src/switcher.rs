//! The Alt+Tab switcher: an acrylic panel with a card per window and live
//! thumbnails, in Cascade's theme.
//!
//! The thumbnails are composited by DWM itself (DwmRegisterThumbnail) onto a
//! blurred backdrop window, so no window pixels are ever copied. A layered
//! window on top draws the cards, titles, icons and selection, with see-through
//! holes where the thumbnails are. Nothing exists, and nothing runs, until
//! Alt+Tab is pressed.

use windows::core::{Result, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::backdrop::Backdrop;
use crate::dock::{self, Dock};
use crate::gfx::{Align, Color, Gfx, Layer};
use crate::spring::Spring;
use crate::theme;

const CARD_W: f32 = 232.0;
const CARD_H: f32 = 176.0;
const TITLE_H: f32 = 38.0;
const GAP: f32 = 12.0;
const PAD: f32 = 20.0;
const MAX_ROWS: usize = 3;
/// How far a card rises while it appears, in layout units.
const RISE: f32 = 18.0;

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
        let mon_w = (self.monitor.right - self.monitor.left) as f32 / self.k;
        ((mon_w - 2.0 * PAD - 80.0 + GAP) / (CARD_W + GAP)).floor().max(1.0) as usize
    }

    /// Alt+Tab was pressed: lists the windows and picks the one to switch to.
    pub fn begin(&mut self, k: f32, monitor: RECT) -> bool {
        self.clear_thumbs();
        self.k = k;
        self.monitor = monitor;
        let own = unsafe { GetCurrentProcessId() };
        let mut all: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
        }
        // Top of the z-order first, which is the order they were last used in.
        self.items = all
            .into_iter()
            .filter(|&h| dock::is_app_window(h, own))
            .take(self.max_cols() * MAX_ROWS)
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
        if self.items.is_empty() {
            self.active = false;
            return false;
        }
        let fg = unsafe { GetForegroundWindow() };
        // The first step moves from the window you're in to the next one (or,
        // if you're not in a listed window, to the first).
        self.selected = if self.items[0].window == fg { 0 } else { self.items.len() - 1 };
        self.hovered = None;
        self.active = true;
        self.open.snap(0.0);
        let (cols, ..) = self.layout();
        let (rx, ry) = Self::card(self.selected, cols);
        self.ring_x.snap(rx);
        self.ring_y.snap(ry);
        for (i, it) in self.items.iter_mut().enumerate() {
            it.delay = (i as f32 * 0.03).min(0.15);
            it.sel.snap((i == self.selected) as u8 as f32);
        }
        true
    }

    pub fn step(&mut self, back: bool) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        self.selected = if back { (self.selected + n - 1) % n } else { (self.selected + 1) % n };
        self.dirty = true;
    }

    /// Alt was released: hides the panel and returns the window to switch to.
    pub fn commit(&mut self) -> Option<HWND> {
        let target = self.items.get(self.selected).map(|i| i.window);
        self.hide();
        target
    }

    pub fn hide(&mut self) {
        self.clear_thumbs();
        self.items.clear();
        self.active = false;
        self.shown = false;
        self.hovered = None;
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

    /// Cards in a row, rows, and the panel's size in layout units.
    fn layout(&self) -> (usize, usize, f32, f32) {
        let n = self.items.len().max(1);
        let cols = n.min(self.max_cols());
        let rows = n.div_ceil(cols).min(MAX_ROWS);
        let w = PAD * 2.0 + cols as f32 * CARD_W + (cols - 1) as f32 * GAP;
        let h = PAD * 2.0 + rows as f32 * CARD_H + (rows - 1) as f32 * GAP;
        (cols, rows, w, h)
    }

    fn card(i: usize, cols: usize) -> (f32, f32) {
        (PAD + (i % cols) as f32 * (CARD_W + GAP), PAD + (i / cols) as f32 * (CARD_H + GAP))
    }

    /// The thumbnail's box inside a card.
    fn thumb_box(cx: f32, cy: f32) -> (f32, f32, f32, f32) {
        (cx + 10.0, cy + TITLE_H, CARD_W - 20.0, CARD_H - TITLE_H - 10.0)
    }

    fn screen_pos(&self, pw: i32, ph: i32) -> (i32, i32) {
        let m = self.monitor;
        (m.left + (m.right - m.left - pw) / 2, m.top + (m.bottom - m.top - ph) / 2)
    }

    pub fn wants_frame(&self) -> bool {
        self.shown && (self.dirty || self.moving)
    }

    /// Advances the animations. Returns true while anything is moving.
    pub fn animate(&mut self, dt: f32) -> bool {
        if !self.shown {
            return false;
        }
        let (cols, ..) = self.layout();
        let (rx, ry) = Self::card(self.selected, cols);
        self.ring_x.set(rx);
        self.ring_y.set(ry);
        self.open.set(1.0);
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
        // cards while those appear (and once more when they land).
        self.thumbs_dirty = appearing || self.was_moving;
        self.was_moving = appearing;
        self.moving = moving;
        moving
    }

    /// Puts card `i`'s live thumbnail where its card is drawn right now.
    fn place_thumb(&self, i: usize) {
        let it = &self.items[i];
        if it.thumb == 0 {
            return;
        }
        let a = it.appear.value.clamp(0.0, 1.0);
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
        if self.shown || !self.active || self.items.is_empty() {
            return;
        }
        self.shown = true;
        let (cols, _, w, h) = self.layout();
        let k = self.k;
        let (pw, ph) = ((w * k).ceil() as i32, (h * k).ceil() as i32);
        let (x, y) = self.screen_pos(pw, ph);
        unsafe {
            let _ = ShowWindow(self.overlay, SW_SHOWNOACTIVATE);
        }
        self.backdrop.place(RECT { left: x, top: y, right: x + pw, bottom: y + ph }, self.overlay);
        for i in 0..self.items.len() {
            let (cx, cy) = Self::card(i, cols);
            let (tx, ty, tw, th) = Self::thumb_box(cx, cy);
            let src = self.items[i].window;
            self.items[i].appear.snap(0.0);
            let thumb = unsafe { DwmRegisterThumbnail(self.backdrop.hwnd, src) }.unwrap_or(0);
            self.items[i].thumb = thumb;
            if thumb == 0 {
                continue;
            }
            // Fit the window into the box, keeping its shape.
            let size = unsafe { DwmQueryThumbnailSourceSize(thumb) }.unwrap_or(SIZE { cx: 16, cy: 9 });
            let aspect = size.cx.max(1) as f32 / size.cy.max(1) as f32;
            let (mut fw, mut fh) = (tw, tw / aspect);
            if fh > th {
                fh = th;
                fw = th * aspect;
            }
            self.items[i].fit = (tx + (tw - fw) / 2.0, ty + (th - fh) / 2.0, fw, fh);
            self.place_thumb(i);
        }
        self.dirty = true;
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        let (cols, ..) = self.layout();
        (0..self.items.len()).find(|&i| {
            let (cx, cy) = Self::card(i, cols);
            lx >= cx && lx < cx + CARD_W && ly >= cy && ly < cy + CARD_H
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
        let k = self.k;
        let (cols, _, w, h) = self.layout();
        let (pw, ph) = ((w * k).ceil() as i32, (h * k).ceil() as i32);
        let (x, y) = self.screen_pos(pw, ph);
        let icon_px = (18.0 * k).round() as u32;
        let icons: Vec<Option<ID2D1Bitmap>> =
            self.items.iter().map(|it| it.id.as_ref().and_then(|id| dock.icon(id, gfx, icon_px))).collect();

        if self.thumbs_dirty {
            for i in 0..self.items.len() {
                self.place_thumb(i);
            }
            self.thumbs_dirty = false;
        }
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            let r = self.radius.max(6.0);
            let open = self.open.value.clamp(0.0, 1.0);
            // Without blur the backdrop itself is opaque, so only a trace on top
            // (it keeps the pixels clickable).
            theme::panel(&p, 0.0, 0.0, w, h, self.radius, self.blur, self.opacity, 0.02, open);
            for (i, it) in self.items.iter().enumerate() {
                let (cx, cy0) = Self::card(i, cols);
                let a = it.appear.value.clamp(0.0, 1.0);
                let cy = cy0 + (1.0 - it.appear.value) * RISE;
                let sel = it.sel.value.clamp(0.0, 1.0);
                let hov = it.hov.value.clamp(0.0, 1.0) * (1.0 - sel);
                let fill = 0.05 + 0.07 * sel + 0.03 * hov;
                p.rrect(cx, cy, CARD_W, CARD_H, r, Color::white(fill * a));
                p.stroke_rrect(
                    cx + 0.5,
                    cy + 0.5,
                    CARD_W - 1.0,
                    CARD_H - 1.0,
                    r,
                    Color::white(0.07 * a * (1.0 - sel)),
                    1.0,
                );
                match &icons[i] {
                    Some(bmp) => p.icon(bmp, cx + 22.0, cy + TITLE_H / 2.0, a),
                    None => p.rrect(cx + 14.0, cy + TITLE_H / 2.0 - 8.0, 16.0, 16.0, 4.0, Color::white(0.15 * a)),
                }
                let title = if it.title.is_empty() { "Window" } else { &it.title };
                p.text(
                    title,
                    &f.label,
                    cx + 40.0,
                    cy + 4.0,
                    CARD_W - 52.0,
                    TITLE_H - 8.0,
                    Align::Left,
                    Color::white(0.93 * a),
                );
                let (tx, ty, tw, th) = Self::thumb_box(cx, cy);
                p.hole(tx, ty, tw, th);
                if it.thumb == 0 {
                    // No live preview (e.g. an admin window): its icon, big.
                    p.rrect(tx, ty, tw, th, 6.0, Color::white(0.06 * a));
                    if let Some(bmp) = &icons[i] {
                        p.icon(bmp, tx + tw / 2.0, ty + th / 2.0, a);
                    }
                }
            }
            // The selection ring glides from card to card.
            if let Some(it) = self.items.get(self.selected) {
                let a = it.appear.value.clamp(0.0, 1.0);
                let oy = (1.0 - it.appear.value) * RISE;
                let (x, y) = (self.ring_x.value, self.ring_y.value + oy);
                p.stroke_rrect(x + 1.0, y + 1.0, CARD_W - 2.0, CARD_H - 2.0, r, theme::accent().alpha(a), 2.0);
            }
        }
        gfx.present(&self.layer, self.overlay, POINT { x, y }, pw, ph, 255)
    }
}
