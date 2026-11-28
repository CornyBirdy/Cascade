//! Icons for Cascade's own apps (Files, Calculator, Camera, Snip...), drawn
//! here rather than borrowed from Windows: each the thing itself (a folder,
//! a calculator, a camera...) in flat colours, no plate behind it and no
//! gradients, so they're told apart at a glance and look like one set.
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

    /// Cuts `shape` out of what's drawn (a hole, such as a gear's middle).
    fn cut(&mut self, shape: &Shape) {
        let n = self.size as f32;
        for y in 0..self.size {
            for x in 0..self.size {
                let (u, v) = ((x as f32 + 0.5) / n, (y as f32 + 0.5) / n);
                let cover = (0.5 - distance(shape, u, v) * n).clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.px[y * self.size + x][3] *= 1.0 - cover;
                }
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
/// An icon: its shapes, back to front, each in one flat colour.
type Design = Vec<(Shape, C)>;

fn design(id: &str) -> Option<Design> {
    use Shape::*;
    let w = WHITE;
    let pi = std::f32::consts::PI;
    Some(match id.to_ascii_lowercase().as_str() {
        // A folder: its tab and back, and the front a shade lighter.
        "cascade.files" => vec![
            (Box(0.08, 0.18, 0.38, 0.20, 0.05), hex(0xE5A00D)),
            (Box(0.08, 0.26, 0.84, 0.58, 0.06), hex(0xE5A00D)),
            (Box(0.08, 0.34, 0.84, 0.50, 0.06), hex(0xFFC83D)),
        ],
        // A picture: a frame, sky, hills and the sun.
        "cascade.photos" => vec![
            (Box(0.08, 0.16, 0.84, 0.68, 0.08), hex(0x2B7BD6)),
            (Box(0.14, 0.22, 0.72, 0.56, 0.04), hex(0xA8DAFF)),
            (Tri([(0.14, 0.78), (0.40, 0.44), (0.64, 0.78)]), hex(0x2E9E4F)),
            (Tri([(0.44, 0.78), (0.64, 0.54), (0.86, 0.78)]), hex(0x56C26F)),
            (Circle(0.68, 0.36, 0.07), hex(0xFFC83D)),
        ],
        // A pie: the whole drive, and the part that's used.
        "cascade.diskusage" => vec![
            (Ring(0.5, 0.5, 0.29, 0.20, 0.0, 0.0), hex(0xB8C4CC)),
            (Ring(0.5, 0.5, 0.29, 0.20, -pi / 2.0, pi * 0.75), hex(0x0F9D8A)),
        ],
        // A calculator: its body, display and keys.
        "cascade.calculator" => {
            let mut v = vec![
                (Box(0.18, 0.08, 0.64, 0.84, 0.08), hex(0x3C4043)),
                (Box(0.26, 0.16, 0.48, 0.16, 0.03), hex(0xA8DAFF)),
            ];
            for r in 0..3 {
                for c in 0..3 {
                    let col = if c == 2 && r == 2 { hex(0xFF9F43) } else { hex(0xC4C8CC) };
                    v.push((Box(0.26 + c as f32 * 0.17, 0.40 + r as f32 * 0.155, 0.13, 0.11, 0.025), col));
                }
            }
            v
        }
        // A camera: its body, the bump on top, the lens and a glint.
        "cascade.camera" => vec![
            (Box(0.34, 0.18, 0.32, 0.14, 0.04), hex(0x37474F)),
            (Box(0.08, 0.26, 0.84, 0.58, 0.09), hex(0x455A64)),
            (Circle(0.5, 0.55, 0.21), hex(0x90A4AE)),
            (Circle(0.5, 0.55, 0.14), hex(0x1E2A30)),
            (Circle(0.45, 0.50, 0.035), w),
            (Circle(0.79, 0.37, 0.035), hex(0xFFC83D)),
        ],
        // A page with a blue top and lines of writing.
        "cascade.notepad" => vec![
            (Box(0.18, 0.08, 0.64, 0.84, 0.07), hex(0xF2F4F7)),
            (Box(0.18, 0.08, 0.64, 0.16, 0.07), hex(0x3B82F6)),
            (Box(0.18, 0.18, 0.64, 0.06, 0.0), hex(0x3B82F6)),
            (Line(0.30, 0.40, 0.70, 0.40, 0.045), hex(0x9AA3AE)),
            (Line(0.30, 0.53, 0.70, 0.53, 0.045), hex(0x9AA3AE)),
            (Line(0.30, 0.66, 0.58, 0.66, 0.045), hex(0x9AA3AE)),
        ],
        // A clock face with its hands.
        "cascade.clock" => vec![
            (Circle(0.5, 0.5, 0.40), hex(0x4F5BD5)),
            (Circle(0.5, 0.5, 0.33), w),
            (Line(0.5, 0.5, 0.5, 0.27, 0.06), hex(0x263238)),
            (Line(0.5, 0.5, 0.66, 0.6, 0.06), hex(0x263238)),
            (Circle(0.5, 0.5, 0.045), hex(0xE53935)),
        ],
        // A round play button.
        "cascade.mediaplayer" => {
            vec![(Circle(0.5, 0.5, 0.40), hex(0xF4511E)), (Tri([(0.42, 0.33), (0.42, 0.67), (0.70, 0.5)]), w)]
        }
        // A microphone on its stand.
        "cascade.soundrecorder" => vec![
            (Box(0.37, 0.10, 0.26, 0.46, 0.13), hex(0xE53935)),
            (Ring(0.5, 0.40, 0.22, 0.06, 0.0, pi), hex(0x9AA3AE)),
            (Line(0.5, 0.62, 0.5, 0.80, 0.06), hex(0x9AA3AE)),
            (Line(0.34, 0.84, 0.66, 0.84, 0.06), hex(0x9AA3AE)),
        ],
        // A paintbrush mid-stroke: a swoosh of colour and the brush that made it.
        "cascade.paint" => vec![
            (Ring(0.5, 0.52, 0.32, 0.13, 0.14 * pi, 0.44 * pi), hex(0xE53935)),
            (Ring(0.5, 0.52, 0.32, 0.13, 0.40 * pi, 0.70 * pi), hex(0xFDD835)),
            (Ring(0.5, 0.52, 0.32, 0.13, 0.66 * pi, 0.88 * pi), hex(0x1E88E5)),
            (Line(0.84, 0.10, 0.54, 0.44, 0.10), hex(0xA0714F)),
            (Line(0.56, 0.42, 0.45, 0.55, 0.11), hex(0xB0BEC5)),
            (Line(0.45, 0.55, 0.37, 0.64, 0.12), hex(0x43A047)),
        ],
        // A light bulb, lit.
        "cascade.lighting" => vec![
            (Circle(0.5, 0.42, 0.24), hex(0xFFC83D)),
            (Box(0.40, 0.58, 0.20, 0.16, 0.03), hex(0xFFC83D)),
            (Box(0.39, 0.72, 0.22, 0.12, 0.04), hex(0x8D99A3)),
            (Line(0.5, 0.06, 0.5, 0.11, 0.045), hex(0xFFC83D)),
            (Line(0.12, 0.42, 0.18, 0.42, 0.045), hex(0xFFC83D)),
            (Line(0.82, 0.42, 0.88, 0.42, 0.045), hex(0xFFC83D)),
            (Line(0.22, 0.16, 0.26, 0.20, 0.045), hex(0xFFC83D)),
            (Line(0.78, 0.16, 0.74, 0.20, 0.045), hex(0xFFC83D)),
        ],
        // Scissors cutting out a selection.
        "cascade.snip" => vec![
            (Line(0.10, 0.10, 0.30, 0.10, 0.05), hex(0x3B82F6)),
            (Line(0.10, 0.10, 0.10, 0.30, 0.05), hex(0x3B82F6)),
            (Line(0.90, 0.90, 0.70, 0.90, 0.05), hex(0x3B82F6)),
            (Line(0.90, 0.90, 0.90, 0.70, 0.05), hex(0x3B82F6)),
            (Line(0.36, 0.24, 0.62, 0.62, 0.055), hex(0x90A4AE)),
            (Line(0.64, 0.24, 0.38, 0.62, 0.055), hex(0x90A4AE)),
            (Ring(0.32, 0.70, 0.09, 0.055, 0.0, 0.0), hex(0xE53935)),
            (Ring(0.68, 0.70, 0.09, 0.055, 0.0, 0.0), hex(0xE53935)),
        ],
        // Sharing: three dots joined, on a blue disc.
        "cascade.quickshare" => vec![
            (Circle(0.5, 0.5, 0.42), hex(0x1A73E8)),
            (Line(0.36, 0.5, 0.64, 0.33, 0.05), w),
            (Line(0.36, 0.5, 0.64, 0.67, 0.05), w),
            (Circle(0.36, 0.5, 0.09), w),
            (Circle(0.64, 0.33, 0.09), w),
            (Circle(0.64, 0.67, 0.09), w),
        ],
        // A gear.
        "cascade.settings" => {
            let mut v = Vec::new();
            for i in 0..8 {
                let a = i as f32 * pi / 4.0;
                let (c, s) = (a.cos(), a.sin());
                v.push((Line(0.5 + 0.22 * c, 0.5 + 0.22 * s, 0.5 + 0.34 * c, 0.5 + 0.34 * s, 0.13), hex(0x6B7780)));
            }
            v.push((Circle(0.5, 0.5, 0.27), hex(0x6B7780)));
            v.push((Circle(0.5, 0.5, 0.11), C(0.0, 0.0, 0.0, 0.0)));
            v
        }
        _ => return None,
    })
}

fn draw(id: &str, size: u32) -> Option<Canvas> {
    let picture = design(id)?;
    let mut c = Canvas::new(size.clamp(8, 512) as usize);
    for (s, col) in &picture {
        if col.3 == 0.0 {
            // A hole: cut through what's drawn so far.
            c.cut(s);
        } else {
            c.fill(s, *col, None);
        }
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
        // The folder's middle is solid, the corners round it clear (no plate).
        let at = |x: usize, y: usize| px[(y * 48 + x) * 4 + 3];
        assert_eq!(at(24, 30), 255);
        assert_eq!(at(0, 0), 0);
        assert_eq!(at(47, 47), 0);
    }
}
