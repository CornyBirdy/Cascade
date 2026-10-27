//! Win+V in Cascade's theme: Windows' own clipboard history (the same items
//! as its Win+V, pins and sync included), opened by the text cursor. Click
//! an item, or pick it with the arrow keys and Enter, and it's pasted into
//! the app you were in.
//!
//! The history is read through Windows' clipboard API on a thread of its
//! own, and only while the menu is open; nothing watches the clipboard.

use std::sync::mpsc::{channel, Receiver, Sender};

use windows::core::{Result, HSTRING};
use windows::ApplicationModel::DataTransfer::{
    Clipboard, ClipboardHistoryItem, ClipboardHistoryItemsResultStatus, DataPackageView, SetHistoryItemAsContentStatus,
    StandardDataFormats,
};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::backdrop::Backdrop;
use crate::gfx::{Align, Color, Gfx, Layer};
use crate::msg;
use crate::spring::Spring;
use crate::theme;

/// Most items listed (Windows keeps 25).
const MAX: usize = 25;
const W: f32 = 340.0;
const HEAD: f32 = 40.0;
const ROW: f32 = 50.0;
const PAD: f32 = 6.0;
/// Rows shown at once; the rest scroll.
const VISIBLE: usize = 7;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Text,
    Image,
    Files,
    Other,
}

#[derive(Clone)]
pub struct Clip {
    pub id: String,
    /// The text, on one line (or what it is, for anything else).
    pub text: String,
    pub kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum State {
    /// Being read.
    Loading,
    Ready,
    /// Clipboard history is turned off in Windows' settings.
    Off,
    /// Windows wouldn't hand it over.
    Unavailable,
}

#[derive(Clone)]
pub struct Clips {
    pub state: State,
    /// Newest first.
    pub items: Vec<Clip>,
}

pub enum ClipCmd {
    Refresh,
    /// Put the item back on the clipboard; posts `msg::CLIP_PASTE` when it's there.
    Paste(String),
    Delete(String),
    Clear,
}

/// Starts the (sleeping) reader. Lists arrive as a boxed `Clips` in `msg::CLIPS`.
pub fn spawn() -> Sender<ClipCmd> {
    let (tx, rx) = channel();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        run(rx);
    });
    tx
}

fn send(c: Clips) {
    let ptr = Box::into_raw(Box::new(c));
    if !msg::post(msg::CLIPS, 0, ptr as isize) {
        drop(unsafe { Box::from_raw(ptr) });
    }
}

fn run(rx: Receiver<ClipCmd>) {
    // The items of the last list, to act on by id.
    let mut items: Vec<(String, ClipboardHistoryItem)> = Vec::new();
    while let Ok(cmd) = rx.recv() {
        let find = |id: &str| items.iter().find(|(i, _)| i == id).map(|(_, item)| item.clone());
        match cmd {
            ClipCmd::Refresh => {}
            ClipCmd::Paste(id) => {
                let ok = find(&id).is_some_and(|item| {
                    Clipboard::SetHistoryItemAsContent(&item).is_ok_and(|s| s == SetHistoryItemAsContentStatus::Success)
                });
                msg::post(msg::CLIP_PASTE, ok as usize, 0);
                continue;
            }
            ClipCmd::Delete(id) => {
                if let Some(item) = find(&id) {
                    let _ = Clipboard::DeleteItemFromHistory(&item);
                }
            }
            ClipCmd::Clear => {
                let _ = Clipboard::ClearHistory();
            }
        }
        let (clips, list) =
            read().unwrap_or_else(|_| (Clips { state: State::Unavailable, items: Vec::new() }, Vec::new()));
        items = list;
        send(clips);
    }
}

fn read() -> Result<(Clips, Vec<(String, ClipboardHistoryItem)>)> {
    if !Clipboard::IsHistoryEnabled()? {
        return Ok((Clips { state: State::Off, items: Vec::new() }, Vec::new()));
    }
    let result = Clipboard::GetHistoryItemsAsync()?.get()?;
    let state = match result.Status()? {
        ClipboardHistoryItemsResultStatus::Success => State::Ready,
        ClipboardHistoryItemsResultStatus::ClipboardHistoryDisabled => State::Off,
        _ => State::Unavailable,
    };
    if state != State::Ready {
        return Ok((Clips { state, items: Vec::new() }, Vec::new()));
    }
    let list = result.Items()?;
    let (mut clips, mut items) = (Vec::new(), Vec::new());
    for i in 0..list.Size()?.min(MAX as u32) {
        let Ok(item) = list.GetAt(i) else { continue };
        let Ok(id) = item.Id() else { continue };
        let (kind, text) = item.Content().map(|c| describe(&c)).unwrap_or((Kind::Other, String::new()));
        clips.push(Clip { id: id.to_string(), text, kind });
        items.push((id.to_string(), item));
    }
    Ok((Clips { state: State::Ready, items: clips }, items))
}

/// What's in an item: its text on one line, or what it is.
fn describe(c: &DataPackageView) -> (Kind, String) {
    let has = |format: Result<HSTRING>| format.and_then(|f| c.Contains(&f)).unwrap_or(false);
    if has(StandardDataFormats::Text()) {
        if let Ok(t) = c.GetTextAsync().and_then(|op| op.get()) {
            return (Kind::Text, one_line(&t.to_string()));
        }
    }
    if has(StandardDataFormats::Bitmap()) {
        return (Kind::Image, "Image".into());
    }
    if has(StandardDataFormats::StorageItems()) {
        return (Kind::Files, "Files".into());
    }
    (Kind::Other, "Something else".into())
}

/// `text` on one line, its runs of spaces and line breaks made single
/// spaces, cut short (it's only a preview).
fn one_line(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        if out.len() > 300 {
            break;
        }
    }
    out
}

/// What clicking or a key press asks for.
#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    Paste(String),
    Delete(String),
    Clear,
    /// Open Windows' clipboard settings (to turn history on).
    Settings,
    /// Hand over to Windows' own Win+V.
    Windows,
    Close,
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Item(usize),
    Delete(usize),
    Clear,
    Button,
}

pub struct ClipMenu {
    pub hwnd: HWND,
    layer: Layer,
    backdrop: Backdrop,
    pub open: bool,
    visible: bool,
    reveal: Spring,
    clips: Clips,
    /// The highlighted item, and the first one shown.
    sel: Option<usize>,
    top: usize,
    hovered: Option<Hit>,
    tracking: bool,
    /// Where it opens: just under (x, y), kept on `bounds`.
    anchor: (i32, i32),
    bounds: RECT,
    k: f32,
    acrylic: bool,
    opacity: f32,
    radius: f32,
    pub dirty: bool,
}

impl Default for Clips {
    fn default() -> Self {
        Self { state: State::Loading, items: Vec::new() }
    }
}

impl ClipMenu {
    pub fn new(hwnd: HWND, backdrop: Backdrop) -> Self {
        Self {
            hwnd,
            layer: Layer::default(),
            backdrop,
            open: false,
            visible: false,
            reveal: Spring::new(0.0, 600.0, 44.0),
            clips: Clips { state: State::Loading, items: Vec::new() },
            sel: None,
            top: 0,
            hovered: None,
            tracking: false,
            anchor: (0, 0),
            bounds: RECT::default(),
            k: 1.0,
            acrylic: true,
            opacity: 0.55,
            radius: 8.0,
            dirty: false,
        }
    }

    pub fn configure(&mut self, acrylic: bool, opacity: f32, radius: f32) {
        self.acrylic = theme::style_backdrop(&self.backdrop, acrylic, opacity, radius);
        self.opacity = opacity.clamp(0.0, 1.0);
        self.radius = radius;
        self.dirty = true;
    }

    /// Opens just under screen point (x, y) (the text cursor), kept on `bounds`.
    pub fn show(&mut self, x: i32, y: i32, bounds: RECT, k: f32) {
        self.anchor = (x, y);
        self.bounds = bounds;
        self.k = k;
        self.clips = Clips { state: State::Loading, items: Vec::new() };
        self.sel = None;
        self.top = 0;
        self.hovered = None;
        self.open = true;
        if !self.visible {
            self.visible = true;
            self.reveal.snap(0.0);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOW);
            }
        }
        crate::launcher::force_foreground(self.hwnd);
        self.reveal.set(1.0);
        self.dirty = true;
    }

    pub fn hide(&mut self) {
        crate::mousehook::set_rect(5, None);
        if self.open {
            self.open = false;
            self.reveal.set(0.0);
            self.backdrop.hide();
            self.dirty = true;
        }
    }

    /// A new list from the reader.
    pub fn set(&mut self, clips: Clips) {
        let n = clips.items.len();
        self.clips = clips;
        self.sel = if n == 0 { None } else { Some(self.sel.unwrap_or(0).min(n - 1)) };
        self.top = self.top.min(n.saturating_sub(VISIBLE));
        self.dirty = true;
    }

    fn rows(&self) -> usize {
        self.clips.items.len().clamp(1, VISIBLE)
    }

    fn height(&self) -> f32 {
        HEAD + self.rows() as f32 * ROW + PAD
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        if !(0.0..W).contains(&lx) {
            return None;
        }
        if ly < HEAD {
            let clear = self.clips.state == State::Ready && !self.clips.items.is_empty();
            return (clear && lx > W - 96.0).then_some(Hit::Clear);
        }
        let row = ((ly - HEAD) / ROW) as usize;
        if self.clips.items.is_empty() {
            return matches!(self.clips.state, State::Off | State::Unavailable).then_some(Hit::Button);
        }
        let i = self.top + row;
        if row >= VISIBLE || i >= self.clips.items.len() {
            return None;
        }
        Some(if lx > W - PAD - 36.0 { Hit::Delete(i) } else { Hit::Item(i) })
    }

    pub fn on_move(&mut self, x: i32, y: i32) {
        unsafe {
            if !self.tracking {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                self.tracking = TrackMouseEvent(&mut t).is_ok();
            }
        }
        let hit = self.hit(x, y);
        if let Some(Hit::Item(i) | Hit::Delete(i)) = hit {
            self.sel = Some(i);
        }
        if hit != self.hovered {
            self.hovered = hit;
            self.dirty = true;
        }
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        self.hovered = None;
        self.dirty = true;
    }

    fn id_at(&self, i: usize) -> Option<String> {
        self.clips.items.get(i).map(|c| c.id.clone())
    }

    pub fn on_click(&mut self, x: i32, y: i32) -> Option<Action> {
        match self.hit(x, y)? {
            Hit::Item(i) => self.id_at(i).map(Action::Paste),
            Hit::Delete(i) => self.id_at(i).map(Action::Delete),
            Hit::Clear => Some(Action::Clear),
            Hit::Button if self.clips.state == State::Off => Some(Action::Settings),
            Hit::Button => Some(Action::Windows),
        }
    }

    pub fn on_wheel(&mut self, delta: i16) {
        let max = self.clips.items.len().saturating_sub(VISIBLE);
        self.top = if delta < 0 { (self.top + 1).min(max) } else { self.top.saturating_sub(1) };
        self.dirty = true;
    }

    fn select(&mut self, i: usize) {
        self.sel = Some(i);
        if i < self.top {
            self.top = i;
        } else if i >= self.top + VISIBLE {
            self.top = i + 1 - VISIBLE;
        }
    }

    pub fn on_key(&mut self, vk: u16) -> Option<Action> {
        self.dirty = true;
        let n = self.clips.items.len();
        match VIRTUAL_KEY(vk) {
            VK_ESCAPE => return Some(Action::Close),
            VK_RETURN | VK_SPACE => {
                return match self.clips.state {
                    State::Off => Some(Action::Settings),
                    State::Unavailable => Some(Action::Windows),
                    _ => self.sel.and_then(|i| self.id_at(i)).map(Action::Paste),
                }
            }
            VK_DELETE => return self.sel.and_then(|i| self.id_at(i)).map(Action::Delete),
            _ if n == 0 => {}
            VK_UP => self.select(self.sel.map_or(n - 1, |i| (i + n - 1) % n)),
            VK_DOWN | VK_TAB => self.select(self.sel.map_or(0, |i| (i + 1) % n)),
            VK_HOME => self.select(0),
            VK_END => self.select(n - 1),
            // 1 to 9 paste that item straight away, as in Windows.
            v if (0x31..=0x39).contains(&v.0) => {
                return self.id_at(self.top + (v.0 - 0x31) as usize).map(Action::Paste)
            }
            _ => {}
        }
        None
    }

    pub fn wants_frame(&self) -> bool {
        self.visible && (self.dirty || !self.reveal.settled())
    }

    pub fn step(&mut self, dt: f32) -> bool {
        self.reveal.step(dt)
    }

    pub fn render(&mut self, gfx: &mut Gfx) -> Result<()> {
        self.dirty = false;
        if !self.visible {
            return Ok(());
        }
        if !self.open && self.reveal.value < 0.01 {
            self.visible = false;
            self.layer.release();
            self.backdrop.hide();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return Ok(());
        }
        let k = self.k;
        let h = self.height();
        let (pw, ph) = ((W * k).ceil() as i32, (h * k).ceil() as i32);
        let b = self.bounds;
        let edge = (8.0 * k) as i32;
        let x = self.anchor.0.min(b.right - pw - edge).max(b.left + edge);
        // Under the text cursor, or above it when there's no room below.
        let below = self.anchor.1 + edge;
        let y = if below + ph <= b.bottom - edge { below } else { (self.anchor.1 - 2 * edge - ph).max(b.top + edge) };
        let slide = ((1.0 - self.reveal.value.clamp(0.0, 1.0)) * 10.0 * k) as i32;
        let pos = POINT { x, y: y - slide };
        if self.open {
            crate::mousehook::set_rect(5, Some(RECT { left: x, top: y, right: x + pw, bottom: y + ph }));
        }
        let glass = self.acrylic && self.reveal.value > theme::GLASS_FROM;
        let screen = RECT { left: pos.x, top: pos.y, right: pos.x + pw, bottom: pos.y + ph };
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            theme::panel(&p, 0.0, 0.0, W, h, self.radius, glass, self.opacity, 0.97, 1.0);
            p.text("Clipboard", &f.label, PAD + 12.0, 0.0, 200.0, HEAD, Align::Left, Color::white(0.95));
            let items = &self.clips.items;
            if self.clips.state == State::Ready && !items.is_empty() {
                let hot = self.hovered == Some(Hit::Clear);
                if hot {
                    p.rrect(W - 92.0, 8.0, 80.0, HEAD - 16.0, 6.0, Color::white(0.09));
                }
                let c = Color::white(if hot { 0.95 } else { 0.6 });
                p.text("Clear all", &f.small, W - 92.0, 0.0, 80.0, HEAD, Align::Center, c);
            }
            let message = match self.clips.state {
                State::Loading => Some(("Reading your clipboard\u{2026}", None)),
                State::Off => Some(("Clipboard history is off", Some("Turn it on in Settings"))),
                State::Unavailable => Some(("Windows wouldn't share it", Some("Open Windows' clipboard"))),
                State::Ready if items.is_empty() => Some(("Nothing copied yet", None)),
                State::Ready => None,
            };
            if let Some((text, button)) = message {
                let y0 = HEAD;
                p.text(text, &f.body, PAD + 12.0, y0, W - 24.0, ROW * 0.55, Align::Left, Color::white(0.7));
                if let Some(label) = button {
                    let hot = self.hovered == Some(Hit::Button);
                    let bg = if hot { theme::accent_hover() } else { theme::accent() };
                    let bw = gfx.measure(label, &f.small) + 24.0;
                    p.rrect(PAD + 12.0, y0 + ROW * 0.55, bw, 22.0, 11.0, bg);
                    p.text(label, &f.small, PAD + 12.0, y0 + ROW * 0.55, bw, 22.0, Align::Center, Color::white(1.0));
                }
            }
            for (row, i) in (self.top..items.len()).take(VISIBLE).enumerate() {
                let clip = &items[i];
                let ry = HEAD + row as f32 * ROW;
                if self.sel == Some(i) {
                    p.rrect(PAD, ry + 2.0, W - 2.0 * PAD, ROW - 4.0, 6.0, Color::white(0.09));
                }
                // Its number for the keyboard (1 to 9), as in Windows.
                if row < 9 {
                    p.text(
                        &(row + 1).to_string(),
                        &f.small,
                        PAD + 4.0,
                        ry,
                        18.0,
                        ROW,
                        Align::Center,
                        Color::white(0.35),
                    );
                }
                let dim = if clip.kind == Kind::Text { 0.92 } else { 0.6 };
                let tx = PAD + 26.0;
                p.text_wrap(&clip.text, &f.small, tx, ry + 7.0, W - tx - PAD - 40.0, ROW - 12.0, Color::white(dim));
                if self.sel == Some(i) {
                    // The x that takes it out of the history.
                    let (cx, cy) = (W - PAD - 18.0, ry + ROW / 2.0);
                    if self.hovered == Some(Hit::Delete(i)) {
                        p.circle(cx, cy, 12.0, Color::white(0.12));
                    }
                    let c = Color::white(0.75);
                    p.line(cx - 4.0, cy - 4.0, cx + 4.0, cy + 4.0, c, 1.4);
                    p.line(cx - 4.0, cy + 4.0, cx + 4.0, cy - 4.0, c, 1.4);
                }
            }
            if items.len() > VISIBLE {
                // Where the list is scrolled to.
                let track = VISIBLE as f32 * ROW - 8.0;
                let thumb = track * VISIBLE as f32 / items.len() as f32;
                let at = track * self.top as f32 / items.len() as f32;
                p.rrect(W - 4.0, HEAD + 4.0 + at, 2.5, thumb, 1.2, Color::white(0.25));
            }
        }
        let alpha = (self.reveal.value.clamp(0.0, 1.0) * 255.0) as u8;
        let shown = gfx.present(&self.layer, self.hwnd, pos, pw, ph, alpha);
        if !glass {
            self.backdrop.hold(screen);
        } else if self.backdrop.track(screen, None, self.hwnd) {
            self.dirty = true;
        }
        shown
    }
}

/// Puts `text` on the clipboard (a search answer picked in Start).
pub fn copy(text: &str) {
    use windows::ApplicationModel::DataTransfer::DataPackage;
    let set = || -> Result<()> {
        let package = DataPackage::new()?;
        package.SetText(&HSTRING::from(text))?;
        Clipboard::SetContent(&package)?;
        // Kept on the clipboard after Cascade quits, too.
        Clipboard::Flush()
    };
    let _ = set();
}

/// Where the text cursor is in the window in front (screen px), if it says.
pub fn caret() -> Option<POINT> {
    unsafe {
        let fg = GetForegroundWindow();
        let thread = GetWindowThreadProcessId(fg, None);
        let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        GetGUIThreadInfo(thread, &mut gti).ok()?;
        if gti.hwndCaret.is_invalid() {
            return None;
        }
        let mut p = POINT { x: gti.rcCaret.left, y: gti.rcCaret.bottom };
        let _ = windows::Win32::Graphics::Gdi::ClientToScreen(gti.hwndCaret, &mut p);
        Some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_fit_on_a_line() {
        assert_eq!(one_line("  hello\r\n\tworld  again "), "hello world again");
        assert!(one_line(&"word ".repeat(200)).len() <= 310);
    }
}
