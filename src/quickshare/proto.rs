//! The little of Protocol Buffers that Quick Share's messages need, by hand:
//! numbered fields holding whole numbers (varints) or bytes (strings and
//! nested messages). Messages are built field by field and read back by
//! field number, so no generated code or library is needed.

/// A message being built.
#[derive(Default, Clone)]
pub struct Msg {
    buf: Vec<u8>,
}

fn put_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

impl Msg {
    pub fn new() -> Msg {
        Msg::default()
    }

    /// A whole number (varint; negative ones as protobuf's int64 has them).
    pub fn int(mut self, field: u32, v: i64) -> Msg {
        put_varint(&mut self.buf, (field as u64) << 3);
        put_varint(&mut self.buf, v as u64);
        self
    }

    pub fn bool(self, field: u32, v: bool) -> Msg {
        self.int(field, v as i64)
    }

    pub fn bytes(mut self, field: u32, v: &[u8]) -> Msg {
        put_varint(&mut self.buf, ((field as u64) << 3) | 2);
        put_varint(&mut self.buf, v.len() as u64);
        self.buf.extend_from_slice(v);
        self
    }

    pub fn str(self, field: u32, v: &str) -> Msg {
        self.bytes(field, v.as_bytes())
    }

    pub fn msg(self, field: u32, m: Msg) -> Msg {
        self.bytes(field, &m.buf)
    }

    pub fn build(self) -> Vec<u8> {
        self.buf
    }
}

/// A field's value as read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Val<'a> {
    Int(u64),
    Bytes(&'a [u8]),
}

fn get_varint(buf: &[u8], at: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*at)?;
        *at += 1;
        v |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Every field in `buf`, in order: (number, value). None if it's not a message.
pub fn fields(buf: &[u8]) -> Option<Vec<(u32, Val<'_>)>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < buf.len() {
        let key = get_varint(buf, &mut at)?;
        let (field, wire) = ((key >> 3) as u32, key & 7);
        let val = match wire {
            0 => Val::Int(get_varint(buf, &mut at)?),
            1 => {
                let b = buf.get(at..at + 8)?;
                at += 8;
                Val::Int(u64::from_le_bytes(b.try_into().ok()?))
            }
            2 => {
                let n = get_varint(buf, &mut at)? as usize;
                let b = buf.get(at..at.checked_add(n)?)?;
                at += n;
                Val::Bytes(b)
            }
            5 => {
                let b = buf.get(at..at + 4)?;
                at += 4;
                Val::Int(u32::from_le_bytes(b.try_into().ok()?) as u64)
            }
            _ => return None,
        };
        out.push((field, val));
    }
    Some(out)
}

/// Field `field` as a whole number (its last value, as protobuf reads it).
pub fn int(buf: &[u8], field: u32) -> Option<i64> {
    fields(buf)?.into_iter().rev().find_map(|(f, v)| match v {
        Val::Int(n) if f == field => Some(n as i64),
        _ => None,
    })
}

/// Field `field` as bytes (a string or nested message).
pub fn bytes(buf: &[u8], field: u32) -> Option<&[u8]> {
    fields(buf)?.into_iter().rev().find_map(|(f, v)| match v {
        Val::Bytes(b) if f == field => Some(b),
        _ => None,
    })
}

/// Every value of a repeated field `field`.
pub fn all(buf: &[u8], field: u32) -> Vec<&[u8]> {
    fields(buf)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(f, v)| match v {
            Val::Bytes(b) if f == field => Some(b),
            _ => None,
        })
        .collect()
}

pub fn string(buf: &[u8], field: u32) -> Option<String> {
    bytes(buf, field).map(|b| String::from_utf8_lossy(b).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let inner = Msg::new().int(1, 300).str(2, "hi");
        let m = Msg::new().int(1, -5).msg(2, inner).bool(3, true).bytes(4, &[1, 2]).bytes(4, &[3]).build();
        assert_eq!(int(&m, 1), Some(-5));
        let inner = bytes(&m, 2).unwrap();
        assert_eq!(int(inner, 1), Some(300));
        assert_eq!(string(inner, 2).as_deref(), Some("hi"));
        assert_eq!(int(&m, 3), Some(1));
        assert_eq!(all(&m, 4), vec![&[1u8, 2][..], &[3u8][..]]);
        assert_eq!(int(&m, 9), None);
        assert!(fields(&[0x08]).is_none());
    }
}
