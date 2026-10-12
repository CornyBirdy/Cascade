//! Current weather from Open-Meteo (free, no API key). One small HTTPS
//! request every 30 minutes on a sleeping thread; nothing else.

use std::sync::atomic::{AtomicU32, Ordering};
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

/// Stops any running fetch thread.
pub fn stop() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// Starts fetching; a later call (e.g. after a config reload) retires the
/// previous thread.
pub fn spawn(location: String, fahrenheit: bool) {
    let gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || loop {
        if GENERATION.load(Ordering::SeqCst) != gen {
            return;
        }
        let wait = match fetch(&location, fahrenheit) {
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

fn fetch(location: &str, fahrenheit: bool) -> Option<Weather> {
    let (lat, lon, place) = locate(location)?;
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

/// Latitude, longitude and a display name for the configured location.
fn locate(location: &str) -> Option<(f32, f32, String)> {
    let location = location.trim();
    if let Some((a, b)) = location.split_once(',') {
        if let (Ok(lat), Ok(lon)) = (a.trim().parse(), b.trim().parse()) {
            return Some((lat, lon, String::new()));
        }
    }
    if location.is_empty() {
        let body = get_text("https://ipapi.co/json/")?;
        let city = string_after(&body, "\"city\":").unwrap_or_default();
        return Some((number_after(&body, "\"latitude\":")?, number_after(&body, "\"longitude\":")?, city));
    }
    let query: String = location
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    let body = get_text(&format!("https://geocoding-api.open-meteo.com/v1/search?name={query}&count=1"))?;
    let name = string_after(&body, "\"name\":").unwrap_or_else(|| location.to_string());
    Some((number_after(&body, "\"latitude\":")?, number_after(&body, "\"longitude\":")?, name))
}
