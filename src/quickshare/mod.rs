//! Quick Share, built in (Settings > Bluetooth & devices > Quick Share):
//! send files to Android phones and PCs nearby, and take what they send,
//! over your Wi-Fi, with no app of Google's.
//!
//! - **Receiving**: Cascade listens on a port and advertises itself on the
//!   network (`mdns`), so phones list "this PC" when they share. A phone
//!   that connects shakes hands (`conn`, `crypto`), says what it's sending,
//!   and a card (`card`) asks: who, what, the PIN both screens show, Accept
//!   or Decline. Files land in Downloads (or the folder picked); a link
//!   opens; a text is saved and copied.
//! - **Sending**: "Share with Quick Share" (in right-click menus, Files'
//!   Share button, or Settings) opens a window (`send`) that lists what's
//!   nearby (phones show up when their Quick Share can be seen, woken by
//!   Cascade's Bluetooth advert, see `ble`) and sends to the one picked.

pub mod ble;
pub mod card;
pub mod conn;
pub mod crypto;
pub mod mdns;
pub mod proto;
pub mod send;

use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use conn::{Conn, Event, Offer};

/// The settings (from the config).
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    pub on: bool,
    pub name: String,
    pub folder: String,
    pub auto_accept: bool,
    pub menus: bool,
}

static SETUP: Mutex<Option<Setup>> = Mutex::new(None);
static STATUS: Mutex<String> = Mutex::new(String::new());

fn setup() -> Option<Setup> {
    SETUP.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set_status(s: impl Into<String>) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = s.into();
}

/// What it's doing, for Settings.
pub fn status() -> String {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Our 4-character endpoint id: kept in the data folder, so the window that
/// sends (a process of its own) knows the receiver's advert is this PC.
pub fn endpoint_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let path = crate::config::dir().join("quickshare-id");
        if let Ok(s) = std::fs::read_to_string(&path) {
            let s = s.trim().to_string();
            if s.len() == 4 && s.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return s;
            }
        }
        const A: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let id: String = crypto::random(4).iter().map(|b| A[*b as usize % A.len()] as char).collect();
        let _ = std::fs::create_dir_all(crate::config::dir());
        let _ = std::fs::write(&path, &id);
        id
    })
}

/// Whether endpoint `id` is this PC.
pub fn is_me(id: &str) -> bool {
    id == endpoint_id()
}

/// The name others see: the one set, or the PC's.
pub fn my_name(set: &str) -> String {
    let set = set.trim();
    if !set.is_empty() {
        return set.to_string();
    }
    std::env::var("COMPUTERNAME")
        .map(|n| {
            // DESKTOP-ABC123 reads better as it is; others in title case.
            let mut c = n.chars();
            match c.next() {
                Some(f) if !n.contains('-') => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                _ => n.clone(),
            }
        })
        .unwrap_or_else(|_| "Windows PC".into())
}

/// Where received files go: the folder set, or Downloads.
pub fn folder(set: &str) -> PathBuf {
    let set = set.trim();
    if !set.is_empty() && Path::new(set).is_dir() {
        return PathBuf::from(set);
    }
    crate::apps::known_folder(&windows::Win32::UI::Shell::FOLDERID_Downloads)
        .unwrap_or_else(|| PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Downloads"))
}

/// The receiving side while it's on.
struct Server {
    stop: Arc<AtomicBool>,
    port: u16,
    name: String,
    advert: Option<mdns::Advert>,
    _watch: Option<ble::Watch>,
}

static SERVER: Mutex<Option<Server>> = Mutex::new(None);

/// Applies the settings: starts or stops receiving, renames us, and puts
/// "Share with Quick Share" in right-click menus (or takes it out).
pub fn configure(cfg: &crate::config::Config) {
    let s = Setup {
        on: cfg.quickshare,
        name: cfg.quickshare_name.clone(),
        folder: cfg.quickshare_folder.clone(),
        auto_accept: cfg.quickshare_accept == "auto",
        menus: cfg.quickshare_menus,
    };
    let before = SETUP.lock().unwrap_or_else(|e| e.into_inner()).replace(s.clone());
    if before.as_ref().map(|b| b.menus && b.on) != Some(s.menus && s.on) {
        let on = s.menus && s.on;
        std::thread::spawn(move || menus(on));
    }
    let name = my_name(&s.name);
    let mut server = SERVER.lock().unwrap_or_else(|e| e.into_inner());
    match (&mut *server, s.on) {
        (None, true) => *server = start(&name),
        (Some(_), false) => {
            // Stopped once the lock's let go (Bluetooth's callback takes it too).
            let old = server.take();
            drop(server);
            if let Some(sv) = old {
                stop(sv);
            }
            set_status("Off");
        }
        (Some(sv), true) if sv.name != name => {
            // Renamed: advertised again under the new name.
            sv.advert = mdns::advertise(endpoint_id(), &conn::endpoint_info(&name, 3), sv.port);
            sv.name = name.clone();
            set_status(format!("Visible as \u{201c}{name}\u{201d} to phones and PCs on your Wi-Fi."));
        }
        _ => {}
    }
}

fn start(name: &str) -> Option<Server> {
    let listener = match TcpListener::bind(("0.0.0.0", 0)) {
        Ok(l) => l,
        Err(e) => {
            set_status(format!("Couldn't start: {e}"));
            return None;
        }
    };
    let port = listener.local_addr().ok()?.port();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if flag.load(Ordering::Relaxed) {
                break;
            }
            if let Ok(s) = stream {
                std::thread::spawn(move || {
                    if let Err(e) = receive(s) {
                        crate::config::log(&format!("quickshare: incoming transfer ended: {e}"));
                    }
                });
            }
        }
    });
    let advert = mdns::advertise(endpoint_id(), &conn::endpoint_info(name, 3), port);
    // A phone nearby starting to share: advertised afresh (a phone that began
    // looking before we advertised can miss us otherwise).
    let watch = ble::watch(|| {
        let mut server = SERVER.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(sv) = server.as_mut() {
            sv.advert = None;
            sv.advert = mdns::advertise(endpoint_id(), &conn::endpoint_info(&sv.name, 3), sv.port);
        }
    });
    set_status(if advert.is_some() {
        format!("Visible as \u{201c}{name}\u{201d} to phones and PCs on your Wi-Fi.")
    } else {
        "Couldn't advertise on the network (Windows' DNS service may be off).".into()
    });
    crate::config::log(&format!("quickshare: receiving on port {port} as {name}"));
    Some(Server { stop, port, name: name.to_string(), advert, _watch: watch })
}

fn stop(sv: Server) {
    sv.stop.store(true, Ordering::Relaxed);
    // Wakes the listener so it sees it's to stop.
    let _ = TcpStream::connect_timeout(&([127, 0, 0, 1], sv.port).into(), Duration::from_millis(300));
}

/// A file name made safe to save: no folders, no characters Windows refuses.
fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = base.chars().map(|c| if "<>:\"|?*".contains(c) || c.is_control() { '_' } else { c }).collect();
    let clean = clean.trim().trim_matches('.').to_string();
    if clean.is_empty() {
        "file".into()
    } else {
        clean
    }
}

/// `dir/name`, or `name (2)`, `name (3)`... if that's taken.
fn unique(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap_or(first)
}

/// "3 files, 12.4 MB" and the like.
fn summary(offer: &Offer) -> String {
    let size = crate::appwin::size_text(offer.total().max(0) as u64);
    match (offer.files.len(), offer.texts.first()) {
        (0, Some(t)) if t.kind == 2 => "A link".into(),
        (0, Some(_)) => "Some text".into(),
        (1, _) => format!("1 file \u{b7} {size}"),
        (n, _) => format!("{n} files \u{b7} {size}"),
    }
}

/// A phone (or PC) connected to send us something: the handshake, the
/// question, then the files.
fn receive(stream: TcpStream) -> io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_nodelay(true);
    let mut c = Conn::new(stream.try_clone()?);
    let device = c.accept()?;
    let pin = c.pin();
    let offer = c.offer()?;
    let setup = setup().unwrap_or(Setup {
        on: true,
        name: String::new(),
        folder: String::new(),
        auto_accept: false,
        menus: true,
    });
    let who = if device.name.is_empty() { "A device".to_string() } else { device.name.clone() };
    crate::config::log(&format!("quickshare: {who} offers {}", summary(&offer)));
    let mut names: Vec<String> = offer.files.iter().map(|f| f.name.clone()).collect();
    names.extend(offer.texts.iter().map(|t| t.title.clone()));
    let card = card::show(card::View::Ask { device: who.clone(), summary: summary(&offer), names, pin });
    // Waiting for an answer: keep-alives answered meanwhile (two minutes at most).
    let accepted = if setup.auto_accept {
        true
    } else {
        let asked = Instant::now();
        let _ = stream.set_read_timeout(None);
        loop {
            if let Some(d) = card.decision() {
                break d;
            }
            if asked.elapsed() > Duration::from_secs(120) {
                break false;
            }
            if c.poll().is_err() {
                card.set(card::View::Failed { title: "Cancelled".into(), detail: format!("{who} stopped sharing.") });
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    c.answer(accepted)?;
    if !accepted {
        return Ok(());
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let dir = folder(&setup.folder);
    let total = offer.total().max(1);
    let title = format!("Receiving from {who}");
    card.set(card::View::Progress { title: title.clone(), done: 0, total });
    // The files, opened as they're offered.
    struct Incoming {
        id: i64,
        path: PathBuf,
        file: Option<std::fs::File>,
        written: i64,
        done: bool,
    }
    let mut files: Vec<Incoming> = offer
        .files
        .iter()
        .map(|f| {
            let path = unique(&dir, &safe_name(&f.name));
            let file = std::fs::File::create(&path).ok();
            Incoming { id: f.id, path, file, written: 0, done: false }
        })
        .collect();
    let mut texts_left: Vec<i64> = offer.texts.iter().map(|t| t.id).collect();
    let mut link: Option<String> = None;
    let mut got = 0i64;
    let mut last_shown = Instant::now();
    let mut pending = c.take_held();
    let result: io::Result<()> = (|| loop {
        if card.cancelled() {
            c.cancel();
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        if files.iter().all(|f| f.done) && texts_left.is_empty() {
            c.disconnect();
            return Ok(());
        }
        let ev = if pending.is_empty() {
            c.next()?
        } else {
            let (id, data) = pending.remove(0);
            Event::Bytes { id, data }
        };
        match ev {
            Event::File { id, offset, body, last } => {
                if let Some(f) = files.iter_mut().find(|f| f.id == id) {
                    if offset != f.written {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "a file's pieces came out of order"));
                    }
                    f.written += body.len() as i64;
                    if let Some(file) = f.file.as_mut() {
                        use std::io::Write;
                        file.write_all(&body)?;
                    }
                    got += body.len() as i64;
                    if last {
                        f.file = None;
                        f.done = true;
                    }
                }
            }
            Event::Bytes { id, data } => {
                if let Some(i) = texts_left.iter().position(|&t| t == id) {
                    texts_left.remove(i);
                    let text = String::from_utf8_lossy(&data).into_owned();
                    let kind = offer.texts.iter().find(|t| t.id == id).map_or(1, |t| t.kind);
                    got += data.len() as i64;
                    if kind == 2 {
                        link = Some(text.trim().to_string());
                    } else {
                        // Text: copied, and kept as a file.
                        crate::clipboard::copy(&text);
                        let path = unique(&dir, &crate::apps::stamped("Quick Share text", "txt"));
                        if std::fs::write(&path, text.as_bytes()).is_ok() {
                            files.push(Incoming { id, path, file: None, written: 0, done: true });
                        }
                    }
                }
            }
            Event::Disconnect => {
                if files.iter().all(|f| f.done) && texts_left.is_empty() {
                    return Ok(());
                }
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "they stopped partway"));
            }
        }
        if last_shown.elapsed() > Duration::from_millis(100) {
            last_shown = Instant::now();
            card.set(card::View::Progress { title: title.clone(), done: got, total });
        }
    })();
    match result {
        Ok(()) => {
            let paths: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
            let detail = match (&link, paths.len()) {
                (Some(l), _) => l.clone(),
                (None, 1) => paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                (None, n) => format!(
                    "{n} files in {}",
                    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                ),
            };
            if let Some(l) = &link {
                crate::shell::open(l);
            }
            card.set(card::View::Done { title: format!("From {who}"), detail, paths, link });
            crate::config::log(&format!("quickshare: received from {who}"));
            Ok(())
        }
        Err(e) => {
            // What came only partly is no use: removed.
            for f in files.iter().filter(|f| !f.done) {
                let _ = std::fs::remove_file(&f.path);
            }
            let detail = if e.kind() == io::ErrorKind::Interrupted {
                "The transfer was cancelled.".to_string()
            } else {
                format!("{e}.")
            };
            card.set(card::View::Failed { title: "Didn't arrive".into(), detail });
            Err(e)
        }
    }
}

// ------------------------------------------------------- right-click menus --

const VERB: &str = "Software\\Classes\\*\\shell\\CascadeQuickShare";

/// "Share with Quick Share" in Windows' right-click menus (Explorer's, and
/// the desktop's and Files', which show Windows' own), and in Send to.
fn menus(on: bool) {
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;
    let Ok(exe) = std::env::current_exe() else { return };
    let exe = exe.to_string_lossy().into_owned();
    let sendto =
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Microsoft\\Windows\\SendTo\\Quick Share.lnk"));
    if on {
        crate::winset::set_reg_text(HKEY_CURRENT_USER, VERB, "", "Share with Quick Share");
        crate::winset::set_reg_text(HKEY_CURRENT_USER, VERB, "Icon", &format!("\"{exe}\",0"));
        crate::winset::set_reg_text(HKEY_CURRENT_USER, VERB, "MultiSelectModel", "Player");
        crate::winset::set_reg_text(
            HKEY_CURRENT_USER,
            &format!("{VERB}\\command"),
            "",
            &format!("\"{exe}\" --quickshare-send \"%1\""),
        );
        if let Some(link) = sendto {
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                );
            }
            let _ = crate::shell::create_link(&link, Path::new(&exe), "--quickshare-send", "Send with Quick Share");
        }
    } else {
        unsafe {
            let _ = windows::Win32::System::Registry::RegDeleteTreeW(
                HKEY_CURRENT_USER,
                &windows::core::HSTRING::from(VERB),
            );
        }
        if let Some(link) = sendto {
            let _ = std::fs::remove_file(link);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_safe_and_unique() {
        assert_eq!(safe_name("../../evil.txt"), "evil.txt");
        assert_eq!(safe_name("a:b?.jpg"), "a_b_.jpg");
        assert_eq!(safe_name("..."), "file");
        let dir = std::env::temp_dir().join(format!("qs-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let first = unique(&dir, "x.txt");
        std::fs::write(&first, b"1").unwrap();
        assert_eq!(unique(&dir, "x.txt").file_name().unwrap(), "x (2).txt");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
