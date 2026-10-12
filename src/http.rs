//! Minimal blocking HTTPS client on WinHTTP (built into Windows, uses the
//! system proxy settings). Only used by worker threads.

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

pub struct Response {
    pub status: u32,
    pub body: Vec<u8>,
    pub location: Option<String>,
}

/// Splits `https://host/path` into host and path.
fn split(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    Some(match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    })
}

/// GET `url` with extra `headers`. With `follow` false, redirects are
/// returned (status 3xx + `location`) instead of followed, so credentials
/// are never forwarded to another host.
pub fn get(url: &str, headers: &[(&str, &str)], follow: bool) -> Option<Response> {
    let (host, path) = split(url)?;
    unsafe {
        let session =
            WinHttpOpen(w!("Cascade"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return None;
        }
        let conn = WinHttpConnect(session, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0);
        let req = if conn.is_null() {
            std::ptr::null_mut()
        } else {
            WinHttpOpenRequest(
                conn,
                w!("GET"),
                &HSTRING::from(path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        };
        let result = (|| {
            if req.is_null() {
                return None;
            }
            if !follow {
                let off = WINHTTP_DISABLE_REDIRECTS.to_ne_bytes();
                let _ = WinHttpSetOption(Some(req), WINHTTP_OPTION_DISABLE_FEATURE, Some(&off));
            }
            let header_text: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
            let header_wide: Vec<u16> = header_text.encode_utf16().collect();
            let header_arg = (!header_wide.is_empty()).then_some(header_wide.as_slice());
            WinHttpSendRequest(req, header_arg, None, 0, 0, 0).ok()?;
            WinHttpReceiveResponse(req, std::ptr::null_mut()).ok()?;

            let mut status = 0u32;
            let mut len = 4u32;
            let _ = WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut _),
                &mut len,
                std::ptr::null_mut(),
            );
            let mut loc = [0u16; 4096];
            let mut loc_len = (loc.len() * 2) as u32;
            let location = WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_LOCATION,
                PCWSTR::null(),
                Some(loc.as_mut_ptr() as *mut _),
                &mut loc_len,
                std::ptr::null_mut(),
            )
            .ok()
            .map(|_| String::from_utf16_lossy(&loc[..loc_len as usize / 2]));

            let mut body = Vec::new();
            loop {
                let mut buf = [0u8; 16384];
                let mut read = 0u32;
                if WinHttpReadData(req, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut read).is_err() || read == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..read as usize]);
                if body.len() > 64 << 20 {
                    return None;
                }
            }
            Some(Response { status, body, location })
        })();
        if !req.is_null() {
            let _ = WinHttpCloseHandle(req);
        }
        if !conn.is_null() {
            let _ = WinHttpCloseHandle(conn);
        }
        let _ = WinHttpCloseHandle(session);
        result
    }
}

/// GET returning the body as text when the server answers 200.
pub fn get_text(url: &str) -> Option<String> {
    let r = get(url, &[], true)?;
    (r.status == 200).then(|| String::from_utf8_lossy(&r.body).into_owned())
}

/// The number after `key` in a JSON-ish string.
pub fn number_after(s: &str, key: &str) -> Option<f32> {
    let rest = &s[s.find(key)? + key.len()..];
    let rest = rest.trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The quoted string after `key` in a JSON-ish string.
pub fn string_after(s: &str, key: &str) -> Option<String> {
    let rest = &s[s.find(key)? + key.len()..];
    let rest = &rest[rest.find('"')? + 1..];
    Some(rest[..rest.find('"')?].to_string())
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

    #[test]
    fn splits_urls() {
        assert_eq!(split("https://api.github.com/repos/a/b"), Some(("api.github.com", "/repos/a/b")));
        assert_eq!(split("https://example.com"), Some(("example.com", "/")));
        assert_eq!(split("http://x"), None);
    }
}
