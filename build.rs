//! Generates the app icon (the glass-C mark; no image files in the repo) and embeds it as
//! resource 1, so Explorer, the Start menu and the tray all show it.

use std::path::PathBuf;

/// Signed distance (negative inside) to a rounded rect.
fn rrect_sd(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r
}

/// Distance to the segment (ax, ay)-(bx, by).
fn seg(x: f32, y: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((x - ax - t * dx).powi(2) + (y - ay - t * dy).powi(2)).sqrt()
}

/// Distance to the centre line of the C (a 120-unit design grid): a top bar,
/// a left-hand arc and a short bottom bar.
fn c_line(u: f32, v: f32) -> f32 {
    let top = seg(u, v, 56.0, 22.0, 78.0, 22.0);
    let bottom = seg(u, v, 56.0, 98.0, 60.0, 98.0);
    let arc = if u < 56.0 { ((u - 56.0).hypot(v - 60.0) - 38.0).abs() } else { f32::MAX };
    top.min(bottom).min(arc)
}

/// Composites `col` at `alpha` over a premultiplied (colour, alpha) pixel.
fn over(c: &mut [f32; 3], a: &mut f32, col: [f32; 3], alpha: f32) {
    for (v, k) in c.iter_mut().zip(col) {
        *v = *v * (1.0 - alpha) + k * alpha;
    }
    *a = *a * (1.0 - alpha) + alpha;
}

/// The Cascade mark on a transparent background: a glass letter C (a
/// translucent white rim around a soft orange / pink / violet gradient), a
/// detached glass pill like a taskbar below it, and an orange pebble on top.
fn render(size: u32) -> Vec<u8> {
    let s = size as f32;
    // The design grid is 120 units; the mark fills x 11..113, y 11..105.
    let span = 112.0;
    let k = span / s;
    let px_u = k; // grid units per pixel
    let hex = |h: u32| [(h >> 16 & 255) as f32 / 255.0, (h >> 8 & 255) as f32 / 255.0, (h & 255) as f32 / 255.0];
    // Blurred colour blobs behind the glass: (x, y, radius, colour).
    let orbs = [
        (5.8, 5.8, 50.0, hex(0xFF6A3D)),
        (120.4, 20.4, 54.0, hex(0xFF4FA3)),
        (60.0, 126.7, 62.5, hex(0x7A3CFF)),
        (130.8, 143.3, 37.5, hex(0xFFB020)),
    ];
    let blur = 9.0;
    let mut px = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let u = 6.0 + (x as f32 + 0.5) * k;
            let v = 2.0 + (y as f32 + 0.5) * k;
            // Coverage from a signed distance, anti-aliased over one pixel.
            let cov = |sd: f32| (0.5 - sd / px_u).clamp(0.0, 1.0);
            let cl = c_line(u, v);
            let outer = (cl - 7.0).min(rrect_sd(u, v, 74.0, 91.0, 113.0, 105.0, 7.0));
            let inner = (cl - 6.0).min(rrect_sd(u, v, 75.0, 92.0, 112.0, 104.0, 6.0));
            let (mut c, mut a) = ([0.0f32; 3], 0.0f32);
            // Glass rim, then the tinted body inside it.
            over(&mut c, &mut a, [1.0; 3], 0.6 * cov(outer));
            let ci = cov(inner);
            if ci > 0.0 {
                let mut body = hex(0x0E0B14);
                for (ox, oy, r, col) in orbs {
                    let d = (u - ox).hypot(v - oy) - r;
                    let t = 1.0 / (1.0 + (d / (blur * 0.55)).exp());
                    for (b, k) in body.iter_mut().zip(col) {
                        *b = *b * (1.0 - t) + k * t;
                    }
                }
                for b in &mut body {
                    *b = *b * 0.8 + 0.2;
                }
                // The body is opaque glass: it replaces the rim underneath.
                for (v, b) in c.iter_mut().zip(body) {
                    *v = *v * (1.0 - ci) + b * ci;
                }
                a = a * (1.0 - ci) + ci;
            }
            // The orange pebble.
            over(&mut c, &mut a, hex(0xFF9A1F), cov((u - 101.2).hypot(v - 21.8) - 10.6));
            let i = ((y * size + x) * 4) as usize;
            let un = |v: f32| if a > 0.0 { (v / a * 255.0).round().clamp(0.0, 255.0) as u8 } else { 0 };
            // BGRA, straight alpha (icons are not premultiplied).
            px[i] = un(c[2]);
            px[i + 1] = un(c[1]);
            px[i + 2] = un(c[0]);
            px[i + 3] = (a * 255.0).round() as u8;
        }
    }
    px
}

fn ico(sizes: &[u32]) -> Vec<u8> {
    let mut images = Vec::new();
    for &s in sizes {
        let px = render(s);
        let mut img = Vec::new();
        // BITMAPINFOHEADER; height counts the colour and mask halves.
        for v in [40u32, s, s * 2] {
            img.extend_from_slice(&v.to_le_bytes());
        }
        img.extend_from_slice(&1u16.to_le_bytes());
        img.extend_from_slice(&32u16.to_le_bytes());
        for _ in 0..6 {
            img.extend_from_slice(&0u32.to_le_bytes());
        }
        // Pixel rows bottom-up.
        for y in (0..s).rev() {
            let row = (y * s * 4) as usize;
            img.extend_from_slice(&px[row..row + (s * 4) as usize]);
        }
        // AND mask (unused with 32-bit alpha), rows padded to 4 bytes.
        let mask_row = s.div_ceil(32) * 4;
        img.extend(std::iter::repeat_n(0u8, (mask_row * s) as usize));
        images.push((s, img));
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (s, img) in &images {
        let dim = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for (_, img) in images {
        out.extend_from_slice(&img);
    }
    out
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CASCADE_BUILD");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let icon = out.join("cascade.ico");
    std::fs::write(&icon, ico(&[16, 20, 24, 32, 40, 48, 64, 256])).unwrap();
    let rc = out.join("cascade.rc");
    let path = icon.display().to_string().replace('\\', "\\\\");
    std::fs::write(&rc, format!("1 ICON \"{path}\"\n")).unwrap();
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
}
