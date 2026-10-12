//! Drawing: Direct2D renders into a 32-bit DIB that `UpdateLayeredWindow`
//! hands to the compositor. Per-pixel alpha means fully transparent pixels
//! are click-through for free, so no mouse hook is needed.

use std::ffi::c_void;

use windows::core::{w, Result};
use windows::Win32::Foundation::{COLORREF, D2DERR_RECREATE_TARGET, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

#[derive(Clone, Copy)]
pub struct Color(pub f32, pub f32, pub f32, pub f32);

impl Color {
    pub const fn white(a: f32) -> Self {
        Color(1.0, 1.0, 1.0, a)
    }
    pub fn alpha(self, a: f32) -> Self {
        Color(self.0, self.1, self.2, self.3 * a)
    }
    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.0, g: self.1, b: self.2, a: self.3 }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
}

pub struct Fonts {
    pub title: IDWriteTextFormat,
    pub body: IDWriteTextFormat,
    pub small: IDWriteTextFormat,
    pub big: IDWriteTextFormat,
    pub num: IDWriteTextFormat,
    pub label: IDWriteTextFormat,
}

struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    width: i32,
    height: i32,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// The off-screen pixels behind one layered window.
#[derive(Default)]
pub struct Layer {
    surface: Option<Surface>,
}

impl Layer {
    /// Makes sure the backing bitmap can hold `width` x `height` pixels.
    fn ensure(&mut self, width: i32, height: i32) -> Result<HDC> {
        if let Some(s) = &self.surface {
            if s.width >= width && s.height >= height {
                return Ok(s.dc);
            }
        }
        self.surface = None;
        unsafe {
            let dc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(dc, bitmap.into());
            self.surface = Some(Surface { dc, bitmap, old, width, height });
            Ok(dc)
        }
    }

    /// Frees the pixels (e.g. while the window is hidden).
    pub fn release(&mut self) {
        self.surface = None;
    }
}

pub struct Gfx {
    pub factory: ID2D1Factory,
    dw: IDWriteFactory,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    pub fonts: Fonts,
    /// Bumped whenever the render target is recreated; bitmaps made by an
    /// older generation must be thrown away.
    pub generation: u32,
    /// Pixels per layout unit of the frame being drawn.
    scale: std::cell::Cell<f32>,
}

fn text_format(dw: &IDWriteFactory, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextFormat> {
    unsafe {
        let f = dw.CreateTextFormat(
            w!("Segoe UI Variable Text"),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!(""),
        )?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let sign = dw.CreateEllipsisTrimmingSign(&f)?;
        let trimming =
            DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        f.SetTrimming(&trimming, &sign)?;
        Ok(f)
    }
}

impl Gfx {
    pub fn new() -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let rt = Self::create_target(&factory)?;
            let brush = rt.CreateSolidColorBrush(&Color::white(1.0).d2d(), None)?;
            let fonts = Fonts {
                title: text_format(&dw, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
                body: text_format(&dw, 13.0, DWRITE_FONT_WEIGHT_NORMAL)?,
                small: text_format(&dw, 11.0, DWRITE_FONT_WEIGHT_NORMAL)?,
                big: text_format(&dw, 32.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
                num: text_format(&dw, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
                label: text_format(&dw, 12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            };
            Ok(Self { factory, dw, rt, brush, fonts, generation: 0, scale: std::cell::Cell::new(1.0) })
        }
    }

    fn create_target(factory: &ID2D1Factory) -> Result<ID2D1DCRenderTarget> {
        // Software rendering: the surfaces are small, and it avoids a GPU
        // readback every frame plus keeps the GPU asleep.
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        unsafe {
            let rt = factory.CreateDCRenderTarget(&props)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(rt)
        }
    }

    /// Starts a frame of `width` x `height` pixels on `layer`, with drawing
    /// coordinates in layout units of `scale` pixels each.
    pub fn begin(&self, layer: &mut Layer, width: i32, height: i32, scale: f32) -> Result<()> {
        let dc = layer.ensure(width, height)?;
        self.begin_dc(dc, width, height, scale, Color(0.0, 0.0, 0.0, 0.0))
    }

    /// Starts a frame on any device context (used for the opaque preview popup).
    pub fn begin_dc(&self, dc: HDC, width: i32, height: i32, scale: f32, clear: Color) -> Result<()> {
        unsafe {
            self.rt.BindDC(dc, &RECT { left: 0, top: 0, right: width, bottom: height })?;
            self.rt.SetDpi(96.0 * scale, 96.0 * scale);
            self.scale.set(scale);
            self.rt.BeginDraw();
            self.rt.Clear(Some(&clear.d2d()));
        }
        Ok(())
    }

    /// Width of `text` in layout units.
    pub fn measure(&self, text: &str, font: &IDWriteTextFormat) -> f32 {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.dw.CreateTextLayout(&wide, font, 10_000.0, 100.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_ok() {
                m.widthIncludingTrailingWhitespace
            } else {
                0.0
            }
        }
    }

    pub fn painter(&self) -> Painter<'_> {
        Painter { gfx: self }
    }

    /// Ends the frame, recreating the render target if Direct2D lost it.
    pub fn end(&mut self) -> Result<()> {
        unsafe {
            if let Err(e) = self.rt.EndDraw(None, None) {
                if e.code() == D2DERR_RECREATE_TARGET {
                    self.rt = Self::create_target(&self.factory)?;
                    self.brush = self.rt.CreateSolidColorBrush(&Color::white(1.0).d2d(), None)?;
                    self.generation += 1;
                }
                return Err(e);
            }
        }
        Ok(())
    }

    /// Ends the frame and shows `layer` at `pos` on screen.
    pub fn present(&mut self, layer: &Layer, hwnd: HWND, pos: POINT, width: i32, height: i32, alpha: u8) -> Result<()> {
        self.end()?;
        let Some(surface) = &layer.surface else { return Ok(()) };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&pos),
                Some(&SIZE { cx: width, cy: height }),
                Some(surface.dc),
                Some(&POINT { x: 0, y: 0 }),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }

    /// Uploads premultiplied BGRA pixels as a bitmap for this render target.
    pub fn bitmap(&self, width: u32, height: u32, pixels: &[u8]) -> Result<ID2D1Bitmap> {
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        unsafe {
            self.rt.CreateBitmap(
                D2D_SIZE_U { width, height },
                Some(pixels.as_ptr() as *const c_void),
                width * 4,
                &props,
            )
        }
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

fn pt(x: f32, y: f32) -> windows_numerics::Vector2 {
    windows_numerics::Vector2 { X: x, Y: y }
}

/// Immediate-mode drawing helpers for one frame.
pub struct Painter<'a> {
    gfx: &'a Gfx,
}

impl Painter<'_> {
    fn brush(&self, c: Color) -> &ID2D1SolidColorBrush {
        unsafe { self.gfx.brush.SetColor(&c.d2d()) };
        &self.gfx.brush
    }

    pub fn fill_geometry(&self, g: &ID2D1PathGeometry, c: Color) {
        unsafe { self.gfx.rt.FillGeometry(g, self.brush(c), None) };
    }

    pub fn stroke_geometry(&self, g: &ID2D1PathGeometry, c: Color, width: f32) {
        unsafe { self.gfx.rt.DrawGeometry(g, self.brush(c), width, None) };
    }

    pub fn rrect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color) {
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
        unsafe { self.gfx.rt.FillRoundedRectangle(&rr, self.brush(c)) };
    }

    pub fn stroke_rrect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color, width: f32) {
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
        unsafe { self.gfx.rt.DrawRoundedRectangle(&rr, self.brush(c), width, None) };
    }

    pub fn circle(&self, cx: f32, cy: f32, r: f32, c: Color) {
        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
        unsafe { self.gfx.rt.FillEllipse(&e, self.brush(c)) };
    }

    pub fn stroke_circle(&self, cx: f32, cy: f32, r: f32, c: Color, width: f32) {
        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
        unsafe { self.gfx.rt.DrawEllipse(&e, self.brush(c), width, None) };
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: Color, width: f32) {
        unsafe { self.gfx.rt.DrawLine(pt(x0, y0), pt(x1, y1), self.brush(c), width, None) };
    }

    pub fn image(&self, bmp: &ID2D1Bitmap, x: f32, y: f32, w: f32, h: f32, alpha: f32) {
        unsafe {
            self.gfx.rt.DrawBitmap(bmp, Some(&rect(x, y, w, h)), alpha, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None)
        };
    }

    /// Draws a bitmap at its own pixel size, centred on (cx, cy) and snapped
    /// to whole pixels, so icons stay sharp.
    pub fn icon(&self, bmp: &ID2D1Bitmap, cx: f32, cy: f32, alpha: f32) {
        let k = self.gfx.scale.get();
        let size = unsafe { bmp.GetPixelSize() };
        let (w, h) = (size.width as f32 / k, size.height as f32 / k);
        let x = ((cx - w / 2.0) * k).round() / k;
        let y = ((cy - h / 2.0) * k).round() / k;
        self.image(bmp, x, y, w, h, alpha);
    }

    pub fn text(&self, s: &str, font: &IDWriteTextFormat, x: f32, y: f32, w: f32, h: f32, align: Align, c: Color) {
        if s.is_empty() || w <= 0.0 {
            return;
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        let a = match align {
            Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
            Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
        };
        unsafe {
            let _ = font.SetTextAlignment(a);
            self.gfx.rt.DrawText(
                &wide,
                font,
                &rect(x, y, w, h),
                self.brush(c),
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    pub fn clip(&self, x: f32, y: f32, w: f32, h: f32) {
        unsafe { self.gfx.rt.PushAxisAlignedClip(&rect(x, y, w, h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
    }

    pub fn unclip(&self) {
        unsafe { self.gfx.rt.PopAxisAlignedClip() };
    }

    /// Builds a path. `f` adds segments to an open sink.
    pub fn path(&self, f: impl FnOnce(&ID2D1GeometrySink)) -> Option<ID2D1PathGeometry> {
        unsafe {
            let g = self.gfx.factory.CreatePathGeometry().ok()?;
            let sink = g.Open().ok()?;
            sink.SetFillMode(D2D1_FILL_MODE_WINDING);
            f(&sink);
            sink.Close().ok()?;
            Some(g)
        }
    }

    pub fn polygon(&self, pts: &[(f32, f32)], c: Color) {
        if pts.len() < 3 {
            return;
        }
        if let Some(g) = self.path(|s| unsafe {
            s.BeginFigure(pt(pts[0].0, pts[0].1), D2D1_FIGURE_BEGIN_FILLED);
            for p in &pts[1..] {
                s.AddLine(pt(p.0, p.1));
            }
            s.EndFigure(D2D1_FIGURE_END_CLOSED);
        }) {
            self.fill_geometry(&g, c);
        }
    }

    /// An open arc around (cx, cy) from angle `a0` to `a1` (radians, clockwise).
    pub fn arc(&self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, c: Color, width: f32) {
        if let Some(g) = self.path(|s| unsafe {
            s.BeginFigure(pt(cx + r * a0.cos(), cy + r * a0.sin()), D2D1_FIGURE_BEGIN_HOLLOW);
            s.AddArc(&D2D1_ARC_SEGMENT {
                point: pt(cx + r * a1.cos(), cy + r * a1.sin()),
                size: D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if a1 - a0 > std::f32::consts::PI { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            s.EndFigure(D2D1_FIGURE_END_OPEN);
        }) {
            self.stroke_geometry(&g, c, width);
        }
    }

    /// The notch silhouette: flat top, rounded bottom corners and concave
    /// "ears" flaring into the top edge. `x` is the left edge of the body.
    pub fn notch(&self, x: f32, w: f32, h: f32, r: f32, ear: f32, c: Color) {
        let r = r.min(h * 0.5).min(w * 0.5).max(0.0);
        let ear = ear.min(h * 0.5).max(0.0);
        let (x0, x1) = (x, x + w);
        let arc = |s: &ID2D1GeometrySink, px: f32, py: f32, rad: f32, dir: D2D1_SWEEP_DIRECTION| unsafe {
            s.AddArc(&D2D1_ARC_SEGMENT {
                point: pt(px, py),
                size: D2D_SIZE_F { width: rad, height: rad },
                rotationAngle: 0.0,
                sweepDirection: dir,
                arcSize: D2D1_ARC_SIZE_SMALL,
            });
        };
        if let Some(g) = self.path(|s| unsafe {
            s.BeginFigure(pt(x0 - ear, 0.0), D2D1_FIGURE_BEGIN_FILLED);
            s.AddLine(pt(x1 + ear, 0.0));
            arc(s, x1, ear, ear, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE);
            s.AddLine(pt(x1, h - r));
            arc(s, x1 - r, h, r, D2D1_SWEEP_DIRECTION_CLOCKWISE);
            s.AddLine(pt(x0 + r, h));
            arc(s, x0, h - r, r, D2D1_SWEEP_DIRECTION_CLOCKWISE);
            s.AddLine(pt(x0, ear));
            arc(s, x0 - ear, 0.0, ear, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE);
            s.EndFigure(D2D1_FIGURE_END_CLOSED);
        }) {
            self.fill_geometry(&g, c);
        }
    }
}

/// Which source pixels (and how much of each) make up each of `dst`
/// output pixels when shrinking `src` pixels.
fn spans(src: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let r = src as f32 / dst as f32;
    (0..dst)
        .map(|i| {
            let (a, b) = (i as f32 * r, (i + 1) as f32 * r);
            let mut v = Vec::new();
            let mut j = a.floor() as usize;
            while (j as f32) < b && j < src {
                let cover = ((j + 1) as f32).min(b) - (j as f32).max(a);
                if cover > 0.0 {
                    v.push((j, cover / r));
                }
                j += 1;
            }
            v
        })
        .collect()
}

/// Shrinks premultiplied BGRA pixels by averaging the area each output pixel
/// covers: sharp, without the jagged edges plain scaling gives.
pub fn shrink(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    if dw >= sw && dh >= sh {
        return src.to_vec();
    }
    let (dw, dh) = (dw.min(sw).max(1), dh.min(sh).max(1));
    let (xs, ys) = (spans(sw, dw), spans(sh, dh));
    // Horizontal pass into floats, then vertical.
    let mut tmp = vec![0f32; sh * dw * 4];
    for y in 0..sh {
        for (x, span) in xs.iter().enumerate() {
            let out = &mut tmp[(y * dw + x) * 4..][..4];
            for &(sx, f) in span {
                let base = (y * sw + sx) * 4;
                for (c, o) in out.iter_mut().enumerate() {
                    *o += src[base + c] as f32 * f;
                }
            }
        }
    }
    let mut out = vec![0u8; dw * dh * 4];
    for (y, span) in ys.iter().enumerate() {
        for x in 0..dw {
            let mut acc = [0f32; 4];
            for &(sy, f) in span {
                let base = (sy * dw + x) * 4;
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += tmp[base + c] * f;
                }
            }
            for (c, a) in acc.iter().enumerate() {
                out[(y * dw + x) * 4 + c] = a.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::shrink;

    #[test]
    fn shrink_averages_area() {
        // 2x2 -> 1x1: the average of the four pixels.
        let src = [0, 0, 0, 0, 100, 100, 100, 100, 200, 200, 200, 200, 100, 100, 100, 100];
        assert_eq!(shrink(&src, 2, 2, 1, 1), vec![100, 100, 100, 100]);
        // A solid image stays solid at a fractional ratio.
        let solid = vec![255u8; 96 * 96 * 4];
        assert!(shrink(&solid, 96, 96, 36, 36).iter().all(|&v| v == 255));
    }
}
