//! Now-playing info from Windows' media transport controls (the same source
//! the Windows volume flyout uses). Runs on its own thread and only wakes on
//! media events, so it costs nothing while the music doesn't change.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{IUnknown, Interface, Result};
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    CurrentSessionChangedEventArgs, GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status, MediaPropertiesChangedEventArgs,
    PlaybackInfoChangedEventArgs, SessionsChangedEventArgs, TimelinePropertiesChangedEventArgs,
};
use windows::Storage::Streams::IRandomAccessStreamWithContentType;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IStream, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::WinRT::CreateStreamOverRandomAccessStream;

use crate::msg;

pub enum MediaCmd {
    Refresh,
    Resubscribe,
    PlayPause,
    Next,
    Previous,
}

/// Album art, pre-scaled and pre-rounded, as premultiplied BGRA.
pub struct Art {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// A bright colour picked from the art, for the visualizer.
    pub accent: [f32; 3],
}

pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub position: Duration,
    pub duration: Duration,
    /// When `position` was sampled.
    pub stamp: Instant,
    pub art: Option<Arc<Art>>,
}

impl MediaInfo {
    pub fn position_now(&self) -> Duration {
        let p = if self.playing { self.position + self.stamp.elapsed() } else { self.position };
        if self.duration > Duration::ZERO {
            p.min(self.duration)
        } else {
            p
        }
    }
}

const ART_SIZE: u32 = 192;
/// Corner radius as a fraction of the art size, baked into the pixels so the
/// art stays rounded at any drawn size.
const ART_RADIUS: f32 = 0.2;

pub fn spawn() -> Sender<MediaCmd> {
    let (tx, rx) = channel();
    let events = tx.clone();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = run(rx, events);
    });
    tx
}

struct Watcher {
    manager: Manager,
    subs: Vec<(Session, i64, i64, i64)>,
    current: Option<Session>,
    art_key: String,
    art: Option<Arc<Art>>,
    wic: Option<IWICImagingFactory>,
}

fn run(rx: Receiver<MediaCmd>, tx: Sender<MediaCmd>) -> Result<()> {
    let manager = Manager::RequestAsync()?.get()?;
    let t = tx.clone();
    manager.CurrentSessionChanged(&TypedEventHandler::<Manager, CurrentSessionChangedEventArgs>::new(
        move |_, _| {
            let _ = t.send(MediaCmd::Resubscribe);
            Ok(())
        },
    ))?;
    let t = tx.clone();
    manager.SessionsChanged(&TypedEventHandler::<Manager, SessionsChangedEventArgs>::new(move |_, _| {
        let _ = t.send(MediaCmd::Resubscribe);
        Ok(())
    }))?;

    let mut w = Watcher { manager, subs: Vec::new(), current: None, art_key: String::new(), art: None, wic: None };
    w.resubscribe(&tx);
    w.refresh();

    // Returns (refresh, resubscribe) after running any player action.
    let handle = |cmd: MediaCmd, w: &Watcher| match cmd {
        MediaCmd::Refresh => (true, false),
        MediaCmd::Resubscribe => (true, true),
        action => {
            w.act(action);
            (true, false)
        }
    };
    while let Ok(cmd) = rx.recv() {
        let (mut refresh, mut resubscribe) = handle(cmd, &w);
        // Players fire several events per track change (title, then art,
        // then timeline). Let them land, then refresh once.
        std::thread::sleep(Duration::from_millis(120));
        while let Ok(more) = rx.try_recv() {
            let (a, b) = handle(more, &w);
            refresh |= a;
            resubscribe |= b;
        }
        if resubscribe {
            w.resubscribe(&tx);
        }
        if refresh {
            w.refresh();
        }
    }
    Ok(())
}

fn now_filetime() -> i64 {
    let ft = unsafe { GetSystemTimeAsFileTime() };
    ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
}

fn is_playing(s: &Session) -> bool {
    s.GetPlaybackInfo().and_then(|p| p.PlaybackStatus()).map(|st| st == Status::Playing).unwrap_or(false)
}

impl Watcher {
    fn act(&self, cmd: MediaCmd) {
        let Some(s) = &self.current else { return };
        let _ = match cmd {
            MediaCmd::PlayPause => s.TryTogglePlayPauseAsync().and_then(|op| op.get()),
            MediaCmd::Next => s.TrySkipNextAsync().and_then(|op| op.get()),
            MediaCmd::Previous => s.TrySkipPreviousAsync().and_then(|op| op.get()),
            _ => return,
        };
    }

    fn resubscribe(&mut self, tx: &Sender<MediaCmd>) {
        for (s, a, b, c) in self.subs.drain(..) {
            let _ = s.RemoveMediaPropertiesChanged(a);
            let _ = s.RemovePlaybackInfoChanged(b);
            let _ = s.RemoveTimelinePropertiesChanged(c);
        }
        let Ok(sessions) = self.manager.GetSessions() else { return };
        let count = sessions.Size().unwrap_or(0);
        for i in 0..count {
            let Ok(s) = sessions.GetAt(i) else { continue };
            let (t1, t2, t3) = (tx.clone(), tx.clone(), tx.clone());
            let a = s.MediaPropertiesChanged(&TypedEventHandler::<Session, MediaPropertiesChangedEventArgs>::new(
                move |_, _| {
                    let _ = t1.send(MediaCmd::Refresh);
                    Ok(())
                },
            ));
            let b =
                s.PlaybackInfoChanged(&TypedEventHandler::<Session, PlaybackInfoChangedEventArgs>::new(move |_, _| {
                    let _ = t2.send(MediaCmd::Refresh);
                    Ok(())
                }));
            let c = s.TimelinePropertiesChanged(
                &TypedEventHandler::<Session, TimelinePropertiesChangedEventArgs>::new(move |_, _| {
                    let _ = t3.send(MediaCmd::Refresh);
                    Ok(())
                }),
            );
            if let (Ok(a), Ok(b), Ok(c)) = (a, b, c) {
                self.subs.push((s, a, b, c));
            }
        }
    }

    /// Picks the session worth showing: one that is playing (preferring the
    /// one Windows calls current), otherwise the current one.
    fn pick(&self) -> Option<Session> {
        let current = self.manager.GetCurrentSession().ok();
        if let Some(c) = &current {
            if is_playing(c) {
                return current;
            }
        }
        if let Ok(sessions) = self.manager.GetSessions() {
            for i in 0..sessions.Size().unwrap_or(0) {
                if let Ok(s) = sessions.GetAt(i) {
                    if is_playing(&s) {
                        return Some(s);
                    }
                }
            }
        }
        current
    }

    fn refresh(&mut self) {
        self.current = self.pick();
        let info = self.current.clone().and_then(|s| self.read(&s).ok()).flatten();
        let ptr = Box::into_raw(Box::new(info));
        if !msg::post(msg::MEDIA, 0, ptr as isize) {
            drop(unsafe { Box::from_raw(ptr) });
        }
    }

    fn read(&mut self, s: &Session) -> Result<Option<MediaInfo>> {
        let props = s.TryGetMediaPropertiesAsync()?.get()?;
        let title = props.Title()?.to_string_lossy();
        let artist = props.Artist()?.to_string_lossy();
        let album = props.AlbumTitle().map(|a| a.to_string_lossy()).unwrap_or_default();
        let playing = is_playing(s);
        if title.is_empty() && !playing {
            return Ok(None);
        }

        let (mut position, mut duration) = (0i64, 0i64);
        if let Ok(tl) = s.GetTimelineProperties() {
            let start = tl.StartTime().map(|t| t.Duration).unwrap_or(0);
            let end = tl.EndTime().map(|t| t.Duration).unwrap_or(0);
            position = tl.Position().map(|t| t.Duration).unwrap_or(0) - start;
            duration = (end - start).max(0);
            // Players report position only now and then; advance it to now.
            let updated = tl.LastUpdatedTime().map(|t| t.UniversalTime).unwrap_or(0);
            if playing && updated > 0 {
                position += (now_filetime() - updated).clamp(0, 36_000_000_000);
            }
            if duration > 0 {
                position = position.clamp(0, duration);
            }
        }

        let key = format!("{title}\u{1}{artist}\u{1}{album}");
        if key != self.art_key || self.art.is_none() {
            self.art_key = key;
            self.art = props
                .Thumbnail()
                .and_then(|t| t.OpenReadAsync())
                .and_then(|op| op.get())
                .ok()
                .and_then(|stream| self.decode(&stream).ok())
                .map(Arc::new);
        }

        Ok(Some(MediaInfo {
            title,
            artist,
            playing,
            position: Duration::from_nanos(position.max(0) as u64 * 100),
            duration: Duration::from_nanos(duration as u64 * 100),
            stamp: Instant::now(),
            art: self.art.clone(),
        }))
    }

    fn decode(&mut self, stream: &IRandomAccessStreamWithContentType) -> Result<Art> {
        unsafe {
            if self.wic.is_none() {
                self.wic = Some(CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?);
            }
            let wic = self.wic.as_ref().unwrap();
            let unknown: IUnknown = stream.cast()?;
            let istream: IStream = CreateStreamOverRandomAccessStream(&unknown)?;
            let decoder = wic.CreateDecoderFromStream(&istream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
            let frame = decoder.GetFrame(0)?;
            let (mut w, mut h) = (0u32, 0u32);
            frame.GetSize(&mut w, &mut h)?;
            if w == 0 || h == 0 {
                return Err(windows::core::Error::from(windows::Win32::Foundation::E_FAIL));
            }
            // Centre-crop to a square, then scale.
            let side = w.min(h);
            let clipper = wic.CreateBitmapClipper()?;
            let crop = WICRect {
                X: ((w - side) / 2) as i32,
                Y: ((h - side) / 2) as i32,
                Width: side as i32,
                Height: side as i32,
            };
            clipper.Initialize(&frame, &crop)?;
            let scaler = wic.CreateBitmapScaler()?;
            scaler.Initialize(&clipper, ART_SIZE, ART_SIZE, WICBitmapInterpolationModeFant)?;
            let conv = wic.CreateFormatConverter()?;
            conv.Initialize(
                &scaler,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            let mut pixels = vec![0u8; (ART_SIZE * ART_SIZE * 4) as usize];
            conv.CopyPixels(std::ptr::null(), ART_SIZE * 4, &mut pixels)?;
            let accent = accent(&pixels);
            round_corners(&mut pixels, ART_SIZE, ART_SIZE as f32 * ART_RADIUS);
            Ok(Art { width: ART_SIZE, height: ART_SIZE, pixels, accent })
        }
    }
}

/// Saturation-weighted average colour, brightened so it reads on black.
fn accent(px: &[u8]) -> [f32; 3] {
    let (mut r, mut g, mut b, mut wsum) = (0f32, 0f32, 0f32, 0f32);
    for p in px.chunks_exact(4).step_by(7) {
        let (pb, pg, pr) = (p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
        let max = pr.max(pg).max(pb);
        let min = pr.min(pg).min(pb);
        let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
        let w = sat * sat * max + 0.002;
        r += pr * w;
        g += pg * w;
        b += pb * w;
        wsum += w;
    }
    if wsum <= 0.0 {
        return [1.0, 1.0, 1.0];
    }
    let (r, g, b) = (r / wsum, g / wsum, b / wsum);
    let max = r.max(g).max(b).max(0.001);
    let mix = |c: f32| (c / max) * 0.85 + 0.15;
    [mix(r), mix(g), mix(b)]
}

fn round_corners(px: &mut [u8], size: u32, radius: f32) {
    let s = size as f32;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.clamp(radius, s - radius);
            let cy = fy.clamp(radius, s - radius);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let cover = (radius - d + 0.5).clamp(0.0, 1.0);
            if cover < 1.0 {
                let i = ((y * size + x) * 4) as usize;
                for c in &mut px[i..i + 4] {
                    *c = (*c as f32 * cover) as u8;
                }
            }
        }
    }
}
