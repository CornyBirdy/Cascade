//! Generates the app icon (no image files in the repo) and embeds it as
//! resource 1, so Explorer, the Start menu and the tray all show it.

use std::path::PathBuf;

/// Coverage (0..1) of a rounded rect at pixel centre (x, y), anti-aliased.
fn rrect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r;
    (0.5 - d).clamp(0.0, 1.0)
}

/// Three rounded windows, cascaded like Windows' "Cascade windows", each
/// cut out from the one behind it, deep blue at the back to light at the front.
fn render(size: u32) -> Vec<u8> {
    let s = size as f32;
    let side = s * 0.56;
    let radius = s * 0.13;
    let gap = (s * 0.055).max(1.0);
    let layers = [(0.0, [0.11, 0.27, 0.80]), (0.22, [0.20, 0.50, 1.00]), (0.44, [0.56, 0.84, 1.00])];
    let mut px = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Premultiplied colour and alpha, composed back to front.
            let (mut c, mut a) = ([0.0f32; 3], 0.0f32);
            for (n, (off, col)) in layers.iter().enumerate() {
                let (x0, y0) = (s * off, s * off);
                let (x1, y1) = (x0 + side, y0 + side);
                if n > 0 {
                    let cut = rrect(fx, fy, x0 - gap, y0 - gap, x1 + gap, y1 + gap, radius + gap);
                    a *= 1.0 - cut;
                    for v in &mut c {
                        *v *= 1.0 - cut;
                    }
                }
                let cov = rrect(fx, fy, x0, y0, x1, y1, radius);
                // A little lighter at the top of each window.
                let shade = 1.08 - 0.16 * ((fy - y0) / side).clamp(0.0, 1.0);
                for (v, k) in c.iter_mut().zip(col) {
                    *v = *v * (1.0 - cov) + (k * shade).min(1.0) * cov;
                }
                a = a * (1.0 - cov) + cov;
            }
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
