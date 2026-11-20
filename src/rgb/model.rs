//! What Cascade's lighting is set to: profiles (layers of effects, which
//! devices each covers, devices left on one of their own hardware modes,
//! the apps a profile comes on for), where each device sits on the canvas,
//! and the settings. All in one file, `lighting.json` in Cascade's data
//! folder, written by the Lighting app and read by the engine (which picks
//! up changes as they're saved).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::effects::{Blend, Effect, Kind, Led};
use super::openrgb::{Controller, ZONE_MATRIX};

/// Which devices a layer lights.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub enum Target {
    #[default]
    All,
    /// These devices (by `Controller::key`).
    Devices(Vec<String>),
    /// These zones: (device key, zone number).
    Zones(Vec<(String, usize)>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layer {
    pub name: String,
    pub effect: Effect,
    pub target: Target,
    pub blend: Blend,
    pub opacity: f32,
    pub on: bool,
}

impl Default for Layer {
    fn default() -> Self {
        Layer {
            name: String::new(),
            effect: Effect::default(),
            target: Target::All,
            blend: Blend::Normal,
            opacity: 1.0,
            on: true,
        }
    }
}

impl Layer {
    pub fn new(kind: Kind) -> Layer {
        Layer {
            name: kind.name().to_string(),
            effect: Effect { kind, colours: kind.default_colours(), ..Default::default() },
            ..Default::default()
        }
    }

    pub fn covers(&self, key: &str, zone: usize) -> bool {
        match &self.target {
            Target::All => true,
            Target::Devices(d) => d.iter().any(|k| k == key),
            Target::Zones(z) => z.iter().any(|(k, i)| k == key && *i == zone),
        }
    }
}

/// A device left on one of its own modes (run by the device itself, as
/// iCUE's hardware lighting is): the mode's name and how it's set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Hardware {
    pub mode: String,
    pub colours: Vec<u32>,
    /// 0..1 of the mode's own range.
    pub speed: f32,
    pub brightness: f32,
    pub direction: u32,
    /// Also save it to the device, so it keeps it with Cascade closed (or the PC off).
    pub save: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    /// Bottom first.
    pub layers: Vec<Layer>,
    /// Devices on a hardware mode instead (by device key).
    pub hardware: HashMap<String, Hardware>,
    /// Programs this profile comes on for while they're in front (exe names, lower case).
    pub apps: Vec<String>,
    /// 0..1, on top of the global brightness.
    pub brightness: f32,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: "Default".into(),
            layers: vec![Layer::new(Kind::RainbowWave)],
            hardware: HashMap::new(),
            apps: Vec::new(),
            brightness: 1.0,
        }
    }
}

/// Where a device sits on the canvas, and how its LEDs are spread over it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Place {
    /// Top-left corner and size, 0..1 of the canvas.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// LEDs laid out as a ring (fans) rather than a line.
    pub ring: bool,
    /// Flipped (a fan facing the other way, a strip run backwards).
    pub flip: bool,
    /// Left out of Cascade's lighting altogether.
    pub off: bool,
}

impl Default for Place {
    fn default() -> Self {
        Place { x: 0.1, y: 0.1, w: 0.3, h: 0.15, ring: false, flip: false, off: false }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Lighting on at all.
    pub on: bool,
    /// 0..1 for everything.
    pub brightness: f32,
    /// Frames a second for moving effects (still ones are sent once).
    pub fps: u32,
    /// Switch profiles as apps come to the front.
    pub auto_switch: bool,
    /// Lights off while the PC is locked or its screens are off.
    pub off_when_idle: bool,
    /// OpenRGB's server port.
    pub port: u16,
    /// Screen ambience: how many times a second the screen is looked at.
    pub screen_fps: u32,
    /// OpenRGB.exe, if not found by itself.
    pub openrgb: String,
    /// Start OpenRGB with administrator rights (for RAM, motherboard and GPU lighting).
    pub admin: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            on: true,
            brightness: 1.0,
            fps: 30,
            auto_switch: true,
            off_when_idle: true,
            port: super::openrgb::PORT,
            screen_fps: 20,
            openrgb: String::new(),
            admin: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Lighting {
    pub profiles: Vec<Profile>,
    /// The profile chosen (the one used when no app's profile is in front).
    pub active: usize,
    pub layout: HashMap<String, Place>,
    pub settings: Settings,
}

impl Lighting {
    pub fn path() -> std::path::PathBuf {
        crate::config::dir().join("lighting.json")
    }

    pub fn load() -> Lighting {
        let mut l: Lighting =
            std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        if l.profiles.is_empty() {
            l.profiles.push(Profile::default());
        }
        l.active = l.active.min(l.profiles.len() - 1);
        l
    }

    /// Saves it (written to a new file and swapped in, so a reader never sees half).
    pub fn save(&self) {
        let Ok(text) = serde_json::to_string_pretty(self) else { return };
        let path = Self::path();
        let tmp = path.with_extension("json.tmp");
        let _ = std::fs::create_dir_all(crate::config::dir());
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// The profile for `exe` in front (when switching by app), or the chosen one.
    pub fn profile_for(&self, exe: Option<&str>) -> usize {
        if self.settings.auto_switch {
            if let Some(e) = exe {
                let e = e.to_ascii_lowercase();
                if let Some(i) = self.profiles.iter().position(|p| p.apps.contains(&e)) {
                    return i;
                }
            }
        }
        self.active
    }

    /// Places the devices that have no place yet, one under another.
    pub fn place_new(&mut self, devices: &[Controller]) {
        let mut y = self.layout.values().filter(|p| !p.off).map(|p| p.y + p.h).fold(0.05f32, f32::max);
        for d in devices {
            let key = d.key();
            if self.layout.contains_key(&key) {
                continue;
            }
            let (w, h, ring) = match d.kind {
                5 => (0.6, 0.22, false),  // keyboard
                6 => (0.08, 0.12, false), // mouse
                7 => (0.3, 0.2, false),   // mouse mat
                1 => (0.04, 0.3, false),  // memory: upright
                3 => (0.14, 0.14, true),  // cooler: a ring
                2 => (0.4, 0.08, false),  // graphics card
                _ => (0.5, 0.06, false),
            };
            if y + h > 0.98 {
                y = 0.05;
            }
            self.layout.insert(key, Place { x: 0.05, y, w, h, ring, ..Default::default() });
            y += h + 0.03;
        }
    }
}

/// Where each LED of `d` sits on the canvas (in the device's own LED order),
/// with its zone, placed at `p`.
pub fn led_positions(d: &Controller, p: &Place) -> Vec<(Led, usize)> {
    let mut out = Vec::with_capacity(d.leds.len());
    let mut base = 0usize;
    let total = d.leds.len().max(1);
    for (zi, z) in d.zones.iter().enumerate() {
        let n = z.leds_count as usize;
        // Within the device's box: 0..1 across and down, for this zone's LEDs.
        let mut local: Vec<(f32, f32)> = vec![(0.5, 0.5); n];
        match &z.matrix {
            Some((h, w, cells)) if z.kind == ZONE_MATRIX && *h > 0 && *w > 0 => {
                for (cell, &led) in cells.iter().enumerate() {
                    if (led as usize) < n {
                        let (cx, cy) = (cell as u32 % w, cell as u32 / w);
                        local[led as usize] = ((cx as f32 + 0.5) / *w as f32, (cy as f32 + 0.5) / *h as f32);
                    }
                }
            }
            _ if p.ring && n > 2 => {
                for (i, slot) in local.iter_mut().enumerate() {
                    let a = i as f32 / n as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                    *slot = (0.5 + 0.5 * a.cos(), 0.5 + 0.5 * a.sin());
                }
            }
            _ => {
                // A line along the box's longer side; several zones stacked across the other.
                let zones = d.zones.len().max(1) as f32;
                let across = (zi as f32 + 0.5) / zones;
                let tall = p.h > p.w;
                for (i, slot) in local.iter_mut().enumerate() {
                    let t = if n > 1 { i as f32 / (n - 1) as f32 } else { 0.5 };
                    *slot = if tall { (across, t) } else { (t, across) };
                }
            }
        }
        for (i, (lx, ly)) in local.into_iter().enumerate() {
            let lx = if p.flip { 1.0 - lx } else { lx };
            let index = base + i;
            out.push((
                Led {
                    x: p.x + lx * p.w,
                    y: p.y + ly * p.h,
                    index,
                    count: total,
                    id: (index as u32).wrapping_mul(2654435761) ^ hash_str(&d.name),
                    key: d.leds.get(index).and_then(|n| super::keys::vk_for(n)),
                },
                zi,
            ));
        }
        base += n;
    }
    // LEDs not in any zone (shouldn't happen): in the middle.
    while out.len() < d.leds.len() {
        let index = out.len();
        out.push((Led { x: p.x + p.w / 2.0, y: p.y + p.h / 2.0, index, count: total, id: index as u32, key: None }, 0));
    }
    out
}

fn hash_str(s: &str) -> u32 {
    s.bytes().fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rgb::openrgb::Zone;

    #[test]
    fn profiles_round_trip() {
        let mut l = Lighting { profiles: vec![Profile::default()], ..Default::default() };
        l.profiles.push(Profile { name: "Game".into(), apps: vec!["valorant.exe".into()], ..Default::default() });
        l.profiles[0].layers.push(Layer::new(Kind::TypeRipple));
        let text = serde_json::to_string(&l).unwrap();
        let back: Lighting = serde_json::from_str(&text).unwrap();
        assert_eq!(back, l);
        assert_eq!(back.profile_for(Some("VALORANT.exe")), 1);
        assert_eq!(back.profile_for(Some("notepad.exe")), 0);
    }

    #[test]
    fn leds_spread_over_their_place() {
        let d = Controller {
            name: "Strip".into(),
            leds: (0..10).map(|i| format!("LED {i}")).collect(),
            zones: vec![Zone { name: "Z".into(), kind: 1, leds_count: 10, ..Default::default() }],
            ..Default::default()
        };
        let p = Place { x: 0.2, y: 0.5, w: 0.5, h: 0.1, ..Default::default() };
        let leds = led_positions(&d, &p);
        assert_eq!(leds.len(), 10);
        assert!((leds[0].0.x - 0.2).abs() < 1e-5 && (leds[9].0.x - 0.7).abs() < 1e-5);
        assert!(leds.iter().all(|(l, _)| (l.y - 0.55).abs() < 1e-5));
    }
}
