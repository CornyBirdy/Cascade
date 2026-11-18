//! The look shared by every Cascade surface (the notch's lip, the taskbar,
//! the Start menu, the panels and Alt+Tab): one of two themes, and your
//! accent colour. Picked in Settings > Appearance, or `theme =` and
//! `accent =` in the config.
//!
//! - **Acrylic**: Windows 11's frosted glass with a fine edge lit along the top.
//! - **Liquid Glass**: a light blur and what thick glass does to light, drawn
//!   rather than simulated: a brighter top where it catches the light, a
//!   band of gathered light just inside the rounded edge, a faint split of
//!   colour along it, a soft reflection from the top left, the glass's
//!   thickness as a shadow along the bottom, and a specular rim with glints
//!   on two corners. Only gradients, painted once per panel size into a
//!   bitmap and drawn from that (see `panel`): nothing reads the screen, so
//!   it costs no more than Acrylic.
//!
//! The accent colour (highlights: the app in front, toggles, progress,
//! today) is your Windows accent by default, or one picked from Windows'
//! own palette, and can tint the glass too, as Windows' *Show accent colour
//! on Start and taskbar* does.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use windows::core::BOOL;
use windows::Win32::Graphics::Direct2D::ID2D1Brush;
use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;

use crate::backdrop::Backdrop;
use crate::gfx::{Color, Painter};

/// What DWM puts behind a panel.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Glass {
    /// Frosted acrylic, like the Windows 11 taskbar.
    Acrylic,
    /// A plain blur with no frost or noise.
    Blur,
    /// No blur: a solid panel (the Blur setting off).
    Solid,
}

/// How a panel's edge is drawn.
#[derive(Clone, Copy)]
pub enum Rim {
    /// One line, lighter along the top than the bottom (light from above).
    Lit(Color, Color),
    /// Light catching thick glass: a bright top edge with glints on the
    /// top-left and bottom-right corners, faint down the sides (the number
    /// is its strength).
    Specular(f32),
}

/// What thick glass does to the light. Every effect is off at 0.
#[derive(Clone, Copy)]
pub struct Light {
    /// Light gathered just inside the rounded edge: (width, strength) of a
    /// band brightest at the rim that fades smoothly inwards, lit from above.
    pub lens: (f32, f32),
    /// Light split into colours along the edge: a cool fringe along the lit
    /// top, a warm one along the bottom.
    pub dispersion: f32,
    /// A soft reflection from the top left.
    pub sheen: f32,
    /// The glass's thickness seen from the front: a soft dark band just
    /// inside the edge, heaviest along the bottom.
    pub depth: f32,
}

const NONE: Light = Light { lens: (0.0, 0.0), dispersion: 0.0, sheen: 0.0, depth: 0.0 };

#[derive(Clone, Copy)]
pub struct Theme {
    /// Config value.
    pub key: &'static str,
    pub name: &'static str,
    pub about: &'static str,
    pub glass: Glass,
    /// Colour the glass is tinted with; the Tint sliders set how strongly.
    pub tint: (f32, f32, f32),
    /// Scales the Tint sliders, so light glass stays light.
    pub tint_scale: f32,
    /// Panel colour without blur.
    pub solid: (f32, f32, f32),
    /// A wash over the panel, top and bottom: the haze of the glass itself
    /// (it also keeps the pixels over the blur clickable, so neither end may
    /// be fully clear).
    pub film: (Color, Color),
    pub rim: Rim,
    pub light: Light,
    /// Highlights: the app in front, toggles, progress, today (filled in
    /// from the accent colour by `resolve`).
    pub accent: Color,
}

const fn white(a: f32) -> Color {
    Color(1.0, 1.0, 1.0, a)
}

const fn black(a: f32) -> Color {
    Color(0.0, 0.0, 0.0, a)
}

pub const THEMES: [Theme; 2] = [
    Theme {
        key: "acrylic",
        name: "Acrylic",
        about: "Frosted dark glass with a fine edge, lit along the top, like Windows 11. The default.",
        glass: Glass::Acrylic,
        // Neutral grey: any colour in it comes from the accent, if you want it.
        tint: (0.114, 0.114, 0.114),
        tint_scale: 1.0,
        solid: (0.133, 0.133, 0.133),
        film: (white(0.015), white(0.015)),
        rim: Rim::Lit(white(0.14), white(0.06)),
        light: NONE,
        accent: Color(0.38, 0.67, 1.0, 1.0),
    },
    Theme {
        key: "liquid_glass",
        name: "Liquid Glass",
        about:
            "Thick, lightly frosted glass: bright along its top, light gathered in its rounded edge, a glinting rim.",
        glass: Glass::Blur,
        tint: (0.10, 0.10, 0.11),
        tint_scale: 0.4,
        solid: (0.17, 0.17, 0.18),
        // Brighter at the top, as the glass catches the light from above.
        film: (white(0.09), white(0.02)),
        rim: Rim::Specular(0.6),
        light: Light { lens: (9.0, 0.09), dispersion: 0.05, sheen: 0.05, depth: 0.22 },
        accent: Color(0.38, 0.67, 1.0, 1.0),
    },
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);
/// The Windows accent colour (0xRRGGBB).
static SYSTEM_ACCENT: AtomicU32 = AtomicU32::new(0x0078D4);

pub fn is_known(key: &str) -> bool {
    THEMES.iter().any(|t| t.key == key)
}

/// Switches to the theme named `key` (unknown names, the themes there used
/// to be among them, get Acrylic).
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

/// Windows' accent colours (Settings > Personalisation > Colours), for
/// picking one of your own: (0xRRGGBB, name).
pub const PALETTE: [(u32, &str); 24] = [
    (0xFFB900, "Yellow gold"),
    (0xF7630C, "Orange"),
    (0xDA3B01, "Rust"),
    (0xE81123, "Red"),
    (0xEA005E, "Rose"),
    (0xE3008C, "Plum"),
    (0xC239B3, "Orchid"),
    (0x881798, "Violet"),
    (0x744DA9, "Iris"),
    (0x8764B8, "Lavender"),
    (0x6B69D6, "Indigo"),
    (0x0078D4, "Blue"),
    (0x0063B1, "Navy"),
    (0x0099BC, "Cool blue"),
    (0x00B7C3, "Seafoam"),
    (0x038387, "Teal"),
    (0x00B294, "Mint"),
    (0x00CC6A, "Turf green"),
    (0x107C10, "Green"),
    (0x498205, "Meadow"),
    (0x7A7574, "Grey"),
    (0x68768A, "Steel"),
    (0x567C73, "Moss"),
    (0x847545, "Desert"),
];

/// The accent picked (`accent`: "windows", or "#RRGGBB"), or `u32::MAX`
/// for the Windows accent.
static ACCENT: AtomicU32 = AtomicU32::new(u32::MAX);
/// Whether the glass takes on the accent colour (`accent_glass`).
static ACCENT_GLASS: AtomicBool = AtomicBool::new(false);

/// `accent` as 0xRRGGBB, or None for the Windows accent ("windows", or
/// anything that isn't a colour).
pub fn parse_accent(value: &str) -> Option<u32> {
    let hex = value.trim().strip_prefix('#')?;
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok()).flatten()
}

/// Sets the accent colour (`accent` in the config) and whether the glass
/// is tinted with it (`accent_glass`).
pub fn set_accent(value: &str, on_glass: bool) {
    ACCENT.store(parse_accent(value).unwrap_or(u32::MAX), Ordering::Relaxed);
    ACCENT_GLASS.store(on_glass, Ordering::Relaxed);
}

/// Whether the look follows the Windows accent colour (so it has to be
/// redrawn when that changes).
pub fn follows_accent() -> bool {
    ACCENT.load(Ordering::Relaxed) == u32::MAX
}

/// The accent colour as it is, before it's lightened for dark panels.
fn base_accent() -> (f32, f32, f32) {
    match ACCENT.load(Ordering::Relaxed) {
        u32::MAX => unpack(SYSTEM_ACCENT.load(Ordering::Relaxed)),
        c => unpack(c),
    }
}

/// The Windows accent colour now (for the settings page's sample of it).
pub fn windows_accent() -> Color {
    let (r, g, b) = unpack(SYSTEM_ACCENT.load(Ordering::Relaxed));
    Color(r, g, b, 1.0)
}

/// `t` with the accent colour filled in (and on its glass, if asked).
pub fn resolve(mut t: Theme) -> Theme {
    let (r, g, b) = base_accent();
    // Lighter, as Windows draws its accent on dark surfaces, so it reads.
    t.accent = lighten(Color(r, g, b, 1.0), 0.3);
    if ACCENT_GLASS.load(Ordering::Relaxed) {
        // Darkened behind the glass so white text stays readable.
        let tint = |base: (f32, f32, f32), k: f32| {
            (mix(base.0, r * k, 0.75), mix(base.1, g * k, 0.75), mix(base.2, b * k, 0.75))
        };
        t.tint = tint(t.tint, 0.42);
        t.solid = tint(t.solid, 0.34);
        t.film = (Color(r, g, b, t.film.0 .3.max(0.05)), Color(r, g, b, t.film.1 .3.max(0.012)));
    }
    edged(t)
}

/// `theme_edge` (1 = as designed), as f32 bits.
static EDGE: AtomicU32 = AtomicU32::new(0x3F80_0000); // 1.0

/// How strongly the glass's edge catches the light: its outline and the
/// light gathered along it (0 = none, 1 = as designed, 2 = twice as strong).
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
        Rim::Lit(a, b) => Rim::Lit(c(a), c(b)),
        Rim::Specular(s) => Rim::Specular(s * k),
    };
    let l = &mut t.light;
    l.lens.1 *= k;
    l.dispersion *= k;
    l.depth *= k;
    t
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
/// turned off).
pub fn style_backdrop(b: &Backdrop, acrylic: bool, opacity: f32, radius: f32) -> bool {
    let t = current();
    let blur = blurs(acrylic);
    // Acrylic takes its tint from DWM. A plain blur gets none there (it is
    // painted in `panel` instead, so it looks the same on every Windows build).
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
/// theme's wash, otherwise a solid panel at `solid_alpha`; then the light
/// in and on it, and its edge. `alpha` fades all of it.
///
/// It's painted once into a bitmap and drawn from that while it stays the
/// same (see `Painter::cached`): the gradients and edge rings cost nothing
/// on frames where only what's on the panel moves.
#[allow(clippy::too_many_arguments)]
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

/// Everything the current look depends on (the theme, the accent and the
/// edge setting), for telling cached panels apart.
fn look() -> u64 {
    let parts = [
        CURRENT.load(Ordering::Relaxed) as u64,
        SYSTEM_ACCENT.load(Ordering::Relaxed) as u64,
        ACCENT.load(Ordering::Relaxed) as u64,
        ACCENT_GLASS.load(Ordering::Relaxed) as u64,
        EDGE.load(Ordering::Relaxed) as u64,
    ];
    parts.iter().fold(0xcbf2_9ce4_8422_2325, |h, &v| (h ^ v).wrapping_mul(0x0100_0000_01b3))
}

/// `panel` in theme `t`.
#[allow(clippy::too_many_arguments)]
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

    let l = t.light;
    if l.sheen > 0.0 && h > 4.0 {
        sheen(p, x, y, w, h, r, l.sheen * alpha);
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

    match t.rim {
        Rim::Lit(a, b) => {
            let line = p.px(1.0);
            let (lx, ly, lw, lh) = (x + line / 2.0, y + line / 2.0, w - line, h - line);
            if let Some(brush) = p.linear(x, y, x, y + h, &[(0.0, a.alpha(alpha)), (1.0, b.alpha(alpha))]) {
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
/// colourful wallpaper, so frosted and light glass look different.
pub fn preview(p: &Painter, t: &Theme, x: f32, y: f32, w: f32, h: f32, r: f32) {
    // The "wallpaper", seen through the glass.
    if let Some(b) = p.linear(
        x,
        y,
        x + w,
        y + h,
        &[(0.0, Color(1.0, 0.42, 0.24, 1.0)), (0.5, Color(0.98, 0.31, 0.64, 1.0)), (1.0, Color(0.20, 0.45, 1.0, 1.0))],
    ) {
        p.fill_rrect_with(x, y, w, h, r, &b);
    }
    let opacity = 0.55;
    let (cr, cg, cb) = t.tint;
    match t.glass {
        // Frost hides the wallpaper; a light blur mostly shows it.
        Glass::Acrylic => {
            p.rrect(x, y, w, h, r, Color(cr, cg, cb, (0.55 + opacity * t.tint_scale * 0.4).min(0.95)));
            p.rrect(x, y, w, h, r, white(0.04));
        }
        Glass::Blur => p.rrect(x, y, w, h, r, Color(cr, cg, cb, 0.25 + opacity * t.tint_scale * 0.5)),
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
    fn reads_accents() {
        assert_eq!(parse_accent("#0078D4"), Some(0x0078D4));
        assert_eq!(parse_accent(" #ffb900 "), Some(0xFFB900));
        assert_eq!(parse_accent("windows"), None);
        assert_eq!(parse_accent("#12345"), None);
        assert!(PALETTE.iter().all(|(c, _)| *c <= 0xFF_FFFF));
    }
}
