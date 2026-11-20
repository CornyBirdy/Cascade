//! Instant file search: every file and folder on the PC's NTFS drives, found
//! as you type. Like Everything (and WinRust), it reads each drive's own file
//! table (the MFT, see `mft`) in one go instead of walking folders, then
//! keeps up with changes from the drive's change journal (the USN journal),
//! so it never rescans. A million files take a few seconds to read and
//! about 45 MB of memory; a search over all of them takes milliseconds.
//!
//! Reading a drive's file table needs administrator rights, so the index
//! lives in a small second copy of Cascade (`cascade.exe --indexer`), started
//! with those rights by a scheduled task you approve once (as the admin
//! helper is). Cascade asks it over a named pipe that only your account can
//! open. If Cascade itself runs as administrator, the index lives in Cascade.
//! Without either, Start search falls back to Windows' search index.

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Ioctl::*;
use windows::Win32::System::IO::DeviceIoControl;

use crate::{config, mft};

// ------------------------------------------------------------- the index --

const IN_USE: u8 = 1;
const DIR: u8 = 2;
const HIDDEN: u8 = 4;
const SYSTEM: u8 = 8;

/// One file or folder, by its record number on the drive.
#[derive(Clone, Copy, Default)]
struct Entry {
    size: u64,
    parent: u32,
    /// Where its name is in the drive's name store, and how long.
    name: u32,
    /// Seconds since 1601 / 16 (fits a u32 until well past 2100... as minutes would too).
    modified: u32,
    name_len: u16,
    flags: u8,
}

/// One drive's index.
struct Volume {
    letter: char,
    entries: Vec<Entry>,
    names: Vec<u8>,
    /// Folder totals for the disk map (worked out when asked, after changes).
    totals: Option<Vec<u64>>,
}

const ROOT: u32 = 5;

impl Volume {
    fn name(&self, e: &Entry) -> &str {
        let (a, b) = (e.name as usize, e.name as usize + e.name_len as usize);
        std::str::from_utf8(self.names.get(a..b).unwrap_or(&[])).unwrap_or("")
    }

    fn set(&mut self, n: u64, parent: u64, name: &str, flags: u8, size: Option<u64>, modified: u64) {
        let n = n as usize;
        if n >= self.entries.len() {
            self.entries.resize(n + 1024, Entry::default());
        }
        let e = &mut self.entries[n];
        if self.names.get(e.name as usize..e.name as usize + e.name_len as usize) != Some(name.as_bytes()) {
            e.name = self.names.len() as u32;
            e.name_len = name.len().min(u16::MAX as usize) as u16;
            self.names.extend_from_slice(&name.as_bytes()[..e.name_len as usize]);
        }
        e.parent = parent as u32;
        e.flags = flags;
        if let Some(s) = size {
            e.size = s;
        }
        if modified != 0 {
            e.modified = (modified / 10_000_000 / 16) as u32;
        }
        self.totals = None;
    }

    /// The full path of record `n` ("C:\Users\you\file.txt"), or None if
    /// it isn't reachable from the drive's root (deleted, or system).
    fn path(&self, n: u32) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        let mut at = n;
        for _ in 0..256 {
            if at == ROOT {
                parts.reverse();
                return Some(format!("{}:\\{}", self.letter, parts.join("\\")));
            }
            let e = self.entries.get(at as usize)?;
            if e.flags & IN_USE == 0 || e.name_len == 0 {
                return None;
            }
            parts.push(self.name(e));
            at = e.parent;
        }
        None
    }

    /// Record number of a folder's path ("C:\Users"), walking down from the root.
    fn find(&self, path: &str) -> Option<u32> {
        let rest = path.get(3..).unwrap_or("").trim_end_matches('\\');
        let mut at = ROOT;
        for part in rest.split('\\').filter(|p| !p.is_empty()) {
            at = self
                .entries
                .iter()
                .enumerate()
                .find(|(_, e)| e.parent == at && e.flags & IN_USE != 0 && self.name(e).eq_ignore_ascii_case(part))
                .map(|(i, _)| i as u32)?;
        }
        Some(at)
    }

    /// Each folder's total size (all it holds, at every depth).
    fn totals(&mut self) -> &Vec<u64> {
        if self.totals.is_none() {
            let mut t = vec![0u64; self.entries.len()];
            for (i, e) in self.entries.iter().enumerate() {
                if e.flags & IN_USE == 0 || e.flags & DIR != 0 || e.size == 0 {
                    continue;
                }
                let mut at = e.parent as usize;
                for _ in 0..256 {
                    let Some(slot) = t.get_mut(at) else { break };
                    *slot += e.size;
                    if at == ROOT as usize {
                        break;
                    }
                    at = self.entries.get(at).map_or(ROOT as usize, |p| p.parent as usize);
                }
                let _ = i;
            }
            self.totals = Some(t);
        }
        self.totals.as_ref().expect("just made")
    }
}

type Shared = Arc<RwLock<Volume>>;

static VOLUMES: Mutex<Vec<Shared>> = Mutex::new(Vec::new());
static READY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn flags_of(r: &mft::Record) -> u8 {
    let mut f = 0;
    if r.in_use {
        f |= IN_USE;
    }
    if r.dir {
        f |= DIR;
    }
    if r.attributes & 0x2 != 0 {
        f |= HIDDEN;
    }
    if r.attributes & 0x4 != 0 {
        f |= SYSTEM;
    }
    f
}

// ------------------------------------------------------------- reading --

/// A buffer aligned for reading straight from the disk.
struct Aligned {
    ptr: *mut u8,
    len: usize,
}

impl Aligned {
    fn new(len: usize) -> Self {
        let layout = std::alloc::Layout::from_size_align(len.max(4096), 4096).expect("valid layout");
        Self { ptr: unsafe { std::alloc::alloc_zeroed(layout) }, len: len.max(4096) }
    }
    fn slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Aligned {
    fn drop(&mut self) {
        let layout = std::alloc::Layout::from_size_align(self.len, 4096).expect("valid layout");
        unsafe { std::alloc::dealloc(self.ptr, layout) };
    }
}

unsafe fn read_at(h: HANDLE, offset: u64, buf: &mut [u8]) -> bool {
    let mut moved = 0i64;
    if SetFilePointerEx(h, offset as i64, Some(&mut moved), FILE_BEGIN).is_err() {
        return false;
    }
    let mut got = 0u32;
    ReadFile(h, Some(buf), Some(&mut got), None).is_ok() && got as usize == buf.len()
}

unsafe fn open_volume(letter: char) -> Option<HANDLE> {
    CreateFileW(
        &HSTRING::from(format!("\\\\.\\{letter}:")),
        (GENERIC_READ).0,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        None,
        OPEN_EXISTING,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        None,
    )
    .ok()
}

unsafe fn ioctl<I, O>(h: HANDLE, code: u32, input: Option<&I>, output: &mut O) -> bool {
    let mut got = 0u32;
    DeviceIoControl(
        h,
        code,
        input.map(|i| i as *const I as *const _),
        input.map_or(0, |_| std::mem::size_of::<I>() as u32),
        Some(output as *mut O as *mut _),
        std::mem::size_of::<O>() as u32,
        Some(&mut got),
        None,
    )
    .is_ok()
}

/// Reads drive `letter`'s whole file table into a fresh index.
unsafe fn read_volume(h: HANDLE, letter: char) -> Option<Volume> {
    let mut vd = NTFS_VOLUME_DATA_BUFFER::default();
    if !ioctl::<(), _>(h, FSCTL_GET_NTFS_VOLUME_DATA, None, &mut vd) {
        return None;
    }
    let cluster = vd.BytesPerCluster as u64;
    let rec = vd.BytesPerFileRecordSegment as usize;
    if rec == 0 || cluster == 0 {
        return None;
    }
    // Record 0 is the MFT's own: its $DATA says where the rest is.
    let mut first = Aligned::new(rec.max(cluster as usize));
    if !read_at(h, vd.MftStartLcn as u64 * cluster, first.slice()) {
        return None;
    }
    let r0 = &mut first.slice()[..rec];
    if !mft::apply_fixups(r0) {
        return None;
    }
    let runs = mft::data_runs(r0);
    let total = (vd.MftValidDataLength as u64 / rec as u64) as usize;
    let mut v =
        Volume { letter, entries: vec![Entry::default(); total], names: Vec::with_capacity(total * 16), totals: None };
    // Read a megabyte or so at a time (a whole number of clusters and records).
    let chunk = ((1 << 20) / cluster.max(rec as u64) * cluster.max(rec as u64)) as usize;
    let mut buf = Aligned::new(chunk);
    let mut number = 0u64;
    'runs: for (lcn, len) in runs {
        let (mut off, end) = (lcn * cluster, (lcn + len) * cluster);
        while off < end {
            let n = ((end - off) as usize).min(chunk);
            let b = &mut buf.slice()[..n];
            if !read_at(h, off, b) {
                break 'runs;
            }
            for r in b.chunks_exact_mut(rec) {
                if number as usize >= total {
                    break 'runs;
                }
                if mft::apply_fixups(r) {
                    if let Some(p) = mft::parse(r, number) {
                        apply_record(&mut v, &p);
                    }
                }
                number += 1;
            }
            off += n as u64;
        }
    }
    v.totals = None;
    Some(v)
}

fn apply_record(v: &mut Volume, p: &mft::Record) {
    let n = p.base;
    if p.base != 0 && p.name.is_none() && p.size.is_none() {
        return;
    }
    let is_base = p.in_use;
    match &p.name {
        Some(name) if is_base => {
            let flags = flags_of(p);
            v.set(n, p.parent, name, flags, p.size, p.modified)
        }
        // An extension record: only its size (or name) adds to the base record.
        None if p.size.is_some() => {
            if let Some(e) = v.entries.get_mut(n as usize) {
                e.size = p.size.unwrap_or(e.size);
            }
        }
        _ => {}
    }
}

/// Re-reads one record (after the journal says it changed).
unsafe fn reread(h: HANDLE, v: &mut Volume, number: u64) {
    #[repr(C)]
    struct Out {
        reference: i64,
        length: u32,
        buf: [u8; 4096],
    }
    let input = NTFS_FILE_RECORD_INPUT_BUFFER { FileReferenceNumber: number as i64 };
    let mut out = Out { reference: 0, length: 0, buf: [0; 4096] };
    if !ioctl(h, FSCTL_GET_NTFS_FILE_RECORD, Some(&input), &mut out) {
        return;
    }
    // The record that came back may be an earlier one (that record is unused): only take this one.
    if mft::record_number(out.reference as u64) != number {
        return;
    }
    let len = (out.length as usize).min(4096);
    // FSCTL_GET_NTFS_FILE_RECORD hands records back with their fixups already applied.
    if let Some(p) = mft::parse(&out.buf[..len], number) {
        if !p.in_use {
            if let Some(e) = v.entries.get_mut(number as usize) {
                e.flags = 0;
            }
            v.totals = None;
        } else {
            apply_record(v, &p);
        }
    }
}

/// Follows the drive's change journal from `next`, keeping the index up to
/// date, for as long as the indexer runs.
unsafe fn follow(h: HANDLE, vol: Shared, journal: u64, mut next: i64) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let input = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: next,
            ReasonMask: 0xFFFF_FFFF,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            // Waits until there's something to read.
            BytesToWaitFor: 1,
            UsnJournalID: journal,
        };
        let mut got = 0u32;
        let ok = DeviceIoControl(
            h,
            FSCTL_READ_USN_JOURNAL,
            Some(&input as *const _ as *const _),
            std::mem::size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
            Some(buf.as_mut_ptr() as *mut _),
            buf.len() as u32,
            Some(&mut got),
            None,
        )
        .is_ok();
        if !ok || got < 8 {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        next = i64::from_le_bytes(buf[0..8].try_into().unwrap_or_default());
        let mut at = 8usize;
        let mut changed: Vec<(u64, u32, u64, String, u32)> = Vec::new();
        while at + 60 <= got as usize {
            let b = &buf[at..];
            let len = u32::from_le_bytes(b[0..4].try_into().unwrap_or_default()) as usize;
            if len < 60 || at + len > got as usize {
                break;
            }
            let frn = mft::record_number(u64::from_le_bytes(b[8..16].try_into().unwrap_or_default()));
            let parent = mft::record_number(u64::from_le_bytes(b[16..24].try_into().unwrap_or_default()));
            let reason = u32::from_le_bytes(b[40..44].try_into().unwrap_or_default());
            let attrs = u32::from_le_bytes(b[52..56].try_into().unwrap_or_default());
            let (nl, no) = (
                u16::from_le_bytes(b[56..58].try_into().unwrap_or_default()) as usize,
                u16::from_le_bytes(b[58..60].try_into().unwrap_or_default()) as usize,
            );
            let units: Vec<u16> =
                b.get(no..no + nl).unwrap_or(&[]).chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            changed.push((frn, reason, parent, String::from_utf16_lossy(&units), attrs));
            at += len;
        }
        if changed.is_empty() {
            continue;
        }
        let mut v = vol.write().unwrap_or_else(|e| e.into_inner());
        for (frn, reason, parent, name, attrs) in changed {
            const DELETE: u32 = 0x200;
            const CLOSE: u32 = 0x8000_0000;
            const DATA: u32 = 0x1 | 0x2 | 0x4;
            if reason & DELETE != 0 {
                if let Some(e) = v.entries.get_mut(frn as usize) {
                    e.flags = 0;
                }
                v.totals = None;
                continue;
            }
            let mut flags = IN_USE;
            if attrs & 0x10 != 0 {
                flags |= DIR;
            }
            if attrs & 0x2 != 0 {
                flags |= HIDDEN;
            }
            if attrs & 0x4 != 0 {
                flags |= SYSTEM;
            }
            v.set(frn, parent, &name, flags, None, 0);
            // A file written to: its new size once it's closed.
            if reason & CLOSE != 0 && reason & DATA != 0 {
                reread(h, &mut v, frn);
            }
        }
    }
}

/// The fixed NTFS drives.
fn ntfs_drives() -> Vec<char> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| (b'A' + i) as char)
        .filter(|l| unsafe {
            let root = HSTRING::from(format!("{l}:\\"));
            if GetDriveTypeW(&root) != 3 {
                return false; // DRIVE_FIXED
            }
            let mut fs = [0u16; 32];
            GetVolumeInformationW(&root, None, None, None, None, Some(&mut fs)).is_ok()
                && String::from_utf16_lossy(&fs[..fs.iter().position(|&c| c == 0).unwrap_or(0)]) == "NTFS"
        })
        .collect()
}

/// Reads every NTFS drive and follows their journals, each on a thread of
/// its own. Needs administrator rights (to open the drives).
pub fn start_indexing() {
    for letter in ntfs_drives() {
        std::thread::spawn(move || unsafe {
            let Some(h) = open_volume(letter) else {
                config::log(&format!("index: couldn't open {letter}: (needs administrator rights)"));
                return;
            };
            let mut jd = USN_JOURNAL_DATA_V0::default();
            let journal = ioctl::<(), _>(h, FSCTL_QUERY_USN_JOURNAL, None, &mut jd);
            let t = std::time::Instant::now();
            let Some(v) = read_volume(h, letter) else {
                config::log(&format!("index: couldn't read {letter}:'s file table"));
                return;
            };
            let count = v.entries.iter().filter(|e| e.flags & IN_USE != 0).count();
            config::log(&format!(
                "index: {letter}: {count} files and folders in {} ms ({} KB of names)",
                t.elapsed().as_millis(),
                v.names.len() / 1024
            ));
            let shared: Shared = Arc::new(RwLock::new(v));
            VOLUMES.lock().unwrap_or_else(|e| e.into_inner()).push(shared.clone());
            READY.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            if journal {
                follow(h, shared, jd.UsnJournalID, jd.NextUsn);
            }
        });
    }
}

/// A folder's total and its items (name, folder, size).
pub type Folder = (u64, Vec<(String, bool, u64)>);

// --------------------------------------------------------------- search --

/// What a search found: (path, folder, size, modified as seconds/16 since 1601).
pub struct Hit {
    pub path: String,
    pub dir: bool,
    pub size: u64,
}

fn contains_ci(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_ascii() {
        let (h, n) = (hay.as_bytes(), needle.as_bytes());
        if n.is_empty() {
            return Some(0);
        }
        if h.len() < n.len() {
            return None;
        }
        (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
    } else {
        hay.to_lowercase().find(needle)
    }
}

/// Where a path is (system folders and caches rank last).
fn penalty(path: &str) -> u32 {
    let p = path.to_ascii_lowercase();
    let low = [
        "\\windows\\",
        "\\$recycle.bin",
        "\\appdata\\",
        "\\programdata\\",
        "\\node_modules\\",
        "\\.git\\",
        "\\winsxs\\",
        "\\temp\\",
        "\\cache",
    ];
    low.iter().filter(|w| p.contains(*w)).count() as u32 * 4
}

/// Searches every drive for names holding every word of `query` (folders and
/// files; hidden and system ones left out), under `scope` if it's a folder
/// path. The best `max`, best first.
pub fn search_local(query: &str, scope: &str, max: usize) -> Vec<Hit> {
    let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let scope = scope.trim_end_matches('\\').to_ascii_lowercase();
    let whole = query.trim().to_lowercase();
    let mut found: Vec<(u32, std::cmp::Reverse<u32>, usize, Hit)> = Vec::new();
    for vol in VOLUMES.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        let v = vol.read().unwrap_or_else(|e| e.into_inner());
        if !scope.is_empty() && !scope.starts_with(&v.letter.to_ascii_lowercase().to_string()) {
            continue;
        }
        let mut seen = 0usize;
        for (i, e) in v.entries.iter().enumerate() {
            if e.flags & IN_USE == 0 || e.flags & (HIDDEN | SYSTEM) != 0 || e.name_len == 0 {
                continue;
            }
            let name = v.name(e);
            if name.starts_with('$') {
                continue;
            }
            let Some(at) = contains_ci(name, &words[0]) else { continue };
            if !words[1..].iter().all(|w| contains_ci(name, w).is_some()) {
                continue;
            }
            let Some(path) = v.path(i as u32) else { continue };
            if !scope.is_empty() && !path.to_ascii_lowercase().starts_with(&scope) {
                continue;
            }
            let lower = name.to_lowercase();
            let kind = if lower == whole {
                0
            } else if at == 0 {
                1
            } else if !name.as_bytes()[at - 1].is_ascii_alphanumeric() {
                2
            } else {
                3
            };
            let score = kind + penalty(&path);
            found.push((
                score,
                std::cmp::Reverse(e.modified),
                name.len(),
                Hit { path, dir: e.flags & DIR != 0, size: e.size },
            ));
            seen += 1;
            // Plenty to rank from: keep the best, drop the rest as we go.
            if found.len() > max * 40 {
                found.sort_by(|a, b| (a.0, &a.1, a.2).cmp(&(b.0, &b.1, b.2)));
                found.truncate(max * 4);
            }
            if seen > 200_000 {
                break;
            }
        }
    }
    found.sort_by(|a, b| (a.0, &a.1, a.2).cmp(&(b.0, &b.1, b.2)));
    found.into_iter().take(max).map(|f| f.3).collect()
}

/// A folder's contents with totals, for the disk map: (name, folder, total size, items), biggest first.
pub fn children_local(path: &str) -> Option<Folder> {
    let letter = path.chars().next()?.to_ascii_uppercase();
    let vols = VOLUMES.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let vol = vols.iter().find(|v| v.read().map(|v| v.letter == letter).unwrap_or(false))?;
    let mut v = vol.write().unwrap_or_else(|e| e.into_inner());
    let at = v.find(path)?;
    let totals = v.totals().clone();
    let mut out = Vec::new();
    for (i, e) in v.entries.iter().enumerate() {
        if e.parent == at && i as u32 != at && e.flags & IN_USE != 0 && e.name_len > 0 {
            let dir = e.flags & DIR != 0;
            let size = if dir { totals.get(i).copied().unwrap_or(0) } else { e.size };
            out.push((v.name(e).to_string(), dir, size));
        }
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.2));
    Some((totals.get(at as usize).copied().unwrap_or(0), out))
}

// ----------------------------------------------------------------- pipe --

fn pipe_name() -> HSTRING {
    let mut session = 0u32;
    unsafe {
        let _ = windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(
            windows::Win32::System::Threading::GetCurrentProcessId(),
            &mut session,
        );
    }
    HSTRING::from(format!("\\\\.\\pipe\\CascadeIndex-{session}"))
}

fn write_frame(h: HANDLE, data: &[u8]) -> bool {
    let len = (data.len() as u32).to_le_bytes();
    unsafe {
        let mut n = 0u32;
        WriteFile(h, Some(&len), Some(&mut n), None).is_ok() && WriteFile(h, Some(data), Some(&mut n), None).is_ok()
    }
}

fn read_exact(h: HANDLE, buf: &mut [u8]) -> bool {
    let mut at = 0;
    while at < buf.len() {
        let mut n = 0u32;
        if unsafe { ReadFile(h, Some(&mut buf[at..]), Some(&mut n), None) }.is_err() || n == 0 {
            return false;
        }
        at += n as usize;
    }
    true
}

fn read_frame(h: HANDLE) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    if !read_exact(h, &mut len) {
        return None;
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > 64 << 20 {
        return None;
    }
    let mut data = vec![0u8; len];
    read_exact(h, &mut data).then_some(data)
}

/// Answers one request (the same for the pipe and in-process use).
fn answer(req: &str) -> String {
    let mut lines = req.splitn(4, '\n');
    match lines.next() {
        Some("Q") => {
            let max: usize = lines.next().and_then(|m| m.parse().ok()).unwrap_or(10).min(500);
            let scope = lines.next().unwrap_or("");
            let q = lines.next().unwrap_or("");
            search_local(q, scope, max)
                .into_iter()
                .map(|h| format!("{}\t{}\t{}", if h.dir { 'd' } else { 'f' }, h.size, h.path))
                .collect::<Vec<_>>()
                .join("\n")
        }
        Some("C") => match children_local(lines.next().unwrap_or("")) {
            Some((total, kids)) => {
                let mut out = vec![format!("{total}")];
                out.extend(kids.into_iter().map(|(n, d, s)| format!("{}\t{s}\t{n}", if d { 'd' } else { 'f' })));
                out.join("\n")
            }
            None => String::new(),
        },
        Some("S") => format!("{}", READY.load(std::sync::atomic::Ordering::Acquire)),
        Some("X") => {
            std::process::exit(0);
        }
        _ => String::new(),
    }
}

/// `cascade.exe --indexer`: builds the index, then answers Cascade over the pipe.
pub fn run_indexer() {
    unsafe {
        let _ = windows::Win32::System::Threading::CreateMutexW(None, true, w!("CascadeIndexer"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let _ = windows::Win32::System::Threading::SetPriorityClass(
            windows::Win32::System::Threading::GetCurrentProcess(),
            windows::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS,
        );
    }
    config::log("index: indexer started");
    start_indexing();
    // Quit once Cascade has been gone a while (it starts this again when it comes back).
    std::thread::spawn(|| {
        let mut missing = 0;
        loop {
            std::thread::sleep(Duration::from_secs(30));
            if crate::helper::main_window().is_some() {
                missing = 0;
            } else {
                missing += 1;
                if missing >= 4 {
                    std::process::exit(0);
                }
            }
        }
    });
    serve();
}

/// The pipe's security: only this user (and SYSTEM), reachable from a
/// program without admin rights (medium integrity).
unsafe fn pipe_security() -> Option<windows::Win32::Security::SECURITY_ATTRIBUTES> {
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    };
    use windows::Win32::Security::*;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = HANDLE::default();
    OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
    let mut buf = vec![0u8; 256];
    let mut len = 0u32;
    let ok =
        GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as *mut _), buf.len() as u32, &mut len).is_ok();
    let _ = CloseHandle(token);
    if !ok {
        return None;
    }
    let user = &*(buf.as_ptr() as *const TOKEN_USER);
    let mut sid = windows::core::PWSTR::null();
    ConvertSidToStringSidW(user.User.Sid, &mut sid).ok()?;
    let sid_text = sid.to_string().unwrap_or_default();
    let _ = LocalFree(Some(HLOCAL(sid.0 as *mut _)));
    let sddl = HSTRING::from(format!("D:(A;;GA;;;SY)(A;;GA;;;{sid_text})S:(ML;;NW;;;ME)"));
    let mut sd = PSECURITY_DESCRIPTOR::default();
    ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, 1, &mut sd, None).ok()?;
    Some(SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    })
}

fn serve() {
    use windows::Win32::System::Pipes::*;
    let name = pipe_name();
    let sa = unsafe { pipe_security() };
    loop {
        let h = unsafe {
            CreateNamedPipeW(
                &name,
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                4 * 1024,
                0,
                sa.as_ref().map(|s| s as *const _),
            )
        };
        if h.is_invalid() {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let connected =
            unsafe { ConnectNamedPipe(h, None) }.is_ok() || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if !connected {
            unsafe {
                let _ = CloseHandle(h);
            }
            continue;
        }
        let raw = h.0 as isize;
        std::thread::spawn(move || {
            let h = HANDLE(raw as *mut _);
            while let Some(req) = read_frame(h) {
                let out = answer(&String::from_utf8_lossy(&req));
                if !write_frame(h, out.as_bytes()) {
                    break;
                }
            }
            unsafe {
                let _ = windows::Win32::Storage::FileSystem::FlushFileBuffers(h);
                let _ = windows::Win32::System::Pipes::DisconnectNamedPipe(h);
                let _ = CloseHandle(h);
            }
        });
    }
}

// --------------------------------------------------------------- client --

/// The index runs in this process (Cascade itself has admin rights).
static IN_PROCESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Asks the index (in this process, or the indexer over the pipe). None if
/// there's no index to ask.
fn ask(req: &str) -> Option<String> {
    if IN_PROCESS.load(std::sync::atomic::Ordering::Acquire) {
        return Some(answer(req));
    }
    use windows::Win32::System::Pipes::WaitNamedPipeW;
    let name = pipe_name();
    unsafe {
        let open = || {
            CreateFileW(
                &name,
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        };
        let h = match open() {
            Ok(h) => h,
            Err(_) => {
                if !WaitNamedPipeW(&name, 150).as_bool() {
                    return None;
                }
                open().ok()?
            }
        };
        let out = write_frame(h, req.as_bytes()).then(|| read_frame(h)).flatten();
        let _ = CloseHandle(h);
        out.map(|b| String::from_utf8_lossy(&b).into_owned())
    }
}

/// Whether an index is there to ask, with at least one drive read.
pub fn ready() -> bool {
    ask("S").and_then(|s| s.trim().parse::<usize>().ok()).is_some_and(|n| n > 0)
}

/// Searches the index: up to `max` hits under `scope` ("" for everywhere). None without an index.
pub fn search(query: &str, scope: &str, max: usize) -> Option<Vec<Hit>> {
    let out = ask(&format!("Q\n{max}\n{scope}\n{query}"))?;
    Some(
        out.lines()
            .filter_map(|l| {
                let mut p = l.splitn(3, '\t');
                let dir = p.next()? == "d";
                let size = p.next()?.parse().ok()?;
                Some(Hit { path: p.next()?.to_string(), dir, size })
            })
            .collect(),
    )
}

/// A folder's contents with every subfolder's total, biggest first: (its own total, (name, folder, size)).
pub fn children(path: &str) -> Option<Folder> {
    let out = ask(&format!("C\n{path}"))?;
    let mut lines = out.lines();
    let total = lines.next()?.parse().ok()?;
    let kids = lines
        .filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            let dir = p.next()? == "d";
            let size = p.next()?.parse().ok()?;
            Some((p.next()?.to_string(), dir, size))
        })
        .collect();
    Some((total, kids))
}

const TASK: &str = "Cascade file index";

/// Turns instant search on or off (the first time, Windows asks for
/// permission to set up the indexer's task). Starts the indexer if it's on.
pub fn set_enabled(on: bool) {
    std::thread::spawn(move || {
        if !on {
            let _ = ask("X");
            crate::helper::delete_task(TASK);
            return;
        }
        if is_elevated() {
            start_in_process();
            return;
        }
        if crate::helper::ensure_task(
            TASK,
            "--indexer",
            "Cascade's instant file search: reads your drives' file tables.",
        ) {
            crate::helper::run_task(TASK);
        }
    });
}

/// At Cascade's start: brings the index up if instant search is on.
pub fn start(on: bool) {
    if !on {
        return;
    }
    if is_elevated() {
        start_in_process();
    } else {
        std::thread::spawn(|| crate::helper::run_task(TASK));
    }
}

fn start_in_process() {
    if !IN_PROCESS.swap(true, std::sync::atomic::Ordering::AcqRel) {
        start_indexing();
    }
}

fn is_elevated() -> bool {
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}
