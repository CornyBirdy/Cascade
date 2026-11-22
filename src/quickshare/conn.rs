//! A Quick Share connection: frames over TCP (each with its length in front),
//! the handshake from either side, then everything sealed (see `crypto`),
//! and the transfer itself: what's offered, the answer, the files' bytes.
//!
//! Written against any stream (`Stream`), so a sender and a receiver can be
//! tested against each other without a network.

use std::collections::HashMap;
use std::io::{self, Read, Write};

use super::crypto::{self, KeyPair, Keys};
use super::proto::{self, Msg};

/// What a connection runs over.
pub trait Stream: Read + Write {
    /// Whether something has arrived to read (without waiting for it).
    fn readable(&mut self) -> bool;
}

impl Stream for std::net::TcpStream {
    fn readable(&mut self) -> bool {
        let _ = self.set_nonblocking(true);
        let mut b = [0u8; 1];
        // Something to read, or the other side gone (which reading will tell).
        let ready = self.peek(&mut b).is_ok();
        let _ = self.set_nonblocking(false);
        ready
    }
}

/// The largest frame taken (a file's chunk is half a megabyte).
const MAX_FRAME: usize = 5 * 1024 * 1024;
/// How much of a file goes in each frame.
pub const CHUNK: usize = 512 * 1024;

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// What the other side is: its name and kind (1 phone, 2 tablet, 3 computer).
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub name: String,
    pub kind: u8,
}

/// Endpoint info as Quick Share has it: the kind of device, 16 random bytes,
/// then the name with its length in front.
pub fn endpoint_info(name: &str, kind: u8) -> Vec<u8> {
    let mut out = vec![kind << 1];
    out.extend(crypto::random(16));
    let name = &name.as_bytes()[..name.len().min(255)];
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    out
}

pub fn parse_endpoint_info(b: &[u8]) -> Option<Device> {
    if b.len() < 17 {
        return None;
    }
    let kind = (b[0] >> 1) & 7;
    let has_name = b[0] & 0x10 == 0 && b.len() > 17;
    let name = if has_name {
        let n = b[17] as usize;
        String::from_utf8_lossy(b.get(18..18 + n)?).into_owned()
    } else {
        String::new()
    };
    Some(Device { name, kind })
}

/// An OfflineFrame: V1, of `kind`, with `inner` in `field`.
fn offline(kind: i64, field: u32, inner: Msg) -> Vec<u8> {
    Msg::new().int(1, 1).msg(2, Msg::new().int(1, kind).msg(field, inner)).build()
}

/// A sharing Frame: V1, of `kind`, with `inner` in `field`.
fn sharing(kind: i64, field: u32, inner: Msg) -> Vec<u8> {
    Msg::new().int(1, 1).msg(2, Msg::new().int(1, kind).msg(field, inner)).build()
}

/// Our answer to a connection: accepted, from Windows.
fn connection_response() -> Vec<u8> {
    offline(2, 3, Msg::new().int(1, 0).int(3, 1).msg(4, Msg::new().int(1, 3)))
}

fn paired_key_encryption() -> Vec<u8> {
    sharing(3, 4, Msg::new().bytes(1, &crypto::random(72)).bytes(2, &crypto::random(6)))
}

fn paired_key_result() -> Vec<u8> {
    // UNABLE: there's no account to vouch for either side, as with any stranger.
    sharing(4, 5, Msg::new().int(1, 3))
}

/// The answer to what's offered (1 accept, 2 reject).
fn response(status: i64) -> Vec<u8> {
    sharing(2, 3, Msg::new().int(1, status))
}

fn cancel() -> Vec<u8> {
    Msg::new().int(1, 1).msg(2, Msg::new().int(1, 6)).build()
}

/// The type of a sharing frame (1 introduction, 2 response, 3 and 4 the
/// paired key's, 6 cancel).
fn sharing_kind(frame: &[u8]) -> i64 {
    proto::bytes(frame, 2).and_then(|v1| proto::int(v1, 1)).unwrap_or(0)
}

/// Something that arrived.
#[derive(Debug)]
pub enum Event {
    /// A whole bytes payload (a sharing frame, or a text that was sent).
    Bytes { id: i64, data: Vec<u8> },
    /// Part of a file.
    File { id: i64, offset: i64, body: Vec<u8>, last: bool },
    /// The other side is done.
    Disconnect,
}

pub struct Conn<S: Stream> {
    s: S,
    keys: Option<Keys>,
    seq_out: i32,
    seq_in: i32,
    partial: HashMap<i64, Vec<u8>>,
    /// Bytes payloads that arrived while waiting for something else.
    held: Vec<(i64, Vec<u8>)>,
}

impl<S: Stream> Conn<S> {
    pub fn new(s: S) -> Conn<S> {
        Conn { s, keys: None, seq_out: 0, seq_in: 0, partial: HashMap::new(), held: Vec::new() }
    }

    pub fn pin(&self) -> String {
        self.keys.as_ref().map(|k| k.pin.clone()).unwrap_or_default()
    }

    fn send_raw(&mut self, data: &[u8]) -> io::Result<()> {
        let mut out = Vec::with_capacity(data.len() + 4);
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
        self.s.write_all(&out)?;
        self.s.flush()
    }

    fn recv_raw(&mut self) -> io::Result<Vec<u8>> {
        let mut len = [0u8; 4];
        self.s.read_exact(&mut len)?;
        let n = u32::from_be_bytes(len) as usize;
        if n > MAX_FRAME {
            return Err(bad("frame too big"));
        }
        let mut buf = vec![0u8; n];
        self.s.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// An OfflineFrame out: sealed once the handshake's done.
    fn send_offline(&mut self, frame: &[u8]) -> io::Result<()> {
        match &self.keys {
            Some(k) => {
                self.seq_out += 1;
                let d2d = Msg::new().bytes(1, frame).int(2, self.seq_out as i64).build();
                let sealed = k.seal(&d2d);
                self.send_raw(&sealed)
            }
            None => self.send_raw(frame),
        }
    }

    fn recv_offline(&mut self) -> io::Result<Vec<u8>> {
        let raw = self.recv_raw()?;
        let Some(k) = &self.keys else { return Ok(raw) };
        let d2d = k.open(&raw).ok_or_else(|| bad("message failed its signature"))?;
        self.seq_in += 1;
        if proto::int(&d2d, 2) != Some(self.seq_in as i64) {
            return Err(bad("message out of order"));
        }
        proto::bytes(&d2d, 1).map(|b| b.to_vec()).ok_or_else(|| bad("empty message"))
    }

    /// The next thing that arrives: keep-alives are answered on the way.
    pub fn next(&mut self) -> io::Result<Event> {
        loop {
            let frame = self.recv_offline()?;
            let v1 = proto::bytes(&frame, 2).ok_or_else(|| bad("not a V1 frame"))?;
            match proto::int(v1, 1).unwrap_or(0) {
                3 => {
                    let t = proto::bytes(v1, 4).ok_or_else(|| bad("no payload"))?;
                    let header = proto::bytes(t, 2).unwrap_or_default();
                    let chunk = proto::bytes(t, 3).unwrap_or_default();
                    let id = proto::int(header, 1).unwrap_or(0);
                    let kind = proto::int(header, 2).unwrap_or(0);
                    let offset = proto::int(chunk, 2).unwrap_or(0);
                    let last = proto::int(chunk, 1).unwrap_or(0) & 1 == 1;
                    let body = proto::bytes(chunk, 3).unwrap_or_default().to_vec();
                    if kind == 2 {
                        return Ok(Event::File { id, offset, body, last });
                    }
                    let total = proto::int(header, 3).unwrap_or(0);
                    if total as usize > MAX_FRAME {
                        return Err(bad("payload too big"));
                    }
                    let buf = self.partial.entry(id).or_default();
                    if offset as usize != buf.len() {
                        return Err(bad("payload out of order"));
                    }
                    buf.extend_from_slice(&body);
                    if last {
                        let data = self.partial.remove(&id).unwrap_or_default();
                        return Ok(Event::Bytes { id, data });
                    }
                }
                5 => {
                    let ack = proto::bytes(v1, 6).and_then(|k| proto::int(k, 1)).unwrap_or(0) != 0;
                    if !ack {
                        self.send_offline(&offline(5, 6, Msg::new().bool(1, true)))?;
                    }
                }
                6 => return Ok(Event::Disconnect),
                _ => {}
            }
        }
    }

    /// The next sharing frame (the transfer's own messages).
    fn next_sharing(&mut self) -> io::Result<Vec<u8>> {
        if !self.held.is_empty() {
            return Ok(self.held.remove(0).1);
        }
        loop {
            match self.next()? {
                Event::Bytes { data, .. } => {
                    if sharing_kind(&data) == 6 {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled on the other side"));
                    }
                    return Ok(data);
                }
                Event::Disconnect => return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "disconnected")),
                Event::File { .. } => {}
            }
        }
    }

    /// Waits for a sharing frame of `kind`.
    fn expect_sharing(&mut self, kind: i64) -> io::Result<Vec<u8>> {
        loop {
            let f = self.next_sharing()?;
            if sharing_kind(&f) == kind {
                return Ok(f);
            }
        }
    }

    /// A bytes payload: the whole of it, then an empty last chunk.
    fn send_bytes(&mut self, id: i64, data: &[u8]) -> io::Result<()> {
        let total = data.len() as i64;
        self.send_offline(&payload(id, 1, total, 0, false, Some(data)))?;
        self.send_offline(&payload(id, 1, total, total, true, None))
    }

    fn send_sharing(&mut self, frame: &[u8]) -> io::Result<()> {
        self.send_bytes(crypto::random_i64(), frame)
    }

    /// A chunk of a file (`last` once it's all sent: an empty chunk follows).
    pub fn send_file_chunk(&mut self, id: i64, total: i64, offset: i64, body: &[u8]) -> io::Result<()> {
        self.send_offline(&payload(id, 2, total, offset, false, Some(body)))?;
        if offset + body.len() as i64 >= total {
            self.send_offline(&payload(id, 2, total, total, true, None))?;
        }
        Ok(())
    }

    /// Says goodbye.
    pub fn disconnect(&mut self) {
        let _ = self.send_offline(&offline(6, 7, Msg::new()));
    }

    pub fn cancel(&mut self) {
        let _ = self.send_sharing(&cancel());
        self.disconnect();
    }

    /// Answers keep-alives and notices a cancel or disconnection while busy
    /// with something else (waiting for an answer, sending a file). Bytes
    /// payloads that arrive meanwhile are kept for `next_sharing`.
    pub fn poll(&mut self) -> io::Result<()> {
        while self.s.readable() {
            match self.next()? {
                Event::Bytes { id, data } => {
                    if sharing_kind(&data) == 6 {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled on the other side"));
                    }
                    self.held.push((id, data));
                }
                Event::Disconnect => return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "disconnected")),
                Event::File { .. } => {}
            }
        }
        Ok(())
    }

    // ------------------------------------------------------- handshakes --

    /// Someone connected to us: the handshake, from their connection request
    /// to the paired-key exchange. Returns who it is.
    pub fn accept(&mut self) -> io::Result<Device> {
        // Their connection request, with who they are.
        let req = self.recv_raw()?;
        let v1 = proto::bytes(&req, 2).ok_or_else(|| bad("no frame"))?;
        if proto::int(v1, 1) != Some(1) {
            return Err(bad("expected a connection request"));
        }
        let info = proto::bytes(v1, 2).and_then(|r| proto::bytes(r, 6)).unwrap_or_default();
        let device = parse_endpoint_info(info).unwrap_or(Device { name: "A device".into(), kind: 1 });
        // UKEY2: their init (with a commitment to their key), ours, their key.
        let client_init = self.recv_raw()?;
        if proto::int(&client_init, 1) != Some(2) {
            return Err(bad("expected UKEY2 client init"));
        }
        let ci = proto::bytes(&client_init, 2).ok_or_else(|| bad("empty client init"))?;
        let commitment = proto::all(ci, 3)
            .into_iter()
            .find(|c| proto::int(c, 1) == Some(100))
            .and_then(|c| proto::bytes(c, 2))
            .ok_or_else(|| bad("no P-256 commitment"))?
            .to_vec();
        let keys = KeyPair::new();
        let server_init = Msg::new()
            .int(1, 3)
            .msg(2, Msg::new().int(1, 1).bytes(2, &crypto::random(32)).int(3, 100).bytes(4, &keys.public_message()))
            .build();
        self.send_raw(&server_init)?;
        let client_finish = self.recv_raw()?;
        if crypto::sha512(&client_finish) != commitment {
            return Err(bad("their key doesn't match their commitment"));
        }
        let cf = proto::bytes(&client_finish, 2).ok_or_else(|| bad("empty client finish"))?;
        let their_key = proto::bytes(cf, 1).ok_or_else(|| bad("no key"))?;
        let secret = keys.shared(their_key).ok_or_else(|| bad("bad key"))?;
        let derived = crypto::derive(&secret, &client_init, &server_init, true);
        // Their connection response, then ours; sealed from here.
        let _ = self.recv_raw()?;
        self.send_raw(&connection_response())?;
        self.keys = Some(derived);
        self.send_sharing(&paired_key_encryption())?;
        self.expect_sharing(3)?;
        self.send_sharing(&paired_key_result())?;
        self.expect_sharing(4)?;
        Ok(device)
    }

    /// We connected to someone (to send them something): the handshake from
    /// our side. `me` is our name; `endpoint` our 4-character id.
    pub fn connect(&mut self, me: &str, endpoint: &str) -> io::Result<()> {
        let request = Msg::new().str(1, endpoint).str(2, me).bytes(6, &endpoint_info(me, 3)).int(5, 5); // over Wi-Fi
        self.send_raw(&offline(1, 2, request))?;
        let keys = KeyPair::new();
        let client_finish = Msg::new().int(1, 4).msg(2, Msg::new().bytes(1, &keys.public_message())).build();
        let commitment = crypto::sha512(&client_finish);
        let client_init = Msg::new()
            .int(1, 2)
            .msg(
                2,
                Msg::new()
                    .int(1, 1)
                    .bytes(2, &crypto::random(32))
                    .msg(3, Msg::new().int(1, 100).bytes(2, &commitment))
                    .str(4, "AES_256_CBC-HMAC_SHA256"),
            )
            .build();
        self.send_raw(&client_init)?;
        let server_init = self.recv_raw()?;
        if proto::int(&server_init, 1) != Some(3) {
            return Err(bad("expected UKEY2 server init"));
        }
        let si = proto::bytes(&server_init, 2).ok_or_else(|| bad("empty server init"))?;
        if proto::int(si, 3) != Some(100) {
            return Err(bad("they don't do P-256"));
        }
        let their_key = proto::bytes(si, 4).ok_or_else(|| bad("no key"))?;
        let secret = keys.shared(their_key).ok_or_else(|| bad("bad key"))?;
        let derived = crypto::derive(&secret, &client_init, &server_init, false);
        self.send_raw(&client_finish)?;
        self.send_raw(&connection_response())?;
        // Their answer to the connection.
        let resp = self.recv_raw()?;
        let accepted = proto::bytes(&resp, 2).and_then(|v1| proto::bytes(v1, 3)).and_then(|r| proto::int(r, 3));
        if accepted != Some(1) {
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, "they turned the connection down"));
        }
        self.keys = Some(derived);
        self.send_sharing(&paired_key_encryption())?;
        self.expect_sharing(3)?;
        self.send_sharing(&paired_key_result())?;
        self.expect_sharing(4)?;
        Ok(())
    }

    // --------------------------------------------------------- transfer --

    /// What they're offering (after `accept`).
    pub fn offer(&mut self) -> io::Result<Offer> {
        let intro = self.expect_sharing(1)?;
        let v1 = proto::bytes(&intro, 2).unwrap_or_default();
        let i = proto::bytes(v1, 2).unwrap_or_default();
        let files = proto::all(i, 1)
            .into_iter()
            .map(|f| OfferedFile {
                name: proto::string(f, 1).unwrap_or_else(|| "file".into()),
                id: proto::int(f, 3).unwrap_or(0),
                size: proto::int(f, 4).unwrap_or(0).max(0),
            })
            .collect();
        let texts = proto::all(i, 2)
            .into_iter()
            .map(|t| OfferedText {
                title: proto::string(t, 2).unwrap_or_default(),
                kind: proto::int(t, 3).unwrap_or(0),
                id: proto::int(t, 4).unwrap_or(0),
                size: proto::int(t, 5).unwrap_or(0).max(0),
            })
            .collect();
        Ok(Offer { files, texts })
    }

    /// Answers the offer: yes or no.
    pub fn answer(&mut self, accept: bool) -> io::Result<()> {
        self.send_sharing(&response(if accept { 1 } else { 2 }))?;
        if !accept {
            self.disconnect();
        }
        Ok(())
    }

    /// Offers `files` (name, size, mime type, payload id); true once accepted.
    pub fn introduce(&mut self, files: &[(String, i64, String, i64)]) -> io::Result<bool> {
        let mut intro = Msg::new();
        for (name, size, mime, id) in files {
            let kind = match mime.split('/').next().unwrap_or("") {
                "image" => 1,
                "video" => 2,
                "audio" => 4,
                _ if name.to_ascii_lowercase().ends_with(".apk") => 3,
                _ => 0,
            };
            intro = intro.msg(1, Msg::new().str(1, name).int(2, kind).int(3, *id).int(4, *size).str(5, mime));
        }
        self.send_sharing(&sharing(1, 2, intro))?;
        let answer = self.expect_sharing(2)?;
        let status = proto::bytes(&answer, 2).and_then(|v1| proto::bytes(v1, 3)).and_then(|r| proto::int(r, 1));
        Ok(status == Some(1))
    }

    /// Bytes payloads that arrived already (a text sent with the offer).
    pub fn take_held(&mut self) -> Vec<(i64, Vec<u8>)> {
        std::mem::take(&mut self.held)
    }
}

/// A PayloadTransferFrame in an OfflineFrame.
fn payload(id: i64, kind: i64, total: i64, offset: i64, last: bool, body: Option<&[u8]>) -> Vec<u8> {
    let header = Msg::new().int(1, id).int(2, kind).int(3, total).bool(4, false);
    let mut chunk = Msg::new().int(1, last as i64).int(2, offset);
    if let Some(b) = body {
        chunk = chunk.bytes(3, b);
    }
    offline(3, 4, Msg::new().int(1, 1).msg(2, header).msg(3, chunk))
}

/// What someone wants to send us.
#[derive(Clone, Debug, Default)]
pub struct Offer {
    pub files: Vec<OfferedFile>,
    pub texts: Vec<OfferedText>,
}

#[derive(Clone, Debug)]
pub struct OfferedFile {
    pub name: String,
    pub id: i64,
    pub size: i64,
}

/// A text, a link, an address or a phone number.
#[derive(Clone, Debug)]
pub struct OfferedText {
    pub title: String,
    /// 1 text, 2 link, 3 address, 4 phone number.
    pub kind: i64,
    pub id: i64,
    pub size: i64,
}

impl Offer {
    pub fn total(&self) -> i64 {
        self.files.iter().map(|f| f.size).sum::<i64>() + self.texts.iter().map(|t| t.size).sum::<i64>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    /// Two ends of a connection on this machine.
    fn pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let b = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (a, _) = l.accept().unwrap();
        (a, b)
    }

    #[test]
    fn a_file_goes_across() {
        let (a, b) = pair();
        let data: Vec<u8> = (0..1_300_000u32).map(|i| (i % 251) as u8).collect();
        let expected = data.clone();
        let receiver = std::thread::spawn(move || {
            let mut c = Conn::new(a);
            let who = c.accept().unwrap();
            assert_eq!(who, Device { name: "Sender PC".into(), kind: 3 });
            let pin = c.pin();
            let offer = c.offer().unwrap();
            assert_eq!(offer.files.len(), 1);
            assert_eq!(offer.files[0].name, "photo.jpg");
            c.answer(true).unwrap();
            let mut got = Vec::new();
            loop {
                match c.next().unwrap() {
                    Event::File { body, last, .. } => {
                        got.extend(body);
                        if last {
                            break;
                        }
                    }
                    Event::Disconnect => break,
                    Event::Bytes { .. } => {}
                }
            }
            (got, pin)
        });
        let mut c = Conn::new(b);
        c.connect("Sender PC", "ABCD").unwrap();
        let pin = c.pin();
        let id = crypto::random_i64();
        let accepted = c.introduce(&[("photo.jpg".into(), data.len() as i64, "image/jpeg".into(), id)]).unwrap();
        assert!(accepted);
        let mut off = 0;
        for chunk in data.chunks(CHUNK) {
            c.poll().unwrap();
            c.send_file_chunk(id, data.len() as i64, off, chunk).unwrap();
            off += chunk.len() as i64;
        }
        c.disconnect();
        let (got, their_pin) = receiver.join().unwrap();
        assert_eq!(got, expected);
        assert_eq!(pin, their_pin);
    }

    #[test]
    fn endpoint_info_round_trips() {
        let b = endpoint_info("My PC", 3);
        assert_eq!(parse_endpoint_info(&b), Some(Device { name: "My PC".into(), kind: 3 }));
    }
}
