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

/// A dark rounded tile with a white notch at the top and a dock bar below.
fn render(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let tile = rrect(fx, fy, 0.0, 0.0, s, s, s * 0.22);
            // Background: deep blue-black vertical gradient.
            let t = fy / s;
            let bg = [0.10 + 0.06 * (1.0 - t), 0.11 + 0.08 * (1.0 - t), 0.16 + 0.16 * (1.0 - t)];
            let notch = rrect(fx, fy, s * 0.28, -s, s * 0.72, s * 0.24, s * 0.09);
            let dock = rrect(fx, fy, s * 0.18, s * 0.70, s * 0.82, s * 0.82, s * 0.06);
            let mut c = bg;
            for (cov, col) in [(notch, [0.96, 0.97, 1.0]), (dock, [0.36, 0.60, 1.0])] {
                for i in 0..3 {
                    c[i] = c[i] * (1.0 - cov) + col[i] * cov;
                }
            }
            let a = tile;
            let i = ((y * size + x) * 4) as usize;
            // BGRA, straight alpha (icons are not premultiplied).
            px[i] = (c[2] * 255.0) as u8;
            px[i + 1] = (c[1] * 255.0) as u8;
            px[i + 2] = (c[0] * 255.0) as u8;
            px[i + 3] = (a * 255.0) as u8;
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
