//! Previewers (section 4.4) and the windowed preview model.

use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use super::items::PickerItem;
use crate::color::{Palette, Role};
use crate::highlight::{
    highlight_lines_windowed, language_from_path, CodeHighlighter, HlState, Seg,
};
use ratatui::style::Modifier;

/// The layer-1 byte cap. A file above it is never read.
pub const PREVIEW_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Extra hard lines a content window takes past the visible rows.
pub const PREVIEW_WRAP_MARGIN: usize = 2;

/// The highlight window of one frame, in source-line units.
/// It covers `start` through `start + count` at pane width `width`.
#[derive(Debug, Clone, Copy)]
pub struct PreviewWindow {
    pub start: usize,
    pub count: usize,
    pub width: usize,
}

/// A settled load that shows a status line instead of content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewStatus {
    /// Above the cap. `size` is the byte size shown in the pane.
    TooLarge { size: u64 },
    /// The read failed after the guard passed.
    Unreadable,
}

/// The layer-1 guard decision for one path. One `stat`, no read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewPlan {
    /// Within the cap: read it in the background.
    Read { mtime: u64 },
    /// Above the cap: never read. `size` is the byte size.
    TooLarge { size: u64 },
    /// The stat failed: the file is unreadable.
    Unreadable,
}

/// Run the layer-1 guard on `path`: one `stat`, no read.
pub fn plan_read(path: &str) -> PreviewPlan {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return PreviewPlan::Unreadable,
    };
    let size = meta.len();
    if size > PREVIEW_MAX_BYTES {
        return PreviewPlan::TooLarge { size };
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    PreviewPlan::Read { mtime }
}

/// One background read request. `key` is the item payload the read
/// was dispatched for; `path` is what the worker reads.
#[derive(Debug, Clone)]
pub struct PreviewRequest {
    pub seq: u64,
    pub key: String,
    pub path: String,
    pub mtime: u64,
}

/// One settled background read. `status` is set when the read
/// failed or was refused by the guard; then `lines` is empty.
#[derive(Debug, Clone)]
pub struct PreviewResult {
    pub seq: u64,
    pub key: String,
    pub mtime: u64,
    pub lines: Vec<String>,
    pub status: Option<PreviewStatus>,
}

/// Read one file into hard lines. A read error settles as
/// `Unreadable` with empty lines.
pub fn read_one(req: &PreviewRequest) -> PreviewResult {
    let status = match std::fs::read_to_string(&req.path) {
        Ok(text) => {
            let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
            PreviewResult {
                seq: req.seq,
                key: req.key.clone(),
                mtime: req.mtime,
                lines,
                status: None,
            }
        }
        Err(_) => PreviewResult {
            seq: req.seq,
            key: req.key.clone(),
            mtime: req.mtime,
            lines: Vec::new(),
            status: Some(PreviewStatus::Unreadable),
        },
    };
    status
}

/// The background preview reader (plan layer 2). A dedicated thread
/// like [`crate::transcript_worker::TranscriptWorker`]. The render
/// thread never blocks on a file read.
pub struct PreviewLoader {
    tx: mpsc::Sender<PreviewRequest>,
    result_rx: mpsc::Receiver<PreviewResult>,
    _thread: thread::JoinHandle<()>,
}

impl PreviewLoader {
    /// Spawn the reader thread.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<PreviewRequest>();
        let (res_tx, result_rx) = mpsc::channel::<PreviewResult>();
        let _thread = thread::Builder::new()
            .name("tui-preview-read".into())
            .spawn(move || reader_loop(rx, res_tx))
            .expect("the preview reader thread starts");
        Self {
            tx,
            result_rx,
            _thread,
        }
    }

    /// Queue one read. `Err` when the worker channel is closed (the
    /// worker died); the caller falls back to a main-thread read.
    pub fn send(&self, req: PreviewRequest) -> Result<(), mpsc::SendError<PreviewRequest>> {
        self.tx.send(req)
    }

    /// The next finished read, without blocking.
    pub fn try_recv_result(&self) -> Result<PreviewResult, mpsc::TryRecvError> {
        self.result_rx.try_recv()
    }

    /// The next finished read, blocking. Test helper.
    #[allow(dead_code)] // consumed only by the cancel test below.
    pub fn recv_result(&self) -> Result<PreviewResult, mpsc::RecvError> {
        self.result_rx.recv()
    }
}

/// The reader loop. Reads settle in request order. A moved cursor
/// does not interrupt the in-flight read. The layer-1 cap bounds it,
/// and the main thread drops the stale result on settle.
fn reader_loop(rx: mpsc::Receiver<PreviewRequest>, out: mpsc::Sender<PreviewResult>) {
    for req in rx {
        let result = read_one(&req);
        if out.send(result).is_err() {
            return;
        }
    }
}

/// Fold one finished read into the pending slot (plan layer 2).
/// Returns `true` when the result attached. A result whose key the
/// cursor left is the cancelled load. It is dropped, slot untouched.
pub fn settle_load(slot: &mut PreviewLoad, res: &PreviewResult, current_key: &str) -> bool {
    if res.key != current_key {
        return false;
    }
    *slot = PreviewLoad::Settled {
        key: res.key.clone(),
        lines: res.lines.clone(),
        status: res.status,
        mtime: res.mtime,
    };
    true
}

/// The picker state's preview-load slot (plan layer 2).
/// Exactly one load is tracked, keyed by the item payload.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum PreviewLoad {
    /// No load dispatched yet.
    #[default]
    None,
    /// A read is in flight for `key`, dispatched as sequence `seq`.
    Pending {
        key: String,
        #[allow(dead_code)] // diagnostics only; settle compares keys
        seq: u64,
    },
    /// The load for `key` settled. `lines` is empty when `status`
    /// is set. `mtime` is the file mtime in seconds at read time.
    Settled {
        key: String,
        lines: Vec<String>,
        status: Option<PreviewStatus>,
        mtime: u64,
    },
}

impl PreviewLoad {
    /// The item key the load belongs to, if any.
    pub fn key(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Pending { key, .. } => Some(key),
            Self::Settled { key, .. } => Some(key),
        }
    }

    /// The settled line count, when a real load is settled.
    pub fn total_lines(&self) -> Option<usize> {
        match self {
            Self::Settled {
                lines,
                status: None,
                ..
            } => Some(lines.len()),
            _ => None,
        }
    }

    /// The settled lines, when a real load is settled.
    #[allow(dead_code)] // consumed by the tests; the render path
                        // matches the load variants directly.
    pub fn lines(&self) -> Option<&[String]> {
        match self {
            Self::Settled {
                lines,
                status: None,
                ..
            } => Some(lines),
            _ => None,
        }
    }
}

// ── the highlighted-window LRU (plan layer 3) ─────────────────────

/// The window cache key. It covers the file identity (path and
/// mtime), the line range, the pane width, the palette, and the
/// highlight language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowKey {
    pub path: String,
    pub mtime: u64,
    pub start: usize,
    pub width: usize,
    pub palette: Palette,
    pub lang: Option<&'static str>,
}

/// One cached highlighted window. `state_after` is the highlighter
/// state at the window end, the next window's checkpoint.
pub struct WindowEntry {
    pub key: WindowKey,
    pub lines: Vec<Vec<Seg>>,
    pub state_after: HlState,
}

/// The highlighted-window LRU. Entries sit oldest-to-newest. A hit
/// promotes its entry. An over-capacity insert evicts the oldest.
/// It mirrors the [`crate::transcript_worker::BuildMemo`] shape.
pub struct WindowCache {
    cap: usize,
    entries: Vec<WindowEntry>,
    /// Window-cache hits since creation.
    pub hits: u64,
    /// Window-cache misses since creation.
    pub misses: u64,
}

impl WindowCache {
    /// The default cache capacity. Eight windows cover a back-and-
    /// forth scroll plus a few width toggles.
    pub const DEFAULT_CAP: usize = 8;

    /// A cache at the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_CAP)
    }

    /// A cache at `cap` slots, minimum 1.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            entries: Vec::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// The cached window on a full-key match. A hit promotes its
    /// entry to newest and counts the hit.
    pub fn get(&mut self, key: &WindowKey) -> Option<&WindowEntry> {
        if let Some(pos) = self.entries.iter().rposition(|e| e.key == *key) {
            self.hits += 1;
            let entry = self.entries.remove(pos);
            self.entries.push(entry);
            return self.entries.last();
        }
        self.misses += 1;
        None
    }

    /// Store `entry`. A same-key entry is replaced in place and
    /// promoted. An over-capacity insert evicts the oldest entry.
    pub fn insert(&mut self, entry: WindowEntry) {
        if let Some(pos) = self.entries.iter().position(|e| e.key == entry.key) {
            self.entries[pos] = entry;
            return;
        }
        if self.entries.len() >= self.cap {
            self.entries.remove(0);
        }
        self.entries.push(entry);
    }

    /// The checkpoint a window at `key.start` resumes from.
    /// The newest usable end index is the largest one at or
    /// before `key.start`. Only same-file, same-width, same-palette
    /// entries count.
    pub fn checkpoint(&self, key: &WindowKey) -> Option<(usize, HlState)> {
        self.entries
            .iter()
            .filter(|e| {
                e.key.path == key.path
                    && e.key.mtime == key.mtime
                    && e.key.width == key.width
                    && e.key.palette == key.palette
                    && e.key.lang == key.lang
            })
            .map(|e| (e.key.start + e.lines.len(), e.state_after))
            .filter(|(end, _)| *end <= key.start)
            .max_by_key(|(end, _)| *end)
    }

    /// The number of stored entries.
    #[allow(dead_code)] // consumed by the cache tests below.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache stores no entries.
    #[allow(dead_code)] // consumed by the cache tests below.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for WindowCache {
    fn default() -> Self {
        Self::new()
    }
}

/// A previewer that reads and shows the text content of a file,
/// windowed. The item `payload` is the absolute path.
pub struct FilePreviewer {
    cache: Arc<Mutex<WindowCache>>,
}

impl FilePreviewer {
    /// A file previewer sharing the given window LRU. The cache is
    /// shared because the render path rebuilds the previewer every
    /// frame. The lock is main-thread only.
    pub fn new(cache: Arc<Mutex<WindowCache>>) -> Self {
        Self { cache }
    }

    /// The pane's placeholder line for a load settled with a status.
    fn status_line(
        &self,
        item: &PickerItem,
        status: &PreviewStatus,
        palette: &Palette,
    ) -> Vec<Vec<Seg>> {
        let text = match status {
            PreviewStatus::TooLarge { size } => {
                format!("(file too large to preview: {})", human_size(*size))
            }
            PreviewStatus::Unreadable => format!("(cannot read {})", item.label),
        };
        vec![vec![(palette.style(Role::Hint, Modifier::empty()), text)]]
    }

    /// The layer-3 highlight of one window, through the LRU. A hit
    /// returns the stored lines. A miss highlights the window from
    /// the newest checkpoint, then stores the result.
    fn windowed_content(
        &self,
        item: &PickerItem,
        lines: &[String],
        mtime: u64,
        window: &PreviewWindow,
        palette: &Palette,
        cache: &mut MutexGuard<'_, WindowCache>,
    ) -> Vec<Vec<Seg>> {
        let lang = language_from_path(&item.payload);
        let key = WindowKey {
            path: item.payload.clone(),
            mtime,
            start: window.start,
            width: window.width,
            palette: palette.clone(),
            lang,
        };
        if let Some(entry) = cache.get(&key) {
            return entry.lines.clone();
        }
        // Resume from the newest checkpoint at or before the window
        // start. No checkpoint means a fresh rescan from line 0.
        let (ckpt, mut state) = cache.checkpoint(&key).unwrap_or((0, HlState::default()));
        if ckpt < window.start {
            // Rescan the gap to carry the state to the window start.
            // Only the state matters; the rescan output is dropped.
            let end = window.start.min(lines.len());
            let mut hl = CodeHighlighter::new().with_state(state);
            for line in lines[ckpt..end].iter() {
                let _ = hl.line(line, lang, palette);
            }
            state = hl.state();
        }
        let out =
            highlight_lines_windowed(lines, window.start, window.count, lang, palette, &mut state);
        cache.insert(WindowEntry {
            key,
            lines: out.clone(),
            state_after: state,
        });
        out
    }
}

/// Lock the window cache. The lock lives on the main thread, so a
/// poisoned guard is a hard bug. Recover, not stall.
fn lock(cache: &Arc<Mutex<WindowCache>>) -> MutexGuard<'_, WindowCache> {
    cache.lock().unwrap_or_else(|e| e.into_inner())
}

/// The previewer seam. A picker body takes one previewer and renders
/// the preview pane for the selected item.
pub trait Previewer {
    /// Whether this previewer renders anything. The null previewer
    /// returns `false` and the pane is dropped.
    fn enabled(&self) -> bool;

    /// The header line for the preview pane (path, size, and the
    /// line count once settled). `None` when the item cannot be
    /// previewed.
    fn header(&self, item: &PickerItem, load: &PreviewLoad) -> Option<String>;

    /// The highlighted lines for the visible `window` of the item,
    /// as styled segments per hard line. Highlighted runs carry the
    /// palette syntax roles; plain runs keep the default style,
    /// which the pane paints with the plain-text tone.
    fn content(
        &self,
        item: &PickerItem,
        load: &PreviewLoad,
        window: &PreviewWindow,
        palette: &Palette,
    ) -> Vec<Vec<Seg>>;
}

impl Previewer for FilePreviewer {
    fn enabled(&self) -> bool {
        true
    }

    /// The header line: label, byte size, and the line count once the
    /// load settles. No file read: the size is a `stat`, the count
    /// comes from the settled lines (plan header fix).
    fn header(&self, item: &PickerItem, load: &PreviewLoad) -> Option<String> {
        let meta = match std::fs::metadata(&item.payload) {
            Ok(m) => m,
            Err(_) => return Some(format!("{} (unreadable)", item.label)),
        };
        let size = meta.len();
        let tail = if size > PREVIEW_MAX_BYTES {
            "  (too large to preview)".to_string()
        } else {
            match load {
                PreviewLoad::Settled {
                    lines,
                    status: None,
                    ..
                } => format!("  {} lines", lines.len()),
                PreviewLoad::Settled {
                    status: Some(PreviewStatus::Unreadable),
                    ..
                } => "  (cannot read)".to_string(),
                _ => "  …".to_string(),
            }
        };
        Some(format!("{}  {}{}", item.label, human_size(size), tail))
    }

    /// The highlighted visible window for the item. The `stat` guard
    /// short-circuits over-cap files (layer 1). An unsettled load
    /// renders the loading placeholder (layer 2). Settled lines
    /// highlight for `window` only, through the LRU (layer 3).
    fn content(
        &self,
        item: &PickerItem,
        load: &PreviewLoad,
        window: &PreviewWindow,
        palette: &Palette,
    ) -> Vec<Vec<Seg>> {
        let path = &item.payload;
        // Layer 1: the O(1) guard, before any read.
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > PREVIEW_MAX_BYTES {
                return vec![vec![(
                    palette.style(Role::Hint, Modifier::empty()),
                    format!("(file too large to preview: {})", human_size(meta.len())),
                )]];
            }
        }
        // Layer 2: the read must have settled for this item.
        match load {
            PreviewLoad::Settled {
                key,
                lines,
                status: None,
                mtime,
            } if key == path => {
                let mut cache = lock(&self.cache);
                self.windowed_content(item, lines, *mtime, window, palette, &mut cache)
            }
            PreviewLoad::Settled {
                status: Some(status),
                ..
            } => self.status_line(item, status, palette),
            _ => vec![vec![(
                palette.style(Role::Hint, Modifier::empty()),
                "(loading…)".to_string(),
            )]],
        }
    }
}

/// A previewer that shows nothing. The preview pane is dropped.
/// This is the off seam for later previews that need no pane.
#[allow(dead_code)]
pub struct NullPreviewer;

impl Previewer for NullPreviewer {
    fn enabled(&self) -> bool {
        false
    }

    fn header(&self, _item: &PickerItem, _load: &PreviewLoad) -> Option<String> {
        None
    }

    fn content(
        &self,
        _item: &PickerItem,
        _load: &PreviewLoad,
        _window: &PreviewWindow,
        _palette: &Palette,
    ) -> Vec<Vec<Seg>> {
        Vec::new()
    }
}

/// Format a byte count as a human-readable string.
pub fn human_size(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if n >= GB {
        format!("{:.1}G", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1}M", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1}K", n as f64 / KB as f64)
    } else {
        format!("{n}B")
    }
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    static DIR_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn palette() -> Palette {
        Palette::builtin(crate::color::Level::Rgb)
    }

    fn item(path: &std::path::Path, label: &str) -> PickerItem {
        let p = path.to_string_lossy().to_string();
        PickerItem {
            label: label.to_string(),
            value: label.to_string(),
            payload: p,
        }
    }

    fn window(start: usize, count: usize, width: usize) -> PreviewWindow {
        PreviewWindow {
            start,
            count,
            width,
        }
    }

    fn settled(key: &str, lines: Vec<String>) -> PreviewLoad {
        PreviewLoad::Settled {
            key: key.to_string(),
            lines,
            status: None,
            mtime: 0,
        }
    }

    fn text_of(out: &[Vec<Seg>]) -> Vec<String> {
        out.iter()
            .map(|segs| segs.iter().map(|s| s.1.as_str()).collect::<String>())
            .collect()
    }

    /// A unique temp dir for one test.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let n = DIR_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p =
            std::env::temp_dir().join(format!("rushi-preview-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::create_dir_all(&p);
        p
    }

    /// Plan 1: a file above `PREVIEW_MAX_BYTES` skips the read.
    /// The content shows the placeholder, the header the byte size.
    #[test]
    fn size_guard_skips_read_on_huge_file() {
        let dir = temp_dir("guard");
        let big = dir.join("big.bin");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(PREVIEW_MAX_BYTES + 1).unwrap();
        drop(f);
        // The guard decides from the stat alone. No read is owed.
        let plan = plan_read(&big.to_string_lossy());
        assert!(
            matches!(plan, PreviewPlan::TooLarge { size } if size == PREVIEW_MAX_BYTES + 1),
            "the guard marks the over-cap file too large: {plan:?}"
        );
        let p = FilePreviewer::new(Arc::new(Mutex::new(WindowCache::new())));
        let it = item(&big, "big.bin");
        // Even with no load at all the guard short-circuits.
        let out = p.content(&it, &PreviewLoad::None, &window(0, 10, 60), &palette());
        assert_eq!(out.len(), 1, "a single placeholder line");
        let text: String = out[0].iter().map(|s| s.1.as_str()).collect();
        assert!(text.contains("too large"), "placeholder: {text:?}");
        // The header shows the byte size, no line count.
        let h = p.header(&it, &PreviewLoad::None).unwrap();
        assert!(!h.contains("lines"), "no count pre-settle: {h:?}");
        assert!(h.contains("10.0M"), "size shown: {h:?}");
    }

    /// The cap is inclusive. A file at exactly the cap still reads.
    #[test]
    fn size_guard_cap_boundary() {
        let dir = temp_dir("guard-b");
        let at_cap = dir.join("at_cap.bin");
        let f = std::fs::File::create(&at_cap).unwrap();
        f.set_len(PREVIEW_MAX_BYTES).unwrap();
        drop(f);
        assert!(
            matches!(
                plan_read(&at_cap.to_string_lossy()),
                PreviewPlan::Read { .. }
            ),
            "exactly at the cap still reads"
        );
    }

    /// Plan 2: a cursor move before the read settles drops the
    /// in-flight result. A fresh dispatch settles instead. Driven on
    /// a named pipe so the first read blocks deterministically.
    #[cfg(unix)]
    #[test]
    fn cursor_move_cancels_in_flight_load() {
        use std::io::Write;

        let dir = temp_dir("cancel");
        let fifo = dir.join("a.pipe");
        let b = dir.join("b.txt");
        std::fs::write(&b, "b-line-1\nb-line-2\n").unwrap();
        let c_fifo = std::ffi::CString::new(fifo.to_string_lossy().into_owned()).unwrap();
        let c_b = std::ffi::CString::new(b.to_string_lossy().into_owned()).unwrap();
        unsafe {
            assert_eq!(libc::mkfifo(c_fifo.as_ptr(), 0o600), 0, "mkfifo");
        }
        // The writer opens the pipe to release the blocked reader.
        let a_key = c_fifo.to_string_lossy().into_owned();
        let b_key = c_b.to_string_lossy().into_owned();

        let loader = PreviewLoader::spawn();

        // Dispatch the in-flight read for item a. It blocks in the
        // worker until a writer opens the pipe.
        let _ = loader.send(PreviewRequest {
            seq: 1,
            key: a_key.clone(),
            path: a_key.clone(),
            mtime: 0,
        });
        // The cursor moves to item b: cancel and dispatch the fresh
        // read. The slot now holds the fresh dispatch; the first
        // read's result will arrive stale.
        let _ = loader.send(PreviewRequest {
            seq: 2,
            key: b_key.clone(),
            path: b_key.clone(),
            mtime: 0,
        });
        let mut slot = PreviewLoad::Pending {
            key: b_key.clone(),
            seq: 2,
        };
        // Unblock the first read. Its result is now stale: the
        // cursor is on b.
        {
            let mut w = std::fs::OpenOptions::new().write(true).open(&fifo).unwrap();
            w.write_all(b"a-content").unwrap();
        }
        // The stale result arrives first and must be dropped.
        let ra = loader.recv_result().unwrap();
        assert_eq!(ra.key, a_key);
        assert!(
            !settle_load(&mut slot, &ra, &b_key),
            "the in-flight result for the old item is dropped"
        );
        // The fresh read settles.
        let rb = loader.recv_result().unwrap();
        assert_eq!(rb.key, b_key);
        assert!(
            settle_load(&mut slot, &rb, &b_key),
            "the fresh read settles"
        );
        assert_eq!(
            slot,
            PreviewLoad::Settled {
                key: b_key,
                lines: vec!["b-line-1".to_string(), "b-line-2".to_string()],
                status: None,
                mtime: 0,
            }
        );
        let _ = std::fs::remove_file(&fifo);
    }

    /// Plan 3: `content` highlights only the visible window, not
    /// the whole file.
    #[test]
    fn windowed_highlight_only_visible_lines() {
        let dir = temp_dir("window");
        let path = dir.join("code.rs");
        let lines: Vec<String> = (0..200).map(|i| format!("let v_{i} = {i};")).collect();
        std::fs::write(&path, lines.join("\n")).unwrap();
        let it = item(&path, "code.rs");
        let load = settled(&path.to_string_lossy(), lines.clone());
        let p = FilePreviewer::new(Arc::new(Mutex::new(WindowCache::new())));

        let out = p.content(&it, &load, &window(50, 10, 80), &palette());
        assert_eq!(out.len(), 10, "only the window's lines are returned");
        assert_eq!(text_of(&out), lines[50..60], "the window's source lines");
        // The cache stores one entry of ten lines, not 200.
        let cache = p.cache.lock().unwrap();
        assert_eq!(cache.len(), 1, "one window entry, not the whole file");
        assert_eq!(cache.entries[0].lines.len(), 10);
    }

    /// Plan 4: scrolling away and back to a recent window hits the
    /// LRU. No re-highlight.
    #[test]
    fn window_cache_hit_on_rescroll() {
        let dir = temp_dir("rescroll");
        let path = dir.join("code.rs");
        let lines: Vec<String> = (0..120).map(|i| format!("let v_{i} = {i};")).collect();
        std::fs::write(&path, lines.join("\n")).unwrap();
        let it = item(&path, "code.rs");
        let load = settled(&path.to_string_lossy(), lines.clone());
        let cache = Arc::new(Mutex::new(WindowCache::new()));
        let p = FilePreviewer::new(Arc::clone(&cache));

        let _ = p.content(&it, &load, &window(0, 10, 80), &palette());
        let _ = p.content(&it, &load, &window(30, 10, 80), &palette());
        let (hits, misses) = {
            let c = cache.lock().unwrap();
            (c.hits, c.misses)
        };
        assert_eq!(hits, 0);
        assert_eq!(misses, 2);
        let _ = p.content(&it, &load, &window(0, 10, 80), &palette());
        let c = cache.lock().unwrap();
        assert_eq!(c.hits, hits + 1, "the re-scroll is a cache hit");
        assert_eq!(c.misses, misses, "a hit does not miss");
    }

    /// Plan 5: a pane resize invalidates the window cache. The
    /// same range re-highlights for the new width.
    #[test]
    fn width_change_invalidates_window_cache() {
        let dir = temp_dir("width");
        let path = dir.join("code.rs");
        let lines: Vec<String> = (0..20).map(|i| format!("let v_{i} = {i};")).collect();
        std::fs::write(&path, lines.join("\n")).unwrap();
        let it = item(&path, "code.rs");
        let load = settled(&path.to_string_lossy(), lines.clone());
        let cache = Arc::new(Mutex::new(WindowCache::new()));
        let p = FilePreviewer::new(Arc::clone(&cache));

        let _ = p.content(&it, &load, &window(0, 5, 80), &palette());
        let misses_before = cache.lock().unwrap().misses;
        let _ = p.content(&it, &load, &window(0, 5, 120), &palette());
        // The guard dies before the next `content` call: it locks the
        // same mutex, so a held guard would self-deadlock.
        let (misses, len) = {
            let c = cache.lock().unwrap();
            (c.misses, c.len())
        };
        assert_eq!(misses, misses_before + 1, "the new width is a miss");
        assert_eq!(len, 2, "both widths are stored");
        let _ = p.content(&it, &load, &window(0, 5, 120), &palette());
        assert_eq!(cache.lock().unwrap().len(), 2, "the hit reuses the entry");
    }

    /// Plan 6: with no line cap, `preview_scroll` reaches the last
    /// line of a >50-line file.
    #[test]
    fn scroll_range_covers_full_file() {
        let dir = temp_dir("scroll");
        let path = dir.join("long.rs");
        let n = 60usize;
        let lines: Vec<String> = (0..n).map(|i| format!("line {i}")).collect();
        std::fs::write(&path, lines.join("\n")).unwrap();
        let it = item(&path, "long.rs");
        let load = settled(&path.to_string_lossy(), lines.clone());
        let p = FilePreviewer::new(Arc::new(Mutex::new(WindowCache::new())));

        // The render clamps the scroll to the last line. The window
        // starting there must include the file's last line.
        let start = 55usize.min(n - 1);
        let out = p.content(&it, &load, &window(start, 10, 80), &palette());
        assert_eq!(
            text_of(&out).last(),
            Some(&lines[n - 1]),
            "last line reachable"
        );
        // A window past the end yields nothing.
        let past = p.content(&it, &load, &window(n, 10, 80), &palette());
        assert!(past.is_empty());
    }

    /// Plan 7: the header never re-reads the file for a line count.
    /// Pre-settle it shows the byte size. Post-settle the count.
    #[test]
    fn header_does_not_reread_file() {
        let dir = temp_dir("header");
        let path = dir.join("hdr.txt");
        let body = "one\ntwo\nthree\n";
        std::fs::write(&path, body).unwrap();
        let it = item(&path, "hdr.txt");
        let p = FilePreviewer::new(Arc::new(Mutex::new(WindowCache::new())));

        let pre = p.header(&it, &PreviewLoad::None).unwrap();
        assert!(!pre.contains("lines"), "no count pre-settle: {pre:?}");
        // The byte size is the body length: 4 + 4 + 6 = 14.
        assert!(pre.contains("14B"), "the byte size is shown: {pre:?}");

        let load = settled(
            &path.to_string_lossy(),
            body.lines().map(|l| l.to_string()).collect(),
        );
        let post = p.header(&it, &load).unwrap();
        assert!(post.contains("3 lines"), "the settled count: {post:?}");
    }

    /// Plan 8 (unit part): a 50 MiB file is guarded. No read is
    /// owed, the pane shows the placeholder immediately.
    #[test]
    fn perf_gate_huge_file_guard() {
        let dir = temp_dir("perf");
        let big = dir.join("huge.log");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(50 * 1024 * 1024).unwrap();
        drop(f);
        let plan = plan_read(&big.to_string_lossy());
        assert!(
            matches!(plan, PreviewPlan::TooLarge { .. }),
            "guarded: {plan:?}"
        );
        let it = item(&big, "huge.log");
        let p = FilePreviewer::new(Arc::new(Mutex::new(WindowCache::new())));
        let out = p.content(&it, &PreviewLoad::None, &window(0, 10, 60), &palette());
        let text: String = out[0].iter().map(|s| s.1.as_str()).collect();
        assert!(
            text.contains("50.0M"),
            "placeholder sizes the file: {text:?}"
        );
        let h = p.header(&it, &PreviewLoad::None).unwrap();
        assert!(h.contains("50.0M"), "header sizes the file: {h:?}");
    }

    /// A load settled as `Unreadable` renders the cannot-read note.
    #[test]
    fn unreadable_status_renders_note() {
        let dir = temp_dir("unreadable");
        let path = dir.join("gone.txt");
        std::fs::write(&path, "x\n").unwrap();
        let key = path.to_string_lossy().to_string();
        let it = item(&path, "gone.txt");
        std::fs::remove_file(&path).unwrap();
        let load = PreviewLoad::Settled {
            key,
            lines: Vec::new(),
            status: Some(PreviewStatus::Unreadable),
            mtime: 0,
        };
        let p = FilePreviewer::new(Arc::new(Mutex::new(WindowCache::new())));
        let h = p.header(&it, &load).unwrap();
        assert!(h.contains("unreadable"), "header names the state: {h:?}");
        let out = p.content(&it, &load, &window(0, 10, 60), &palette());
        let text: String = out[0].iter().map(|s| s.1.as_str()).collect();
        assert!(
            text.contains("cannot read"),
            "content shows the note: {text:?}"
        );
    }

    /// A cache hit returns the stored lines verbatim.
    #[test]
    fn window_cache_hit_returns_stored_lines() {
        let cache = Arc::new(Mutex::new(WindowCache::new()));
        let p = FilePreviewer::new(Arc::clone(&cache));
        let dir = temp_dir("hit");
        let path = dir.join("c.rs");
        let lines = vec!["let a = 1;".to_string(), "let b = 2;".to_string()];
        std::fs::write(&path, lines.join("\n")).unwrap();
        let it = item(&path, "c.rs");
        let load = settled(&path.to_string_lossy(), lines.clone());
        let first = p.content(&it, &load, &window(0, 2, 80), &palette());
        let second = p.content(&it, &load, &window(0, 2, 80), &palette());
        assert_eq!(first, second, "a hit reuses the stored lines");
    }

    /// The LRU evicts the oldest entry past its capacity.
    #[test]
    fn window_cache_evicts_oldest() {
        let mut cache = WindowCache::with_capacity(2);
        for i in 0..3 {
            cache.insert(WindowEntry {
                key: WindowKey {
                    path: format!("p{i}"),
                    mtime: 0,
                    start: 0,
                    width: 80,
                    palette: palette(),
                    lang: None,
                },
                lines: vec![],
                state_after: HlState::default(),
            });
        }
        assert_eq!(cache.len(), 2);
        let mk_key = |path: &str| WindowKey {
            path: path.to_string(),
            mtime: 0,
            start: 0,
            width: 80,
            palette: palette(),
            lang: None,
        };
        assert!(cache.get(&mk_key("p0")).is_none(), "oldest evicted");
        assert!(cache.get(&mk_key("p1")).is_some());
        assert!(cache.get(&mk_key("p2")).is_some());
    }

    /// A checkpoint only counts at or before the window start.
    #[test]
    fn window_cache_checkpoint_respects_start() {
        let mut cache = WindowCache::new();
        cache.insert(WindowEntry {
            key: WindowKey {
                path: "f".into(),
                mtime: 7,
                start: 0,
                width: 80,
                palette: palette(),
                lang: None,
            },
            lines: vec![vec![], vec![], vec![], vec![], vec![]],
            state_after: HlState {
                in_block_comment: true,
                ..Default::default()
            },
        });
        // A window at line 3 cannot use the checkpoint at line 5.
        let k3 = WindowKey {
            path: "f".into(),
            mtime: 7,
            start: 3,
            width: 80,
            palette: palette(),
            lang: None,
        };
        assert!(cache.checkpoint(&k3).is_none(), "5 > 3 is not usable");
        // A window at line 5 resumes from it.
        let k5 = WindowKey { start: 5, ..k3 };
        assert_eq!(
            cache.checkpoint(&k5),
            Some((
                5,
                HlState {
                    in_block_comment: true,
                    ..Default::default()
                }
            )),
        );
    }

    /// Plan 7 (hard part): an unreadable file shows no line count.
    /// Skipped when root, who can read anything.
    #[test]
    fn header_skips_line_count_when_unreadable() {
        if std::env::var("USER").map(|u| u == "root").unwrap_or(false) {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("noread");
        let path = dir.join("noread.txt");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&path, perms).unwrap();
        let it = item(&path, "noread.txt");
        let p = FilePreviewer::new(Arc::new(Mutex::new(WindowCache::new())));
        let h = p.header(&it, &PreviewLoad::None).unwrap();
        assert!(
            !h.contains("lines"),
            "no line count when the file is unreadable: {h:?}"
        );
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
    }
}
