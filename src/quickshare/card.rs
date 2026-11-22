//! The card Quick Share shows at the bottom right: someone wants to send you
//! something (who, what, the PIN to match with theirs, Accept or Decline),
//! then how far it's got, then what arrived (Open, Show in folder). Drawn in
//! Cascade's look on a layered window of its own thread, as Snip's preview is.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gfx::{Align, Color};
use crate::recui::{at, ease, glass, glyph, layered, mix, register, scale_at, track_leave, work_area, Surface, SHADOW};

/// What the card says.
#[derive(Clone, Debug)]
pub enum View {
    /// `device` wants to send: a summary, the first few names, the PIN.
    Ask {
        device: String,
        summary: String,
        names: Vec<String>,
        pin: String,
    },
    /// Under way: how much of how much.
    Progress {
        title: String,
        done: i64,
        total: i64,
    },
    /// Arrived (or sent): the files, and where they are.
    Done {
        title: String,
        detail: String,
        paths: Vec<PathBuf>,
        link: Option<String>,
    },
    Failed {
        title: String,
        detail: String,
    },
}

#[derive(Default)]
struct Shared {
    view: Option<View>,
    decision: Option<bool>,
    cancelled: bool,
    closed: bool,
}

/// A card on screen; the connection's thread updates it.
#[derive(Clone)]
pub struct Card {
    shared: Arc<Mutex<Shared>>,
    hwnd: Arc<Mutex<isize>>,
}

impl Card {
    pub fn set(&self, v: View) {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).view = Some(v);
        self.poke();
    }

    /// Accept (true) or Decline (false), once one's pressed.
    pub fn decision(&self) -> Option<bool> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).decision
    }

    pub fn cancelled(&self) -> bool {
        let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.cancelled || s.closed
    }

    fn poke(&self) {
        let h = *self.hwnd.lock().unwrap_or_else(|e| e.into_inner());
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_TIMER, WPARAM(1), LPARAM(0));
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Btn {
    Accept,
    Decline,
    Cancel,
    Open,
    Folder,
    Copy,
    Close,
}

const W: f32 = 360.0;

struct Ui {
    s: Surface,
    card: Card,
    shown: Instant,
    last: Instant,
    closing: Option<Instant>,
    /// When it last changed to Done or Failed (it goes by itself after a while).
    finished: Option<Instant>,
    inside: bool,
    hot: Option<Btn>,
    lit: Vec<(Btn, f32)>,
    corner: POINT,
    hits: Vec<(f32, f32, f32, f32, Btn)>,
    copied: Option<Instant>,
}

thread_local! {
    static UI: std::cell::RefCell<Option<Ui>> = const { std::cell::RefCell::new(None) };
}

fn height(v: &View) -> f32 {
    match v {
        View::Ask { names, .. } => 150.0 + names.len().min(3) as f32 * 18.0 + 60.0,
        View::Progress { .. } => 150.0,
        View::Done { .. } => 156.0,
        View::Failed { .. } => 136.0,
    }
}

fn draw(ui: &mut Ui) -> bool {
    let dt = ui.last.elapsed().as_secs_f32().min(0.05);
    ui.last = Instant::now();
    let (view, closed) = {
        let s = ui.card.shared.lock().unwrap_or_else(|e| e.into_inner());
        (s.view.clone(), s.closed)
    };
    let Some(view) = view else { return true };
    let finished = matches!(view, View::Done { .. } | View::Failed { .. });
    if finished && ui.finished.is_none() {
        ui.finished = Some(Instant::now());
    }
    if let Some(t) = ui.finished {
        let stay = if matches!(view, View::Failed { .. }) { 8.0 } else { 12.0 };
        if !ui.inside && t.elapsed().as_secs_f32() > stay && ui.closing.is_none() {
            ui.closing = Some(Instant::now());
        }
    }
    if closed && ui.closing.is_none() {
        ui.closing = Some(Instant::now());
    }
    let out = ui.closing.map_or(0.0, |t| ease(t.elapsed().as_secs_f32() / 0.2));
    if out >= 1.0 {
        return false;
    }
    for (b, a) in &mut ui.lit {
        let to = if ui.hot == Some(*b) { 1.0 } else { 0.0 };
        *a += (to - *a) * (1.0 - (-18.0 * dt).exp());
    }
    let ent = ease(ui.shown.elapsed().as_secs_f32() / 0.3);
    let k = ui.s.k;
    let h = height(&view);
    let (ww, wh) = (W + 2.0 * SHADOW, h + 2.0 * SHADOW);
    let slide = ((1.0 - ent) * 48.0 + out * 24.0) * k;
    let pos = POINT { x: ui.corner.x - (ww * k) as i32 + slide as i32, y: ui.corner.y - (wh * k) as i32 };
    let lit = ui.lit.clone();
    let lit_of = |b: Btn| lit.iter().find(|(x, _)| *x == b).map_or(0.0, |(_, a)| *a);
    let copied = ui.copied.is_some_and(|t| t.elapsed().as_secs_f32() < 1.6);
    let mut hits = Vec::new();
    let accent = crate::theme::accent();
    let alpha = (ent * (1.0 - out)).clamp(0.0, 1.0);
    ui.s.show(pos, ww, wh, alpha, |p, f| {
        let (x, y) = (SHADOW, SHADOW);
        glass(p, x, y, W, h, 12.0, 1.0);
        // Header: Quick Share, and close.
        glyph(p, f, 0xE72D, x + 16.0, y + 14.0, 16.0, Color(0.36, 0.62, 1.0, 1.0));
        p.text("Quick Share", &f.label, x + 40.0, y + 10.0, 200.0, 24.0, Align::Left, Color::white(0.7));
        let (cx, cy) = (x + W - 38.0, y + 8.0);
        p.circle(cx + 14.0, cy + 14.0, 13.0, Color::white(0.08 * lit_of(Btn::Close)));
        glyph(p, f, 0xE711, cx + 7.0, cy + 7.0, 14.0, Color::white(0.75));
        hits.push((cx, cy, 28.0, 28.0, Btn::Close));
        let ty = y + 44.0;
        let mut button = |label: &str, glyph_code: u32, bx: f32, by: f32, bw: f32, b: Btn, main: bool| {
            let l = lit_of(b);
            let fill = if main {
                crate::theme::lighten(accent, 0.12 * l).alpha(0.95)
            } else if b == Btn::Decline || b == Btn::Cancel {
                mix(Color::white(0.08), Color(0.77, 0.17, 0.11, 0.9), l)
            } else {
                Color::white(0.08 + 0.07 * l)
            };
            p.rrect(bx, by, bw, 36.0, 8.0, fill);
            let tw = p.measure(label, &f.body) + if glyph_code != 0 { 22.0 } else { 0.0 };
            let lx = bx + (bw - tw) / 2.0;
            if glyph_code != 0 {
                glyph(p, f, glyph_code, lx, by + 11.0, 14.0, Color::white(0.95));
            }
            let tx = if glyph_code != 0 { lx + 22.0 } else { lx };
            p.text(label, &f.body, tx, by, tw, 36.0, Align::Left, Color::white(0.96));
            hits.push((bx, by, bw, 36.0, b));
        };
        let half = (W - 32.0 - 8.0) / 2.0;
        match &view {
            View::Ask { device, summary, names, pin } => {
                p.text(
                    &format!("{device} wants to share"),
                    &f.title,
                    x + 16.0,
                    ty,
                    W - 32.0,
                    22.0,
                    Align::Left,
                    Color::white(0.96),
                );
                p.text(summary, &f.small, x + 16.0, ty + 24.0, W - 32.0, 18.0, Align::Left, Color::white(0.6));
                let mut ny = ty + 46.0;
                for n in names.iter().take(3) {
                    glyph(p, f, 0xE8A5, x + 18.0, ny + 2.0, 12.0, Color::white(0.5));
                    p.text(n, &f.small, x + 38.0, ny, W - 56.0, 18.0, Align::Left, Color::white(0.82));
                    ny += 18.0;
                }
                // The PIN, to match with the one on their screen.
                let py = ny + 10.0;
                p.rrect(x + 16.0, py, W - 32.0, 40.0, 8.0, Color::white(0.05));
                p.text("PIN on their screen", &f.small, x + 28.0, py, 200.0, 40.0, Align::Left, Color::white(0.6));
                p.text(pin, &f.big, x + W - 140.0, py, 112.0, 40.0, Align::Right, Color::white(0.96));
                let by = py + 54.0;
                button("Decline", 0, x + 16.0, by, half, Btn::Decline, false);
                button("Accept", 0xE73E, x + 16.0 + half + 8.0, by, half, Btn::Accept, true);
            }
            View::Progress { title, done, total } => {
                p.text(title, &f.title, x + 16.0, ty, W - 32.0, 22.0, Align::Left, Color::white(0.96));
                let share = if *total > 0 { (*done as f32 / *total as f32).clamp(0.0, 1.0) } else { 0.0 };
                let detail = format!(
                    "{:.0}% \u{b7} {} of {}",
                    share * 100.0,
                    crate::appwin::size_text(*done as u64),
                    crate::appwin::size_text(*total as u64)
                );
                p.text(&detail, &f.small, x + 16.0, ty + 24.0, W - 32.0, 18.0, Align::Left, Color::white(0.6));
                p.rrect(x + 16.0, ty + 52.0, W - 32.0, 6.0, 3.0, Color::white(0.1));
                p.rrect(x + 16.0, ty + 52.0, ((W - 32.0) * share).max(6.0), 6.0, 3.0, accent);
                button("Cancel", 0, x + W - 16.0 - 110.0, ty + 70.0, 110.0, Btn::Cancel, false);
            }
            View::Done { title, detail, paths, link } => {
                glyph(p, f, 0xE73E, x + 16.0, ty + 3.0, 16.0, Color(0.42, 0.84, 0.52, 1.0));
                p.text(title, &f.title, x + 40.0, ty, W - 56.0, 22.0, Align::Left, Color::white(0.96));
                p.text(detail, &f.small, x + 16.0, ty + 26.0, W - 32.0, 18.0, Align::Left, Color::white(0.6));
                let by = ty + 58.0;
                if link.is_some() {
                    button("Open link", 0xE8A7, x + 16.0, by, half, Btn::Open, true);
                    button(
                        if copied { "Copied" } else { "Copy" },
                        0xE8C8,
                        x + 16.0 + half + 8.0,
                        by,
                        half,
                        Btn::Copy,
                        false,
                    );
                } else if !paths.is_empty() {
                    button("Open", 0xE8E5, x + 16.0, by, half, Btn::Open, true);
                    button("Show in folder", 0xE8B7, x + 16.0 + half + 8.0, by, half, Btn::Folder, false);
                }
            }
            View::Failed { title, detail } => {
                glyph(p, f, 0xE783, x + 16.0, ty + 3.0, 16.0, Color(1.0, 0.46, 0.42, 1.0));
                p.text(title, &f.title, x + 40.0, ty, W - 56.0, 22.0, Align::Left, Color::white(0.96));
                p.text_wrap(detail, &f.small, x + 16.0, ty + 26.0, W - 32.0, 40.0, Color::white(0.6));
            }
        }
    });
    ui.hits = hits;
    true
}

fn press(ui: &mut Ui, b: Btn) {
    let view = ui.card.shared.lock().unwrap_or_else(|e| e.into_inner()).view.clone();
    let mut s = ui.card.shared.lock().unwrap_or_else(|e| e.into_inner());
    match b {
        Btn::Accept => s.decision = Some(true),
        Btn::Decline => {
            s.decision = Some(false);
            s.closed = true;
        }
        Btn::Cancel => s.cancelled = true,
        Btn::Close => {
            // Closing an offer turns it down; closing a transfer cancels it.
            if s.decision.is_none() && matches!(view, Some(View::Ask { .. })) {
                s.decision = Some(false);
            }
            if matches!(view, Some(View::Progress { .. })) {
                s.cancelled = true;
            }
            s.closed = true;
        }
        Btn::Open => match &view {
            Some(View::Done { link: Some(l), .. }) => crate::shell::open(l),
            Some(View::Done { paths, .. }) => {
                if let Some(p) = paths.first() {
                    crate::shell::open(&p.to_string_lossy());
                }
                s.closed = true;
            }
            _ => {}
        },
        Btn::Folder => {
            if let Some(View::Done { paths, .. }) = &view {
                if let Some(dir) = paths.first().and_then(|p| p.parent()) {
                    crate::appwin::launch(&["--files", &dir.to_string_lossy()]);
                }
            }
            s.closed = true;
        }
        Btn::Copy => {
            if let Some(View::Done { link: Some(l), .. }) = &view {
                crate::clipboard::copy(l);
                ui.copied = Some(Instant::now());
            }
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let k = UI.with(|u| u.borrow().as_ref().map_or(1.0, |u| u.s.k));
    let hit = |x: f32, y: f32| {
        UI.with(|u| {
            u.borrow().as_ref().and_then(|u| {
                u.hits.iter().rev().find(|h| x >= h.0 && x < h.0 + h.2 && y >= h.1 && y < h.1 + h.3).map(|h| h.4)
            })
        })
    };
    match m {
        WM_TIMER => {
            let alive = UI.with(|u| match u.try_borrow_mut() {
                Ok(mut u) => u.as_mut().is_none_or(draw),
                Err(_) => true,
            });
            if !alive {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = at(lp, k);
            let h = hit(x, y);
            UI.with(|u| {
                if let Some(u) = u.borrow_mut().as_mut() {
                    u.hot = h;
                    u.inside = true;
                }
            });
            track_leave(hwnd);
            LRESULT(0)
        }
        0x02A3 => {
            // WM_MOUSELEAVE
            UI.with(|u| {
                if let Some(u) = u.borrow_mut().as_mut() {
                    u.hot = None;
                    u.inside = false;
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = at(lp, k);
            if let Some(b) = hit(x, y) {
                UI.with(|u| {
                    if let Some(u) = u.borrow_mut().as_mut() {
                        press(u, b);
                    }
                });
            }
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
            let over = hit(pt.x as f32 / k, pt.y as f32 / k).is_some();
            SetCursor(LoadCursorW(None, if over { IDC_HAND } else { IDC_ARROW }).ok());
            LRESULT(1)
        }
        WM_DESTROY => {
            UI.with(|u| {
                if let Some(u) = u.borrow().as_ref() {
                    let mut s = u.card.shared.lock().unwrap_or_else(|e| e.into_inner());
                    s.closed = true;
                    if s.decision.is_none() {
                        s.decision = Some(false);
                    }
                }
            });
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// Puts up a card showing `view`, on a thread of its own. Returns at once.
pub fn show(view: View) -> Card {
    let card = Card {
        shared: Arc::new(Mutex::new(Shared { view: Some(view), ..Default::default() })),
        hwnd: Arc::new(Mutex::new(0)),
    };
    let c = card.clone();
    std::thread::spawn(move || unsafe {
        register(w!("CascadeQuickShareCard"), Some(proc));
        let mut cur = POINT::default();
        let _ = GetCursorPos(&mut cur);
        let near = RECT { left: cur.x, top: cur.y, right: cur.x + 1, bottom: cur.y + 1 };
        let k = scale_at(&near);
        let work = work_area(&near);
        let margin = (16.0 * k) as i32;
        let corner =
            POINT { x: work.right - margin + (SHADOW * k) as i32, y: work.bottom - margin + (SHADOW * k) as i32 };
        let Some(hwnd) = layered(w!("CascadeQuickShareCard"), WS_EX_NOACTIVATE, corner.x, corner.y) else { return };
        let Ok(gfx) = crate::gfx::Gfx::new() else { return };
        *c.hwnd.lock().unwrap_or_else(|e| e.into_inner()) = hwnd.0 as isize;
        let lit = [Btn::Accept, Btn::Decline, Btn::Cancel, Btn::Open, Btn::Folder, Btn::Copy, Btn::Close]
            .into_iter()
            .map(|b| (b, 0.0))
            .collect();
        UI.with(|u| {
            *u.borrow_mut() = Some(Ui {
                s: Surface { hwnd, gfx, layer: Default::default(), k },
                card: c.clone(),
                shown: Instant::now(),
                last: Instant::now(),
                closing: None,
                finished: None,
                inside: false,
                hot: None,
                lit,
                corner,
                hits: Vec::new(),
                copied: None,
            })
        });
        UI.with(|u| {
            if let Some(u) = u.borrow_mut().as_mut() {
                draw(u);
            }
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), 1, 16, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UI.with(|u| u.borrow_mut().take());
        *c.hwnd.lock().unwrap_or_else(|e| e.into_inner()) = 0;
    });
    card
}
