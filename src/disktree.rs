//! Disk usage as a tree, as TreeSize shows it: each folder with its total,
//! files and folders inside, a bar for its share of the folder above, and
//! when it last changed; open a folder to see what's in it, biggest first,
//! with its loose files together as one "[n Files]" row.
//!
//! With the instant-search index on, every folder's total is there at once
//! and a folder's contents are read from it as it's opened. Without it, the
//! folder is walked on a thread of its own, and the tree fills in as it
//! goes: folders appear as they're found and their sizes climb as files are
//! counted, rather than a count and nothing else until it's done.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A folder in the tree.
#[derive(Default, Clone)]
pub struct Node {
    pub name: String,
    pub parent: Option<usize>,
    /// Everything in it, all the way down: bytes, files, folders.
    pub size: u64,
    pub files: u64,
    pub folders: u64,
    /// Its own files (not its folders'): bytes and how many.
    pub own_size: u64,
    pub own_files: u64,
    /// When it or anything in it last changed (FILETIME), 0 if not known.
    pub modified: u64,
    pub children: Vec<usize>,
    /// Its contents have been read (the index fills a folder in when it's opened).
    pub read: bool,
    /// Folders in it (itself included) not read yet: 0 once it's all worked out.
    pub pending: u32,
}

/// How the tree's folders are ordered.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Sort {
    #[default]
    Size,
    Name,
    Files,
    Folders,
    Modified,
}

/// The tree being built, shared with the thread walking it.
pub struct Tree {
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    /// From the index (sizes all known, counts not), or walked.
    pub indexed: bool,
    /// The walk has finished (always, for the index).
    pub done: bool,
    stop: Arc<AtomicBool>,
}

pub type Shared = Arc<Mutex<Tree>>;

impl Tree {
    /// Folder `id`'s path.
    pub fn path_of(&self, id: usize) -> PathBuf {
        let mut parts = Vec::new();
        let mut at = Some(id);
        while let Some(i) = at {
            if i == 0 {
                break;
            }
            parts.push(self.nodes[i].name.clone());
            at = self.nodes[i].parent;
        }
        let mut p = self.root.clone();
        for part in parts.iter().rev() {
            p.push(part);
        }
        p
    }

    /// Folder `id`'s folders, biggest first (or by `sort`).
    pub fn sorted(&self, id: usize, sort: Sort) -> Vec<usize> {
        let mut kids = self.nodes[id].children.clone();
        let n = &self.nodes;
        match sort {
            Sort::Size => kids.sort_by_key(|&k| std::cmp::Reverse(n[k].size)),
            Sort::Name => kids.sort_by_key(|&k| n[k].name.to_lowercase()),
            Sort::Files => kids.sort_by_key(|&k| std::cmp::Reverse(n[k].files)),
            Sort::Folders => kids.sort_by_key(|&k| std::cmp::Reverse(n[k].folders)),
            Sort::Modified => kids.sort_by_key(|&k| std::cmp::Reverse(n[k].modified)),
        }
        kids
    }

    /// Changes `pending` by `by` for `id` and every folder above it.
    fn pend(&mut self, id: usize, by: i32) {
        let mut at = Some(id);
        while let Some(i) = at {
            let n = &mut self.nodes[i];
            n.pending = (n.pending as i32 + by).max(0) as u32;
            at = n.parent;
        }
    }

    /// Stops the walk (Stop): what's been found stays.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Whether the walk was stopped before it finished.
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Adds `size` bytes and `files` files under `id` (and so to each folder above it).
    fn add(&mut self, id: usize, size: u64, files: u64, folders: u64, modified: u64) {
        let mut at = Some(id);
        while let Some(i) = at {
            let n = &mut self.nodes[i];
            n.size += size;
            n.files += files;
            n.folders += folders;
            n.modified = n.modified.max(modified);
            at = n.parent;
        }
    }

    /// Reads folder `id` from the index (when it's opened).
    pub fn read_from_index(&mut self, id: usize) {
        if !self.indexed || self.nodes[id].read {
            return;
        }
        self.nodes[id].read = true;
        let path = self.path_of(id);
        let Some((_, items)) = crate::fileindex::children(&path.to_string_lossy()) else { return };
        for (name, dir, size) in items {
            if dir {
                let child = self.nodes.len();
                self.nodes.push(Node { name, parent: Some(id), size, ..Default::default() });
                self.nodes[id].children.push(child);
                self.nodes[id].folders += 1;
            } else {
                self.nodes[id].own_size += size;
                self.nodes[id].own_files += 1;
            }
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts a tree of `root`: from the index if it has it, else walked on a
/// thread of its own (the tree fills in as it goes; `wake` is called now
/// and then, and when it's done, to show it).
pub fn start(root: &Path, wake: impl Fn() + Send + Sync + 'static) -> Shared {
    let stop = Arc::new(AtomicBool::new(false));
    let name = root.to_string_lossy().into_owned();
    let indexed = crate::fileindex::children(&name).is_some();
    let total = if indexed { crate::fileindex::children(&name).map_or(0, |f| f.0) } else { 0 };
    let tree = Arc::new(Mutex::new(Tree {
        root: root.to_path_buf(),
        nodes: vec![Node { name, size: total, pending: if indexed { 0 } else { 1 }, ..Default::default() }],
        indexed,
        done: indexed,
        stop: stop.clone(),
    }));
    if indexed {
        tree.lock().unwrap_or_else(|e| e.into_inner()).read_from_index(0);
        return tree;
    }
    let shared = tree.clone();
    std::thread::spawn(move || {
        walk(&shared, &stop, &wake);
        shared.lock().unwrap_or_else(|e| e.into_inner()).done = true;
        wake();
    });
    tree
}

/// Walks the tree from its root on several threads at once, level by level:
/// every folder near the top is read early, so all their sizes climb
/// together (rather than one folder at a time, with the rest at nothing).
/// Each folder appears as it's found and its files count as it's read.
fn walk(tree: &Shared, stop: &AtomicBool, wake: &(dyn Fn() + Sync)) {
    use std::collections::VecDeque;
    use std::sync::Condvar;
    let queue: Mutex<(VecDeque<usize>, usize)> = Mutex::new((VecDeque::from([0usize]), 0));
    let more = Condvar::new();
    let last_wake = Mutex::new(std::time::Instant::now());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                // The next folder to read, or the end once none is left and none is being read.
                let id = {
                    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            more.notify_all();
                            return;
                        }
                        if let Some(id) = q.0.pop_front() {
                            q.1 += 1;
                            break id;
                        }
                        if q.1 == 0 {
                            more.notify_all();
                            return;
                        }
                        q = more.wait(q).unwrap_or_else(|e| e.into_inner());
                    }
                };
                let found = read_folder(tree, id);
                {
                    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                    q.0.extend(found);
                    q.1 -= 1;
                }
                more.notify_all();
                let mut lw = last_wake.lock().unwrap_or_else(|e| e.into_inner());
                if lw.elapsed() > std::time::Duration::from_millis(150) {
                    *lw = std::time::Instant::now();
                    wake();
                }
            });
        }
    });
}

/// Reads folder `id`: its files counted, its folders added (returned, to be read).
fn read_folder(tree: &Shared, id: usize) -> Vec<usize> {
    use std::os::windows::fs::MetadataExt;
    let path = tree.lock().unwrap_or_else(|e| e.into_inner()).path_of(id);
    let (mut size, mut files, mut newest) = (0u64, 0u64, 0u64);
    let mut dirs: Vec<(String, u64)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&path) {
        for e in entries.flatten() {
            let Ok(m) = e.metadata() else { continue };
            // Reparse points (junctions, links, OneDrive placeholders) aren't followed.
            if m.file_attributes() & 0x400 != 0 {
                continue;
            }
            let modified = m.last_write_time();
            if m.is_dir() {
                dirs.push((e.file_name().to_string_lossy().into_owned(), modified));
            } else {
                size += m.len();
                files += 1;
                newest = newest.max(modified);
            }
        }
    }
    let mut t = tree.lock().unwrap_or_else(|e| e.into_inner());
    t.nodes[id].read = true;
    t.nodes[id].own_size += size;
    t.nodes[id].own_files += files;
    let n = dirs.len() as u64;
    t.add(id, size, files, n, newest);
    let mut found = Vec::with_capacity(dirs.len());
    for (name, modified) in dirs {
        let child = t.nodes.len();
        t.nodes.push(Node { name, parent: Some(id), modified, ..Default::default() });
        t.nodes[id].children.push(child);
        t.pend(child, 1);
        found.push(child);
    }
    t.pend(id, -1);
    found
}

/// A row of the tree as shown.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Row {
    Folder(usize),
    /// Folder `id`'s own files, together.
    Files(usize),
}

/// The rows to show: the root, and inside every open folder its folders,
/// biggest first, then its files together. (row, depth)
pub fn rows(t: &Tree, open: &std::collections::HashSet<usize>, sort: Sort) -> Vec<(Row, usize)> {
    let mut out = Vec::new();
    fn add(
        t: &Tree,
        id: usize,
        depth: usize,
        open: &std::collections::HashSet<usize>,
        sort: Sort,
        out: &mut Vec<(Row, usize)>,
    ) {
        out.push((Row::Folder(id), depth));
        if !open.contains(&id) {
            return;
        }
        // Loose files go in among the folders by size, as TreeSize lists them
        // (last, sorted any other way).
        let files = t.nodes[id].own_size;
        let mut placed = t.nodes[id].own_files == 0;
        for k in t.sorted(id, sort) {
            if !placed && sort == Sort::Size && files >= t.nodes[k].size {
                out.push((Row::Files(id), depth + 1));
                placed = true;
            }
            add(t, k, depth + 1, open, sort, out);
        }
        if !placed {
            out.push((Row::Files(id), depth + 1));
        }
    }
    if !t.nodes.is_empty() {
        add(t, 0, 0, open, sort, &mut out);
    }
    out
}

/// For each row, the row of its last child shown (for the tree's lines),
/// worked out in one pass.
pub fn last_children(rows: &[(Row, usize)]) -> Vec<Option<usize>> {
    let mut last = vec![None; rows.len()];
    let mut open: Vec<(usize, usize)> = Vec::new();
    for (j, &(_, depth)) in rows.iter().enumerate() {
        while open.last().is_some_and(|&(_, d)| d >= depth) {
            open.pop();
        }
        if let Some(&(parent, d)) = open.last() {
            if d + 1 == depth {
                last[parent] = Some(j);
            }
        }
        open.push((j, depth));
    }
    last
}

/// Windows' own folder and document icons (premultiplied BGRA, `size`
/// square), for the tree's rows.
pub fn stock_icons(size: u32) -> Option<(Vec<u8>, Vec<u8>)> {
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::*;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let get = |id: SHSTOCKICONID| -> Option<Vec<u8>> {
            let mut info =
                SHSTOCKICONINFO { cbSize: std::mem::size_of::<SHSTOCKICONINFO>() as u32, ..Default::default() };
            SHGetStockIconInfo(id, SHGSI_ICON | SHGSI_LARGEICON, &mut info).ok()?;
            let bmp = wic.CreateBitmapFromHICON(info.hIcon).ok();
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(info.hIcon);
            let px = crate::icons::to_pixels(&wic, &bmp?).ok()?;
            let n = crate::icons::ICON_SIZE as usize;
            if px.len() != n * n * 4 {
                return None;
            }
            // `to_pixels` gives the icons' own size (ICON_SIZE): shrunk to `size` here.
            Some(shrink(&px, crate::icons::ICON_SIZE, size))
        };
        Some((get(SIID_FOLDER)?, get(SIID_DOCNOASSOC)?))
    }
}

/// A square BGRA picture `from` pixels across, made `to` across (box filter).
fn shrink(px: &[u8], from: u32, to: u32) -> Vec<u8> {
    let (from, to) = (from as usize, to.max(1) as usize);
    if px.len() < from * from * 4 || to >= from {
        return px.to_vec();
    }
    let mut out = vec![0u8; to * to * 4];
    for y in 0..to {
        for x in 0..to {
            let (x0, x1) = (x * from / to, ((x + 1) * from / to).max(x * from / to + 1));
            let (y0, y1) = (y * from / to, ((y + 1) * from / to).max(y * from / to + 1));
            let mut acc = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = (sy * from + sx) * 4;
                    for c in 0..4 {
                        acc[c] += px[i + c] as u32;
                    }
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as u32;
            let o = (y * to + x) * 4;
            for c in 0..4 {
                out[o + c] = (acc[c] / n) as u8;
            }
        }
    }
    out
}

/// A FILETIME as a date ("21/11/2026").
pub fn date(ft: u64) -> String {
    if ft == 0 {
        return String::new();
    }
    crate::filesio::date_text(ft).split_whitespace().next().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Tree {
        let mut t = Tree {
            root: PathBuf::from("C:\\"),
            nodes: vec![Node { name: "C:\\".into(), ..Default::default() }],
            indexed: false,
            done: false,
            stop: Arc::new(AtomicBool::new(false)),
        };
        for (name, parent) in [("Users", 0), ("Windows", 0), ("you", 1)] {
            let id = t.nodes.len();
            t.nodes.push(Node { name: name.into(), parent: Some(parent), ..Default::default() });
            t.nodes[parent].children.push(id);
        }
        t
    }

    #[test]
    fn sizes_climb_to_the_top() {
        let mut t = tree();
        t.add(3, 500, 2, 0, 7);
        t.add(2, 900, 1, 0, 3);
        assert_eq!((t.nodes[0].size, t.nodes[0].files, t.nodes[0].modified), (1400, 3, 7));
        assert_eq!(t.nodes[1].size, 500);
        assert_eq!(t.path_of(3), PathBuf::from("C:\\Users\\you"));
        assert_eq!(t.sorted(0, Sort::Size), vec![2, 1]);
        assert_eq!(t.sorted(0, Sort::Name), vec![1, 2]);
    }

    #[test]
    fn lines_go_to_the_last_child() {
        // Root, two children (the first with one of its own), then the second.
        let rows = vec![
            (Row::Folder(0), 0),
            (Row::Folder(1), 1),
            (Row::Folder(3), 2),
            (Row::Folder(2), 1),
            (Row::Files(0), 1),
        ];
        assert_eq!(last_children(&rows), vec![Some(4), Some(2), None, None, None]);
    }

    #[test]
    fn open_folders_show_what_is_in_them() {
        let mut t = tree();
        t.add(3, 500, 2, 0, 0);
        t.add(2, 900, 1, 0, 0);
        t.nodes[0].own_files = 1;
        t.nodes[0].own_size = 600;
        t.add(0, 600, 1, 0, 0);
        let mut open = std::collections::HashSet::new();
        assert_eq!(rows(&t, &open, Sort::Size).len(), 1);
        open.insert(0);
        let r: Vec<Row> = rows(&t, &open, Sort::Size).into_iter().map(|r| r.0).collect();
        // Windows (900), the loose files (600), Users (500).
        assert_eq!(r, vec![Row::Folder(0), Row::Folder(2), Row::Files(0), Row::Folder(1)]);
    }
}
