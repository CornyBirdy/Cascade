//! Screen recording, as the Snipping Tool's: an area or a whole screen,
//! with the PC's sound, to an MP4 in Videos\Screen Recordings. A countdown
//! comes first, a small bar (left out of the recording itself) shows how
//! long it's been going and stops it (so does the shortcut that started it
//! again), and a preview pops up once it's saved (see `recui`).
//!
//! Frames are taken 30 times a second (the pointer drawn in) and sound is
//! taken from the default output as it plays; Windows' own encoder makes the
//! video (H.264, on the graphics card where Windows offers that) and the
//! sound (AAC). It all runs on threads of its own.
//!
//! Not as the Snipping Tool: no microphone track, and games running in
//! exclusive fullscreen (which bypass the desktop) record as black.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::HSTRING;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::UI::Shell::{FOLDERID_Videos, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{config, msg};

pub(crate) const FPS: u32 = 30;

static RECORDING: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
/// When it began (QPC ticks), for the bar's clock.
static BEGAN: AtomicIsize = AtomicIsize::new(0);

/// Seconds since the recording began (for the bar's clock).
pub fn elapsed() -> f32 {
    let began = BEGAN.load(Ordering::Acquire) as i64;
    if began == 0 {
        return 0.0;
    }
    hns(qpc() - began) as f32 / 10_000_000.0
}

pub fn recording() -> bool {
    RECORDING.load(Ordering::Acquire)
}

/// Stops the recording (it's saved, and the notch says where).
pub fn stop() {
    STOP.store(true, Ordering::Release);
}

/// The sink writer, shared by the video and sound threads.
struct Writer(IMFSinkWriter);
// The sink writer may be called from any thread (calls are made one at a time, under a lock).
unsafe impl Send for Writer {}

pub(crate) fn qpc() -> i64 {
    let mut t = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut t);
    }
    t
}

fn qpc_freq() -> i64 {
    let mut f = 1i64;
    unsafe {
        let _ = QueryPerformanceFrequency(&mut f);
    }
    f.max(1)
}

/// QPC ticks as Media Foundation's 100 ns units.
pub(crate) fn hns(ticks: i64) -> i64 {
    (ticks as i128 * 10_000_000 / qpc_freq() as i128) as i64
}

fn output_path() -> PathBuf {
    let dir = unsafe {
        SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None).ok().and_then(|p| {
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        })
    }
    .map(PathBuf::from)
    .or_else(|| std::env::var_os("USERPROFILE").map(|u| PathBuf::from(u).join("Videos")))
    .unwrap_or_else(|| PathBuf::from("."))
    .join("Screen Recordings");
    let _ = std::fs::create_dir_all(&dir);
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    dir.join(format!(
        "Screen Recording {:04}-{:02}-{:02} {:02}{:02}{:02}.mp4",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    ))
}

// ---------------------------------------------------------------- sound --

/// The default output's sound as it plays (loopback), and how to turn its
/// samples into 16-bit stereo PCM.
pub(crate) struct Loopback {
    pub(crate) client: IAudioClient,
    pub(crate) capture: IAudioCaptureClient,
    pub(crate) rate: u32,
    channels: u16,
    float: bool,
    bits: u16,
}

unsafe fn open_loopback() -> Option<Loopback> {
    open_sound(false)
}

/// The PC's sound as it plays, or (`mic`) the default microphone.
pub(crate) unsafe fn open_sound(mic: bool) -> Option<Loopback> {
    let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
    let dev = e.GetDefaultAudioEndpoint(if mic { eCapture } else { eRender }, eConsole).ok()?;
    let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).ok()?;
    let fmt = client.GetMixFormat().ok()?;
    let f = *fmt;
    const EXTENSIBLE: u16 = 0xFFFE;
    const IEEE_FLOAT: u16 = 3;
    let float = match f.wFormatTag {
        IEEE_FLOAT => true,
        EXTENSIBLE => {
            let ext = &*(fmt as *const WAVEFORMATEXTENSIBLE);
            let sub = ext.SubFormat;
            sub.data1 == 3 // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT's first part (PCM's is 1)
        }
        _ => false,
    };
    let flags = if mic { 0 } else { AUDCLNT_STREAMFLAGS_LOOPBACK };
    let r = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 10_000_000, 0, fmt, None);
    let lb = Loopback {
        client: client.clone(),
        capture: client.GetService().ok()?,
        rate: f.nSamplesPerSec,
        channels: f.nChannels,
        float,
        bits: f.wBitsPerSample,
    };
    CoTaskMemFree(Some(fmt as *const _));
    r.ok()?;
    // The AAC encoder takes these two rates.
    (lb.rate == 44100 || lb.rate == 48000).then_some(lb)
}

/// `frames` frames of the loopback's samples at `data` as 16-bit stereo.
pub(crate) unsafe fn to_pcm16(lb: &Loopback, data: *const u8, frames: usize, silent: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(frames * 4);
    let ch = lb.channels.max(1) as usize;
    for i in 0..frames {
        for c in 0..2 {
            let src = c.min(ch - 1);
            let v: i16 = if silent || data.is_null() {
                0
            } else if lb.float && lb.bits == 32 {
                let s = *(data as *const f32).add(i * ch + src);
                (s.clamp(-1.0, 1.0) * 32767.0) as i16
            } else if lb.bits == 16 {
                *(data as *const i16).add(i * ch + src)
            } else if lb.bits == 32 {
                (*(data as *const i32).add(i * ch + src) >> 16) as i16
            } else {
                0
            };
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

// ------------------------------------------------------------- the file --

/// The MP4 writer, with a video stream (and a sound one at `audio`'s rate, if any).
pub(crate) unsafe fn writer(
    path: &std::path::Path,
    w: u32,
    h: u32,
    audio: Option<u32>,
) -> windows::core::Result<(IMFSinkWriter, u32, Option<u32>)> {
    let mut attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attrs, 2)?;
    let attrs = attrs.ok_or(windows::core::Error::from(E_FAIL))?;
    attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
    let sink = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs)?;
    let size = ((w as u64) << 32) | h as u64;
    let rate = ((FPS as u64) << 32) | 1;
    let ratio = (1u64 << 32) | 1;
    // What's written: H.264.
    let out = MFCreateMediaType()?;
    out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
    let bitrate = ((w as u64 * h as u64 * FPS as u64) / 9).clamp(2_000_000, 40_000_000) as u32;
    out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
    out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    out.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
    out.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
    out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, ratio)?;
    let video = sink.AddStream(&out)?;
    // What's handed over: the screen's pixels, top row first.
    let input = MFCreateMediaType()?;
    input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
    input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    input.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
    input.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
    input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, ratio)?;
    input.SetUINT32(&MF_MT_DEFAULT_STRIDE, w * 4)?;
    sink.SetInputMediaType(video, &input, None)?;
    let sound = match audio {
        None => None,
        Some(hz) => {
            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, hz)?;
            out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24000)?;
            let stream = sink.AddStream(&out)?;
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, hz)?;
            input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
            input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, hz * 4)?;
            match sink.SetInputMediaType(stream, &input, None) {
                Ok(()) => Some(stream),
                Err(e) => {
                    config::log(&format!("recorder: no sound track ({e})"));
                    None
                }
            }
        }
    };
    Ok((sink, video, sound))
}

pub(crate) unsafe fn sample(bytes: &[u8], time: i64, duration: i64) -> windows::core::Result<IMFSample> {
    let buf = MFCreateMemoryBuffer(bytes.len() as u32)?;
    let mut at: *mut u8 = std::ptr::null_mut();
    buf.Lock(&mut at, None, None)?;
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len());
    buf.Unlock()?;
    buf.SetCurrentLength(bytes.len() as u32)?;
    let s = MFCreateSample()?;
    s.AddBuffer(&buf)?;
    s.SetSampleTime(time)?;
    s.SetSampleDuration(duration)?;
    Ok(s)
}

// ------------------------------------------------------------ recording --

/// Records `area` (screen pixels), with the PC's sound if `audio`, after
/// counting down `countdown` seconds (0 for none).
pub fn start(area: RECT, audio: bool, countdown: u32) {
    if RECORDING.swap(true, Ordering::AcqRel) {
        return;
    }
    STOP.store(false, Ordering::Release);
    // H.264 wants even sizes.
    let area = RECT {
        right: area.left + ((area.right - area.left) & !1),
        bottom: area.top + ((area.bottom - area.top) & !1),
        ..area
    };
    std::thread::spawn(move || unsafe {
        BEGAN.store(0, Ordering::Release);
        if !crate::recui::countdown(area, countdown) {
            RECORDING.store(false, Ordering::Release);
            return;
        }
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let result = record(area, audio);
        RECORDING.store(false, Ordering::Release);
        match result {
            Ok((path, thumb, secs)) => {
                config::log(&format!("recorder: saved {}", path.display()));
                crate::recui::preview(path, thumb, secs, area);
            }
            Err(e) => {
                config::log(&format!("recorder: failed ({e})"));
                msg::note(format!("The recording couldn't be made ({e})"));
            }
        }
    });
}

/// The video file, its last frame and how long it runs.
type Recorded = (PathBuf, Option<crate::recui::Thumb>, f32);

unsafe fn record(area: RECT, audio: bool) -> windows::core::Result<Recorded> {
    MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    let (w, h) = ((area.right - area.left) as u32, (area.bottom - area.top) as u32);
    if w < 16 || h < 16 {
        let _ = MFShutdown();
        return Err(windows::core::Error::from(E_INVALIDARG));
    }
    let lb = if audio { open_loopback() } else { None };
    let path = output_path();
    let (sink, video, sound) = writer(&path, w, h, lb.as_ref().map(|l| l.rate))?;
    sink.BeginWriting()?;
    let start = qpc();
    BEGAN.store(start as isize, Ordering::Release);
    let shared = Arc::new(Mutex::new(Writer(sink.clone())));

    // The bar, and an edge round the area, on a thread of their own (left out of the recording).
    let ui = std::thread::spawn(move || crate::recui::bar(area));

    // Sound.
    let sound_thread = match (lb, sound) {
        (Some(lb), Some(stream)) => {
            let shared = shared.clone();
            let lb = SendLoopback(lb);
            Some(std::thread::spawn(move || {
                let lb = lb;
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                capture_sound(&lb.0, &shared, stream, start);
            }))
        }
        _ => None,
    };

    // Video, here.
    let screen = GetDC(None);
    let mem = CreateCompatibleDC(Some(screen));
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let dib = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
    let old = SelectObject(mem, dib.into());
    let frame = 10_000_000 / FPS as i64;
    let mut next = 0i64;
    let mut failed = None;
    while !STOP.load(Ordering::Acquire) {
        let now = hns(qpc() - start);
        if now < next {
            std::thread::sleep(std::time::Duration::from_micros(((next - now) / 10) as u64));
            continue;
        }
        let _ = BitBlt(mem, 0, 0, w as i32, h as i32, Some(screen), area.left, area.top, SRCCOPY | CAPTUREBLT);
        draw_pointer(mem, area);
        let _ = GdiFlush();
        let bytes = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
        let s = sample(bytes, now, frame)?;
        if let Err(e) = shared.lock().unwrap_or_else(|e| e.into_inner()).0.WriteSample(video, &s) {
            failed = Some(e);
            break;
        }
        // The next frame on the beat (frames that fall behind are skipped, not bunched).
        next = (now / frame + 1) * frame;
    }
    STOP.store(true, Ordering::Release);
    let secs = elapsed();
    let thumb =
        crate::recui::Thumb::from_frame(std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize), w, h, 640);
    SelectObject(mem, old);
    let _ = DeleteObject(dib.into());
    let _ = DeleteDC(mem);
    ReleaseDC(None, screen);
    if let Some(t) = sound_thread {
        let _ = t.join();
    }
    let finished = shared.lock().unwrap_or_else(|e| e.into_inner()).0.Finalize();
    crate::recui::close_bar();
    let _ = ui.join();
    let _ = MFShutdown();
    if let Some(e) = failed {
        return Err(e);
    }
    finished.map(|_| (path, thumb, secs))
}

struct SendLoopback(Loopback);
unsafe impl Send for SendLoopback {}

unsafe fn capture_sound(lb: &Loopback, shared: &Mutex<Writer>, stream: u32, start: i64) {
    if lb.client.Start().is_err() {
        return;
    }
    let start_hns = hns(start);
    while !STOP.load(Ordering::Acquire) {
        std::thread::sleep(std::time::Duration::from_millis(10));
        loop {
            let Ok(n) = lb.capture.GetNextPacketSize() else { break };
            if n == 0 {
                break;
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let mut qpc_pos = 0u64;
            if lb.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc_pos)).is_err() {
                break;
            }
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            let pcm = to_pcm16(lb, data, frames as usize, silent);
            let _ = lb.capture.ReleaseBuffer(frames);
            // Its time on the same clock as the video's (QPC, in 100 ns units).
            let time = qpc_pos as i64 - start_hns;
            if time < 0 {
                continue;
            }
            let duration = frames as i64 * 10_000_000 / lb.rate as i64;
            if let Ok(s) = sample(&pcm, time, duration) {
                let _ = shared.lock().unwrap_or_else(|e| e.into_inner()).0.WriteSample(stream, &s);
            }
        }
    }
    let _ = lb.client.Stop();
}

/// Draws the pointer into the frame, where it is on the screen.
unsafe fn draw_pointer(dc: HDC, area: RECT) {
    let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    if GetCursorInfo(&mut ci).is_err() || ci.flags.0 & CURSOR_SHOWING.0 == 0 {
        return;
    }
    let mut ii = ICONINFO::default();
    if GetIconInfo(HICON(ci.hCursor.0), &mut ii).is_err() {
        return;
    }
    let x = ci.ptScreenPos.x - ii.xHotspot as i32 - area.left;
    let y = ci.ptScreenPos.y - ii.yHotspot as i32 - area.top;
    let _ = DrawIconEx(dc, x, y, HICON(ci.hCursor.0), 0, 0, 0, None, DI_NORMAL);
    if !ii.hbmMask.is_invalid() {
        let _ = DeleteObject(ii.hbmMask.into());
    }
    if !ii.hbmColor.is_invalid() {
        let _ = DeleteObject(ii.hbmColor.into());
    }
}
