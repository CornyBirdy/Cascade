//! Paint (`cascade.exe --paint [picture]`), as Windows' own: a pencil, a
//! soft brush, an eraser, fill, a colour picker, lines, rectangles and
//! ellipses (filled or not), a palette and any colour, sizes, undo and redo
//! (Ctrl+Z, Ctrl+Y), zoom (Ctrl+wheel, or the buttons), and New, Open, Save
//! and Save as (PNG or JPEG). Ctrl+V pastes a picture from the clipboard as
//! a floating piece to drag into place (Enter or a click elsewhere puts it
//! down, Esc drops it), Ctrl+C copies the picture, and the menu at the end
//! of the bar resizes the canvas (cropping or stretching) and rotates or
//! flips the picture.

use std::path::PathBuf;

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_BACK, VK_CONTROL, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_TAB,
};

use super::{App, Ctx, Kit, Look, Mouse};
use crate::gfx::{Align, Color, Fonts, Painter};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tool {
    Pencil,
    Brush,
    Eraser,
    Fill,
    Picker,
    Line,
    Rect,
    Ellipse,
}

const TOOLS: [(Tool, u32, &str); 8] = [
    (Tool::Pencil, 0xED63, "Pencil"),
    (Tool::Brush, 0xE771, "Brush"),
    (Tool::Eraser, 0xE75C, "Eraser"),
    (Tool::Fill, 0xE8CF, "Fill"),
    (Tool::Picker, 0xEF3C, "Pick"),
    (Tool::Line, 0xE738, "Line"),
    (Tool::Rect, 0xE739, "Rectangle"),
    (Tool::Ellipse, 0xEA3A, "Ellipse"),
];

/// 0xRRGGBB.
const PALETTE: [u32; 14] = [
    0x000000, 0x7F7F7F, 0xFFFFFF, 0xC3C3C3, 0xED1C24, 0xFF7F27, 0xFFF200, 0x22B14C, 0x00A2E8, 0x3F48CC, 0xA349A4,
    0xB97A57, 0xFFAEC9, 0x99D9EA,
];

struct Canvas {
    w: i32,
    h: i32,
    /// 0xAARRGGBB, opaque.
    px: Vec<u32>,
}

impl Canvas {
    fn new(w: i32, h: i32) -> Self {
        Canvas { w, h, px: vec![0xFFFF_FFFF; (w * h) as usize] }
    }

    fn get(&self, x: i32, y: i32) -> Option<u32> {
        (x >= 0 && y >= 0 && x < self.w && y < self.h).then(|| self.px[(y * self.w + x) as usize])
    }

    /// Blends `c` over a pixel, `a` (0..1) of the way.
    fn put(&mut self, x: i32, y: i32, c: u32, a: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        let i = (y * self.w + x) as usize;
        if a >= 1.0 {
            self.px[i] = 0xFF00_0000 | c;
            return;
        }
        let d = self.px[i];
        let mix = |s: u32| {
            let (cs, ds) = (((c >> s) & 0xFF) as f32, ((d >> s) & 0xFF) as f32);
            ((ds + (cs - ds) * a).round() as u32) << s
        };
        self.px[i] = 0xFF00_0000 | mix(16) | mix(8) | mix(0);
    }

    /// A round dab of radius `r` at (cx, cy): hard, or soft at the edge.
    fn dab(&mut self, cx: f32, cy: f32, r: f32, c: u32, soft: bool) {
        let r = r.max(0.5);
        let (x0, x1) = ((cx - r - 1.0).floor() as i32, (cx + r + 1.0).ceil() as i32);
        let (y0, y1) = ((cy - r - 1.0).floor() as i32, (cy + r + 1.0).ceil() as i32);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let a =
                    if soft { (1.0 - d / r).clamp(0.0, 1.0).powf(1.5) * 0.35 } else { (r + 0.5 - d).clamp(0.0, 1.0) };
                self.put(x, y, c, a);
            }
        }
    }

    /// Dabs along a line.
    fn stroke(&mut self, a: (f32, f32), b: (f32, f32), r: f32, c: u32, soft: bool) {
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        let step = (r * if soft { 0.25 } else { 0.4 }).max(0.5);
        let n = (len / step).ceil().max(1.0) as i32;
        for i in 0..=n {
            let t = i as f32 / n as f32;
            self.dab(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, r, c, soft);
        }
    }

    fn rect(&mut self, a: (f32, f32), b: (f32, f32), r: f32, c: u32, fill: bool) {
        let (l, t, rr, bb) = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
        if fill {
            for y in t as i32..=bb as i32 {
                for x in l as i32..=rr as i32 {
                    self.put(x, y, c, 1.0);
                }
            }
        } else {
            self.stroke((l, t), (rr, t), r, c, false);
            self.stroke((rr, t), (rr, bb), r, c, false);
            self.stroke((rr, bb), (l, bb), r, c, false);
            self.stroke((l, bb), (l, t), r, c, false);
        }
    }

    fn ellipse(&mut self, a: (f32, f32), b: (f32, f32), r: f32, c: u32, fill: bool) {
        let (cx, cy) = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        let (rx, ry) = (((b.0 - a.0) / 2.0).abs().max(0.5), ((b.1 - a.1) / 2.0).abs().max(0.5));
        if fill {
            for y in (cy - ry) as i32..=(cy + ry) as i32 {
                let dy = (y as f32 + 0.5 - cy) / ry;
                if dy.abs() > 1.0 {
                    continue;
                }
                let half = rx * (1.0 - dy * dy).sqrt();
                for x in (cx - half) as i32..=(cx + half) as i32 {
                    self.put(x, y, c, 1.0);
                }
            }
        } else {
            let n = ((rx + ry) * 2.0).max(24.0) as i32;
            let mut prev = (cx + rx, cy);
            for i in 1..=n {
                let t = i as f32 / n as f32 * std::f32::consts::TAU;
                let p = (cx + rx * t.cos(), cy + ry * t.sin());
                self.stroke(prev, p, r, c, false);
                prev = p;
            }
        }
    }

    /// Fills the area of one colour round (x, y) with `c`.
    fn fill(&mut self, x: i32, y: i32, c: u32) {
        let Some(target) = self.get(x, y) else { return };
        let c = 0xFF00_0000 | c;
        if target == c {
            return;
        }
        let near = |p: u32| {
            let d = |s: u32| (((p >> s) & 0xFF) as i32 - ((target >> s) & 0xFF) as i32).abs();
            d(16) + d(8) + d(0) < 24
        };
        let mut stack = vec![(x, y)];
        while let Some((x, y)) = stack.pop() {
            let row = (y * self.w) as usize;
            let mut l = x;
            while l > 0 && near(self.px[row + (l - 1) as usize]) {
                l -= 1;
            }
            let mut r = x;
            while r + 1 < self.w && near(self.px[row + (r + 1) as usize]) {
                r += 1;
            }
            for xx in l..=r {
                self.px[row + xx as usize] = c;
                for ny in [y - 1, y + 1] {
                    if ny >= 0 && ny < self.h && near(self.px[(ny * self.w + xx) as usize]) {
                        stack.push((xx, ny));
                    }
                }
            }
        }
    }

    fn bytes(&self) -> Vec<u8> {
        self.px.iter().flat_map(|p| p.to_le_bytes()).collect()
    }

    /// The canvas at `w`×`h`: cut off or extended with white (the picture
    /// stays at the top left), or stretched to fit.
    fn resized(&self, w: i32, h: i32, stretch: bool) -> Canvas {
        let mut out = Canvas::new(w, h);
        if stretch {
            for y in 0..h {
                let fy = ((y as f32 + 0.5) * self.h as f32 / h as f32 - 0.5).clamp(0.0, (self.h - 1) as f32);
                let (y0, ty) = (fy.floor() as i32, fy.fract());
                for x in 0..w {
                    let fx = ((x as f32 + 0.5) * self.w as f32 / w as f32 - 0.5).clamp(0.0, (self.w - 1) as f32);
                    let (x0, tx) = (fx.floor() as i32, fx.fract());
                    let at = |xx: i32, yy: i32| self.px[(yy.min(self.h - 1) * self.w + xx.min(self.w - 1)) as usize];
                    let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
                    let mix = |s: u32| {
                        let ch = |p: u32| ((p >> s) & 0xFF) as f32;
                        let top = ch(a) + (ch(b) - ch(a)) * tx;
                        let bottom = ch(c) + (ch(d) - ch(c)) * tx;
                        ((top + (bottom - top) * ty).round() as u32) << s
                    };
                    out.px[(y * w + x) as usize] = 0xFF00_0000 | mix(16) | mix(8) | mix(0);
                }
            }
        } else {
            for y in 0..h.min(self.h) {
                for x in 0..w.min(self.w) {
                    out.px[(y * w + x) as usize] = self.px[(y * self.w + x) as usize];
                }
            }
        }
        out
    }

    /// Turned a quarter, clockwise or not.
    fn rotated(&self, clockwise: bool) -> Canvas {
        let mut out = Canvas::new(self.h, self.w);
        for y in 0..self.h {
            for x in 0..self.w {
                let (nx, ny) = if clockwise { (self.h - 1 - y, x) } else { (y, self.w - 1 - x) };
                out.px[(ny * self.h + nx) as usize] = self.px[(y * self.w + x) as usize];
            }
        }
        out
    }

    /// Mirrored left to right, or top to bottom.
    fn flipped(&self, horizontal: bool) -> Canvas {
        let mut out = Canvas::new(self.w, self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                let (nx, ny) = if horizontal { (self.w - 1 - x, y) } else { (x, self.h - 1 - y) };
                out.px[(ny * self.w + nx) as usize] = self.px[(y * self.w + x) as usize];
            }
        }
        out
    }

    /// `piece` laid over the canvas with its top left at (x, y).
    fn blit(&mut self, piece: &Canvas, x: i32, y: i32) {
        for py in 0..piece.h {
            for px in 0..piece.w {
                let (cx, cy) = (x + px, y + py);
                if cx >= 0 && cy >= 0 && cx < self.w && cy < self.h {
                    self.px[(cy * self.w + cx) as usize] = piece.px[(py * piece.w + px) as usize];
                }
            }
        }
    }
}

/// A picture on the clipboard (a DIB: 24 or 32 bits), as a canvas. A 32-bit
/// picture with see-through parts is laid over white.
fn clipboard_picture(owner: HWND) -> Option<Canvas> {
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    const CF_DIB: u32 = 8;
    unsafe {
        if IsClipboardFormatAvailable(CF_DIB).is_err() || OpenClipboard(Some(owner)).is_err() {
            return None;
        }
        let mut found = None;
        if let Ok(h) = GetClipboardData(CF_DIB) {
            let g = windows::Win32::Foundation::HGLOBAL(h.0);
            let size = GlobalSize(g);
            let at = GlobalLock(g) as *const u8;
            if !at.is_null() && size > 40 {
                found = dib_canvas(std::slice::from_raw_parts(at, size));
            }
            let _ = GlobalUnlock(g);
        }
        let _ = CloseClipboard();
        found
    }
}

/// A DIB (header then pixels) as a canvas.
fn dib_canvas(b: &[u8]) -> Option<Canvas> {
    let u32_at = |o: usize| b.get(o..o + 4).map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]));
    let (head, w, h) = (u32_at(0)? as usize, u32_at(4)? as i32, u32_at(8)? as i32);
    let bits = b.get(14..16).map(|v| u16::from_le_bytes([v[0], v[1]]))? as usize;
    let compression = u32_at(16)?;
    let used = u32_at(32).unwrap_or(0) as usize;
    if head < 40 || w <= 0 || h == 0 || !(bits == 24 || bits == 32) || !(compression == 0 || compression == 3) {
        return None;
    }
    let (rows, top_down) = (h.unsigned_abs() as i32, h < 0);
    if w > 16000 || rows > 16000 {
        return None;
    }
    let masks = if compression == 3 && head == 40 { 12 } else { 0 };
    let start = head + masks + used * 4;
    let stride = (w as usize * bits).div_ceil(32) * 4;
    if b.len() < start + stride * rows as usize {
        return None;
    }
    let bytes = bits / 8;
    // A 32-bit picture whose alpha is all 0 has none (the byte is unused).
    let alpha =
        bits == 32 && (0..rows as usize).any(|y| (0..w as usize).any(|x| b[start + y * stride + x * 4 + 3] != 0));
    let mut c = Canvas::new(w, rows);
    for y in 0..rows as usize {
        let src = if top_down { y } else { rows as usize - 1 - y };
        for x in 0..w as usize {
            let o = start + src * stride + x * bytes;
            let a = if alpha { b[o + 3] as u32 } else { 255 };
            let ch = |v: u8| (v as u32 * a + 255 * (255 - a)) / 255;
            c.px[y * w as usize + x] = 0xFF00_0000 | ch(b[o + 2]) << 16 | ch(b[o + 1]) << 8 | ch(b[o]);
        }
    }
    Some(c)
}

/// A picture pasted, not yet put down: it's dragged into place.
struct Floating {
    piece: Canvas,
    /// Its top left on the canvas.
    at: (f32, f32),
    /// How big it's shown on the canvas (resized with the handles; the
    /// piece itself is only scaled to this when it's put down).
    size: (f32, f32),
    /// Where in it the pointer holds it, while it's dragged.
    grab: Option<(f32, f32)>,
    /// A handle being dragged (0..8, left to right then top to bottom, the
    /// middle one unused) and the piece's (left, top, right, bottom) when
    /// the drag began.
    resize: Option<(usize, (f32, f32, f32, f32))>,
}

/// The size of a handle on a pasted picture, in pixels (a bit more to hit).
const HANDLE: f32 = 10.0;

impl Floating {
    /// The handle at screen point (x, y), the picture drawn at (px, py) at `s`.
    fn handle_at(&self, px: f32, py: f32, s: f32, x: f32, y: f32) -> Option<usize> {
        let (w, h) = (self.size.0 * s, self.size.1 * s);
        let (fx, fy) = (px + self.at.0 * s, py + self.at.1 * s);
        (0..9).filter(|i| *i != 4).find(|i| {
            let (hx, hy) = (fx + w * (*i % 3) as f32 / 2.0, fy + h * (*i / 3) as f32 / 2.0);
            (x - hx).abs() <= HANDLE && (y - hy).abs() <= HANDLE
        })
    }

    /// Drags handle `i` (from the picture's `start` rectangle) to canvas
    /// point `at`: the corners keep its shape (Shift lets them go), the
    /// sides move one edge.
    fn drag_handle(&mut self, i: usize, start: (f32, f32, f32, f32), at: (f32, f32), free: bool) {
        let (mut l, mut t, mut r, mut b) = start;
        let (hx, hy) = (i % 3, i / 3);
        match hx {
            0 => l = at.0.min(r - 1.0),
            2 => r = at.0.max(l + 1.0),
            _ => {}
        }
        match hy {
            0 => t = at.1.min(b - 1.0),
            2 => b = at.1.max(t + 1.0),
            _ => {}
        }
        if hx != 1 && hy != 1 && !free {
            // A corner: both sides by the same factor, from the opposite corner.
            let (w0, h0) = (start.2 - start.0, start.3 - start.1);
            let k = ((r - l) / w0).max((b - t) / h0).max(1.0 / w0.min(h0));
            let (w, h) = (w0 * k, h0 * k);
            if hx == 0 {
                l = r - w;
            } else {
                r = l + w;
            }
            if hy == 0 {
                t = b - h;
            } else {
                b = t + h;
            }
        }
        self.at = (l, t);
        self.size = (r - l, b - t);
    }
}

/// The Resize canvas box: what's typed in each side, which is being typed in,
/// and whether the picture stretches to fit.
struct Dialog {
    size: [String; 2],
    field: usize,
    stretch: bool,
}

struct Paint {
    canvas: Canvas,
    undo: Vec<Vec<u32>>,
    redo: Vec<Vec<u32>>,
    tool: Tool,
    colour: u32,
    size: f32,
    fill_shapes: bool,
    zoom: f32,
    /// Where the canvas was drawn: (x, y, scale).
    view: (f32, f32, f32),
    drag: Option<((f32, f32), (f32, f32))>,
    before: Option<Vec<u32>>,
    bmp: Option<ID2D1Bitmap>,
    path: Option<PathBuf>,
    dirty: bool,
    hwnd: HWND,
    /// A pasted picture waiting to be put down, and its bitmap.
    floating: Option<Floating>,
    float_bmp: Option<ID2D1Bitmap>,
    dialog: Option<Dialog>,
}

const BAR: f32 = 64.0;
const T_TOOL: u32 = 100;
const T_COLOUR: u32 = 200;
const T_MORE: u32 = 250;
const T_SIZE_DOWN: u32 = 260;
const T_SIZE_UP: u32 = 261;
const T_FILLED: u32 = 262;
const T_NEW: u32 = 1;
const T_OPEN: u32 = 2;
const T_SAVE: u32 = 3;
const T_SAVE_AS: u32 = 4;
const T_UNDO: u32 = 5;
const T_REDO: u32 = 6;
const T_ZOOM_OUT: u32 = 7;
const T_ZOOM_IN: u32 = 8;
const T_IMAGE: u32 = 270;
const T_DLG_BLOCK: u32 = 300;
const T_DLG_OK: u32 = 301;
const T_DLG_CANCEL: u32 = 302;
const T_DLG_W: u32 = 303;
const T_DLG_H: u32 = 304;
const T_DLG_STRETCH: u32 = 305;
/// The biggest a side of the canvas can be.
const MAX_SIDE: i32 = 6000;

fn colour_of(c: u32) -> Color {
    Color(((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0, 1.0)
}

impl Paint {
    fn to_canvas(&self, x: f32, y: f32) -> (f32, f32) {
        let (vx, vy, s) = self.view;
        ((x - vx) / s, (y - vy) / s)
    }

    fn snapshot(&mut self) {
        self.undo.push(self.canvas.px.clone());
        if self.undo.len() > 40 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    fn changed(&mut self) {
        self.bmp = None;
    }

    /// Puts a pasted picture down where it is.
    fn commit(&mut self) {
        if let Some(f) = self.floating.take() {
            self.snapshot();
            let (w, h) = ((f.size.0.round() as i32).max(1), (f.size.1.round() as i32).max(1));
            if (w, h) == (f.piece.w, f.piece.h) {
                self.canvas.blit(&f.piece, f.at.0.round() as i32, f.at.1.round() as i32);
            } else {
                self.canvas.blit(&f.piece.resized(w, h, true), f.at.0.round() as i32, f.at.1.round() as i32);
            }
            self.float_bmp = None;
            self.changed();
        }
    }

    /// Pastes the clipboard's picture as a floating piece at the top left of
    /// what's showing; the canvas grows if it's too small for it.
    fn paste(&mut self) {
        let Some(piece) = clipboard_picture(self.hwnd) else { return };
        self.commit();
        if piece.w > self.canvas.w || piece.h > self.canvas.h {
            self.snapshot();
            self.canvas = self.canvas.resized(self.canvas.w.max(piece.w), self.canvas.h.max(piece.h), false);
            self.changed();
        }
        let (vx, vy, s) = self.view;
        // The top left of the visible canvas (the canvas's own, when it all shows).
        let at = (
            ((20.0 - vx) / s).max(0.0).min((self.canvas.w - piece.w) as f32),
            ((BAR + 20.0 - vy) / s).max(0.0).min((self.canvas.h - piece.h) as f32),
        );
        self.float_bmp = None;
        let size = (piece.w as f32, piece.h as f32);
        self.floating = Some(Floating { piece, at, size, grab: None, resize: None });
    }

    /// The picture on the clipboard (a pasted piece waiting is put down first).
    fn copy(&mut self) {
        self.commit();
        unsafe {
            crate::snip::copy_image(Some(self.hwnd), &self.canvas.px, self.canvas.w, self.canvas.h);
        }
    }

    /// Replaces the canvas with `c` (undoable).
    fn replace(&mut self, c: Canvas) {
        self.snapshot();
        self.canvas = c;
        self.zoom = 0.0;
        self.changed();
    }

    fn apply_dialog(&mut self) {
        let Some(d) = self.dialog.take() else { return };
        let side = |s: &str| s.parse::<i32>().ok().filter(|v| *v >= 1).map(|v| v.min(MAX_SIDE));
        if let (Some(w), Some(h)) = (side(&d.size[0]), side(&d.size[1])) {
            if (w, h) != (self.canvas.w, self.canvas.h) {
                let c = self.canvas.resized(w, h, d.stretch);
                self.replace(c);
            }
        }
    }

    fn draw_dialog(&self, p: &Painter, f: &Fonts, w: f32, h: f32, kit: &mut Kit, d: &Dialog) {
        // The rest of the window is out of reach while it's open.
        p.rrect(0.0, 0.0, w, h, 0.0, Color(0.0, 0.0, 0.0, 0.5));
        kit.hit(0.0, 0.0, w, h, T_DLG_BLOCK);
        let (cw, ch) = (340.0, 232.0);
        let (x, y) = ((w - cw) / 2.0, (h - ch) / 2.0);
        for i in 1..=6 {
            let g = i as f32 * 2.0;
            p.rrect(x - g, y - g + 4.0, cw + 2.0 * g, ch + 2.0 * g, 10.0 + g, Color(0.0, 0.0, 0.0, 0.04));
        }
        p.rrect(x, y, cw, ch, 10.0, Color(0.17, 0.17, 0.175, 1.0));
        p.stroke_rrect(x + 0.5, y + 0.5, cw - 1.0, ch - 1.0, 10.0, Color::white(0.1), 1.0);
        p.text("Resize canvas", &f.title, x + 20.0, y + 12.0, cw - 40.0, 30.0, Align::Left, Color::white(0.95));
        for (i, label) in ["Width", "Height"].iter().enumerate() {
            let fy = y + 58.0 + i as f32 * 46.0;
            p.text(label, &f.body, x + 20.0, fy, 70.0, 34.0, Align::Left, Color::white(0.75));
            let on = d.field == i;
            p.rrect(x + 96.0, fy, 130.0, 34.0, 6.0, Color::white(if on { 0.10 } else { 0.06 }));
            if on {
                p.rrect(x + 96.0, fy + 32.0, 130.0, 2.0, 1.0, crate::theme::accent());
            }
            let text = format!("{}{}", d.size[i], if on { "\u{2502}" } else { "" });
            p.text(&text, &f.body, x + 106.0, fy, 114.0, 34.0, Align::Left, Color::white(0.95));
            p.text("pixels", &f.small, x + 236.0, fy, 70.0, 34.0, Align::Left, Color::white(0.5));
            kit.hit(x + 96.0, fy, 130.0, 34.0, T_DLG_W + i as u32);
        }
        // Stretch the picture to fit, or cut it off / add white.
        let ty = y + 150.0;
        let (bx, by) = (x + 20.0, ty);
        p.rrect(bx, by, 18.0, 18.0, 4.0, if d.stretch { crate::theme::accent() } else { Color::white(0.08) });
        p.stroke_rrect(bx + 0.5, by + 0.5, 17.0, 17.0, 4.0, Color::white(0.3), 1.0);
        if d.stretch {
            p.line(bx + 4.0, by + 9.0, bx + 8.0, by + 13.0, Color::white(0.98), 1.8);
            p.line(bx + 8.0, by + 13.0, bx + 14.5, by + 5.0, Color::white(0.98), 1.8);
        }
        p.text(
            "Stretch the picture to fit",
            &f.body,
            bx + 28.0,
            by - 3.0,
            cw - 70.0,
            24.0,
            Align::Left,
            Color::white(0.85),
        );
        kit.hit(bx, by - 4.0, cw - 40.0, 26.0, T_DLG_STRETCH);
        kit.button(p, f, x + cw - 20.0 - 168.0, y + ch - 52.0, 80.0, 34.0, "Cancel", None, Look::Plain, T_DLG_CANCEL);
        kit.button(p, f, x + cw - 20.0 - 80.0, y + ch - 52.0, 80.0, 34.0, "OK", None, Look::Accent, T_DLG_OK);
    }

    fn image_menu(&mut self) {
        use crate::menu::{item, show, Entry, Place};
        let paste = {
            let mut e = item(2, "Paste");
            if let Entry::Item(i) = &mut e {
                i.hint = "Ctrl+V".into();
            }
            e
        };
        let copy = {
            let mut e = item(3, "Copy picture");
            if let Entry::Item(i) = &mut e {
                i.hint = "Ctrl+C".into();
            }
            e
        };
        let entries = vec![
            item(1, "Resize canvas\u{2026}"),
            Entry::Separator,
            paste,
            copy,
            Entry::Separator,
            item(4, "Rotate right"),
            item(5, "Rotate left"),
            item(6, "Flip horizontal"),
            item(7, "Flip vertical"),
            Entry::Separator,
            item(8, "Clear to the colour"),
        ];
        self.commit();
        match show(self.hwnd, entries, Place::Cursor) {
            1 => {
                self.dialog = Some(Dialog {
                    size: [self.canvas.w.to_string(), self.canvas.h.to_string()],
                    field: 0,
                    stretch: false,
                })
            }
            2 => self.paste(),
            3 => self.copy(),
            4 => self.replace(self.canvas.rotated(true)),
            5 => self.replace(self.canvas.rotated(false)),
            6 => self.replace(self.canvas.flipped(true)),
            7 => self.replace(self.canvas.flipped(false)),
            8 => {
                let mut c = Canvas::new(self.canvas.w, self.canvas.h);
                c.px.fill(0xFF00_0000 | self.colour);
                self.replace(c);
            }
            _ => {}
        }
    }

    fn save(&mut self, ask: bool) -> bool {
        self.commit();
        let path = match (&self.path, ask) {
            (Some(p), false) => p.clone(),
            _ => {
                let name = self
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map_or("Untitled.png".into(), |n| n.to_string_lossy().into_owned());
                match super::save_path(self.hwnd, &name, &[("PNG", "*.png"), ("JPEG", "*.jpg")], "png") {
                    Some(p) => p,
                    None => return false,
                }
            }
        };
        let ok = unsafe { crate::snip::save_image(&path, &self.canvas.px, self.canvas.w, self.canvas.h) };
        if ok {
            self.path = Some(path);
            self.dirty = false;
        }
        ok
    }

    fn open(&mut self, path: PathBuf) {
        if let Some((px, w, h)) = crate::viewer::load_pixels(&path) {
            let mut c = Canvas::new(w as i32, h as i32);
            for (i, p) in px.as_chunks::<4>().0.iter().enumerate() {
                // Over white, as it would print.
                let a = p[3] as u32;
                let ch = |v: u8| (v as u32 + (255 - a)).min(255);
                c.px[i] = 0xFF00_0000 | ch(p[2]) << 16 | ch(p[1]) << 8 | ch(p[0]);
            }
            self.canvas = c;
            self.undo.clear();
            self.redo.clear();
            self.path = Some(path);
            self.dirty = false;
            self.zoom = 0.0;
            self.changed();
        }
    }

    fn keep_going(&mut self) -> bool {
        if !self.dirty {
            return true;
        }
        use windows::Win32::UI::WindowsAndMessaging::*;
        let r = unsafe {
            MessageBoxW(Some(self.hwnd), w!("Save your picture first?"), w!("Paint"), MB_YESNOCANCEL | MB_ICONQUESTION)
        };
        match r {
            IDYES => self.save(false),
            IDNO => true,
            _ => false,
        }
    }
}

impl App for Paint {
    fn draw(&mut self, p: &Painter, f: &Fonts, w: f32, h: f32, kit: &mut Kit) {
        // The bar: file, tools, size, colours, undo, zoom.
        p.rrect(0.0, 0.0, w, BAR, 0.0, Color(0.14, 0.14, 0.145, 1.0));
        let mut x = 8.0;
        for (id, g) in [(T_NEW, 0xE710), (T_OPEN, 0xE8E5), (T_SAVE, 0xE74E), (T_SAVE_AS, 0xE792)] {
            kit.button(p, f, x, 14.0, 36.0, 36.0, "", Some(g), Look::Plain, id);
            x += 38.0;
        }
        p.line(x + 4.0, 18.0, x + 4.0, 46.0, Color::white(0.12), 1.0);
        x += 12.0;
        for (i, (t, g, _)) in TOOLS.iter().enumerate() {
            let look = if self.tool == *t { Look::Accent } else { Look::Plain };
            // Fill is drawn as a bucket of the colour in use: the icon font has none.
            let glyph = if *t == Tool::Fill { None } else { Some(*g) };
            kit.button(p, f, x, 14.0, 36.0, 36.0, "", glyph, look, T_TOOL + i as u32);
            if *t == Tool::Fill {
                bucket(p, x + 18.0, 32.0, colour_of(self.colour));
            }
            x += 38.0;
        }
        if matches!(self.tool, Tool::Rect | Tool::Ellipse) {
            let look = if self.fill_shapes { Look::Accent } else { Look::Plain };
            kit.button(
                p,
                f,
                x,
                14.0,
                70.0,
                36.0,
                if self.fill_shapes { "Filled" } else { "Outline" },
                None,
                look,
                T_FILLED,
            );
            x += 72.0;
        }
        p.line(x + 4.0, 18.0, x + 4.0, 46.0, Color::white(0.12), 1.0);
        x += 12.0;
        kit.button(p, f, x, 14.0, 30.0, 36.0, "", Some(0xE738), Look::Plain, T_SIZE_DOWN);
        p.circle(x + 50.0, 32.0, (self.size / 2.0).clamp(1.5, 14.0), Color::white(0.9));
        p.text(&format!("{:.0}", self.size), &f.small, x + 32.0, 46.0, 36.0, 14.0, Align::Center, Color::white(0.5));
        kit.button(p, f, x + 70.0, 14.0, 30.0, 36.0, "", Some(0xE710), Look::Plain, T_SIZE_UP);
        x += 108.0;
        // The colour in use, then the palette in two rows.
        p.rrect(x, 14.0, 36.0, 36.0, 8.0, colour_of(self.colour));
        p.stroke_rrect(x + 0.5, 14.5, 35.0, 35.0, 8.0, Color::white(0.4), 1.0);
        x += 44.0;
        for (i, c) in PALETTE.iter().enumerate() {
            let (cx, cy) = (x + (i / 2) as f32 * 22.0, 12.0 + (i % 2) as f32 * 22.0);
            p.circle(cx + 9.0, cy + 9.0, 8.0, colour_of(*c));
            p.stroke_circle(cx + 9.0, cy + 9.0, 8.0, Color::white(if *c == self.colour { 0.9 } else { 0.2 }), 1.0);
            kit.hit(cx, cy, 20.0, 20.0, T_COLOUR + i as u32);
        }
        x += 7.0 * 22.0 + 4.0;
        kit.button(p, f, x, 14.0, 36.0, 36.0, "", Some(0xE790), Look::Plain, T_MORE);
        // The picture's own menu: resize canvas, paste, copy, rotate, flip.
        kit.button(p, f, x + 40.0, 14.0, 36.0, 36.0, "", Some(0xE712), Look::Plain, T_IMAGE);
        // Undo, redo and zoom on the right.
        let rx = w - 4.0 * 38.0 - 8.0;
        kit.button(p, f, rx, 14.0, 36.0, 36.0, "", Some(0xE7A7), Look::Plain, T_UNDO);
        kit.button(p, f, rx + 38.0, 14.0, 36.0, 36.0, "", Some(0xE7A6), Look::Plain, T_REDO);
        kit.button(p, f, rx + 76.0, 14.0, 36.0, 36.0, "", Some(0xE71F), Look::Plain, T_ZOOM_OUT);
        kit.button(p, f, rx + 114.0, 14.0, 36.0, 36.0, "", Some(0xE8A3), Look::Plain, T_ZOOM_IN);
        // The canvas, fitted (or at the zoom asked for), with a shadow.
        let (aw, ah) = (w - 40.0, h - BAR - 56.0);
        let (cw, ch) = (self.canvas.w as f32, self.canvas.h as f32);
        let fit = (aw / cw).min(ah / ch).min(1.0);
        let s = if self.zoom > 0.0 { self.zoom } else { fit };
        let (dw, dh) = (cw * s, ch * s);
        let (dx, dy) = (20.0 + (aw - dw).max(0.0) / 2.0, BAR + 20.0 + (ah - dh).max(0.0) / 2.0);
        self.view = (dx, dy, s);
        for i in 1..=8 {
            let d = i as f32 * 1.5;
            p.rrect(dx - d, dy - d + 3.0, dw + 2.0 * d, dh + 2.0 * d, d, Color(0.0, 0.0, 0.0, 0.05));
        }
        if self.bmp.is_none() {
            self.bmp = p.bitmap(self.canvas.w as u32, self.canvas.h as u32, &self.canvas.bytes());
        }
        if let Some(b) = &self.bmp {
            p.clip(20.0, BAR + 20.0, aw, ah);
            p.image(b, dx, dy, dw, dh, 1.0);
            p.unclip();
        }
        // A pasted picture, over the canvas, with a dashed edge to say it's loose.
        if let Some(fl) = &self.floating {
            if self.float_bmp.is_none() {
                self.float_bmp = p.bitmap(fl.piece.w as u32, fl.piece.h as u32, &fl.piece.bytes());
            }
            let (fx, fy) = (dx + fl.at.0 * s, dy + fl.at.1 * s);
            let (fw, fh) = (fl.size.0 * s, fl.size.1 * s);
            p.clip(20.0, BAR + 20.0, aw, ah);
            if let Some(b) = &self.float_bmp {
                p.image(b, fx, fy, fw, fh, 1.0);
            }
            p.stroke_rrect(fx - 0.5, fy - 0.5, fw + 1.0, fh + 1.0, 0.0, crate::theme::accent(), 1.5);
            p.unclip();
            // Handles to resize it by: the corners and the middle of each side.
            for i in (0..9).filter(|i| *i != 4) {
                let (hx, hy) = (fx + fw * (i % 3) as f32 / 2.0, fy + fh * (i / 3) as f32 / 2.0);
                p.circle(hx, hy, 6.5, Color::white(1.0));
                p.circle(hx, hy, 4.5, crate::theme::accent());
            }
        }
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or("Untitled".into(), |n| n.to_string_lossy().into_owned());
        let status = format!(
            "{}{name}   \u{b7}   {} \u{d7} {}   \u{b7}   {:.0}%",
            if self.dirty { "\u{2022} " } else { "" },
            self.canvas.w,
            self.canvas.h,
            s * 100.0
        );
        let status = if self.floating.is_some() {
            format!("{status}   \u{b7}   Drag the picture into place or by its handles to resize it (Shift for any shape), Enter puts it down, Esc drops it")
        } else {
            status
        };
        p.text(&status, &f.small, 20.0, h - 28.0, w - 40.0, 24.0, Align::Left, Color::white(0.5));
        if let Some(d) = &self.dialog {
            self.draw_dialog(p, f, w, h, kit, d);
        }
    }

    fn press(&mut self, id: u32, ctx: &Ctx) {
        if self.dialog.is_some() {
            match id {
                T_DLG_OK => self.apply_dialog(),
                T_DLG_CANCEL => self.dialog = None,
                T_DLG_W | T_DLG_H => {
                    if let Some(d) = &mut self.dialog {
                        d.field = (id - T_DLG_W) as usize;
                    }
                }
                T_DLG_STRETCH => {
                    if let Some(d) = &mut self.dialog {
                        d.stretch = !d.stretch;
                    }
                }
                _ => {}
            }
            ctx.redraw();
            return;
        }
        // A pasted picture is put down before anything else happens (undo drops it).
        if self.floating.is_some()
            && !matches!(id, T_ZOOM_OUT | T_ZOOM_IN | T_SIZE_DOWN | T_SIZE_UP | T_FILLED | T_MORE | T_IMAGE | T_UNDO)
            && !(T_COLOUR..T_COLOUR + 14).contains(&id)
        {
            self.commit();
        }
        match id {
            T_UNDO if self.floating.is_some() => {
                self.floating = None;
                self.float_bmp = None;
            }
            T_IMAGE => self.image_menu(),
            T_NEW if self.keep_going() => {
                self.canvas = Canvas::new(1280, 720);
                self.undo.clear();
                self.redo.clear();
                self.path = None;
                self.dirty = false;
                self.changed();
            }
            T_OPEN if self.keep_going() => {
                if let Some(p) = super::pick_file(
                    "Open a picture",
                    &[("Pictures", "*.png;*.jpg;*.jpeg;*.bmp;*.gif;*.tif;*.tiff;*.webp;*.heic")],
                ) {
                    self.open(p);
                }
            }
            T_SAVE => {
                self.save(false);
            }
            T_SAVE_AS => {
                self.save(true);
            }
            T_UNDO => {
                if let Some(px) = self.undo.pop() {
                    self.redo.push(std::mem::replace(&mut self.canvas.px, px));
                    self.changed();
                }
            }
            T_REDO => {
                if let Some(px) = self.redo.pop() {
                    self.undo.push(std::mem::replace(&mut self.canvas.px, px));
                    self.changed();
                }
            }
            T_ZOOM_OUT => self.zoom = (if self.zoom > 0.0 { self.zoom } else { self.view.2 } / 1.25).max(0.05),
            T_ZOOM_IN => self.zoom = (if self.zoom > 0.0 { self.zoom } else { self.view.2 } * 1.25).min(16.0),
            T_SIZE_DOWN => self.size = (self.size - if self.size > 10.0 { 4.0 } else { 1.0 }).max(1.0),
            T_SIZE_UP => self.size = (self.size + if self.size >= 10.0 { 4.0 } else { 1.0 }).min(80.0),
            T_FILLED => self.fill_shapes = !self.fill_shapes,
            T_MORE => {
                if let Some(c) = pick(ctx.hwnd, self.colour) {
                    self.colour = c;
                }
            }
            x if (T_TOOL..T_TOOL + 8).contains(&x) => self.tool = TOOLS[(x - T_TOOL) as usize].0,
            x if (T_COLOUR..T_COLOUR + 14).contains(&x) => self.colour = PALETTE[(x - T_COLOUR) as usize],
            _ => {}
        }
        ctx.redraw();
    }

    fn mouse(&mut self, ev: Mouse, x: f32, y: f32, _: &Ctx) -> bool {
        let at = self.to_canvas(x, y);
        let r = self.size / 2.0;
        let c = if self.tool == Tool::Eraser { 0xFFFFFF } else { self.colour };
        if self.dialog.is_some() {
            return false;
        }
        // A pasted picture: dragged where it's held; a click elsewhere puts it down.
        let (vx, vy, vs) = self.view;
        if let Some(fl) = &mut self.floating {
            match ev {
                Mouse::Down if y >= BAR => {
                    if let Some(i) = fl.handle_at(vx, vy, vs, x, y) {
                        fl.resize = Some((i, (fl.at.0, fl.at.1, fl.at.0 + fl.size.0, fl.at.1 + fl.size.1)));
                        return true;
                    }
                    let inside = at.0 >= fl.at.0
                        && at.1 >= fl.at.1
                        && at.0 < fl.at.0 + fl.size.0
                        && at.1 < fl.at.1 + fl.size.1;
                    if inside {
                        fl.grab = Some((at.0 - fl.at.0, at.1 - fl.at.1));
                        return true;
                    }
                    self.commit();
                }
                Mouse::Move if fl.resize.is_some() => {
                    if let Some((i, start)) = fl.resize {
                        let free = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                        fl.drag_handle(i, start, at, free);
                    }
                    return true;
                }
                Mouse::Move if fl.grab.is_some() => {
                    if let Some((gx, gy)) = fl.grab {
                        fl.at = (at.0 - gx, at.1 - gy);
                    }
                    return true;
                }
                Mouse::Up if fl.grab.is_some() || fl.resize.is_some() => {
                    fl.grab = None;
                    fl.resize = None;
                    return true;
                }
                _ => {}
            }
        }
        match ev {
            Mouse::Down => {
                if y < BAR {
                    return false;
                }
                match self.tool {
                    Tool::Fill => {
                        self.snapshot();
                        self.canvas.fill(at.0 as i32, at.1 as i32, self.colour);
                    }
                    Tool::Picker => {
                        if let Some(p) = self.canvas.get(at.0 as i32, at.1 as i32) {
                            self.colour = p & 0xFF_FFFF;
                            self.tool = Tool::Pencil;
                        }
                    }
                    _ => {
                        self.snapshot();
                        self.before = Some(self.canvas.px.clone());
                        self.drag = Some((at, at));
                        if matches!(self.tool, Tool::Pencil | Tool::Brush | Tool::Eraser) {
                            self.canvas.dab(at.0, at.1, r, c, self.tool == Tool::Brush);
                        }
                    }
                }
                self.changed();
                true
            }
            Mouse::Move => {
                let Some((from, last)) = self.drag else { return false };
                match self.tool {
                    Tool::Pencil | Tool::Brush | Tool::Eraser => {
                        self.canvas.stroke(last, at, r, c, self.tool == Tool::Brush);
                        self.drag = Some((from, at));
                    }
                    Tool::Line | Tool::Rect | Tool::Ellipse => {
                        // Drawn afresh over what was there, as it's dragged.
                        if let Some(b) = &self.before {
                            self.canvas.px.copy_from_slice(b);
                        }
                        match self.tool {
                            Tool::Line => self.canvas.stroke(from, at, r, c, false),
                            Tool::Rect => self.canvas.rect(from, at, r, c, self.fill_shapes),
                            _ => self.canvas.ellipse(from, at, r, c, self.fill_shapes),
                        }
                        self.drag = Some((from, at));
                    }
                    _ => return false,
                }
                self.changed();
                true
            }
            Mouse::Up => {
                self.drag = None;
                self.before = None;
                true
            }
            Mouse::RightDown => false,
        }
    }

    fn wheel(&mut self, delta: i32, _: &Ctx) -> bool {
        if unsafe { GetKeyState(VK_CONTROL.0 as i32) } >= 0 {
            return false;
        }
        let base = if self.zoom > 0.0 { self.zoom } else { self.view.2 };
        self.zoom = (base * if delta > 0 { 1.15 } else { 1.0 / 1.15 }).clamp(0.05, 16.0);
        true
    }

    fn key(&mut self, vk: u16, ctx: &Ctx) -> bool {
        // The Resize canvas box takes the keys while it's open.
        if let Some(d) = &mut self.dialog {
            match vk {
                v if v == VK_RETURN.0 => self.apply_dialog(),
                v if v == VK_ESCAPE.0 => self.dialog = None,
                v if v == VK_TAB.0 => d.field = 1 - d.field,
                v if v == VK_BACK.0 => {
                    d.size[d.field].pop();
                }
                _ => {}
            }
            return true;
        }
        if self.floating.is_some() {
            match vk {
                v if v == VK_RETURN.0 => {
                    self.commit();
                    return true;
                }
                v if v == VK_ESCAPE.0 || v == 0x2E => {
                    self.floating = None;
                    self.float_bmp = None;
                    return true;
                }
                _ => {}
            }
        }
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        if !ctrl {
            // [ and ] change the size of the pen.
            return match vk {
                0xDB => {
                    self.press(T_SIZE_DOWN, ctx);
                    true
                }
                0xDD => {
                    self.press(T_SIZE_UP, ctx);
                    true
                }
                _ => false,
            };
        }
        match vk {
            0x43 => self.copy(),
            0x56 => self.paste(),
            // Ctrl + and -, Ctrl+0 to fit.
            0xBB | 0x6B => self.press(T_ZOOM_IN, ctx),
            0xBD | 0x6D => self.press(T_ZOOM_OUT, ctx),
            0x30 | 0x60 => self.zoom = 0.0,
            0x53 if shift => self.press(T_SAVE_AS, ctx),
            _ => {
                let id = match vk as u8 {
                    b'Z' => T_UNDO,
                    b'Y' => T_REDO,
                    b'S' => T_SAVE,
                    b'O' => T_OPEN,
                    b'N' => T_NEW,
                    _ => return false,
                };
                self.press(id, ctx);
                return true;
            }
        }
        true
    }

    fn char(&mut self, c: char, _: &Ctx) -> bool {
        let Some(d) = &mut self.dialog else { return false };
        if c.is_ascii_digit() && d.size[d.field].len() < 4 {
            d.size[d.field].push(c);
        }
        true
    }

    fn closing(&mut self, _: &Ctx) -> bool {
        self.keep_going()
    }
}

/// The Fill tool's icon: a bucket tipped to pour, a drop of the colour in use
/// coming out of it.
fn bucket(p: &Painter, cx: f32, cy: f32, paint: Color) {
    let ink = Color::white(0.95);
    p.rotated(20.0, cx - 1.0, cy + 1.0, || {
        // The handle, an arch over the rim.
        p.line(cx - 7.5, cy - 4.0, cx - 4.5, cy - 9.5, ink, 1.4);
        p.line(cx - 4.5, cy - 9.5, cx + 4.5, cy - 9.5, ink, 1.4);
        p.line(cx + 4.5, cy - 9.5, cx + 7.5, cy - 4.0, ink, 1.4);
        // The bucket, wider at the rim, with the paint showing in it.
        p.polygon(&[(cx - 8.5, cy - 4.5), (cx + 8.5, cy - 4.5), (cx + 6.0, cy + 8.0), (cx - 6.0, cy + 8.0)], ink);
        p.rrect(cx - 7.2, cy - 4.0, 14.4, 2.6, 1.3, paint);
    });
    p.circle(cx + 11.0, cy + 6.5, 2.4, paint);
}

/// Windows' colour picker, starting at `c` (0xRRGGBB).
fn pick(owner: HWND, c: u32) -> Option<u32> {
    use windows::Win32::UI::Controls::Dialogs::*;
    let mut custom = [0x00FF_FFFFu32; 16];
    let bgr = |c: u32| ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF);
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: COLORREF(bgr(c)),
        lpCustColors: custom.as_mut_ptr() as *mut COLORREF,
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    unsafe { ChooseColorW(&mut cc).as_bool() }.then(|| bgr(cc.rgbResult.0))
}

/// `cascade.exe --paint [picture]`.
pub fn run(file: Option<String>) {
    super::run(w!("CascadePaint"), "Paint", "Cascade.Paint", 1280, 820, |ctx| {
        let mut p = Paint {
            canvas: Canvas::new(1280, 720),
            undo: Vec::new(),
            redo: Vec::new(),
            tool: Tool::Brush,
            colour: 0x000000,
            size: 6.0,
            fill_shapes: false,
            zoom: 0.0,
            view: (0.0, 0.0, 1.0),
            drag: None,
            before: None,
            bmp: None,
            path: None,
            dirty: false,
            hwnd: ctx.hwnd,
            floating: None,
            float_bmp: None,
            dialog: None,
        };
        if let Some(f) = file.filter(|f| !f.is_empty()) {
            p.open(PathBuf::from(f));
        }
        Box::new(p)
    });
}
