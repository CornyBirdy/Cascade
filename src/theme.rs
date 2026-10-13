//! Themes: one look shared by every Cascade surface (the notch's lip, the
//! taskbar, the Start menu, the panels and Alt+Tab). Picked in Settings >
//! Appearance, or `theme =` in the config.
//!
//! A theme is a material, not only a colour: which glass DWM puts behind a
//! panel (frosted acrylic, a clear blur or none), how the glass is washed,
//! and how its edge catches the light (a flat line, a specular rim with
//! glints on the corners, Aero's dark-and-light double edge, a coloured
//! glow). It costs nothing at run time beyond a few strokes per panel.

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
    /// A plain blur with no frost or noise (clear glass).
    Blur,
    /// No blur: a solid panel.
    Solid,
}

/// How a panel's edge is drawn.
#[derive(Clone, Copy)]
pub enum Rim {
    None,
    /// One flat line.
    Line(Color),
    /// Light catching thick glass: a bright top edge with glints on the
    /// top-left and bottom-right corners, faint down the sides (the number
    /// is its strength).
    Specular(f32),
    /// A dark outer line around a light inner one, like Aero's frames.
    Double(Color, Color),
    /// A line shading from one colour (top-left) to another (bottom-right).
    Gradient(Color, Color),
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
    /// Panel colour without blur.
    pub solid: (f32, f32, f32),
    /// A wash over the panel, top and bottom (it also keeps the pixels over
    /// the blur clickable, so neither end may be fully clear).
    pub film: (Color, Color),
    pub rim: Rim,
    /// Light caught just inside the edge, as in the thick rim of real glass
    /// (with a Gradient rim, a glow in its colours). 0 = none.
    pub bezel: f32,
    /// A glossy reflection over the top of the panel, Aero-style. 0 = none.
    pub gloss: f32,
    /// Highlights: the app in front, toggles, progress, today.
    pub accent: Color,
}

const WIN_BLUE: Color = Color(0.38, 0.67, 1.0, 1.0);
const LOVE: Color = Color(0.922, 0.435, 0.573, 1.0);

const fn white(a: f32) -> Color {
    Color(1.0, 1.0, 1.0, a)
}

pub const THEMES: [Theme; 10] = [
    Theme {
        key: "acrylic",
        name: "Acrylic",
        about: "Frosted dark glass with a fine outline, like Windows 11. The default.",
        glass: Glass::Acrylic,
        tint: (0.110, 0.110, 0.118),
        tint_scale: 1.0,
        solid: (0.13, 0.13, 0.14),
        film: (white(0.015), white(0.015)),
        rim: Rim::Line(white(0.10)),
        bezel: 0.0,
        gloss: 0.0,
        accent: WIN_BLUE,
    },
    Theme {
        key: "liquid_glass",
        name: "Liquid Glass",
        about: "Apple-style: nearly clear, with a thick rim that catches the light on its corners.",
        glass: Glass::Blur,
        tint: (0.10, 0.11, 0.14),
        tint_scale: 0.3,
        solid: (0.17, 0.18, 0.21),
        film: (white(0.07), white(0.02)),
        rim: Rim::Specular(1.0),
        bezel: 0.12,
        gloss: 0.0,
        accent: Color(0.04, 0.52, 1.0, 1.0),
    },
    Theme {
        key: "windows_glass",
        name: "Windows Glass",
        about: "Aero, as in Windows 7: sky-blue glass, a glossy top and a dark-and-light edge.",
        glass: Glass::Blur,
        tint: (0.10, 0.28, 0.48),
        tint_scale: 0.85,
        solid: (0.10, 0.22, 0.36),
        film: (white(0.06), white(0.01)),
        rim: Rim::Double(Color(0.0, 0.0, 0.0, 0.55), white(0.40)),
        bezel: 0.0,
        gloss: 0.20,
        accent: Color(0.45, 0.78, 1.0, 1.0),
    },
    Theme {
        key: "frosted",
        name: "Frosted Glass",
        about: "Milky, sand-blasted glass that glows softly at its edges.",
        glass: Glass::Acrylic,
        tint: (0.30, 0.31, 0.34),
        tint_scale: 1.0,
        solid: (0.25, 0.26, 0.28),
        film: (white(0.11), white(0.06)),
        rim: Rim::Line(white(0.24)),
        bezel: 0.09,
        gloss: 0.0,
        accent: Color(0.42, 0.70, 1.0, 1.0),
    },
    Theme {
        key: "tinted",
        name: "Tinted Glass",
        about: "Acrylic in your Windows accent colour, edged in a lighter shade of it.",
        glass: Glass::Acrylic,
        // Filled in from the accent colour by `resolve`.
        tint: (0.11, 0.11, 0.12),
        tint_scale: 1.0,
        solid: (0.13, 0.13, 0.14),
        film: (white(0.05), white(0.01)),
        rim: Rim::Gradient(white(0.4), white(0.1)),
        bezel: 0.0,
        gloss: 0.0,
        accent: WIN_BLUE,
    },
    Theme {
        key: "translucent",
        name: "Translucent",
        about: "Barely-there clear glass: no tint, no outline.",
        glass: Glass::Blur,
        tint: (0.05, 0.05, 0.06),
        tint_scale: 0.15,
        solid: (0.12, 0.12, 0.13),
        film: (white(0.012), white(0.012)),
        rim: Rim::None,
        bezel: 0.0,
        gloss: 0.0,
        accent: WIN_BLUE,
    },
    Theme {
        key: "rose_pine",
        name: "Ros\u{e9} Pine",
        about: "Deep purple glass edged from iris to love, from the Ros\u{e9} Pine palette.",
        glass: Glass::Acrylic,
        tint: (0.098, 0.090, 0.141),
        tint_scale: 1.0,
        solid: (0.122, 0.114, 0.180),
        film: (Color(0.77, 0.65, 0.91, 0.05), Color(0.92, 0.44, 0.57, 0.025)),
        rim: Rim::Gradient(Color(0.769, 0.655, 0.906, 0.75), Color(0.922, 0.435, 0.573, 0.6)),
        bezel: 0.0,
        gloss: 0.0,
        accent: LOVE,
    },
    Theme {
        key: "neon",
        name: "Neon",
        about: "Near-black glass with a cyan-to-magenta glow round the edge.",
        glass: Glass::Acrylic,
        tint: (0.02, 0.02, 0.04),
        tint_scale: 1.4,
        solid: (0.03, 0.03, 0.05),
        film: (Color(0.0, 0.85, 1.0, 0.025), Color(1.0, 0.2, 0.8, 0.025)),
        rim: Rim::Gradient(Color(0.0, 0.9, 1.0, 0.95), Color(1.0, 0.25, 0.85, 0.95)),
        bezel: 0.35,
        gloss: 0.0,
        accent: Color(0.10, 0.70, 0.95, 1.0),
    },
    Theme {
        key: "obsidian",
        name: "Obsidian",
        about: "Solid black, polished: a soft reflection on top and a lit rim. No blur.",
        glass: Glass::Solid,
        tint: (0.035, 0.035, 0.04),
        tint_scale: 1.0,
        solid: (0.035, 0.035, 0.04),
        film: (white(0.02), white(0.0)),
        rim: Rim::Specular(0.45),
        bezel: 0.0,
        gloss: 0.07,
        accent: Color(0.98, 0.55, 0.20, 1.0),
    },
    Theme {
        key: "clean_slate",
        name: "Clean Slate",
        about: "Matte and flat: solid slate grey, no blur, no shine.",
        glass: Glass::Solid,
        tint: (0.12, 0.125, 0.14),
        tint_scale: 1.0,
        solid: (0.12, 0.125, 0.14),
        film: (white(0.0), white(0.0)),
        rim: Rim::Line(white(0.05)),
        bezel: 0.0,
        gloss: 0.0,
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
        t.film = (Color(r, g, b, 0.08), Color(r, g, b, 0.01));
        t.rim = Rim::Gradient(
            Color(mix(r, 1.0, 0.6), mix(g, 1.0, 0.6), mix(b, 1.0, 0.6), 0.55),
            Color(mix(r, 1.0, 0.2), mix(g, 1.0, 0.2), mix(b, 1.0, 0.2), 0.2),
        );
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
    // Acrylic takes its tint from DWM. A clear blur gets none there (it is
    // painted over the blur in `panel` instead, so it looks the same on
    // every Windows build).
    let tint = match t.glass {
        Glass::Acrylic => pack(t.tint, (opacity * t.tint_scale).min(1.0)),
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

/// Paints a panel in the current theme: over a live blur (`blurred`) the
/// theme's wash, otherwise a solid panel at `solid_alpha`; then its
/// reflections and edge. `alpha` fades all of it.
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
    paint(p, &current(), x, y, w, h, r, blurred, opacity, solid_alpha, alpha);
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
    if blurred {
        if t.glass == Glass::Blur {
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

    if t.gloss > 0.0 && h > 4.0 {
        // A reflection with a crisp lower edge over the top of the panel
        // (about half of a bar, a strip across the top of a tall panel).
        let band = (h * 0.5).min(42.0) / h;
        let g = t.gloss * alpha;
        p.gradient_rrect(
            x,
            y,
            w,
            h,
            r,
            &[(0.0, white(g)), (band * 0.92, white(g * 0.35)), (band, white(0.0)), (1.0, white(0.0))],
        );
    }

    if t.bezel > 0.0 && h > 6.0 {
        bezel(p, t, x, y, w, h, r, alpha);
    }

    let line = p.px(1.0);
    let (lx, ly, lw, lh) = (x + line / 2.0, y + line / 2.0, w - line, h - line);
    match t.rim {
        Rim::None => {}
        Rim::Line(c) => p.stroke_rrect(lx, ly, lw, lh, r, c.alpha(alpha), line),
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

/// Light just inside the edge: a few soft rings, strongest at the rim and
/// fading inwards. Over glass they read as the bevel of a thick pane; with a
/// gradient rim they are its glow.
fn bezel(p: &Painter, t: &Theme, x: f32, y: f32, w: f32, h: f32, r: f32, alpha: f32) {
    let rings = [(1.5, 1.0), (3.5, 0.55), (6.0, 0.25)];
    for (inset, k) in rings {
        let (ix, iy, iw, ih) = (x + inset, y + inset, w - 2.0 * inset, h - 2.0 * inset);
        if iw <= 0.0 || ih <= 0.0 {
            break;
        }
        let a = t.bezel * k * alpha;
        let ir = (r - inset).max(0.0);
        let width = 2.0;
        let stops = match t.rim {
            Rim::Gradient(c0, c1) => [(0.0, Color(c0.0, c0.1, c0.2, a)), (1.0, Color(c1.0, c1.1, c1.2, a))],
            // Glass: brighter at the top, where the light comes from.
            _ => [(0.0, white(a)), (1.0, white(a * 0.45))],
        };
        let brush = match t.rim {
            Rim::Gradient(..) => p.linear(x, y, x + w, y + h, &stops),
            _ => p.linear(x, y, x, y + h, &stops),
        };
        if let Some(b) = brush {
            p.stroke_rrect_with(ix, iy, iw, ih, ir, &b, width);
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
}
