//! Lighting effects: what colour each LED is at a moment, worked out from
//! where it sits on the shared canvas (so a wave runs across every device,
//! whatever its brand, as one surface), the time, and what's happening (the
//! music, the screen, temperatures, keys pressed). Plain arithmetic with no
//! Windows calls, so it's tested on its own; the engine (see `engine`) feeds
//! it and sends the colours to the devices.
//!
//! Effects stack in layers, as iCUE's do: each layer covers all devices or
//! the ones picked, with its own opacity and blend, over the layers below.

use serde::{Deserialize, Serialize};

/// A colour, 0 to 1 per channel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rgb(pub f32, pub f32, pub f32);

impl Rgb {
    pub const BLACK: Rgb = Rgb(0.0, 0.0, 0.0);

    pub fn from_hex(c: u32) -> Rgb {
        Rgb(((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0)
    }

    pub fn to_bytes(self) -> [u8; 3] {
        let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        [b(self.0), b(self.1), b(self.2)]
    }

    pub fn scale(self, k: f32) -> Rgb {
        Rgb(self.0 * k, self.1 * k, self.2 * k)
    }

    pub fn mix(self, o: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        Rgb(self.0 + (o.0 - self.0) * t, self.1 + (o.1 - self.1) * t, self.2 + (o.2 - self.2) * t)
    }

    /// Hue 0..1 (wrapping), full saturation and brightness.
    pub fn hue(h: f32) -> Rgb {
        let h = h.rem_euclid(1.0) * 6.0;
        let x = 1.0 - (h % 2.0 - 1.0).abs();
        match h as u32 {
            0 => Rgb(1.0, x, 0.0),
            1 => Rgb(x, 1.0, 0.0),
            2 => Rgb(0.0, 1.0, x),
            3 => Rgb(0.0, x, 1.0),
            4 => Rgb(x, 0.0, 1.0),
            _ => Rgb(1.0, 0.0, x),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Kind {
    #[default]
    Static,
    Gradient,
    Spectrum,
    RainbowWave,
    SpiralRainbow,
    ColorShift,
    ColorPulse,
    ColorWave,
    Visor,
    Rain,
    Marquee,
    Sequential,
    Twinkle,
    Strobe,
    Heartbeat,
    Fire,
    Plasma,
    Temperature,
    AudioVisualizer,
    AudioPulse,
    ScreenAmbience,
    TypeKey,
    TypeRipple,
    Off,
}

impl Kind {
    pub const ALL: [Kind; 24] = [
        Kind::Static,
        Kind::Gradient,
        Kind::Spectrum,
        Kind::RainbowWave,
        Kind::SpiralRainbow,
        Kind::ColorShift,
        Kind::ColorPulse,
        Kind::ColorWave,
        Kind::Visor,
        Kind::Rain,
        Kind::Marquee,
        Kind::Sequential,
        Kind::Twinkle,
        Kind::Strobe,
        Kind::Heartbeat,
        Kind::Fire,
        Kind::Plasma,
        Kind::Temperature,
        Kind::AudioVisualizer,
        Kind::AudioPulse,
        Kind::ScreenAmbience,
        Kind::TypeKey,
        Kind::TypeRipple,
        Kind::Off,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Static => "Static",
            Kind::Gradient => "Gradient",
            Kind::Spectrum => "Spectrum cycle",
            Kind::RainbowWave => "Rainbow wave",
            Kind::SpiralRainbow => "Spiral rainbow",
            Kind::ColorShift => "Colour shift",
            Kind::ColorPulse => "Colour pulse",
            Kind::ColorWave => "Colour wave",
            Kind::Visor => "Visor",
            Kind::Rain => "Rain",
            Kind::Marquee => "Marquee",
            Kind::Sequential => "Sequential",
            Kind::Twinkle => "Twinkle",
            Kind::Strobe => "Strobe",
            Kind::Heartbeat => "Heartbeat",
            Kind::Fire => "Fire",
            Kind::Plasma => "Plasma",
            Kind::Temperature => "Temperature",
            Kind::AudioVisualizer => "Audio visualiser",
            Kind::AudioPulse => "Audio pulse",
            Kind::ScreenAmbience => "Screen ambience",
            Kind::TypeKey => "Type lighting (key)",
            Kind::TypeRipple => "Type lighting (ripple)",
            Kind::Off => "Off",
        }
    }

    /// Whether it changes over time (a still effect is sent once, not every frame).
    pub fn moves(self) -> bool {
        !matches!(self, Kind::Static | Kind::Gradient | Kind::Off)
    }

    /// How many colours it takes (min, max).
    pub fn colours(self) -> (usize, usize) {
        match self {
            Kind::Static
            | Kind::ColorPulse
            | Kind::Strobe
            | Kind::Heartbeat
            | Kind::Twinkle
            | Kind::Rain
            | Kind::Visor => (1, 4),
            Kind::Gradient | Kind::ColorShift | Kind::Sequential => (2, 8),
            Kind::ColorWave | Kind::Marquee | Kind::TypeKey | Kind::TypeRipple | Kind::AudioPulse => (1, 2),
            Kind::Temperature | Kind::AudioVisualizer => (3, 3),
            _ => (0, 0),
        }
    }

    /// Default colours for a new layer of this kind.
    pub fn default_colours(self) -> Vec<u32> {
        match self {
            Kind::Temperature => vec![0x0080FF, 0x00FF60, 0xFF2000],
            Kind::AudioVisualizer => vec![0x00FF80, 0xFFE000, 0xFF0040],
            Kind::Gradient | Kind::ColorShift | Kind::Sequential => vec![0xFF0080, 0x00A0FF],
            Kind::ColorWave | Kind::Marquee => vec![0x00A0FF, 0x000000],
            Kind::TypeKey | Kind::TypeRipple => vec![0xFFFFFF, 0x000000],
            Kind::Rain | Kind::Twinkle => vec![0x40C0FF],
            _ => vec![0x00A0FF],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Dir {
    #[default]
    Right,
    Left,
    Down,
    Up,
    Out,
    In,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Blend {
    #[default]
    Normal,
    Add,
    Multiply,
    Screen,
    Lighten,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Sensor {
    #[default]
    Cpu,
    Gpu,
}

/// One effect and its settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Effect {
    pub kind: Kind,
    /// 0xRRGGBB.
    pub colours: Vec<u32>,
    /// 0 (slowest) to 1 (fastest).
    pub speed: f32,
    pub dir: Dir,
    /// How big the wave, band, ripple or drop is: 0 to 1.
    pub size: f32,
    /// Temperature: which sensor, and the range its colours span (°C).
    pub sensor: Sensor,
    pub temp_range: (f32, f32),
}

impl Default for Effect {
    fn default() -> Self {
        Effect {
            kind: Kind::RainbowWave,
            colours: vec![0x00A0FF],
            speed: 0.5,
            dir: Dir::Right,
            size: 0.5,
            sensor: Sensor::Cpu,
            temp_range: (35.0, 85.0),
        }
    }
}

/// One LED as the effects see it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Led {
    /// Where it is on the canvas: 0 to 1 across and down.
    pub x: f32,
    pub y: f32,
    /// Its place along its device (for chases), and how many LEDs that device has.
    pub index: usize,
    pub count: usize,
    /// A stable number per LED (for the random-looking effects).
    pub id: u32,
    /// The key it lights, as a Windows virtual-key code (keyboards).
    pub key: Option<u16>,
}

/// What's happening, for the effects that react to something.
#[derive(Default)]
pub struct Context {
    /// Seconds since the engine started.
    pub t: f32,
    /// The music's spectrum, low to high (0..1 each), and its overall level.
    pub bands: Vec<f32>,
    pub level: f32,
    /// °C.
    pub cpu_temp: f32,
    pub gpu_temp: f32,
    /// The screen, shrunk: width, height and RGB bytes, row by row.
    pub screen: Option<(usize, usize, Vec<u8>)>,
    /// Keys pressed lately: (virtual-key code, x, y on the canvas, seconds ago).
    pub keys: Vec<(u16, f32, f32, f32)>,
}

fn hash(a: u32, b: u32) -> f32 {
    let mut h = a.wrapping_mul(0x9E37_79B9) ^ b.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 13;
    (h & 0xFFFF) as f32 / 65535.0
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// A colour along a list of colours spread evenly over 0..1.
pub fn gradient(colours: &[Rgb], p: f32) -> Rgb {
    match colours.len() {
        0 => Rgb::BLACK,
        1 => colours[0],
        n => {
            let p = p.clamp(0.0, 1.0) * (n - 1) as f32;
            let i = (p.floor() as usize).min(n - 2);
            colours[i].mix(colours[i + 1], p - i as f32)
        }
    }
}

/// A colour cycling round a list (the last back to the first), 0..1 per round.
fn cycle(colours: &[Rgb], p: f32) -> Rgb {
    match colours.len() {
        0 => Rgb::BLACK,
        1 => colours[0],
        n => {
            let p = p.rem_euclid(1.0) * n as f32;
            let i = p.floor() as usize % n;
            colours[i].mix(colours[(i + 1) % n], smooth(p - p.floor()))
        }
    }
}

/// How far along the direction of travel an LED is, 0..1.
fn along(led: &Led, dir: Dir) -> f32 {
    match dir {
        Dir::Right => led.x,
        Dir::Left => 1.0 - led.x,
        Dir::Down => led.y,
        Dir::Up => 1.0 - led.y,
        Dir::Out => ((led.x - 0.5).hypot(led.y - 0.5) * std::f32::consts::SQRT_2).min(1.0),
        Dir::In => 1.0 - ((led.x - 0.5).hypot(led.y - 0.5) * std::f32::consts::SQRT_2).min(1.0),
    }
}

/// Speed as cycles per second: 0 a slow drift, 1 brisk.
fn hz(speed: f32) -> f32 {
    0.05 + speed.clamp(0.0, 1.0).powf(1.5) * 2.0
}

/// The colour of `led` under `e` at the moment `c` describes.
pub fn colour(e: &Effect, led: &Led, c: &Context) -> Rgb {
    let cols: Vec<Rgb> = e.colours.iter().map(|&h| Rgb::from_hex(h)).collect();
    let first = cols.first().copied().unwrap_or(Rgb(1.0, 1.0, 1.0));
    let second = cols.get(1).copied().unwrap_or(Rgb::BLACK);
    let f = hz(e.speed);
    let t = c.t;
    let size = e.size.clamp(0.02, 1.0);
    let p = along(led, e.dir);
    match e.kind {
        Kind::Off => Rgb::BLACK,
        Kind::Static => first,
        Kind::Gradient => gradient(&cols, p),
        Kind::Spectrum => Rgb::hue(t * f * 0.25),
        Kind::RainbowWave => Rgb::hue(p / (size * 2.0) - t * f * 0.5),
        Kind::SpiralRainbow => {
            let a = (led.y - 0.5).atan2(led.x - 0.5) / std::f32::consts::TAU;
            let sign = if e.dir == Dir::Left { -1.0 } else { 1.0 };
            Rgb::hue(a * sign + t * f * 0.3)
        }
        Kind::ColorShift => cycle(&cols, t * f * 0.25),
        Kind::ColorPulse => {
            let round = t * f * 0.5;
            let b = smooth((round * std::f32::consts::TAU).sin() * 0.5 + 0.5);
            cycle(&cols, (round + 0.25).floor() / cols.len().max(1) as f32).scale(b)
        }
        Kind::ColorWave => {
            let head = (t * f * 0.4).rem_euclid(1.0 + size) - size / 2.0;
            let d = ((p - head).abs() / (size / 2.0)).min(1.0);
            first.mix(second, smooth(d))
        }
        Kind::Visor => {
            let ph = (t * f * 0.5).rem_euclid(2.0);
            let head = if ph < 1.0 { ph } else { 2.0 - ph };
            let d = ((p - head).abs() / (size * 0.4 + 0.02)).min(1.0);
            first.scale(1.0 - smooth(d))
        }
        Kind::Rain => {
            let col = (led.x * 24.0).floor() as u32;
            let mut b: f32 = 0.0;
            for k in 0..2u32 {
                let rate = f * (0.6 + hash(col, 7 + k));
                let head = (t * rate * 0.6 + hash(col, 31 + k)).rem_euclid(1.0) * 1.4 - 0.2;
                let tail = size * 0.5 + 0.05;
                let d = head - led.y;
                if d >= 0.0 && d < tail {
                    b = b.max(1.0 - d / tail);
                }
            }
            cycle(&cols, hash(col, 3)).scale(b)
        }
        Kind::Marquee => {
            let n = led.count.max(1) as f32;
            let seg = (size * 8.0 + 1.0).round();
            let pos = (led.index as f32 + t * f * n * 0.5 * if e.dir == Dir::Left { -1.0 } else { 1.0 })
                .rem_euclid(seg * 2.0);
            if pos < seg {
                first
            } else {
                second
            }
        }
        Kind::Sequential => {
            let n = led.count.max(1) as f32;
            let step = t * f * n * 0.3;
            let round = (step / n).floor();
            let filled = step - round * n;
            let i = led.index as f32;
            let cur = cycle(&cols, round / cols.len().max(1) as f32);
            let prev = cycle(&cols, (round - 1.0) / cols.len().max(1) as f32);
            if i < filled {
                cur
            } else {
                prev
            }
        }
        Kind::Twinkle => {
            let rate = f * 0.5;
            let ph = t * rate + hash(led.id, 1) * 10.0;
            let cyc = ph.floor() as u32;
            let on = hash(led.id, cyc) < 0.15 + size * 0.5;
            let b = if on { (std::f32::consts::PI * ph.fract()).sin() } else { 0.0 };
            cycle(&cols, hash(led.id, cyc + 99)).scale(b)
        }
        Kind::Strobe => {
            if (t * f * 4.0).fract() < 0.15 {
                first
            } else {
                Rgb::BLACK
            }
        }
        Kind::Heartbeat => {
            let ph = (t * f * 0.7).fract();
            let beat = |c: f32, w: f32| (1.0 - ((ph - c).abs() / w)).max(0.0);
            first.scale(beat(0.1, 0.08).max(beat(0.3, 0.1) * 0.7))
        }
        Kind::Fire => {
            let flicker = hash(led.id, (t * 12.0 * (0.5 + e.speed)) as u32);
            let heat = ((1.0 - led.y) * 1.2 + 0.15 * (t * 3.0 + led.x * 9.0).sin() - flicker * 0.35).clamp(0.0, 1.0);
            gradient(&[Rgb::BLACK, Rgb(0.8, 0.0, 0.0), Rgb(1.0, 0.4, 0.0), Rgb(1.0, 0.85, 0.3)], heat)
        }
        Kind::Plasma => {
            let s = 4.0 + (1.0 - size) * 8.0;
            let v = (led.x * s + t * f).sin()
                + (led.y * s * 0.8 - t * f * 1.3).sin()
                + ((led.x + led.y) * s * 0.6 + t * f * 0.7).sin();
            Rgb::hue(v / 6.0 + t * f * 0.05)
        }
        Kind::Temperature => {
            let temp = if e.sensor == Sensor::Gpu { c.gpu_temp } else { c.cpu_temp };
            let (lo, hi) = e.temp_range;
            gradient(&cols, (temp - lo) / (hi - lo).max(1.0))
        }
        Kind::AudioVisualizer => {
            let n = c.bands.len();
            if n == 0 {
                return Rgb::BLACK;
            }
            let x = if e.dir == Dir::Left { 1.0 - led.x } else { led.x };
            let v = c.bands[((x * n as f32) as usize).min(n - 1)];
            let height = 1.0 - led.y;
            if height <= v {
                gradient(&cols, height)
            } else {
                Rgb::BLACK
            }
        }
        Kind::AudioPulse => {
            first.mix(second, 1.0 - c.level.clamp(0.0, 1.0)).scale(0.15 + 0.85 * c.level.clamp(0.0, 1.0))
        }
        Kind::ScreenAmbience => match &c.screen {
            Some((w, h, px)) if *w > 0 && *h > 0 => {
                let sx = ((led.x * *w as f32) as usize).min(w - 1);
                let sy = ((led.y * *h as f32) as usize).min(h - 1);
                let i = (sy * w + sx) * 3;
                Rgb(px[i] as f32 / 255.0, px[i + 1] as f32 / 255.0, px[i + 2] as f32 / 255.0)
            }
            _ => Rgb::BLACK,
        },
        Kind::TypeKey => {
            let fade = 0.2 + size * 2.0;
            let mut b: f32 = 0.0;
            for &(vk, _, _, ago) in &c.keys {
                if Some(vk) == led.key && ago < fade {
                    b = b.max(1.0 - ago / fade);
                }
            }
            second.mix(first, b)
        }
        Kind::TypeRipple => {
            let speed = 0.4 + e.speed * 1.6;
            let width = 0.03 + size * 0.1;
            let mut b: f32 = 0.0;
            for &(_, kx, ky, ago) in &c.keys {
                let radius = ago * speed;
                if radius > 1.5 {
                    continue;
                }
                let d = ((led.x - kx).hypot((led.y - ky) * 0.5) - radius).abs();
                b = b.max((1.0 - d / width).max(0.0) * (1.0 - radius / 1.5));
            }
            second.mix(first, b)
        }
    }
}

/// `top` over `bottom` with `blend`, at `opacity`.
pub fn blend(bottom: Rgb, top: Rgb, how: Blend, opacity: f32) -> Rgb {
    let mixed = match how {
        Blend::Normal => top,
        Blend::Add => Rgb((bottom.0 + top.0).min(1.0), (bottom.1 + top.1).min(1.0), (bottom.2 + top.2).min(1.0)),
        Blend::Multiply => Rgb(bottom.0 * top.0, bottom.1 * top.1, bottom.2 * top.2),
        Blend::Screen => Rgb(
            1.0 - (1.0 - bottom.0) * (1.0 - top.0),
            1.0 - (1.0 - bottom.1) * (1.0 - top.1),
            1.0 - (1.0 - bottom.2) * (1.0 - top.2),
        ),
        Blend::Lighten => Rgb(bottom.0.max(top.0), bottom.1.max(top.1), bottom.2.max(top.2)),
    };
    bottom.mix(mixed, opacity)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn led(x: f32, y: f32) -> Led {
        Led { x, y, index: 0, count: 10, id: 1, key: None }
    }

    #[test]
    fn hues_go_round() {
        assert_eq!(Rgb::hue(0.0).to_bytes(), [255, 0, 0]);
        assert_eq!(Rgb::hue(1.0 / 3.0).to_bytes(), [0, 255, 0]);
        assert_eq!(Rgb::hue(2.0 / 3.0).to_bytes(), [0, 0, 255]);
        assert_eq!(Rgb::hue(1.0).to_bytes(), [255, 0, 0]);
    }

    #[test]
    fn gradients_reach_both_ends() {
        let cols = [Rgb(1.0, 0.0, 0.0), Rgb(0.0, 0.0, 1.0)];
        assert_eq!(gradient(&cols, 0.0).to_bytes(), [255, 0, 0]);
        assert_eq!(gradient(&cols, 1.0).to_bytes(), [0, 0, 255]);
        let e = Effect { kind: Kind::Gradient, colours: vec![0xFF0000, 0x0000FF], ..Default::default() };
        let c = Context::default();
        assert_eq!(colour(&e, &led(0.0, 0.5), &c).to_bytes(), [255, 0, 0]);
        assert_eq!(colour(&e, &led(1.0, 0.5), &c).to_bytes(), [0, 0, 255]);
    }

    #[test]
    fn every_effect_stays_in_range() {
        let mut c = Context { bands: vec![0.5; 16], level: 0.7, cpu_temp: 60.0, gpu_temp: 50.0, ..Default::default() };
        c.keys.push((65, 0.3, 0.4, 0.2));
        c.screen = Some((4, 2, vec![128; 24]));
        for k in Kind::ALL {
            let e = Effect { kind: k, colours: k.default_colours(), ..Default::default() };
            for i in 0..50 {
                c.t = i as f32 * 0.37;
                let l = Led {
                    x: (i % 7) as f32 / 6.0,
                    y: (i % 5) as f32 / 4.0,
                    index: i,
                    count: 50,
                    id: i as u32,
                    key: Some(65),
                };
                let col = colour(&e, &l, &c);
                for v in [col.0, col.1, col.2] {
                    assert!((-0.001..=1.001).contains(&v) && v.is_finite(), "{k:?} gave {col:?}");
                }
            }
        }
    }

    #[test]
    fn blends() {
        let (a, b) = (Rgb(0.5, 0.5, 0.5), Rgb(0.5, 0.0, 1.0));
        assert_eq!(blend(a, b, Blend::Normal, 1.0), b);
        assert_eq!(blend(a, b, Blend::Normal, 0.0), a);
        assert_eq!(blend(a, b, Blend::Add, 1.0).to_bytes(), [255, 128, 255]);
        assert_eq!(blend(a, b, Blend::Lighten, 1.0).to_bytes(), [128, 128, 255]);
    }
}
