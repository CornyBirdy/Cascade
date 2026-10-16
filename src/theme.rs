//! Themes: one look shared by every Cascade surface (the notch's lip, the
//! taskbar, the Start menu, the panels and Alt+Tab). Picked in Settings >
//! Appearance, or `theme =` in the config.
//!
//! A theme is a material, not only a colour: which glass DWM puts behind a
//! panel (frosted acrylic, a blur, clear glass or none), how the glass is washed,
//! how its edge is drawn, and how light passes through it (`Light`): thick
//! glass bending light round its rounded edge and splitting it into colours,
//! frosted glass carrying light to its edges, Aero's gloss and streaks, a
//! polished stone's reflection, a matte surface lit from above. It costs
//! nothing at run time beyond a few gradients per panel.
//!
//! The theme's colours can be swapped for the Windows accent, a hue of your
//! own or none at all, and made stronger or weaker (`set_colour`).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicUsize, Ordering};

use windows::core::BOOL;
use windows::Win32::Graphics::Direct2D::ID2D1Brush;
use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;

use crate::backdrop::Backdrop;
use crate::gfx::{Color, Painter};
use crate::lens::Lens;

/// What DWM puts behind a panel.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Glass {
    /// Frosted acrylic, like the Windows 11 taskbar.
    Acrylic,
    /// A plain blur with no frost or noise.
    Blur,
    /// No blur at all: clear glass that what's behind shows through sharp,
    /// with only the light in and on the glass drawn over it.
    Clear,
    /// No blur: a solid panel.
    Solid,
}

/// How a panel's edge is drawn.
#[derive(Clone, Copy)]
pub enum Rim {
    None,
    /// One flat line.
    Line(Color),
    /// One line, lighter along the top than the bottom (light from above).
    Lit(Color, Color),
    /// Light catching thick glass: a bright top edge with glints on the
    /// top-left and bottom-right corners, faint down the sides (the number
    /// is its strength).
    Specular(f32),
    /// A dark outer line around a light inner one, like Aero's frames.
    Double(Color, Color),
    /// A line shading from one colour (top-left) to another (bottom-right).
    Gradient(Color, Color),
}

/// How light passes through and off a panel. Every effect is off at 0.
#[derive(Clone, Copy)]
pub struct Light {
    /// Thick glass bending light round its rounded edge: (width, strength)
    /// of a band just inside the edge where the light gathers. It is
    /// brightest at the rim, fades smoothly inwards, and is lit from above,
    /// with some of the light coming out again along the bottom.
    pub lens: (f32, f32),
    /// Light split into colours where the edge bends it: a cool fringe along
    /// the lit top and a warm one along the bottom.
    pub dispersion: f32,
    /// Light carried inside the glass and given off at its edge: (width,
    /// strength) of a soft glow, in the rim's colours with a gradient rim and
    /// white otherwise.
    pub glow: (f32, f32),
    /// A glossy reflection over the top of the panel, Aero-style.
    pub gloss: f32,
    /// Faint diagonal streaks of reflected light, as on Aero glass.
    pub streaks: f32,
    /// A soft reflection of the room on a polished surface, from the top left.
    pub sheen: f32,
    /// Lit from above, as a matte or solid surface is: a touch lighter at the
    /// top, darker at the bottom.
    pub shade: f32,
    /// The thickness of clear glass seen from the front: a soft dark band
    /// just inside the edge, heaviest along the bottom, so the pane stands
    /// out over light things behind it too.
    pub depth: f32,
}

const DARK: Light = Light {
    lens: (0.0, 0.0),
    dispersion: 0.0,
    glow: (0.0, 0.0),
    gloss: 0.0,
    streaks: 0.0,
    sheen: 0.0,
    shade: 0.0,
    depth: 0.0,
};

#[derive(Clone, Copy)]
pub struct Theme {
    /// Config value.
    pub key: &'static str,
    pub name: &'static str,
    pub about: &'static str,
    pub glass: Glass,
    /// Colour the glass is tinted with; the Tint sliders set how strongly.
    pub tint: (f32, f32, f32),
    /// Scales the Tint sliders, so clear-glass themes stay clear.
    pub tint_scale: f32,
    /// Panel colour without blur.
    pub solid: (f32, f32, f32),
    /// A wash over the panel, top and bottom: the haze of the glass itself
    /// (it also keeps the pixels over the blur clickable, so neither end may
    /// be fully clear).
    pub film: (Color, Color),
    pub rim: Rim,
    pub light: Light,
    /// Clear glass that bends what's behind it round its edge (see `lens`),
    /// with Refraction on. It then needs no drawn edge: the bending shows it.
    pub refract: Option<Lens>,
    /// Highlights: the app in front, toggles, progress, today.
    pub accent: Color,
}

const WIN_BLUE: Color = Color(0.38, 0.67, 1.0, 1.0);
const LOVE: Color = Color(0.922, 0.435, 0.573, 1.0);

const fn white(a: f32) -> Color {
    Color(1.0, 1.0, 1.0, a)
}

const fn black(a: f32) -> Color {
    Color(0.0, 0.0, 0.0, a)
}

pub const THEMES: [Theme; 10] = [
    Theme {
        key: "acrylic",
        name: "Acrylic",
        about: "Frosted dark glass with a fine edge, lit along the top, like Windows 11. The default.",
        glass: Glass::Acrylic,
        tint: (0.110, 0.110, 0.118),
        tint_scale: 1.0,
        solid: (0.13, 0.13, 0.14),
        film: (white(0.015), white(0.015)),
        rim: Rim::Lit(white(0.14), white(0.06)),
        light: DARK,
        refract: None,
        accent: WIN_BLUE,
    },
    Theme {
        key: "liquid_glass",
        name: "Liquid Glass",
        about:
            "Apple-style clear glass you see through: light gathers in its thick edge, splits into colour and glints.",
        glass: Glass::Clear,
        tint: (0.10, 0.11, 0.14),
        tint_scale: 0.25,
        solid: (0.17, 0.18, 0.21),
        film: (white(0.05), white(0.02)),
        // A soft edge: only drawn with Refraction off (the bent light shows
        // the edge otherwise), and then a hint, not a white outline.
        rim: Rim::Specular(0.26),
        light: Light { lens: (6.0, 0.06), dispersion: 0.07, ..DARK },
        refract: Some(Lens { band: 18.0, bend: 0.75, split: 0.12 }),
        accent: Color(0.04, 0.52, 1.0, 1.0),
    },
    Theme {
        key: "windows_glass",
        name: "Windows Glass",
        about:
            "Aero, as in Windows 7: coloured glass with a glossy top, faint light streaks and a dark-and-light edge.",
        glass: Glass::Blur,
        tint: (0.10, 0.28, 0.48),
        tint_scale: 0.85,
        solid: (0.10, 0.22, 0.36),
        film: (white(0.06), white(0.01)),
        rim: Rim::Double(black(0.55), white(0.40)),
        light: Light { lens: (5.0, 0.08), gloss: 0.20, streaks: 0.05, ..DARK },
        refract: None,
        accent: Color(0.45, 0.78, 1.0, 1.0),
    },
    Theme {
        key: "frosted",
        name: "Frosted Glass",
        about: "Milky, sand-blasted glass that carries light to its edges, where it glows.",
        glass: Glass::Acrylic,
        tint: (0.30, 0.31, 0.34),
        tint_scale: 1.0,
        solid: (0.25, 0.26, 0.28),
        film: (white(0.11), white(0.06)),
        rim: Rim::Line(white(0.16)),
        light: Light { glow: (10.0, 0.09), ..DARK },
        refract: None,
        accent: Color(0.42, 0.70, 1.0, 1.0),
    },
    Theme {
        key: "tinted",
        name: "Tinted Glass",
        about: "Acrylic in your Windows accent colour, edged and softly lit in a lighter shade of it.",
        glass: Glass::Acrylic,
        // Filled in from the accent colour by `resolve`.
        tint: (0.11, 0.11, 0.12),
        tint_scale: 1.0,
        solid: (0.13, 0.13, 0.14),
        film: (white(0.05), white(0.01)),
        rim: Rim::Gradient(white(0.3), white(0.08)),
        light: Light { glow: (6.0, 0.06), ..DARK },
        refract: None,
        accent: WIN_BLUE,
    },
    Theme {
        key: "translucent",
        name: "Translucent",
        about: "Barely-there clear glass: no blur, no tint, no outline, only a faint bend of light at its edge.",
        glass: Glass::Clear,
        tint: (0.05, 0.05, 0.06),
        tint_scale: 0.12,
        solid: (0.12, 0.12, 0.13),
        film: (white(0.012), white(0.012)),
        rim: Rim::None,
        light: Light { lens: (5.0, 0.05), ..DARK },
        refract: Some(Lens { band: 10.0, bend: 0.5, split: 0.06 }),
        accent: WIN_BLUE,
    },
    Theme {
        key: "rose_pine",
        name: "Ros\u{e9} Pine",
        about: "Deep purple glass edged and lit from iris to love, from the Ros\u{e9} Pine palette.",
        glass: Glass::Acrylic,
        tint: (0.098, 0.090, 0.141),
        tint_scale: 1.0,
        solid: (0.122, 0.114, 0.180),
        film: (Color(0.77, 0.65, 0.91, 0.05), Color(0.92, 0.44, 0.57, 0.025)),
        rim: Rim::Gradient(Color(0.769, 0.655, 0.906, 0.5), Color(0.922, 0.435, 0.573, 0.4)),
        light: Light { glow: (8.0, 0.07), ..DARK },
        refract: None,
        accent: LOVE,
    },
    Theme {
        key: "neon",
        name: "Neon",
        about: "Near-black glass whose cyan-to-magenta edge gives off light, glowing into the glass.",
        glass: Glass::Acrylic,
        tint: (0.02, 0.02, 0.04),
        tint_scale: 1.4,
        solid: (0.03, 0.03, 0.05),
        film: (Color(0.0, 0.85, 1.0, 0.025), Color(1.0, 0.2, 0.8, 0.025)),
        rim: Rim::Gradient(Color(0.0, 0.9, 1.0, 0.95), Color(1.0, 0.25, 0.85, 0.95)),
        light: Light { glow: (16.0, 0.30), ..DARK },
        refract: None,
        accent: Color(0.10, 0.70, 0.95, 1.0),
    },
    Theme {
        key: "obsidian",
        name: "Obsidian",
        about: "Solid black, polished: it reflects the room, with a bevelled, lit edge. No blur.",
        glass: Glass::Solid,
        tint: (0.035, 0.035, 0.04),
        tint_scale: 1.0,
        solid: (0.035, 0.035, 0.04),
        film: (white(0.02), white(0.0)),
        rim: Rim::Specular(0.35),
        light: Light { lens: (3.0, 0.07), sheen: 0.10, ..DARK },
        refract: None,
        accent: Color(0.98, 0.55, 0.20, 1.0),
    },
    Theme {
        key: "clean_slate",
        name: "Clean Slate",
        about: "Matte, flat slate grey, softly lit from above. No blur, no shine.",
        glass: Glass::Solid,
        tint: (0.12, 0.125, 0.14),
        tint_scale: 1.0,
        solid: (0.12, 0.125, 0.14),
        film: (white(0.0), white(0.0)),
        rim: Rim::Line(white(0.05)),
        light: Light { shade: 0.12, ..DARK },
        refract: None,
        accent: Color(0.36, 0.52, 0.74, 1.0),
    },
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);
/// The Windows accent colour (0xRRGGBB), for Tinted Glass.
static SYSTEM_ACCENT: AtomicU32 = AtomicU32::new(0x0078D4);

pub fn is_known(key: &str) -> bool {
    THEMES.iter().any(|t| t.key == key)
}

/// Switches to the theme named `key` (unknown names get the default).
pub fn set(key: &str) {
    CURRENT.store(THEMES.iter().position(|t| t.key == key).unwrap_or(0), Ordering::Relaxed);
    refresh_accent();
}

/// Re-reads the Windows accent colour (it can change while Cascade runs).
pub fn refresh_accent() {
    let mut c = 0u32;
    let mut opaque = BOOL(0);
    if unsafe { DwmGetColorizationColor(&mut c, &mut opaque) }.is_ok() {
        SYSTEM_ACCENT.store(c & 0x00FF_FFFF, Ordering::Relaxed);
    }
}

// ------------------------------------------------------------ colour --

/// What a theme's colours are based on (`theme_color` in the config).
pub const SOURCES: [&str; 4] = ["theme", "accent", "neutral", "custom"];

static SOURCE: AtomicU8 = AtomicU8::new(0);
/// `theme_hue` in degrees, as f32 bits.
static HUE: AtomicU32 = AtomicU32::new(0x4352_0000); // 210.0
/// `theme_intensity` (1 = the theme as designed), as f32 bits.
static INTENSITY: AtomicU32 = AtomicU32::new(0x3F80_0000); // 1.0

/// `theme_edge` (1 = as designed), as f32 bits.
static EDGE: AtomicU32 = AtomicU32::new(0x3F80_0000); // 1.0

/// How strongly every theme's edge catches the light: its outline and the
/// light gathered and glowing along it (0 = none, 1 = as designed, 2 =
/// twice as strong).
pub fn set_edge(k: f32) {
    EDGE.store(k.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
}

/// `t` with its edge scaled by the Edge setting.
fn edged(mut t: Theme) -> Theme {
    let k = f32::from_bits(EDGE.load(Ordering::Relaxed));
    if (k - 1.0).abs() < 1e-3 {
        return t;
    }
    let c = |c: Color| Color(c.0, c.1, c.2, (c.3 * k).min(1.0));
    t.rim = match t.rim {
        Rim::None => Rim::None,
        Rim::Line(a) => Rim::Line(c(a)),
        Rim::Lit(a, b) => Rim::Lit(c(a), c(b)),
        Rim::Specular(s) => Rim::Specular(s * k),
        Rim::Double(a, b) => Rim::Double(c(a), c(b)),
        Rim::Gradient(a, b) => Rim::Gradient(c(a), c(b)),
    };
    let l = &mut t.light;
    l.lens.1 *= k;
    l.dispersion *= k;
    l.glow.1 *= k;
    l.depth *= k;
    t
}

/// Sets what every theme's colours are based on: `source` is one of
/// `SOURCES` (the theme's own colours, the Windows accent, none, or `hue`),
/// and `intensity` how colourful they are (0 = grey, 1 = as designed, up to
/// 2 = twice as colourful).
pub fn set_colour(source: &str, hue: f32, intensity: f32) {
    SOURCE.store(SOURCES.iter().position(|s| *s == source).unwrap_or(0) as u8, Ordering::Relaxed);
    HUE.store(hue.rem_euclid(360.0).to_bits(), Ordering::Relaxed);
    INTENSITY.store(intensity.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
}

/// Whether the look follows the Windows accent colour (so it has to be
/// redrawn when that changes).
pub fn follows_accent(theme: &str) -> bool {
    match SOURCE.load(Ordering::Relaxed) {
        1 => true,
        0 => theme == "tinted",
        _ => false,
    }
}

/// Below this saturation a colour counts as grey: it has no hue to turn.
const GREY: f32 = 0.1;
/// How colourful a theme's greys get when they take on a hue.
const GREY_TINT: f32 = 0.5;

/// (hue in degrees, saturation, lightness).
fn to_hsl((r, g, b): (f32, f32, f32)) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-5 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-5);
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s.min(1.0), l)
}

fn from_hsl(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    ((r + m).clamp(0.0, 1.0), (g + m).clamp(0.0, 1.0), (b + m).clamp(0.0, 1.0))
}

/// How a theme's colours are turned to the chosen colour.
#[derive(Clone, Copy)]
enum Turn {
    /// Its own colours.
    Own,
    /// No colour at all.
    Grey,
    /// Every hue turned by `by` degrees (so a theme's own shades keep
    /// their places relative to each other), greys given hue `to`, and
    /// colourfulness scaled by `sat`.
    Hue { to: f32, by: f32, sat: f32 },
}

#[derive(Clone, Copy)]
struct Recolour {
    turn: Turn,
    intensity: f32,
}

impl Recolour {
    fn rgb(self, c: (f32, f32, f32)) -> (f32, f32, f32) {
        if matches!(self.turn, Turn::Own) && (self.intensity - 1.0).abs() < 1e-3 {
            return c;
        }
        let (h, s, l) = to_hsl(c);
        let (h, s) = match self.turn {
            Turn::Own => (h, s),
            Turn::Grey => (h, 0.0),
            Turn::Hue { to, sat, .. } if s < GREY => (to, GREY_TINT * sat),
            Turn::Hue { by, sat, .. } => (h + by, s * sat),
        };
        from_hsl(h, (s * self.intensity).clamp(0.0, 1.0), l)
    }

    fn color(self, c: Color) -> Color {
        let (r, g, b) = self.rgb((c.0, c.1, c.2));
        Color(r, g, b, c.3)
    }

    fn rim(self, rim: Rim) -> Rim {
        match rim {
            Rim::Line(c) => Rim::Line(self.color(c)),
            Rim::Lit(a, b) => Rim::Lit(self.color(a), self.color(b)),
            Rim::Double(a, b) => Rim::Double(self.color(a), self.color(b)),
            Rim::Gradient(a, b) => Rim::Gradient(self.color(a), self.color(b)),
            r @ (Rim::None | Rim::Specular(_)) => r,
        }
    }
}

/// The colour settings, as they apply to `t` (whose main hue is its accent's).
fn recolour(t: &Theme) -> Recolour {
    let intensity = f32::from_bits(INTENSITY.load(Ordering::Relaxed));
    let (own, own_s, _) = to_hsl((t.accent.0, t.accent.1, t.accent.2));
    let own = if own_s < GREY { 0.0 } else { own };
    let turn = match SOURCE.load(Ordering::Relaxed) {
        1 => {
            let (h, s, _) = to_hsl(unpack(SYSTEM_ACCENT.load(Ordering::Relaxed)));
            if s < GREY {
                Turn::Grey
            } else {
                Turn::Hue { to: h, by: h - own, sat: s }
            }
        }
        2 => Turn::Grey,
        3 => {
            let h = f32::from_bits(HUE.load(Ordering::Relaxed));
            Turn::Hue { to: h, by: h - own, sat: 1.0 }
        }
        _ => Turn::Own,
    };
    Recolour { turn, intensity }
}

fn unpack(c: u32) -> (f32, f32, f32) {
    let f = |s: u32| ((c >> s) & 0xFF) as f32 / 255.0;
    (f(16), f(8), f(0))
}

/// 0xAARRGGBB from a colour and an alpha.
pub fn pack((r, g, b): (f32, f32, f32), a: f32) -> u32 {
    let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (u(a) << 24) | (u(r) << 16) | (u(g) << 8) | u(b)
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// `c` moved `t` of the way towards white.
pub fn lighten(c: Color, t: f32) -> Color {
    Color(mix(c.0, 1.0, t), mix(c.1, 1.0, t), mix(c.2, 1.0, t), c.3)
}

pub fn current() -> Theme {
    let mut t = resolve(THEMES[CURRENT.load(Ordering::Relaxed).min(THEMES.len() - 1)]);
    if lens_of(&t).is_some() {
        // The bent light shows the edge; a drawn one would only fight it.
        t.rim = Rim::None;
        t.light.lens = (0.0, 0.0);
        t.light.dispersion = 0.0;
    }
    t
}

static REFRACT: AtomicBool = AtomicBool::new(true);

/// Turns refraction for clear glass on or off (`refraction` in the config).
pub fn set_refraction(on: bool) {
    REFRACT.store(on, Ordering::Relaxed);
}

/// How `t` bends light, if it does and Refraction is on.
fn lens_of(t: &Theme) -> Option<Lens> {
    let k = f32::from_bits(EDGE.load(Ordering::Relaxed));
    let lens = t.refract.filter(|_| REFRACT.load(Ordering::Relaxed) && t.glass == Glass::Clear)?;
    // The Edge slider sets how strongly it bends.
    (k > 0.0).then_some(Lens { bend: lens.bend * k.min(1.5), split: lens.split * k, ..lens })
}

/// Whether panels in the current theme bend light (and so need their
/// rings refreshed as what's behind them changes). `acrylic` is the Blur
/// setting.
pub fn refracts(acrylic: bool) -> bool {
    blurs(acrylic) && lens_of(&current()).is_some()
}

/// `t` with the colours it takes from Windows filled in, then turned to the
/// colour settings.
pub fn resolve(mut t: Theme) -> Theme {
    if t.key == "tinted" {
        let (r, g, b) = unpack(SYSTEM_ACCENT.load(Ordering::Relaxed));
        // Darkened behind the glass so white text stays readable.
        t.tint = (r * 0.4, g * 0.4, b * 0.4);
        t.solid = (r * 0.3 + 0.04, g * 0.3 + 0.04, b * 0.3 + 0.04);
        t.film = (Color(r, g, b, 0.08), Color(r, g, b, 0.01));
        t.rim = Rim::Gradient(
            Color(mix(r, 1.0, 0.6), mix(g, 1.0, 0.6), mix(b, 1.0, 0.6), 0.4),
            Color(mix(r, 1.0, 0.2), mix(g, 1.0, 0.2), mix(b, 1.0, 0.2), 0.12),
        );
        t.accent = Color(mix(r, 1.0, 0.3), mix(g, 1.0, 0.3), mix(b, 1.0, 0.3), 1.0);
    }
    let c = recolour(&t);
    t.tint = c.rgb(t.tint);
    t.solid = c.rgb(t.solid);
    t.film = (c.color(t.film.0), c.color(t.film.1));
    t.rim = c.rim(t.rim);
    t.accent = c.color(t.accent);
    edged(t)
}

pub fn accent() -> Color {
    current().accent
}

/// The accent under the pointer.
pub fn accent_hover() -> Color {
    lighten(accent(), 0.15)
}

/// How far a panel fading in (or out) has to be shown for its glass to be
/// on. Below it the panel is too faint for the blur to look right, so it is
/// drawn solid; above it the blur follows the panel as it slides.
pub const GLASS_FROM: f32 = 0.3;

/// Whether panels blur what's behind them (`acrylic` is the Blur setting).
pub fn blurs(acrylic: bool) -> bool {
    acrylic && current().glass != Glass::Solid
}

/// Sets up a panel's blur window for the current theme, with the Tint
/// slider at `opacity`. Returns whether the panel blurs (false with Blur
/// turned off, or a theme without glass).
pub fn style_backdrop(b: &Backdrop, acrylic: bool, opacity: f32, radius: f32) -> bool {
    let t = current();
    let blur = blurs(acrylic);
    // Acrylic takes its tint from DWM. A clear blur, or clear glass, gets
    // none there (it is painted in `panel` instead, so it looks the same on
    // every Windows build).
    let tint = match t.glass {
        Glass::Acrylic => pack(t.tint, (opacity * t.tint_scale).min(1.0)),
        _ => pack(t.tint, 0.0),
    };
    b.set_lens(lens_of(&t).filter(|_| blur));
    b.style(if blur { t.glass } else { Glass::Solid }, tint, radius);
    blur
}

/// An opaque backdrop in the theme's panel colour (for Alt+Tab, whose
/// backdrop also hosts the live previews, so it must always be there).
pub fn style_opaque(b: &Backdrop, radius: f32) {
    b.set_lens(None);
    b.style(Glass::Acrylic, pack(current().solid, 1.0), radius);
}

/// Paints a panel in the current theme: over a live blur (`blurred`) the
/// theme's wash, otherwise a solid panel at `solid_alpha`; then the light
/// in and on it, and its edge. `alpha` fades all of it.
///
/// It's painted once into a bitmap and drawn from that while it stays the
/// same (see `Painter::cached`): the gradients and edge rings cost nothing
/// on frames where only what's on the panel moves.
pub fn panel(
    p: &Painter,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    blurred: bool,
    opacity: f32,
    solid_alpha: f32,
    alpha: f32,
) {
    use std::hash::{Hash, Hasher};
    let t = current();
    let mut key = std::collections::hash_map::DefaultHasher::new();
    (look(), w.to_bits(), h.to_bits(), r.to_bits(), blurred, opacity.to_bits(), solid_alpha.to_bits()).hash(&mut key);
    p.cached(key.finish(), x, y, w, h, alpha, |p, x, y, a| {
        paint(p, &t, x, y, w, h, r, blurred, opacity, solid_alpha, a)
    });
}

/// Everything the current look depends on (the theme, its colour and edge
/// settings, refraction and the Windows accent), for telling cached panels
/// apart.
fn look() -> u64 {
    let parts = [
        CURRENT.load(Ordering::Relaxed) as u64,
        SYSTEM_ACCENT.load(Ordering::Relaxed) as u64,
        SOURCE.load(Ordering::Relaxed) as u64,
        HUE.load(Ordering::Relaxed) as u64,
        INTENSITY.load(Ordering::Relaxed) as u64,
        EDGE.load(Ordering::Relaxed) as u64,
        REFRACT.load(Ordering::Relaxed) as u64,
    ];
    parts.iter().fold(0xcbf2_9ce4_8422_2325, |h, &v| (h ^ v).wrapping_mul(0x0100_0000_01b3))
}

/// `panel` in theme `t`.
pub fn paint(
    p: &Painter,
    t: &Theme,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    blurred: bool,
    opacity: f32,
    solid_alpha: f32,
    alpha: f32,
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    // The body: what the glass does to the light coming through it.
    if blurred {
        if matches!(t.glass, Glass::Blur | Glass::Clear) {
            let (cr, cg, cb) = t.tint;
            p.rrect(x, y, w, h, r, Color(cr, cg, cb, (opacity * t.tint_scale).clamp(0.0, 1.0) * alpha));
        }
    } else {
        let (cr, cg, cb) = t.solid;
        p.rrect(x, y, w, h, r, Color(cr, cg, cb, solid_alpha * alpha));
    }
    let (top, bottom) = t.film;
    if top.3 > 0.0 || bottom.3 > 0.0 {
        p.gradient_rrect(x, y, w, h, r, &[(0.0, top.alpha(alpha)), (1.0, bottom.alpha(alpha))]);
    }

    // Light on the surface.
    let l = t.light;
    if l.shade > 0.0 && h > 4.0 {
        let s = l.shade * alpha;
        p.gradient_rrect(x, y, w, h, r, &[(0.0, white(s * 0.25)), (0.45, white(0.0)), (1.0, black(s))]);
    }
    if l.sheen > 0.0 && h > 4.0 {
        sheen(p, x, y, w, h, r, l.sheen * alpha);
    }
    if l.streaks > 0.0 && h > 4.0 {
        streaks(p, x, y, w, h, r, l.streaks * alpha);
    }
    if l.gloss > 0.0 && h > 4.0 {
        // A reflection with a crisp lower edge over the top of the panel
        // (about half of a bar, a strip across the top of a tall panel).
        let band = (h * 0.5).min(42.0) / h;
        let g = l.gloss * alpha;
        p.gradient_rrect(
            x,
            y,
            w,
            h,
            r,
            &[(0.0, white(g)), (band * 0.92, white(g * 0.35)), (band, white(0.0)), (1.0, white(0.0))],
        );
    }

    // Light in the glass, gathered at its edge.
    if l.glow.1 > 0.0 && h > 6.0 {
        let brush = match t.rim {
            Rim::Gradient(a, b) => {
                p.linear(x, y, x + w, y + h, &[(0.0, Color(a.0, a.1, a.2, 1.0)), (1.0, Color(b.0, b.1, b.2, 1.0))])
            }
            _ => p.linear(x, y, x, y + h, &[(0.0, white(1.0)), (1.0, white(0.7))]),
        };
        if let Some(b) = brush {
            edge_band(p, x, y, w, h, r, l.glow.0, l.glow.1 * alpha, &b);
        }
    }
    if l.depth > 0.0 && h > 6.0 {
        if let Some(b) = lit(p, x, y, w, h, &[(0.0, black(0.35)), (0.6, black(0.55)), (1.0, black(1.0))]) {
            edge_band(p, x, y, w, h, r, 7.0, l.depth * alpha, &b);
        }
    }
    if l.lens.1 > 0.0 && h > 6.0 {
        if let Some(b) =
            lit(p, x, y, w, h, &[(0.0, white(1.0)), (0.3, white(0.35)), (0.7, white(0.18)), (1.0, white(0.6))])
        {
            edge_band(p, x, y, w, h, r, l.lens.0, l.lens.1 * alpha, &b);
        }
    }
    if l.dispersion > 0.0 && h > 8.0 {
        dispersion(p, x, y, w, h, r, l.dispersion * alpha);
    }

    let line = p.px(1.0);
    let (lx, ly, lw, lh) = (x + line / 2.0, y + line / 2.0, w - line, h - line);
    match t.rim {
        Rim::None => {}
        Rim::Line(c) => p.stroke_rrect(lx, ly, lw, lh, r, c.alpha(alpha), line),
        Rim::Lit(a, b) => {
            if let Some(brush) = p.linear(x, y, x, y + h, &[(0.0, a.alpha(alpha)), (1.0, b.alpha(alpha))]) {
                p.stroke_rrect_with(lx, ly, lw, lh, r, &brush, line);
            }
        }
        Rim::Double(outer, inner) => {
            p.stroke_rrect(lx, ly, lw, lh, r, outer.alpha(alpha), line);
            let i = line;
            p.stroke_rrect(lx + i, ly + i, lw - 2.0 * i, lh - 2.0 * i, (r - i).max(0.0), inner.alpha(alpha), line);
        }
        Rim::Gradient(a, b) => {
            if let Some(brush) = p.linear(x, y, x + w, y + h, &[(0.0, a.alpha(alpha)), (1.0, b.alpha(alpha))]) {
                p.stroke_rrect_with(lx, ly, lw, lh, r, &brush, line);
            }
        }
        Rim::Specular(s) => specular(p, x, y, w, h, r, s * alpha),
    }
}

/// A gradient lit from above (a little from the left): `stops` run from the
/// top of the panel to its bottom, the same on a wide bar and a tall panel.
fn lit(p: &Painter, x: f32, y: f32, w: f32, h: f32, stops: &[(f32, Color)]) -> Option<ID2D1Brush> {
    let tilt = h * 0.25;
    p.linear(x + w / 2.0 - tilt, y, x + w / 2.0 + tilt, y + h, stops)
}

/// Light in a band `width` wide just inside the edge, at `strength`,
/// brightest at the rim and fading smoothly inwards (`brush` gives its
/// colours across the panel). Drawn as rings a pixel apart, each fainter
/// than the one outside it, so the light falls off evenly instead of
/// reading as a border.
fn edge_band(p: &Painter, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, strength: f32, brush: &ID2D1Brush) {
    let step = p.px(1.0);
    let rings = width / step;
    // Small panels (the lip) only have room for the outer part.
    let room = ((w.min(h) / 2.0 - step) / step).floor().max(0.0);
    let n = rings.min(room).min(32.0) as usize;
    for i in 0..n {
        let u = (i as f32 + 0.5) / rings;
        let a = strength * (1.0 - u).max(0.0).powf(2.4);
        if a < 0.002 {
            break;
        }
        let inset = step * (i as f32 + 0.5);
        p.fade(brush, a);
        p.stroke_rrect_with(x + inset, y + inset, w - 2.0 * inset, h - 2.0 * inset, (r - inset).max(0.0), brush, step);
    }
    p.fade(brush, 1.0);
}

/// The colours split off where thick glass bends light: a cool fringe just
/// inside the lit top edge and a warm one along the bottom.
fn dispersion(p: &Painter, x: f32, y: f32, w: f32, h: f32, r: f32, s: f32) {
    let step = p.px(1.0);
    let cool = Color(0.55, 0.82, 1.0, 1.0);
    let warm = Color(1.0, 0.70, 0.42, 1.0);
    let fringes = [
        (2.5, [(0.0, cool.alpha(s)), (0.35, cool.alpha(s * 0.15)), (1.0, cool.alpha(0.0))]),
        (3.5, [(0.0, warm.alpha(0.0)), (0.65, warm.alpha(s * 0.15)), (1.0, warm.alpha(s * 0.8))]),
    ];
    for (k, stops) in fringes {
        let inset = step * k;
        if w <= 2.0 * inset || h <= 2.0 * inset {
            break;
        }
        if let Some(b) = lit(p, x, y, w, h, &stops) {
            p.stroke_rrect_with(x + inset, y + inset, w - 2.0 * inset, h - 2.0 * inset, (r - inset).max(0.0), &b, step);
        }
    }
}

/// Faint diagonal streaks of light across the glass, leaning like Aero's,
/// of uneven width and spacing so they read as reflections, not stripes.
fn streaks(p: &Painter, x: f32, y: f32, w: f32, h: f32, r: f32, s: f32) {
    // The gradient runs down to the right at 30 degrees; the streaks cross it.
    let (ax, ay) = (0.866, 0.5);
    let len = w * ax + h * ay;
    if len < 1.0 {
        return;
    }
    // (gap before, width, strength), repeated along the panel.
    const PATTERN: [(f32, f32, f32); 6] = [
        (70.0, 26.0, 1.0),
        (22.0, 8.0, 0.55),
        (120.0, 40.0, 0.8),
        (46.0, 12.0, 0.4),
        (90.0, 30.0, 0.9),
        (34.0, 10.0, 0.5),
    ];
    let mut stops = vec![(0.0, white(0.0))];
    let mut at = 0.0;
    for &(gap, width, a) in PATTERN.iter().cycle().take(60) {
        let start = at + gap;
        let end = start + width;
        if end >= len {
            break;
        }
        stops.push((start / len, white(0.0)));
        stops.push(((start + width / 2.0) / len, white(a * s)));
        stops.push((end / len, white(0.0)));
        at = end;
    }
    stops.push((1.0, white(0.0)));
    if let Some(b) = p.linear(x, y, x + len * ax, y + len * ay, &stops) {
        p.fill_rrect_with(x, y, w, h, r, &b);
    }
}

/// A polished surface reflecting the room: a broad soft highlight from the
/// top left.
fn sheen(p: &Painter, x: f32, y: f32, w: f32, h: f32, r: f32, s: f32) {
    if let Some(b) = p.elliptic(
        x + w * 0.22,
        y - h * 0.15,
        w * 0.55,
        h * 0.95,
        &[(0.0, white(s)), (0.55, white(s * 0.35)), (1.0, white(0.0))],
    ) {
        p.fill_rrect_with(x, y, w, h, r, &b);
    }
}

/// The lit rim of thick glass: a faint line all round, brighter along the
/// top and a little along the bottom, with glints on the top-left and
/// bottom-right corners.
fn specular(p: &Painter, x: f32, y: f32, w: f32, h: f32, r: f32, s: f32) {
    let line = p.px(1.0);
    let (lx, ly, lw, lh) = (x + line / 2.0, y + line / 2.0, w - line, h - line);
    let edge = (28.0 / h).min(0.45);
    if let Some(b) = p.linear(
        x,
        y,
        x,
        y + h,
        &[(0.0, white(0.50 * s)), (edge, white(0.16 * s)), (1.0 - edge, white(0.12 * s)), (1.0, white(0.30 * s))],
    ) {
        p.stroke_rrect_with(lx, ly, lw, lh, r, &b, line);
    }
    // Glints, a little wider than the line so they read as light.
    let reach = w.min(h * 3.0).clamp(24.0, 220.0);
    let glint = p.px(1.5);
    let (gx, gy, gw, gh) = (x + glint / 2.0, y + glint / 2.0, w - glint, h - glint);
    for (cx, cy, a) in [(x + r * 0.3, y + r * 0.3, 0.85), (x + w - r * 0.3, y + h - r * 0.3, 0.5)] {
        if let Some(b) = p.radial(cx, cy, reach, &[(0.0, white(a * s)), (0.45, white(a * s * 0.3)), (1.0, white(0.0))])
        {
            p.stroke_rrect_with(gx, gy, gw, gh, r, &b, glint);
        }
    }
}

/// A small sample of theme `t` for the settings page: the panel over a
/// colourful wallpaper, so clear and frosted glass look different.
pub fn preview(p: &Painter, t: &Theme, x: f32, y: f32, w: f32, h: f32, r: f32) {
    // The "wallpaper", seen through the glass.
    if let Some(b) = p.linear(
        x,
        y,
        x + w,
        y + h,
        &[(0.0, Color(1.0, 0.42, 0.24, 1.0)), (0.5, Color(0.98, 0.31, 0.64, 1.0)), (1.0, Color(0.48, 0.24, 1.0, 1.0))],
    ) {
        p.fill_rrect_with(x, y, w, h, r, &b);
    }
    let opacity = 0.55;
    match t.glass {
        // Frost hides the wallpaper; clear glass mostly shows it.
        Glass::Acrylic => {
            let (cr, cg, cb) = t.tint;
            p.rrect(x, y, w, h, r, Color(cr, cg, cb, (0.55 + opacity * t.tint_scale * 0.4).min(0.95)));
            p.rrect(x, y, w, h, r, white(0.04));
        }
        Glass::Blur => {
            let (cr, cg, cb) = t.tint;
            p.rrect(x, y, w, h, r, Color(cr, cg, cb, 0.25 + opacity * t.tint_scale * 0.5));
        }
        // Clear glass: the wallpaper as it is; `paint` adds the tint.
        Glass::Clear => {}
        Glass::Solid => {}
    }
    paint(p, t, x, y, w, h, r, t.glass != Glass::Solid, opacity, 1.0, 1.0);
    p.rrect(x + 12.0, y + h / 2.0 - 2.0, 30.0, 4.0, 2.0, t.accent);
    p.circle(x + w - 16.0, y + h / 2.0, 4.0, white(0.85));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_default_first() {
        assert_eq!(THEMES[0].key, "acrylic");
        for (i, a) in THEMES.iter().enumerate() {
            assert!(THEMES.iter().skip(i + 1).all(|b| b.key != a.key), "duplicate theme {}", a.key);
        }
    }

    #[test]
    fn glass_stays_clickable() {
        // Over a blur, the film is all that keeps the panel's pixels from
        // being fully clear (and so clicking through).
        for t in THEMES.iter().filter(|t| t.glass != Glass::Solid) {
            assert!(t.film.0 .3 > 0.0 && t.film.1 .3 > 0.0, "{} has a clear film", t.key);
        }
    }

    #[test]
    fn pack_round_trips() {
        assert_eq!(pack((1.0, 0.0, 0.0), 1.0), 0xFFFF_0000);
        assert_eq!(pack((0.0, 0.0, 1.0), 0.0), 0x0000_00FF);
        let (r, g, b) = unpack(0x00_80_40_20);
        assert_eq!(pack((r, g, b), 0.0), 0x0080_4020);
    }

    #[test]
    fn hsl_round_trips() {
        for c in [(0.38, 0.67, 1.0), (0.922, 0.435, 0.573), (0.11, 0.11, 0.118), (1.0, 1.0, 1.0), (0.0, 0.9, 1.0)] {
            let (h, s, l) = to_hsl(c);
            let back = from_hsl(h, s, l);
            assert!((back.0 - c.0).abs() < 1e-3 && (back.1 - c.1).abs() < 1e-3 && (back.2 - c.2).abs() < 1e-3);
        }
    }

    #[test]
    fn recolouring_keeps_lightness_and_white() {
        let turn = Recolour { turn: Turn::Hue { to: 0.0, by: 150.0, sat: 1.0 }, intensity: 1.0 };
        // Windows Glass's blue tint turned red keeps how dark it is.
        let (r, g, b) = turn.rgb((0.10, 0.28, 0.48));
        assert!(r > g && r > b);
        assert!((to_hsl((r, g, b)).2 - to_hsl((0.10, 0.28, 0.48)).2).abs() < 1e-3);
        // White highlights stay white.
        let w = turn.rgb((1.0, 1.0, 1.0));
        assert!(w.0 > 0.999 && w.1 > 0.999 && w.2 > 0.999);
        // No colour: grey.
        let grey = Recolour { turn: Turn::Grey, intensity: 1.0 }.rgb((0.10, 0.28, 0.48));
        assert!((grey.0 - grey.1).abs() < 1e-3 && (grey.1 - grey.2).abs() < 1e-3);
        // Intensity 0 is grey too; the default leaves colours alone.
        let flat = Recolour { turn: Turn::Own, intensity: 0.0 }.rgb((0.38, 0.67, 1.0));
        assert!((flat.0 - flat.2).abs() < 1e-3);
        assert_eq!(Recolour { turn: Turn::Own, intensity: 1.0 }.rgb((0.38, 0.67, 1.0)), (0.38, 0.67, 1.0));
    }
}
