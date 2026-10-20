//! Generates the app icon (the Rack Suite tile; no image files in the repo)
//! and embeds it as resource 1, so Explorer, the Start menu and the tray all
//! show it.

use std::path::PathBuf;

/// Signed distance (negative inside) to a rounded rect.
fn rrect_sd(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    let inside = x > x0 + r && x < x1 - r && y > y0 + r && y < y1 - r;
    if inside {
        // Deep inside: the distance to the nearest straight edge.
        -(x - x0).min(x1 - x).min(y - y0).min(y1 - y)
    } else {
        ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r
    }
}

/// Composites `col` at `alpha` over a premultiplied (colour, alpha) pixel.
fn over(c: &mut [f32; 3], a: &mut f32, col: [f32; 3], alpha: f32) {
    for (v, k) in c.iter_mut().zip(col) {
        *v = *v * (1.0 - alpha) + k * alpha;
    }
    *a = *a * (1.0 - alpha) + alpha;
}

/// The Cascade mark in the Rack Suite style (the same tile as Rack, MC
/// Hoster and Shelf): on a dark rounded tile, a teal screen outline with the
/// notch hanging from its top edge and a taskbar along its bottom. Drawn on
/// a 512-unit grid.
fn render(size: u32) -> Vec<u8> {
    let k = 512.0 / size as f32; // grid units per pixel
    let hex = |h: u32| [(h >> 16 & 255) as f32 / 255.0, (h >> 8 & 255) as f32 / 255.0, (h & 255) as f32 / 255.0];
    let (tile, mark) = (hex(0x262830), hex(0x1FB8B1));
    let mut px = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let u = (x as f32 + 0.5) * k;
            let v = (y as f32 + 0.5) * k;
            // Coverage from a signed distance, anti-aliased over one pixel.
            let cov = |sd: f32| (0.5 - sd / k).clamp(0.0, 1.0);
            let (mut c, mut a) = ([0.0f32; 3], 0.0f32);
            over(&mut c, &mut a, tile, cov(rrect_sd(u, v, 0.0, 0.0, 512.0, 512.0, 112.0)));
            // The screen: a 40-unit ring round 112..400 x 122..390.
            let outer = rrect_sd(u, v, 92.0, 102.0, 420.0, 410.0, 64.0);
            let inner = rrect_sd(u, v, 132.0, 142.0, 380.0, 370.0, 24.0);
            let ring = outer.max(-inner);
            let notch = rrect_sd(u, v, 196.0, 110.0, 316.0, 170.0, 30.0);
            let bar = rrect_sd(u, v, 168.0, 304.0, 344.0, 340.0, 18.0);
            over(&mut c, &mut a, mark, cov(ring.min(notch).min(bar)));
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
