//! Camera (`cascade.exe --camera`), as Windows' own: the webcam's picture,
//! a shutter for photos (Pictures\Camera Roll, as PNG) and a record button
//! for video with the microphone's sound (MP4, H.264 and AAC, through the
//! screen recorder's writer), switching cameras, mirroring, and a 3-second
//! self-timer. The last shot shows in the corner: click it to open it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, Interface};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::{App, Ctx, Kit, Look};
use crate::gfx::{Align, Color, Fonts, Painter};

const FRAME: u32 = 0x8000 + 20;

/// A frame from the camera: BGRA, opaque, top row first.
struct Frame {
    w: u32,
    h: u32,
    px: Vec<u8>,
    n: u64,
}

static LATEST: Mutex<Option<Frame>> = Mutex::new(None);

/// The picture sizes the running camera offers, biggest first.
static SIZES: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());

/// The cameras there are: their names and activators.
fn cameras() -> Vec<(String, IMFActivate)> {
    let mut out = Vec::new();
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        if MFCreateAttributes(&mut attrs, 1).is_err() {
            return out;
        }
        let Some(attrs) = attrs else { return out };
        let _ = attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID);
        let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0u32;
        if MFEnumDeviceSources(&attrs, &mut list, &mut n).is_err() || list.is_null() {
            return out;
        }
        for i in 0..n as usize {
            if let Some(a) = (*list.add(i)).take() {
                let mut name = windows::core::PWSTR::null();
                let mut len = 0u32;
                let label = if a.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut name, &mut len).is_ok()
                {
                    let s = name.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(name.0 as *const _));
                    s
                } else {
                    format!("Camera {}", i + 1)
                };
                out.push((label, a));
            }
        }
        CoTaskMemFree(Some(list as *const _));
    }
    out
}

/// Reads camera `index` until `stop`, putting each frame in LATEST and
/// telling the window.
fn capture(index: usize, want: Option<(u32, u32)>, stop: Arc<AtomicBool>, hwnd: isize) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        let list = cameras();
        let (_, act) = list.get(index).ok_or("No camera is connected")?;
        let source: IMFMediaSource =
            act.ActivateObject().map_err(|_| "The camera couldn't start (another app may be using it)".to_string())?;
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1).map_err(|e| e.to_string())?;
        let attrs = attrs.ok_or("no attributes")?;
        let _ = attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1);
        let reader = MFCreateSourceReaderFromMediaSource(&source, &attrs).map_err(|e| e.to_string())?;
        let ty = MFCreateMediaType().map_err(|e| e.to_string())?;
        let _ = ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video);
        let _ = ty.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32);
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        // What the camera can do: its sizes (for the settings), and the type for the one asked for
        // (of those, the one with the most frames a second).
        let mut modes: Vec<(u32, u32, f64, IMFMediaType)> = Vec::new();
        for i in 0.. {
            let Ok(t) = reader.GetNativeMediaType(stream, i) else { break };
            let Ok(size) = t.GetUINT64(&MF_MT_FRAME_SIZE) else { continue };
            let fps = t.GetUINT64(&MF_MT_FRAME_RATE).map_or(30.0, |r| {
                let (n, d) = ((r >> 32) as f64, (r & 0xFFFF_FFFF) as f64);
                if d > 0.0 {
                    n / d
                } else {
                    30.0
                }
            });
            modes.push(((size >> 32) as u32, (size & 0xFFFF_FFFF) as u32, fps, t));
        }
        let mut sizes: Vec<(u32, u32)> = modes.iter().map(|m| (m.0, m.1)).collect();
        sizes.sort_by(|a, b| (b.0 * b.1).cmp(&(a.0 * a.1)).then(b.cmp(a)));
        sizes.dedup();
        *SIZES.lock().unwrap_or_else(|e| e.into_inner()) = sizes;
        if let Some((ww, wh)) = want {
            let pick = modes
                .iter()
                .filter(|m| (m.0, m.1) == (ww, wh))
                .max_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
            if let Some(m) = pick {
                if reader.SetCurrentMediaType(stream, None, &m.3).is_err() {
                    crate::config::log(&format!("camera: {ww}x{wh} wasn't taken, using the camera's own pick"));
                }
            }
        }
        reader.SetCurrentMediaType(stream, None, &ty).map_err(|e| e.to_string())?;
        let cur = reader.GetCurrentMediaType(stream).map_err(|e| e.to_string())?;
        let size = cur.GetUINT64(&MF_MT_FRAME_SIZE).map_err(|e| e.to_string())?;
        let (w, h) = ((size >> 32) as u32, (size & 0xFFFF_FFFF) as u32);
        let mut n = 0u64;
        while !stop.load(Ordering::Acquire) {
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            if reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample)).is_err() {
                break;
            }
            let Some(sample) = sample else { continue };
            let Ok(buf) = sample.ConvertToContiguousBuffer() else { continue };
            let mut px = vec![0u8; (w * h * 4) as usize];
            let row = (w * 4) as usize;
            // Rows in the order the buffer has them (its pitch says which way up).
            if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
                let mut line: *mut u8 = std::ptr::null_mut();
                let mut pitch = 0i32;
                if b2.Lock2D(&mut line, &mut pitch).is_ok() {
                    for y in 0..h as usize {
                        let src = line.offset(pitch as isize * y as isize);
                        std::ptr::copy_nonoverlapping(src, px.as_mut_ptr().add(y * row), row);
                    }
                    let _ = b2.Unlock2D();
                }
            } else {
                let mut at: *mut u8 = std::ptr::null_mut();
                let mut len = 0u32;
                if buf.Lock(&mut at, None, Some(&mut len)).is_ok() {
                    let len = (len as usize).min(px.len());
                    std::ptr::copy_nonoverlapping(at, px.as_mut_ptr(), len);
                    let _ = buf.Unlock();
                }
            }
            for a in px.iter_mut().skip(3).step_by(4) {
                *a = 255;
            }
            n += 1;
            *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = Some(Frame { w, h, px, n });
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), FRAME, WPARAM(0), LPARAM(0));
        }
        let _ = source.Shutdown();
        Ok(())
    }
}

/// A video being made: the writer, its streams and when it began.
struct Clip {
    sink: IMFSinkWriter,
    video: u32,
    start: i64,
    path: PathBuf,
    sound: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    w: u32,
    h: u32,
}

struct Shared(Mutex<IMFSinkWriter>);
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

struct Camera {
    list: Vec<String>,
    index: usize,
    stop: Option<Arc<AtomicBool>>,
    error: Option<String>,
    bmp: Option<ID2D1Bitmap>,
    shown: u64,
    size: (u32, u32),
    mirror: bool,
    video_mode: bool,
    clip: Option<(Clip, Arc<Shared>)>,
    /// The self-timer's seconds (0 for none).
    timer: u32,
    countdown: Option<Instant>,
    /// The picture size asked for (None: the camera's own pick).
    res: Option<(u32, u32)>,
    /// Rule-of-thirds lines over the picture.
    grid: bool,
    /// Photos as JPEG instead of PNG.
    jpeg: bool,
    last: Option<PathBuf>,
    flash: Option<Instant>,
    hwnd: HWND,
}

fn camera_roll() -> PathBuf {
    let base = super::known_folder(&windows::Win32::UI::Shell::FOLDERID_Pictures).unwrap_or_else(|| PathBuf::from("."));
    base.join("Camera Roll")
}

impl Camera {
    fn start(&mut self) {
        if let Some(s) = self.stop.take() {
            s.store(true, Ordering::Release);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (s, idx, hwnd, res) = (stop.clone(), self.index, self.hwnd.0 as isize, self.res);
        self.error = None;
        std::thread::spawn(move || {
            if let Err(e) = capture(idx, res, s, hwnd) {
                crate::config::log(&format!("camera: {e}"));
                *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = None;
                ERROR.lock().map(|mut m| *m = Some(e)).ok();
                let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), FRAME, WPARAM(1), LPARAM(0)) };
            }
        });
        self.stop = Some(stop);
    }

    fn photo(&mut self) {
        let frame = LATEST.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|f| (f.w, f.h, f.px.clone()));
        let Some((w, h, px)) = frame else { return };
        let mut pixels: Vec<u32> =
            px.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        if self.mirror {
            for row in pixels.chunks_mut(w as usize) {
                row.reverse();
            }
        }
        let _ = std::fs::create_dir_all(camera_roll());
        let path = camera_roll().join(super::stamped("Photo", if self.jpeg { "jpg" } else { "png" }));
        if unsafe { crate::snip::save_image(&path, &pixels, w as i32, h as i32) } {
            self.last = Some(path);
            self.flash = Some(Instant::now());
        } else {
            self.error = Some("Couldn't save the photo".into());
        }
    }

    fn record(&mut self) {
        if let Some((clip, _)) = self.clip.take() {
            // Stop: the sound first, then the file.
            if let Some((stop, t)) = clip.sound {
                stop.store(true, Ordering::Release);
                let _ = t.join();
            }
            unsafe {
                let _ = clip.sink.Finalize();
            }
            self.last = Some(clip.path);
            return;
        }
        let (w, h) = self.size;
        if w == 0 {
            return;
        }
        let _ = std::fs::create_dir_all(camera_roll());
        let path = camera_roll().join(super::stamped("Video", "mp4"));
        unsafe {
            let mic = crate::recorder::open_sound(true);
            let made = crate::recorder::writer(&path, w & !1, h & !1, mic.as_ref().map(|m| m.rate));
            let Ok((sink, video, sound)) = made else {
                self.error = Some("Couldn't start the video (Windows' encoder)".into());
                return;
            };
            if sink.BeginWriting().is_err() {
                self.error = Some("Couldn't start the video".into());
                return;
            }
            let start = crate::recorder::qpc();
            let shared = Arc::new(Shared(Mutex::new(sink.clone())));
            let sound = match (mic, sound) {
                (Some(m), Some(stream)) => {
                    struct SendMic(crate::recorder::Loopback);
                    unsafe impl Send for SendMic {}
                    let m = SendMic(m);
                    let stop = Arc::new(AtomicBool::new(false));
                    let (s, sh) = (stop.clone(), shared.clone());
                    let t = std::thread::spawn(move || {
                        let m = m;
                        let lb = &m.0;
                        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                        if lb.client.Start().is_err() {
                            return;
                        }
                        let start_hns = crate::recorder::hns(start);
                        while !s.load(Ordering::Acquire) {
                            std::thread::sleep(Duration::from_millis(10));
                            loop {
                                let Ok(n) = lb.capture.GetNextPacketSize() else { break };
                                if n == 0 {
                                    break;
                                }
                                let mut data: *mut u8 = std::ptr::null_mut();
                                let (mut frames, mut flags, mut pos) = (0u32, 0u32, 0u64);
                                if lb
                                    .capture
                                    .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut pos))
                                    .is_err()
                                {
                                    break;
                                }
                                let silent =
                                    flags & windows::Win32::Media::Audio::AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                                let pcm = crate::recorder::to_pcm16(lb, data, frames as usize, silent);
                                let _ = lb.capture.ReleaseBuffer(frames);
                                let time = pos as i64 - start_hns;
                                if time < 0 {
                                    continue;
                                }
                                let dur = frames as i64 * 10_000_000 / lb.rate as i64;
                                if let Ok(smp) = crate::recorder::sample(&pcm, time, dur) {
                                    let _ = sh.0.lock().unwrap_or_else(|e| e.into_inner()).WriteSample(stream, &smp);
                                }
                            }
                        }
                        let _ = lb.client.Stop();
                    });
                    Some((stop, t))
                }
                _ => None,
            };
            self.clip = Some((Clip { sink, video, start, path, sound, w: w & !1, h: h & !1 }, shared));
        }
    }

    /// The settings menu: the picture's size, the self-timer, a grid, the photo format.
    fn settings(&mut self) {
        use crate::menu::{check, item, show, sub, Entry, Place};
        let sizes = SIZES.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let recording = self.clip.is_some();
        let mut quality = vec![check(100, "Automatic", self.res.is_none())];
        for (i, (w, h)) in sizes.iter().enumerate() {
            let tag = match h {
                2160.. => " (4K)",
                1440..=2159 => " (QHD)",
                1080..=1439 => " (Full HD)",
                720..=1079 => " (HD)",
                _ => "",
            };
            quality.push(check(101 + i, format!("{w} \u{d7} {h}{tag}"), self.res == Some((*w, *h))));
        }
        let timer = |id: usize, s: u32| {
            check(id, if s == 0 { "Off".to_string() } else { format!("{s} seconds") }, self.timer == s)
        };
        let mut entries = vec![
            sub("Quality", quality),
            sub("Self-timer", vec![timer(10, 0), timer(11, 3), timer(12, 5), timer(13, 10)]),
            Entry::Separator,
            check(20, "Grid", self.grid),
            check(21, "Mirror the picture", self.mirror),
            Entry::Separator,
            sub("Photo format", vec![check(30, "PNG", !self.jpeg), check(31, "JPEG (smaller files)", self.jpeg)]),
            Entry::Separator,
            item(40, "Open the Camera Roll"),
        ];
        if recording {
            // The picture size can't change in the middle of a video.
            entries.remove(0);
        }
        match show(self.hwnd, entries, Place::Cursor) {
            100 => self.set_res(None),
            n @ 101..=199 => self.set_res(sizes.get(n - 101).copied()),
            10 => self.timer = 0,
            11 => self.timer = 3,
            12 => self.timer = 5,
            13 => self.timer = 10,
            20 => self.grid = !self.grid,
            21 => self.mirror = !self.mirror,
            30 => self.jpeg = false,
            31 => self.jpeg = true,
            40 => crate::appwin::launch(&["--files", &camera_roll().to_string_lossy()]),
            _ => {}
        }
    }

    /// Restarts the camera at another picture size.
    fn set_res(&mut self, res: Option<(u32, u32)>) {
        if self.res == res || self.clip.is_some() {
            return;
        }
        self.res = res;
        *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.bmp = None;
        self.start();
    }

    /// A new frame for the clip being made.
    fn write_frame(&mut self) {
        let Some((clip, shared)) = &self.clip else { return };
        let guard = LATEST.lock().unwrap_or_else(|e| e.into_inner());
        let Some(f) = guard.as_ref() else { return };
        let (cw, ch) = (clip.w as usize, clip.h as usize);
        let mut bytes = Vec::with_capacity(cw * ch * 4);
        for y in 0..ch.min(f.h as usize) {
            let row = &f.px[y * f.w as usize * 4..][..cw * 4];
            if self.mirror {
                for x in (0..cw).rev() {
                    bytes.extend_from_slice(&row[x * 4..x * 4 + 4]);
                }
            } else {
                bytes.extend_from_slice(row);
            }
        }
        drop(guard);
        let now = crate::recorder::hns(crate::recorder::qpc() - clip.start);
        let dur = 10_000_000 / crate::recorder::FPS as i64;
        unsafe {
            if let Ok(s) = crate::recorder::sample(&bytes, now, dur) {
                let _ = shared.0.lock().unwrap_or_else(|e| e.into_inner()).WriteSample(clip.video, &s);
            }
        }
    }
}

static ERROR: Mutex<Option<String>> = Mutex::new(None);

const SHUTTER: u32 = 1;
const MODE: u32 = 2;
const SWITCH: u32 = 3;
const MIRROR: u32 = 4;
const TIMER: u32 = 5;
const LAST: u32 = 6;
const FOLDER: u32 = 7;
const SETTINGS: u32 = 8;

impl App for Camera {
    fn draw(&mut self, p: &Painter, f: &Fonts, w: f32, h: f32, kit: &mut Kit) {
        let side = 96.0;
        let (vw, vh) = (w - side, h);
        p.rrect(0.0, 0.0, vw, vh, 0.0, Color(0.0, 0.0, 0.0, 1.0));
        // The picture, fitted (mirrored if asked).
        let frame = LATEST.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|f| (f.n, f.w, f.h, f.px.clone()));
        if let Some((n, fw, fh, px)) = frame {
            if n != self.shown || self.bmp.is_none() {
                self.bmp = None;
                self.shown = n;
            }
            self.size = (fw, fh);
            if self.bmp.is_none() {
                self.bmp = p_bitmap(p, fw, fh, &px);
            }
            if let Some(b) = &self.bmp {
                let s = (vw / fw as f32).min(vh / fh as f32);
                let (dw, dh) = (fw as f32 * s, fh as f32 * s);
                let (dx, dy) = ((vw - dw) / 2.0, (vh - dh) / 2.0);
                if self.mirror {
                    p.flipped(dx + dw / 2.0, || p.image(b, dx, dy, dw, dh, 1.0));
                } else {
                    p.image(b, dx, dy, dw, dh, 1.0);
                }
                if self.grid {
                    let line = Color::white(0.45);
                    for n in 1..3 {
                        let (gx, gy) = (dx + dw * n as f32 / 3.0, dy + dh * n as f32 / 3.0);
                        p.line(gx, dy, gx, dy + dh, line, 1.0);
                        p.line(dx, gy, dx + dw, gy, line, 1.0);
                    }
                }
            }
        } else {
            let msg = self.error.clone().unwrap_or_else(|| "Starting the camera\u{2026}".into());
            p.text(&msg, &f.body, 0.0, vh / 2.0 - 12.0, vw, 24.0, Align::Center, Color::white(0.6));
        }
        if let Some(t) = self.flash {
            let a = 1.0 - t.elapsed().as_secs_f32() / 0.3;
            if a > 0.0 {
                p.rrect(0.0, 0.0, vw, vh, 0.0, Color::white(a * 0.8));
            }
        }
        if let Some(t) = self.countdown {
            let left = self.timer as f32 - t.elapsed().as_secs_f32();
            p.scaled(4.0, vw / 2.0, vh / 2.0, || {
                p.text(
                    &format!("{}", left.ceil().max(1.0) as u32),
                    &f.big,
                    vw / 2.0 - 30.0,
                    vh / 2.0 - 18.0,
                    60.0,
                    36.0,
                    Align::Center,
                    Color::white(0.95),
                );
            });
        }
        if let Some((clip, _)) = &self.clip {
            let s = crate::recorder::hns(crate::recorder::qpc() - clip.start) / 10_000_000;
            p.rrect(vw / 2.0 - 50.0, 16.0, 100.0, 30.0, 15.0, Color(0.0, 0.0, 0.0, 0.6));
            p.circle(vw / 2.0 - 30.0, 31.0, 5.0, Color(0.95, 0.25, 0.22, 1.0));
            p.text(
                &format!("{:02}:{:02}", s / 60, s % 60),
                &f.body,
                vw / 2.0 - 20.0,
                16.0,
                64.0,
                30.0,
                Align::Left,
                Color::white(0.95),
            );
        }
        // The side: switch, mirror and timer up top; the shutter in the middle; the last shot below.
        p.rrect(vw, 0.0, side, h, 0.0, Color(0.09, 0.09, 0.095, 1.0));
        let cx = vw + side / 2.0;
        if self.list.len() > 1 {
            kit.round(p, f, cx, 40.0, 20.0, 0xE89E, Look::Plain, SWITCH);
        }
        kit.round(p, f, cx, 90.0, 20.0, 0xE8AB, if self.mirror { Look::Accent } else { Look::Plain }, MIRROR);
        kit.round(p, f, cx, 140.0, 20.0, 0xE916, if self.timer > 0 { Look::Accent } else { Look::Plain }, TIMER);
        if self.timer > 0 {
            p.text(&format!("{} s", self.timer), &f.small, vw, 164.0, side, 18.0, Align::Center, Color::white(0.6));
        }
        kit.round(p, f, cx, 200.0, 20.0, 0xE713, Look::Plain, SETTINGS);
        let mid = h / 2.0;
        kit.round(p, f, cx, mid - 64.0, 16.0, if self.video_mode { 0xE722 } else { 0xE714 }, Look::Plain, MODE);
        let recording = self.clip.is_some();
        let shutter_icon = if self.video_mode {
            if recording {
                0xE71A
            } else {
                0xE7C8
            }
        } else {
            0xE722
        };
        kit.round(
            p,
            f,
            cx,
            mid,
            32.0,
            shutter_icon,
            if self.video_mode { Look::Danger } else { Look::Accent },
            SHUTTER,
        );
        let label = if self.video_mode { "Video" } else { "Photo" };
        p.text(label, &f.small, vw, mid + 36.0, side, 18.0, Align::Center, Color::white(0.6));
        if self.last.is_some() {
            kit.round(p, f, cx, h - 100.0, 22.0, 0xE91B, Look::Plain, LAST);
        }
        kit.round(p, f, cx, h - 48.0, 18.0, 0xE8B7, Look::Plain, FOLDER);
    }

    fn press(&mut self, id: u32, ctx: &Ctx) {
        match id {
            SHUTTER if self.video_mode => self.record(),
            SHUTTER if self.timer > 0 => self.countdown = Some(Instant::now()),
            SHUTTER => self.photo(),
            MODE if self.clip.is_none() => self.video_mode = !self.video_mode,
            SWITCH if self.clip.is_none() => {
                self.index = (self.index + 1) % self.list.len().max(1);
                *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = None;
                self.bmp = None;
                self.start();
            }
            MIRROR => self.mirror = !self.mirror,
            TIMER => {
                self.timer = match self.timer {
                    0 => 3,
                    3 => 5,
                    5 => 10,
                    _ => 0,
                }
            }
            SETTINGS => self.settings(),
            LAST => {
                if let Some(p) = &self.last {
                    let flag = if p.extension().is_some_and(|e| e == "mp4") { "--player" } else { "--view" };
                    crate::appwin::launch(&[flag, &p.to_string_lossy()]);
                }
            }
            FOLDER => crate::appwin::launch(&["--files", &camera_roll().to_string_lossy()]),
            _ => {}
        }
        ctx.redraw();
    }

    fn key(&mut self, vk: u16, ctx: &Ctx) -> bool {
        if vk == 0x20 || vk == 0x0D {
            self.press(SHUTTER, ctx);
            return true;
        }
        false
    }

    fn tick_ms(&self) -> u32 {
        if self.countdown.is_some() || self.flash.is_some() {
            33
        } else {
            0
        }
    }

    fn tick(&mut self, _: &Ctx) -> bool {
        if self.countdown.is_some_and(|t| t.elapsed() >= Duration::from_secs(self.timer as u64)) {
            self.countdown = None;
            self.photo();
        }
        if self.flash.is_some_and(|t| t.elapsed() > Duration::from_millis(300)) {
            self.flash = None;
        }
        true
    }

    fn message(&mut self, m: u32, wp: WPARAM, _: LPARAM, _: &Ctx) -> bool {
        if m != FRAME {
            return false;
        }
        if wp.0 == 1 {
            self.error = ERROR.lock().ok().and_then(|mut e| e.take());
        }
        self.bmp = None;
        self.write_frame();
        true
    }

    fn closing(&mut self, _: &Ctx) -> bool {
        if self.clip.is_some() {
            self.record();
        }
        if let Some(s) = self.stop.take() {
            s.store(true, Ordering::Release);
        }
        true
    }
}

/// A bitmap of a frame (the painter's render target makes it).
fn p_bitmap(p: &Painter, w: u32, h: u32, px: &[u8]) -> Option<ID2D1Bitmap> {
    p.bitmap(w, h, px)
}

/// `cascade.exe --camera`.
pub fn run() {
    unsafe {
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    }
    super::run(w!("CascadeCamera"), "Camera", "Cascade.Camera", 1100, 680, |ctx| {
        let list: Vec<String> = cameras().into_iter().map(|(n, _)| n).collect();
        let mut c = Camera {
            error: list.is_empty().then(|| "No camera is connected".to_string()),
            list,
            index: 0,
            stop: None,
            bmp: None,
            shown: 0,
            size: (0, 0),
            mirror: true,
            video_mode: false,
            clip: None,
            timer: 0,
            countdown: None,
            res: None,
            grid: false,
            jpeg: false,
            last: None,
            flash: None,
            hwnd: ctx.hwnd,
        };
        if c.error.is_none() {
            c.start();
        }
        Box::new(c)
    });
}
