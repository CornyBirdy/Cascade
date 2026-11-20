//! The file manager's reading side (see `files`): what's in a folder (a
//! real one, read straight from the disk; or one of Windows' own, such as
//! This PC or the Recycle Bin, through the shell), its icons and thumbnails,
//! all on threads of their own so the window never waits.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;

use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{DeleteObject, HPALETTE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::SystemServices::{SFGAO_FILESYSANCESTOR, SFGAO_FILESYSTEM, SFGAO_FOLDER, SFGAO_HIDDEN};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Shell::*;

/// Where a tab is.
#[derive(Clone, Debug, PartialEq)]
pub enum Loc {
    /// Your folders and drives.
    Home,
    /// A folder on disk ("C:\Users\you").
    Path(String),
    /// One of Windows' own folders, by its shell name (::{GUID}, shell:...).
    Shell(String, String),
    /// Search results: (where, what for).
    Search(String, String),
}

impl Loc {
    /// What the tab and title say.
    pub fn title(&self) -> String {
        match self {
            Loc::Home => "Home".into(),
            Loc::Path(p) => {
                let t = p.trim_end_matches('\\');
                let name = t.rsplit('\\').next().unwrap_or(t);
                if name.len() == 2 && name.ends_with(':') {
                    format!("Local Disk ({name})")
                } else {
                    name.to_string()
                }
            }
            Loc::Shell(_, name) => name.clone(),
            Loc::Search(_, q) => format!("Search: {q}"),
        }
    }

    /// What the address box shows.
    pub fn address(&self) -> String {
        match self {
            Loc::Home => "Home".into(),
            Loc::Path(p) => p.clone(),
            Loc::Shell(_, name) => name.clone(),
            Loc::Search(scope, q) => format!("{q} in {scope}"),
        }
    }

    /// The folder on disk, if it is one (for New, Paste, Terminal...).
    pub fn dir(&self) -> Option<String> {
        match self {
            Loc::Path(p) => Some(p.clone()),
            _ => None,
        }
    }

    /// Where a typed address goes ("C:\", "shell:Downloads", "::{GUID}", "Home").
    pub fn parse(text: &str) -> Option<Loc> {
        let t = text.trim().trim_matches('"');
        if t.eq_ignore_ascii_case("home") || t.is_empty() {
            return Some(Loc::Home);
        }
        if t.eq_ignore_ascii_case("this pc") {
            return Some(this_pc());
        }
        if t.eq_ignore_ascii_case("recycle bin") {
            return Some(Loc::Shell("::{645FF040-5081-101B-9F08-00AA002F954E}".into(), "Recycle Bin".into()));
        }
        let expanded = crate::shellmode::expand_env(t);
        if std::path::Path::new(&expanded).is_dir() {
            let mut p = expanded;
            if p.len() == 2 && p.ends_with(':') {
                p.push('\\');
            }
            return Some(Loc::Path(p));
        }
        // shell:Downloads, ::{GUID} and the like: the shell knows where.
        unsafe {
            let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(t), None).ok()?;
            Some(from_item(&item))
        }
    }
}

pub fn this_pc() -> Loc {
    Loc::Shell("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".into(), "This PC".into())
}

/// Where a shell item is: its folder on disk if it has one.
unsafe fn from_item(item: &IShellItem) -> Loc {
    let name = item.GetDisplayName(SIGDN_NORMALDISPLAY).map(|p| take(p)).unwrap_or_default();
    if let Ok(p) = item.GetDisplayName(SIGDN_FILESYSPATH) {
        let p = take(p);
        if std::path::Path::new(&p).is_dir() {
            return Loc::Path(p);
        }
    }
    let parse = item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING).map(|p| take(p)).unwrap_or_default();
    Loc::Shell(parse, name)
}

unsafe fn take(p: PWSTR) -> String {
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

/// One thing in a folder.
#[derive(Clone, Debug, Default)]
pub struct Item {
    pub name: String,
    /// Its path, or its shell name for Windows' own things.
    pub path: String,
    /// It opens in the file manager (a folder, a drive, This PC...).
    pub folder: bool,
    pub size: Option<u64>,
    /// FILETIME, for sorting; and as text.
    pub modified: u64,
    pub date: String,
    pub kind: String,
    pub hidden: bool,
    /// What its icon is cached by (its extension for plain files).
    pub icon_key: String,
    /// A drive: (free, total).
    pub space: Option<(u64, u64)>,
}

/// "19/11/2026 8:03 AM", in Windows' own short formats.
pub fn date_text(ft: u64) -> String {
    use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, DATE_SHORTDATE, TIME_NOSECONDS};
    if ft == 0 {
        return String::new();
    }
    unsafe {
        let f = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
        let mut utc = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        if FileTimeToSystemTime(&f, &mut utc).is_err()
            || SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err()
        {
            return String::new();
        }
        let (mut d, mut t) = ([0u16; 64], [0u16; 64]);
        let nd = GetDateFormatEx(
            windows::core::PCWSTR::null(),
            DATE_SHORTDATE,
            Some(&local),
            windows::core::PCWSTR::null(),
            Some(&mut d),
            windows::core::PCWSTR::null(),
        );
        let nt = GetTimeFormatEx(
            windows::core::PCWSTR::null(),
            TIME_NOSECONDS,
            Some(&local),
            windows::core::PCWSTR::null(),
            Some(&mut t),
        );
        let s = |b: &[u16], n: i32| String::from_utf16_lossy(&b[..(n.max(1) - 1) as usize]);
        format!("{} {}", s(&d, nd), s(&t, nt))
    }
}

/// "Text Document" for ".txt": Windows' name for a kind of file (asked once per extension).
fn type_name(ext: &str, dir: bool) -> String {
    static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    if dir {
        return "File folder".into();
    }
    let key = ext.to_ascii_lowercase();
    if let Some(t) = CACHE.lock().ok().and_then(|c| c.as_ref().and_then(|m| m.get(&key).cloned())) {
        return t;
    }
    let mut info = SHFILEINFOW::default();
    let name = HSTRING::from(format!("x{key}"));
    unsafe {
        SHGetFileInfoW(
            &name,
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
        );
    }
    let n = info.szTypeName.iter().position(|&c| c == 0).unwrap_or(0);
    let mut t = String::from_utf16_lossy(&info.szTypeName[..n]);
    if t.is_empty() {
        t = if key.is_empty() { "File".into() } else { format!("{} File", key.trim_start_matches('.').to_uppercase()) };
    }
    if let Ok(mut c) = CACHE.lock() {
        c.get_or_insert_with(HashMap::new).insert(key, t.clone());
    }
    t
}

/// Files whose icon is their own (programs, shortcuts, icons): cached by path, not extension.
fn own_icon(ext: &str) -> bool {
    matches!(ext, ".exe" | ".lnk" | ".ico" | ".url" | ".cpl" | ".msc" | ".scr" | ".appref-ms" | ".cur" | ".ani")
}

pub fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// The name as shown: without its extension when Windows is set to hide them.
fn shown_name(name: &str, hide_ext: bool) -> String {
    if hide_ext {
        if let Some(i) = name.rfind('.') {
            if i > 0 {
                return name[..i].to_string();
            }
        }
    }
    name.to_string()
}

/// A folder on disk, read straight from it (fast even with thousands of files).
fn read_dir(dir: &str, hidden: bool, system: bool, hide_ext: bool) -> Vec<Item> {
    let mut out = Vec::new();
    let pattern = HSTRING::from(format!("{}\\*", dir.trim_end_matches('\\')));
    unsafe {
        let mut fd = WIN32_FIND_DATAW::default();
        let Ok(h) = FindFirstFileExW(
            &pattern,
            FindExInfoBasic,
            &mut fd as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        ) else {
            return out;
        };
        loop {
            let n = fd.cFileName.iter().position(|&c| c == 0).unwrap_or(0);
            let name = String::from_utf16_lossy(&fd.cFileName[..n]);
            let attrs = fd.dwFileAttributes;
            let is_hidden = attrs & FILE_ATTRIBUTE_HIDDEN.0 != 0;
            let is_system = attrs & FILE_ATTRIBUTE_SYSTEM.0 != 0;
            let skip = name == "." || name == ".." || (is_hidden && !hidden) || (is_hidden && is_system && !system);
            if !skip {
                let dir_ = attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
                let path = format!("{}\\{name}", dir.trim_end_matches('\\'));
                let ext = if dir_ { String::new() } else { ext_of(&name) };
                let modified =
                    ((fd.ftLastWriteTime.dwHighDateTime as u64) << 32) | fd.ftLastWriteTime.dwLowDateTime as u64;
                let size = ((fd.nFileSizeHigh as u64) << 32) | fd.nFileSizeLow as u64;
                out.push(Item {
                    name: if dir_ { name.clone() } else { shown_name(&name, hide_ext) },
                    icon_key: if dir_ || own_icon(&ext) { path.clone() } else { format!("ext:{ext}") },
                    path,
                    folder: dir_,
                    size: (!dir_).then_some(size),
                    modified,
                    date: date_text(modified),
                    kind: type_name(&ext, dir_),
                    hidden: is_hidden,
                    space: None,
                });
            }
            if FindNextFileW(h, &mut fd).is_err() {
                break;
            }
        }
        let _ = FindClose(h);
    }
    out
}

const PKEY_SIZE: windows::Win32::Foundation::PROPERTYKEY =
    PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0xB725F130_47EF_101A_A5F1_02608C9EEBAC), pid: 12 };
const PKEY_MODIFIED: PROPERTYKEY =
    PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0xB725F130_47EF_101A_A5F1_02608C9EEBAC), pid: 14 };
const PKEY_TYPE: PROPERTYKEY =
    PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0xB725F130_47EF_101A_A5F1_02608C9EEBAC), pid: 4 };

/// One of Windows' own folders (This PC, the Recycle Bin...), through the shell.
unsafe fn read_shell(parse: &str) -> Vec<Item> {
    let mut out = Vec::new();
    let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(parse), None) else { return out };
    let Ok(list) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return out };
    loop {
        let mut batch = [None; 1];
        let mut got = 0u32;
        if list.Next(&mut batch, Some(&mut got)).is_err() || got == 0 {
            break;
        }
        let Some(it) = batch[0].take() else { break };
        let name = it.GetDisplayName(SIGDN_NORMALDISPLAY).map(|p| take(p)).unwrap_or_default();
        let attrs = it
            .GetAttributes(SFGAO_FOLDER | SFGAO_FILESYSTEM | SFGAO_HIDDEN | SFGAO_FILESYSANCESTOR)
            .unwrap_or_default();
        let fs_path = it.GetDisplayName(SIGDN_FILESYSPATH).ok().map(|p| take(p));
        let parse = it.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING).map(|p| take(p)).unwrap_or_default();
        let folder = attrs.0 & SFGAO_FOLDER.0 != 0 && !parse.to_ascii_lowercase().ends_with(".zip");
        let (mut size, mut modified, mut kind) = (None, 0u64, String::new());
        if let Ok(i2) = it.cast::<IShellItem2>() {
            size = i2.GetUInt64(&PKEY_SIZE).ok();
            if let Ok(ft) = i2.GetFileTime(&PKEY_MODIFIED) {
                modified = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
            }
            kind = i2.GetString(&PKEY_TYPE).map(|p| take(p)).unwrap_or_default();
        }
        // A drive: its free space.
        let path = fs_path.clone().unwrap_or(parse.clone());
        let space = (path.len() == 3 && path.ends_with(":\\")).then(|| {
            let (mut free, mut total) = (0u64, 0u64);
            let _ = GetDiskFreeSpaceExW(&HSTRING::from(path.as_str()), None, Some(&mut total), Some(&mut free));
            (free, total)
        });
        out.push(Item {
            name,
            icon_key: path.clone(),
            path,
            folder,
            size: if folder { None } else { size },
            modified,
            date: date_text(modified),
            kind,
            hidden: attrs.0 & SFGAO_HIDDEN.0 != 0,
            space,
        });
    }
    out
}

/// Home: your folders, then your drives.
unsafe fn read_home() -> Vec<Item> {
    let mut out = Vec::new();
    for id in [
        &FOLDERID_Desktop,
        &FOLDERID_Downloads,
        &FOLDERID_Documents,
        &FOLDERID_Pictures,
        &FOLDERID_Music,
        &FOLDERID_Videos,
    ] {
        if let Ok(p) = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) {
            let path = take(p);
            let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
            out.push(Item {
                name,
                icon_key: path.clone(),
                path,
                folder: true,
                kind: "Folder".into(),
                ..Default::default()
            });
        }
    }
    out.extend(read_shell("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}").into_iter().filter(|i| i.space.is_some()));
    out
}

/// Search results: from the instant index when it's on, or by walking the
/// folder (stopping at 2,000 found or 20 seconds).
fn read_search(scope: &str, query: &str, hide_ext: bool) -> Vec<Item> {
    let to_item = |path: String, dir: bool, size: Option<u64>| {
        let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
        let ext = if dir { String::new() } else { ext_of(&name) };
        let meta = std::fs::metadata(&path).ok();
        let modified = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() * 10_000_000 + 116_444_736_000_000_000);
        Item {
            name: if dir { name.clone() } else { shown_name(&name, hide_ext) },
            icon_key: if dir || own_icon(&ext) { path.clone() } else { format!("ext:{ext}") },
            kind: path.rsplit_once('\\').map(|(p, _)| p.to_string()).unwrap_or_default(),
            path,
            folder: dir,
            size,
            modified,
            date: date_text(modified),
            ..Default::default()
        }
    };
    if let Some(hits) = crate::fileindex::search(query, scope, 2000) {
        return hits.into_iter().map(|h| to_item(h.path, h.dir, (!h.dir).then_some(h.size))).collect();
    }
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(scope)];
    while let Some(d) = stack.pop() {
        if out.len() >= 2000 || start.elapsed().as_secs() > 20 {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            use std::os::windows::fs::MetadataExt;
            let Ok(m) = e.metadata() else { continue };
            if m.file_attributes() & (0x2 | 0x400) != 0 {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_lowercase();
            if m.is_dir() {
                stack.push(e.path());
            }
            if words.iter().all(|w| name.contains(w.as_str())) {
                out.push(to_item(
                    e.path().to_string_lossy().into_owned(),
                    m.is_dir(),
                    (!m.is_dir()).then_some(m.len()),
                ));
            }
        }
    }
    out
}

/// Explorer's settings: show hidden files, show protected system files, hide extensions.
pub fn explorer_settings() -> (bool, bool, bool) {
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;
    const ADV: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced";
    let hidden = crate::winset::reg_dword(HKEY_CURRENT_USER, ADV, "Hidden") == Some(1);
    let system = crate::winset::reg_dword(HKEY_CURRENT_USER, ADV, "ShowSuperHidden") == Some(1);
    let hide_ext = crate::winset::reg_dword(HKEY_CURRENT_USER, ADV, "HideFileExt") != Some(0);
    (hidden, system, hide_ext)
}

/// What's at `loc` (on the calling thread: call it from a worker).
pub fn load(loc: &Loc, show_hidden: bool) -> Vec<Item> {
    let (hidden, system, hide_ext) = explorer_settings();
    let hidden = hidden || show_hidden;
    unsafe {
        match loc {
            Loc::Home => read_home(),
            Loc::Path(p) => read_dir(p, hidden, system && hidden, hide_ext),
            Loc::Shell(parse, _) => read_shell(parse).into_iter().filter(|i| hidden || !i.hidden).collect(),
            Loc::Search(scope, q) => read_search(scope, q, hide_ext),
        }
    }
}

// --------------------------------------------------------------- icons --

/// Asks for icons and thumbnails on a thread of its own: send (key, path,
/// thumbnail), get (key, premultiplied BGRA 96 x 96) back through `done`,
/// which is told with a message to `hwnd`.
/// Icons that have come back: (key, pixels).
pub type Arrived = std::sync::Arc<Mutex<Vec<(String, Vec<u8>)>>>;

pub fn icon_worker(hwnd: HWND, msg: u32) -> (Sender<(String, String, bool)>, Arrived) {
    let (tx, rx) = channel::<(String, String, bool)>();
    let done = std::sync::Arc::new(Mutex::new(Vec::new()));
    let out = done.clone();
    let hwnd = hwnd.0 as isize;
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(wic) = CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
        else {
            return;
        };
        while let Ok((key, path, thumb)) = rx.recv() {
            let target = path.clone();
            let pixels = image(&wic, &target, thumb).unwrap_or_default();
            if let Ok(mut d) = out.lock() {
                d.push((key, pixels));
            }
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(HWND(hwnd as *mut _)),
                msg,
                WPARAM(0),
                LPARAM(0),
            );
        }
    });
    (tx, done)
}

unsafe fn image(wic: &IWICImagingFactory, path: &str, thumb: bool) -> Option<Vec<u8>> {
    let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
    let factory: IShellItemImageFactory = item.cast().ok()?;
    let size = SIZE { cx: crate::icons::ICON_SIZE as i32, cy: crate::icons::ICON_SIZE as i32 };
    let flags = if thumb { SIIGBF_BIGGERSIZEOK } else { SIIGBF(SIIGBF_ICONONLY.0 | SIIGBF_BIGGERSIZEOK.0) };
    let hbmp = factory.GetImage(size, flags).ok()?;
    let r = wic
        .CreateBitmapFromHBITMAP(hbmp, HPALETTE::default(), WICBitmapUsePremultipliedAlpha)
        .ok()
        .and_then(|b| crate::icons::to_pixels(wic, &b).ok());
    let _ = DeleteObject(hbmp.into());
    r
}

/// Whether a file gets a thumbnail (a picture or video) rather than its icon.
pub fn has_thumbnail(name: &str) -> bool {
    matches!(
        ext_of(name).as_str(),
        ".jpg"
            | ".jpeg"
            | ".png"
            | ".gif"
            | ".bmp"
            | ".webp"
            | ".heic"
            | ".tif"
            | ".tiff"
            | ".mp4"
            | ".mkv"
            | ".mov"
            | ".avi"
            | ".webm"
            | ".pdf"
    )
}
