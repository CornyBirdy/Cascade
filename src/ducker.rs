//! The auto ducker (`ducker`): while a chosen app is making sound (Discord, with
//! your friends talking), the apps you chose (or everything else) are turned
//! down, and brought back a moment after it goes quiet.
//!
//! A thread of its own looks at Windows' audio sessions a few times a second.
//! An app counts as speaking when its session is active and its meter is above
//! a small threshold for a moment; others fade down to `level` of what they were
//! and back up again. Volumes you change by hand while they're down are taken
//! as the new "normal". Everything is put back when the ducker is turned off or
//! Cascade closes.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{Interface, Result, PWSTR};
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

/// How often the sessions are looked at.
const TICK: Duration = Duration::from_millis(80);
/// A meter reading above this is sound (about -34 dB).
const LOUD: f32 = 0.02;
/// How long the trigger has to be loud before the others go down, so a single
/// notification ping doesn't.
const ATTACK: Duration = Duration::from_millis(150);
/// How long a full fade down or up takes.
const FADE: Duration = Duration::from_millis(280);
/// A volume this far from what the ducker set was changed by someone else.
const MOVED: f32 = 0.03;

/// What the ducker is set to (see `Config`).
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Params {
    /// Apps that duck the others when they make sound.
    pub trigger: Vec<String>,
    /// Apps that are turned down; none listed means every other app.
    pub targets: Vec<String>,
    /// What the turned-down apps are left at, 0 to 1 of their own volume.
    pub level: f32,
    /// How long after the trigger goes quiet the others come back.
    pub release: Duration,
}

/// "Discord, Spotify.exe" as lower-case process names: ["discord", "spotify"].
pub fn parse_list(text: &str) -> Vec<String> {
    text.split([',', ';'])
        .map(|s| s.trim().trim_matches('"').to_lowercase())
        .map(|s| s.strip_suffix(".exe").map(str::to_string).unwrap_or(s))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether process name `name` is one of `list`: the same, or one that
/// contains it ("discord" for "discordptb").
fn listed(name: &str, list: &[String]) -> bool {
    list.iter().any(|w| name == w || name.contains(w.as_str()))
}

/// The settings the thread goes by (None: off) and the thread itself.
static PARAMS: Mutex<Option<Params>> = Mutex::new(None);
static THREAD: Mutex<Option<(Arc<AtomicBool>, JoinHandle<()>)>> = Mutex::new(None);

/// Turns the ducker on with `params`, changes its settings, or (None) turns it off.
pub fn configure(params: Option<Params>) {
    let mut thread = THREAD.lock().unwrap_or_else(|e| e.into_inner());
    match params.filter(|p| !p.trigger.is_empty()) {
        Some(p) => {
            *PARAMS.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
            if thread.is_none() {
                let stop = Arc::new(AtomicBool::new(false));
                let flag = stop.clone();
                *thread = Some((stop, std::thread::spawn(move || run(flag))));
            }
        }
        None => {
            if let Some((stop, handle)) = thread.take() {
                stop.store(true, Ordering::Relaxed);
                let _ = handle.join();
            }
            *PARAMS.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
}

/// One audio session, as read this tick.
struct Sess {
    /// Stable for the session's life (an app can have several).
    id: String,
    name: String,
    active: bool,
    peak: f32,
    volume: ISimpleAudioVolume,
}

/// A turned-down app: its volume before, and what was last set.
struct Saved {
    volume: ISimpleAudioVolume,
    normal: f32,
    set: f32,
}

#[derive(Default)]
struct State {
    /// 0 (not ducked) to 1 (fully ducked).
    amount: f32,
    /// Whether the others should be down: set once the trigger has been loud
    /// for `ATTACK`, cleared once it's been quiet for `release`.
    on: bool,
    loud_since: Option<Instant>,
    last_loud: Option<Instant>,
    saved: HashMap<String, Saved>,
}

impl State {
    /// Puts every turned-down app back.
    fn restore(&mut self) {
        for (_, s) in self.saved.drain() {
            unsafe {
                let _ = s.volume.SetMasterVolume(s.normal, std::ptr::null());
            }
        }
        self.amount = 0.0;
        self.on = false;
    }
}

unsafe fn take(p: PWSTR) -> String {
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

/// The process name ("discord") of `pid`, without its folder and ".exe".
fn process_name(pid: u32, cache: &mut HashMap<u32, String>) -> String {
    cache
        .entry(pid)
        .or_insert_with(|| {
            let path = crate::dock::process_path(pid).unwrap_or_default().to_lowercase();
            let file = path.rsplit(['\\', '/']).next().unwrap_or("").to_string();
            file.strip_suffix(".exe").map(str::to_string).unwrap_or(file)
        })
        .clone()
}

/// The sessions playing through the default output.
fn sessions(names: &mut HashMap<u32, String>) -> Result<Vec<Sess>> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let list = manager.GetSessionEnumerator()?;
        let mut out = Vec::new();
        for i in 0..list.GetCount()? {
            let Ok(session) = list.GetSession(i) else { continue };
            let Ok(session2) = session.cast::<IAudioSessionControl2>() else { continue };
            let Ok(volume) = session.cast::<ISimpleAudioVolume>() else { continue };
            let state = session.GetState().unwrap_or(AudioSessionStateInactive);
            if state == AudioSessionStateExpired {
                continue;
            }
            // Windows' own sounds (process 0) are left alone.
            let pid = session2.GetProcessId().unwrap_or(0);
            if pid == 0 {
                continue;
            }
            let id = session2.GetSessionInstanceIdentifier().map(|p| take(p)).unwrap_or_default();
            let peak = session.cast::<IAudioMeterInformation>().and_then(|m| m.GetPeakValue()).unwrap_or(0.0);
            out.push(Sess {
                id,
                name: process_name(pid, names),
                active: state == AudioSessionStateActive,
                peak,
                volume,
            });
        }
        Ok(out)
    }
}

/// One look at the sessions: decides whether the others should be down, moves
/// them a step towards it and applies that.
fn tick(st: &mut State, p: &Params, dt: Duration, names: &mut HashMap<u32, String>) -> Result<()> {
    let list = sessions(names)?;
    let now = Instant::now();
    let speaking = list.iter().any(|s| s.active && s.peak > LOUD && listed(&s.name, &p.trigger));
    if speaking {
        st.last_loud = Some(now);
        let since = *st.loud_since.get_or_insert(now);
        if now.duration_since(since) >= ATTACK {
            st.on = true;
        }
    } else {
        st.loud_since = None;
        if st.on && st.last_loud.is_none_or(|t| now.duration_since(t) >= p.release) {
            st.on = false;
        }
    }
    let step = dt.as_secs_f32() / FADE.as_secs_f32();
    st.amount = if st.on { (st.amount + step).min(1.0) } else { (st.amount - step).max(0.0) };
    if st.amount <= 0.0 && st.saved.is_empty() {
        return Ok(());
    }

    // What a turned-down app plays at, as a share of its normal volume.
    let factor = 1.0 - st.amount * (1.0 - p.level.clamp(0.0, 1.0));
    let mut seen = HashSet::new();
    for s in &list {
        let target = !listed(&s.name, &p.trigger) && (p.targets.is_empty() || listed(&s.name, &p.targets));
        if !target || s.id.is_empty() {
            continue;
        }
        seen.insert(s.id.clone());
        let now_vol = unsafe { s.volume.GetMasterVolume() }.unwrap_or(1.0);
        match st.saved.get_mut(&s.id) {
            Some(saved) => {
                if (now_vol - saved.set).abs() > MOVED {
                    // Someone moved it: that's its normal now.
                    saved.normal = (now_vol / factor.max(0.05)).clamp(0.0, 1.0);
                }
                saved.set = (saved.normal * factor).clamp(0.0, 1.0);
                unsafe {
                    let _ = saved.volume.SetMasterVolume(saved.set, std::ptr::null());
                }
            }
            None if st.amount > 0.0 => {
                let set = (now_vol * factor).clamp(0.0, 1.0);
                unsafe {
                    let _ = s.volume.SetMasterVolume(set, std::ptr::null());
                }
                st.saved.insert(s.id.clone(), Saved { volume: s.volume.clone(), normal: now_vol, set });
            }
            None => {}
        }
    }
    // Apps that closed while down have nothing to put back.
    st.saved.retain(|id, _| seen.contains(id));
    if st.amount <= 0.0 {
        st.restore();
    }
    Ok(())
}

fn run(stop: Arc<AtomicBool>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut st = State::default();
    let mut names: HashMap<u32, String> = HashMap::new();
    let mut last = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(TICK);
        let now = Instant::now();
        let dt = now.duration_since(last);
        last = now;
        let params = PARAMS.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match params {
            Some(p) => {
                if tick(&mut st, &p, dt, &mut names).is_err() {
                    // The output went away (a device change): start over on the next one.
                    st.saved.clear();
                    st.amount = 0.0;
                    st.on = false;
                }
                // Names of processes that came and went aren't kept for ever.
                if names.len() > 256 {
                    names.clear();
                }
            }
            None => st.restore(),
        }
    }
    st.restore();
    unsafe {
        CoUninitialize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_lower_case_names_without_exe() {
        assert_eq!(parse_list("Discord, Spotify.exe ; \"Chrome\" ,,"), vec!["discord", "spotify", "chrome"]);
        assert!(parse_list("  ").is_empty());
    }

    #[test]
    fn a_name_matches_whole_or_inside() {
        let list = parse_list("discord, game");
        assert!(listed("discord", &list) && listed("discordptb", &list) && listed("mygame", &list));
        assert!(!listed("spotify", &list));
    }
}
