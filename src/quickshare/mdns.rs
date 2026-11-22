//! Being seen on the network, and seeing others: Quick Share devices
//! advertise themselves with DNS-SD over multicast DNS (service
//! `_FC9F5ED42C8A._tcp`, an instance name holding the device's 4-character
//! id, a TXT record `n` holding its name and kind). Windows does both itself
//! (`DnsServiceRegister`, `DnsServiceBrowse`, `DnsServiceResolve`), so no
//! responder of Cascade's own has to share port 5353 with Windows'.

use std::ffi::c_void;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::NetworkManagement::Dns::*;

use super::conn::{parse_endpoint_info, Device};

const SERVICE: &str = "_FC9F5ED42C8A._tcp.local";

/// URL-safe base64 without padding, as Quick Share uses it.
pub fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

pub fn unb64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::new();
    for c in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &ch) in c.iter().enumerate() {
            n |= val(ch)? << (18 - 6 * i);
        }
        for i in 0..c.len().saturating_sub(1) {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

/// The instance name for endpoint `id`: 0x23, the id, the service's hash, two zeros.
pub fn instance_name(id: &str) -> String {
    let mut b = vec![0x23];
    b.extend(id.bytes().take(4));
    b.extend([0xFC, 0x9F, 0x5E, 0, 0]);
    b64(&b)
}

/// The endpoint id in an instance name, if it's a Quick Share one.
pub fn endpoint_of(instance: &str) -> Option<String> {
    let label = instance.split('.').next()?;
    let b = unb64(label)?;
    (b.len() >= 10 && b[0] == 0x23 && b[5..8] == [0xFC, 0x9F, 0x5E])
        .then(|| String::from_utf8_lossy(&b[1..5]).into_owned())
}

/// A registration: dropped, it's taken back.
pub struct Advert {
    request: Box<DNS_SERVICE_REGISTER_REQUEST>,
    instance: *mut DNS_SERVICE_INSTANCE,
}

// The pointers are only used to take the registration back.
unsafe impl Send for Advert {}

unsafe extern "system" fn registered(status: u32, _ctx: *const c_void, instance: *const DNS_SERVICE_INSTANCE) {
    if status != 0 {
        crate::config::log(&format!("quickshare: couldn't advertise on the network ({status})"));
    }
    if !instance.is_null() {
        DnsServiceFreeInstance(instance);
    }
}

/// Advertises us as Quick Share endpoint `id`, called `name`, on `port`.
pub fn advertise(id: &str, info: &[u8], port: u16) -> Option<Advert> {
    let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into());
    let full = HSTRING::from(format!("{}.{SERVICE}", instance_name(id)));
    let host = HSTRING::from(format!("{host}.local"));
    let key = HSTRING::from("n");
    let value = HSTRING::from(b64(info));
    let keys = [PCWSTR(key.as_ptr())];
    let values = [PCWSTR(value.as_ptr())];
    unsafe {
        let instance = DnsServiceConstructInstance(
            PCWSTR(full.as_ptr()),
            PCWSTR(host.as_ptr()),
            None,
            None,
            port,
            0,
            0,
            1,
            keys.as_ptr(),
            values.as_ptr(),
        );
        if instance.is_null() {
            return None;
        }
        let request = Box::new(DNS_SERVICE_REGISTER_REQUEST {
            Version: DNS_QUERY_REQUEST_VERSION1.0,
            InterfaceIndex: 0,
            pServiceInstance: instance,
            pRegisterCompletionCallback: Some(registered),
            pQueryContext: std::ptr::null_mut(),
            hCredentials: Default::default(),
            unicastEnabled: false.into(),
        });
        let status = DnsServiceRegister(&*request, None);
        // DNS_REQUEST_PENDING: it's under way.
        if status != 9506 {
            crate::config::log(&format!("quickshare: DnsServiceRegister failed ({status})"));
            DnsServiceFreeInstance(instance);
            return None;
        }
        Some(Advert { request, instance })
    }
}

impl Drop for Advert {
    fn drop(&mut self) {
        unsafe {
            let _ = DnsServiceDeRegister(&*self.request, None);
            let _ = self.instance;
        }
    }
}

/// A device found on the network that we could send to.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub id: String,
    pub device: Device,
    pub addr: Ipv4Addr,
    pub port: u16,
}

/// What a browse has found so far (shared with Windows' callbacks).
pub type FoundList = Arc<Mutex<Vec<Found>>>;

struct BrowseCtx {
    found: FoundList,
}

/// A browse in progress: dropped, it stops.
pub struct Browse {
    cancel: Box<DNS_SERVICE_CANCEL>,
    _ctx: Box<BrowseCtx>,
    _name: HSTRING,
}

unsafe impl Send for Browse {}

/// A resolve's context: where to put what it finds, and the name asked
/// about (kept until Windows answers).
struct ResolveCtx {
    found: FoundList,
    _name: Vec<u16>,
}

unsafe extern "system" fn resolved(status: u32, ctx: *const c_void, inst: *const DNS_SERVICE_INSTANCE) {
    // The context was handed over in `resolve`; taken back (and freed) here.
    let ctx = Box::from_raw(ctx as *mut ResolveCtx);
    let found = &ctx.found;
    if status != 0 || inst.is_null() {
        if !inst.is_null() {
            DnsServiceFreeInstance(inst);
        }
        return;
    }
    let i = &*inst;
    let name = i.pszInstanceName.to_string().unwrap_or_default();
    let Some(id) = endpoint_of(&name) else {
        DnsServiceFreeInstance(inst);
        return;
    };
    let mut info = None;
    for k in 0..i.dwPropertyCount as usize {
        let key = (*i.keys.add(k)).to_string().unwrap_or_default();
        if key == "n" {
            let v = (*i.values.add(k)).to_string().unwrap_or_default();
            info = unb64(&v).and_then(|b| parse_endpoint_info(&b));
        }
    }
    let addr = if i.ip4Address.is_null() { None } else { Some(Ipv4Addr::from(u32::from_be(*i.ip4Address))) };
    if let (Some(device), Some(addr)) = (info, addr) {
        // Not ourselves (a PC browsing sees its own advert too).
        if !device.name.is_empty() && !super::is_me(&id) {
            let f = Found { id: id.clone(), device, addr, port: i.wPort };
            let mut list = found.lock().unwrap_or_else(|e| e.into_inner());
            list.retain(|x| x.id != id);
            list.push(f);
        }
    }
    DnsServiceFreeInstance(inst);
}

unsafe extern "system" fn browsed(status: u32, ctx: *const c_void, record: *const DNS_RECORDW) {
    if status != 0 || record.is_null() {
        if !record.is_null() {
            DnsFree(Some(record as *const c_void), DnsFreeRecordList);
        }
        return;
    }
    let bctx = &*(ctx as *const BrowseCtx);
    let mut r = record;
    while !r.is_null() {
        let rec = &*r;
        if rec.wType == DNS_TYPE_PTR.0 {
            let target = rec.Data.PTR.pNameHost.to_string().unwrap_or_default();
            if rec.dwTtl == 0 {
                // Gone (a goodbye).
                if let Some(id) = endpoint_of(&target) {
                    bctx.found.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| f.id != id);
                }
            } else if endpoint_of(&target).is_some() {
                resolve(&target, bctx.found.clone());
            }
        }
        r = rec.pNext;
    }
    DnsFree(Some(record as *const c_void), DnsFreeRecordList);
}

/// Asks where instance `name` is (its address, port and name record).
fn resolve(name: &str, found: FoundList) {
    let mut ctx = Box::new(ResolveCtx { found, _name: name.encode_utf16().chain([0]).collect() });
    let query = PWSTR(ctx._name.as_mut_ptr());
    let raw = Box::into_raw(ctx);
    let request = DNS_SERVICE_RESOLVE_REQUEST {
        Version: DNS_QUERY_REQUEST_VERSION1.0,
        InterfaceIndex: 0,
        QueryName: query,
        pResolveCompletionCallback: Some(resolved),
        pQueryContext: raw as *mut c_void,
    };
    let mut cancel = DNS_SERVICE_CANCEL::default();
    unsafe {
        if DnsServiceResolve(&request, &mut cancel) != 9506 {
            // Never started: take the context back.
            drop(Box::from_raw(raw));
        }
    }
}

/// Starts looking for Quick Share devices; what's found lands in `found`.
pub fn browse(found: FoundList) -> Option<Browse> {
    let name = HSTRING::from(SERVICE);
    let ctx = Box::new(BrowseCtx { found });
    let request = DNS_SERVICE_BROWSE_REQUEST {
        Version: DNS_QUERY_REQUEST_VERSION1.0,
        InterfaceIndex: 0,
        QueryName: PCWSTR(name.as_ptr()),
        Anonymous: DNS_SERVICE_BROWSE_REQUEST_0 { pBrowseCallback: Some(browsed) },
        pQueryContext: &*ctx as *const BrowseCtx as *mut c_void,
    };
    let mut cancel = Box::new(DNS_SERVICE_CANCEL::default());
    unsafe {
        let status = DnsServiceBrowse(&request, &mut *cancel);
        if status != 9506 {
            crate::config::log(&format!("quickshare: DnsServiceBrowse failed ({status})"));
            return None;
        }
    }
    Some(Browse { cancel, _ctx: ctx, _name: name })
}

impl Drop for Browse {
    fn drop(&mut self) {
        unsafe {
            let _ = DnsServiceBrowseCancel(&*self.cancel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        let n = instance_name("Ab3x");
        assert_eq!(endpoint_of(&format!("{n}._FC9F5ED42C8A._tcp.local")).as_deref(), Some("Ab3x"));
        assert_eq!(unb64(&b64(b"hello world!?")).unwrap(), b"hello world!?");
        assert_eq!(b64(&[0xFB, 0xFF]), "-_8");
        assert!(endpoint_of("printer._ipp._tcp.local").is_none());
    }
}
