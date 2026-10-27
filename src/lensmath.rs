//! The arithmetic behind the refraction ring (see `lens`), kept apart from
//! the Win32 calls so it can be tested on its own.
//!
//! Where each ring pixel reads its light from, and how it's blended, depends
//! only on the ring's size and on which part of its surroundings is on a
//! screen: never on what's behind it. So that is worked out once into a
//! `Mesh` (a list of cells), and drawing the ring after what's behind it
//! changed is only reading and blending the pixels each cell points at.

/// What the ring is made of, sizes in px.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Geometry {
    /// The ring's own size, and the corner radius of the pane it belongs to.
    pub w: i32,
    pub h: i32,
    pub radius: f32,
    /// How far in from the edge the glass is curved, how far the light is
    /// bent at the very edge, and how much more red bends than blue.
    pub band: f32,
    pub bend: f32,
    pub split: f32,
    /// The capture of the surroundings: `margin` px beyond the ring on each
    /// side, `cw` x `ch` px in all.
    pub margin: i32,
    pub cw: i32,
    pub ch: i32,
    /// The part of the capture that is on a screen (left, top, right,
    /// bottom), where light can be read from.
    pub valid: (f32, f32, f32, f32),
}

/// One ring pixel: where it reads each colour from and how it's blended.
#[derive(Clone, Copy)]
pub struct Cell {
    /// The pixel's index in the ring.
    at: u32,
    /// For blue, green and red: the byte of the top-left source pixel (in
    /// the capture) and how far along the pixel and the row under it are
    /// (bit 0 / bit 1 of `step`, counted per colour in pairs), then how far
    /// between its neighbours the point falls, 0 to 255.
    from: [u32; 3],
    fx: [u8; 3],
    fy: [u8; 3],
    step: u8,
    /// Light caught in the curve, added to each colour (0 to 255).
    glint: u8,
    /// How much of the bent light shows, and the glint left on its own
    /// where there is no light to bend in.
    cover: u8,
    lone: u8,
    alpha: u8,
}

pub struct Mesh {
    pub geometry: Geometry,
    cells: Vec<Cell>,
}

/// Signed distance from (x, y) to the edge of a w x h rounded rect with
/// corners of radius r (negative inside), and the outward normal there.
pub fn edge(x: f32, y: f32, w: f32, h: f32, r: f32) -> (f32, f32, f32) {
    let (px, py) = (x - w / 2.0, y - h / 2.0);
    let (qx, qy) = (px.abs() - (w / 2.0 - r), py.abs() - (h / 2.0 - r));
    let (sx, sy) = (px.signum(), py.signum());
    if qx > 0.0 && qy > 0.0 {
        let len = qx.hypot(qy).max(1e-4);
        (len - r, sx * qx / len, sy * qy / len)
    } else if qx > qy {
        (qx - r, sx, 0.0)
    } else {
        (qy - r, 0.0, sy)
    }
}

/// Where to read one colour: the bytes of the four pixels around the point
/// (x, y) in the capture, and how far between them it falls. The point is
/// kept inside `valid`, the part of the capture that is on a screen.
fn tap(g: &Geometry, x: f32, y: f32, channel: usize) -> (u32, u8, u8, u8) {
    let stride = g.cw as usize * 4;
    let x = x.clamp(g.valid.0, g.valid.2) - 0.5;
    let y = y.clamp(g.valid.1, g.valid.3) - 0.5;
    let (x0, y0) = (x.floor().max(0.0), y.floor().max(0.0));
    let (fx, fy) = ((x - x0).clamp(0.0, 1.0), (y - y0).clamp(0.0, 1.0));
    let (ix, iy) = (x0 as usize, y0 as usize);
    let max_x = (g.valid.2 - 1.0).max(0.0) as usize;
    let max_y = (g.valid.3 - 1.0).max(0.0) as usize;
    let (bx, by) = (ix.min(max_x), iy.min(max_y));
    let right = (ix + 1).min(max_x) != bx;
    let down = (iy + 1).min(max_y) != by;
    let from = (by * stride + bx * 4 + channel) as u32;
    (from, (fx * 255.0).round() as u8, (fy * 255.0).round() as u8, right as u8 | (down as u8) << 1)
}

fn byte(v: f32) -> u8 {
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Works out the whole ring: a cell for each pixel the glass bends light into.
pub fn mesh(g: &Geometry) -> Mesh {
    let (w, h) = (g.w, g.h);
    let (fw, fh) = (w as f32, h as f32);
    let r = g.radius.min(fw.min(fh) / 2.0);
    let band = g.band;
    let bend = band * (g.bend);
    // Rows clear of the curved corners and the top and bottom bands only
    // have a band at each end.
    let reach = (band + r).ceil() as i32 + 1;
    let mut cells = Vec::new();
    for y in 0..h {
        let mid = y >= reach && y < h - reach;
        let spans: [(i32, i32); 2] = if mid { [(0, reach.min(w)), ((w - reach).max(0), w)] } else { [(0, w), (w, w)] };
        for (x0, x1) in spans {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let (d, nx, ny) = edge(px, py, fw, fh, r);
                let inside = -d;
                if inside < -0.5 || inside >= band {
                    continue;
                }
                // 1 at the edge, 0 where the glass is flat again.
                let t = (1.0 - inside.max(0.0) / band).clamp(0.0, 1.0);
                // How far outside the edge this pixel's light comes from:
                // furthest at the edge, right at it where the glass is flat.
                let out = bend * t * t;
                // Out to the edge first, then a pixel clear of it.
                let to_edge = inside.max(0.0) + 1.0;
                // Each colour from a little further along the bend than the next.
                let (sx, sy) = (px + g.margin as f32, py + g.margin as f32);
                let mut cell = Cell {
                    at: (y * w + x) as u32,
                    from: [0; 3],
                    fx: [0; 3],
                    fy: [0; 3],
                    step: 0,
                    glint: 0,
                    cover: 0,
                    lone: 0,
                    alpha: 0,
                };
                // Blue, green, red: the further along the bend, the redder.
                for (c, k) in [1.0 - g.split, 1.0, 1.0 + g.split].into_iter().enumerate() {
                    let d = to_edge + out * k;
                    let (from, fx, fy, step) = tap(g, sx + nx * d, sy + ny * d, c);
                    cell.from[c] = from;
                    cell.fx[c] = fx;
                    cell.fy[c] = fy;
                    cell.step |= step << (2 * c);
                }
                // Off the screen there's nothing to bend in (the panel is
                // against its edge there): fade out over a couple of pixels.
                let far = to_edge + out * (1.0 + g.split);
                let (rx, ry) = (sx + nx * far, sy + ny * far);
                let off = (g.valid.0 - rx).max(rx - g.valid.2).max(g.valid.1 - ry).max(ry - g.valid.3);
                let seen = (1.0 - off.max(0.0) / 2.0).clamp(0.0, 1.0);
                // Light caught in the curve, most along its lit (upper) part:
                // what shows where the edge is over a plain background.
                let glint = 0.16 * t * t * t * (0.55 - 0.45 * ny);
                let cover = (inside + 0.5).clamp(0.0, 1.0);
                // Fade into the flat glass, where the ring shows what's behind as it is.
                let fade = ((band - inside) / (band * 0.25)).clamp(0.0, 1.0);
                let a = cover * fade * seen;
                // Where the bent light faded, the glint is left on its own.
                let lone = cover * glint * (1.0 - seen);
                cell.glint = byte(glint);
                cell.cover = byte(a);
                cell.lone = byte(lone * (1.0 - a));
                cell.alpha = byte(a + lone * (1.0 - a));
                if cell.cover != 0 || cell.lone != 0 || cell.alpha != 0 {
                    cells.push(cell);
                }
            }
        }
    }
    Mesh { geometry: *g, cells }
}

/// Draws the ring (`w` x `h` BGRA, premultiplied) from the capture `src`
/// of its surroundings. Only the cells' pixels are written: the rest are
/// left as they are, which is clear in a ring that was cleared first.
pub fn compose(mesh: &Mesh, src: &[u8], dst: &mut [u8]) {
    let stride = mesh.geometry.cw as usize * 4;
    for cell in &mesh.cells {
        let mut rgb = [0u8; 3];
        for (c, out) in rgb.iter_mut().enumerate() {
            let i = cell.from[c] as usize;
            let right = if cell.step >> (2 * c) & 1 != 0 { 4 } else { 0 };
            let down = if cell.step >> (2 * c + 1) & 1 != 0 { stride } else { 0 };
            let (fx, fy) = (cell.fx[c] as u32, cell.fy[c] as u32);
            let top = src[i] as u32 * (255 - fx) + src[i + right] as u32 * fx;
            let bottom = src[i + down] as u32 * (255 - fx) + src[i + down + right] as u32 * fx;
            let v = (top * (255 - fy) + bottom * fy + 32512) / 65025;
            // The bent light, with the glint on it, over what's under it.
            let lit = (v + cell.glint as u32).min(255);
            *out = ((lit * cell.cover as u32 + 127) / 255 + cell.lone as u32).min(255) as u8;
        }
        let o = cell.at as usize * 4;
        dst[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], cell.alpha]);
    }
}

/// How far in from the edge the light-catching glint of the plain ring is
/// drawn, in px.
const GLINT_BAND: f32 = 3.0;

/// The ring with no bent light, only the glint along its edge, which doesn't
/// depend on anything behind the pane. Cheap enough to redraw every frame
/// while a pane moves or changes size; clears `dst` first.
pub fn glint_only(w: i32, h: i32, radius: f32, dst: &mut [u8]) {
    dst.fill(0);
    let (fw, fh) = (w as f32, h as f32);
    let r = radius.min(fw.min(fh) / 2.0);
    let reach = (GLINT_BAND + r).ceil() as i32 + 1;
    for y in 0..h {
        let mid = y >= reach && y < h - reach;
        let spans: [(i32, i32); 2] = if mid { [(0, reach.min(w)), ((w - reach).max(0), w)] } else { [(0, w), (w, w)] };
        for (x0, x1) in spans {
            for x in x0..x1 {
                let (d, _, ny) = edge(x as f32 + 0.5, y as f32 + 0.5, fw, fh, r);
                let inside = -d;
                if inside < -0.5 || inside >= GLINT_BAND {
                    continue;
                }
                let t = (1.0 - inside.max(0.0) / GLINT_BAND).clamp(0.0, 1.0);
                // The same light as the full ring's, over a band of its own.
                let glint = 0.16 * t * t * t * (0.55 - 0.45 * ny);
                let v = byte((inside + 0.5).clamp(0.0, 1.0) * glint);
                let o = (y * w + x) as usize * 4;
                dst[o..o + 4].copy_from_slice(&[v, v, v, v]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ring as it was drawn before the mesh: every pixel worked out
    /// from scratch, blending in floats. The mesh has to look the same.
    fn reference(g: &Geometry, src: &[u8], dst: &mut [u8]) {
        let (w, h) = (g.w, g.h);
        let stride = g.cw as usize * 4;
        let (fw, fh) = (w as f32, h as f32);
        let r = g.radius.min(fw.min(fh) / 2.0);
        let band = g.band;
        let bend = band * g.bend;
        let split = g.split;
        let margin = g.margin as f32;
        let valid = g.valid;
        let sample = |x: f32, y: f32, channel: usize| -> f32 {
            let x = x.clamp(valid.0, valid.2) - 0.5;
            let y = y.clamp(valid.1, valid.3) - 0.5;
            let (x0, y0) = (x.floor().max(0.0), y.floor().max(0.0));
            let (fx, fy) = ((x - x0).clamp(0.0, 1.0), (y - y0).clamp(0.0, 1.0));
            let (ix, iy) = (x0 as usize, y0 as usize);
            let max_x = (valid.2 - 1.0).max(0.0) as usize;
            let max_y = (valid.3 - 1.0).max(0.0) as usize;
            let at = |x: usize, y: usize| src[y.min(max_y) * stride + x.min(max_x) * 4 + channel] as f32;
            let top = at(ix, iy) + (at(ix + 1, iy) - at(ix, iy)) * fx;
            let bottom = at(ix, iy + 1) + (at(ix + 1, iy + 1) - at(ix, iy + 1)) * fx;
            top + (bottom - top) * fy
        };
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let (d, nx, ny) = edge(fx, fy, fw, fh, r);
                let inside = -d;
                let o = (y * w + x) as usize * 4;
                if inside < -0.5 || inside >= band {
                    dst[o..o + 4].copy_from_slice(&[0, 0, 0, 0]);
                    continue;
                }
                let t = (1.0 - inside.max(0.0) / band).clamp(0.0, 1.0);
                let out = bend * t * t;
                let to_edge = inside.max(0.0) + 1.0;
                let (sx, sy) = (fx + margin, fy + margin);
                let at = |k: f32, channel: usize| {
                    let d = to_edge + out * k;
                    sample(sx + nx * d, sy + ny * d, channel)
                };
                let (red, green, blue) = (at(1.0 + split, 2), at(1.0, 1), at(1.0 - split, 0));
                let far = to_edge + out * (1.0 + split);
                let (rx, ry) = (sx + nx * far, sy + ny * far);
                let off = (valid.0 - rx).max(rx - valid.2).max(valid.1 - ry).max(ry - valid.3);
                let seen = (1.0 - off.max(0.0) / 2.0).clamp(0.0, 1.0);
                let glint = 0.16 * t * t * t * (0.55 - 0.45 * ny);
                let cover = (inside + 0.5).clamp(0.0, 1.0);
                let fade = ((band - inside) / (band * 0.25)).clamp(0.0, 1.0);
                let a = cover * fade * seen;
                let lone = cover * glint * (1.0 - seen);
                let alpha = a + lone * (1.0 - a);
                let px = |c: f32| ((c + glint * 255.0).min(255.0) * a + 255.0 * lone * (1.0 - a)).round() as u8;
                dst[o..o + 4].copy_from_slice(&[px(blue), px(green), px(red), (alpha * 255.0).round() as u8]);
            }
        }
    }

    /// A noisy capture, so a pixel read from the wrong place shows.
    fn capture(cw: i32, ch: i32) -> Vec<u8> {
        let mut s = 0x2545_f491_4f6c_dd1du64;
        (0..cw * ch * 4)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect()
    }

    fn geometry(w: i32, h: i32, radius: f32, valid: Option<(f32, f32, f32, f32)>) -> Geometry {
        let (band, bend, split) = (16.0f32, 0.8f32, 0.12f32);
        let margin = (band * bend * (1.0 + split)).ceil() as i32 + 3;
        let (cw, ch) = (w + 2 * margin, h + 2 * margin);
        let valid = valid.unwrap_or((0.5, 0.5, cw as f32 - 0.5, ch as f32 - 0.5));
        Geometry { w, h, radius, band, bend, split, margin, cw, ch, valid }
    }

    fn compare(g: &Geometry) -> (i32, usize) {
        let src = capture(g.cw, g.ch);
        let mut want = vec![0u8; (g.w * g.h * 4) as usize];
        reference(g, &src, &mut want);
        let mut got = vec![0u8; want.len()];
        compose(&mesh(g), &src, &mut got);
        let worst = want.iter().zip(&got).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        let drawn = want.chunks(4).filter(|p| p.iter().any(|&b| b != 0)).count();
        (worst, drawn)
    }

    #[test]
    fn edge_distance_and_normal() {
        // The middle of the top edge of a 100 x 40 rect: on the edge, normal up.
        let (d, nx, ny) = edge(50.0, 0.0, 100.0, 40.0, 8.0);
        assert!(d.abs() < 1e-4 && nx == 0.0 && ny == -1.0);
        // 5 px in from the left edge.
        let (d, nx, _) = edge(5.0, 20.0, 100.0, 40.0, 8.0);
        assert!((d + 5.0).abs() < 1e-4 && nx == -1.0);
        // The very corner of the box is outside the rounded corner.
        let (d, nx, ny) = edge(0.0, 0.0, 100.0, 40.0, 8.0);
        assert!(d > 0.0 && nx < 0.0 && ny < 0.0);
    }

    #[test]
    fn mesh_draws_the_same_ring_as_working_every_pixel_out() {
        for &(w, h, radius) in &[(300, 48, 8.0), (96, 96, 12.0), (600, 700, 8.0), (40, 24, 8.0), (30, 90, 30.0)] {
            let (worst, drawn) = compare(&geometry(w, h, radius, None));
            assert!(drawn > 0, "{w}x{h}: nothing drawn");
            assert!(worst <= 2, "{w}x{h}: a pixel is {worst} off");
        }
    }

    #[test]
    fn mesh_matches_where_the_ring_is_against_the_screen_edge() {
        // The bottom and left of the capture are off the screen, as for a taskbar.
        let base = geometry(400, 48, 8.0, None);
        let valid = (base.margin as f32 + 0.5, 0.5, base.cw as f32 - 0.5, (base.ch - base.margin) as f32 - 0.5);
        let (worst, drawn) = compare(&geometry(400, 48, 8.0, Some(valid)));
        assert!(drawn > 0 && worst <= 2, "a pixel is {worst} off");
    }

    #[test]
    fn the_edge_glints_over_a_flat_background() {
        let g = geometry(200, 60, 8.0, None);
        let src = vec![128u8; (g.cw * g.ch * 4) as usize];
        let mut ring = vec![0u8; (g.w * g.h * 4) as usize];
        compose(&mesh(&g), &src, &mut ring);
        // On the top edge the ring shows the same grey, with light on it, at some alpha.
        let top = &ring[((g.w / 2) * 4) as usize..][..4];
        assert!(top[3] > 0, "the edge is drawn");
        // The middle of the pane is never touched.
        let mid = &ring[(((g.h / 2) * g.w + g.w / 2) * 4) as usize..][..4];
        assert_eq!(mid, &[0, 0, 0, 0]);
    }

    #[test]
    fn glint_only_leaves_the_middle_clear_and_a_thin_lit_edge() {
        let (w, h) = (120, 40);
        let mut ring = vec![9u8; (w * h * 4) as usize];
        glint_only(w, h, 8.0, &mut ring);
        let at = |x: i32, y: i32| ring[((y * w + x) * 4) as usize..][..4].to_vec();
        assert_eq!(at(w / 2, h / 2), vec![0, 0, 0, 0]);
        assert!(at(w / 2, 0)[3] > 0);
        // Only a few px deep.
        assert_eq!(at(w / 2, 6), vec![0, 0, 0, 0]);
    }
}
