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

const BAR: f32 = 50.0;
const PAD: f32 = 16.0;
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

    fn buttons(&self) -> Vec<(Button, RECT, &'static str)> {
        let k = self.k();
        // (button, width, label, gap after it)
        let mut list: Vec<(Button, f32, &'static str, f32)> = vec![
            (Button::Tool(Tool::Pen), 56.0, "Pen", 4.0),
            (Button::Tool(Tool::Highlighter), 84.0, "Highlighter", 4.0),
            (Button::Tool(Tool::Eraser), 64.0, "Eraser", 4.0),
            (Button::Tool(Tool::Crop), 56.0, "Crop", 12.0),
        ];
        for i in 0..COLOURS.len() {
            list.push((Button::Colour(i), 28.0, "", 4.0));
        }
        list.push((Button::Size, 58.0, "", 12.0));
        list.push((Button::Undo, 54.0, "Undo", 4.0));
        list.push((Button::Redo, 54.0, "Redo", 12.0));
        list.push((Button::Text, 94.0, "Copy text", 4.0));
        list.push((Button::Copy, 54.0, "Copy", 4.0));
        list.push((Button::Save, 76.0, "Save as\u{2026}", 4.0));
        let mut x = 10.0;
        list.into_iter()
            .map(|(b, w, label, gap)| {
                let r = RECT {
                    left: (x * k) as i32,
                    top: (8.0 * k) as i32,
                    right: ((x + w) * k) as i32,
                    bottom: ((BAR - 8.0) * k) as i32,
                };
                x += w + gap;
                (b, r, label)
            })
            .collect()
    }

    /// Where the image is drawn (client pixels) and its scale.
    fn view(&self) -> (f32, f32, f32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let k = self.k();
        let (aw, ah) = (r.right as f32 - 2.0 * PAD * k, r.bottom as f32 - (BAR + 2.0 * PAD + 22.0) * k);
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

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let mem = CreateCompatibleDC(Some(dc));
    let buf = CreateCompatibleBitmap(dc, client.right.max(1), client.bottom.max(1));
    let old = SelectObject(mem, buf.into());
    with(|e| {
        let k = e.k();
        fill(mem, client, 0x00201C1C);
        fill(mem, RECT { bottom: (BAR * k) as i32, ..client }, 0x00302A2A);
        SetBkMode(mem, TRANSPARENT);
        let font = CreateFontW(
            -(13.0 * k) as i32,
            0,
            0,
            0,
            500,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        );
        let of = SelectObject(mem, font.into());
        let accent = crate::theme::accent();
        let accent = ((accent.2 * 255.0) as u32) << 16 | ((accent.1 * 255.0) as u32) << 8 | (accent.0 * 255.0) as u32;
        for (b, r, label) in e.buttons() {
            let on = match b {
                Button::Tool(t) => t == e.tool,
                _ => false,
            };
            if on {
                fill(mem, r, accent);
            } else if e.hot == Some(b) {
                fill(mem, r, 0x00483E3E);
            }
            match b {
                Button::Colour(i) => {
                    let c = (8.0 * k) as i32;
                    let sw =
                        RECT { left: r.left + c / 2, top: r.top + c, right: r.right - c / 2, bottom: r.bottom - c };
                    fill(mem, sw, COLOURS[i]);
                    if i == e.colour {
                        let edge = CreateSolidBrush(COLORREF(0x00FFFFFF));
                        let _ = FrameRect(
                            mem,
                            &RECT { left: sw.left - 2, top: sw.top - 2, right: sw.right + 2, bottom: sw.bottom + 2 },
                            edge,
                        );
                        let _ = DeleteObject(edge.into());
                    }
                }
                Button::Size => {
                    // Three dots, the size in use lit.
                    for (i, d) in [3.0f32, 5.0, 8.0].iter().enumerate() {
                        let cx = r.left + ((12.0 + i as f32 * 16.0) * k) as i32;
                        let cy = (r.top + r.bottom) / 2;
                        let rad = (d * k / 2.0) as i32;
                        let c = if i == e.size { 0x00FFFFFF } else { 0x00807070 };
                        let b = CreateSolidBrush(COLORREF(c));
                        let ob = SelectObject(mem, b.into());
                        let op = SelectObject(mem, GetStockObject(NULL_PEN));
                        let _ = Ellipse(mem, cx - rad, cy - rad, cx + rad + 1, cy + rad + 1);
                        SelectObject(mem, op);
                        SelectObject(mem, ob);
                        let _ = DeleteObject(b.into());
                    }
                }
                _ => {
                    let dim = matches!(b, Button::Undo if e.undo.is_empty())
                        || matches!(b, Button::Redo if e.redo.is_empty());
                    SetTextColor(mem, COLORREF(if dim { 0x00706060 } else { 0x00FFFFFF }));
                    let mut t: Vec<u16> = label.encode_utf16().collect();
                    let mut tr = r;
                    DrawTextW(mem, &mut t, &mut tr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                }
            }
        }
        // The image and the marks on it.
        let (x, y, s) = e.view();
        let (iw, ih) = ((e.image.w as f32 * s).round() as i32, (e.image.h as f32 * s).round() as i32);
        let src = CreateCompatibleDC(Some(mem));
        let os = SelectObject(src, e.bitmap.into());
        SetStretchBltMode(mem, HALFTONE);
        let _ = StretchBlt(mem, x as i32, y as i32, iw, ih, Some(src), 0, 0, e.image.w, e.image.h, SRCCOPY);
        SelectObject(src, os);
        let _ = DeleteDC(src);
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
        // The status line.
        SetTextColor(mem, COLORREF(0x00A09090));
        let mut t: Vec<u16> =
            format!("{}   \u{b7}   {} \u{d7} {}", e.status, e.image.w, e.image.h).encode_utf16().collect();
        let mut tr = RECT {
            left: (PAD * k) as i32,
            top: client.bottom - (24.0 * k) as i32,
            right: client.right,
            bottom: client.bottom,
        };
        DrawTextW(mem, &mut t, &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        SelectObject(mem, of);
        let _ = DeleteObject(font.into());
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
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let p = point(lp);
            let on_bar = with(|e| e.buttons().into_iter().any(|(_, r, _)| inside(&r, p))).unwrap_or(false);
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
