//! Drawing: Direct2D renders into a 32-bit DIB that `UpdateLayeredWindow`
//! hands to the compositor. Per-pixel alpha means fully transparent pixels
//! are click-through for free, so no mouse hook is needed.

use std::ffi::c_void;

use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::{COLORREF, D2DERR_RECREATE_TARGET, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{
    UpdateLayeredWindow, UpdateLayeredWindowIndirect, ULW_ALPHA, UPDATELAYEREDWINDOWINFO,
};

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

    /// Whether it holds the last frame (so part of it can be redrawn).
    pub fn ready(&self) -> bool {
        self.surface.is_some()
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
    /// Panels painted once and drawn from a bitmap since (see `Painter::cached`).
    cache: std::cell::RefCell<PanelCache>,
}

/// One cached panel: its bitmap, how many pixels it holds, and when it was
/// last drawn.
struct Cached {
    key: u64,
    bitmap: ID2D1Bitmap,
    pixels: u32,
    used: u64,
}

/// Most panels and pixels kept (a few per surface; the Start menu and the
/// widgets panel are the big ones).
const CACHE_ITEMS: usize = 16;
const CACHE_PIXELS: u32 = 6_000_000;

#[derive(Default)]
struct PanelCache {
    items: Vec<Cached>,
    /// Keys drawn once without caching: a panel is cached the second time
    /// it's drawn the same, so one changing size every frame (the notch
    /// opening) never makes a bitmap.
    seen: Vec<u64>,
    tick: u64,
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

fn text_width(dw: &IDWriteFactory, text: &str, font: &IDWriteTextFormat) -> f32 {
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let Ok(layout) = dw.CreateTextLayout(&wide, font, 10_000.0, 100.0) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        if layout.GetMetrics(&mut m).is_ok() {
            m.widthIncludingTrailingWhitespace
        } else {
            0.0
        }
    }
}

/// `text` laid out `w` wide, wrapping between words, top-aligned.
fn wrapped(dw: &IDWriteFactory, text: &str, font: &IDWriteTextFormat, w: f32, h: f32) -> Option<IDWriteTextLayout> {
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let layout = dw.CreateTextLayout(&wide, font, w.max(1.0), h.max(1.0)).ok()?;
        layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP).ok()?;
        layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR).ok()?;
        layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING).ok()?;
        Some(layout)
    }
}

/// Text measuring outside a frame, for laying things out before they're drawn.
pub struct Measure {
    dw: IDWriteFactory,
    pub body: IDWriteTextFormat,
    pub small: IDWriteTextFormat,
}

impl Measure {
    pub fn new() -> Result<Self> {
        unsafe {
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let body = text_format(&dw, 13.0, DWRITE_FONT_WEIGHT_NORMAL)?;
            let small = text_format(&dw, 11.0, DWRITE_FONT_WEIGHT_NORMAL)?;
            Ok(Self { dw, body, small })
        }
    }

    pub fn width(&self, text: &str, font: &IDWriteTextFormat) -> f32 {
        text_width(&self.dw, text, font)
    }

    /// How many lines `text` takes wrapped to `w` (at least one).
    pub fn lines(&self, text: &str, font: &IDWriteTextFormat, w: f32) -> usize {
        let mut m = DWRITE_TEXT_METRICS::default();
        match wrapped(&self.dw, text, font, w, 10_000.0) {
            Some(l) if unsafe { l.GetMetrics(&mut m) }.is_ok() => (m.lineCount as usize).max(1),
            _ => 1,
        }
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
            Ok(Self {
                factory,
                dw,
                rt,
                brush,
                fonts,
                generation: 0,
                scale: std::cell::Cell::new(1.0),
                cache: Default::default(),
            })
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
            self.rt.SetTransform(&windows_numerics::Matrix3x2::identity());
            self.rt.Clear(Some(&clear.d2d()));
        }
        Ok(())
    }

    /// Starts redrawing only `part` (pixels) of the last frame on `layer`, a
    /// frame `width` x `height` pixels in all. Drawing uses the whole frame's
    /// coordinates, as with `begin`; only what falls in `part` is drawn, onto
    /// a cleared `part`. Show it with `present_part`.
    pub fn begin_part(&self, layer: &mut Layer, width: i32, height: i32, part: RECT, scale: f32) -> Result<()> {
        let dc = layer.ensure(width, height)?;
        unsafe {
            self.rt.BindDC(dc, &part)?;
            self.rt.SetDpi(96.0 * scale, 96.0 * scale);
            self.scale.set(scale);
            self.rt.BeginDraw();
            // The target starts at the part's corner: a whole number of pixels,
            // so everything lands on the same pixels as in a full frame.
            let (dx, dy) = (-part.left as f32 / scale, -part.top as f32 / scale);
            self.rt.SetTransform(&windows_numerics::Matrix3x2::translation(dx, dy));
            self.rt.Clear(Some(&Color(0.0, 0.0, 0.0, 0.0).d2d()));
        }
        Ok(())
    }

    /// Width of `text` in layout units.
    pub fn measure(&self, text: &str, font: &IDWriteTextFormat) -> f32 {
        text_width(&self.dw, text, font)
    }

    pub fn painter(&self) -> Painter<'_> {
        Painter { gfx: self, rt: &self.rt }
    }

    /// Forgets the cached panels (see `Painter::cached`), e.g. after the
    /// theme changed.
    pub fn clear_cache(&self) {
        let mut c = self.cache.borrow_mut();
        c.items.clear();
        c.seen.clear();
    }

    /// Ends the frame, recreating the render target if Direct2D lost it.
    pub fn end(&mut self) -> Result<()> {
        unsafe {
            if let Err(e) = self.rt.EndDraw(None, None) {
                if e.code() == D2DERR_RECREATE_TARGET {
                    self.rt = Self::create_target(&self.factory)?;
                    self.brush = self.rt.CreateSolidColorBrush(&Color::white(1.0).d2d(), None)?;
                    self.generation += 1;
                    // The cached panels belonged to the old target.
                    self.clear_cache();
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

    /// Ends a `begin_part` frame and hands the compositor only that `part`
    /// of `layer` (the rest of the window keeps its pixels).
    pub fn present_part(
        &mut self,
        layer: &Layer,
        hwnd: HWND,
        pos: POINT,
        width: i32,
        height: i32,
        alpha: u8,
        part: RECT,
    ) -> Result<()> {
        let ended = self.end();
        unsafe { self.rt.SetTransform(&windows_numerics::Matrix3x2::identity()) };
        ended?;
        let Some(surface) = &layer.surface else { return Ok(()) };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let (size, src) = (SIZE { cx: width, cy: height }, POINT { x: 0, y: 0 });
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            hdcDst: HDC::default(),
            pptDst: &pos,
            psize: &size,
            hdcSrc: surface.dc,
            pptSrc: &src,
            crKey: COLORREF(0),
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: &part,
        };
        unsafe {
            let _ = UpdateLayeredWindowIndirect(hwnd, &info);
        }
        Ok(())
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
    /// What it draws on: the frame, or a cached panel's bitmap (see `cached`).
    rt: &'a ID2D1RenderTarget,
}

impl Painter<'_> {
    fn brush(&self, c: Color) -> &ID2D1SolidColorBrush {
        unsafe { self.gfx.brush.SetColor(&c.d2d()) };
        &self.gfx.brush
    }

    /// A length in layout units rounded to whole device pixels (at least one),
    /// so thin strokes land on the pixel grid instead of smearing across it.
    pub fn px(&self, w: f32) -> f32 {
        let k = self.gfx.scale.get();
        (w * k).round().max(1.0) / k
    }

    /// A coordinate rounded to a whole device pixel.
    pub fn snap(&self, v: f32) -> f32 {
        let k = self.gfx.scale.get();
        (v * k).round() / k
    }

    /// The centre line for a stroke `w` wide (already from `px`) near `v`, put
    /// so its edges fall on pixel boundaries.
    pub fn snap_line(&self, v: f32, w: f32) -> f32 {
        let k = self.gfx.scale.get();
        if (w * k).round() as i32 % 2 == 1 {
            ((v * k).floor() + 0.5) / k
        } else {
            (v * k).round() / k
        }
    }

    pub fn fill_geometry(&self, g: &ID2D1PathGeometry, c: Color) {
        unsafe { self.rt.FillGeometry(g, self.brush(c), None) };
    }

    pub fn stroke_geometry(&self, g: &ID2D1PathGeometry, c: Color, width: f32) {
        unsafe { self.rt.DrawGeometry(g, self.brush(c), width, None) };
    }

    pub fn rrect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color) {
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
        unsafe { self.rt.FillRoundedRectangle(&rr, self.brush(c)) };
    }

    /// A rounded rect filled top to bottom with a gradient: `stops` are
    /// (position from 0 at the top to 1 at the bottom, colour), in order.
    pub fn gradient_rrect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, stops: &[(f32, Color)]) {
        let stops: Vec<D2D1_GRADIENT_STOP> =
            stops.iter().map(|&(position, c)| D2D1_GRADIENT_STOP { position, color: c.d2d() }).collect();
        let rt = self.rt;
        unsafe {
            let Ok(collection) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else {
                return;
            };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(x, y), endPoint: pt(x, y + h) };
            let Ok(brush) = rt.CreateLinearGradientBrush(&props, None, &collection) else { return };
            let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
            rt.FillRoundedRectangle(&rr, &brush);
        }
    }

    fn stops(&self, stops: &[(f32, Color)]) -> Option<ID2D1GradientStopCollection> {
        let stops: Vec<D2D1_GRADIENT_STOP> =
            stops.iter().map(|&(position, c)| D2D1_GRADIENT_STOP { position, color: c.d2d() }).collect();
        unsafe { self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok() }
    }

    /// A gradient running from (x0, y0) to (x1, y1); `stops` as in `gradient_rrect`.
    pub fn linear(&self, x0: f32, y0: f32, x1: f32, y1: f32, stops: &[(f32, Color)]) -> Option<ID2D1Brush> {
        let collection = self.stops(stops)?;
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(x0, y0), endPoint: pt(x1, y1) };
        let b = unsafe { self.rt.CreateLinearGradientBrush(&props, None, &collection).ok()? };
        b.cast().ok()
    }

    /// A gradient spreading out from (cx, cy) to `radius`.
    pub fn radial(&self, cx: f32, cy: f32, radius: f32, stops: &[(f32, Color)]) -> Option<ID2D1Brush> {
        self.elliptic(cx, cy, radius, radius, stops)
    }

    /// A gradient spreading out from (cx, cy) to an ellipse of radii `rx`, `ry`.
    pub fn elliptic(&self, cx: f32, cy: f32, rx: f32, ry: f32, stops: &[(f32, Color)]) -> Option<ID2D1Brush> {
        let collection = self.stops(stops)?;
        let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
            center: pt(cx, cy),
            gradientOriginOffset: pt(0.0, 0.0),
            radiusX: rx,
            radiusY: ry,
        };
        let b = unsafe { self.rt.CreateRadialGradientBrush(&props, None, &collection).ok()? };
        b.cast().ok()
    }

    /// Sets how opaque everything `brush` paints is, so one gradient can be
    /// drawn at several strengths.
    pub fn fade(&self, brush: &ID2D1Brush, opacity: f32) {
        unsafe { brush.SetOpacity(opacity.clamp(0.0, 1.0)) };
    }

    pub fn fill_rrect_with(&self, x: f32, y: f32, w: f32, h: f32, r: f32, brush: &ID2D1Brush) {
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
        unsafe { self.rt.FillRoundedRectangle(&rr, brush) };
    }

    pub fn stroke_rrect_with(&self, x: f32, y: f32, w: f32, h: f32, r: f32, brush: &ID2D1Brush, width: f32) {
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
        unsafe { self.rt.DrawRoundedRectangle(&rr, brush, width, None) };
    }

    pub fn stroke_rrect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color, width: f32) {
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
        unsafe { self.rt.DrawRoundedRectangle(&rr, self.brush(c), width, None) };
    }

    pub fn circle(&self, cx: f32, cy: f32, r: f32, c: Color) {
        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
        unsafe { self.rt.FillEllipse(&e, self.brush(c)) };
    }

    pub fn stroke_circle(&self, cx: f32, cy: f32, r: f32, c: Color, width: f32) {
        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
        unsafe { self.rt.DrawEllipse(&e, self.brush(c), width, None) };
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: Color, width: f32) {
        unsafe { self.rt.DrawLine(pt(x0, y0), pt(x1, y1), self.brush(c), width, None) };
    }

    pub fn image(&self, bmp: &ID2D1Bitmap, x: f32, y: f32, w: f32, h: f32, alpha: f32) {
        unsafe { self.rt.DrawBitmap(bmp, Some(&rect(x, y, w, h)), alpha, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None) };
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
        // Whole-pixel origin: grayscale text drawn between pixel rows is smeared.
        let (x, y) = (self.snap(x), self.snap(y));
        let wide: Vec<u16> = s.encode_utf16().collect();
        let a = match align {
            Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
            Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
        };
        unsafe {
            let _ = font.SetTextAlignment(a);
            self.rt.DrawText(
                &wide,
                font,
                &rect(x, y, w, h),
                self.brush(c),
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    /// Left-aligned text wrapped between words to `w`, from the top of the
    /// box; what doesn't fit in `h` ends in an ellipsis.
    pub fn text_wrap(&self, s: &str, font: &IDWriteTextFormat, x: f32, y: f32, w: f32, h: f32, c: Color) {
        if s.is_empty() || w <= 0.0 {
            return;
        }
        let (x, y) = (self.snap(x), self.snap(y));
        let Some(layout) = wrapped(&self.gfx.dw, s, font, w, h) else { return };
        unsafe {
            self.rt.PushAxisAlignedClip(&rect(x, y, w, h), D2D1_ANTIALIAS_MODE_ALIASED);
            self.rt.DrawTextLayout(pt(x, y), &layout, self.brush(c), D2D1_DRAW_TEXT_OPTIONS_NONE);
            self.rt.PopAxisAlignedClip();
        }
    }

    /// Makes a rectangle see-through but still clickable (a layered window
    /// ignores the mouse where it is fully transparent), so a DWM thumbnail
    /// under the window shows through.
    pub fn hole(&self, x: f32, y: f32, w: f32, h: f32) {
        unsafe {
            self.rt.PushAxisAlignedClip(&rect(x, y, w, h), D2D1_ANTIALIAS_MODE_ALIASED);
            self.rt.Clear(Some(&Color(0.0, 0.0, 0.0, 0.004).d2d()));
            self.rt.PopAxisAlignedClip();
        }
    }

    pub fn clip(&self, x: f32, y: f32, w: f32, h: f32) {
        unsafe { self.rt.PushAxisAlignedClip(&rect(x, y, w, h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
    }

    pub fn unclip(&self) {
        unsafe { self.rt.PopAxisAlignedClip() };
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

    /// The notch silhouette against a screen edge: flat where it meets the
    /// edge, rounded corners on its free side and concave "ears" flaring
    /// into the edge. The edge runs along `base` (x for a side edge, y
    /// otherwise); the body starts `a` along it, is `len` long and reaches
    /// `depth` out from it.
    pub fn notch(&self, edge: Edge, base: f32, a: f32, len: f32, depth: f32, r: f32, ear: f32, c: Color) {
        let r = r.min(depth * 0.5).min(len * 0.5).max(0.0);
        let ear = ear.min(depth * 0.5).max(0.0);
        let (u0, u1) = (a, a + len);
        // Drawn as if against the top edge, then mapped onto the real one. A
        // mirrored mapping turns arcs the other way.
        let at = |u: f32, v: f32| match edge {
            Edge::Top => pt(u, base + v),
            Edge::Bottom => pt(u, base - v),
            Edge::Left => pt(base + v, u),
            Edge::Right => pt(base - v, u),
        };
        let mirrored = matches!(edge, Edge::Bottom | Edge::Left);
        let (cw, ccw) = if mirrored {
            (D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, D2D1_SWEEP_DIRECTION_CLOCKWISE)
        } else {
            (D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE)
        };
        let arc = |s: &ID2D1GeometrySink, u: f32, v: f32, rad: f32, dir: D2D1_SWEEP_DIRECTION| unsafe {
            s.AddArc(&D2D1_ARC_SEGMENT {
                point: at(u, v),
                size: D2D_SIZE_F { width: rad, height: rad },
                rotationAngle: 0.0,
                sweepDirection: dir,
                arcSize: D2D1_ARC_SIZE_SMALL,
            });
        };
        if let Some(g) = self.path(|s| unsafe {
            s.BeginFigure(at(u0 - ear, 0.0), D2D1_FIGURE_BEGIN_FILLED);
            s.AddLine(at(u1 + ear, 0.0));
            arc(s, u1, ear, ear, ccw);
            s.AddLine(at(u1, depth - r));
            arc(s, u1 - r, depth, r, cw);
            s.AddLine(at(u0 + r, depth));
            arc(s, u0, depth - r, r, cw);
            s.AddLine(at(u0, ear));
            arc(s, u0 - ear, 0.0, ear, ccw);
            s.EndFigure(D2D1_FIGURE_END_CLOSED);
        }) {
            self.fill_geometry(&g, c);
        }
    }

    /// Draws a `w` x `h` panel at (x, y) that `draw` paints (given where, and
    /// how opaque), from a bitmap of it when it was drawn the same (`key`, at
    /// the same scale and offset within the pixel) the time before: steady
    /// panels are painted once instead of every frame. `alpha` fades all of
    /// it; cached, `draw` paints at full strength and the bitmap is faded.
    pub fn cached(&self, key: u64, x: f32, y: f32, w: f32, h: f32, alpha: f32, draw: impl Fn(&Painter, f32, f32, f32)) {
        use std::hash::{Hash, Hasher};
        let k = self.gfx.scale.get();
        // From the pixel before (x, y), a pixel spare all round for smoothing.
        let (px, py) = ((x * k).floor() - 1.0, (y * k).floor() - 1.0);
        let (pw, ph) = (((x + w) * k - px).ceil() as u32 + 1, ((y + h) * k - py).ceil() as u32 + 1);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (key, k.to_bits(), (x * k - px).to_bits(), (y * k - py).to_bits(), pw, ph).hash(&mut hasher);
        let key = hasher.finish();
        if pw == 0 || ph == 0 || pw.saturating_mul(ph) > CACHE_PIXELS / 2 || alpha <= 0.0 {
            draw(self, x, y, alpha);
            return;
        }
        let found = {
            let mut c = self.gfx.cache.borrow_mut();
            c.tick += 1;
            let tick = c.tick;
            match c.items.iter().position(|i| i.key == key) {
                Some(n) => {
                    c.items[n].used = tick;
                    Some(c.items[n].bitmap.clone())
                }
                None if c.seen.contains(&key) => None,
                None => {
                    // The first time: drawn as it is, and remembered.
                    if c.seen.len() >= 32 {
                        c.seen.remove(0);
                    }
                    c.seen.push(key);
                    drop(c);
                    draw(self, x, y, alpha);
                    return;
                }
            }
        };
        let bitmap = match found {
            Some(b) => b,
            None => match self.paint_cached(px, py, pw, ph, k, |p| draw(p, x, y, 1.0)) {
                Some(b) => {
                    let mut c = self.gfx.cache.borrow_mut();
                    c.seen.retain(|s| *s != key);
                    let used = c.tick;
                    c.items.push(Cached { key, bitmap: b.clone(), pixels: pw * ph, used });
                    // Least recently drawn first out, to keep within the limits.
                    while c.items.len() > CACHE_ITEMS || c.items.iter().map(|i| i.pixels).sum::<u32>() > CACHE_PIXELS {
                        let Some(oldest) = c.items.iter().enumerate().min_by_key(|(_, i)| i.used).map(|(n, _)| n)
                        else {
                            break;
                        };
                        c.items.remove(oldest);
                    }
                    b
                }
                None => {
                    draw(self, x, y, alpha);
                    return;
                }
            },
        };
        let dest = rect(px / k, py / k, pw as f32 / k, ph as f32 / k);
        unsafe {
            // Pixel for pixel, so it matches drawing it directly.
            self.rt.DrawBitmap(&bitmap, Some(&dest), alpha, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
        }
    }

    /// Paints `draw` (in the frame's coordinates) into a new `pw` x `ph`
    /// bitmap whose top-left is pixel (px, py) of the frame.
    fn paint_cached(
        &self,
        px: f32,
        py: f32,
        pw: u32,
        ph: u32,
        k: f32,
        draw: impl FnOnce(&Painter),
    ) -> Option<ID2D1Bitmap> {
        unsafe {
            let dips = D2D_SIZE_F { width: pw as f32 / k, height: ph as f32 / k };
            let pixels = D2D_SIZE_U { width: pw, height: ph };
            let target = self
                .rt
                .CreateCompatibleRenderTarget(
                    Some(&dips as *const _),
                    Some(&pixels as *const _),
                    None,
                    D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
                )
                .ok()?;
            target.BeginDraw();
            target.SetTransform(&windows_numerics::Matrix3x2::translation(-px / k, -py / k));
            target.Clear(Some(&Color(0.0, 0.0, 0.0, 0.0).d2d()));
            draw(&Painter { gfx: self.gfx, rt: &target });
            target.EndDraw(None, None).ok()?;
            target.GetBitmap().ok()
        }
    }

    /// Moves everything drawn after it by (x, y); `translate(0.0, 0.0)` undoes it.
    pub fn translate(&self, x: f32, y: f32) {
        unsafe { self.rt.SetTransform(&windows_numerics::Matrix3x2::translation(x, y)) };
    }
}

/// A screen edge (where the notch sits).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    pub fn parse(v: &str) -> Self {
        match v {
            "bottom" => Edge::Bottom,
            "left" => Edge::Left,
            "right" => Edge::Right,
            _ => Edge::Top,
        }
    }

    /// The left or right edge, where "along" runs down the screen.
    pub fn side(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }

    /// A rect given against the top edge (`u` along it, `v` out from it,
    /// `ul` by `vl`), placed against this edge running along `base`.
    pub fn rect(self, base: f32, u: f32, v: f32, ul: f32, vl: f32) -> (f32, f32, f32, f32) {
        match self {
            Edge::Top => (u, base + v, ul, vl),
            Edge::Bottom => (u, base - v - vl, ul, vl),
            Edge::Left => (base + v, u, vl, ul),
            Edge::Right => (base - v - vl, u, vl, ul),
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
