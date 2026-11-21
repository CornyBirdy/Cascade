//! Calculator (`cascade.exe --calc`), as Windows' own: Standard and
//! Scientific, the answer worked out as you type, a history to pick an
//! earlier answer from, and the keyboard (digits, + - * / ^ ( ) %, Enter,
//! Backspace, Esc, Delete).

use windows::core::w;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

use super::{App, Ctx, Kit, Look, PANEL};
use crate::gfx::{Align, Color, Fonts, Painter};
use crate::smart::{eval, number};

/// A key: what it shows and what it does.
#[derive(Clone, Copy)]
enum K {
    /// Typed as it is.
    Put(&'static str),
    Clear,
    ClearEntry,
    Back,
    Equals,
    Negate,
    /// Wraps the last number: "sqrt(", "1/(", or squares it.
    Wrap(&'static str),
    Square,
    Percent,
    Mode,
    ClearHistory,
}

const STANDARD: [[(&str, K); 4]; 6] = [
    [("%", K::Percent), ("CE", K::ClearEntry), ("C", K::Clear), ("\u{232b}", K::Back)],
    [
        ("\u{215f}x", K::Wrap("1/(")),
        ("x\u{b2}", K::Square),
        ("\u{221a}x", K::Wrap("sqrt(")),
        ("\u{f7}", K::Put("\u{f7}")),
    ],
    [("7", K::Put("7")), ("8", K::Put("8")), ("9", K::Put("9")), ("\u{d7}", K::Put("\u{d7}"))],
    [("4", K::Put("4")), ("5", K::Put("5")), ("6", K::Put("6")), ("\u{2212}", K::Put("\u{2212}"))],
    [("1", K::Put("1")), ("2", K::Put("2")), ("3", K::Put("3")), ("+", K::Put("+"))],
    [("\u{b1}", K::Negate), ("0", K::Put("0")), (".", K::Put(".")), ("=", K::Equals)],
];

const SCIENTIFIC: [[(&str, K); 4]; 2] = [
    [("sin", K::Put("sin(")), ("cos", K::Put("cos(")), ("tan", K::Put("tan(")), ("x^y", K::Put("^"))],
    [("ln", K::Put("ln(")), ("log", K::Put("log(")), ("\u{3c0}", K::Put("\u{3c0}")), ("e", K::Put("e"))],
];

const PARENS: [(&str, K); 2] = [("(", K::Put("(")), (")", K::Put(")"))];

struct Calc {
    expr: String,
    /// The answer last worked out with = (shown big until something's typed).
    shown: Option<String>,
    history: Vec<(String, String)>,
    scientific: bool,
    keys: Vec<K>,
}

fn is_num(c: char) -> bool {
    c.is_ascii_digit() || c == '.'
}

impl Calc {
    /// Where the last number starts in the expression.
    fn last_number(&self) -> usize {
        let b: Vec<char> = self.expr.chars().collect();
        let mut i = b.len();
        while i > 0 && is_num(b[i - 1]) {
            i -= 1;
        }
        b[..i].iter().map(|c| c.len_utf8()).sum()
    }

    fn preview(&self) -> Option<String> {
        if self.expr.is_empty() {
            return None;
        }
        // Brackets left open are closed for the preview.
        let open = self.expr.matches('(').count().saturating_sub(self.expr.matches(')').count());
        let full = format!("{}{}", self.expr, ")".repeat(open));
        eval(&full).map(number)
    }

    fn put(&mut self, s: &str) {
        if self.shown.is_some() {
            // After =: an operator goes on from the answer, anything else starts afresh.
            let op = matches!(s, "+" | "\u{2212}" | "\u{d7}" | "\u{f7}" | "^");
            if !op {
                self.expr.clear();
            }
            self.shown = None;
        }
        self.expr.push_str(s);
    }

    fn act(&mut self, k: K) {
        match k {
            K::Put(s) => self.put(s),
            K::Clear => {
                self.expr.clear();
                self.shown = None;
            }
            K::ClearEntry => {
                let i = self.last_number();
                self.expr.truncate(i);
                self.shown = None;
            }
            K::Back => {
                if self.shown.take().is_none() {
                    self.expr.pop();
                }
            }
            K::Equals => {
                if let Some(v) = self.preview() {
                    let open = self.expr.matches('(').count().saturating_sub(self.expr.matches(')').count());
                    let full = format!("{}{}", self.expr, ")".repeat(open));
                    self.history.insert(0, (full, v.clone()));
                    self.history.truncate(40);
                    self.expr = v.replace(',', "").replace("\u{d7}10^", "e");
                    self.shown = Some(v);
                }
            }
            K::Negate => {
                let i = self.last_number();
                if i < self.expr.len() {
                    let n = self.expr[i..].to_string();
                    self.expr.truncate(i);
                    if self.expr.ends_with("(\u{2212}") {
                        self.expr.truncate(self.expr.len() - "(\u{2212}".len());
                        self.expr.push_str(&n);
                    } else {
                        self.expr.push_str(&format!("(\u{2212}{n}"));
                    }
                }
                self.shown = None;
            }
            K::Wrap(f) => {
                let i = self.last_number();
                let n = self.expr[i..].to_string();
                self.expr.truncate(i);
                self.expr.push_str(&format!("{f}{n})"));
                self.shown = None;
            }
            K::Square => {
                if !self.expr.is_empty() {
                    self.expr.push_str("^2");
                }
                self.shown = None;
            }
            K::Percent => {
                let i = self.last_number();
                if i < self.expr.len() {
                    let n = self.expr[i..].to_string();
                    self.expr.truncate(i);
                    self.expr.push_str(&format!("({n}/100)"));
                }
                self.shown = None;
            }
            K::Mode => self.scientific = !self.scientific,
            K::ClearHistory => self.history.clear(),
        }
    }
}

const HIST: u32 = 1000;

impl App for Calc {
    fn draw(&mut self, p: &Painter, f: &Fonts, w: f32, h: f32, kit: &mut Kit) {
        self.keys.clear();
        let accent = crate::theme::accent();
        // The history down the right, when there's room.
        let hist_w = if w > 620.0 { 260.0 } else { 0.0 };
        let cw = w - hist_w;
        let pad = 12.0;
        // The mode, the expression and the answer.
        let mode = if self.scientific { "Scientific" } else { "Standard" };
        kit.button(p, f, pad, 8.0, 130.0, 32.0, mode, Some(0xE700), Look::Plain, 900);
        self.keys.push(K::Mode);
        let expr = if self.shown.is_some() {
            self.history.first().map(|h| format!("{} =", h.0)).unwrap_or_default()
        } else {
            self.expr.clone()
        };
        p.text(&expr, &f.body, pad, 52.0, cw - 2.0 * pad, 24.0, Align::Right, Color::white(0.55));
        let big = match (&self.shown, self.preview()) {
            (Some(s), _) => s.clone(),
            (None, Some(v)) if !self.expr.is_empty() => v,
            _ if self.expr.is_empty() => "0".into(),
            _ => self.expr.clone(),
        };
        p.scaled(2.4, cw - pad, 100.0, || {
            p.text(
                &big,
                &f.big,
                cw - pad - (cw - 2.0 * pad) / 2.4,
                100.0 - 18.0,
                (cw - 2.0 * pad) / 2.4,
                36.0,
                Align::Right,
                Color::white(0.97),
            );
        });
        // The keys.
        let mut rows: Vec<Vec<(&str, K)>> = Vec::new();
        if self.scientific {
            for r in SCIENTIFIC {
                rows.push(r.to_vec());
            }
            rows.push(vec![PARENS[0], PARENS[1], ("n!", K::Put("")), ("|x|", K::Wrap("abs("))]);
        }
        for r in STANDARD {
            rows.push(r.to_vec());
        }
        let top = 150.0;
        let gap = 4.0;
        let kh = ((h - top - pad) / rows.len() as f32 - gap).max(30.0);
        let kw = (cw - 2.0 * pad - 3.0 * gap) / 4.0;
        for (ri, row) in rows.iter().enumerate() {
            for (ci, (label, k)) in row.iter().enumerate() {
                if label.is_empty() || (*label == "n!") {
                    continue;
                }
                let x = pad + ci as f32 * (kw + gap);
                let y = top + ri as f32 * (kh + gap);
                let digit = label.chars().all(|c| c.is_ascii_digit()) || *label == "." || *label == "\u{b1}";
                let look = match k {
                    K::Equals => Look::Accent,
                    _ if digit => Look::Key,
                    _ => Look::Plain,
                };
                let id = self.keys.len() as u32;
                if !digit && look == Look::Plain {
                    p.rrect(x, y, kw, kh, 6.0, Color::white(0.05));
                }
                kit.button(p, f, x, y, kw, kh, label, None, look, id);
                self.keys.push(*k);
            }
        }
        // History.
        if hist_w > 0.0 {
            let hx = cw;
            p.rrect(hx, 0.0, hist_w, h, 0.0, PANEL);
            p.text("History", &f.title, hx + 16.0, 12.0, hist_w - 70.0, 26.0, Align::Left, Color::white(0.92));
            if !self.history.is_empty() {
                kit.button(p, f, hx + hist_w - 44.0, 10.0, 32.0, 30.0, "", Some(0xE74D), Look::Plain, HIST - 1);
            }
            if self.history.is_empty() {
                p.text(
                    "There's no history yet",
                    &f.small,
                    hx + 16.0,
                    52.0,
                    hist_w - 32.0,
                    20.0,
                    Align::Left,
                    Color::white(0.45),
                );
            }
            for (i, (e, v)) in self.history.iter().enumerate() {
                let y = 52.0 + i as f32 * 58.0;
                if y > h - 58.0 {
                    break;
                }
                let (hot, _) = kit.lit(HIST + i as u32);
                if hot {
                    p.rrect(hx + 8.0, y, hist_w - 16.0, 54.0, 6.0, Color::white(0.06));
                }
                p.text(
                    &format!("{e} ="),
                    &f.small,
                    hx + 16.0,
                    y + 6.0,
                    hist_w - 32.0,
                    18.0,
                    Align::Right,
                    Color::white(0.55),
                );
                p.text(v, &f.title, hx + 16.0, y + 24.0, hist_w - 32.0, 26.0, Align::Right, Color::white(0.95));
                kit.hit(hx + 8.0, y, hist_w - 16.0, 54.0, HIST + i as u32);
            }
            let _ = accent;
        }
    }

    fn press(&mut self, id: u32, _: &Ctx) {
        match id {
            900 => self.act(K::Mode),
            x if x == HIST - 1 => self.act(K::ClearHistory),
            x if x >= HIST => {
                if let Some((_, v)) = self.history.get((x - HIST) as usize) {
                    self.expr = v.replace(',', "");
                    self.shown = None;
                }
            }
            x => {
                if let Some(k) = self.keys.get(x as usize).copied() {
                    self.act(k);
                }
            }
        }
    }

    fn key(&mut self, vk: u16, _: &Ctx) -> bool {
        let k = match VIRTUAL_KEY(vk) {
            VK_RETURN => K::Equals,
            VK_BACK => K::Back,
            VK_ESCAPE => K::Clear,
            VK_DELETE => K::ClearEntry,
            _ => return false,
        };
        self.act(k);
        true
    }

    fn char(&mut self, c: char, _: &Ctx) -> bool {
        let s = match c {
            '0'..='9' | '.' | '(' | ')' | '^' => c.to_string(),
            '+' => "+".into(),
            '-' => "\u{2212}".into(),
            '*' | 'x' => "\u{d7}".into(),
            '/' => "\u{f7}".into(),
            '%' => {
                self.act(K::Percent);
                return true;
            }
            '=' => {
                self.act(K::Equals);
                return true;
            }
            _ => return false,
        };
        self.put(&s);
        true
    }
}

/// `cascade.exe --calc`.
pub fn run() {
    super::run(w!("CascadeCalculator"), "Calculator", "Cascade.Calculator", 640, 600, |_| {
        Box::new(Calc { expr: String::new(), shown: None, history: Vec::new(), scientific: false, keys: Vec::new() })
    });
}
