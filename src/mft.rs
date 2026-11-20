//! Reading NTFS's own file table (the MFT): plain parsing of its records,
//! with nothing Windows-specific, so it can be tested anywhere. The volume
//! reading and the index around it are in `fileindex`.
//!
//! A record is 1 KB (usually): a header, then attributes. Cascade reads
//! three of them: $STANDARD_INFORMATION (attributes, date modified),
//! $FILE_NAME (name and parent folder) and the unnamed $DATA (the size).

/// What one record says about a file or folder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Record {
    /// The record this one belongs to: itself, or (for an extension record
    /// of a file with many attributes) its base record.
    pub base: u64,
    pub in_use: bool,
    pub dir: bool,
    /// The parent folder's record number, and the name (the Win32 one, not
    /// the 8.3 short name when there's a choice).
    pub parent: u64,
    pub name: Option<String>,
    /// 1 Win32, 2 DOS, 3 both, 0 POSIX: the name's kind.
    pub namespace: u8,
    pub size: Option<u64>,
    /// FILETIME of the last change, and Windows' file attributes.
    pub modified: u64,
    pub attributes: u32,
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// A file reference's record number (its low 48 bits; the rest is a sequence number).
pub fn record_number(reference: u64) -> u64 {
    reference & 0x0000_FFFF_FFFF_FFFF
}

/// Puts back the last two bytes of each 512-byte sector, which NTFS swaps
/// for a check value on disk (the "update sequence"). False if the record
/// is torn (a sector's check value doesn't match).
pub fn apply_fixups(rec: &mut [u8]) -> bool {
    let (Some(off), Some(count)) = (u16_at(rec, 4), u16_at(rec, 6)) else { return false };
    let (off, count) = (off as usize, count as usize);
    if count == 0 || off + count * 2 > rec.len() {
        return false;
    }
    let check = [rec[off], rec[off + 1]];
    for i in 1..count {
        let end = i * 512;
        if end > rec.len() || end < 2 {
            break;
        }
        if rec[end - 2..end] != check {
            return false;
        }
        rec[end - 2] = rec[off + i * 2];
        rec[end - 1] = rec[off + i * 2 + 1];
    }
    true
}

/// Reads a record (already fixed up). None if it isn't a file record.
pub fn parse(rec: &[u8], number: u64) -> Option<Record> {
    if rec.get(0..4)? != b"FILE" {
        return None;
    }
    let flags = u16_at(rec, 22)?;
    let base = record_number(u64_at(rec, 32)?);
    let mut r = Record {
        base: if base == 0 { number } else { base },
        in_use: flags & 1 != 0,
        dir: flags & 2 != 0,
        ..Default::default()
    };
    let used = (u32_at(rec, 24)? as usize).min(rec.len());
    let mut at = u16_at(rec, 20)? as usize;
    while at + 16 <= used {
        let kind = u32_at(rec, at)?;
        if kind == 0xFFFF_FFFF {
            break;
        }
        let len = u32_at(rec, at + 4)? as usize;
        if len < 16 || at + len > used {
            break;
        }
        let attr = &rec[at..at + len];
        let nonresident = attr[8] != 0;
        let named = attr[9] != 0;
        let value = || -> Option<&[u8]> {
            let (vl, vo) = (u32_at(attr, 16)? as usize, u16_at(attr, 20)? as usize);
            attr.get(vo..vo + vl)
        };
        match kind {
            0x10 if !nonresident => {
                if let Some(v) = value() {
                    r.modified = u64_at(v, 8).unwrap_or(0);
                    r.attributes = u32_at(v, 32).unwrap_or(0);
                }
            }
            0x30 if !nonresident => {
                if let Some(v) = value() {
                    let ns = *v.get(65)?;
                    let n = *v.get(64)? as usize;
                    // Keep the Win32 name over the 8.3 one.
                    if r.name.is_none() || r.namespace == 2 {
                        let units: Vec<u16> = v
                            .get(66..66 + n * 2)?
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|c| u16::from_le_bytes([c[0], c[1]]))
                            .collect();
                        r.name = Some(String::from_utf16_lossy(&units));
                        r.namespace = ns;
                        r.parent = record_number(u64_at(v, 0)?);
                    }
                }
            }
            0x80 if !named => {
                if nonresident {
                    // The real size is in the first piece of the attribute only.
                    if u64_at(attr, 16)? == 0 {
                        r.size = u64_at(attr, 48);
                    }
                } else {
                    r.size = Some(u32_at(attr, 16)? as u64);
                }
            }
            _ => {}
        }
        at += len;
    }
    Some(r)
}

/// The $DATA attribute's runs (where its pieces are on disk: first cluster,
/// number of clusters) in a record, for reading the MFT itself.
pub fn data_runs(rec: &[u8]) -> Vec<(u64, u64)> {
    let (Some(first), Some(used)) = (u16_at(rec, 20), u32_at(rec, 24)) else { return Vec::new() };
    let (mut at, used) = (first as usize, (used as usize).min(rec.len()));
    while at + 16 <= used {
        let (Some(kind), Some(len)) = (u32_at(rec, at), u32_at(rec, at + 4)) else { break };
        let len = len as usize;
        if kind == 0xFFFF_FFFF || len < 16 || at + len > used {
            break;
        }
        let attr = &rec[at..at + len];
        if kind == 0x80 && attr[8] != 0 && attr[9] == 0 {
            let Some(ro) = u16_at(attr, 32) else { break };
            return decode_runs(&attr[ro as usize..]);
        }
        at += len;
    }
    Vec::new()
}

/// A run list: each entry's header byte says how many bytes its length and
/// (signed, relative to the last) starting cluster take.
pub fn decode_runs(b: &[u8]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let (mut at, mut lcn) = (0usize, 0i64);
    while at < b.len() && b[at] != 0 {
        let h = b[at];
        let (nl, no) = ((h & 0x0F) as usize, (h >> 4) as usize);
        at += 1;
        if nl == 0 || nl > 8 || no > 8 || at + nl + no > b.len() {
            break;
        }
        let mut len = 0u64;
        for i in 0..nl {
            len |= (b[at + i] as u64) << (8 * i);
        }
        at += nl;
        if no == 0 {
            // A sparse run (no clusters on disk): nothing to read there.
            continue;
        }
        let mut off = 0i64;
        for i in 0..no {
            off |= (b[at + i] as i64) << (8 * i);
        }
        // Sign-extend.
        let shift = 64 - 8 * no as u32;
        off = (off << shift) >> shift;
        at += no;
        lcn += off;
        out.push((lcn as u64, len));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1 KB record with fixups, a $STANDARD_INFORMATION, a $FILE_NAME and a resident $DATA.
    fn sample() -> Vec<u8> {
        let mut r = vec![0u8; 1024];
        r[0..4].copy_from_slice(b"FILE");
        r[4..6].copy_from_slice(&48u16.to_le_bytes()); // update sequence at 48
        r[6..8].copy_from_slice(&3u16.to_le_bytes()); // 1 + 2 sectors
        r[20..22].copy_from_slice(&56u16.to_le_bytes()); // first attribute
        r[22..24].copy_from_slice(&1u16.to_le_bytes()); // in use, a file
                                                        // $STANDARD_INFORMATION, resident, value of 72 bytes.
        let mut at = 56;
        let si = |r: &mut Vec<u8>, at: usize| {
            r[at..at + 4].copy_from_slice(&0x10u32.to_le_bytes());
            r[at + 4..at + 8].copy_from_slice(&96u32.to_le_bytes());
            r[at + 16..at + 20].copy_from_slice(&72u32.to_le_bytes());
            r[at + 20..at + 22].copy_from_slice(&24u16.to_le_bytes());
            r[at + 24 + 8..at + 24 + 16].copy_from_slice(&1234u64.to_le_bytes());
            r[at + 24 + 32..at + 24 + 36].copy_from_slice(&0x20u32.to_le_bytes());
        };
        si(&mut r, at);
        at += 96;
        // $FILE_NAME "a.txt" in folder 5.
        let name: Vec<u16> = "a.txt".encode_utf16().collect();
        let vlen = 66 + name.len() * 2;
        let alen = (24 + vlen + 7) & !7;
        r[at..at + 4].copy_from_slice(&0x30u32.to_le_bytes());
        r[at + 4..at + 8].copy_from_slice(&(alen as u32).to_le_bytes());
        r[at + 16..at + 20].copy_from_slice(&(vlen as u32).to_le_bytes());
        r[at + 20..at + 22].copy_from_slice(&24u16.to_le_bytes());
        let v = at + 24;
        r[v..v + 8].copy_from_slice(&(5u64 | (5 << 48)).to_le_bytes());
        r[v + 64] = name.len() as u8;
        r[v + 65] = 1;
        for (i, u) in name.iter().enumerate() {
            r[v + 66 + i * 2..v + 68 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        at += alen;
        // Resident $DATA of 300 bytes.
        r[at..at + 4].copy_from_slice(&0x80u32.to_le_bytes());
        r[at + 4..at + 8].copy_from_slice(&24u32.to_le_bytes());
        r[at + 16..at + 20].copy_from_slice(&300u32.to_le_bytes());
        at += 24;
        r[at..at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        r[24..28].copy_from_slice(&((at + 8) as u32).to_le_bytes());
        // Fixups: check value 0xABCD at the end of each sector, originals saved.
        r[48..50].copy_from_slice(&0xABCDu16.to_le_bytes());
        for s in 1..3 {
            let end = s * 512;
            let orig = [r[end - 2], r[end - 1]];
            r[48 + s * 2] = orig[0];
            r[48 + s * 2 + 1] = orig[1];
            r[end - 2..end].copy_from_slice(&0xABCDu16.to_le_bytes());
        }
        r
    }

    #[test]
    fn reads_a_record() {
        let mut r = sample();
        assert!(apply_fixups(&mut r));
        let rec = parse(&r, 42).unwrap();
        assert_eq!(rec.name.as_deref(), Some("a.txt"));
        assert_eq!(rec.parent, 5);
        assert_eq!(rec.size, Some(300));
        assert_eq!(rec.modified, 1234);
        assert_eq!(rec.attributes, 0x20);
        assert!(rec.in_use && !rec.dir);
        assert_eq!(rec.base, 42);
    }

    #[test]
    fn torn_records_are_refused() {
        let mut r = sample();
        r[1022] = 0;
        assert!(!apply_fixups(&mut r));
    }

    #[test]
    fn decodes_run_lists() {
        // 0x21: 1-byte length, 2-byte offset. 16 clusters at 0x1234, then 8 more 0x10 before... (relative -0x10).
        let runs = [0x21, 0x10, 0x34, 0x12, 0x21, 0x08, 0xF0, 0xFF, 0x00];
        assert_eq!(decode_runs(&runs), vec![(0x1234, 16), (0x1224, 8)]);
        // A sparse run is skipped.
        assert_eq!(decode_runs(&[0x01, 0x05, 0x11, 0x02, 0x10, 0x00]), vec![(0x10, 2)]);
    }
}
