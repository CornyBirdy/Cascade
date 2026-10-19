//! Your calendar's events for the notch: read from its iCal (.ics) link,
//! the private address Google Calendar, Outlook and iCloud all give out, on
//! a sleeping thread that looks again every 15 minutes (see `ics`).

use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::Duration;

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::SystemTimeToTzSpecificLocalTime;

use crate::ics::{self, Event, DAY};
use crate::msg;

/// How often the feed is read again.
const EVERY: Duration = Duration::from_secs(15 * 60);

/// The local wall clock now, in local seconds (see `ics`).
pub fn now() -> i64 {
    let t = unsafe { GetLocalTime() };
    ics::days_from_civil(t.wYear as i32, t.wMonth as u32, t.wDay as u32) * DAY
        + t.wHour as i64 * 3600
        + t.wMinute as i64 * 60
        + t.wSecond as i64
}

/// A local-seconds time as a SYSTEMTIME (for Windows' date formatting).
pub fn system_time(t: i64) -> SYSTEMTIME {
    let (y, m, d) = ics::civil_from_days(t.div_euclid(DAY));
    let s = t.rem_euclid(DAY);
    SYSTEMTIME {
        wYear: y as u16,
        wMonth: m as u16,
        wDayOfWeek: ((ics::weekday(t.div_euclid(DAY)) + 1) % 7) as u16,
        wDay: d as u16,
        wHour: (s / 3600) as u16,
        wMinute: (s / 60 % 60) as u16,
        wSecond: (s % 60) as u16,
        wMilliseconds: 0,
    }
}

/// UTC seconds (on the same scale) to the local wall clock at that moment.
fn to_local(utc: i64) -> i64 {
    let mut local = SYSTEMTIME::default();
    if unsafe { SystemTimeToTzSpecificLocalTime(None, &system_time(utc), &mut local) }.is_err() {
        return utc;
    }
    ics::days_from_civil(local.wYear as i32, local.wMonth as u32, local.wDay as u32) * DAY
        + local.wHour as i64 * 3600
        + local.wMinute as i64 * 60
        + local.wSecond as i64
}

/// `webcal://` links are plain https.
fn https(url: &str) -> String {
    let url = url.trim();
    match url.strip_prefix("webcal://") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    }
}

/// Starts reading `url` (empty: nothing). Send a new link to switch, or an
/// empty one to stop. Events arrive as a boxed `Vec<Event>` in
/// `msg::CALENDAR`: from last month's to about three months ahead.
pub fn spawn() -> Sender<String> {
    let (tx, rx) = channel::<String>();
    std::thread::spawn(move || {
        let mut url = String::new();
        loop {
            let next = if url.is_empty() {
                rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                rx.recv_timeout(EVERY)
            };
            match next {
                Ok(u) => url = https(&u),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if url.is_empty() {
                continue;
            }
            // Offline (or a bad link): keep what was there and try again later.
            let Some(text) = crate::http::get_text(&url) else { continue };
            let today = now().div_euclid(DAY);
            // From the start of last month to about three months on, for
            // the widgets panel's calendar to step through.
            let (y, m, _) = ics::civil_from_days(today);
            let (y, m) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
            let from = ics::days_from_civil(y, m, 1) * DAY;
            let to = (today + 100) * DAY;
            let events = ics::occurrences(&text, from, to, &to_local);
            let ptr = Box::into_raw(Box::new(events));
            if !msg::post(msg::CALENDAR, 0, ptr as isize) {
                drop(unsafe { Box::from_raw(ptr) });
            }
        }
    });
    tx
}

/// The events still to come (or under way) after local time `now`, in order.
pub fn upcoming(events: &[Event], now: i64) -> impl Iterator<Item = &Event> {
    events.iter().filter(move |e| e.end > now || (e.start >= now && e.end == e.start))
}
