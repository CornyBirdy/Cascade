//! The widgets panel: cards for Windows' notifications, what's playing,
//! system stats with graphs, the weather and a calendar, in the order set in Settings. It opens above
//! the taskbar's right side and closes when you click away.

use std::collections::VecDeque;
use std::time::Duration;

use windows::core::Result;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::backdrop::Backdrop;
use crate::dock::ACCENT;
use crate::gfx::{Align, Color, Fonts, Gfx, Layer, Painter};
use crate::media::MediaInfo;
use crate::notifications::{Access, Snapshot};
use crate::spring::Spring;
use crate::sysmon::{self, Sampler, HISTORY};
use crate::weather::Weather;

const CARD_W: f32 = 330.0;
const PAD: f32 = 12.0;
const GAP: f32 = 10.0;
const HEADER: f32 = 40.0;
const GREEN: Color = Color(0.40, 0.85, 0.50, 1.0);
const ORANGE: Color = Color(1.0, 0.65, 0.30, 1.0);
const NOTIF_ROW: f32 = 52.0;
/// Notifications listed at once (the newest).
const NOTIF_MAX: usize = 5;

#[derive(Clone, Copy, PartialEq)]
enum Card {
    Notifications,
    Media,
    System,
    Weather,
    Calendar,
}

/// "media,system" -> the cards, in that order (unknown names and repeats ignored).
fn parse_cards(list: &str) -> Vec<Card> {
    let mut out = Vec::new();
    for name in list.split(',') {
        let card = match name.trim().to_ascii_lowercase().as_str() {
            "notifications" | "notification" => Card::Notifications,
            "media" | "music" => Card::Media,
            "system" | "stats" => Card::System,
            "weather" => Card::Weather,
            "calendar" => Card::Calendar,
            _ => continue,
        };
        if !out.contains(&card) {
            out.push(card);
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Prev,
    Play,
    Next,
    Gear,
    NotifClear,
    NotifRemove(u32),
    NotifSettings,
}

pub enum Action {
    Previous,
    PlayPause,
    Next,
    Customise,
    ClearNotifications,
    RemoveNotification(u32),
    NotificationSettings,
}

/// What the cards show.
pub struct Ctx<'a> {
    pub sampler: &'a Sampler,
    pub media: Option<&'a MediaInfo>,
    pub art: Option<ID2D1Bitmap>,
    pub weather: Option<&'a Weather>,
    pub notifs: &'a Snapshot,
}

pub struct Widgets {
    pub hwnd: HWND,
    layer: Layer,
    backdrop: Backdrop,
    pub open: bool,
    visible: bool,
    reveal: Spring,
    cards: Vec<Card>,
    columns: usize,
    /// Tallest the panel may be (layout units) before it goes to two columns.
    max_h: f32,
    hovered: Option<Target>,
    tracking: bool,
    hits: Vec<(Target, f32, f32, f32, f32)>,
    /// Screen x of the panel's right edge, and y of its bottom edge.
    anchor: (i32, i32),
    k: f32,
    acrylic: bool,
    radius: f32,
    pub dirty: bool,
}

fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

fn days_in_month(year: u16, month: u16) -> u16 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400) => 29,
        2 => 28,
        _ => 31,
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A filled line graph of `hist` filling the box, newest on the right.
fn spark(p: &Painter, hist: &VecDeque<f32>, x: f32, y: f32, w: f32, h: f32, max: f32, c: Color) {
    if hist.len() < 2 {
        return;
    }
    let step = w / (HISTORY - 1) as f32;
    let skip = HISTORY - hist.len();
    let pts: Vec<(f32, f32)> = hist
        .iter()
        .enumerate()
        .map(|(i, &v)| (x + (skip + i) as f32 * step, y + h - (v / max).clamp(0.0, 1.0) * h))
        .collect();
    let mut area = Vec::with_capacity(pts.len() + 2);
    area.push((pts[0].0, y + h));
    area.extend_from_slice(&pts);
    area.push((pts[pts.len() - 1].0, y + h));
    p.polygon(&area, c.alpha(0.18));
    for w in pts.windows(2) {
        p.line(w[0].0, w[0].1, w[1].0, w[1].1, c, 1.3);
    }
}

fn card_height(card: Card, ctx: &Ctx) -> f32 {
    match card {
        Card::Notifications => {
            let rows = match ctx.notifs.access {
                Access::Allowed => ctx.notifs.items.len().clamp(1, NOTIF_MAX),
                _ => 1,
            };
            34.0 + rows as f32 * NOTIF_ROW + 6.0
        }
        Card::Media => 128.0,
        Card::System => {
            let s = &ctx.sampler.stats;
            let rows = 3 + s.gpu.is_some() as usize + s.disk.is_some() as usize;
            34.0 + rows as f32 * 44.0 + 6.0
        }
        Card::Weather => 112.0,
        Card::Calendar => 256.0,
    }
}

/// Each card's place (card, x, y, height) and the panel's size.
struct Layout {
    cards: Vec<(Card, f32, f32, f32)>,
    w: f32,
    h: f32,
}

impl Widgets {
    pub fn new(hwnd: HWND, backdrop: Backdrop) -> Self {
        Self {
            hwnd,
            layer: Layer::default(),
            backdrop,
            open: false,
            visible: false,
            reveal: Spring::new(0.0, 600.0, 44.0),
            cards: Vec::new(),
            columns: 1,
            max_h: f32::MAX,
            hovered: None,
            tracking: false,
            hits: Vec::new(),
            anchor: (0, 0),
            k: 1.0,
            acrylic: true,
            radius: 8.0,
            dirty: false,
        }
    }

    pub fn configure(&mut self, acrylic: bool, opacity: f32, radius: f32) {
        self.acrylic = acrylic;
        self.radius = radius;
        let tint = ((opacity.clamp(0.0, 1.0) * 255.0) as u32) << 24 | 0x00_1C1C1E;
        self.backdrop.style(acrylic, tint, radius);
        self.dirty = true;
    }

    /// Opens with its right edge at screen x `right`, just above `bottom`,
    /// at most `max_h` screen pixels tall (two columns if one is too tall).
    /// `notifications` puts the Notifications card first.
    pub fn show(
        &mut self,
        cards: &str,
        notifications: bool,
        columns: u32,
        right: i32,
        bottom: i32,
        max_h: i32,
        k: f32,
    ) {
        self.cards = parse_cards(cards);
        self.cards.retain(|&c| c != Card::Notifications);
        if notifications {
            self.cards.insert(0, Card::Notifications);
        }
        self.columns = columns.clamp(1, 2) as usize;
        self.max_h = max_h as f32 / k;
        self.anchor = (right, bottom);
        self.k = k;
        self.open = true;
        self.hovered = None;
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
        crate::mousehook::set_rect(2, None);
        if self.open {
            self.open = false;
            self.reveal.set(0.0);
            self.backdrop.hide();
            self.dirty = true;
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

    /// Forgets the hovered button (its item just went away).
    pub fn hovered_clear(&mut self) {
        self.hovered = None;
        self.dirty = true;
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        self.hovered = None;
        self.dirty = true;
    }

    pub fn on_click(&mut self, x: i32, y: i32) -> Option<Action> {
        Some(match self.hit(x, y)? {
            Target::Prev => Action::Previous,
            Target::Play => Action::PlayPause,
            Target::Next => Action::Next,
            Target::Gear => Action::Customise,
            Target::NotifClear => Action::ClearNotifications,
            Target::NotifRemove(id) => Action::RemoveNotification(id),
            Target::NotifSettings => Action::NotificationSettings,
        })
    }

    pub fn wants_frame(&self) -> bool {
        self.visible && (self.dirty || !self.reveal.settled())
    }

    pub fn step(&mut self, dt: f32) -> bool {
        self.reveal.step(dt)
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        let one = self.layout_in(ctx, self.columns.max(1));
        if one.h > self.max_h && self.columns == 1 && self.cards.len() > 1 {
            return self.layout_in(ctx, 2);
        }
        one
    }

    fn layout_in(&self, ctx: &Ctx, cols: usize) -> Layout {
        let mut col_y = vec![HEADER; cols];
        let mut out = Vec::new();
        for &card in &self.cards {
            // The shorter column takes the next card.
            let c = (0..cols).min_by(|&a, &b| col_y[a].total_cmp(&col_y[b])).unwrap_or(0);
            let h = card_height(card, ctx);
            out.push((card, PAD + c as f32 * (CARD_W + GAP), col_y[c], h));
            col_y[c] += h + GAP;
        }
        let w = PAD * 2.0 + cols as f32 * CARD_W + (cols - 1) as f32 * GAP;
        let tallest = col_y.iter().copied().fold(HEADER, f32::max);
        let h = if self.cards.is_empty() { HEADER + 50.0 } else { tallest - GAP + PAD };
        Layout { cards: out, w, h }
    }

    pub fn render(&mut self, gfx: &mut Gfx, ctx: &Ctx) -> Result<()> {
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
        let Layout { cards, w, h } = self.layout(ctx);
        let (pw, ph) = ((w * k).ceil() as i32, (h * k).ceil() as i32);
        let slide = ((1.0 - self.reveal.value.clamp(0.0, 1.0)) * 16.0 * k) as i32;
        let (x, y) = (self.anchor.0 - pw, self.anchor.1 - ph);
        let pos = POINT { x, y: y + slide };
        if self.open {
            crate::mousehook::set_rect(2, Some(RECT { left: x, top: y, right: x + pw, bottom: y + ph }));
        }
        let settled = self.open && self.acrylic && self.reveal.value > 0.97;
        if settled {
            self.backdrop.place(RECT { left: x, top: y, right: x + pw, bottom: y + ph }, self.hwnd);
        } else {
            self.backdrop.hide();
        }

        self.hits.clear();
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            let fill = if settled { Color::white(0.015) } else { Color(0.13, 0.13, 0.14, 0.97) };
            p.rrect(0.0, 0.0, w, h, self.radius, fill);
            p.stroke_rrect(0.5, 0.5, w - 1.0, h - 1.0, self.radius, Color::white(0.10), 1.0);

            // Header with the customise button.
            p.text("Widgets", &f.title, PAD + 4.0, 0.0, 200.0, HEADER, Align::Left, Color::white(0.95));
            let (gx, gy) = (w - PAD - 16.0, HEADER / 2.0);
            if self.hovered == Some(Target::Gear) {
                p.circle(gx, gy, 15.0, Color::white(0.10));
            }
            let c = Color::white(0.8);
            p.stroke_circle(gx, gy, 3.0, c, 1.3);
            for j in 0..8 {
                let a = j as f32 * std::f32::consts::FRAC_PI_4;
                p.line(gx + a.cos() * 6.0, gy + a.sin() * 6.0, gx + a.cos() * 8.0, gy + a.sin() * 8.0, c, 2.0);
            }
            self.hits.push((Target::Gear, gx - 15.0, gy - 15.0, 30.0, 30.0));

            if cards.is_empty() {
                let hint = "No cards. Add some in Settings > Widgets.";
                p.text(hint, &f.small, PAD + 4.0, HEADER, w - 2.0 * PAD, 40.0, Align::Left, Color::white(0.5));
            }
            for (card, cx, cy, ch) in cards {
                p.rrect(cx, cy, CARD_W, ch, 10.0, Color::white(0.05));
                p.stroke_rrect(cx + 0.5, cy + 0.5, CARD_W - 1.0, ch - 1.0, 10.0, Color::white(0.07), 1.0);
                match card {
                    Card::Notifications => self.notifications_card(&p, f, ctx, cx, cy),
                    Card::Media => self.media_card(&p, f, ctx, cx, cy),
                    Card::System => system_card(&p, f, ctx, cx, cy),
                    Card::Weather => weather_card(&p, f, ctx, cx, cy),
                    Card::Calendar => calendar_card(&p, f, cx, cy),
                }
            }
        }
        let alpha = (self.reveal.value.clamp(0.0, 1.0) * 255.0) as u8;
        gfx.present(&self.layer, self.hwnd, pos, pw, ph, alpha)
    }

    fn notifications_card(&mut self, p: &Painter, f: &Fonts, ctx: &Ctx, x: f32, y: f32) {
        let n = ctx.notifs;
        p.text("Notifications", &f.label, x + 14.0, y + 6.0, 200.0, 26.0, Align::Left, Color::white(0.9));
        let top = y + 34.0;
        match n.access {
            Access::Unknown => {
                p.text("Loading\u{2026}", &f.body, x, top, CARD_W, NOTIF_ROW, Align::Center, Color::white(0.5));
            }
            Access::Denied => {
                let msg = "Cascade isn't allowed to read notifications.";
                p.text(msg, &f.small, x + 14.0, top, CARD_W - 28.0, 18.0, Align::Left, Color::white(0.6));
                let (bx, by, bw, bh) = (x + 14.0, top + 22.0, 170.0, 24.0);
                let a = if self.hovered == Some(Target::NotifSettings) { 0.16 } else { 0.09 };
                p.rrect(bx, by, bw, bh, 6.0, Color::white(a));
                p.text("Open privacy settings", &f.small, bx, by, bw, bh, Align::Center, Color::white(0.9));
                self.hits.push((Target::NotifSettings, bx, by, bw, bh));
            }
            Access::Allowed if n.items.is_empty() => {
                p.text("No new notifications", &f.body, x, top, CARD_W, NOTIF_ROW, Align::Center, Color::white(0.5));
            }
            Access::Allowed => {
                let (cw, ch) = (70.0, 22.0);
                let (bx, by) = (x + CARD_W - 10.0 - cw, y + 8.0);
                if self.hovered == Some(Target::NotifClear) {
                    p.rrect(bx, by, cw, ch, 6.0, Color::white(0.10));
                }
                p.text("Clear all", &f.small, bx, by, cw, ch, Align::Center, Color::white(0.75));
                self.hits.push((Target::NotifClear, bx, by, cw, ch));
                let tw = CARD_W - 28.0 - 30.0;
                for (i, item) in n.items.iter().take(NOTIF_MAX).enumerate() {
                    let ry = top + i as f32 * NOTIF_ROW;
                    if i > 0 {
                        p.rrect(x + 14.0, ry, CARD_W - 28.0, 1.0, 0.0, Color::white(0.06));
                    }
                    p.text(&item.app, &f.small, x + 14.0, ry + 4.0, tw, 15.0, Align::Left, Color::white(0.5));
                    p.text(&item.title, &f.label, x + 14.0, ry + 18.0, tw, 16.0, Align::Left, Color::white(0.95));
                    p.text(&item.body, &f.small, x + 14.0, ry + 34.0, tw, 15.0, Align::Left, Color::white(0.65));
                    // Dismiss.
                    let t = Target::NotifRemove(item.id);
                    let (cx, cy) = (x + CARD_W - 26.0, ry + NOTIF_ROW / 2.0);
                    if self.hovered == Some(t) {
                        p.circle(cx, cy, 11.0, Color::white(0.10));
                    }
                    let c = Color::white(0.7);
                    p.line(cx - 4.0, cy - 4.0, cx + 4.0, cy + 4.0, c, 1.3);
                    p.line(cx - 4.0, cy + 4.0, cx + 4.0, cy - 4.0, c, 1.3);
                    self.hits.push((t, cx - 12.0, cy - 12.0, 24.0, 24.0));
                }
            }
        }
    }

    fn media_card(&mut self, p: &Painter, f: &Fonts, ctx: &Ctx, x: f32, y: f32) {
        let Some(m) = ctx.media else {
            p.text("Nothing playing", &f.body, x, y, CARD_W, 128.0, Align::Center, Color::white(0.5));
            return;
        };
        let art = 76.0;
        match &ctx.art {
            Some(b) => p.image(b, x + 14.0, y + 14.0, art, art, 1.0),
            None => p.rrect(x + 14.0, y + 14.0, art, art, 10.0, Color::white(0.10)),
        }
        let tx = x + 14.0 + art + 14.0;
        let tw = CARD_W - (tx - x) - 14.0;
        p.text(&m.title, &f.title, tx, y + 12.0, tw, 24.0, Align::Left, Color::white(0.95));
        p.text(&m.artist, &f.body, tx, y + 36.0, tw, 20.0, Align::Left, Color::white(0.6));
        // Progress.
        let (pos, dur) = (m.position_now(), m.duration);
        let by = y + 66.0;
        p.rrect(tx, by, tw, 4.0, 2.0, Color::white(0.15));
        if dur > Duration::ZERO {
            let t = (pos.as_secs_f32() / dur.as_secs_f32()).clamp(0.0, 1.0);
            p.rrect(tx, by, (tw * t).max(4.0), 4.0, 2.0, ACCENT);
            p.text(&fmt_time(pos), &f.small, tx, by + 6.0, 60.0, 14.0, Align::Left, Color::white(0.5));
            p.text(&fmt_time(dur), &f.small, tx + tw - 60.0, by + 6.0, 60.0, 14.0, Align::Right, Color::white(0.5));
        }
        // Controls.
        let cy = y + 108.0;
        let mid = tx + tw / 2.0;
        for (t, dx) in [(Target::Prev, -48.0), (Target::Play, 0.0), (Target::Next, 48.0)] {
            let cx = mid + dx;
            if self.hovered == Some(t) {
                p.circle(cx, cy, 15.0, Color::white(0.10));
            }
            let c = Color::white(0.92);
            match t {
                Target::Prev => {
                    p.polygon(&[(cx + 5.0, cy - 6.0), (cx + 5.0, cy + 6.0), (cx - 4.0, cy)], c);
                    p.rrect(cx - 7.0, cy - 6.0, 2.0, 12.0, 1.0, c);
                }
                Target::Next => {
                    p.polygon(&[(cx - 5.0, cy - 6.0), (cx - 5.0, cy + 6.0), (cx + 4.0, cy)], c);
                    p.rrect(cx + 5.0, cy - 6.0, 2.0, 12.0, 1.0, c);
                }
                _ if m.playing => {
                    p.rrect(cx - 5.5, cy - 7.0, 4.0, 14.0, 1.2, c);
                    p.rrect(cx + 1.5, cy - 7.0, 4.0, 14.0, 1.2, c);
                }
                _ => p.polygon(&[(cx - 5.0, cy - 8.0), (cx - 5.0, cy + 8.0), (cx + 7.0, cy)], c),
            }
            self.hits.push((t, cx - 15.0, cy - 15.0, 30.0, 30.0));
        }
    }
}

fn system_card(p: &Painter, f: &Fonts, ctx: &Ctx, x: f32, y: f32) {
    let (s, h) = (&ctx.sampler.stats, &ctx.sampler.hist);
    p.text("System", &f.label, x + 14.0, y + 6.0, 200.0, 26.0, Align::Left, Color::white(0.9));
    let net_max = h.down.iter().copied().fold(100.0 * 1024.0, f32::max);
    let mut rows: Vec<(&str, String, &VecDeque<f32>, f32, Color)> = vec![
        ("CPU", format!("{:.0}%", s.cpu), &h.cpu, 100.0, ACCENT),
        (
            "Memory",
            format!("{:.1} / {:.0} GB   {:.0}%", s.ram_used, s.ram_total, s.ram),
            &h.ram,
            100.0,
            Color(0.70, 0.55, 1.0, 1.0),
        ),
    ];
    if let Some(g) = s.gpu {
        rows.push(("GPU", format!("{g:.0}%"), &h.gpu, 100.0, GREEN));
    }
    if let Some(d) = s.disk {
        rows.push(("Disk", format!("{d:.0}%"), &h.disk, 100.0, ORANGE));
    }
    rows.push((
        "Network",
        format!("\u{2193} {}   \u{2191} {}", sysmon::speed(s.down), sysmon::speed(s.up)),
        &h.down,
        net_max,
        Color(0.40, 0.80, 0.90, 1.0),
    ));
    let w = CARD_W - 28.0;
    for (i, (label, value, hist, max, color)) in rows.into_iter().enumerate() {
        let ry = y + 34.0 + i as f32 * 44.0;
        p.text(label, &f.small, x + 14.0, ry, 120.0, 16.0, Align::Left, Color::white(0.6));
        p.text(&value, &f.label, x + 14.0, ry, w, 16.0, Align::Right, Color::white(0.95));
        spark(p, hist, x + 14.0, ry + 20.0, w, 20.0, max, color);
    }
}

fn weather_card(p: &Painter, f: &Fonts, ctx: &Ctx, x: f32, y: f32) {
    let Some(wx) = ctx.weather else {
        p.text("Weather isn't available", &f.body, x, y, CARD_W, 112.0, Align::Center, Color::white(0.5));
        return;
    };
    crate::ui::weather_icon(p, x + 44.0, y + 50.0, 56.0, wx.code, wx.day, 1.0);
    let unit = if wx.fahrenheit { "F" } else { "C" };
    p.text(
        &format!("{:.0}\u{b0}{unit}", wx.temp),
        &f.big,
        x + 90.0,
        y + 12.0,
        130.0,
        48.0,
        Align::Left,
        Color::white(0.95),
    );
    p.text(wx.describe(), &f.body, x + 90.0, y + 60.0, 220.0, 20.0, Align::Left, Color::white(0.7));
    let hl = format!("High {:.0}\u{b0}   Low {:.0}\u{b0}", wx.high, wx.low);
    p.text(&hl, &f.small, x + 90.0, y + 82.0, 220.0, 18.0, Align::Left, Color::white(0.55));
    p.text(&wx.place, &f.small, x + 14.0, y + 8.0, CARD_W - 28.0, 16.0, Align::Right, Color::white(0.55));
}

fn calendar_card(p: &Painter, f: &Fonts, x: f32, y: f32) {
    let now = unsafe { GetLocalTime() };
    let title = format!("{} {}", MONTHS[(now.wMonth.clamp(1, 12) - 1) as usize], now.wYear);
    p.text(&title, &f.label, x + 14.0, y + 6.0, CARD_W - 28.0, 26.0, Align::Left, Color::white(0.9));
    // Weeks start on Monday.
    let today = (now.wDayOfWeek as i32 + 6) % 7;
    let first = (today - (now.wDay as i32 - 1)).rem_euclid(7);
    let (cell, top) = ((CARD_W - 28.0) / 7.0, y + 36.0);
    for (i, d) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
        p.text(d, &f.small, x + 14.0 + i as f32 * cell, top, cell, 20.0, Align::Center, Color::white(0.45));
    }
    for day in 1..=days_in_month(now.wYear, now.wMonth) {
        let slot = first + day as i32 - 1;
        let (col, row) = ((slot % 7) as f32, (slot / 7) as f32);
        let (cx, cy) = (x + 14.0 + col * cell + cell / 2.0, top + 30.0 + row * 31.0);
        let is_today = day == now.wDay;
        if is_today {
            p.circle(cx, cy, 13.0, ACCENT);
        }
        let c = if is_today { Color::white(1.0) } else { Color::white(0.85) };
        p.text(&day.to_string(), &f.body, cx - cell / 2.0, cy - 12.0, cell, 24.0, Align::Center, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_card_list() {
        assert!(parse_cards("media, system,Weather,bogus,media") == [Card::Media, Card::System, Card::Weather]);
        assert!(parse_cards("").is_empty());
        assert!(parse_cards("calendar,notifications") == [Card::Calendar, Card::Notifications]);
    }

    #[test]
    fn month_lengths() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2026, 10), 31);
    }
}
