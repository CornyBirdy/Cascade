//! Sound Recorder (`cascade.exe --soundrec`), as Windows' own: one big
//! button records the microphone (Windows' default) to a WAV file in
//! Documents\Sound Recordings, a meter shows the level as it goes, and the
//! recordings are listed to play (in Media Player), show or delete.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use windows::core::w;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

use super::{App, Ctx, Kit, Look, PANEL};
use crate::gfx::{Align, Color, Fonts, Painter};

fn folder() -> PathBuf {
    let base =
        super::known_folder(&windows::Win32::UI::Shell::FOLDERID_Documents).unwrap_or_else(|| PathBuf::from("."));
    base.join("Sound Recordings")
}

fn recordings() -> Vec<(PathBuf, u64)> {
    let mut out: Vec<(PathBuf, u64, std::time::SystemTime)> = std::fs::read_dir(folder())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path(), m.len(), m.modified().ok()?))
        })
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.2));
    out.into_iter().map(|(p, s, _)| (p, s)).collect()
}

/// A WAV's header for 16-bit PCM (`data_len` bytes of samples).
fn wav_header(rate: u32, channels: u16, data_len: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(44);
    let block = channels * 2;
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data_len).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * block as u32).to_le_bytes());
    h.extend_from_slice(&block.to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());
    h
}

/// Records the default microphone into `path` until `stop`; `level` gets
/// the loudness (0..1000) as it goes.
fn record(path: PathBuf, stop: Arc<AtomicBool>, level: Arc<AtomicU32>) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let e: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
        let dev = e.GetDefaultAudioEndpoint(eCapture, eConsole).map_err(|_| "No microphone is set up".to_string())?;
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).map_err(|e| e.to_string())?;
        let fmt = client.GetMixFormat().map_err(|e| e.to_string())?;
        let f = *fmt;
        let float = f.wFormatTag == 3
            || (f.wFormatTag == 0xFFFE && (*(fmt as *const WAVEFORMATEXTENSIBLE)).SubFormat.data1 == 3);
        let (rate, ch, bits) = (f.nSamplesPerSec, f.nChannels.max(1), f.wBitsPerSample);
        let r = client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 10_000_000, 0, fmt, None);
        CoTaskMemFree(Some(fmt as *const _));
        r.map_err(|e| e.to_string())?;
        let capture: IAudioCaptureClient = client.GetService().map_err(|e| e.to_string())?;
        let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        use std::io::{Seek, Write};
        let out_ch = ch.min(2);
        file.write_all(&wav_header(rate, out_ch, 0)).map_err(|e| e.to_string())?;
        let mut written = 0u32;
        client.Start().map_err(|e| e.to_string())?;
        while !stop.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(15));
            loop {
                let Ok(n) = capture.GetNextPacketSize() else { break };
                if n == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let (mut frames, mut flags) = (0u32, 0u32);
                if capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None).is_err() {
                    break;
                }
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                let mut pcm = Vec::with_capacity(frames as usize * out_ch as usize * 2);
                let mut peak = 0i32;
                for i in 0..frames as usize {
                    for c in 0..out_ch as usize {
                        let v: i16 = if silent || data.is_null() {
                            0
                        } else if float && bits == 32 {
                            (((*(data as *const f32).add(i * ch as usize + c)).clamp(-1.0, 1.0)) * 32767.0) as i16
                        } else if bits == 16 {
                            *(data as *const i16).add(i * ch as usize + c)
                        } else if bits == 32 {
                            (*(data as *const i32).add(i * ch as usize + c) >> 16) as i16
                        } else {
                            0
                        };
                        peak = peak.max((v as i32).abs());
                        pcm.extend_from_slice(&v.to_le_bytes());
                    }
                }
                let _ = capture.ReleaseBuffer(frames);
                level.store((peak as u32 * 1000 / 32767).min(1000), Ordering::Release);
                if file.write_all(&pcm).is_ok() {
                    written += pcm.len() as u32;
                }
            }
        }
        let _ = client.Stop();
        // The sizes, now they're known.
        let _ = file.seek(std::io::SeekFrom::Start(0));
        let _ = file.write_all(&wav_header(rate, out_ch, written));
        Ok(())
    }
}

struct Recorder {
    stop: Option<Arc<AtomicBool>>,
    level: Arc<AtomicU32>,
    began: Instant,
    meter: Vec<f32>,
    list: Vec<(PathBuf, u64)>,
    error: Option<String>,
    worker: Option<std::thread::JoinHandle<Result<(), String>>>,
}

const REC: u32 = 1;
const PLAY: u32 = 100;
const DEL: u32 = 300;
const SHOW: u32 = 500;

impl App for Recorder {
    fn draw(&mut self, p: &Painter, f: &Fonts, w: f32, h: f32, kit: &mut Kit) {
        let accent = crate::theme::accent();
        let recording = self.stop.is_some();
        // The big button, the time and the meter.
        let (cx, cy) = (w / 2.0, 150.0);
        let red = Color(0.9, 0.25, 0.22, 1.0);
        if recording {
            let pulse = 0.5 + 0.5 * (self.began.elapsed().as_secs_f32() * 3.0).sin().abs();
            p.circle(cx, cy, 64.0, red.alpha(0.15 * pulse));
        }
        kit.round(
            p,
            f,
            cx,
            cy,
            48.0,
            if recording { 0xE71A } else { 0xE720 },
            if recording { Look::Danger } else { Look::Accent },
            REC,
        );
        let label = if recording {
            let s = self.began.elapsed().as_secs();
            format!("{:02}:{:02}", s / 60, s % 60)
        } else {
            "Press to record".into()
        };
        p.text(&label, &f.title, 0.0, cy + 60.0, w, 30.0, Align::Center, Color::white(0.9));
        let bars = self.meter.len();
        let bw = 4.0;
        let total = bars as f32 * (bw + 2.0);
        for (i, v) in self.meter.iter().enumerate() {
            let bh = (v * 60.0).max(2.0);
            let x = cx - total / 2.0 + i as f32 * (bw + 2.0);
            p.rrect(x, cy + 130.0 - bh / 2.0, bw, bh, 2.0, if recording { accent } else { Color::white(0.2) });
        }
        if let Some(e) = &self.error {
            p.text(e, &f.small, 0.0, cy + 172.0, w, 18.0, Align::Center, Color(1.0, 0.55, 0.5, 1.0));
        }
        // The recordings.
        let top = 360.0;
        p.text("Recordings", &f.title, 24.0, top, w - 48.0, 26.0, Align::Left, Color::white(0.92));
        if self.list.is_empty() {
            p.text("None yet", &f.small, 24.0, top + 32.0, w - 48.0, 20.0, Align::Left, Color::white(0.45));
        }
        for (i, (path, size)) in self.list.iter().enumerate() {
            let y = top + 34.0 + i as f32 * 50.0;
            if y > h - 50.0 {
                break;
            }
            p.rrect(24.0, y, w - 48.0, 44.0, 8.0, PANEL);
            let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            p.text(&name, &f.body, 40.0, y, w - 220.0, 44.0, Align::Left, Color::white(0.9));
            // Its length: 16-bit samples at the format in the header (near enough from the size).
            p.text(
                &crate::appwin::size_text(*size),
                &f.small,
                w - 230.0,
                y,
                80.0,
                44.0,
                Align::Right,
                Color::white(0.5),
            );
            let id = i as u32;
            kit.round(p, f, w - 126.0, y + 22.0, 15.0, 0xE768, Look::Plain, PLAY + id);
            kit.round(p, f, w - 90.0, y + 22.0, 15.0, 0xE8B7, Look::Plain, SHOW + id);
            kit.round(p, f, w - 54.0, y + 22.0, 15.0, 0xE74D, Look::Plain, DEL + id);
        }
    }

    fn press(&mut self, id: u32, ctx: &Ctx) {
        match id {
            REC => match self.stop.take() {
                Some(stop) => {
                    stop.store(true, Ordering::Release);
                    if let Some(Err(e)) = self.worker.take().and_then(|w| w.join().ok()) {
                        self.error = Some(e);
                    }
                    self.list = recordings();
                }
                None => {
                    let _ = std::fs::create_dir_all(folder());
                    let path = folder().join(super::stamped("Recording", "wav"));
                    let stop = Arc::new(AtomicBool::new(false));
                    let (s, l) = (stop.clone(), self.level.clone());
                    self.worker = Some(std::thread::spawn(move || record(path, s, l)));
                    self.stop = Some(stop);
                    self.began = Instant::now();
                    self.error = None;
                }
            },
            x if (PLAY..PLAY + 200).contains(&x) => {
                if let Some((p, _)) = self.list.get((x - PLAY) as usize) {
                    crate::appwin::launch(&["--player", &p.to_string_lossy()]);
                }
            }
            x if (SHOW..SHOW + 200).contains(&x) => {
                crate::appwin::launch(&["--files", &folder().to_string_lossy()]);
            }
            x if (DEL..DEL + 200).contains(&x) => {
                if let Some((p, _)) = self.list.get((x - DEL) as usize) {
                    crate::deskicons::delete(ctx.hwnd, &[p.to_string_lossy().into_owned()], false);
                }
                self.list = recordings();
            }
            _ => {}
        }
        ctx.redraw();
    }

    fn tick_ms(&self) -> u32 {
        if self.stop.is_some() {
            40
        } else {
            0
        }
    }

    fn tick(&mut self, _: &Ctx) -> bool {
        // A recording that stopped by itself (the microphone went away).
        if self.worker.as_ref().is_some_and(|w| w.is_finished()) {
            if let Some(Err(e)) = self.worker.take().and_then(|w| w.join().ok()) {
                self.error = Some(e);
            }
            self.stop = None;
            self.list = recordings();
        }
        let v = self.level.load(Ordering::Acquire) as f32 / 1000.0;
        self.meter.remove(0);
        self.meter.push(v.sqrt());
        true
    }

    fn closing(&mut self, _: &Ctx) -> bool {
        if let Some(stop) = self.stop.take() {
            stop.store(true, Ordering::Release);
            if let Some(w) = self.worker.take() {
                let _ = w.join();
            }
        }
        true
    }
}

/// `cascade.exe --soundrec`.
pub fn run() {
    super::run(w!("CascadeSoundRecorder"), "Sound Recorder", "Cascade.SoundRecorder", 640, 720, |_| {
        Box::new(Recorder {
            stop: None,
            level: Arc::new(AtomicU32::new(0)),
            began: Instant::now(),
            meter: vec![0.0; 48],
            list: recordings(),
            error: None,
            worker: None,
        })
    });
}
