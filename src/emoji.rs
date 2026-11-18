//! The emoji panel (Win+. or Win+;) in shell mode: Windows' own is part of
//! Explorer's text-input host. Emoji by category, the ones you've used
//! lately first, kaomoji and symbols too; pick one (click, or the arrow keys
//! and Enter) and it's typed into the app you were in. Tab steps through
//! the categories, Esc closes it.
//!
//! Windows' panel also searches by name and has GIFs; this one doesn't (the
//! names of every emoji in every language are more than Cascade carries).

use windows::core::Result;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::backdrop::Backdrop;
use crate::gfx::{Align, Color, Gfx, Layer};
use crate::spring::Spring;
use crate::theme;

const COLS: usize = 8;
const CELL: f32 = 40.0;
const PAD: f32 = 10.0;
const TABS: f32 = 38.0;
const ROWS: usize = 6;
const W: f32 = COLS as f32 * CELL + 2.0 * PAD;
const H: f32 = TABS + ROWS as f32 * CELL + PAD;

/// (tab label, items separated by spaces).
const CATEGORIES: [(&str, &str); 9] = [
    ("Recent", ""),
    ("\u{1F600}", "😀 😃 😄 😁 😆 😅 🤣 😂 🙂 🙃 😉 😊 😇 🥰 😍 🤩 😘 😗 😚 😋 😛 😜 🤪 😝 🤑 🤗 🤭 🤫 🤔 🤐 🤨 😐 😑 😶 😏 😒 🙄 😬 😮‍💨 🤥 😌 😔 😪 🤤 😴 😷 🤒 🤕 🤢 🤮 🥵 🥶 🥴 😵 🤯 🤠 🥳 🥸 😎 🤓 🧐 😕 😟 🙁 😮 😯 😲 😳 🥺 😦 😧 😨 😰 😥 😢 😭 😱 😖 😣 😞 😓 😩 😫 🥱 😤 😡 😠 🤬 😈 👿 💀 💩 🤡 👻 👽 🤖 😺 😸 😹 😻 😼 😽 🙀 😿 😾"),
    ("\u{1F44B}", "👋 🤚 🖐️ ✋ 🖖 👌 🤌 🤏 ✌️ 🤞 🤟 🤘 🤙 👈 👉 👆 👇 ☝️ 👍 👎 ✊ 👊 🤛 🤜 👏 🙌 👐 🤲 🤝 🙏 ✍️ 💪 🦾 🧠 👀 👁️ 👅 👄 💋 🫶 👶 🧒 👦 👧 🧑 👱 👨 🧔 👩 🧓 👴 👵 🙍 🙎 🙅 🙆 💁 🙋 🧏 🙇 🤦 🤷 👮 🕵️ 💂 👷 🤴 👸 👳 🧕 🤵 👰 🤰 🤱 👼 🎅 🤶 🦸 🦹 🧙 🧚 🧛 🧜 🧝 🧞 🧟 💆 💇 🚶 🧍 🧎 🏃 💃 🕺"),
    ("\u{2764}\u{FE0F}", "❤️ 🧡 💛 💚 💙 💜 🖤 🤍 🤎 💔 ❣️ 💕 💞 💓 💗 💖 💘 💝 💟 ☮️ ✝️ ☯️ ♈ ♉ ♊ ♋ ♌ ♍ ♎ ♏ ♐ ♑ ♒ ♓ ✅ ☑️ ✔️ ❌ ❎ ➕ ➖ ➗ ✖️ ♾️ ‼️ ⁉️ ❓ ❔ ❕ ❗ 〰️ 💱 💲 ⚠️ 🚸 ⛔ 🚫 🔞 💯 🔅 🔆 ⭐ 🌟 ✨ ⚡ 🔥 💥 💫 💤 🎵 🎶 🔔 🔕 📣 📢 💬 💭 🗯️ ♠️ ♣️ ♥️ ♦️ 🃏 🀄 🎴"),
    ("\u{1F436}", "🐶 🐱 🐭 🐹 🐰 🦊 🐻 🐼 🐻‍❄️ 🐨 🐯 🦁 🐮 🐷 🐸 🐵 🙈 🙉 🙊 🐒 🐔 🐧 🐦 🐤 🦆 🦅 🦉 🦇 🐺 🐗 🐴 🦄 🐝 🪱 🐛 🦋 🐌 🐞 🐜 🪲 🦟 🦗 🕷️ 🦂 🐢 🐍 🦎 🦖 🦕 🐙 🦑 🦐 🦞 🦀 🐡 🐠 🐟 🐬 🐳 🐋 🦈 🐊 🐅 🐆 🦓 🦍 🦧 🐘 🦛 🦏 🐪 🐫 🦒 🦘 🐃 🐂 🐄 🐎 🐖 🐏 🐑 🦙 🐐 🦌 🐕 🐩 🐈 🐓 🦃 🦚 🦜 🦢 🦩 🕊️ 🐇 🦝 🦨 🦡 🦦 🦥 🐁 🐀 🐿️ 🦔 🌵 🎄 🌲 🌳 🌴 🌱 🌿 ☘️ 🍀 🍁 🍂 🍃 🌷 🌹 🥀 🌺 🌸 🌼 🌻 🌞 🌝 🌛 🌙 🌎 🪐 ⭐ ☀️ 🌤️ ⛅ 🌧️ ⛈️ 🌩️ ❄️ ☃️ 🌈 🌊"),
    ("\u{1F354}", "🍏 🍎 🍐 🍊 🍋 🍌 🍉 🍇 🍓 🫐 🍈 🍒 🍑 🥭 🍍 🥥 🥝 🍅 🍆 🥑 🥦 🥬 🥒 🌶️ 🫑 🌽 🥕 🧄 🧅 🥔 🍠 🥐 🥯 🍞 🥖 🥨 🧀 🥚 🍳 🧈 🥞 🧇 🥓 🥩 🍗 🍖 🌭 🍔 🍟 🍕 🥪 🥙 🧆 🌮 🌯 🥗 🥘 🍝 🍜 🍲 🍛 🍣 🍱 🥟 🍤 🍙 🍚 🍘 🍥 🥠 🍢 🍡 🍧 🍨 🍦 🥧 🧁 🍰 🎂 🍮 🍭 🍬 🍫 🍿 🍩 🍪 🌰 🥜 🍯 🥛 🍼 ☕ 🍵 🧃 🥤 🍶 🍺 🍻 🥂 🍷 🥃 🍸 🍹 🧉 🍾 🧊"),
    ("\u{26BD}", "⚽ 🏀 🏈 ⚾ 🥎 🎾 🏐 🏉 🥏 🎱 🪀 🏓 🏸 🏒 🏑 🥍 🏏 🥅 ⛳ 🪁 🏹 🎣 🤿 🥊 🥋 🎽 🛹 🛼 🛷 ⛸️ 🥌 🎿 ⛷️ 🏂 🏋️ 🤸 ⛹️ 🤺 🤾 🏌️ 🏇 🧘 🏄 🏊 🤽 🚣 🧗 🚵 🚴 🏆 🥇 🥈 🥉 🏅 🎖️ 🎗️ 🎫 🎟️ 🎪 🎭 🎨 🎬 🎤 🎧 🎼 🎹 🥁 🎷 🎺 🎸 🪕 🎻 🎲 ♟️ 🎯 🎳 🎮 🕹️ 🎰 🧩 🚗 🚕 🚙 🚌 🏎️ 🚓 🚑 🚒 🚚 🚜 🏍️ 🛵 🚲 🛴 🚨 🚔 🚍 🚘 ✈️ 🛫 🛬 🚁 🛸 🚀 🛰️ ⛵ 🚤 🛥️ 🚢 ⚓ 🚧 ⛽ 🚦 🗺️ 🗿 🗽 🗼 🏰 🏯 🏟️ 🎡 🎢 🎠 ⛲ 🏖️ 🏝️ 🏜️ 🌋 ⛰️ 🏔️ 🗻 🏕️ 🏠 🏡 🏢 🏥 🏦 🏨 🏫 🏭"),
    ("\u{1F4A1}", "⌚ 📱 💻 ⌨️ 🖥️ 🖨️ 🖱️ 🕹️ 💽 💾 💿 📀 📷 📸 📹 🎥 📞 ☎️ 📺 📻 🎙️ ⏱️ ⏰ ⌛ ⏳ 📡 🔋 🔌 💡 🔦 🕯️ 🧯 💸 💵 💳 💎 ⚖️ 🧰 🔧 🔨 ⚒️ 🛠️ ⛏️ 🔩 ⚙️ 🧱 ⛓️ 🧲 🔫 💣 🧨 🪓 🔪 🗡️ ⚔️ 🛡️ 🚬 ⚰️ 🔮 📿 💈 ⚗️ 🔭 🔬 🩹 🩺 💊 💉 🧬 🦠 🧪 🌡️ 🧹 🧺 🧻 🚽 🚿 🛁 🧼 🔑 🗝️ 🚪 🛋️ 🛏️ 🧸 🖼️ 🛍️ 🛒 🎁 🎈 🎏 🎀 🎊 🎉 ✉️ 📩 📨 📧 📦 📮 📝 📁 📂 📅 📆 📇 📈 📉 📊 📋 📌 📍 📎 🖇️ 📏 📐 ✂️ 🗃️ 🗄️ 🗑️ 🔒 🔓 🔏 🔐 🖊️ 🖋️ ✒️ 🖌️ 🖍️ ✏️ 🔍 🔎"),
    ("(^_^)", "(^_^) (^o^) (>_<) (T_T) (;_;) (*^_^*) (^_-) (o_O) (O_O) (-_-) (=_=) (¬_¬) (ಠ_ಠ) ¯\\_(ツ)_/¯ (╯°□°)╯︵┻━┻ ┬─┬ノ(º_ºノ) ( ͡° ͜ʖ ͡°) (づ｡◕‿‿◕｡)づ ʕ•ᴥ•ʔ (•‿•) (◕‿◕) (✿◠‿◠) ♪(^∇^*) (ﾉ◕ヮ◕)ﾉ*:･ﾟ✧ ٩(◕‿◕)۶ (っ˘ω˘ς) (ᵔᴥᵔ) ◉_◉ (⌐■_■) ← → ↑ ↓ ↔ ↕ ⇐ ⇒ ° ± × ÷ ≈ ≠ ≤ ≥ ∞ √ π Σ Ω µ € £ ¥ ¢ © ® ™ § ¶ † ‡ • … ‰ ½ ¼ ¾ ² ³ ¹ ★ ☆ ✓ ✗ ♀ ♂ ⚥ ☐ ☑ ☒"),
];

fn recent_path() -> std::path::PathBuf {
    crate::config::dir().join("emoji-recent")
}

fn recent() -> Vec<String> {
    std::fs::read_to_string(recent_path())
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect()
}

fn remember(item: &str) {
    let mut list = recent();
    list.retain(|r| r != item);
    list.insert(0, item.to_string());
    list.truncate(24);
    let _ = std::fs::write(recent_path(), list.join("\n"));
}

fn items(category: usize) -> Vec<String> {
    if category == 0 {
        return recent();
    }
    CATEGORIES[category].1.split(' ').filter(|s| !s.is_empty()).map(str::to_string).collect()
}

pub enum Action {
    /// Type this into the app that was in front.
    Pick(String),
    Close,
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Tab(usize),
    Item(usize),
}

pub struct EmojiPanel {
    pub hwnd: HWND,
    layer: Layer,
    backdrop: Backdrop,
    pub open: bool,
    visible: bool,
    reveal: Spring,
    category: usize,
    list: Vec<String>,
    sel: usize,
    /// The first row shown.
    top: usize,
    hovered: Option<Hit>,
    anchor: (i32, i32),
    bounds: RECT,
    k: f32,
    acrylic: bool,
    opacity: f32,
    radius: f32,
    pub dirty: bool,
}

impl EmojiPanel {
    pub fn new(hwnd: HWND, backdrop: Backdrop) -> Self {
        Self {
            hwnd,
            layer: Layer::default(),
            backdrop,
            open: false,
            visible: false,
            reveal: Spring::new(0.0, 600.0, 44.0),
            category: 0,
            list: Vec::new(),
            sel: 0,
            top: 0,
            hovered: None,
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

    fn set_category(&mut self, c: usize) {
        self.category = c % CATEGORIES.len();
        self.list = items(self.category);
        self.sel = 0;
        self.top = 0;
        self.dirty = true;
    }

    pub fn show(&mut self, x: i32, y: i32, bounds: RECT, k: f32) {
        self.anchor = (x, y);
        self.bounds = bounds;
        self.k = k;
        // The ones used lately, if there are any.
        self.set_category(if recent().is_empty() { 1 } else { 0 });
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
        if self.open {
            self.open = false;
            self.reveal.set(0.0);
            self.backdrop.hide();
            self.dirty = true;
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        if ly < TABS {
            let tw = (W - 2.0 * PAD) / CATEGORIES.len() as f32;
            let i = ((lx - PAD) / tw).floor();
            return (i >= 0.0 && (i as usize) < CATEGORIES.len()).then_some(Hit::Tab(i as usize));
        }
        let (c, r) = (((lx - PAD) / CELL).floor(), ((ly - TABS) / CELL).floor());
        if c < 0.0 || c as usize >= COLS || r < 0.0 || r as usize >= ROWS {
            return None;
        }
        let i = (self.top + r as usize) * COLS + c as usize;
        (i < self.list.len()).then_some(Hit::Item(i))
    }

    pub fn on_move(&mut self, x: i32, y: i32) {
        let h = self.hit(x, y);
        if let Some(Hit::Item(i)) = h {
            self.sel = i;
        }
        if h != self.hovered {
            self.hovered = h;
            self.dirty = true;
        }
    }

    pub fn on_click(&mut self, x: i32, y: i32) -> Option<Action> {
        match self.hit(x, y)? {
            Hit::Tab(c) => {
                self.set_category(c);
                None
            }
            Hit::Item(i) => self.pick(i),
        }
    }

    fn pick(&mut self, i: usize) -> Option<Action> {
        let item = self.list.get(i)?.clone();
        remember(&item);
        Some(Action::Pick(item))
    }

    pub fn on_wheel(&mut self, delta: i16) {
        let rows = self.list.len().div_ceil(COLS);
        let max = rows.saturating_sub(ROWS);
        self.top = if delta < 0 { (self.top + 1).min(max) } else { self.top.saturating_sub(1) };
        self.dirty = true;
    }

    fn select(&mut self, i: usize) {
        if self.list.is_empty() {
            return;
        }
        self.sel = i.min(self.list.len() - 1);
        let row = self.sel / COLS;
        if row < self.top {
            self.top = row;
        } else if row >= self.top + ROWS {
            self.top = row + 1 - ROWS;
        }
        self.dirty = true;
    }

    pub fn on_key(&mut self, vk: u16) -> Option<Action> {
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        match VIRTUAL_KEY(vk) {
            VK_ESCAPE => return Some(Action::Close),
            VK_RETURN | VK_SPACE => return self.pick(self.sel),
            VK_TAB if shift => self.set_category(self.category + CATEGORIES.len() - 1),
            VK_TAB => self.set_category(self.category + 1),
            VK_LEFT => self.select(self.sel.saturating_sub(1)),
            VK_RIGHT => self.select(self.sel + 1),
            VK_UP => self.select(self.sel.saturating_sub(COLS)),
            VK_DOWN => self.select(self.sel + COLS),
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
        let (pw, ph) = ((W * k).ceil() as i32, (H * k).ceil() as i32);
        let b = self.bounds;
        let edge = (8.0 * k) as i32;
        let x = self.anchor.0.min(b.right - pw - edge).max(b.left + edge);
        let below = self.anchor.1 + edge;
        let y = if below + ph <= b.bottom - edge { below } else { (self.anchor.1 - 2 * edge - ph).max(b.top + edge) };
        let slide = ((1.0 - self.reveal.value.clamp(0.0, 1.0)) * 10.0 * k) as i32;
        let pos = POINT { x, y: y - slide };
        let glass = self.acrylic && self.reveal.value > theme::GLASS_FROM;
        let screen = RECT { left: pos.x, top: pos.y, right: pos.x + pw, bottom: pos.y + ph };
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            theme::panel(&p, 0.0, 0.0, W, H, self.radius, glass, self.opacity, 0.97, 1.0);
            let tw = (W - 2.0 * PAD) / CATEGORIES.len() as f32;
            for (i, (label, _)) in CATEGORIES.iter().enumerate() {
                let tx = PAD + i as f32 * tw;
                if i == self.category {
                    p.rrect(tx + 2.0, 6.0, tw - 4.0, TABS - 10.0, 6.0, Color::white(0.1));
                    p.rrect(tx + tw / 2.0 - 7.0, TABS - 5.0, 14.0, 2.5, 1.2, theme::accent());
                } else if self.hovered == Some(Hit::Tab(i)) {
                    p.rrect(tx + 2.0, 6.0, tw - 4.0, TABS - 10.0, 6.0, Color::white(0.06));
                }
                if i == 0 {
                    // A clock, for the recently used.
                    let (cx, cy, c) = (tx + tw / 2.0, TABS / 2.0, Color::white(0.8));
                    p.stroke_circle(cx, cy, 6.5, c, 1.3);
                    p.line(cx, cy, cx, cy - 4.0, c, 1.3);
                    p.line(cx, cy, cx + 3.0, cy, c, 1.3);
                } else if i == CATEGORIES.len() - 1 {
                    p.text("\u{3a9}", &f.label, tx, 2.0, tw, TABS - 4.0, Align::Center, Color::white(0.8));
                } else {
                    p.text_colour(label, &f.body, tx, 2.0, tw, TABS - 4.0, Color::white(0.9));
                }
            }
            if self.list.is_empty() {
                p.text("Nothing used yet", &f.body, PAD, TABS, W - 2.0 * PAD, CELL, Align::Center, Color::white(0.6));
            }
            let text_like = self.category == CATEGORIES.len() - 1;
            for (n, item) in self.list.iter().enumerate().skip(self.top * COLS).take(ROWS * COLS) {
                let row = n / COLS - self.top;
                let (cx, cy) = (PAD + (n % COLS) as f32 * CELL, TABS + row as f32 * CELL);
                if n == self.sel {
                    p.rrect(cx + 2.0, cy + 2.0, CELL - 4.0, CELL - 4.0, 6.0, Color::white(0.12));
                }
                let font = if text_like || item.chars().count() > 3 { &f.small } else { &f.emoji };
                p.text_colour(item, font, cx, cy, CELL, CELL, Color::white(0.95));
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

/// Types `text` into the window in front, as if typed on the keyboard.
pub fn type_text(text: &str) {
    let mut inputs = Vec::new();
    for unit in text.encode_utf16() {
        for up in [false, true] {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: unit,
                        dwFlags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }
    }
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}
