//! Notepad (`cascade.exe --notepad [file]`), as Windows' own: a plain text
//! editor with New, Open, Save and Save as (UTF-8, reading UTF-8, UTF-16 and
//! ANSI files), word wrap, zoom, Ctrl+N/O/S, and the line and column it's at.
//! Closing with changes asks to save them.

use std::path::PathBuf;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_CONTROL};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::{App, Ctx, Kit, Look};
use crate::gfx::{Align, Color, Fonts, Painter};

const BAR: f32 = 46.0;
const FOOT: f32 = 26.0;
const ID_EDIT: u16 = 10;
const EN_CHANGE: u16 = 0x0300;

struct Notepad {
    edit: HWND,
    font: HFONT,
    brush: HBRUSH,
    path: Option<PathBuf>,
    dirty: bool,
    wrap: bool,
    zoom: i32,
    hwnd: HWND,
}

fn read_text(path: &PathBuf) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let w: Vec<u16> = bytes[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
        return String::from_utf16_lossy(&w);
    }
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        // ANSI: Windows-1252 as near enough (Latin-1 for the rest).
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

impl Notepad {
    fn make_edit(&mut self) {
        unsafe {
            let text = self.text();
            if !self.edit.is_invalid() {
                let _ = DestroyWindow(self.edit);
            }
            let mut style = WS_CHILD | WS_VISIBLE | WS_VSCROLL | WINDOW_STYLE(0x0004 | 0x0040 | 0x1000); // ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN
            if !self.wrap {
                style |= WS_HSCROLL | WINDOW_STYLE(0x0080); // ES_AUTOHSCROLL
            }
            self.edit = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                &HSTRING::from(text.replace('\n', "\r\n").replace("\r\r\n", "\r\n")),
                style,
                0,
                0,
                10,
                10,
                Some(self.hwnd),
                Some(HMENU(ID_EDIT as usize as *mut _)),
                Some(crate::appwin::instance()),
                None,
            )
            .unwrap_or_default();
            SendMessageW(self.edit, 0x00C5, Some(WPARAM(0)), Some(LPARAM(0))); // EM_LIMITTEXT: no limit
            self.set_font();
            self.place();
            let _ = SetFocus(Some(self.edit));
        }
    }

    fn set_font(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font.into());
            }
            let k = crate::appwin::scale(self.hwnd);
            let size = (15.0 * k * (1.0 + self.zoom as f32 * 0.1)) as i32;
            self.font = CreateFontW(
                -size,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Consolas"),
            );
            SendMessageW(self.edit, WM_SETFONT, Some(WPARAM(self.font.0 as usize)), Some(LPARAM(1)));
        }
    }

    fn place(&self) {
        let k = crate::appwin::scale(self.hwnd);
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
            let top = (BAR * k) as i32;
            let bottom = r.bottom - (FOOT * k) as i32;
            let _ = MoveWindow(self.edit, (8.0 * k) as i32, top, r.right - (8.0 * k) as i32, bottom - top, true);
        }
    }

    fn text(&self) -> String {
        if self.edit.is_invalid() {
            return String::new();
        }
        unsafe {
            let n = GetWindowTextLengthW(self.edit) as usize;
            let mut buf = vec![0u16; n + 1];
            let got = GetWindowTextW(self.edit, &mut buf) as usize;
            String::from_utf16_lossy(&buf[..got])
        }
    }

    fn title(&self) {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or("Untitled".into(), |n| n.to_string_lossy().into_owned());
        let t = format!("{}{name} \u{2014} Notepad", if self.dirty { "\u{2022} " } else { "" });
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(t));
        }
    }

    fn open(&mut self, path: PathBuf) {
        let text = read_text(&path);
        unsafe {
            let _ = SetWindowTextW(self.edit, &HSTRING::from(text.replace("\r\n", "\n").replace('\n', "\r\n")));
        }
        self.path = Some(path);
        self.dirty = false;
        self.title();
    }

    /// Saves (asking where if it's new, or `ask`). False if it wasn't.
    fn save(&mut self, ask: bool) -> bool {
        let path = match (&self.path, ask) {
            (Some(p), false) => p.clone(),
            _ => {
                let name = self
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map_or("Untitled.txt".into(), |n| n.to_string_lossy().into_owned());
                match super::save_path(self.hwnd, &name, &[("Text files", "*.txt"), ("All files", "*.*")], "txt") {
                    Some(p) => p,
                    None => return false,
                }
            }
        };
        let ok = std::fs::write(&path, self.text().replace("\r\n", "\n").replace('\n', "\r\n")).is_ok();
        if ok {
            self.path = Some(path);
            self.dirty = false;
            self.title();
        }
        ok
    }

    /// Changes not saved: save them? False to stay.
    fn keep_going(&mut self) -> bool {
        if !self.dirty {
            return true;
        }
        let r = unsafe {
            MessageBoxW(Some(self.hwnd), w!("Save your changes?"), w!("Notepad"), MB_YESNOCANCEL | MB_ICONQUESTION)
        };
        match r {
            IDYES => self.save(false),
            IDNO => true,
            _ => false,
        }
    }

    fn caret(&self) -> (usize, usize) {
        unsafe {
            let mut start = 0u32;
            SendMessageW(self.edit, 0x00B0, Some(WPARAM(&mut start as *mut _ as usize)), Some(LPARAM(0))); // EM_GETSEL
            let line = SendMessageW(self.edit, 0x00C9, Some(WPARAM(start as usize)), Some(LPARAM(0))).0 as usize; // EM_LINEFROMCHAR
            let first = SendMessageW(self.edit, 0x00BB, Some(WPARAM(line)), Some(LPARAM(0))).0 as usize; // EM_LINEINDEX
            (line + 1, start as usize - first.min(start as usize) + 1)
        }
    }
}

impl App for Notepad {
    fn draw(&mut self, p: &Painter, f: &Fonts, w: f32, h: f32, kit: &mut Kit) {
        p.rrect(0.0, 0.0, w, BAR, 0.0, Color(0.14, 0.14, 0.145, 1.0));
        let items: [(&str, u32, u32); 4] =
            [("New", 0xE710, 1), ("Open", 0xE8E5, 2), ("Save", 0xE74E, 3), ("Save as", 0xE792, 4)];
        let mut x = 8.0;
        for (label, g, id) in items {
            let bw = p.measure(label, &f.body) + 44.0;
            kit.button(p, f, x, 7.0, bw, 32.0, label, Some(g), Look::Plain, id);
            x += bw + 2.0;
        }
        p.line(x + 6.0, 14.0, x + 6.0, 32.0, Color::white(0.12), 1.0);
        x += 14.0;
        let wrap_label = if self.wrap { "Word wrap: on" } else { "Word wrap: off" };
        let bw = p.measure(wrap_label, &f.body) + 44.0;
        kit.button(p, f, x, 7.0, bw, 32.0, wrap_label, Some(0xE751), Look::Plain, 5);
        x += bw + 2.0;
        kit.button(p, f, x, 7.0, 34.0, 32.0, "", Some(0xE71F), Look::Plain, 6);
        kit.button(p, f, x + 36.0, 7.0, 34.0, 32.0, "", Some(0xE8A3), Look::Plain, 7);
        // The status line.
        let (line, col) = self.caret();
        let chars = unsafe { GetWindowTextLengthW(self.edit) };
        p.rrect(0.0, h - FOOT, w, FOOT, 0.0, Color(0.14, 0.14, 0.145, 1.0));
        let status = format!(
            "Ln {line}, Col {col}   \u{b7}   {chars} characters   \u{b7}   {}%   \u{b7}   UTF-8",
            100 + self.zoom * 10
        );
        p.text(&status, &f.small, 14.0, h - FOOT, w - 28.0, FOOT, Align::Left, Color::white(0.55));
    }

    fn press(&mut self, id: u32, ctx: &Ctx) {
        match id {
            1 if self.keep_going() => {
                unsafe {
                    let _ = SetWindowTextW(self.edit, w!(""));
                }
                self.path = None;
                self.dirty = false;
                self.title();
            }
            2 if self.keep_going() => {
                if let Some(p) = super::pick_file(
                    "Open",
                    &[("Text files", "*.txt;*.log;*.md;*.ini;*.cfg;*.json;*.xml;*.csv"), ("All files", "*.*")],
                ) {
                    self.open(p);
                }
            }
            3 => {
                self.save(false);
            }
            4 => {
                self.save(true);
            }
            5 => {
                self.wrap = !self.wrap;
                self.make_edit();
            }
            6 => {
                self.zoom = (self.zoom - 1).max(-5);
                self.set_font();
            }
            7 => {
                self.zoom = (self.zoom + 1).min(20);
                self.set_font();
            }
            _ => {}
        }
        unsafe {
            let _ = SetFocus(Some(self.edit));
        }
        ctx.redraw();
    }

    fn command(&mut self, id: u16, code: u16, ctx: &Ctx) {
        if id == ID_EDIT && code == EN_CHANGE {
            if !self.dirty {
                self.dirty = true;
                self.title();
            }
            ctx.redraw();
        }
    }

    fn size(&mut self, _: &Ctx) {
        self.place();
    }

    fn tick_ms(&self) -> u32 {
        // The caret's place in the status line.
        300
    }

    fn tick(&mut self, _: &Ctx) -> bool {
        true
    }

    fn message(&mut self, m: u32, _: WPARAM, _: LPARAM, ctx: &Ctx) -> bool {
        // Ctrl+N, O, S from the text box (see `hook`).
        match m {
            x if x == WM_APP + 1 => self.press(1, ctx),
            x if x == WM_APP + 2 => self.press(2, ctx),
            x if x == WM_APP + 3 => self.press(3, ctx),
            x if x == WM_APP + 4 => self.press(4, ctx),
            _ => return false,
        }
        true
    }

    fn closing(&mut self, _: &Ctx) -> bool {
        self.keep_going()
    }

    fn edit_brush(&self) -> Option<HBRUSH> {
        Some(self.brush)
    }
}

/// Ctrl+N, O and S typed in the text box go to the window as its own messages.
unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let msg = &*(lp.0 as *const MSG);
    if code >= 0 && msg.message == WM_KEYDOWN && GetKeyState(VK_CONTROL.0 as i32) < 0 {
        let shift = GetKeyState(0x10) < 0;
        let which = match (msg.wParam.0 as u8, shift) {
            (b'N', _) => Some(1),
            (b'O', _) => Some(2),
            (b'S', false) => Some(3),
            (b'S', true) => Some(4),
            _ => None,
        };
        if let Some(n) = which {
            let root = GetAncestor(msg.hwnd, GA_ROOT);
            let _ = PostMessageW(Some(root), WM_APP + n, WPARAM(0), LPARAM(0));
            // Swallowed: the text box doesn't type it.
            let m = lp.0 as *mut MSG;
            (*m).message = WM_NULL;
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// `cascade.exe --notepad [file]`.
pub fn run(file: Option<String>) {
    super::run(w!("CascadeNotepad"), "Notepad", "Cascade.Notepad", 900, 680, |ctx| {
        let mut n = Notepad {
            edit: HWND::default(),
            font: HFONT::default(),
            brush: unsafe { CreateSolidBrush(COLORREF(0x00222020)) },
            path: None,
            dirty: false,
            wrap: true,
            zoom: 0,
            hwnd: ctx.hwnd,
        };
        n.make_edit();
        if let Some(f) = file.filter(|f| !f.is_empty()) {
            n.open(PathBuf::from(f));
        }
        n.title();
        unsafe {
            let tid = windows::Win32::System::Threading::GetCurrentThreadId();
            let _ = SetWindowsHookExW(WH_GETMESSAGE, Some(hook), None, tid);
        }
        let _ = PCWSTR::null();
        Box::new(n)
    });
}
