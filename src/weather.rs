//! Current weather from Open-Meteo (free, no API key). One small HTTPS
//! request every 30 minutes on a sleeping thread; nothing else.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::http::{get_text, number_after, string_after};
use crate::msg;

pub struct Weather {
    pub temp: f32,
    pub high: f32,
    pub low: f32,
    pub code: u32,
    pub day: bool,
    pub place: String,
    pub fahrenheit: bool,
}

impl Weather {
    pub fn describe(&self) -> &'static str {
        match self.code {
            0 => "Clear",
            1 => "Mostly clear",
            2 => "Partly cloudy",
            3 => "Overcast",
            45 | 48 => "Fog",
            51..=57 => "Drizzle",
            61..=67 => "Rain",
            71..=77 => "Snow",
            80..=82 => "Showers",
            85 | 86 => "Snow showers",
            95..=99 => "Thunderstorm",
            _ => "",
        }
    }
}

static GENERATION: AtomicU32 = AtomicU32::new(0);

/// What came of looking up the configured location (shown under the
/// Location box in Settings).
#[derive(Clone, PartialEq, Debug)]
pub enum Lookup {
    Checking,
    /// Found; the place's full name (or the coordinates).
    Found(String),
    /// Nothing by that name, or coordinates off the globe.
    NotFound,
    /// No answer from the server; it is tried again in a few minutes.
    Offline,
}

/// The location last looked up, and how that went.
static LOOKUP: Mutex<Option<(String, Lookup)>> = Mutex::new(None);

/// How looking up `location` went, if it is the one being used.
pub fn lookup(location: &str) -> Option<Lookup> {
    let guard = LOOKUP.lock().ok()?;
    let (loc, state) = guard.as_ref()?;
    (loc.trim() == location.trim()).then(|| state.clone())
}

fn set_lookup(location: &str, state: Lookup) {
    if let Ok(mut l) = LOOKUP.lock() {
        if l.as_ref().is_some_and(|(loc, s)| loc == location && *s == state) {
            return;
        }
        *l = Some((location.to_string(), state));
    }
    // Lets the settings window redraw (no weather attached).
    msg::post(msg::WEATHER, 0, 0);
}

/// Stops any running fetch thread.
pub fn stop() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// Starts fetching; a later call (e.g. after a config reload) retires the
/// previous thread.
pub fn spawn(location: String, fahrenheit: bool) {
    let gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    set_lookup(&location, Lookup::Checking);
    std::thread::spawn(move || loop {
        if GENERATION.load(Ordering::SeqCst) != gen {
            return;
        }
        let wait = match fetch(&location, fahrenheit, gen) {
            Some(_) if GENERATION.load(Ordering::SeqCst) != gen => return,
            Some(w) => {
                let ptr = Box::into_raw(Box::new(w));
                if !msg::post(msg::WEATHER, 0, ptr as isize) {
                    drop(unsafe { Box::from_raw(ptr) });
                }
                30 * 60
            }
            None => 5 * 60,
        };
        std::thread::sleep(Duration::from_secs(wait));
    });
}

fn fetch(location: &str, fahrenheit: bool, gen: u32) -> Option<Weather> {
    let found = locate(location);
    if GENERATION.load(Ordering::SeqCst) != gen {
        return None;
    }
    set_lookup(
        location,
        match &found {
            Ok(p) => Lookup::Found(p.name.clone()),
            Err(e) => e.clone(),
        },
    );
    let Place { lat, lon, town: place, .. } = found.ok()?;
    let unit = if fahrenheit { "&temperature_unit=fahrenheit" } else { "" };
    let body = get_text(&format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
         &current=temperature_2m,weather_code,is_day&daily=temperature_2m_max,temperature_2m_min\
         &timezone=auto&forecast_days=1{unit}"
    ))?;
    let current = &body[body.find("\"current\":")?..];
    let daily = &body[body.find("\"daily\":")?..];
    Some(Weather {
        temp: number_after(current, "\"temperature_2m\":")?,
        code: number_after(current, "\"weather_code\":")? as u32,
        day: number_after(current, "\"is_day\":").unwrap_or(1.0) > 0.5,
        high: number_after(daily, "\"temperature_2m_max\":[").unwrap_or(f32::NAN),
        low: number_after(daily, "\"temperature_2m_min\":[").unwrap_or(f32::NAN),
        place,
        fahrenheit,
    })
}

/// Where the weather is for.
#[derive(Debug, PartialEq)]
struct Place {
    lat: f32,
    lon: f32,
    /// In full, for Settings ("Adelaide, South Australia, Australia").
    name: String,
    /// Short, for the notch and the taskbar ("Adelaide"; empty for coordinates).
    town: String,
}

/// Latitude, longitude and a full name for the configured location: a
/// place name ("Adelaide", or "Adelaide, Australia" to pick between places
/// of the same name), "lat,lon", or empty to go by the IP address.
fn locate(location: &str) -> Result<Place, Lookup> {
    let location = location.trim();
    if let Some((a, b)) = location.split_once(',') {
        if let (Ok(lat), Ok(lon)) = (a.trim().parse::<f32>(), b.trim().parse::<f32>()) {
            if !((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)) {
                return Err(Lookup::NotFound);
            }
            return Ok(Place { lat, lon, name: format!("{lat:.3}, {lon:.3}"), town: String::new() });
        }
    }
    if location.is_empty() {
        let body = get_text("https://ipapi.co/json/").ok_or(Lookup::Offline)?;
        let city = string_after(&body, "\"city\":").unwrap_or_default();
        let region = string_after(&body, "\"region\":").unwrap_or_default();
        let country = string_after(&body, "\"country_name\":").unwrap_or_default();
        let lat = number_after(&body, "\"latitude\":").ok_or(Lookup::Offline)?;
        let lon = number_after(&body, "\"longitude\":").ok_or(Lookup::Offline)?;
        return Ok(Place { lat, lon, name: join_name(&[&city, &region, &country]), town: city });
    }
    // "Town, region or country": search for the town, then prefer the match
    // in that region or country.
    let (name, within) = match location.split_once(',') {
        Some((n, w)) => (n.trim(), w.trim().to_lowercase()),
        None => (location, String::new()),
    };
    let query: String = name
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    let body = get_text(&format!("https://geocoding-api.open-meteo.com/v1/search?name={query}&count=10"))
        .ok_or(Lookup::Offline)?;
    let places = parse_places(&body);
    let best = places
        .iter()
        .find(|p| !within.is_empty() && p.3.iter().any(|part| part.to_lowercase().contains(&within)))
        .or_else(|| places.first())
        .ok_or(Lookup::NotFound)?;
    Ok(Place { lat: best.0, lon: best.1, name: best.2.clone(), town: best.3[0].clone() })
}

/// Results of a geocoding search: latitude, longitude, full name, and the
/// parts it was made from (town, region, country, country code).
fn parse_places(body: &str) -> Vec<(f32, f32, String, Vec<String>)> {
    let Some(start) = body.find("\"results\":") else { return Vec::new() };
    body[start..]
        .split("{\"id\":")
        .skip(1)
        .filter_map(|r| {
            let lat = number_after(r, "\"latitude\":")?;
            let lon = number_after(r, "\"longitude\":")?;
            let parts: Vec<String> = ["\"name\":", "\"admin1\":", "\"country\":", "\"country_code\":"]
                .iter()
                .map(|k| string_after(r, k).unwrap_or_default())
                .collect();
            let name = join_name(&[&parts[0], &parts[1], &parts[2]]);
            Some((lat, lon, name, parts))
        })
        .collect()
}

/// "Town, Region, Country", skipping empty and repeated parts.
fn join_name(parts: &[&str]) -> String {
    let mut out: Vec<&str> = Vec::new();
    for p in parts.iter().map(|p| p.trim()) {
        if !p.is_empty() && !out.contains(&p) {
            out.push(p);
        }
    }
    out.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_places_from_a_search() {
        let body = r#"{"results":[{"id":1,"name":"Adelaide","latitude":-34.9,"longitude":138.6,"country_code":"AU","admin1_id":5,"country":"Australia","admin1":"South Australia"},{"id":2,"name":"Adelaide","latitude":-32.7,"longitude":26.3,"country_code":"ZA","country":"South Africa","admin1":"Eastern Cape"}],"generationtime_ms":0.5}"#;
        let places = parse_places(body);
        assert_eq!(places.len(), 2);
        assert_eq!(places[0].2, "Adelaide, South Australia, Australia");
        assert_eq!(places[1].3[3], "ZA");
        assert!(parse_places(r#"{"generationtime_ms":0.5}"#).is_empty());
    }

    #[test]
    fn checks_coordinates() {
        assert_eq!(locate("100,20"), Err(Lookup::NotFound));
        assert_eq!(locate("-34.9, 138.6").map(|p| p.town), Ok(String::new()));
    }

    #[test]
    fn joins_names() {
        assert_eq!(join_name(&["Perth", "Western Australia", "Australia"]), "Perth, Western Australia, Australia");
        assert_eq!(join_name(&["Singapore", "", "Singapore"]), "Singapore");
    }
}
