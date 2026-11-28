//! The optics behind Liquid Glass on the GPU (see `glass`), kept apart from
//! Direct3D so it can be tested on its own.
//!
//! A pane is a slab of glass whose edge is rounded over, like the edge of a
//! thick lens: flat in the middle, curving down to nothing at the rim over
//! a `band` of px. The curve is a squircle, `(1 - (1 - x)^4)^(1/4)` with `x`
//! running from 0 at the rim to 1 where the glass turns flat (the profile
//! Apple's glass is built on: it starts steep and eases into the flat part,
//! so there is no crease where the two meet).
//!
//! Looking straight down through it, a ray meets the curved top at a slant,
//! is bent towards the middle of the pane as it enters (Snell's law, glass
//! at 1.5), and reaches what's behind the pane a little further in than
//! where it went in. So each pixel near the edge shows what's behind a
//! point further in: what's under the rim is squeezed into a fine band and
//! what's just inside it swells out towards the edge, which is what makes a
//! lens look like one. Nothing comes from outside the pane (that would be a
//! concave edge).
//!
//! How far in, for each `x`, is worked out once here into a small table
//! (`profile`) that the shader reads; the shape of the pane, its corners
//! and the direction to bend in come from a signed distance to its rounded
//! rect, worked out per pixel on the GPU (`shape_sd`, `bend_dir`).

/// Entries in the bend table: `x` from 0 to 1 in equal steps.
pub const LUT_LEN: usize = 64;

/// How thick the glass is, against how wide its curved edge is (1 = as
/// thick as the edge is wide). Sets how strongly the edge bends, before the
/// theme scales it.
pub const THICKNESS: f32 = 1.0;
/// Glass.
const IOR: f32 = 1.5;

/// Height of the glass's top at `x` (0 at the rim, 1 where it's flat), as a
/// share of its thickness.
pub fn height(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    (1.0 - (1.0 - x).powi(4)).max(0.0).powf(0.25)
}

/// How steeply the top rises at `x` (d height / d x): endless at the rim,
/// 0 where the glass is flat.
pub fn slope(x: f32) -> f32 {
    let x = x.clamp(1e-4, 1.0);
    let u = 1.0 - x;
    u.powi(3) * (1.0 - u.powi(4)).max(1e-6).powf(-0.75)
}

/// How far (as a share of the curved band's width) a ray coming straight
/// down at `x` lands from where it went in, towards the middle of the pane.
fn landing(x: f32) -> f32 {
    let tilt = (THICKNESS * slope(x)).atan();
    let bent = (tilt.sin() / IOR).asin();
    THICKNESS * height(x) * (tilt - bent).tan()
}

/// The bend table: how far each `x` reads from, with the furthest (just in
/// from the rim) as 1, so the theme's `bend` is how far the edge bends.
pub fn profile() -> [f32; LUT_LEN] {
    let mut t = [0.0f32; LUT_LEN];
    for (i, v) in t.iter_mut().enumerate() {
        *v = landing(i as f32 / (LUT_LEN - 1) as f32);
    }
    let peak = t.iter().cloned().fold(0.0f32, f32::max).max(1e-6);
    for v in &mut t {
        *v /= peak;
    }
    t
}

// The rest mirrors the shader on the CPU, for the tests.

/// Reads the bend table at `x`, as the GPU does (linearly between entries).
#[cfg(test)]
pub fn lookup(table: &[f32; LUT_LEN], x: f32) -> f32 {
    let f = x.clamp(0.0, 1.0) * (LUT_LEN - 1) as f32;
    let i = (f.floor() as usize).min(LUT_LEN - 2);
    let t = f - i as f32;
    table[i] + (table[i + 1] - table[i]) * t
}

/// A pane: the centre of its rounded rect, half its width and height, and
/// its corner radius, in px.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Shape {
    pub cx: f32,
    pub cy: f32,
    pub hx: f32,
    pub hy: f32,
    pub r: f32,
}

impl Shape {
    /// The rounded rect `w` x `h` px at (`x`, `y`) with corners of `r` px.
    pub fn new(x: f32, y: f32, w: f32, h: f32, r: f32) -> Self {
        let (hx, hy) = (w.max(0.0) / 2.0, h.max(0.0) / 2.0);
        Self { cx: x + hx, cy: y + hy, hx, hy, r: r.clamp(0.0, hx.min(hy)) }
    }

    /// How far in the glass can be from its edge at most (half its shorter side).
    #[cfg(test)]
    pub fn depth(&self) -> f32 {
        self.hx.min(self.hy)
    }
}

/// Signed distance from (`px`, `py`) to the edge of `s` (negative inside).
#[cfg(test)]
pub fn shape_sd(s: &Shape, px: f32, py: f32) -> f32 {
    let qx = (px - s.cx).abs() - (s.hx - s.r);
    let qy = (py - s.cy).abs() - (s.hy - s.r);
    let outside = qx.max(0.0).hypot(qy.max(0.0));
    outside + qx.max(qy).min(0.0) - s.r
}

/// Which way the edge near (`px`, `py`) faces, outwards, as the slope of the
/// distance to it measured `step` px either side: a unit vector along a
/// straight edge and round a corner, turning smoothly where two edges meet
/// inside the pane, and shrinking to nothing along its middle (where the
/// edges on either side cancel), so the bend fades out there instead of
/// flipping over.
#[cfg(test)]
pub fn bend_dir(s: &Shape, px: f32, py: f32, step: f32) -> (f32, f32) {
    let gx = shape_sd(s, px + step, py) - shape_sd(s, px - step, py);
    let gy = shape_sd(s, px, py + step) - shape_sd(s, px, py - step);
    (gx / (2.0 * step), gy / (2.0 * step))
}

/// How the glass looks, sizes in px at the pane's scale.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Look {
    /// How far in from the edge the glass is curved.
    pub band: f32,
    /// How far the light is bent just in from the rim, as a share of the band.
    pub bend: f32,
    /// How much further red bends than blue, as a share of the bend.
    pub split: f32,
    /// The blur over what's behind (a Gaussian's sigma, px).
    pub blur: f32,
    /// Extra softening in the curved edge, in steps of a halving of detail.
    pub edge_blur: f32,
    /// Light caught by the curve: on the side facing the light (top left),
    /// on the far side, and all round.
    pub key: f32,
    pub back: f32,
    pub ambient: f32,
    /// How much more vivid what's behind looks through the glass (1 = as it is).
    pub vivid: f32,
}

/// What the shader works out for one pixel of a pane, before it reads what's
/// behind: how much of the pixel the pane covers, where to read (as an
/// offset in px from the pixel, for red, green and blue), how soft to read,
/// and the light the curve adds.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub struct Pixel {
    pub cover: f32,
    pub offset: [(f32, f32); 3],
    pub lod: f32,
    pub light: f32,
}

/// The light caught by the curve at depth `x` facing `(nx, ny)` (outwards,
/// not necessarily unit length).
#[cfg(test)]
pub fn rim_light(look: &Look, x: f32, nx: f32, ny: f32) -> f32 {
    let len = nx.hypot(ny);
    if len < 1e-4 || x >= 1.0 {
        return 0.0;
    }
    // How far the surface leans away from facing the viewer: 0 on the flat
    // top, 1 at the upright rim.
    let tilt = (THICKNESS * slope(x)).min(60.0) * len;
    let lean = 1.0 - 1.0 / (1.0 + tilt * tilt).sqrt();
    // Light from the top left.
    let facing = (nx * -0.6 + ny * -0.8) / len;
    let edge = lean.powi(3);
    edge * (look.key * facing.max(0.0).powf(1.5) + look.back * (-facing).max(0.0).powf(1.5) + look.ambient)
}

/// The shader's per-pixel work for the pixel centred at (`px`, `py`).
#[cfg(test)]
pub fn pixel(s: &Shape, look: &Look, table: &[f32; LUT_LEN], px: f32, py: f32) -> Pixel {
    let sd = shape_sd(s, px, py);
    let cover = (0.5 - sd).clamp(0.0, 1.0);
    let band = look.band.min(s.depth()).max(1.0);
    let x = (-sd / band).clamp(0.0, 1.0);
    let (nx, ny) = bend_dir(s, px, py, (band * 0.25).max(1.0));
    let d = lookup(table, x) * look.bend * band;
    let at = |k: f32| (-nx * d * k, -ny * d * k);
    Pixel {
        cover,
        offset: [at(1.0 + look.split), at(1.0), at(1.0 - look.split)],
        lod: look.edge_blur * (1.0 - x) * (1.0 - x),
        light: rim_light(look, x, nx, ny),
    }
}

#[cfg(test)]
pub mod reference {
    //! The whole pane drawn on the CPU the way the shader draws it, over a
    //! picture of what's behind: for the tests, and for looking at the
    //! glass without Windows.

    use super::*;

    /// Reads `src` (`w` x `h` RGB floats) at (x, y), bilinear, held at the edges.
    fn sample(src: &[[f32; 3]], w: usize, h: usize, x: f32, y: f32, c: usize) -> f32 {
        let x = (x - 0.5).clamp(0.0, (w - 1) as f32);
        let y = (y - 0.5).clamp(0.0, (h - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let at = |x: usize, y: usize| src[y * w + x][c];
        let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * fx;
        let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * fx;
        top + (bottom - top) * fy
    }

    /// A Gaussian blur of `src` with sigma `s` px (for the tuner; the tests
    /// draw over what's behind as it is).
    #[allow(dead_code)]
    pub fn blur(src: &[[f32; 3]], w: usize, h: usize, s: f32) -> Vec<[f32; 3]> {
        if s < 0.3 {
            return src.to_vec();
        }
        let k = (s * 3.0).ceil() as isize;
        let weights: Vec<f32> = (-k..=k).map(|i| (-(i * i) as f32 / (2.0 * s * s)).exp()).collect();
        let total: f32 = weights.iter().sum();
        let pass = |src: &[[f32; 3]], dx: isize, dy: isize| -> Vec<[f32; 3]> {
            let mut out = vec![[0.0f32; 3]; w * h];
            for y in 0..h as isize {
                for x in 0..w as isize {
                    let mut acc = [0.0f32; 3];
                    for (j, wt) in weights.iter().enumerate() {
                        let i = j as isize - k;
                        let sx = (x + i * dx).clamp(0, w as isize - 1) as usize;
                        let sy = (y + i * dy).clamp(0, h as isize - 1) as usize;
                        for c in 0..3 {
                            acc[c] += src[sy * w + sx][c] * wt;
                        }
                    }
                    out[y as usize * w + x as usize] = acc.map(|v| v / total);
                }
            }
            out
        };
        let h1 = pass(src, 1, 0);
        pass(&h1, 0, 1)
    }

    /// Draws the pane `s` over `behind` (already blurred, and a softer copy
    /// for the edge), returning premultiplied RGBA floats for the whole
    /// picture (the pane over what's behind it).
    pub fn draw(behind: &[[f32; 3]], soft: &[[f32; 3]], w: usize, h: usize, s: &Shape, look: &Look) -> Vec<[f32; 3]> {
        let table = profile();
        let mut out = behind.to_vec();
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let p = pixel(s, look, &table, px, py);
                if p.cover <= 0.0 {
                    continue;
                }
                let mix = (p.lod / look.edge_blur.max(1e-3)).clamp(0.0, 1.0);
                let mut rgb = [0.0f32; 3];
                for (c, v) in rgb.iter_mut().enumerate() {
                    let (ox, oy) = p.offset[c];
                    let a = sample(behind, w, h, px + ox, py + oy, c);
                    let b = sample(soft, w, h, px + ox, py + oy, c);
                    *v = a + (b - a) * mix;
                }
                let luma = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
                for v in &mut rgb {
                    *v = (luma + (*v - luma) * look.vivid + p.light).clamp(0.0, 1.0);
                }
                let o = &mut out[y * w + x];
                for c in 0..3 {
                    o[c] = o[c] * (1.0 - p.cover) + rgb[c] * p.cover;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look() -> Look {
        Look {
            band: 24.0,
            bend: 0.45,
            split: 0.06,
            blur: 2.0,
            edge_blur: 1.5,
            key: 0.9,
            back: 0.45,
            ambient: 0.12,
            vivid: 1.1,
        }
    }

    #[test]
    fn the_profile_bends_most_at_the_rim_and_not_at_all_where_flat() {
        let t = profile();
        assert_eq!(t[0], 0.0, "nothing to bend at the very rim (no glass)");
        let peak = t.iter().position(|&v| v == 1.0).unwrap();
        assert!(peak <= 4, "the strongest bend is just in from the rim, at {peak}");
        assert!(t[LUT_LEN - 1].abs() < 1e-3, "flat glass doesn't bend");
        // Falls off steadily from the peak to the flat.
        for i in peak..LUT_LEN - 1 {
            assert!(t[i + 1] <= t[i] + 1e-6, "rises again at {i}");
        }
    }

    #[test]
    fn distance_to_a_rounded_rect() {
        let s = Shape::new(0.0, 0.0, 100.0, 40.0, 8.0);
        assert!((shape_sd(&s, 50.0, 0.0)).abs() < 1e-4);
        assert!((shape_sd(&s, 50.0, 20.0) + 20.0).abs() < 1e-4);
        assert!((shape_sd(&s, 5.0, 20.0) + 5.0).abs() < 1e-4);
        // The square corner is outside the rounded one.
        assert!(shape_sd(&s, 0.0, 0.0) > 0.0);
        assert!(shape_sd(&s, 0.0, 0.0) < 8.0 * std::f32::consts::SQRT_2);
    }

    #[test]
    fn light_is_read_from_further_in_never_from_outside() {
        let s = Shape::new(0.0, 0.0, 300.0, 60.0, 8.0);
        let t = profile();
        for &(px, py) in &[(150.0, 2.0), (150.0, 58.0), (2.0, 30.0), (298.0, 30.0), (4.0, 4.0), (296.0, 57.0)] {
            let p = pixel(&s, &look(), &t, px, py);
            let (ox, oy) = p.offset[1];
            assert!(shape_sd(&s, px + ox, py + oy) <= shape_sd(&s, px, py) + 1e-3, "({px}, {py}) reads outward");
            assert!(ox.hypot(oy) > 0.5, "({px}, {py}) isn't bent");
        }
        // The flat middle isn't bent.
        let p = pixel(&s, &look(), &t, 150.0, 30.0);
        assert!(p.offset[1].0.hypot(p.offset[1].1) < 0.05);
    }

    #[test]
    fn a_bar_all_edge_has_no_seam_down_its_middle() {
        // 48 px tall with a 24 px band: curved all the way across, so the
        // middle line is where the top and bottom edges meet.
        let s = Shape::new(0.0, 0.0, 400.0, 48.0, 8.0);
        let t = profile();
        let mid = pixel(&s, &look(), &t, 200.0, 24.0);
        assert!(mid.offset[1].1.abs() < 0.05, "bent at the middle line: {:?}", mid.offset[1]);
        // And it changes smoothly either side of it.
        let a = pixel(&s, &look(), &t, 200.0, 23.0).offset[1].1;
        let b = pixel(&s, &look(), &t, 200.0, 25.0).offset[1].1;
        assert!((a + b).abs() < 1e-3 && a.abs() < 1.5, "jumps across the middle: {a} {b}");
    }

    #[test]
    fn corners_bend_smoothly() {
        // Walking round the inside of a corner, the bend never jumps.
        let s = Shape::new(0.0, 0.0, 200.0, 200.0, 12.0);
        let t = profile();
        let mut last: Option<(f32, f32)> = None;
        for i in 0..=90 {
            let a = (i as f32).to_radians();
            let (px, py) = (40.0 - 30.0 * a.cos(), 40.0 - 30.0 * a.sin());
            let o = pixel(&s, &look(), &t, px, py).offset[1];
            if let Some(l) = last {
                assert!((o.0 - l.0).hypot(o.1 - l.1) < 0.6, "jumps at {i} degrees");
            }
            last = Some(o);
        }
    }

    #[test]
    fn the_rim_catches_light_from_the_top_left() {
        let s = Shape::new(0.0, 0.0, 300.0, 100.0, 8.0);
        let t = profile();
        let top = pixel(&s, &look(), &t, 150.0, 0.8).light;
        let bottom = pixel(&s, &look(), &t, 150.0, 99.2).light;
        let middle = pixel(&s, &look(), &t, 150.0, 50.0).light;
        assert!(top > bottom && bottom > 0.0, "top {top}, bottom {bottom}");
        assert!(middle < 1e-3);
        // A thin line: gone a few px in.
        assert!(pixel(&s, &look(), &t, 150.0, 6.0).light < top * 0.2);
    }

    #[test]
    fn a_flat_background_stays_flat_away_from_the_rim() {
        let (w, h) = (120usize, 60usize);
        let behind = vec![[0.3f32, 0.5, 0.7]; w * h];
        let s = Shape::new(10.0, 10.0, 100.0, 40.0, 8.0);
        let mut l = look();
        l.vivid = 1.0;
        let out = reference::draw(&behind, &behind, w, h, &s, &l);
        let mid = out[30 * w + 60];
        assert!((mid[0] - 0.3).abs() < 1e-3 && (mid[2] - 0.7).abs() < 1e-3);
        // Outside the pane it's untouched.
        assert_eq!(out[2 * w + 2], [0.3, 0.5, 0.7]);
    }
}
