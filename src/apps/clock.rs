//! Clock (`cascade.exe --clock`), as Windows' own: a timer (several, each
//! with a ring running down), a stopwatch with laps, alarms (kept between
//! runs; they ring while Clock is open, which it can stay in the tray of
//! your attention minimised) and the time round the world.

use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::WindowsAndMessaging::{FlashWindow, MB_ICONEXCLAMATION};

use super::{App, Ctx, Kit, Look, PANEL};
use crate::gfx::{Align, Color, Fonts, Painter};

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Timer,
    Alarm,
    Stopwatch,
    World,
}

const TABS: [(Tab, &str, u32); 4] = [
    (Tab::Timer, "Timer", 0xE916),
    (Tab::Alarm, "Alarm", 0xEA8F),
    (Tab::Stopwatch, "Stopwatch", 0xE916),
    (Tab::World, "World clock", 0xE909),
];

struct Timer {
    total: Duration,
    left: Duration,
    /// Running since (with `left` as it was then).
    since: Option<Instant>,
    rang: bool,
}

impl Timer {
    fn remaining(&self) -> Duration {
        match self.since {
            Some(t) => self.left.saturating_sub(t.elapsed()),
            None => self.left,
        }
    }
}

#[derive(Clone)]
struct Alarm {
    hour: u32,
    minute: u32,
    on: bool,
    /// The day it last rang (so it rings once a day).
    rang_on: u32,
}

const CITIES: [(&str, f32); 8] = [
    ("London", 0.0),
    ("New York", -5.0),
    ("Los Angeles", -8.0),
    ("Tokyo", 9.0),
    ("Sydney", 11.0),
    ("Perth", 8.0),
    ("Berlin", 1.0),
    ("Singapore", 8.0),
];

struct Clock {
    tab: Tab,
    timers: Vec<Timer>,
    /// The new timer being set: minutes and seconds.
    new_min: u32,
    new_sec: u32,
    watch: Duration,
    watch_since: Option<Instant>,
    laps: Vec<Duration>,
    alarms: Vec<Alarm>,
    new_alarm: (u32, u32),
    ringing: Option<String>,
    ring_beep: Instant,
}

fn alarms_path() -> std::path::PathBuf {
    crate::config::dir().join("alarms.txt")
}

fn load_alarms() -> Vec<Alarm> {
    std::fs::read_to_string(alarms_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut p = l.split(',');
            Some(Alarm {
                hour: p.next()?.parse().ok()?,
                minute: p.next()?.parse().ok()?,
                on: p.next()? == "1",
                rang_on: 0,
            })
        })
        .collect()
}

fn hms(d: Duration, tenths: bool) -> String {
    let s = d.as_secs();
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    let mut out = if h > 0 { format!("{h}:{m:02}:{sec:02}") } else { format!("{m:02}:{sec:02}") };
    if tenths {
        out.push_str(&format!(".{:02}", d.subsec_millis() / 10));
    }
    out
}

fn now() -> windows::Win32::Foundation::SYSTEMTIME {
    unsafe { windows::Win32::System::SystemInformation::GetLocalTime() }
}

fn utc() -> windows::Win32::Foundation::SYSTEMTIME {
    unsafe { windows::Win32::System::SystemInformation::GetSystemTime() }
}

const NAV: u32 = 100;
const T_START: u32 = 200;
const T_DEL: u32 = 300;
const A_TOGGLE: u32 = 400;
const A_DEL: u32 = 500;
const CTRL: u32 = 600;

impl Clock {
    fn save_alarms(&self) {
        let text: String =
            self.alarms.iter().map(|a| format!("{},{},{}\n", a.hour, a.minute, if a.on { 1 } else { 0 })).collect();
        let _ = std::fs::create_dir_all(crate::config::dir());
        let _ = std::fs::write(alarms_path(), text);
    }

    fn watch_now(&self) -> Duration {
        self.watch + self.watch_since.map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// A big number with − and + under it, for setting a time.
    #[allow(clippy::too_many_arguments)]
    fn spinner(&self, p: &Painter, f: &Fonts, kit: &mut Kit, x: f32, y: f32, value: u32, label: &str, id: u32) {
        p.scaled(2.0, x + 50.0, y + 30.0, || {
            p.text(&format!("{value:02}"), &f.big, x + 20.0, y + 12.0, 60.0, 36.0, Align::Center, Color::white(0.95));
        });
        p.text(label, &f.small, x, y + 66.0, 100.0, 18.0, Align::Center, Color::white(0.5));
        kit.round(p, f, x + 24.0, y + 104.0, 18.0, 0xE738, Look::Plain, id);
        kit.round(p, f, x + 76.0, y + 104.0, 18.0, 0xE710, Look::Plain, id + 1);
    }
}

impl App for Clock {
    fn draw(&mut self, p: &Painter, f: &Fonts, w: f32, h: f32, kit: &mut Kit) {
        let accent = crate::theme::accent();
        // The side: the four parts.
        let side = 200.0;
        p.rrect(0.0, 0.0, side, h, 0.0, Color(0.105, 0.105, 0.11, 1.0));
        for (i, (t, name, g)) in TABS.iter().enumerate() {
            let y = 16.0 + i as f32 * 42.0;
            let on = self.tab == *t;
            if on {
                p.rrect(8.0, y, side - 16.0, 38.0, 6.0, Color::white(0.08));
                p.rrect(8.0, y + 11.0, 3.0, 16.0, 1.5, accent);
            }
            kit.button(p, f, 8.0, y, side - 16.0, 38.0, "", None, Look::Plain, NAV + i as u32);
            if let (Some(font), Some(c)) = (&f.icons, char::from_u32(*g)) {
                p.text(&c.to_string(), font, 22.0, y, 18.0, 38.0, Align::Center, Color::white(0.85));
            }
            p.text(name, &f.body, 52.0, y, side - 60.0, 38.0, Align::Left, Color::white(if on { 0.96 } else { 0.75 }));
        }
        let (x, cw) = (side + 28.0, w - side - 56.0);
        if let Some(what) = &self.ringing {
            p.rrect(x, h - 70.0, cw, 54.0, 10.0, accent.alpha(0.25));
            p.text(what, &f.title, x + 16.0, h - 70.0, cw - 140.0, 54.0, Align::Left, Color::white(0.95));
            kit.button(p, f, x + cw - 116.0, h - 59.0, 100.0, 32.0, "Dismiss", None, Look::Accent, CTRL + 50);
        }
        match self.tab {
            Tab::Timer => {
                p.text("Timer", &f.big, x, 16.0, cw, 40.0, Align::Left, Color::white(0.96));
                // Setting a new one.
                self.spinner(p, f, kit, x, 70.0, self.new_min, "minutes", CTRL);
                self.spinner(p, f, kit, x + 120.0, 70.0, self.new_sec, "seconds", CTRL + 2);
                kit.button(p, f, x + 260.0, 160.0, 130.0, 36.0, "Add timer", Some(0xE710), Look::Accent, CTRL + 4);
                // The timers, as cards with a ring.
                for (i, t) in self.timers.iter().enumerate() {
                    let (col, row) = (i % 3, i / 3);
                    let (tx, ty) = (x + col as f32 * 220.0, 240.0 + row as f32 * 230.0);
                    if tx + 210.0 > w || ty + 220.0 > h {
                        continue;
                    }
                    p.rrect(tx, ty, 210.0, 220.0, 12.0, PANEL);
                    let (cx, cy) = (tx + 105.0, ty + 95.0);
                    let left = t.remaining();
                    let frac = left.as_secs_f32() / t.total.as_secs_f32().max(0.001);
                    p.stroke_circle(cx, cy, 70.0, Color::white(0.08), 6.0);
                    p.arc(
                        cx,
                        cy,
                        70.0,
                        0.0,
                        360.0 * frac,
                        if left.is_zero() { Color(0.9, 0.35, 0.3, 1.0) } else { accent },
                        6.0,
                    );
                    p.text(
                        &hms(left, false),
                        &f.big,
                        cx - 70.0,
                        cy - 20.0,
                        140.0,
                        40.0,
                        Align::Center,
                        Color::white(0.96),
                    );
                    let (icon, look) = if t.since.is_some() { (0xE769, Look::Plain) } else { (0xE768, Look::Accent) };
                    kit.round(p, f, cx - 26.0, ty + 192.0, 18.0, icon, look, T_START + i as u32);
                    kit.round(p, f, cx + 26.0, ty + 192.0, 18.0, 0xE74D, Look::Plain, T_DEL + i as u32);
                }
            }
            Tab::Stopwatch => {
                p.text("Stopwatch", &f.big, x, 16.0, cw, 40.0, Align::Left, Color::white(0.96));
                let t = self.watch_now();
                p.scaled(2.6, x + cw / 2.0, 150.0, || {
                    p.text(
                        &hms(t, true),
                        &f.big,
                        x + cw / 2.0 - 100.0,
                        132.0,
                        200.0,
                        36.0,
                        Align::Center,
                        Color::white(0.97),
                    );
                });
                let running = self.watch_since.is_some();
                let cx = x + cw / 2.0;
                kit.round(p, f, cx - 60.0, 250.0, 26.0, if running { 0xE769 } else { 0xE768 }, Look::Accent, CTRL + 10);
                kit.round(p, f, cx, 250.0, 26.0, 0xE7C1, Look::Plain, CTRL + 11);
                kit.round(p, f, cx + 60.0, 250.0, 26.0, 0xE72C, Look::Plain, CTRL + 12);
                for (i, lap) in self.laps.iter().enumerate().rev() {
                    let row = self.laps.len() - 1 - i;
                    let y = 300.0 + row as f32 * 30.0;
                    if y > h - 30.0 {
                        break;
                    }
                    let prev = if i == 0 { Duration::ZERO } else { self.laps[i - 1] };
                    p.text(
                        &format!("Lap {}", i + 1),
                        &f.body,
                        cx - 180.0,
                        y,
                        100.0,
                        26.0,
                        Align::Left,
                        Color::white(0.7),
                    );
                    p.text(
                        &hms(*lap - prev, true),
                        &f.body,
                        cx - 60.0,
                        y,
                        120.0,
                        26.0,
                        Align::Center,
                        Color::white(0.9),
                    );
                    p.text(&hms(*lap, true), &f.body, cx + 60.0, y, 120.0, 26.0, Align::Right, Color::white(0.55));
                }
            }
            Tab::Alarm => {
                p.text("Alarm", &f.big, x, 16.0, cw, 40.0, Align::Left, Color::white(0.96));
                self.spinner(p, f, kit, x, 70.0, self.new_alarm.0, "hour", CTRL + 20);
                self.spinner(p, f, kit, x + 120.0, 70.0, self.new_alarm.1, "minute", CTRL + 22);
                kit.button(p, f, x + 260.0, 160.0, 130.0, 36.0, "Add alarm", Some(0xE710), Look::Accent, CTRL + 24);
                p.text(
                    "Alarms ring while Clock is open (minimised is fine).",
                    &f.small,
                    x,
                    206.0,
                    cw,
                    18.0,
                    Align::Left,
                    Color::white(0.45),
                );
                for (i, a) in self.alarms.iter().enumerate() {
                    let y = 240.0 + i as f32 * 70.0;
                    if y > h - 70.0 {
                        break;
                    }
                    p.rrect(x, y, cw.min(520.0), 62.0, 10.0, PANEL);
                    let c = Color::white(if a.on { 0.96 } else { 0.45 });
                    p.text(&format!("{:02}:{:02}", a.hour, a.minute), &f.big, x + 18.0, y, 140.0, 62.0, Align::Left, c);
                    let tx = x + cw.min(520.0) - 110.0;
                    p.rrect(tx, y + 21.0, 40.0, 20.0, 10.0, if a.on { accent } else { Color::white(0.15) });
                    p.circle(if a.on { tx + 30.0 } else { tx + 10.0 }, y + 31.0, 7.0, Color::white(0.95));
                    kit.hit(tx - 6.0, y + 15.0, 52.0, 32.0, A_TOGGLE + i as u32);
                    kit.round(p, f, x + cw.min(520.0) - 30.0, y + 31.0, 16.0, 0xE74D, Look::Plain, A_DEL + i as u32);
                }
            }
            Tab::World => {
                p.text("World clock", &f.big, x, 16.0, cw, 40.0, Align::Left, Color::white(0.96));
                let t = now();
                p.scaled(2.4, x + 130.0, 110.0, || {
                    p.text(
                        &format!("{:02}:{:02}", t.wHour, t.wMinute),
                        &f.big,
                        x + 80.0,
                        92.0,
                        100.0,
                        36.0,
                        Align::Center,
                        Color::white(0.97),
                    );
                });
                p.text("Here", &f.body, x, 150.0, 260.0, 22.0, Align::Center, Color::white(0.55));
                let u = utc();
                let mins = u.wHour as f32 * 60.0 + u.wMinute as f32;
                for (i, (city, off)) in CITIES.iter().enumerate() {
                    let (col, row) = (i % 2, i / 2);
                    let (cx, cy) = (x + col as f32 * 280.0, 200.0 + row as f32 * 74.0);
                    if cy > h - 70.0 {
                        continue;
                    }
                    p.rrect(cx, cy, 270.0, 66.0, 10.0, PANEL);
                    let m = (mins + off * 60.0).rem_euclid(1440.0) as u32;
                    p.text(city, &f.body, cx + 16.0, cy + 10.0, 150.0, 22.0, Align::Left, Color::white(0.9));
                    let diff = if *off >= 0.0 { format!("UTC+{off}") } else { format!("UTC{off}") };
                    p.text(&diff, &f.small, cx + 16.0, cy + 34.0, 150.0, 18.0, Align::Left, Color::white(0.5));
                    p.text(
                        &format!("{:02}:{:02}", m / 60, m % 60),
                        &f.big,
                        cx + 150.0,
                        cy,
                        104.0,
                        66.0,
                        Align::Right,
                        Color::white(0.95),
                    );
                }
            }
        }
    }

    fn press(&mut self, id: u32, ctx: &Ctx) {
        match id {
            x if (NAV..NAV + 4).contains(&x) => self.tab = TABS[(x - NAV) as usize].0,
            x if x == CTRL => self.new_min = (self.new_min + 59) % 60,
            x if x == CTRL + 1 => self.new_min = (self.new_min + 1) % 60,
            x if x == CTRL + 2 => self.new_sec = (self.new_sec + 55) % 60,
            x if x == CTRL + 3 => self.new_sec = (self.new_sec + 5) % 60,
            x if x == CTRL + 4 => {
                let total = Duration::from_secs((self.new_min * 60 + self.new_sec) as u64);
                if !total.is_zero() {
                    self.timers.push(Timer { total, left: total, since: Some(Instant::now()), rang: false });
                }
            }
            x if x == CTRL + 10 => match self.watch_since.take() {
                Some(t) => self.watch += t.elapsed(),
                None => self.watch_since = Some(Instant::now()),
            },
            x if x == CTRL + 11 => {
                if self.watch_since.is_some() {
                    let t = self.watch_now();
                    self.laps.push(t);
                }
            }
            x if x == CTRL + 12 => {
                self.watch = Duration::ZERO;
                self.watch_since = None;
                self.laps.clear();
            }
            x if x == CTRL + 20 => self.new_alarm.0 = (self.new_alarm.0 + 23) % 24,
            x if x == CTRL + 21 => self.new_alarm.0 = (self.new_alarm.0 + 1) % 24,
            x if x == CTRL + 22 => self.new_alarm.1 = (self.new_alarm.1 + 55) % 60,
            x if x == CTRL + 23 => self.new_alarm.1 = (self.new_alarm.1 + 5) % 60,
            x if x == CTRL + 24 => {
                let (hour, minute) = self.new_alarm;
                self.alarms.push(Alarm { hour, minute, on: true, rang_on: 0 });
                self.save_alarms();
            }
            x if x == CTRL + 50 => self.ringing = None,
            x if (T_START..T_START + 100).contains(&x) => {
                if let Some(t) = self.timers.get_mut((x - T_START) as usize) {
                    match t.since.take() {
                        Some(s) => t.left = t.left.saturating_sub(s.elapsed()),
                        None => {
                            if t.left.is_zero() {
                                t.left = t.total;
                                t.rang = false;
                            }
                            t.since = Some(Instant::now());
                        }
                    }
                }
            }
            x if (T_DEL..T_DEL + 100).contains(&x) => {
                let i = (x - T_DEL) as usize;
                if i < self.timers.len() {
                    self.timers.remove(i);
                }
            }
            x if (A_TOGGLE..A_TOGGLE + 100).contains(&x) => {
                if let Some(a) = self.alarms.get_mut((x - A_TOGGLE) as usize) {
                    a.on = !a.on;
                }
                self.save_alarms();
            }
            x if (A_DEL..A_DEL + 100).contains(&x) => {
                let i = (x - A_DEL) as usize;
                if i < self.alarms.len() {
                    self.alarms.remove(i);
                }
                self.save_alarms();
            }
            _ => {}
        }
        ctx.redraw();
    }

    fn tick_ms(&self) -> u32 {
        if self.tab == Tab::Stopwatch && self.watch_since.is_some() {
            33
        } else {
            250
        }
    }

    fn tick(&mut self, ctx: &Ctx) -> bool {
        // Timers that ran out, and alarms whose minute has come.
        for t in &mut self.timers {
            if t.since.is_some() && t.remaining().is_zero() && !t.rang {
                t.rang = true;
                t.left = Duration::ZERO;
                t.since = None;
                self.ringing = Some(format!("Timer done ({})", hms(t.total, false)));
            }
        }
        let n = now();
        let day = n.wYear as u32 * 400 + n.wMonth as u32 * 32 + n.wDay as u32;
        for a in &mut self.alarms {
            if a.on && a.hour == n.wHour as u32 && a.minute == n.wMinute as u32 && a.rang_on != day {
                a.rang_on = day;
                self.ringing = Some(format!("Alarm {:02}:{:02}", a.hour, a.minute));
            }
        }
        if self.ringing.is_some() && self.ring_beep.elapsed() > Duration::from_millis(1500) {
            self.ring_beep = Instant::now();
            unsafe {
                let _ = MessageBeep(MB_ICONEXCLAMATION);
                let _ = FlashWindow(ctx.hwnd, true);
            }
        }
        true
    }
}

/// `cascade.exe --clock`.
pub fn run() {
    super::run(w!("CascadeClock"), "Clock", "Cascade.Clock", 980, 700, |_| {
        Box::new(Clock {
            tab: Tab::Timer,
            timers: Vec::new(),
            new_min: 5,
            new_sec: 0,
            watch: Duration::ZERO,
            watch_since: None,
            laps: Vec::new(),
            alarms: load_alarms(),
            new_alarm: (7, 0),
            ringing: None,
            ring_beep: Instant::now(),
        })
    });
}
