//! The widgets panel: cards for Windows' notifications, what's playing,
//! system stats with graphs, the weather and a calendar, in the order set in Settings. It opens above
//! the taskbar's right side and closes when you click away.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::audio::BANDS;
use crate::backdrop::Backdrop;
use crate::clipboard::{self, Clips};
use crate::gfx::{Align, Color, Fonts, Gfx, Layer, Painter};
use crate::ics::{self, Event, DAY};
use crate::media::MediaInfo;
use crate::notifications::{Access, Snapshot};
use crate::spring::Spring;
use crate::sysmon::{self, Readings, HISTORY};
use crate::theme;
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
/// Clipboard items listed at once (the newest), and a row's height.
const CLIP_MAX: usize = 5;
const CLIP_ROW: f32 = 30.0;

#[derive(Clone, Copy, PartialEq)]
enum Card {
    Notifications,
    Media,
    System,
    Weather,
    Calendar,
    /// Windows' clipboard history: click an item to copy it again.
    Clipboard,
    /// Your Android phone through Phone Link: what it's notified you of, and
    /// a way into Phone Link.
    Phone,
}

/// How many of the phone's notifications the Phone card lists.
const PHONE_MAX: usize = 3;

/// Whether a notification came from your phone, through Phone Link.
pub fn from_phone(aumid: &str) -> bool {
    crate::media::is_phone(aumid)
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
            "clipboard" => Card::Clipboard,
            "phone" => Card::Phone,
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
    NotifOpen(u32),
    NotifRemove(u32),
    NotifSettings,
    OpenPhoneLink,
    /// A clipboard item (by its place in the list), and the card's Clear.
    ClipItem(usize),
    ClipClear,
    /// The System card (opens Task Manager).
    System,
    /// The calendar card's month arrows (-1, +1), its title (back to this
    /// month) and a day (days since 1970).
    CalMonth(i32),
    CalToday,
    CalDay(i64),
}

pub enum Action {
    Previous,
    PlayPause,
    Next,
    Customise,
    ClearNotifications,
    /// A notification was clicked: open its app.
    OpenNotification(u32),
    RemoveNotification(u32),
    NotificationSettings,
    OpenPhoneLink,
    TaskManager,
    /// A clipboard item was clicked: copy it again (by its place in the list).
    CopyClip(usize),
    ClearClips,
}

/// What the cards show.
pub struct Ctx<'a> {
    /// The music visualizer's bands and colour, if the music card shows it.
    pub bars: Option<([f32; BANDS], Color)>,
    pub sampler: &'a Readings,
    pub media: Option<&'a MediaInfo>,
    pub art: Option<ID2D1Bitmap>,
    pub weather: Option<&'a Weather>,
    pub notifs: &'a Snapshot,
    /// Your calendar's events (see `calendar`).
    pub events: &'a [Event],
    /// The clipboard history (see `clipboard`).
    pub clips: &'a Clips,
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
    /// When the panel last opened, and whether it had gone to two columns by
    /// itself (one would have been too tall) once its data was in.
    opened_at: Instant,
    last_wide: bool,
    /// How tall the panel was in one column, the last time its data had
    /// come in: what the next opening goes by until its own data arrives.
    known_h: f32,
    /// The clipboard item that was just copied again.
    copied: Option<String>,
    hovered: Option<Target>,
    tracking: bool,
    hits: Vec<(Target, f32, f32, f32, f32)>,
    /// Screen x of the panel's right edge, and y of its bottom edge.
    anchor: (i32, i32),
    k: f32,
    acrylic: bool,
    /// The Tint slider.
    opacity: f32,
    radius: f32,
    pub dirty: bool,
    /// The calendar card's month, as months from this one, and the day
    /// picked on it (None: today).
    cal_month: i32,
    cal_day: Option<i64>,
}

/// How long after opening the cards' data is still arriving.
const SETTLE: Duration = Duration::from_millis(1500);

/// The calendar card's events listed for a day at most.
const CAL_EVENTS: usize = 4;
const CAL_ROW: f32 = 22.0;

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

fn card_height(card: Card, ctx: &Ctx, cal_rows: usize) -> f32 {
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
            let gpus = match ctx.sampler.gpus.len() {
                0 => s.gpu.is_some() as usize,
                n => n,
            };
            let rows = 3 + gpus + s.disk.is_some() as usize;
            34.0 + rows as f32 * 44.0 + 6.0
        }
        Card::Clipboard => {
            let rows = match ctx.clips.state {
                clipboard::State::Ready => ctx.clips.items.len().clamp(1, CLIP_MAX),
                _ => 1,
            };
            34.0 + rows as f32 * CLIP_ROW + 6.0
        }
        Card::Weather => 112.0,
        Card::Calendar => 256.0 + if cal_rows > 0 { 12.0 + cal_rows as f32 * CAL_ROW } else { 0.0 },
        Card::Phone => {
            let rows = phone_items(ctx).count().clamp(1, PHONE_MAX);
            34.0 + rows as f32 * NOTIF_ROW + 40.0
        }
    }
}

/// The notifications your phone sent through Phone Link, newest first.
fn phone_items<'a>(ctx: &'a Ctx) -> impl Iterator<Item = &'a crate::notifications::Notif> {
    ctx.notifs.items.iter().filter(|n| from_phone(&n.aumid)).take(PHONE_MAX)
}

/// Each card's place (card, x, y, height) and the panel's size.
struct Layout {
    cards: Vec<(Card, f32, f32, f32)>,
    w: f32,
    h: f32,
    /// How tall it would be in one column.
    one_h: f32,
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
            opened_at: Instant::now(),
            last_wide: false,
            known_h: 0.0,
            copied: None,
            hovered: None,
            tracking: false,
            hits: Vec::new(),
            anchor: (0, 0),
            k: 1.0,
            acrylic: true,
            opacity: 0.55,
            radius: 8.0,
            dirty: false,
            cal_month: 0,
            cal_day: None,
        }
    }

    pub fn configure(&mut self, acrylic: bool, opacity: f32, radius: f32) {
        self.acrylic = theme::style_backdrop(&self.backdrop, acrylic, opacity, radius);
        self.opacity = opacity.clamp(0.0, 1.0);
        self.radius = radius;
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
        self.copied = None;
        if !self.visible {
            // Opened afresh: back to today on the calendar.
            self.cal_month = 0;
            self.cal_day = None;
            self.opened_at = Instant::now();
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
        // The calendar card works by itself.
        match self.hit(x, y)? {
            Target::CalMonth(d) => {
                self.cal_month += d;
                self.cal_day = None;
                self.dirty = true;
                return None;
            }
            Target::CalToday => {
                self.cal_month = 0;
                self.cal_day = None;
                self.dirty = true;
                return None;
            }
            Target::CalDay(d) => {
                // A second click on the picked day goes back to today.
                self.cal_day = if self.cal_day == Some(d) { None } else { Some(d) };
                self.dirty = true;
                return None;
            }
            _ => {}
        }
        Some(match self.hit(x, y)? {
            Target::Prev => Action::Previous,
            Target::Play => Action::PlayPause,
            Target::Next => Action::Next,
            Target::Gear => Action::Customise,
            Target::NotifClear => Action::ClearNotifications,
            Target::NotifOpen(id) => Action::OpenNotification(id),
            Target::NotifRemove(id) => Action::RemoveNotification(id),
            Target::NotifSettings => Action::NotificationSettings,
            Target::OpenPhoneLink => Action::OpenPhoneLink,
            Target::ClipItem(i) => Action::CopyClip(i),
            Target::ClipClear => Action::ClearClips,
            Target::System => Action::TaskManager,
            Target::CalMonth(_) | Target::CalToday | Target::CalDay(_) => return None,
        })
    }

    /// Something changed that the panel shows.
    pub fn dirty(&mut self) {
        self.dirty = true;
    }

    pub fn wants_frame(&self) -> bool {
        self.visible && (self.dirty || !self.reveal.settled())
    }

    pub fn step(&mut self, dt: f32) -> bool {
        self.reveal.step(dt)
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        let mut one = self.layout_in(ctx, self.columns.max(1));
        one.one_h = one.h;
        if self.columns == 1 && self.cards.len() > 1 {
            // Its stats and notifications arrive a moment after it opens and
            // make the cards taller (before then they're whatever was read
            // last, which can be long ago). So until they're in, it goes by
            // how tall it was the last time they were: opening narrow and
            // snapping wide a moment later is what that avoids. It only goes
            // back to one column once that clearly fits.
            let need = if self.settling() { one.h.max(self.known_h) } else { one.h };
            let carry = self.last_wide && need > self.max_h * 0.8;
            if need > self.max_h || carry {
                let mut two = self.layout_in(ctx, 2);
                two.one_h = one.h;
                return two;
            }
        }
        one
    }

    /// Still within the first moments after opening, when data is on its way.
    fn settling(&self) -> bool {
        self.opened_at.elapsed() < SETTLE
    }

    fn layout_in(&self, ctx: &Ctx, cols: usize) -> Layout {
        let mut col_y = vec![HEADER; cols];
        let mut out = Vec::new();
        for &card in &self.cards {
            // The shorter column takes the next card.
            let c = (0..cols).min_by(|&a, &b| col_y[a].total_cmp(&col_y[b])).unwrap_or(0);
            let rows = if card == Card::Calendar { self.cal_rows(ctx) } else { 0 };
            let h = card_height(card, ctx, rows);
            out.push((card, PAD + c as f32 * (CARD_W + GAP), col_y[c], h));
            col_y[c] += h + GAP;
        }
        let w = PAD * 2.0 + cols as f32 * CARD_W + (cols - 1) as f32 * GAP;
        let tallest = col_y.iter().copied().fold(HEADER, f32::max);
        let h = if self.cards.is_empty() { HEADER + 50.0 } else { tallest - GAP + PAD };
        Layout { cards: out, w, h, one_h: h }
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
        let Layout { cards, w, h, one_h } = self.layout(ctx);
        if self.open && !self.settling() {
            self.last_wide = self.columns == 1 && w > PAD * 2.0 + CARD_W + 1.0;
            self.known_h = one_h;
        }
        let (pw, ph) = ((w * k).ceil() as i32, (h * k).ceil() as i32);
        let slide = ((1.0 - self.reveal.value.clamp(0.0, 1.0)) * 16.0 * k) as i32;
        let (x, y) = (self.anchor.0 - pw, self.anchor.1 - ph);
        let pos = POINT { x, y: y + slide };
        if self.open {
            crate::mousehook::set_rect(2, Some(RECT { left: x, top: y, right: x + pw, bottom: y + ph }));
        }
        // Glass while it slides in and out too (placed once the frame is out).
        let glass = self.acrylic && self.reveal.value > theme::GLASS_FROM;
        let screen = RECT { left: pos.x, top: pos.y, right: pos.x + pw, bottom: pos.y + ph };

        self.hits.clear();
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            theme::panel(&p, 0.0, 0.0, w, h, self.radius, glass, self.opacity, 0.97, 1.0);

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
                    Card::System => {
                        // The whole card opens Task Manager.
                        if self.hovered == Some(Target::System) {
                            p.rrect(cx, cy, CARD_W, ch, 10.0, Color::white(0.04));
                        }
                        system_card(&p, f, ctx, cx, cy);
                        self.hits.push((Target::System, cx, cy, CARD_W, ch));
                    }
                    Card::Weather => weather_card(&p, f, ctx, cx, cy),
                    Card::Calendar => self.calendar_card(&p, f, ctx, cx, cy),
                    Card::Clipboard => self.clipboard_card(&p, f, ctx, cx, cy),
                    Card::Phone => self.phone_card(&p, f, ctx, cx, cy, ch),
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
                    let open = Target::NotifOpen(item.id);
                    let remove = Target::NotifRemove(item.id);
                    if matches!(self.hovered, Some(t) if t == open || t == remove) {
                        p.rrect(x + 6.0, ry + 1.0, CARD_W - 12.0, NOTIF_ROW - 2.0, 8.0, Color::white(0.06));
                    } else if i > 0 {
                        p.rrect(x + 14.0, ry, CARD_W - 28.0, 1.0, 0.0, Color::white(0.06));
                    }
                    p.text(&item.app, &f.small, x + 14.0, ry + 4.0, tw, 15.0, Align::Left, Color::white(0.5));
                    p.text(&item.title, &f.label, x + 14.0, ry + 18.0, tw, 16.0, Align::Left, Color::white(0.95));
                    p.text(&item.body, &f.small, x + 14.0, ry + 34.0, tw, 15.0, Align::Left, Color::white(0.65));
                    // Dismiss (before the row, so it wins the click).
                    let (cx, cy) = (x + CARD_W - 26.0, ry + NOTIF_ROW / 2.0);
                    if self.hovered == Some(remove) {
                        p.circle(cx, cy, 11.0, Color::white(0.10));
                    }
                    let c = Color::white(0.7);
                    p.line(cx - 4.0, cy - 4.0, cx + 4.0, cy + 4.0, c, 1.3);
                    p.line(cx - 4.0, cy + 4.0, cx + 4.0, cy - 4.0, c, 1.3);
                    self.hits.push((remove, cx - 12.0, cy - 12.0, 24.0, 24.0));
                    // The rest of the row opens it.
                    self.hits.push((open, x + 6.0, ry + 1.0, CARD_W - 12.0, NOTIF_ROW - 2.0));
                }
            }
        }
    }

    /// The clipboard history: the newest few, click one to copy it again.
    fn clipboard_card(&mut self, p: &Painter, f: &Fonts, ctx: &Ctx, x: f32, y: f32) {
        use clipboard::State;
        p.text("Clipboard", &f.label, x + 14.0, y + 6.0, 200.0, 26.0, Align::Left, Color::white(0.9));
        let top = y + 34.0;
        let note = |text: &str| p.text(text, &f.body, x, top, CARD_W, CLIP_ROW, Align::Center, Color::white(0.5));
        match ctx.clips.state {
            State::Loading => note("Loading\u{2026}"),
            State::Off => note("Clipboard history is off in Windows"),
            State::Unavailable => note("Clipboard history isn't available"),
            State::Ready if ctx.clips.items.is_empty() => note("Nothing copied yet"),
            State::Ready => {
                let (cw, ch) = (50.0, 22.0);
                let (bx, by) = (x + CARD_W - 10.0 - cw, y + 8.0);
                if self.hovered == Some(Target::ClipClear) {
                    p.rrect(bx, by, cw, ch, 6.0, Color::white(0.10));
                }
                p.text("Clear", &f.small, bx, by, cw, ch, Align::Center, Color::white(0.75));
                self.hits.push((Target::ClipClear, bx, by, cw, ch));
                for (i, item) in ctx.clips.items.iter().take(CLIP_MAX).enumerate() {
                    let ry = top + i as f32 * CLIP_ROW;
                    let target = Target::ClipItem(i);
                    if self.hovered == Some(target) {
                        p.rrect(x + 6.0, ry + 1.0, CARD_W - 12.0, CLIP_ROW - 2.0, 8.0, Color::white(0.08));
                    } else if i > 0 {
                        p.rrect(x + 14.0, ry, CARD_W - 28.0, 1.0, 0.0, Color::white(0.06));
                    }
                    let copied = self.copied.as_deref() == Some(item.id.as_str());
                    let tw = CARD_W - 28.0 - if copied { 56.0 } else { 0.0 };
                    p.text(&item.text, &f.small, x + 14.0, ry, tw, CLIP_ROW, Align::Left, Color::white(0.9));
                    if copied {
                        p.text("Copied", &f.small, x + CARD_W - 66.0, ry, 52.0, CLIP_ROW, Align::Right, GREEN);
                    }
                    self.hits.push((target, x + 6.0, ry + 1.0, CARD_W - 12.0, CLIP_ROW - 2.0));
                }
            }
        }
    }

    /// Whether the Clipboard card is in the panel (it reads the history).
    pub fn shows_clipboard(&self) -> bool {
        self.cards.contains(&Card::Clipboard)
    }

    /// Item `id` was copied again: its row says so until the panel closes.
    pub fn set_copied(&mut self, id: Option<String>) {
        self.copied = id;
        self.dirty = true;
    }

    /// Your phone through Phone Link: the notifications it mirrored here
    /// (click one to open it there, × to dismiss it), and Open Phone Link.
    /// Its music shows on the Media card, marked as from the phone.
    fn phone_card(&mut self, p: &Painter, f: &Fonts, ctx: &Ctx, x: f32, y: f32, h: f32) {
        p.text("Phone", &f.label, x + 14.0, y + 6.0, 200.0, 26.0, Align::Left, Color::white(0.9));
        // A phone outline beside the title.
        let (px, py) = (x + CARD_W - 26.0, y + 19.0);
        p.stroke_rrect(px - 5.0, py - 8.0, 10.0, 16.0, 2.5, Color::white(0.6), 1.2);
        p.line(px - 1.5, py + 5.0, px + 1.5, py + 5.0, Color::white(0.6), 1.2);
        let top = y + 34.0;
        let items: Vec<&crate::notifications::Notif> = phone_items(ctx).collect();
        let tw = CARD_W - 28.0 - 30.0;
        if ctx.notifs.access != Access::Allowed {
            let msg = "Phone Link's notifications show here once Cascade can read notifications.";
            p.text_wrap(msg, &f.small, x + 14.0, top, CARD_W - 28.0, NOTIF_ROW, Color::white(0.55));
        } else if items.is_empty() {
            p.text("Nothing from your phone", &f.body, x, top, CARD_W, NOTIF_ROW, Align::Center, Color::white(0.5));
        }
        for (i, item) in items.iter().enumerate() {
            let ry = top + i as f32 * NOTIF_ROW;
            let open = Target::NotifOpen(item.id);
            let remove = Target::NotifRemove(item.id);
            if matches!(self.hovered, Some(t) if t == open || t == remove) {
                p.rrect(x + 6.0, ry + 1.0, CARD_W - 12.0, NOTIF_ROW - 2.0, 8.0, Color::white(0.06));
            } else if i > 0 {
                p.rrect(x + 14.0, ry, CARD_W - 28.0, 1.0, 0.0, Color::white(0.06));
            }
            p.text(&item.title, &f.label, x + 14.0, ry + 8.0, tw, 16.0, Align::Left, Color::white(0.95));
            p.text(&item.body, &f.small, x + 14.0, ry + 26.0, tw, 15.0, Align::Left, Color::white(0.65));
            let (cx, cy) = (x + CARD_W - 26.0, ry + NOTIF_ROW / 2.0);
            if self.hovered == Some(remove) {
                p.circle(cx, cy, 11.0, Color::white(0.10));
            }
            let c = Color::white(0.7);
            p.line(cx - 4.0, cy - 4.0, cx + 4.0, cy + 4.0, c, 1.3);
            p.line(cx - 4.0, cy + 4.0, cx + 4.0, cy - 4.0, c, 1.3);
            self.hits.push((remove, cx - 12.0, cy - 12.0, 24.0, 24.0));
            self.hits.push((open, x + 6.0, ry + 1.0, CARD_W - 12.0, NOTIF_ROW - 2.0));
        }
        let (bw, bh) = (150.0, 26.0);
        let (bx, by) = (x + 14.0, y + h - bh - 10.0);
        let a = if self.hovered == Some(Target::OpenPhoneLink) { 0.16 } else { 0.09 };
        p.rrect(bx, by, bw, bh, 7.0, Color::white(a));
        p.text("Open Phone Link", &f.small, bx, by, bw, bh, Align::Center, Color::white(0.9));
        self.hits.push((Target::OpenPhoneLink, bx, by, bw, bh));
    }

    /// Whether the Phone card is in the panel (it reads notifications too).
    pub fn shows_phone(&self) -> bool {
        self.cards.contains(&Card::Phone)
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
        // Bars moving with the music, under the art.
        if let Some((levels, c)) = &ctx.bars {
            crate::ui::spectrum(p, levels, x + 14.0, y + 118.0, art, 12, 22.0, c.alpha(0.95));
        }
        let tx = x + 14.0 + art + 14.0;
        let tw = CARD_W - (tx - x) - 14.0;
        let mut text_w = tw;
        if m.phone {
            // Playing on your phone (Phone Link): a phone in the corner.
            let (px, py) = (x + CARD_W - 22.0, y + 22.0);
            p.stroke_rrect(px - 5.0, py - 8.0, 10.0, 16.0, 2.5, Color::white(0.6), 1.2);
            p.line(px - 1.5, py + 5.0, px + 1.5, py + 5.0, Color::white(0.6), 1.2);
            text_w -= 22.0;
        }
        p.text(&m.title, &f.title, tx, y + 12.0, text_w, 24.0, Align::Left, Color::white(0.95));
        p.text(&m.artist, &f.body, tx, y + 36.0, text_w, 20.0, Align::Left, Color::white(0.6));
        // Progress.
        let (pos, dur) = (m.position_now(), m.duration);
        let by = y + 66.0;
        p.rrect(tx, by, tw, 4.0, 2.0, Color::white(0.15));
        if dur > Duration::ZERO {
            let t = (pos.as_secs_f32() / dur.as_secs_f32()).clamp(0.0, 1.0);
            p.rrect(tx, by, (tw * t).max(4.0), 4.0, 2.0, theme::accent());
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
        ("CPU", format!("{:.0}%", s.cpu), &h.cpu, 100.0, theme::accent()),
        (
            "Memory",
            format!("{:.1} / {:.0} GB   {:.0}%", s.ram_used, s.ram_total, s.ram),
            &h.ram,
            100.0,
            Color(0.70, 0.55, 1.0, 1.0),
        ),
    ];
    // Each NVIDIA card on a row of its own (see `gpu_rows`), or Windows' figure.
    let gpus = &ctx.sampler.gpus;
    if gpus.is_empty() {
        if let Some(g) = s.gpu {
            rows.push(("GPU", format!("{g:.0}%"), &h.gpu, 100.0, GREEN));
        }
    }
    let gpu_at = rows.len();
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
    let row_y = |i: usize| y + 34.0 + i as f32 * 44.0;
    for (i, (label, value, hist, max, color)) in rows.into_iter().enumerate() {
        // The cards' rows go in after CPU and memory.
        let i = if i >= gpu_at { i + gpus.len() } else { i };
        let ry = row_y(i);
        p.text(label, &f.small, x + 14.0, ry, 120.0, 16.0, Align::Left, Color::white(0.6));
        p.text(&value, &f.label, x + 14.0, ry, w, 16.0, Align::Right, Color::white(0.95));
        spark(p, hist, x + 14.0, ry + 20.0, w, 20.0, max, color);
    }
    let empty = VecDeque::new();
    for (i, g) in gpus.iter().enumerate() {
        let ry = row_y(gpu_at + i);
        let (left, right) = gpu_row(g);
        p.text(&left, &f.small, x + 14.0, ry, w - 76.0, 16.0, Align::Left, Color::white(0.6));
        p.text(&right, &f.label, x + 14.0, ry, w, 16.0, Align::Right, Color::white(0.95));
        let hist = ctx.sampler.hist.gpus.get(i).unwrap_or(&empty);
        spark(p, hist, x + 14.0, ry + 20.0, w, 20.0, 100.0, GREEN);
    }
}

/// A card's row: its name with memory, power and clock on the left, its load
/// and temperature on the right.
fn gpu_row(g: &crate::nvml::Gpu) -> (String, String) {
    let mut left = g.name.clone();
    if let (Some(u), Some(t)) = (g.vram_used, g.vram_total) {
        left += &format!("   {u:.1} / {t:.0} GB");
    }
    if let Some(pw) = g.power {
        left += &format!("   {pw:.0} W");
    }
    let mut right = g.load.map_or_else(|| "\u{2014}".to_string(), |l| format!("{l:.0}%"));
    if let Some(t) = g.temp {
        right += &format!("  {t:.0}\u{b0}C");
    }
    (left, right)
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

/// The (year, month) `offset` months from the one `now` is in.
fn month_from(now: &windows::Win32::Foundation::SYSTEMTIME, offset: i32) -> (i32, u32) {
    let n = now.wYear as i32 * 12 + now.wMonth.clamp(1, 12) as i32 - 1 + offset;
    (n.div_euclid(12), (n.rem_euclid(12) + 1) as u32)
}

/// The days (since 1970) `e` takes up.
fn event_days(e: &Event) -> std::ops::RangeInclusive<i64> {
    let last = if e.end > e.start { (e.end - 1).div_euclid(DAY) } else { e.start.div_euclid(DAY) };
    e.start.div_euclid(DAY)..=last
}

/// What's on `day`, in order.
fn events_on(events: &[Event], day: i64) -> Vec<&Event> {
    let mut on: Vec<&Event> = events.iter().filter(|e| event_days(e).contains(&day)).collect();
    on.sort_by_key(|e| (!e.all_day, e.start));
    on
}

impl Widgets {
    /// The day the calendar card lists events for: the one picked, else today.
    fn cal_selected(&self) -> i64 {
        self.cal_day.unwrap_or_else(|| crate::calendar::now().div_euclid(DAY))
    }

    /// Rows of events under the calendar card's month: the picked day's
    /// (a "nothing on" row if it has none), or today's while it has some.
    fn cal_rows(&self, ctx: &Ctx) -> usize {
        let n = events_on(ctx.events, self.cal_selected()).len().min(CAL_EVENTS);
        if self.cal_day.is_some() {
            n.max(1)
        } else {
            n
        }
    }

    /// The month (arrows to step through months, the title back to this one),
    /// a dot on days with events, and the picked day's events under it.
    fn calendar_card(&mut self, p: &Painter, f: &Fonts, ctx: &Ctx, x: f32, y: f32) {
        let now = unsafe { GetLocalTime() };
        let (year, month) = month_from(&now, self.cal_month);
        let title = format!("{} {}", MONTHS[(month - 1) as usize], year);
        let tw = 200.0;
        if self.hovered == Some(Target::CalToday) && self.cal_month != 0 {
            p.rrect(x + 8.0, y + 6.0, tw - 40.0, 26.0, 6.0, Color::white(0.08));
        }
        p.text(&title, &f.label, x + 14.0, y + 6.0, tw, 26.0, Align::Left, Color::white(0.9));
        self.hits.push((Target::CalToday, x + 8.0, y + 6.0, tw - 40.0, 26.0));
        if self.cal_month != 0 {
            let back = "Today";
            let (bx, bw) = (x + CARD_W - 14.0 - 56.0 - 64.0, 56.0);
            let a = if self.hovered == Some(Target::CalToday) { 0.16 } else { 0.09 };
            p.rrect(bx, y + 8.0, bw, 22.0, 6.0, Color::white(a));
            p.text(back, &f.small, bx, y + 8.0, bw, 22.0, Align::Center, Color::white(0.85));
            self.hits.push((Target::CalToday, bx, y + 8.0, bw, 22.0));
        }
        // Month arrows.
        for (d, cx) in [(-1, x + CARD_W - 52.0), (1, x + CARD_W - 24.0)] {
            let t = Target::CalMonth(d);
            let cy = y + 19.0;
            if self.hovered == Some(t) {
                p.circle(cx, cy, 12.0, Color::white(0.10));
            }
            let s = d as f32 * 3.0;
            let c = Color::white(0.8);
            p.line(cx - s, cy - 5.0, cx + s, cy, c, 1.5);
            p.line(cx + s, cy, cx - s, cy + 5.0, c, 1.5);
            self.hits.push((t, cx - 13.0, cy - 13.0, 26.0, 26.0));
        }

        // Weeks start on Monday.
        let first_day = ics::days_from_civil(year, month, 1);
        let first = ics::weekday(first_day) as i32;
        let today = crate::calendar::now().div_euclid(DAY);
        let picked = self.cal_day;
        let busy: Vec<i64> = ctx.events.iter().flat_map(event_days).collect();
        let (cell, top) = ((CARD_W - 28.0) / 7.0, y + 36.0);
        for (i, d) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
            p.text(d, &f.small, x + 14.0 + i as f32 * cell, top, cell, 20.0, Align::Center, Color::white(0.45));
        }
        for day in 1..=days_in_month(year as u16, month as u16) {
            let slot = first + day as i32 - 1;
            let (col, row) = ((slot % 7) as f32, (slot / 7) as f32);
            let (cx, cy) = (x + 14.0 + col * cell + cell / 2.0, top + 30.0 + row * 31.0);
            let date = first_day + day as i64 - 1;
            let t = Target::CalDay(date);
            let is_today = date == today;
            if is_today {
                p.circle(cx, cy, 13.0, theme::accent());
            } else if picked == Some(date) {
                p.circle(cx, cy, 13.0, Color::white(0.14));
            } else if self.hovered == Some(t) {
                p.circle(cx, cy, 13.0, Color::white(0.08));
            }
            if is_today && picked.is_some_and(|d| d != today) {
                // Today stays marked while another day is picked.
                p.stroke_circle(cx, cy, 13.0, Color::white(0.5), 1.0);
            }
            let weekend = col >= 5.0;
            let c = match (is_today, weekend) {
                (true, _) => Color::white(1.0),
                (false, true) => Color::white(0.55),
                _ => Color::white(0.85),
            };
            p.text(&day.to_string(), &f.body, cx - cell / 2.0, cy - 12.0, cell, 24.0, Align::Center, c);
            if busy.contains(&date) {
                let dot = if is_today { Color::white(1.0) } else { theme::accent() };
                p.circle(cx, cy + 9.0, 1.7, dot);
            }
            self.hits.push((t, cx - cell / 2.0, cy - 14.0, cell, 28.0));
        }

        // The picked day's events (or today's).
        let rows = self.cal_rows(ctx);
        if rows == 0 {
            return;
        }
        let sel = self.cal_selected();
        let ly = y + 256.0;
        p.rrect(x + 14.0, ly - 4.0, CARD_W - 28.0, 1.0, 0.0, Color::white(0.08));
        let on = events_on(ctx.events, sel);
        if on.is_empty() {
            let label = if sel == today { "Nothing on today".to_string() } else { "Nothing on".to_string() };
            p.text(&label, &f.small, x + 14.0, ly + 2.0, CARD_W - 28.0, CAL_ROW, Align::Left, Color::white(0.5));
            return;
        }
        let now_t = crate::calendar::now();
        for (i, e) in on.iter().take(CAL_EVENTS).enumerate() {
            let ry = ly + 2.0 + i as f32 * CAL_ROW;
            let live = e.start <= now_t && e.end > now_t;
            let when = if e.all_day {
                "All day".to_string()
            } else if e.start.div_euclid(DAY) < sel {
                "Continues".to_string()
            } else {
                crate::ui::time_at(e.start)
            };
            let c = if live { theme::accent() } else { Color::white(0.6) };
            p.text(&when, &f.small, x + 14.0, ry, 76.0, CAL_ROW, Align::Left, c);
            let what =
                if e.location.is_empty() { e.title.clone() } else { format!("{} \u{b7} {}", e.title, e.location) };
            p.text(&what, &f.small, x + 92.0, ry, CARD_W - 106.0, CAL_ROW, Align::Left, Color::white(0.92));
        }
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
    fn months_step_across_years() {
        let now = windows::Win32::Foundation::SYSTEMTIME { wYear: 2026, wMonth: 1, wDay: 15, ..Default::default() };
        assert_eq!(month_from(&now, 0), (2026, 1));
        assert_eq!(month_from(&now, -1), (2025, 12));
        assert_eq!(month_from(&now, 12), (2027, 1));
        assert_eq!(month_from(&now, -13), (2024, 12));
    }

    #[test]
    fn month_lengths() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2026, 10), 31);
    }
}
