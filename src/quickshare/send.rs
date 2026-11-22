//! Sending with Quick Share (`cascade.exe --quickshare-send <files>`): a
//! window listing what's nearby, then the PIN to check on the other screen,
//! then the files going across. Opened by "Share with Quick Share" in
//! right-click menus (Explorer starts one copy per file picked: the first
//! window takes the rest), Files' Share button, and Settings.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::conn::{Conn, CHUNK};
use super::mdns::{self, Found, FoundList};
use crate::apps::{App, Ctx, Kit, Look};
use crate::gfx::{Align, Color, Fonts, Painter};

/// Files handed over by another copy (see `run`).
const ADD_FILES: u32 = WM_APP + 20;

fn inbox() -> PathBuf {
    crate::config::dir().join("quickshare-send.txt")
}

/// Opens the window for `files`, or hands them to the one that's open.
pub fn run(files: Vec<String>) {
    let class = w!("CascadeQuickShareSend");
    unsafe {
        // One window: copies started for more files hand them over and go.
        let mutex = windows::Win32::System::Threading::CreateMutexW(None, true, w!("Local\\CascadeQuickShareSend"));
        let first = GetLastError() != ERROR_ALREADY_EXISTS;
        if !first {
            for _ in 0..30 {
                if let Ok(win) = FindWindowW(class, None) {
                    if !files.is_empty() {
                        use std::io::Write;
                        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(inbox()) {
                            for p in &files {
                                let _ = writeln!(f, "{p}");
                            }
                        }
                        let _ = PostMessageW(Some(win), ADD_FILES, WPARAM(0), LPARAM(0));
                    }
                    let _ = SetForegroundWindow(win);
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let _ = std::fs::remove_file(inbox());
        crate::apps::run(class, "Quick Share", "Cascade.QuickShare", 480, 600, move |_| Box::new(Sender::new(files)));
        drop(mutex);
    }
}

/// How a send is going.
#[derive(Clone, Debug, PartialEq)]
enum State {
    Connecting,
    /// Connected: check this PIN on their screen; waiting for them to accept.
    Pin(String),
    Sending {
        pin: String,
        done: i64,
        total: i64,
    },
    Done,
    Failed(String),
}

struct Job {
    to: String,
    state: State,
    cancel: bool,
    socket: Option<std::net::TcpStream>,
}

struct Sender {
    files: Vec<PathBuf>,
    found: FoundList,
    _browse: Option<mdns::Browse>,
    _beacon: Option<super::ble::Beacon>,
    job: Option<Arc<Mutex<Job>>>,
    started: Instant,
}

impl Sender {
    fn new(files: Vec<String>) -> Sender {
        let found: FoundList = Arc::default();
        let mut s = Sender {
            files: Vec::new(),
            _browse: mdns::browse(found.clone()),
            _beacon: super::ble::beacon(),
            found,
            job: None,
            started: Instant::now(),
        };
        s.add(files);
        s
    }

    fn add(&mut self, files: Vec<String>) {
        for f in files {
            let p = PathBuf::from(f.trim().trim_matches('"'));
            if p.is_file() && !self.files.contains(&p) {
                self.files.push(p);
            }
        }
    }

    fn total(&self) -> u64 {
        self.files.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
    }

    fn start(&mut self, to: Found) {
        let job = Arc::new(Mutex::new(Job {
            to: to.device.name.clone(),
            state: State::Connecting,
            cancel: false,
            socket: None,
        }));
        self.job = Some(job.clone());
        let files = self.files.clone();
        std::thread::spawn(move || {
            let result = send(&to, &files, &job);
            let mut j = job.lock().unwrap_or_else(|e| e.into_inner());
            j.state = match result {
                Ok(()) => State::Done,
                Err(_) if j.cancel => State::Failed("Cancelled.".into()),
                Err(e) => State::Failed(match e.kind() {
                    std::io::ErrorKind::PermissionDenied => "They declined.".into(),
                    std::io::ErrorKind::ConnectionRefused => "They turned the connection down.".into(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                        "They didn't answer in time.".into()
                    }
                    _ => format!("It stopped: {e}."),
                }),
            };
        });
    }
}

/// A file's MIME type, from its extension (what the other side shows it as).
fn mime(p: &std::path::Path) -> &'static str {
    match p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("heic") => "image/heic",
        Some("bmp") => "image/bmp",
        Some("mp4") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("mkv") => "video/x-matroska",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("wav") => "audio/wav",
        Some("flac") => "audio/flac",
        Some("ogg") => "audio/ogg",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("zip") => "application/zip",
        Some("apk") => "application/vnd.android.package-archive",
        _ => "application/octet-stream",
    }
}

/// Sends `files` to `to`, telling `job` how it's going.
fn send(to: &Found, files: &[PathBuf], job: &Arc<Mutex<Job>>) -> std::io::Result<()> {
    use std::io::Read;
    let addr = std::net::SocketAddr::from((to.addr, to.port));
    let stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(6))?;
    let _ = stream.set_nodelay(true);
    job.lock().unwrap_or_else(|e| e.into_inner()).socket = stream.try_clone().ok();
    let cfg = crate::config::Config::load();
    let mut c = Conn::new(stream);
    c.connect(&super::my_name(&cfg.quickshare_name), super::endpoint_id())?;
    let pin = c.pin();
    job.lock().unwrap_or_else(|e| e.into_inner()).state = State::Pin(pin.clone());
    let metas: Vec<(String, i64, String, i64)> = files
        .iter()
        .map(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            let size = std::fs::metadata(p).map(|m| m.len() as i64).unwrap_or(0);
            (name, size, mime(p).to_string(), super::crypto::random_i64())
        })
        .collect();
    if !c.introduce(&metas)? {
        c.disconnect();
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "declined"));
    }
    let total: i64 = metas.iter().map(|m| m.1).sum();
    let mut done = 0i64;
    let mut buf = vec![0u8; CHUNK];
    for (p, (_, size, _, id)) in files.iter().zip(&metas) {
        let mut f = std::fs::File::open(p)?;
        let mut off = 0i64;
        loop {
            if job.lock().unwrap_or_else(|e| e.into_inner()).cancel {
                c.cancel();
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
            }
            let n = f.read(&mut buf)?;
            if n == 0 && off >= *size {
                break;
            }
            c.poll()?;
            c.send_file_chunk(*id, *size, off, &buf[..n])?;
            off += n as i64;
            done += n as i64;
            job.lock().unwrap_or_else(|e| e.into_inner()).state = State::Sending { pin: pin.clone(), done, total };
            if n == 0 || off >= *size {
                break;
            }
        }
    }
    c.disconnect();
    Ok(())
}

fn kind_glyph(kind: u8) -> u32 {
    match kind {
        1 => 0xE8EA, // phone
        2 => 0xE70A, // tablet
        _ => 0xE7F8, // computer
    }
}

impl App for Sender {
    fn draw(&mut self, p: &Painter, f: &Fonts, w: f32, h: f32, kit: &mut Kit) {
        let accent = crate::theme::accent();
        let x = 24.0;
        let cw = w - 48.0;
        p.text("Send with Quick Share", &f.big, x, 56.0, cw, 40.0, Align::Left, Color::white(0.96));
        // What's being sent.
        let fy = 108.0;
        p.rrect(x, fy, cw, 64.0, 8.0, Color::white(0.05));
        if let Some(ic) = &f.icons {
            p.text("\u{E8A5}", ic, x + 14.0, fy, 24.0, 64.0, Align::Center, Color::white(0.8));
        }
        let what = match self.files.len() {
            0 => "Nothing picked yet".to_string(),
            1 => self.files[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            n => format!("{n} files"),
        };
        p.text(&what, &f.body, x + 50.0, fy + 12.0, cw - 190.0, 20.0, Align::Left, Color::white(0.94));
        p.text(
            &crate::appwin::size_text(self.total()),
            &f.small,
            x + 50.0,
            fy + 32.0,
            cw - 190.0,
            18.0,
            Align::Left,
            Color::white(0.55),
        );
        let busy = self.job.is_some();
        if !busy {
            kit.button(p, f, x + cw - 130.0, fy + 14.0, 118.0, 36.0, "Add files", Some(0xE710), Look::Plain, 1);
        }
        let ly = fy + 84.0;
        match &self.job {
            None => {
                p.text("Nearby", &f.title, x, ly, cw, 24.0, Align::Left, Color::white(0.9));
                let found = self.found.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if found.is_empty() {
                    // Still looking: a pulsing dot, and how to be seen.
                    let t = self.started.elapsed().as_secs_f32();
                    let a = 0.4 + 0.4 * (t * 3.0).sin().abs();
                    p.circle(x + 8.0, ly + 46.0, 4.0, accent.alpha(a));
                    p.text(
                        "Looking for devices nearby\u{2026}",
                        &f.body,
                        x + 22.0,
                        ly + 34.0,
                        cw,
                        24.0,
                        Align::Left,
                        Color::white(0.8),
                    );
                    p.text_wrap(
                        "On the phone, open Quick Share and set who can share with you to Everyone (or your contacts, with the screen on). It needs to be on the same Wi-Fi as this PC.",
                        &f.small,
                        x,
                        ly + 66.0,
                        cw,
                        60.0,
                        Color::white(0.55),
                    );
                }
                for (i, d) in found.iter().enumerate() {
                    let ry = ly + 34.0 + i as f32 * 56.0;
                    let id = 100 + i as u32;
                    let (hot, down) = kit.lit(id);
                    let fill = if down {
                        0.11
                    } else if hot {
                        0.08
                    } else {
                        0.05
                    };
                    p.rrect(x, ry, cw, 50.0, 8.0, Color::white(fill));
                    p.circle(x + 26.0, ry + 25.0, 16.0, accent.alpha(0.25));
                    if let Some(ic) = &f.icons {
                        let g = char::from_u32(kind_glyph(d.device.kind)).unwrap_or(' ').to_string();
                        p.text(&g, ic, x + 14.0, ry, 24.0, 50.0, Align::Center, Color::white(0.95));
                    }
                    p.text(&d.device.name, &f.body, x + 54.0, ry, cw - 70.0, 50.0, Align::Left, Color::white(0.95));
                    kit.hit(x, ry, cw, 50.0, id);
                }
            }
            Some(job) => {
                let j = job.lock().unwrap_or_else(|e| e.into_inner());
                p.text(&format!("To {}", j.to), &f.title, x, ly, cw, 24.0, Align::Left, Color::white(0.94));
                let (line, pin, progress) = match &j.state {
                    State::Connecting => ("Connecting\u{2026}".to_string(), None, None),
                    State::Pin(pin) => (format!("Accept on {}: check the PIN matches.", j.to), Some(pin.clone()), None),
                    State::Sending { pin, done, total } => {
                        let share = if *total > 0 { *done as f32 / *total as f32 } else { 0.0 };
                        (format!("Sending\u{2026} {:.0}%", share * 100.0), Some(pin.clone()), Some(share))
                    }
                    State::Done => ("Sent.".to_string(), None, Some(1.0)),
                    State::Failed(why) => (why.clone(), None, None),
                };
                p.text_wrap(&line, &f.body, x, ly + 32.0, cw, 44.0, Color::white(0.8));
                if let Some(pin) = pin {
                    p.rrect(x, ly + 84.0, cw, 64.0, 8.0, Color::white(0.05));
                    p.text("PIN", &f.small, x + 16.0, ly + 84.0, 80.0, 64.0, Align::Left, Color::white(0.55));
                    p.text(&pin, &f.big, x, ly + 84.0, cw - 16.0, 64.0, Align::Right, Color::white(0.97));
                }
                if let Some(share) = progress {
                    p.rrect(x, ly + 164.0, cw, 6.0, 3.0, Color::white(0.1));
                    p.rrect(x, ly + 164.0, (cw * share.clamp(0.0, 1.0)).max(6.0), 6.0, 3.0, accent);
                }
                let finished = matches!(j.state, State::Done | State::Failed(_));
                drop(j);
                let by = h - 64.0;
                if finished {
                    kit.button(p, f, x, by, (cw - 8.0) / 2.0, 40.0, "Send again", None, Look::Plain, 4);
                    kit.button(p, f, x + (cw + 8.0) / 2.0, by, (cw - 8.0) / 2.0, 40.0, "Done", None, Look::Accent, 3);
                } else {
                    kit.button(p, f, x + cw - 140.0, by, 140.0, 40.0, "Cancel", None, Look::Danger, 2);
                }
            }
        }
    }

    fn press(&mut self, id: u32, ctx: &Ctx) {
        match id {
            1 => {
                if let Some(p) = crate::apps::pick_file("Pick a file to send", &[("All files", "*.*")]) {
                    self.add(vec![p.to_string_lossy().into_owned()]);
                }
            }
            2 => {
                if let Some(job) = &self.job {
                    let mut j = job.lock().unwrap_or_else(|e| e.into_inner());
                    j.cancel = true;
                    // Waiting on their answer: the connection closed under it.
                    if let Some(s) = &j.socket {
                        if matches!(j.state, State::Connecting | State::Pin(_)) {
                            let _ = s.shutdown(std::net::Shutdown::Both);
                        }
                    }
                }
            }
            3 => unsafe {
                let _ = PostMessageW(Some(ctx.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            4 => self.job = None,
            n if n >= 100 => {
                if self.files.is_empty() {
                    return;
                }
                let found = self.found.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(d) = found.get((n - 100) as usize) {
                    self.start(d.clone());
                }
            }
            _ => {}
        }
        ctx.redraw();
    }

    fn tick_ms(&self) -> u32 {
        120
    }

    fn tick(&mut self, _ctx: &Ctx) -> bool {
        true
    }

    fn message(&mut self, m: u32, _wp: WPARAM, _lp: LPARAM, _ctx: &Ctx) -> bool {
        if m != ADD_FILES {
            return false;
        }
        let text = std::fs::read_to_string(inbox()).unwrap_or_default();
        let _ = std::fs::remove_file(inbox());
        self.add(text.lines().map(str::to_string).collect());
        true
    }
}
