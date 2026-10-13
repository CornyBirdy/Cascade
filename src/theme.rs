//! Themes: one look shared by every Cascade surface (the notch's lip, the
//! taskbar, the Start menu, the panels and Alt+Tab). Picked in Settings >
//! Appearance, or `theme =` in the config.
//!
//! A theme only chooses colours and which kind of glass DWM puts behind a
//! panel, so switching costs nothing at run time.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use windows::core::BOOL;
use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;

use crate::backdrop::Backdrop;
use crate::gfx::{Color, Painter};

/// What DWM puts behind a panel.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Glass {
    /// Frosted acrylic, like the Windows 11 taskbar.
    Acrylic,
    /// A plain blur with no frost or noise (Aero-style clear glass).
    Blur,
    /// No blur: a solid panel.
    Solid,
}

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
    /// Panel colour without blur, and while a panel animates.
    pub solid: (f32, f32, f32),
    /// A faint wash over the blur (it also keeps the pixels clickable).
    pub film: Color,
    /// Panel outline.
    pub border: Color,
    /// Light catching the top edge, and pooling along the bottom (0 = none).
    pub sheen: f32,
    pub glow: f32,
    /// Highlights: the app in front, toggles, progress, today.
    pub accent: Color,
}

const WIN_BLUE: Color = Color(0.38, 0.67, 1.0, 1.0);

pub const THEMES: [Theme; 8] = [
    Theme {
        key: "acrylic",
        name: "Acrylic",
        about: "Frosted dark glass, like Windows 11. The default.",
        glass: Glass::Acrylic,
        tint: (0.110, 0.110, 0.118),
        tint_scale: 1.0,
        solid: (0.13, 0.13, 0.14),
        film: Color::white(0.015),
        border: Color::white(0.10),
        sheen: 0.0,
        glow: 0.0,
        accent: WIN_BLUE,
    },
    Theme {
        key: "liquid_glass",
        name: "Liquid Glass",
        about: "Clear glass with bright edges that catch the light.",
        glass: Glass::Blur,
        tint: (0.09, 0.10, 0.13),
        tint_scale: 0.6,
        solid: (0.15, 0.16, 0.19),
        film: Color::white(0.04),
        border: Color::white(0.30),
        sheen: 0.16,
        glow: 0.07,
        accent: Color(0.42, 0.72, 1.0, 1.0),
    },
    Theme {
        key: "windows_glass",
        name: "Windows Glass",
        about: "Aero-style clear blue glass with a glossy top.",
        glass: Glass::Blur,
        tint: (0.07, 0.19, 0.33),
        tint_scale: 0.8,
        solid: (0.09, 0.19, 0.31),
        film: Color::white(0.03),
        border: Color::white(0.38),
        sheen: 0.22,
        glow: 0.0,
        accent: Color(0.45, 0.78, 1.0, 1.0),
    },
    Theme {
        key: "frosted",
        name: "Frosted Glass",
        about: "Lighter, thicker frost with a soft edge.",
        glass: Glass::Acrylic,
        tint: (0.24, 0.24, 0.27),
        tint_scale: 1.0,
        solid: (0.21, 0.21, 0.23),
        film: Color::white(0.05),
        border: Color::white(0.18),
        sheen: 0.05,
        glow: 0.0,
        accent: Color(0.42, 0.70, 1.0, 1.0),
    },
    Theme {
        key: "tinted",
        name: "Tinted Glass",
        about: "Acrylic in your Windows accent colour.",
        glass: Glass::Acrylic,
        // Filled in from the accent colour by `current`.
        tint: (0.11, 0.11, 0.12),
        tint_scale: 1.0,
        solid: (0.13, 0.13, 0.14),
        film: Color::white(0.02),
        border: Color::white(0.14),
        sheen: 0.04,
        glow: 0.0,
        accent: WIN_BLUE,
    },
    Theme {
        key: "translucent",
        name: "Translucent",
        about: "Barely-there glass with no outline.",
        glass: Glass::Blur,
        tint: (0.06, 0.06, 0.07),
        tint_scale: 0.35,
        solid: (0.12, 0.12, 0.13),
        film: Color::white(0.02),
        border: Color::white(0.0),
        sheen: 0.0,
        glow: 0.0,
        accent: WIN_BLUE,
    },
    Theme {
        key: "rose_pine",
        name: "Ros\u{e9} Pine",
        about: "Deep purple glass with iris highlights.",
        glass: Glass::Acrylic,
        tint: (0.098, 0.090, 0.141),
        tint_scale: 1.0,
        solid: (0.122, 0.114, 0.180),
        film: Color(0.77, 0.65, 0.91, 0.02),
        border: Color(0.77, 0.65, 0.91, 0.22),
        sheen: 0.0,
        glow: 0.0,
        accent: Color(0.769, 0.655, 0.906, 1.0),
    },
    Theme {
        key: "clean_slate",
        name: "Clean Slate",
        about: "Solid and flat, with no blur at all.",
        glass: Glass::Solid,
        tint: (0.12, 0.12, 0.13),
        tint_scale: 1.0,
        solid: (0.12, 0.12, 0.13),
        film: Color::white(0.0),
        border: Color::white(0.08),
        sheen: 0.0,
        glow: 0.0,
        accent: WIN_BLUE,
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
    resolve(THEMES[CURRENT.load(Ordering::Relaxed).min(THEMES.len() - 1)])
}

/// `t` with the colours it takes from Windows filled in.
pub fn resolve(mut t: Theme) -> Theme {
    if t.key == "tinted" {
        let (r, g, b) = unpack(SYSTEM_ACCENT.load(Ordering::Relaxed));
        // Darkened behind the glass so white text stays readable.
        t.tint = (r * 0.4, g * 0.4, b * 0.4);
        t.solid = (r * 0.3 + 0.04, g * 0.3 + 0.04, b * 0.3 + 0.04);
        t.border = Color(mix(r, 1.0, 0.5), mix(g, 1.0, 0.5), mix(b, 1.0, 0.5), 0.22);
        t.accent = Color(mix(r, 1.0, 0.3), mix(g, 1.0, 0.3), mix(b, 1.0, 0.3), 1.0);
    }
    t
}

pub fn accent() -> Color {
    current().accent
}

/// The accent under the pointer.
pub fn accent_hover() -> Color {
    lighten(accent(), 0.15)
}

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
    // Acrylic takes its tint from DWM. A clear blur gets none there (it is
    // painted over the blur in `panel` instead, so it looks the same on
    // every Windows build).
    let tint = match t.glass {
        Glass::Acrylic => pack(t.tint, opacity * t.tint_scale),
        _ => pack(t.tint, 0.0),
    };
    b.style(if blur { t.glass } else { Glass::Solid }, tint, radius);
    blur
}

/// An opaque backdrop in the theme's panel colour (for Alt+Tab, whose
/// backdrop also hosts the live previews, so it must always be there).
pub fn style_opaque(b: &Backdrop, radius: f32) {
    b.style(Glass::Acrylic, pack(current().solid, 1.0), radius);
}

/// Paints a panel: over a live blur (`blurred`) the theme's wash, otherwise
/// a solid panel at `solid_alpha`; then the sheen and the outline. `alpha`
/// fades all of it.
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
    let t = current();
    if blurred {
        if t.glass == Glass::Blur {
            let (cr, cg, cb) = t.tint;
            p.rrect(x, y, w, h, r, Color(cr, cg, cb, (opacity * t.tint_scale).clamp(0.0, 1.0) * alpha));
        }
        p.rrect(x, y, w, h, r, t.film.alpha(alpha));
    } else {
        let (cr, cg, cb) = t.solid;
        p.rrect(x, y, w, h, r, Color(cr, cg, cb, solid_alpha * alpha));
    }
    if (t.sheen > 0.0 || t.glow > 0.0) && h > 2.0 {
        // The light fades out over the first and last few pixels.
        let edge = (36.0 / h).min(0.5);
        p.gradient_rrect(
            x,
            y,
            w,
            h,
            r,
            &[
                (0.0, Color::white(t.sheen * alpha)),
                (edge, Color::white(0.0)),
                (1.0 - edge, Color::white(0.0)),
                (1.0, Color::white(t.glow * alpha)),
            ],
        );
    }
    if t.border.3 > 0.0 {
        p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, r, t.border.alpha(alpha), 1.0);
    }
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
    fn pack_round_trips() {
        assert_eq!(pack((1.0, 0.0, 0.0), 1.0), 0xFFFF_0000);
        assert_eq!(pack((0.0, 0.0, 1.0), 0.0), 0x0000_00FF);
        let (r, g, b) = unpack(0x00_80_40_20);
        assert_eq!(pack((r, g, b), 0.0), 0x0080_4020);
    }
}
