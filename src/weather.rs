//! Current weather from Open-Meteo (free, no API key). One small HTTPS
//! request every 30 minutes on a sleeping thread; nothing else.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

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
    let body = get(
        "api.open-meteo.com",
        &format!(
            "/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,weather_code,is_day\
             &daily=temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=1{unit}"
        ),
    )?;
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
        let body = get("ipapi.co", "/json/")?;
        let city = string_after(&body, "\"city\":").unwrap_or_default();
        return Some((number_after(&body, "\"latitude\":")?, number_after(&body, "\"longitude\":")?, city));
    }
    let query: String = location
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    let body = get("geocoding-api.open-meteo.com", &format!("/v1/search?name={query}&count=1"))?;
    let name = string_after(&body, "\"name\":").unwrap_or_else(|| location.to_string());
    Some((number_after(&body, "\"latitude\":")?, number_after(&body, "\"longitude\":")?, name))
}

fn number_after(s: &str, key: &str) -> Option<f32> {
    let rest = &s[s.find(key)? + key.len()..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn string_after(s: &str, key: &str) -> Option<String> {
    let rest = &s[s.find(key)? + key.len()..];
    let rest = &rest[rest.find('"')? + 1..];
    Some(rest[..rest.find('"')?].to_string())
}

/// A blocking HTTPS GET with WinHTTP (uses the system proxy settings).
fn get(host: &str, path: &str) -> Option<String> {
    unsafe {
        let session =
            WinHttpOpen(w!("Cascade"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return None;
        }
        let result = (|| {
            let conn = WinHttpConnect(session, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0);
            if conn.is_null() {
                return None;
            }
            let req = WinHttpOpenRequest(
                conn,
                w!("GET"),
                &HSTRING::from(path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            );
            let mut body = Vec::new();
            let ok = !req.is_null()
                && WinHttpSendRequest(req, None, None, 0, 0, 0).is_ok()
                && WinHttpReceiveResponse(req, std::ptr::null_mut()).is_ok();
            if ok {
                loop {
                    let mut buf = [0u8; 8192];
                    let mut read = 0u32;
                    if WinHttpReadData(req, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut read).is_err()
                        || read == 0
                    {
                        break;
                    }
                    body.extend_from_slice(&buf[..read as usize]);
                    if body.len() > 1 << 20 {
                        break;
                    }
                }
            }
            if !req.is_null() {
                let _ = WinHttpCloseHandle(req);
            }
            let _ = WinHttpCloseHandle(conn);
            ok.then(|| String::from_utf8_lossy(&body).into_owned())
        })();
        let _ = WinHttpCloseHandle(session);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_bits() {
        let s =
            r#"{"current":{"temperature_2m":-3.5,"weather_code":61,"is_day":0},"daily":{"temperature_2m_max":[4.1]}}"#;
        assert_eq!(number_after(s, "\"temperature_2m\":"), Some(-3.5));
        assert_eq!(number_after(s, "\"weather_code\":"), Some(61.0));
        assert_eq!(number_after(s, "\"temperature_2m_max\":["), Some(4.1));
        assert_eq!(string_after(r#"{"city": "Perth", "x":1}"#, "\"city\":"), Some("Perth".into()));
    }
}
