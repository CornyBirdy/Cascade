//! Paint (`cascade.exe --paint [picture]`), as Windows' own: a pencil, a
//! soft brush, an eraser, fill, a colour picker, lines, rectangles and
//! ellipses (filled or not), a palette and any colour, sizes, undo and redo
//! (Ctrl+Z, Ctrl+Y), zoom (Ctrl+wheel, or the buttons), and New, Open, Save
//! and Save as (PNG or JPEG).

use std::path::PathBuf;

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL};

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

    fn save(&mut self, ask: bool) -> bool {
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
            for (i, p) in px.chunks_exact(4).enumerate() {
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
            kit.button(p, f, x, 14.0, 36.0, 36.0, "", Some(*g), look, T_TOOL + i as u32);
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
        p.text(&status, &f.small, 20.0, h - 28.0, w - 40.0, 24.0, Align::Left, Color::white(0.5));
    }

    fn press(&mut self, id: u32, ctx: &Ctx) {
        match id {
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
        if unsafe { GetKeyState(VK_CONTROL.0 as i32) } >= 0 {
            return false;
        }
        let id = match vk as u8 {
            b'Z' => T_UNDO,
            b'Y' => T_REDO,
            b'S' => T_SAVE,
            b'O' => T_OPEN,
            b'N' => T_NEW,
            _ => return false,
        };
        self.press(id, ctx);
        true
    }

    fn closing(&mut self, _: &Ctx) -> bool {
        self.keep_going()
    }
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
        };
        if let Some(f) = file.filter(|f| !f.is_empty()) {
            p.open(PathBuf::from(f));
        }
        Box::new(p)
    });
}
