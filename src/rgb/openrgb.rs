//! A client for OpenRGB's SDK server: the hardware side of Cascade's
//! lighting. OpenRGB talks to each device directly (USB, and the SMBus for
//! RAM, motherboards and graphics cards), for hundreds of devices across
//! Corsair, ASUS Aura, Gigabyte RGB Fusion, Razer, Logitech, HyperX, MSI,
//! NZXT, SteelSeries and more, with none of their own apps running. Cascade
//! asks it, over a local TCP connection (port 6742), what's there and sets
//! the colours.
//!
//! The protocol: a 16-byte header ("ORGB", device index, packet id, payload
//! size, little-endian) then the payload. Cascade asks for protocol 4 (zones
//! with segments, modes with brightness), which every OpenRGB from 0.9 on
//! speaks, newer servers included (they answer in the version asked for).
//! Plain std networking, no Windows calls: tested against a pretend server.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub const PORT: u16 = 6742;
/// The newest protocol version Cascade reads.
pub const PROTOCOL: u32 = 4;

pub const REQUEST_CONTROLLER_COUNT: u32 = 0;
pub const REQUEST_CONTROLLER_DATA: u32 = 1;
pub const REQUEST_PROTOCOL_VERSION: u32 = 40;
pub const SET_CLIENT_NAME: u32 = 50;
pub const DEVICE_LIST_UPDATED: u32 = 100;
pub const REQUEST_RESCAN_DEVICES: u32 = 140;
pub const RESIZEZONE: u32 = 1000;
pub const UPDATELEDS: u32 = 1050;
pub const UPDATEZONELEDS: u32 = 1051;
pub const SETCUSTOMMODE: u32 = 1100;
pub const UPDATEMODE: u32 = 1101;
pub const SAVEMODE: u32 = 1102;

/// Mode flags.
pub const MODE_HAS_SPEED: u32 = 1 << 0;
pub const MODE_HAS_DIRECTION_LR: u32 = 1 << 1;
pub const MODE_HAS_DIRECTION_UD: u32 = 1 << 2;
pub const MODE_HAS_DIRECTION_HV: u32 = 1 << 3;
pub const MODE_HAS_BRIGHTNESS: u32 = 1 << 4;
pub const MODE_HAS_PER_LED_COLOR: u32 = 1 << 5;
pub const MODE_HAS_MODE_SPECIFIC_COLOR: u32 = 1 << 6;
pub const MODE_HAS_RANDOM_COLOR: u32 = 1 << 7;
pub const MODE_MANUAL_SAVE: u32 = 1 << 8;
pub const MODE_AUTOMATIC_SAVE: u32 = 1 << 9;

/// How a mode takes colours.
pub const COLORS_NONE: u32 = 0;
pub const COLORS_PER_LED: u32 = 1;
pub const COLORS_MODE_SPECIFIC: u32 = 2;
pub const COLORS_RANDOM: u32 = 3;

pub const ZONE_SINGLE: i32 = 0;
pub const ZONE_LINEAR: i32 = 1;
pub const ZONE_MATRIX: i32 = 2;

/// A device's kind, as OpenRGB numbers them (for its name in the list).
pub fn kind_name(t: i32) -> &'static str {
    match t {
        0 => "Motherboard",
        1 => "Memory",
        2 => "Graphics card",
        3 => "Cooler",
        4 => "LED strip",
        5 => "Keyboard",
        6 => "Mouse",
        7 => "Mouse mat",
        8 => "Headset",
        9 => "Headset stand",
        10 => "Gamepad",
        11 => "Light",
        12 => "Speaker",
        13 => "Virtual",
        14 => "Storage",
        15 => "Case",
        16 => "Microphone",
        17 => "Accessory",
        18 => "Keypad",
        _ => "Device",
    }
}

/// A colour as OpenRGB packs it: red in the low byte, then green, then blue.
pub fn pack(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

pub fn unpack(c: u32) -> (u8, u8, u8) {
    (c as u8, (c >> 8) as u8, (c >> 16) as u8)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mode {
    pub name: String,
    pub value: i32,
    pub flags: u32,
    pub speed_min: u32,
    pub speed_max: u32,
    pub brightness_min: u32,
    pub brightness_max: u32,
    pub colors_min: u32,
    pub colors_max: u32,
    pub speed: u32,
    pub brightness: u32,
    pub direction: u32,
    pub color_mode: u32,
    pub colors: Vec<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Zone {
    pub name: String,
    pub kind: i32,
    pub leds_min: u32,
    pub leds_max: u32,
    pub leds_count: u32,
    /// A keyboard's grid: (height, width, each cell's LED number within the
    /// zone, u32::MAX where there's none).
    pub matrix: Option<(u32, u32, Vec<u32>)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Controller {
    /// Its place in the server's list (what packets address).
    pub index: u32,
    pub kind: i32,
    pub name: String,
    pub vendor: String,
    pub description: String,
    pub version: String,
    pub serial: String,
    pub location: String,
    pub active_mode: i32,
    pub modes: Vec<Mode>,
    pub zones: Vec<Zone>,
    pub leds: Vec<String>,
    pub colors: Vec<u32>,
}

impl Controller {
    /// What a device is known by from one start to the next: its name and
    /// serial, or where it's plugged in when it has no serial.
    pub fn key(&self) -> String {
        if self.serial.trim().is_empty() {
            format!("{}|{}", self.name, self.location)
        } else {
            format!("{}|{}", self.name, self.serial)
        }
    }

    /// The mode for setting each LED from a program ("Direct" and the like).
    pub fn direct_mode(&self) -> Option<usize> {
        let by_name = |n: &str| self.modes.iter().position(|m| m.name.eq_ignore_ascii_case(n) && m.color_mode == COLORS_PER_LED);
        by_name("Direct").or_else(|| by_name("Custom")).or_else(|| by_name("Static"))
    }
}

/// Reads the protocol's little-endian fields from a payload.
pub struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Reader { b, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.at..self.at + n)?;
        self.at += n;
        Some(s)
    }

    pub fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    pub fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    /// A string: its length (with the closing NUL) as a u16, then the bytes.
    pub fn string(&mut self) -> Option<String> {
        let n = self.u16()? as usize;
        let s = self.take(n)?;
        Some(String::from_utf8_lossy(s).trim_end_matches('\0').to_string())
    }

    fn colors(&mut self) -> Option<Vec<u32>> {
        let n = self.u16()? as usize;
        (0..n).map(|_| self.u32()).collect()
    }
}

fn read_mode(r: &mut Reader, version: u32) -> Option<Mode> {
    let mut m = Mode { name: r.string()?, value: r.i32()?, flags: r.u32()?, speed_min: r.u32()?, speed_max: r.u32()?, ..Default::default() };
    if version >= 3 {
        m.brightness_min = r.u32()?;
        m.brightness_max = r.u32()?;
    }
    m.colors_min = r.u32()?;
    m.colors_max = r.u32()?;
    m.speed = r.u32()?;
    if version >= 3 {
        m.brightness = r.u32()?;
    }
    m.direction = r.u32()?;
    m.color_mode = r.u32()?;
    m.colors = r.colors()?;
    Some(m)
}

fn read_zone(r: &mut Reader, version: u32) -> Option<Zone> {
    let mut z = Zone { name: r.string()?, kind: r.i32()?, leds_min: r.u32()?, leds_max: r.u32()?, leds_count: r.u32()?, matrix: None };
    let matrix_len = r.u16()? as usize;
    if matrix_len > 0 {
        let (h, w) = (r.u32()?, r.u32()?);
        let cells = (matrix_len.saturating_sub(8)) / 4;
        let data: Vec<u32> = (0..cells).map(|_| r.u32()).collect::<Option<_>>()?;
        z.matrix = Some((h, w, data));
    }
    if version >= 4 {
        // Segments (named parts of a strip): read past them.
        for _ in 0..r.u16()? {
            r.string()?;
            r.i32()?;
            r.u32()?;
            r.u32()?;
        }
    }
    Some(z)
}

/// Reads a controller's description (the payload of its REQUEST_CONTROLLER_DATA answer).
pub fn parse_controller(payload: &[u8], index: u32, version: u32) -> Option<Controller> {
    let mut r = Reader::new(payload);
    let _size = r.u32()?;
    let mut c = Controller { index, kind: r.i32()?, name: r.string()?, ..Default::default() };
    if version >= 1 {
        c.vendor = r.string()?;
    }
    c.description = r.string()?;
    c.version = r.string()?;
    c.serial = r.string()?;
    c.location = r.string()?;
    let modes = r.u16()?;
    c.active_mode = r.i32()?;
    c.modes = (0..modes).map(|_| read_mode(&mut r, version)).collect::<Option<_>>()?;
    let zones = r.u16()?;
    c.zones = (0..zones).map(|_| read_zone(&mut r, version)).collect::<Option<_>>()?;
    let leds = r.u16()?;
    c.leds = (0..leds)
        .map(|_| {
            let name = r.string()?;
            r.u32()?;
            Some(name)
        })
        .collect::<Option<_>>()?;
    c.colors = r.colors()?;
    Some(c)
}

fn put_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&((s.len() + 1) as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// A mode as UPDATEMODE / SAVEMODE send it (the same layout as it's read).
pub fn mode_bytes(m: &Mode, version: u32) -> Vec<u8> {
    let mut b = Vec::new();
    put_string(&mut b, &m.name);
    b.extend_from_slice(&m.value.to_le_bytes());
    for v in [m.flags, m.speed_min, m.speed_max] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    if version >= 3 {
        b.extend_from_slice(&m.brightness_min.to_le_bytes());
        b.extend_from_slice(&m.brightness_max.to_le_bytes());
    }
    for v in [m.colors_min, m.colors_max, m.speed] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    if version >= 3 {
        b.extend_from_slice(&m.brightness.to_le_bytes());
    }
    b.extend_from_slice(&m.direction.to_le_bytes());
    b.extend_from_slice(&m.color_mode.to_le_bytes());
    b.extend_from_slice(&(m.colors.len() as u16).to_le_bytes());
    for c in &m.colors {
        b.extend_from_slice(&c.to_le_bytes());
    }
    b
}

/// A connection to the server.
pub struct Client {
    stream: TcpStream,
    pub version: u32,
    list_changed: bool,
}

impl Client {
    /// Connects to the server on this PC, agreeing a protocol version.
    pub fn connect(port: u16, name: &str) -> std::io::Result<Client> {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut c = Client { stream, version: 0, list_changed: false };
        c.send(0, REQUEST_PROTOCOL_VERSION, &PROTOCOL.to_le_bytes())?;
        // A server that doesn't know versions doesn't answer: it speaks 0.
        c.stream.set_read_timeout(Some(Duration::from_millis(1000)))?;
        c.version = match c.recv_for(REQUEST_PROTOCOL_VERSION) {
            Ok(p) if p.len() >= 4 => u32::from_le_bytes(p[..4].try_into().unwrap_or_default()).min(PROTOCOL),
            _ => 0,
        };
        c.stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut n = name.as_bytes().to_vec();
        n.push(0);
        c.send(0, SET_CLIENT_NAME, &n)?;
        Ok(c)
    }

    pub fn send(&mut self, device: u32, id: u32, payload: &[u8]) -> std::io::Result<()> {
        let mut b = Vec::with_capacity(16 + payload.len());
        b.extend_from_slice(b"ORGB");
        b.extend_from_slice(&device.to_le_bytes());
        b.extend_from_slice(&id.to_le_bytes());
        b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        b.extend_from_slice(payload);
        self.stream.write_all(&b)
    }

    /// The next packet: (device, id, payload).
    pub fn recv(&mut self) -> std::io::Result<(u32, u32, Vec<u8>)> {
        let mut h = [0u8; 16];
        self.stream.read_exact(&mut h)?;
        if &h[0..4] != b"ORGB" {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "not an OpenRGB packet"));
        }
        let word = |i: usize| u32::from_le_bytes(h[i..i + 4].try_into().unwrap_or_default());
        let (dev, id, size) = (word(4), word(8), word(12) as usize);
        if size > 8 << 20 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "packet too big"));
        }
        let mut p = vec![0u8; size];
        self.stream.read_exact(&mut p)?;
        Ok((dev, id, p))
    }

    /// Waits for the answer to `id`, skipping anything else (a device list
    /// change is noted in `list_changed`).
    fn recv_for(&mut self, id: u32) -> std::io::Result<Vec<u8>> {
        loop {
            let (_, got, p) = self.recv()?;
            if got == id {
                return Ok(p);
            }
            if got == DEVICE_LIST_UPDATED {
                self.list_changed = true;
            }
        }
    }

    /// Every device the server has, described in full.
    pub fn controllers(&mut self) -> std::io::Result<Vec<Controller>> {
        self.list_changed = false;
        self.send(0, REQUEST_CONTROLLER_COUNT, &[])?;
        let p = self.recv_for(REQUEST_CONTROLLER_COUNT)?;
        let n = p.get(..4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap_or_default()));
        let mut out = Vec::new();
        for i in 0..n.min(512) {
            let ask: Vec<u8> = if self.version >= 1 { self.version.to_le_bytes().to_vec() } else { Vec::new() };
            self.send(i, REQUEST_CONTROLLER_DATA, &ask)?;
            let p = self.recv_for(REQUEST_CONTROLLER_DATA)?;
            if let Some(c) = parse_controller(&p, i, self.version) {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// Puts a device in the mode for setting each LED ("Direct").
    pub fn custom_mode(&mut self, device: u32) -> std::io::Result<()> {
        self.send(device, SETCUSTOMMODE, &[])
    }

    /// Sets every LED of a device (`colors` packed as `pack` does, one per LED).
    pub fn set_leds(&mut self, device: u32, colors: &[u32]) -> std::io::Result<()> {
        let mut b = Vec::with_capacity(6 + colors.len() * 4);
        b.extend_from_slice(&((6 + colors.len() * 4) as u32).to_le_bytes());
        b.extend_from_slice(&(colors.len() as u16).to_le_bytes());
        for c in colors {
            b.extend_from_slice(&c.to_le_bytes());
        }
        self.send(device, UPDATELEDS, &b)
    }

    /// Switches a device to one of its own (hardware) modes, set as `m` says.
    pub fn set_mode(&mut self, device: u32, index: usize, m: &Mode, save: bool) -> std::io::Result<()> {
        let body = mode_bytes(m, self.version);
        let mut b = Vec::with_capacity(8 + body.len());
        b.extend_from_slice(&((8 + body.len()) as u32).to_le_bytes());
        b.extend_from_slice(&(index as i32).to_le_bytes());
        b.extend_from_slice(&body);
        self.send(device, if save && self.version >= 3 { SAVEMODE } else { UPDATEMODE }, &b)
    }

    /// Sets how many LEDs a resizable zone (an ARGB header, a fan hub port) has.
    pub fn resize_zone(&mut self, device: u32, zone: usize, size: u32) -> std::io::Result<()> {
        let mut b = Vec::new();
        b.extend_from_slice(&(zone as i32).to_le_bytes());
        b.extend_from_slice(&(size as i32).to_le_bytes());
        self.send(device, RESIZEZONE, &b)
    }

    /// Asks the server to look for devices again.
    pub fn rescan(&mut self) -> std::io::Result<()> {
        self.send(0, REQUEST_RESCAN_DEVICES, &[])
    }

    /// Whether the server said its device list changed since this was last
    /// asked: reads whatever it sent meanwhile, without waiting.
    pub fn take_list_changed(&mut self) -> bool {
        let mut h = [0u8; 16];
        loop {
            let _ = self.stream.set_nonblocking(true);
            let waiting = matches!(self.stream.peek(&mut h), Ok(16));
            let _ = self.stream.set_nonblocking(false);
            if !waiting {
                break;
            }
            match self.recv() {
                Ok((_, DEVICE_LIST_UPDATED, _)) => self.list_changed = true,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        std::mem::take(&mut self.list_changed)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A controller's description as a protocol-4 server sends it.
    pub fn sample(name: &str, leds: usize, matrix: bool) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&5i32.to_le_bytes());
        for s in [name, "Corsair", "A keyboard", "1.0", "SN1", "HID: /dev/x"] {
            put_string(&mut b, s);
        }
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&0i32.to_le_bytes());
        for (n, cm) in [("Direct", COLORS_PER_LED), ("Rainbow", COLORS_NONE)] {
            let m = Mode { name: n.into(), color_mode: cm, flags: MODE_HAS_SPEED, speed_max: 4, colors: vec![pack(1, 2, 3)], ..Default::default() };
            b.extend_from_slice(&mode_bytes(&m, 4));
        }
        b.extend_from_slice(&1u16.to_le_bytes());
        put_string(&mut b, "Keys");
        b.extend_from_slice(&(if matrix { ZONE_MATRIX } else { ZONE_LINEAR }).to_le_bytes());
        for v in [leds as u32, leds as u32, leds as u32] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        if matrix {
            let (h, w) = (2u32, (leds as u32).div_ceil(2));
            b.extend_from_slice(&((8 + h * w * 4) as u16).to_le_bytes());
            b.extend_from_slice(&h.to_le_bytes());
            b.extend_from_slice(&w.to_le_bytes());
            for i in 0..h * w {
                b.extend_from_slice(&(if (i as usize) < leds { i } else { u32::MAX }).to_le_bytes());
            }
        } else {
            b.extend_from_slice(&0u16.to_le_bytes());
        }
        b.extend_from_slice(&1u16.to_le_bytes());
        put_string(&mut b, "Seg");
        b.extend_from_slice(&0i32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&(leds as u32).to_le_bytes());
        b.extend_from_slice(&(leds as u16).to_le_bytes());
        for i in 0..leds {
            put_string(&mut b, &format!("Key: {i}"));
            b.extend_from_slice(&0u32.to_le_bytes());
        }
        b.extend_from_slice(&(leds as u16).to_le_bytes());
        for _ in 0..leds {
            b.extend_from_slice(&0u32.to_le_bytes());
        }
        let size = b.len() as u32;
        b[0..4].copy_from_slice(&size.to_le_bytes());
        b
    }

    #[test]
    fn reads_a_controller() {
        let c = parse_controller(&sample("K70", 5, true), 3, 4).unwrap();
        assert_eq!(c.name, "K70");
        assert_eq!(c.vendor, "Corsair");
        assert_eq!(c.modes.len(), 2);
        assert_eq!(c.modes[1].name, "Rainbow");
        assert_eq!(c.direct_mode(), Some(0));
        assert_eq!(c.zones[0].matrix.as_ref().unwrap().0, 2);
        assert_eq!(c.leds.len(), 5);
        assert_eq!(c.leds[4], "Key: 4");
        assert_eq!(c.key(), "K70|SN1");
    }

    #[test]
    fn modes_round_trip() {
        let m = Mode { name: "Breathing".into(), value: 7, flags: 3, speed: 2, brightness: 50, colors: vec![1, 2], ..Default::default() };
        let b = mode_bytes(&m, 4);
        assert_eq!(read_mode(&mut Reader::new(&b), 4).unwrap(), m);
    }

    #[test]
    fn colours_pack_red_low() {
        assert_eq!(pack(0x11, 0x22, 0x33), 0x0033_2211);
        assert_eq!(unpack(0x0033_2211), (0x11, 0x22, 0x33));
    }

    #[test]
    fn talks_to_a_pretend_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let read = |s: &mut TcpStream| {
                let mut h = [0u8; 16];
                s.read_exact(&mut h).unwrap();
                let size = u32::from_le_bytes(h[12..16].try_into().unwrap()) as usize;
                let mut p = vec![0u8; size];
                s.read_exact(&mut p).unwrap();
                (u32::from_le_bytes(h[4..8].try_into().unwrap()), u32::from_le_bytes(h[8..12].try_into().unwrap()), p)
            };
            let reply = |s: &mut TcpStream, dev: u32, id: u32, p: &[u8]| {
                let mut b = b"ORGB".to_vec();
                b.extend_from_slice(&dev.to_le_bytes());
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&(p.len() as u32).to_le_bytes());
                b.extend_from_slice(p);
                s.write_all(&b).unwrap();
            };
            let (_, id, p) = read(&mut s);
            assert_eq!(id, REQUEST_PROTOCOL_VERSION);
            assert_eq!(p, 4u32.to_le_bytes());
            reply(&mut s, 0, REQUEST_PROTOCOL_VERSION, &6u32.to_le_bytes());
            assert_eq!(read(&mut s).1, SET_CLIENT_NAME);
            assert_eq!(read(&mut s).1, REQUEST_CONTROLLER_COUNT);
            // An unasked-for notice first, as a server may send.
            reply(&mut s, 0, DEVICE_LIST_UPDATED, &[]);
            reply(&mut s, 0, REQUEST_CONTROLLER_COUNT, &2u32.to_le_bytes());
            for i in 0..2 {
                let (dev, id, p) = read(&mut s);
                assert_eq!((dev, id), (i, REQUEST_CONTROLLER_DATA));
                assert_eq!(p, 4u32.to_le_bytes());
                reply(&mut s, i, REQUEST_CONTROLLER_DATA, &sample(if i == 0 { "Keyboard" } else { "Fan strip" }, 4 + i as usize * 8, i == 0));
            }
            assert_eq!(read(&mut s).1, SETCUSTOMMODE);
            let (dev, id, p) = read(&mut s);
            assert_eq!((dev, id), (1, UPDATELEDS));
            assert_eq!(p.len(), 6 + 12 * 4);
            assert_eq!(u16::from_le_bytes(p[4..6].try_into().unwrap()), 12);
        });
        let mut c = Client::connect(port, "Cascade").unwrap();
        assert_eq!(c.version, 4);
        let list = c.controllers().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].name, "Fan strip");
        assert_eq!(list[1].leds.len(), 12);
        c.custom_mode(1).unwrap();
        c.set_leds(1, &[pack(255, 0, 0); 12]).unwrap();
        server.join().unwrap();
    }
}
