//! The snip editor, as the Snipping Tool's: a snip opens here to mark up
//! (a pen and a highlighter in six colours and three sizes, an eraser that
//! takes away whole strokes), crop, copy the text out of (Windows' own text
//! recognition, which works offline), copy, or save as PNG or JPEG. Undo
//! and redo (Ctrl+Z, Ctrl+Y), Ctrl+C and Ctrl+S work as there. The snip
//! was already saved in Screenshots; closing after changes asks whether to
//! save them over it.
//!
//! Each editor is a window of its own on a thread of its own.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;

use windows::core::{w, HSTRING, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{FileSaveDialog, IFileSaveDialog, SIGDN_FILESYSPATH};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config;

const BAR: f32 = 56.0;
const PAD: f32 = 20.0;
/// The status line along the bottom.
const FOOT: f32 = 30.0;
/// Hover fades.
const ANIM: usize = 7;
/// The editor's own message: text recognised (a boxed `Result<String, String>`).
const WM_TEXT: u32 = WM_APP + 1;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tool {
    Pen,
    Highlighter,
    Eraser,
    Crop,
}

/// The colours, as COLORREF (0x00BBGGRR): black, red, blue, green, yellow, white.
const COLOURS: [u32; 6] = [0x00202020, 0x003C3CE8, 0x00D47800, 0x0040A000, 0x0000E0FF, 0x00FFFFFF];
/// Stroke widths (image pixels) for the three sizes; the highlighter's are wider.
const SIZES: [f32; 3] = [2.0, 4.0, 8.0];

#[derive(Clone)]
struct Stroke {
    highlighter: bool,
    colour: u32,
    width: f32,
    points: Vec<(f32, f32)>,
}

struct Image {
    w: i32,
    h: i32,
    pixels: Vec<u32>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Button {
    Tool(Tool),
    Colour(usize),
    Size,
    Undo,
    Redo,
    Text,
    Copy,
    Save,
}

struct Editor {
    hwnd: HWND,
    image: Rc<Image>,
    /// The image as a bitmap to draw from.
    bitmap: HBITMAP,
    strokes: Vec<Stroke>,
    undo: Vec<(Rc<Image>, Vec<Stroke>)>,
    redo: Vec<(Rc<Image>, Vec<Stroke>)>,
    tool: Tool,
    colour: usize,
    size: usize,
    drawing: Option<Stroke>,
    crop: Option<((f32, f32), (f32, f32))>,
    hot: Option<Button>,
    saved: Option<PathBuf>,
    /// Changed since it was last saved.
    dirty: bool,
    status: String,
    /// Direct2D, for the bar and the frame round the snip (taken while painting).
    gfx: Option<crate::gfx::Gfx>,
    /// How lit each recently hovered button is.
    glow: Vec<(Button, f32)>,
    last: std::time::Instant,
}

thread_local! {
    static EDITOR: RefCell<Option<Editor>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Editor) -> R) -> Option<R> {
    EDITOR.with(|e| e.try_borrow_mut().ok().and_then(|mut e| e.as_mut().map(f)))
}

unsafe fn bitmap_of(img: &Image) -> HBITMAP {
    match crate::snip::dib(img.w, img.h) {
        Some((b, px)) => {
            // Shown over the editor's background: see-through parts as white.
            for (d, s) in px.iter_mut().zip(&img.pixels) {
                *d = if s >> 24 == 0 { 0xFFFF_FFFF } else { *s };
            }
            b
        }
        None => HBITMAP::default(),
    }
}

/// Opens `pixels` (w×h, top-down BGRA; `saved` where it was saved) in a new editor.
pub fn open(pixels: Vec<u32>, w: i32, h: i32, saved: Option<PathBuf>) {
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(module) = GetModuleHandleW(None) else { return };
        let instance: HINSTANCE = module.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: crate::shell::app_icon(32).unwrap_or_default(),
            lpszClassName: w!("CascadeSnipEditor"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // Big enough for the snip, within most of the screen.
        let work = crate::shell::work_area();
        let (ww, wh) = (work.right - work.left, work.bottom - work.top);
        let width = (w + 80).clamp(720, ww * 9 / 10);
        let height = (h + 160).clamp(480, wh * 9 / 10);
        let x = work.left + (ww - width) / 2;
        let y = work.top + (wh - height) / 2;
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("CascadeSnipEditor"),
            w!("Snip \u{2014} Cascade"),
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(instance),
            None,
        ) else {
            return;
        };
        let image = Rc::new(Image { w, h, pixels });
        let bitmap = bitmap_of(&image);
        let status = match &saved {
            Some(_) => "Copied, and saved in Screenshots".to_string(),
            None => "Copied".to_string(),
        };
        EDITOR.with(|e| {
            *e.borrow_mut() = Some(Editor {
                hwnd,
                image,
                bitmap,
                strokes: Vec::new(),
                undo: Vec::new(),
                redo: Vec::new(),
                tool: Tool::Pen,
                colour: 1,
                size: 1,
                drawing: None,
                crop: None,
                hot: None,
                saved,
                dirty: false,
                status,
                gfx: crate::gfx::Gfx::new().ok(),
                glow: Vec::new(),
                last: std::time::Instant::now(),
            })
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        EDITOR.with(|e| {
            if let Some(ed) = e.borrow_mut().take() {
                let _ = DeleteObject(ed.bitmap.into());
            }
        });
    });
}

impl Editor {
    fn k(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd).max(96) as f32 / 96.0 }
    }

    /// The bar's buttons (client pixels) and their labels: the tools, the
    /// colours and the size on the left, what to do with the snip on the right.
    fn buttons(&self) -> Vec<(Button, RECT, &'static str)> {
        let k = self.k();
        let mut client = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        let width = client.right as f32 / k;
        // (button, width, label, gap after it)
        let mut left: Vec<(Button, f32, &'static str, f32)> = vec![
            (Button::Tool(Tool::Pen), 74.0, "Pen", 4.0),
            (Button::Tool(Tool::Highlighter), 112.0, "Highlighter", 4.0),
            (Button::Tool(Tool::Eraser), 86.0, "Eraser", 4.0),
            (Button::Tool(Tool::Crop), 74.0, "Crop", 18.0),
        ];
        for i in 0..COLOURS.len() {
            left.push((Button::Colour(i), 30.0, "", if i + 1 == COLOURS.len() { 10.0 } else { 2.0 }));
        }
        left.push((Button::Size, 64.0, "", 0.0));
        let right: Vec<(Button, f32, &'static str, f32)> = vec![
            (Button::Undo, 36.0, "", 2.0),
            (Button::Redo, 36.0, "", 14.0),
            (Button::Text, 108.0, "Copy text", 4.0),
            (Button::Copy, 78.0, "Copy", 4.0),
            (Button::Save, 100.0, "Save as\u{2026}", 0.0),
        ];
        let rect = |x: f32, w: f32| RECT {
            left: (x * k) as i32,
            top: (10.0 * k) as i32,
            right: ((x + w) * k) as i32,
            bottom: ((BAR - 10.0) * k) as i32,
        };
        let mut out = Vec::new();
        let mut x = 12.0;
        for (b, w, label, gap) in left {
            out.push((b, rect(x, w), label));
            x += w + gap;
        }
        let used = x;
        let total: f32 = right.iter().map(|r| r.1 + r.3).sum();
        let mut x = (width - 12.0 - total).max(used + 16.0);
        for (b, w, label, gap) in right {
            out.push((b, rect(x, w), label));
            x += w + gap;
        }
        out
    }

    /// How lit `b` is (0 to 1).
    fn lit(&self, b: Button) -> f32 {
        self.glow.iter().find(|(g, _)| *g == b).map_or(0.0, |g| g.1)
    }

    /// Moves the hover fades on; true while any is still moving.
    fn fade(&mut self) -> bool {
        let dt = self.last.elapsed().as_secs_f32().min(0.05);
        self.last = std::time::Instant::now();
        if let Some(h) = self.hot {
            if !self.glow.iter().any(|(g, _)| *g == h) {
                self.glow.push((h, 0.0));
            }
        }
        let hot = self.hot;
        for (g, a) in &mut self.glow {
            let to = if Some(*g) == hot { 1.0 } else { 0.0 };
            *a = if to > *a { (*a + dt / 0.1).min(1.0) } else { (*a - dt / 0.18).max(0.0) };
        }
        self.glow.retain(|(g, a)| *a > 0.0 || Some(*g) == hot);
        self.glow.iter().any(|(g, a)| (Some(*g) == hot && *a < 1.0) || (Some(*g) != hot && *a > 0.0))
    }

    /// Where the image is drawn (client pixels) and its scale.
    fn view(&self) -> (f32, f32, f32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let k = self.k();
        let (aw, ah) = (r.right as f32 - 2.0 * PAD * k, r.bottom as f32 - (BAR + 2.0 * PAD + FOOT) * k);
        let (iw, ih) = (self.image.w as f32, self.image.h as f32);
        let s = (aw / iw).min(ah / ih).clamp(0.05, 1.0);
        let x = (r.right as f32 - iw * s) / 2.0;
        let y = (BAR + PAD) * k + (ah - ih * s).max(0.0) / 2.0;
        (x, y, s)
    }

    fn to_image(&self, p: POINT) -> (f32, f32) {
        let (x, y, s) = self.view();
        ((p.x as f32 - x) / s, (p.y as f32 - y) / s)
    }

    fn snapshot(&mut self) {
        self.undo.push((self.image.clone(), self.strokes.clone()));
        if self.undo.len() > 50 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    fn set_image(&mut self, img: Rc<Image>) {
        unsafe {
            let _ = DeleteObject(self.bitmap.into());
            self.bitmap = bitmap_of(&img);
        }
        self.image = img;
    }

    /// The image with the strokes drawn in.
    fn compose(&self) -> Image {
        let img = &self.image;
        unsafe {
            let Some((bmp, px)) = crate::snip::dib(img.w, img.h) else {
                return Image { w: img.w, h: img.h, pixels: img.pixels.clone() };
            };
            px.copy_from_slice(&img.pixels);
            let dc = CreateCompatibleDC(None);
            let old = SelectObject(dc, bmp.into());
            for s in &self.strokes {
                draw_stroke(dc, s, 0.0, 0.0, 1.0);
            }
            let _ = GdiFlush();
            SelectObject(dc, old);
            let _ = DeleteDC(dc);
            // GDI leaves what it drew see-through: drawn pixels and the snip's own are opaque.
            let pixels: Vec<u32> = px
                .iter()
                .zip(&img.pixels)
                .map(|(&d, &o)| if o >> 24 != 0 || (d & 0xFF_FFFF) != (o & 0xFF_FFFF) { d | 0xFF00_0000 } else { 0 })
                .collect();
            let _ = DeleteObject(bmp.into());
            Image { w: img.w, h: img.h, pixels }
        }
    }

    fn erase_at(&mut self, p: (f32, f32)) -> bool {
        let (_, _, s) = self.view();
        let reach = 10.0 / s;
        let before = self.strokes.len();
        let near = |q: &(f32, f32)| (q.0 - p.0).hypot(q.1 - p.1) <= reach;
        let keep: Vec<Stroke> = self.strokes.iter().filter(|st| !st.points.iter().any(near)).cloned().collect();
        if keep.len() != before {
            self.snapshot();
            self.strokes = keep;
            true
        } else {
            false
        }
    }

    fn apply_crop(&mut self, a: (f32, f32), b: (f32, f32)) {
        let img = self.compose();
        let l = a.0.min(b.0).max(0.0) as i32;
        let t = a.1.min(b.1).max(0.0) as i32;
        let r = (a.0.max(b.0) as i32).min(img.w);
        let bt = (a.1.max(b.1) as i32).min(img.h);
        if r - l < 4 || bt - t < 4 {
            return;
        }
        let mut pixels = Vec::with_capacity(((r - l) * (bt - t)) as usize);
        for y in t..bt {
            let row = (y * img.w) as usize;
            pixels.extend_from_slice(&img.pixels[row + l as usize..row + r as usize]);
        }
        self.snapshot();
        self.strokes.clear();
        self.set_image(Rc::new(Image { w: r - l, h: bt - t, pixels }));
    }

    fn undo(&mut self) {
        if let Some((img, strokes)) = self.undo.pop() {
            self.redo.push((self.image.clone(), std::mem::replace(&mut self.strokes, strokes)));
            self.set_image(img);
            self.dirty = true;
        }
    }

    fn redo(&mut self) {
        if let Some((img, strokes)) = self.redo.pop() {
            self.undo.push((self.image.clone(), std::mem::replace(&mut self.strokes, strokes)));
            self.set_image(img);
            self.dirty = true;
        }
    }
}

unsafe fn pen(colour: u32, width: f32, highlighter: bool) -> HPEN {
    let lb = LOGBRUSH { lbStyle: BS_SOLID, lbColor: COLORREF(colour), lbHatch: 0 };
    let w = (if highlighter { width * 4.0 } else { width }).max(1.0) as u32;
    ExtCreatePen(PS_GEOMETRIC | PS_SOLID | PS_ENDCAP_ROUND | PS_JOIN_ROUND, w, &lb, None)
}

/// Draws a stroke, its image coordinates placed at (x, y) and scaled by `s`.
unsafe fn draw_stroke(dc: HDC, st: &Stroke, x: f32, y: f32, s: f32) {
    if st.points.is_empty() {
        return;
    }
    let p = pen(st.colour, st.width * s, st.highlighter);
    let old = SelectObject(dc, p.into());
    // The highlighter tints what's under it (a pen through a coloured filter), as a marker does.
    let rop = SetROP2(dc, if st.highlighter { R2_MASKPEN } else { R2_COPYPEN });
    let pts: Vec<POINT> = st
        .points
        .iter()
        .map(|(px, py)| POINT { x: (x + px * s).round() as i32, y: (y + py * s).round() as i32 })
        .collect();
    if pts.len() == 1 {
        let _ = MoveToEx(dc, pts[0].x, pts[0].y, None);
        let _ = LineTo(dc, pts[0].x + 1, pts[0].y);
    } else {
        let _ = Polyline(dc, &pts);
    }
    SetROP2(dc, R2_MODE(rop));
    SelectObject(dc, old);
    let _ = DeleteObject(p.into());
}

unsafe fn fill(dc: HDC, r: RECT, colour: u32) {
    let b = CreateSolidBrush(COLORREF(colour));
    FillRect(dc, &r, b);
    let _ = DeleteObject(b.into());
}

/// A glyph from Windows' icon font for each button with one.
fn glyph_of(b: Button) -> Option<u32> {
    Some(match b {
        Button::Tool(Tool::Pen) => 0xE70F,
        Button::Tool(Tool::Highlighter) => 0xE7E6,
        Button::Tool(Tool::Eraser) => 0xE75C,
        Button::Tool(Tool::Crop) => 0xE7A8,
        Button::Undo => 0xE7A7,
        Button::Redo => 0xE7A6,
        Button::Text => 0xE8D2,
        Button::Copy => 0xE8C8,
        Button::Save => 0xE74E,
        _ => return None,
    })
}

fn colour_of(c: u32) -> crate::gfx::Color {
    crate::gfx::Color(
        (c & 0xFF) as f32 / 255.0,
        ((c >> 8) & 0xFF) as f32 / 255.0,
        ((c >> 16) & 0xFF) as f32 / 255.0,
        1.0,
    )
}

/// The bar, the frame round the snip and the status line, in Direct2D.
fn paint_chrome(e: &Editor, gfx: &mut crate::gfx::Gfx, dc: HDC, client: RECT) {
    use crate::gfx::{Align, Color};
    let k = e.k();
    let (pw, ph) = (client.right.max(1), client.bottom.max(1));
    let bg = Color(0.085, 0.085, 0.09, 1.0);
    if gfx.begin_dc(dc, pw, ph, k, bg).is_err() {
        return;
    }
    {
        let p = gfx.painter();
        let f = &gfx.fonts;
        let (w, h) = (pw as f32 / k, ph as f32 / k);
        let accent = crate::theme::accent();
        // The bar.
        p.rrect(0.0, 0.0, w, BAR, 0.0, Color(0.125, 0.125, 0.13, 1.0));
        p.line(0.0, BAR - 0.5, w, BAR - 0.5, Color::white(0.07), 1.0);
        let unit = |r: RECT| {
            (r.left as f32 / k, r.top as f32 / k, (r.right - r.left) as f32 / k, (r.bottom - r.top) as f32 / k)
        };
        let buttons = e.buttons();
        for (i, (b, r, label)) in buttons.iter().enumerate() {
            let (x, y, bw, bh) = unit(*r);
            let lit = e.lit(*b);
            // A divider before the colours and between the two sides.
            if i > 0 && matches!(b, Button::Colour(0) | Button::Undo) {
                p.line(x - 9.0, y + 6.0, x - 9.0, y + bh - 6.0, Color::white(0.12), 1.0);
            }
            match b {
                Button::Colour(c) => {
                    let (cx, cy) = (x + bw / 2.0, y + bh / 2.0);
                    if *c == e.colour {
                        p.stroke_circle(cx, cy, 12.0, Color::white(0.9), 2.0);
                    } else if lit > 0.0 {
                        p.stroke_circle(cx, cy, 12.0, Color::white(0.3 * lit), 1.5);
                    }
                    p.circle(cx, cy, 8.5 + lit, colour_of(COLOURS[*c]));
                    p.stroke_circle(cx, cy, 8.5 + lit, Color::white(0.18), 1.0);
                }
                Button::Size => {
                    p.rrect(x, y, bw, bh, bh / 2.0, Color::white(0.06 * lit));
                    for (i, d) in [3.0f32, 5.0, 8.0].iter().enumerate() {
                        let cx = x + 14.0 + i as f32 * 18.0;
                        let c = if i == e.size { Color::white(0.95) } else { Color::white(0.32) };
                        p.circle(cx, y + bh / 2.0, d / 2.0 + 0.5, c);
                    }
                }
                _ => {
                    let on = matches!(b, Button::Tool(t) if *t == e.tool);
                    let primary = *b == Button::Save;
                    let dim = matches!(b, Button::Undo if e.undo.is_empty())
                        || matches!(b, Button::Redo if e.redo.is_empty());
                    if on || primary {
                        let a = if primary { 0.85 + 0.15 * lit } else { 0.9 };
                        p.rrect(x, y, bw, bh, 8.0, accent.alpha(a));
                        p.gradient_rrect(x, y, bw, bh, 8.0, &[(0.0, Color::white(0.16)), (1.0, Color::white(0.0))]);
                    } else if !dim {
                        p.rrect(x, y, bw, bh, 8.0, Color::white(0.08 * lit));
                    }
                    let ink = Color::white(if dim {
                        0.3
                    } else if on || primary {
                        1.0
                    } else {
                        0.86 + 0.14 * lit
                    });
                    let tw = if label.is_empty() { 0.0 } else { p.measure(label, &f.body) + 8.0 };
                    let total = 16.0 + tw;
                    let gx = x + (bw - total) / 2.0;
                    if let (Some(g), Some(font)) = (glyph_of(*b), &f.icons) {
                        let ch = char::from_u32(g).map(String::from).unwrap_or_default();
                        p.text(&ch, font, gx, y, 16.0, bh, Align::Center, ink);
                    }
                    if !label.is_empty() {
                        p.text(label, &f.body, gx + 24.0, y, tw, bh, Align::Left, ink);
                    }
                }
            }
        }
        // The frame round the snip: a soft shadow and a fine edge.
        let (vx, vy, s) = e.view();
        let (ix, iy, iw, ih) = (vx / k, vy / k, e.image.w as f32 * s / k, e.image.h as f32 * s / k);
        for i in 1..=10 {
            let d = i as f32 * 1.6;
            p.rrect(ix - d, iy - d + 4.0, iw + 2.0 * d, ih + 2.0 * d, 4.0 + d, Color(0.0, 0.0, 0.0, 0.05));
        }
        p.stroke_rrect(ix - 0.5, iy - 0.5, iw + 1.0, ih + 1.0, 1.0, Color::white(0.1), 1.0);
        // The status line.
        let fy = h - FOOT;
        p.line(0.0, fy, w, fy, Color::white(0.05), 1.0);
        let dims = format!("{} \u{d7} {}", e.image.w, e.image.h);
        p.text(&e.status, &f.small, PAD, fy, w - 2.0 * PAD - 120.0, FOOT, Align::Left, Color::white(0.6));
        p.text(&dims, &f.small, w - PAD - 120.0, fy, 120.0, FOOT, Align::Right, Color::white(0.45));
    }
    let _ = gfx.end();
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let mem = CreateCompatibleDC(Some(dc));
    let buf = CreateCompatibleBitmap(dc, client.right.max(1), client.bottom.max(1));
    let old = SelectObject(mem, buf.into());
    with(|e| {
        // The chrome first (Direct2D), then the snip and its marks over it (GDI, as they're saved).
        if let Some(mut gfx) = e.gfx.take() {
            paint_chrome(e, &mut gfx, mem, client);
            e.gfx = Some(gfx);
        } else {
            fill(mem, client, 0x00171515);
        }
        let (x, y, s) = e.view();
        let (iw, ih) = ((e.image.w as f32 * s).round() as i32, (e.image.h as f32 * s).round() as i32);
        let src = CreateCompatibleDC(Some(mem));
        let os = SelectObject(src, e.bitmap.into());
        SetStretchBltMode(mem, HALFTONE);
        let _ = StretchBlt(mem, x as i32, y as i32, iw, ih, Some(src), 0, 0, e.image.w, e.image.h, SRCCOPY);
        SelectObject(src, os);
        let _ = DeleteDC(src);
        // Marks stay on the snip.
        let _ = IntersectClipRect(mem, x as i32, y as i32, x as i32 + iw, y as i32 + ih);
        for st in e.strokes.iter().chain(e.drawing.iter()) {
            draw_stroke(mem, st, x, y, s);
        }
        if let Some((a, b)) = e.crop {
            let r = RECT {
                left: (x + a.0.min(b.0) * s) as i32,
                top: (y + a.1.min(b.1) * s) as i32,
                right: (x + a.0.max(b.0) * s) as i32,
                bottom: (y + a.1.max(b.1) * s) as i32,
            };
            let edge = CreateSolidBrush(COLORREF(0x00FFFFFF));
            let _ = FrameRect(mem, &r, edge);
            let _ = FrameRect(
                mem,
                &RECT { left: r.left + 1, top: r.top + 1, right: r.right - 1, bottom: r.bottom - 1 },
                edge,
            );
            let _ = DeleteObject(edge.into());
        }
        let _ = SelectClipRgn(mem, None);
    });
    let _ = BitBlt(dc, 0, 0, client.right, client.bottom, Some(mem), 0, 0, SRCCOPY);
    SelectObject(mem, old);
    let _ = DeleteObject(buf.into());
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}

fn point(lp: LPARAM) -> POINT {
    POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 }
}

fn inside(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// Recognises the text in the (composed) image, on a thread of its own; the
/// editor hears back with `WM_TEXT`.
fn recognise(hwnd: HWND, img: Image) {
    let hwnd = hwnd.0 as isize;
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let result = ocr(&img);
        let ptr = Box::into_raw(Box::new(result));
        unsafe {
            if PostMessageW(Some(HWND(hwnd as *mut c_void)), WM_TEXT, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                drop(Box::from_raw(ptr));
            }
        }
    });
}

fn ocr(img: &Image) -> Result<String, String> {
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Security::Cryptography::CryptographicBuffer;
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|_| "Text recognition isn't installed for your language".to_string())?;
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|p| (p | 0xFF00_0000).to_le_bytes()).collect();
    let buffer = CryptographicBuffer::CreateFromByteArray(&bytes).map_err(|e| e.to_string())?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, img.w, img.h)
        .map_err(|e| e.to_string())?;
    let result = engine.RecognizeAsync(&bitmap).and_then(|op| op.get()).map_err(|e| e.to_string())?;
    let mut lines = Vec::new();
    for line in result.Lines().map_err(|e| e.to_string())? {
        lines.push(line.Text().map_err(|e| e.to_string())?.to_string());
    }
    Ok(lines.join("\r\n"))
}

/// Asks where to save, PNG or JPEG.
unsafe fn ask_path(owner: HWND, suggested: &str) -> Option<PathBuf> {
    let dialog: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
    let filters = [
        COMDLG_FILTERSPEC { pszName: w!("PNG picture"), pszSpec: w!("*.png") },
        COMDLG_FILTERSPEC { pszName: w!("JPEG picture"), pszSpec: w!("*.jpg;*.jpeg") },
    ];
    let _ = dialog.SetFileTypes(&filters);
    let _ = dialog.SetDefaultExtension(w!("png"));
    let _ = dialog.SetFileName(&HSTRING::from(suggested));
    dialog.Show(Some(owner)).ok()?;
    let item = dialog.GetResult().ok()?;
    let name: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
    let path = name.to_string().ok();
    CoTaskMemFree(Some(name.0 as *const c_void));
    path.map(PathBuf::from)
}

unsafe fn press(hwnd: HWND, b: Button) {
    match b {
        Button::Tool(t) => {
            with(|e| {
                e.tool = t;
                // A highlighter in black is no highlighter: start it in yellow.
                if t == Tool::Highlighter && e.colour == 0 {
                    e.colour = 4;
                }
            });
        }
        Button::Colour(i) => {
            with(|e| e.colour = i);
        }
        Button::Size => {
            with(|e| e.size = (e.size + 1) % SIZES.len());
        }
        Button::Undo => {
            with(|e| e.undo());
        }
        Button::Redo => {
            with(|e| e.redo());
        }
        Button::Text => {
            if let Some(img) = with(|e| {
                e.status = "Reading the text\u{2026}".into();
                e.compose()
            }) {
                recognise(hwnd, img);
            }
        }
        Button::Copy => {
            if let Some(img) = with(|e| e.compose()) {
                let ok = crate::snip::copy_image(Some(hwnd), &img.pixels, img.w, img.h);
                with(|e| e.status = if ok { "Copied".into() } else { "Couldn't copy".into() });
            }
        }
        Button::Save => save_as(hwnd),
    }
    let _ = InvalidateRect(Some(hwnd), None, false);
}

unsafe fn save_as(hwnd: HWND) {
    let Some((img, name)) = with(|e| {
        let name = e
            .saved
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Screenshot.png".into());
        (e.compose(), name)
    }) else {
        return;
    };
    if let Some(path) = ask_path(hwnd, &name) {
        let ok = crate::snip::save_image(&path, &img.pixels, img.w, img.h);
        config::log(&format!("snip editor: saved {} ({ok})", path.display()));
        with(|e| {
            e.status = if ok { format!("Saved as {}", path.display()) } else { "Couldn't save it there".into() };
            if ok {
                e.dirty = false;
            }
        });
    }
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_SIZE => {
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let mm = &mut *(lp.0 as *mut MINMAXINFO);
            mm.ptMinTrackSize = POINT { x: 760, y: 420 };
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let p = point(lp);
            let changed = with(|e| {
                let hot = e.buttons().into_iter().find(|(_, r, _)| inside(r, p)).map(|(b, _, _)| b);
                let mut changed = hot != e.hot;
                e.hot = hot;
                let at = e.to_image(p);
                if let Some(st) = &mut e.drawing {
                    st.points.push(at);
                    changed = true;
                }
                if let Some((_, b)) = &mut e.crop {
                    *b = at;
                    changed = true;
                }
                if e.tool == Tool::Eraser && wp.0 & 1 != 0 {
                    changed |= e.erase_at(at);
                }
                changed
            });
            if changed == Some(true) {
                let _ = InvalidateRect(Some(hwnd), None, false);
                SetTimer(Some(hwnd), ANIM, 16, None);
            }
            let mut t = windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT>() as u32,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::TrackMouseEvent(&mut t);
            LRESULT(0)
        }
        0x02A3 => {
            // WM_MOUSELEAVE: the hover fades out.
            if with(|e| e.hot.take().is_some()) == Some(true) {
                SetTimer(Some(hwnd), ANIM, 16, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == ANIM => {
            if with(|e| e.fade()) != Some(true) {
                let _ = KillTimer(Some(hwnd), ANIM);
            }
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let p = point(lp);
            // Marks start on the snip's side of the bar, never on the bar itself.
            let on_bar = with(|e| p.y < (BAR * e.k()) as i32).unwrap_or(true);
            if !on_bar {
                SetCapture(hwnd);
                with(|e| {
                    let at = e.to_image(p);
                    match e.tool {
                        Tool::Pen | Tool::Highlighter => {
                            e.drawing = Some(Stroke {
                                highlighter: e.tool == Tool::Highlighter,
                                colour: COLOURS[e.colour],
                                width: SIZES[e.size],
                                points: vec![at],
                            })
                        }
                        Tool::Eraser => {
                            e.erase_at(at);
                        }
                        Tool::Crop => e.crop = Some((at, at)),
                    }
                });
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            let p = point(lp);
            let button = with(|e| e.buttons().into_iter().find(|(_, r, _)| inside(r, p)).map(|(b, _, _)| b)).flatten();
            let busy = with(|e| e.drawing.is_some() || e.crop.is_some()).unwrap_or(false);
            if let (Some(b), false) = (button, busy) {
                press(hwnd, b);
                return LRESULT(0);
            }
            with(|e| {
                if let Some(st) = e.drawing.take() {
                    e.snapshot();
                    e.strokes.push(st);
                }
                if let Some((a, b)) = e.crop.take() {
                    e.apply_crop(a, b);
                }
            });
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            let b = match (ctrl, wp.0 as u8) {
                (true, b'Z') => Some(Button::Undo),
                (true, b'Y') => Some(Button::Redo),
                (true, b'C') => Some(Button::Copy),
                (true, b'S') => Some(Button::Save),
                _ => None,
            };
            if let Some(b) = b {
                press(hwnd, b);
            }
            LRESULT(0)
        }
        WM_TEXT => {
            let result = Box::from_raw(lp.0 as *mut Result<String, String>);
            let status = match *result {
                Ok(t) if t.trim().is_empty() => "No text found".to_string(),
                Ok(t) => {
                    crate::clipboard::copy(&t);
                    format!("Text copied ({} characters)", t.chars().count())
                }
                Err(e) => e,
            };
            with(|e| e.status = status);
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_CLOSE => {
            // Changes since it was saved: keep them?
            let ask = with(|e| e.dirty).unwrap_or(false);
            if ask {
                let r = MessageBoxW(
                    Some(hwnd),
                    w!("Save your changes to the snip?\n\nYes saves over the copy in Screenshots (and copies it)."),
                    w!("Cascade"),
                    MB_YESNOCANCEL | MB_ICONQUESTION,
                );
                if r == IDCANCEL {
                    return LRESULT(0);
                }
                if r == IDYES {
                    let done = with(|e| {
                        let img = e.compose();
                        let _ = crate::snip::copy_image(Some(hwnd), &img.pixels, img.w, img.h);
                        e.saved.clone().map(|p| crate::snip::save_image(&p, &img.pixels, img.w, img.h))
                    })
                    .flatten();
                    if done.is_none() {
                        save_as(hwnd);
                    }
                }
            }
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}
