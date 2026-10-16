//! Reading iCalendar (.ics) feeds for the notch's calendar: the events in a
//! window of time, repeating ones expanded (daily, weekly, monthly, yearly,
//! with the usual INTERVAL, COUNT, UNTIL, BYDAY, BYMONTHDAY, EXDATE and moved
//! occurrences), cancelled ones left out.
//!
//! Times are "local seconds": seconds since 1970-01-01 00:00 on the local
//! wall clock, which makes days and weeks plain arithmetic. Times given in
//! UTC are turned local with the caller's `to_local`; times with a TZID, and
//! floating ones, are taken as local already.

use std::collections::HashSet;

pub const DAY: i64 = 86_400;

/// One occurrence of an event.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Local seconds.
    pub start: i64,
    pub end: i64,
    pub title: String,
    pub location: String,
    pub all_day: bool,
}

/// Days since 1970-01-01 of a date (proleptic Gregorian).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date `z` days after 1970-01-01: (year, month, day).
pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + (m <= 2) as i64) as i32;
    (y, m, d)
}

/// Day of the week of day `z` (see `days_from_civil`): 0 Monday .. 6 Sunday.
pub fn weekday(z: i64) -> u32 {
    (z + 3).rem_euclid(7) as u32
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let next = if m == 12 { days_from_civil(y + 1, 1, 1) } else { days_from_civil(y, m + 1, 1) };
    (next - days_from_civil(y, m, 1)) as u32
}

/// The lines of a feed with folded lines joined back up.
fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        match (line.strip_prefix(' ').or_else(|| line.strip_prefix('\t')), out.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ => out.push(line.to_string()),
        }
    }
    out
}

/// "DTSTART;TZID=X:20261016T103000" -> ("DTSTART", "TZID=X", "20261016T103000").
fn split_line(line: &str) -> Option<(&str, &str, &str)> {
    // The value starts at the first ':' outside quotes.
    let mut quoted = false;
    let colon = line.char_indices().find_map(|(i, c)| {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => return Some(i),
            _ => {}
        }
        None
    })?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let (name, params) = head.split_once(';').unwrap_or((head, ""));
    Some((name, params, value))
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push(' '),
            Some(c) => out.push(c),
            None => {}
        }
    }
    out.trim().to_string()
}

/// A DATE or DATE-TIME value: (local seconds, whether it's a whole day).
fn parse_time(value: &str, to_local: &dyn Fn(i64) -> i64) -> Option<(i64, bool)> {
    let v = value.trim();
    let num = |s: &str| s.parse::<u32>().ok();
    let (y, m, d) = (v.get(0..4)?.parse::<i32>().ok()?, num(v.get(4..6)?)?, num(v.get(6..8)?)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let day = days_from_civil(y, m, d) * DAY;
    if v.len() == 8 {
        return Some((day, true));
    }
    let t = v.get(8..)?.strip_prefix('T')?;
    let (hh, mm, ss) = (num(t.get(0..2)?)?, num(t.get(2..4)?)?, num(t.get(4..6)?).unwrap_or(0));
    let secs = day + (hh * 3600 + mm * 60 + ss) as i64;
    Some((if t.ends_with('Z') { to_local(secs) } else { secs }, false))
}

/// A DURATION ("PT1H30M", "P1D", "P2W") in seconds.
fn parse_duration(v: &str) -> Option<i64> {
    let v = v.trim();
    let (sign, v) = match v.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, v.strip_prefix('+').unwrap_or(v)),
    };
    let mut rest = v.strip_prefix('P')?;
    let mut total = 0i64;
    let mut time = false;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('T') {
            time = true;
            rest = r;
            continue;
        }
        let end = rest.find(|c: char| !c.is_ascii_digit())?;
        let n: i64 = rest[..end].parse().ok()?;
        let unit = rest[end..].chars().next()?;
        total += n * match (unit, time) {
            ('W', _) => 7 * DAY,
            ('D', _) => DAY,
            ('H', true) => 3600,
            ('M', true) => 60,
            ('S', true) => 1,
            _ => return None,
        };
        rest = &rest[end + 1..];
    }
    Some(sign * total)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug)]
struct Rule {
    freq: Freq,
    interval: i64,
    count: Option<u32>,
    until: Option<i64>,
    /// (which one in the month, e.g. 2 or -1, or 0 for every; weekday 0 = Monday).
    by_day: Vec<(i32, u32)>,
    by_month_day: Vec<i32>,
}

fn parse_rule(v: &str, to_local: &dyn Fn(i64) -> i64) -> Option<Rule> {
    let mut rule =
        Rule { freq: Freq::Daily, interval: 1, count: None, until: None, by_day: Vec::new(), by_month_day: Vec::new() };
    let mut freq = None;
    for part in v.split(';') {
        let (k, val) = part.split_once('=')?;
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match val.trim().to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return None,
                })
            }
            "INTERVAL" => rule.interval = val.trim().parse::<i64>().ok()?.max(1),
            "COUNT" => rule.count = val.trim().parse().ok(),
            "UNTIL" => rule.until = parse_time(val, to_local).map(|(t, day)| if day { t + DAY - 1 } else { t }),
            "BYDAY" => {
                for d in val.split(',') {
                    let d = d.trim().to_ascii_uppercase();
                    if d.len() < 2 {
                        continue;
                    }
                    let (n, name) = d.split_at(d.len() - 2);
                    let wd = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"].iter().position(|w| *w == name)?;
                    rule.by_day.push((if n.is_empty() { 0 } else { n.parse().ok()? }, wd as u32));
                }
            }
            "BYMONTHDAY" => {
                rule.by_month_day = val.split(',').filter_map(|d| d.trim().parse().ok()).collect();
            }
            _ => {}
        }
    }
    rule.freq = freq?;
    Some(rule)
}

/// The days (see `days_from_civil`) in month (y, m) that `rule` picks, for
/// an event first on day `first`, in order.
fn month_days(rule: &Rule, y: i32, m: u32, first: i64) -> Vec<i64> {
    let start = days_from_civil(y, m, 1);
    let len = days_in_month(y, m) as i64;
    let mut out: Vec<i64> = Vec::new();
    if !rule.by_day.is_empty() {
        for &(n, wd) in &rule.by_day {
            let all: Vec<i64> = (0..len).map(|i| start + i).filter(|&d| weekday(d) == wd).collect();
            match n {
                0 => out.extend(&all),
                n if n > 0 => out.extend(all.get(n as usize - 1)),
                n => out.extend(all.len().checked_sub(n.unsigned_abs() as usize).and_then(|i| all.get(i))),
            }
        }
    } else if !rule.by_month_day.is_empty() {
        for &d in &rule.by_month_day {
            let i = if d > 0 { d as i64 - 1 } else { len + d as i64 };
            if (0..len).contains(&i) {
                out.push(start + i);
            }
        }
    } else {
        let (_, _, d) = civil_from_days(first);
        if (d as i64) <= len {
            out.push(start + d as i64 - 1);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The starts (local seconds) of `rule`'s occurrences of an event first at
/// `dtstart`, up to `to`.
fn expand(rule: &Rule, dtstart: i64, to: i64) -> Vec<i64> {
    let first = dtstart.div_euclid(DAY);
    let time = dtstart.rem_euclid(DAY);
    let mut out = Vec::new();
    let mut taken = 0u32;
    // Each step gives the days of one period (day, week, month or year).
    for step in 0..20_000i64 {
        let days: Vec<i64> = match rule.freq {
            Freq::Daily => vec![first + step * rule.interval],
            Freq::Weekly => {
                let monday = first - weekday(first) as i64 + step * rule.interval * 7;
                let wds: Vec<u32> = if rule.by_day.is_empty() {
                    vec![weekday(first)]
                } else {
                    rule.by_day.iter().map(|d| d.1).collect()
                };
                let mut v: Vec<i64> = wds.iter().map(|&wd| monday + wd as i64).collect();
                v.sort_unstable();
                v.dedup();
                v
            }
            Freq::Monthly => {
                let (y, m, _) = civil_from_days(first);
                let months = (m as i64 - 1) + step * rule.interval;
                let (y, m) = (y + months.div_euclid(12) as i32, (months.rem_euclid(12) + 1) as u32);
                month_days(rule, y, m, first)
            }
            Freq::Yearly => {
                let (y, m, d) = civil_from_days(first);
                let y = y + (step * rule.interval) as i32;
                if d <= days_in_month(y, m) {
                    vec![days_from_civil(y, m, d)]
                } else {
                    Vec::new()
                }
            }
        };
        let Some(&last) = days.last() else { continue };
        for day in days {
            if day < first {
                continue;
            }
            let start = day * DAY + time;
            if rule.until.is_some_and(|u| start > u) || rule.count.is_some_and(|c| taken >= c) || start > to {
                return out;
            }
            taken += 1;
            out.push(start);
        }
        if last * DAY > to {
            break;
        }
    }
    out
}

/// One VEVENT as read.
#[derive(Default)]
struct Raw {
    uid: String,
    start: Option<(i64, bool)>,
    end: Option<i64>,
    duration: Option<i64>,
    title: String,
    location: String,
    rule: Option<String>,
    exdates: Vec<i64>,
    recurrence_id: Option<i64>,
    cancelled: bool,
}

/// The occurrences of every event in `text` that overlap `from`..`to`
/// (local seconds), in order of their start.
pub fn occurrences(text: &str, from: i64, to: i64, to_local: &dyn Fn(i64) -> i64) -> Vec<Event> {
    let mut raws: Vec<Raw> = Vec::new();
    let mut current: Option<Raw> = None;
    // Components inside the event (alarms) have properties of their own.
    let mut nested = 0;
    for line in unfold(text) {
        let Some((name, params, value)) = split_line(&line) else { continue };
        let name = name.to_ascii_uppercase();
        match (name.as_str(), value.trim().to_ascii_uppercase().as_str()) {
            ("BEGIN", "VEVENT") => {
                current = Some(Raw::default());
                nested = 0;
                continue;
            }
            ("END", "VEVENT") => {
                raws.extend(current.take());
                continue;
            }
            ("BEGIN", _) if current.is_some() => nested += 1,
            ("END", _) if current.is_some() => nested -= 1,
            _ => {}
        }
        let Some(ev) = current.as_mut().filter(|_| nested == 0) else { continue };
        let date_only = params.to_ascii_uppercase().contains("VALUE=DATE") && !params.contains("DATE-TIME");
        let time = |v: &str| parse_time(v, to_local).map(|(t, day)| (t, day || date_only));
        match name.as_str() {
            "UID" => ev.uid = value.trim().to_string(),
            "DTSTART" => ev.start = time(value),
            "DTEND" => ev.end = time(value).map(|t| t.0),
            "DURATION" => ev.duration = parse_duration(value),
            "SUMMARY" => ev.title = unescape(value),
            "LOCATION" => ev.location = unescape(value),
            "RRULE" => ev.rule = Some(value.to_string()),
            "EXDATE" => ev.exdates.extend(value.split(',').filter_map(|v| time(v).map(|t| t.0))),
            "RECURRENCE-ID" => ev.recurrence_id = time(value).map(|t| t.0),
            "STATUS" => ev.cancelled = value.trim().eq_ignore_ascii_case("CANCELLED"),
            _ => {}
        }
    }

    // Occurrences moved (or cancelled) on their own replace the series' ones.
    let moved: HashSet<(String, i64)> =
        raws.iter().filter_map(|r| r.recurrence_id.map(|t| (r.uid.clone(), t))).collect();
    let mut out = Vec::new();
    for r in &raws {
        let Some((start, all_day)) = r.start else { continue };
        if r.cancelled {
            continue;
        }
        let length = match (r.end, r.duration) {
            (Some(end), _) => (end - start).max(0),
            (None, Some(d)) => d.max(0),
            (None, None) if all_day => DAY,
            _ => 0,
        };
        let starts = match r.rule.as_deref().and_then(|v| parse_rule(v, to_local)) {
            Some(rule) if r.recurrence_id.is_none() => expand(&rule, start, to),
            _ => vec![start],
        };
        for s in starts {
            let e = s + length;
            if e < from || s >= to || (e == from && length > 0) {
                continue;
            }
            if r.recurrence_id.is_none() && (r.exdates.contains(&s) || moved.contains(&(r.uid.clone(), s))) {
                continue;
            }
            out.push(Event { start: s, end: e, title: r.title.clone(), location: r.location.clone(), all_day });
        }
    }
    out.sort_by_key(|e| (e.start, !e.all_day));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, hh: i64, mm: i64) -> i64 {
        days_from_civil(y, m, d) * DAY + hh * 3600 + mm * 60
    }

    /// UTC+10, like Brisbane.
    fn plus10(t: i64) -> i64 {
        t + 10 * 3600
    }

    #[test]
    fn dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2026, 10, 16)), (2026, 10, 16));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        // 2026-10-16 is a Friday.
        assert_eq!(weekday(days_from_civil(2026, 10, 16)), 4);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(parse_duration("PT1H30M"), Some(5400));
        assert_eq!(parse_duration("P1W"), Some(7 * DAY));
        assert_eq!(parse_duration("P1DT2H"), Some(DAY + 7200));
    }

    const FEED: &str = "BEGIN:VCALENDAR\r
VERSION:2.0\r
BEGIN:VEVENT\r
UID:standup\r
DTSTART;TZID=Australia/Brisbane:20261012T093000\r
DTEND;TZID=Australia/Brisbane:20261012T094500\r
RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR\r
EXDATE;TZID=Australia/Brisbane:20261014T093000\r
SUMMARY:Stand \r
 up\r
LOCATION:Room 4\\, level 2\r
BEGIN:VALARM\r
SUMMARY:alarm text\r
END:VALARM\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:standup\r
RECURRENCE-ID;TZID=Australia/Brisbane:20261016T093000\r
DTSTART;TZID=Australia/Brisbane:20261016T110000\r
DTEND;TZID=Australia/Brisbane:20261016T111500\r
SUMMARY:Stand up (moved)\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:flight\r
DTSTART:20261017T000000Z\r
DURATION:PT2H\r
SUMMARY:Flying\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:holiday\r
DTSTART;VALUE=DATE:20261019\r
SUMMARY:Holiday\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:gone\r
DTSTART:20261016T020000Z\r
STATUS:CANCELLED\r
SUMMARY:Cancelled\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn reads_a_week() {
        let from = at(2026, 10, 12, 0, 0);
        let to = at(2026, 10, 20, 0, 0);
        let ev = occurrences(FEED, from, to, &plus10);
        let list: Vec<(i64, &str)> = ev.iter().map(|e| (e.start, e.title.as_str())).collect();
        assert_eq!(
            list,
            vec![
                (at(2026, 10, 12, 9, 30), "Stand up"),
                // Wednesday is excluded; Friday's was moved to 11:00.
                (at(2026, 10, 16, 11, 0), "Stand up (moved)"),
                // 00:00 UTC is 10:00 in UTC+10.
                (at(2026, 10, 17, 10, 0), "Flying"),
                (at(2026, 10, 19, 0, 0), "Holiday"),
                (at(2026, 10, 19, 9, 30), "Stand up"),
            ]
        );
        assert_eq!(ev[0].location, "Room 4, level 2");
        assert_eq!(ev[0].end - ev[0].start, 15 * 60);
        assert!(ev[3].all_day && ev[3].end - ev[3].start == DAY);
        assert_eq!(ev[2].end - ev[2].start, 2 * 3600);
    }

    #[test]
    fn repeats() {
        let none = |t: i64| t;
        let rule = |v: &str| parse_rule(v, &none).unwrap();
        let start = at(2026, 1, 31, 8, 0);
        // The 31st only in months that have one.
        let m = expand(&rule("FREQ=MONTHLY;COUNT=4"), start, at(2027, 1, 1, 0, 0));
        let days: Vec<(i32, u32, u32)> = m.iter().map(|&t| civil_from_days(t.div_euclid(DAY))).collect();
        assert_eq!(days, vec![(2026, 1, 31), (2026, 3, 31), (2026, 5, 31), (2026, 7, 31)]);
        // The last Friday of each month.
        let f = expand(&rule("FREQ=MONTHLY;BYDAY=-1FR;UNTIL=20260401T000000Z"), at(2026, 1, 1, 9, 0), i64::MAX / 2);
        let days: Vec<u32> = f.iter().map(|&t| civil_from_days(t.div_euclid(DAY)).2).collect();
        assert_eq!(days, vec![30, 27, 27]);
        // Every other day, three times.
        let d = expand(&rule("FREQ=DAILY;INTERVAL=2;COUNT=3"), start, i64::MAX / 2);
        assert_eq!(d, vec![start, start + 2 * DAY, start + 4 * DAY]);
        // Yearly on the 29th of February: leap years only.
        let y = expand(&rule("FREQ=YEARLY;COUNT=2"), at(2024, 2, 29, 0, 0), i64::MAX / 2);
        assert_eq!(civil_from_days(y[1].div_euclid(DAY)), (2028, 2, 29));
    }
}
