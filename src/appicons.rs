//! Icons for Cascade's own apps (Files, Calculator, Camera, Snip...), drawn
//! here rather than borrowed from Windows: each a rounded tile in its own
//! colour with a white picture on it, so they're told apart at a glance on
//! the taskbar and in Start, and look like one set.
//!
//! They're drawn in software from a few shapes (rounded boxes, circles,
//! rings, lines, triangles), each edge smoothed by its distance from the
//! pixel, so any size comes out sharp and no window or COM is needed.

use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

/// A colour, 0..1, straight (not premultiplied).
#[derive(Clone, Copy)]
struct C(f32, f32, f32, f32);

const WHITE: C = C(1.0, 1.0, 1.0, 1.0);

fn hex(v: u32) -> C {
    C(((v >> 16) & 0xFF) as f32 / 255.0, ((v >> 8) & 0xFF) as f32 / 255.0, (v & 0xFF) as f32 / 255.0, 1.0)
}

fn fade(c: C, a: f32) -> C {
    C(c.0, c.1, c.2, c.3 * a)
}

/// What can be drawn: positions and sizes are shares of the icon (0..1).
enum Shape {
    /// x, y, w, h, corner radius.
    Box(f32, f32, f32, f32, f32),
    Circle(f32, f32, f32),
    /// Centre, radius, line width; only the part from angle `a0` to `a1`
    /// (radians, clockwise from three o'clock) when they differ.
    Ring(f32, f32, f32, f32, f32, f32),
    /// From, to, width (round ends).
    Line(f32, f32, f32, f32, f32),
    Tri([(f32, f32); 3]),
}

/// How far point (x, y) is outside the shape (negative inside), in icon units.
fn distance(s: &Shape, x: f32, y: f32) -> f32 {
    match *s {
        Shape::Box(bx, by, w, h, r) => {
            let (cx, cy) = (bx + w / 2.0, by + h / 2.0);
            let r = r.min(w / 2.0).min(h / 2.0);
            let qx = (x - cx).abs() - (w / 2.0 - r);
            let qy = (y - cy).abs() - (h / 2.0 - r);
            qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
        }
        Shape::Circle(cx, cy, r) => (x - cx).hypot(y - cy) - r,
        Shape::Ring(cx, cy, r, w, a0, a1) => {
            let d = ((x - cx).hypot(y - cy) - r).abs() - w / 2.0;
            if a0 == a1 {
                return d;
            }
            let mut a = (y - cy).atan2(x - cx);
            let tau = std::f32::consts::TAU;
            while a < a0 {
                a += tau;
            }
            while a > a0 + tau {
                a -= tau;
            }
            if a <= a1 {
                d
            } else {
                // Past the ends: the distance to the nearer round end.
                let end = |t: f32| (x - (cx + r * t.cos())).hypot(y - (cy + r * t.sin())) - w / 2.0;
                end(a0).min(end(a1))
            }
        }
        Shape::Line(x0, y0, x1, y1, w) => {
            let (dx, dy) = (x1 - x0, y1 - y0);
            let len2 = (dx * dx + dy * dy).max(1e-9);
            let t = (((x - x0) * dx + (y - y0) * dy) / len2).clamp(0.0, 1.0);
            (x - (x0 + t * dx)).hypot(y - (y0 + t * dy)) - w / 2.0
        }
        Shape::Tri(p) => {
            // Inside all three edges (wound either way): the nearest edge's distance.
            let area = (p[1].0 - p[0].0) * (p[2].1 - p[0].1) - (p[2].0 - p[0].0) * (p[1].1 - p[0].1);
            let sign = if area < 0.0 { -1.0 } else { 1.0 };
            let mut out = f32::MIN;
            for i in 0..3 {
                let (a, b) = (p[i], p[(i + 1) % 3]);
                let (ex, ey) = (b.0 - a.0, b.1 - a.1);
                let len = ex.hypot(ey).max(1e-9);
                // Outward normal distance.
                let d = -sign * (ex * (y - a.1) - ey * (x - a.0)) / len;
                out = out.max(d);
            }
            out
        }
    }
}

/// An icon being drawn: straight RGBA, size×size.
struct Canvas {
    size: usize,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(size: usize) -> Canvas {
        Canvas { size, px: vec![[0.0; 4]; size * size] }
    }

    /// Lays `shape` on in `c` (or a gradient from `c` at the top to `c2` at the bottom).
    fn fill(&mut self, shape: &Shape, c: C, c2: Option<C>) {
        let n = self.size as f32;
        for y in 0..self.size {
            for x in 0..self.size {
                let (u, v) = ((x as f32 + 0.5) / n, (y as f32 + 0.5) / n);
                let cover = (0.5 - distance(shape, u, v) * n).clamp(0.0, 1.0);
                if cover <= 0.0 {
                    continue;
                }
                let col = match c2 {
                    Some(b) => {
                        C(c.0 + (b.0 - c.0) * v, c.1 + (b.1 - c.1) * v, c.2 + (b.2 - c.2) * v, c.3 + (b.3 - c.3) * v)
                    }
                    None => c,
                };
                let a = col.3 * cover;
                let d = &mut self.px[y * self.size + x];
                let out_a = a + d[3] * (1.0 - a);
                if out_a > 0.0 {
                    for k in 0..3 {
                        let src = [col.0, col.1, col.2][k];
                        d[k] = (src * a + d[k] * d[3] * (1.0 - a)) / out_a;
                    }
                }
                d[3] = out_a;
            }
        }
    }

    /// BGRA bytes, premultiplied or straight.
    fn bytes(&self, premultiplied: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 4);
        for p in &self.px {
            let a = p[3].clamp(0.0, 1.0);
            let k = if premultiplied { a } else { 1.0 };
            let ch = |v: f32| ((v * k).clamp(0.0, 1.0) * 255.0).round() as u8;
            out.extend_from_slice(&[ch(p[2]), ch(p[1]), ch(p[0]), (a * 255.0).round() as u8]);
        }
        out
    }
}

/// The app's tile colours (top, bottom) and its picture.
/// A tile's colours (top, bottom) and the picture on it.
type Design = (u32, u32, Vec<(Shape, C)>);

fn design(id: &str) -> Option<Design> {
    use Shape::*;
    let w = WHITE;
    let soft = fade(WHITE, 0.55);
    let pi = std::f32::consts::PI;
    Some(match id.to_ascii_lowercase().as_str() {
        "cascade.files" => (
            0xFFD54F,
            0xF5A300,
            vec![
                (Box(0.22, 0.30, 0.26, 0.14, 0.04), w),
                (Box(0.22, 0.36, 0.56, 0.36, 0.05), w),
                (Box(0.22, 0.42, 0.56, 0.04, 0.0), fade(hex(0xF5A300), 0.35)),
            ],
        ),
        "cascade.photos" => (
            0x4FC3F7,
            0x1E73D8,
            vec![
                (Box(0.20, 0.24, 0.60, 0.52, 0.07), w),
                (Box(0.25, 0.29, 0.50, 0.42, 0.04), hex(0x2A8BE6)),
                (Tri([(0.27, 0.69), (0.44, 0.45), (0.58, 0.69)]), w),
                (Tri([(0.46, 0.69), (0.60, 0.53), (0.73, 0.69)]), soft),
                (Circle(0.63, 0.39, 0.055), w),
            ],
        ),
        "cascade.diskusage" => (
            0x4DD0C0,
            0x00897B,
            vec![(Ring(0.5, 0.5, 0.21, 0.13, 0.0, 0.0), soft), (Ring(0.5, 0.5, 0.21, 0.13, -pi / 2.0, pi * 0.75), w)],
        ),
        "cascade.calculator" => (0x5C6670, 0x2E3338, {
            let mut v = vec![(Box(0.25, 0.20, 0.50, 0.14, 0.03), fade(hex(0xB3E5FC), 0.9))];
            for r in 0..3 {
                for c in 0..3 {
                    let col = if c == 2 && r == 2 { hex(0xFF9F43) } else { w };
                    v.push((Box(0.25 + c as f32 * 0.18, 0.40 + r as f32 * 0.13, 0.14, 0.10, 0.025), col));
                }
            }
            v
        }),
        "cascade.camera" => (
            0x9575CD,
            0x5E35B1,
            vec![
                (Box(0.38, 0.25, 0.24, 0.10, 0.03), w),
                (Box(0.18, 0.31, 0.64, 0.44, 0.08), w),
                (Circle(0.5, 0.53, 0.15), hex(0x6A42BE)),
                (Circle(0.5, 0.53, 0.09), w),
                (Circle(0.72, 0.39, 0.03), hex(0x6A42BE)),
            ],
        ),
        "cascade.notepad" => (
            0x64B5F6,
            0x2F6FD0,
            vec![
                (Box(0.26, 0.18, 0.48, 0.64, 0.06), w),
                (Line(0.34, 0.34, 0.66, 0.34, 0.04), hex(0x5A9BE8)),
                (Line(0.34, 0.46, 0.66, 0.46, 0.04), hex(0x5A9BE8)),
                (Line(0.34, 0.58, 0.58, 0.58, 0.04), hex(0x5A9BE8)),
            ],
        ),
        "cascade.clock" => (
            0x7986CB,
            0x3949AB,
            vec![
                (Circle(0.5, 0.5, 0.29), w),
                (Line(0.5, 0.5, 0.5, 0.31, 0.055), hex(0x3F51B5)),
                (Line(0.5, 0.5, 0.64, 0.58, 0.055), hex(0x3F51B5)),
                (Circle(0.5, 0.5, 0.04), hex(0xFF7043)),
            ],
        ),
        "cascade.mediaplayer" => (
            0xFF8A65,
            0xE64A19,
            vec![(Circle(0.5, 0.5, 0.28), w), (Tri([(0.43, 0.37), (0.43, 0.63), (0.65, 0.5)]), hex(0xEE5A2A))],
        ),
        "cascade.soundrecorder" => (
            0xEF5350,
            0xC62828,
            vec![
                (Box(0.40, 0.18, 0.20, 0.36, 0.10), w),
                (Ring(0.5, 0.42, 0.18, 0.05, 0.0, pi), w),
                (Line(0.5, 0.62, 0.5, 0.74, 0.05), w),
                (Line(0.38, 0.76, 0.62, 0.76, 0.05), w),
            ],
        ),
        "cascade.paint" => (
            0xF06292,
            0xAB47BC,
            vec![
                (Circle(0.5, 0.5, 0.30), w),
                (Circle(0.38, 0.40, 0.055), hex(0xE53935)),
                (Circle(0.52, 0.33, 0.055), hex(0xFDD835)),
                (Circle(0.65, 0.42, 0.055), hex(0x43A047)),
                (Circle(0.62, 0.58, 0.055), hex(0x1E88E5)),
                (Circle(0.42, 0.60, 0.07), fade(hex(0xC0C0C0), 0.5)),
            ],
        ),
        "cascade.lighting" => (
            0x26C6DA,
            0x7E57C2,
            vec![
                (Circle(0.5, 0.44, 0.16), w),
                (Box(0.43, 0.58, 0.14, 0.12, 0.03), w),
                (Line(0.5, 0.16, 0.5, 0.21, 0.04), soft),
                (Line(0.25, 0.44, 0.30, 0.44, 0.04), soft),
                (Line(0.70, 0.44, 0.75, 0.44, 0.04), soft),
                (Line(0.32, 0.26, 0.36, 0.30, 0.04), soft),
                (Line(0.68, 0.26, 0.64, 0.30, 0.04), soft),
            ],
        ),
        "cascade.snip" => (
            0xEC407A,
            0x8E24AA,
            vec![
                // A selection's corners, and the scissors' rings in it.
                (Line(0.22, 0.22, 0.36, 0.22, 0.05), w),
                (Line(0.22, 0.22, 0.22, 0.36, 0.05), w),
                (Line(0.78, 0.78, 0.64, 0.78, 0.05), w),
                (Line(0.78, 0.78, 0.78, 0.64, 0.05), w),
                (Line(0.40, 0.36, 0.62, 0.62, 0.045), w),
                (Line(0.60, 0.36, 0.38, 0.62, 0.045), w),
                (Ring(0.35, 0.66, 0.06, 0.04, 0.0, 0.0), w),
                (Ring(0.65, 0.66, 0.06, 0.04, 0.0, 0.0), w),
            ],
        ),
        "cascade.settings" => (0x78909C, 0x455A64, {
            let mut v = Vec::new();
            for i in 0..8 {
                let a = i as f32 * pi / 4.0;
                let (c, s) = (a.cos(), a.sin());
                v.push((Line(0.5 + 0.17 * c, 0.5 + 0.17 * s, 0.5 + 0.27 * c, 0.5 + 0.27 * s, 0.10), w));
            }
            v.push((Circle(0.5, 0.5, 0.21), w));
            v.push((Circle(0.5, 0.5, 0.085), hex(0x5D707B)));
            v
        }),
        _ => return None,
    })
}

fn draw(id: &str, size: u32) -> Option<Canvas> {
    let (top, bottom, picture) = design(id)?;
    let mut c = Canvas::new(size.clamp(8, 512) as usize);
    // The tile, filling the icon but for a hair of room round it.
    let m = 0.06;
    let tile = Shape::Box(m, m, 1.0 - 2.0 * m, 1.0 - 2.0 * m, 0.22);
    c.fill(&tile, hex(top), Some(hex(bottom)));
    // A soft light along the top.
    c.fill(&Shape::Box(m, m, 1.0 - 2.0 * m, 0.45, 0.22), fade(WHITE, 0.10), Some(fade(WHITE, 0.0)));
    for (s, col) in &picture {
        c.fill(s, *col, None);
    }
    Some(c)
}

/// Whether `id` (an AppUserModelID such as "Cascade.Files") has an icon here.
pub fn has(id: &str) -> bool {
    design(id).is_some()
}

/// The icon of app `id`, `size` px square, as premultiplied BGRA.
pub fn pixels(id: &str, size: u32) -> Option<Vec<u8>> {
    draw(id, size).map(|c| c.bytes(true))
}

/// The icon of app `id` as a Windows icon, `size` px square (the caller destroys it).
pub fn hicon(id: &str, size: i32) -> Option<HICON> {
    let c = draw(id, size.max(8) as u32)?;
    let n = c.size as i32;
    let bytes = c.bytes(false);
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: n,
                biHeight: -n,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let colour = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() {
            let _ = DeleteObject(colour.into());
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), bits as *mut u8, bytes.len());
        // The mask is unused for a 32-bit icon with alpha, but must be there.
        let mask = CreateBitmap(n, n, 1, 1, None);
        let ii = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: colour };
        let icon = CreateIconIndirect(&ii).ok();
        let _ = DeleteObject(colour.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_has_an_icon() {
        for (id, ..) in crate::appwin::OWN_APPS {
            assert!(has(id), "{id} has no icon");
        }
        let px = pixels("Cascade.Files", 48).unwrap();
        assert_eq!(px.len(), 48 * 48 * 4);
        // The middle is the tile (opaque), the very corner is clear.
        let at = |x: usize, y: usize| px[(y * 48 + x) * 4 + 3];
        assert_eq!(at(24, 4), 255);
        assert_eq!(at(0, 0), 0);
    }
}
