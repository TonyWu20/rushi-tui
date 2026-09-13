use std::sync::mpsc;
use std::thread;

use crate::render::{build_transcript_input, TranscriptBuild, TranscriptBuildInput};

/// The six fields of the transcript cache key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildKey {
    pub events_version: u64,
    pub width: usize,
    pub ext_ver: u64,
    pub palette_level: crate::color::Level,
    pub palette: crate::color::Palette,
    pub frac_epoch: u64,
}

#[derive(Debug)]
pub struct BuildRequest {
    pub seq: u64,
    pub key: BuildKey,
    pub input: TranscriptBuildInput,
}

#[derive(Debug)]
pub struct BuildResult {
    /// The request sequence number.
    /// It traces a result back to its request.
    /// The main loop swaps on key match, not sequence.
    #[allow(dead_code)]
    pub seq: u64,
    pub key: BuildKey,
    pub build: TranscriptBuild,
}

/// Trailing debounce window for width-triggered transcript rebuilds
/// (docs/tui-perf-background-build-plan.md, stage 4). Each new width
/// event resets the deadline. Only the settled width after a burst of
/// resizes or browse toggles builds. Event-commit misses bypass it.
pub const TRANSCRIPT_WIDTH_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(75);

/// Keep the newest queued build request, drop the older ones.
pub fn coalesce(burst: Vec<BuildRequest>) -> Option<BuildRequest> {
    burst.into_iter().max_by_key(|r| r.seq)
}

/// The width-keyed build memo
/// (docs/tui-perf-background-build-plan.md, stage 3).
///
/// The LRU key is `(events_version, width)`.
/// A repeated toggle between two widths returns the cached build.
/// Each entry keeps the full key that built it.
/// Only a full-key match is a hit.
/// A palette or fraction change at the same width is a miss.
/// The miss rebuilds and replaces the entry.
/// This keeps a palette toggle from settling on stale colors.
#[derive(Debug)]
pub struct BuildMemo {
    cap: usize,
    /// Entries, oldest to newest.
    /// Each holds the memo key, the full key, and the build.
    entries: Vec<(u64, usize, BuildKey, TranscriptBuild)>,
}

impl BuildMemo {
    /// The default memo capacity.
    /// Four slots cover two toggles plus a transient build.
    pub const DEFAULT_CAP: usize = 4;

    /// A memo at the default capacity 4.
    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_CAP)
    }

    /// A memo at `cap` slots, minimum 1.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            entries: Vec::new(),
        }
    }

    /// The width-keyed identity of a build key.
    pub fn memo_key(key: &BuildKey) -> (u64, usize) {
        (key.events_version, key.width)
    }

    /// The cached build on a full-key match.
    /// A same-width entry with a stale full key is a miss.
    /// A hit promotes its entry to newest.
    pub fn get(&mut self, key: &BuildKey) -> Option<&TranscriptBuild> {
        let (ev, width) = Self::memo_key(key);
        let pos = self
            .entries
            .iter()
            .rposition(|e| e.0 == ev && e.1 == width)?;
        if self.entries[pos].2 != *key {
            return None;
        }
        let entry = self.entries.remove(pos);
        self.entries.push(entry);
        self.entries.last().map(|e| &e.3)
    }

    /// Store `build` under `key`.
    /// A same-width entry is replaced in place.
    /// An over-capacity insert evicts the oldest entry.
    pub fn insert(&mut self, key: &BuildKey, build: TranscriptBuild) {
        let (ev, width) = Self::memo_key(key);
        if let Some(pos) = self.entries.iter().position(|e| e.0 == ev && e.1 == width) {
            let mut entry = self.entries.remove(pos);
            entry.2 = key.clone();
            entry.3 = build;
            self.entries.push(entry);
            return;
        }
        if self.entries.len() >= self.cap {
            self.entries.remove(0);
        }
        self.entries.push((ev, width, key.clone(), build));
    }
}

/// Consult the width-keyed memo before building.
/// A hit returns the cached build.
/// A miss runs `build` and stores the result.
pub fn build_with_memo<F: FnOnce(&TranscriptBuildInput) -> TranscriptBuild>(
    memo: &mut BuildMemo,
    req: &BuildRequest,
    build: F,
) -> TranscriptBuild {
    if let Some(cached) = memo.get(&req.key) {
        return cached.clone();
    }
    let built = build(&req.input);
    memo.insert(&req.key, built.clone());
    built
}

/// The background build worker, owned by the main thread.
pub struct TranscriptWorker {
    tx: mpsc::Sender<BuildRequest>,
    result_rx: mpsc::Receiver<BuildResult>,
    _thread: thread::JoinHandle<()>,
}

impl TranscriptWorker {
    /// Spawn the worker with an empty queue and memo.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<BuildRequest>();
        let (res_tx, res_rx) = mpsc::channel::<BuildResult>();
        let _thread = thread::Builder::new()
            .name("tui-transcript-build".into())
            .spawn(move || worker_loop(rx, res_tx, build_transcript_input))
            .expect("the transcript build thread starts");
        Self {
            tx,
            result_rx: res_rx,
            _thread,
        }
    }

    /// Queue one build request for the worker.
    #[allow(clippy::result_large_err)]
    pub fn send(&self, req: BuildRequest) -> Result<(), mpsc::SendError<BuildRequest>> {
        self.tx.send(req)
    }

    /// The next finished result, without blocking.
    pub fn try_recv_result(&self) -> Result<BuildResult, mpsc::TryRecvError> {
        self.result_rx.try_recv()
    }
}

/// The worker loop.
/// It coalesces each burst to its newest request.
/// It consults the memo, builds on a miss, and publishes.
/// `build` is the full-build function.
/// Production passes `build_transcript_input`.
/// A test injects a counting closure to prove a hit skips the build.
fn worker_loop<B: Fn(&TranscriptBuildInput) -> TranscriptBuild + Send + 'static>(
    rx: mpsc::Receiver<BuildRequest>,
    out: mpsc::Sender<BuildResult>,
    build: B,
) {
    let mut memo = BuildMemo::new();
    while let Ok(first) = rx.recv() {
        let mut burst = vec![first];
        while let Ok(r) = rx.try_recv() {
            burst.push(r);
        }
        let req = coalesce(burst).expect("a non-empty burst coalesces to its newest");
        let built = build_with_memo(&mut memo, &req, &build);
        if out.send(BuildResult { seq: req.seq, key: req.key, build: built }).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{Level, Palette};
    use crate::event::Event;

    fn small_input() -> TranscriptBuildInput {
        TranscriptBuildInput::from_app(&crate::app::App::new(), 80, None)
    }

    fn key(events_version: u64, width: usize, level: Level) -> BuildKey {
        BuildKey {
            events_version,
            width,
            ext_ver: 0,
            palette_level: level,
            palette: Palette::builtin(level),
            frac_epoch: 0,
        }
    }

    fn req(seq: u64, width: usize) -> BuildRequest {
        BuildRequest {
            seq,
            key: key(1, width, Level::Rgb),
            input: {
                let mut input = small_input();
                input.width = width;
                input
            },
        }
    }

    #[test]
    fn coalesce_keeps_only_the_newest() {
        let Some(latest) = coalesce(vec![req(1, 100), req(2, 150), req(3, 200)]) else {
            panic!("a non-empty burst coalesces to its newest request");
        };
        assert_eq!(latest.seq, 3);
        assert_eq!(latest.key.width, 200);
        assert!(coalesce(vec![]).is_none());
    }

    #[test]
    fn worker_builds_and_returns_results() {
        let mut events = vec![];
        for i in 0..5 {
            events.push(
                Event::parse_line(
                    &format!(
                        r#"{{"v":1,"type":"user_message","ts":"t","id":"u{i}","content":"hello {i}"}}"#
                    ),
                )
                .unwrap(),
            );
        }
        let mut input = small_input();
        input.events = events;
        let w = TranscriptWorker::spawn();
        let key = key(1, 80, Level::Rgb);
        w.send(BuildRequest {
            seq: 1,
            key: key.clone(),
            input,
        })
        .unwrap();
        let mut got = None;
        for _ in 0..500 {
            if let Ok(r) = w.try_recv_result() {
                got = Some(r);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let r = got.expect("the worker returns its result within 5 s");
        assert_eq!(r.seq, 1);
        assert_eq!(r.key, key);
        assert!(!r.build.lines.is_empty());
        assert!(w.try_recv_result().is_err());
    }

    #[test]
    fn worker_settles_on_the_newest_request() {
        let w = TranscriptWorker::spawn();
        for seq in 1..=3u64 {
            let width = 100 * seq as usize;
            w.send(req(seq, width)).unwrap();
        }
        let mut last = None;
        for _ in 0..1000 {
            match w.try_recv_result() {
                Ok(r) => last = Some(r),
                Err(mpsc::TryRecvError::Empty) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        let r = last.expect("the worker returns at least one result");
        assert_eq!(r.seq, 3, "the settled value is the newest request");
        assert_eq!(r.key.width, 300);
    }

    /// Stage 3 gate.
    /// A repeated toggle between two widths hits the memo.
    /// The build is not called on the toggle back.
    #[test]
    fn width_toggle_hits_the_memo_without_rebuilding() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let builds = AtomicUsize::new(0);
        let counting = |input: &TranscriptBuildInput| {
            builds.fetch_add(1, Ordering::SeqCst);
            build_transcript_input(input)
        };
        let mut memo = BuildMemo::new();
        // First toggle to 80: a miss runs the build.
        let b80 = build_with_memo(&mut memo, &req(1, 80), |i| counting(i));
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        // Toggle to 120: a miss runs the build.
        let b120 = build_with_memo(&mut memo, &req(2, 120), |i| counting(i));
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        // Toggle back to 80: a hit returns the cached build.
        let b80_again = build_with_memo(&mut memo, &req(3, 80), |i| counting(i));
        assert_eq!(
            builds.load(Ordering::SeqCst),
            2,
            "the second toggle must hit the memo, not rebuild"
        );
        assert_eq!(b80_again, b80, "the memo returns the cached build");
        assert_eq!(b120, build_transcript_input(&req(2, 120).input));
    }

    fn b() -> TranscriptBuild {
        build_transcript_input(&small_input())
    }

    #[test]
    fn memo_evicts_oldest_width_at_capacity() {
        // A fresh memo fills four slots. The fifth insert evicts the
        // oldest width (80).
        let mut memo = BuildMemo::new();
        assert_eq!(BuildMemo::DEFAULT_CAP, 4);
        for w in [80usize, 100, 120, 140] {
            memo.insert(&key(1, w, Level::Rgb), b());
        }
        memo.insert(&key(1, 160, Level::Rgb), b());
        assert!(memo.get(&key(1, 80, Level::Rgb)).is_none());
        assert!(memo.get(&key(1, 100, Level::Rgb)).is_some());
        assert!(memo.get(&key(1, 160, Level::Rgb)).is_some());

        // A same-width replace moves its entry to newest. It opens no
        // new slot and drops no sibling.
        let mut memo = BuildMemo::new();
        for w in [80usize, 100, 120, 140] {
            memo.insert(&key(1, w, Level::Rgb), b());
        }
        memo.insert(&key(1, 100, Level::Rgb), b());
        memo.insert(&key(1, 160, Level::Rgb), b());
        assert!(
            memo.get(&key(1, 80, Level::Rgb)).is_none(),
            "the replace must keep 80 in the oldest slot"
        );
        assert!(memo.get(&key(1, 100, Level::Rgb)).is_some());
        assert!(memo.get(&key(1, 160, Level::Rgb)).is_some());

        // A new events version is its own slot, not a replace.
        let mut memo = BuildMemo::new();
        for w in [80usize, 100, 120] {
            memo.insert(&key(1, w, Level::Rgb), b());
        }
        memo.insert(&key(2, 80, Level::Rgb), b());
        assert!(memo.get(&key(2, 80, Level::Rgb)).is_some());
        assert!(memo.get(&key(1, 80, Level::Rgb)).is_some());
    }

    #[test]
    fn memo_full_key_mismatch_is_a_miss() {
        let input = small_input();
        let rgb = key(1, 80, Level::Rgb);
        let c256 = key(1, 80, Level::C256);
        let mut memo = BuildMemo::new();
        let built = build_transcript_input(&input);
        memo.insert(&rgb, built.clone());
        // Same width, different palette: a miss.
        assert!(memo.get(&c256).is_none());
        // The stored key still hits.
        assert!(memo.get(&rgb).is_some());
    }

    /// End to end through the worker loop.
    /// Three settled requests: 80, 120, 80.
    /// The third is a memo hit, so two builds run.
    #[test]
    fn worker_toggle_back_hits_the_memo() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let (tx, rx) = mpsc::channel::<BuildRequest>();
        let (out_tx, out_rx) = mpsc::channel::<BuildResult>();
        let builds = Arc::new(AtomicUsize::new(0));
        let counter = builds.clone();
        let t = thread::spawn(move || {
            worker_loop(rx, out_tx, move |input: &TranscriptBuildInput| {
                counter.fetch_add(1, Ordering::SeqCst);
                build_transcript_input(input)
            })
        });
        fn next(rx: &mpsc::Receiver<BuildResult>) -> BuildResult {
            let start = std::time::Instant::now();
            loop {
                match rx.try_recv() {
                    Ok(r) => return r,
                    Err(mpsc::TryRecvError::Empty) => {
                        assert!(
                            start.elapsed() < std::time::Duration::from_secs(10),
                            "a worker result should arrive within 10 s"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        panic!("the worker closed the result channel early")
                    }
                }
            }
        }
        // One request per round so each settles first.
        // The same-width third request is the memo hit.
        let r1 = {
            tx.send(req(1, 80)).unwrap();
            next(&out_rx)
        };
        assert_eq!(r1.seq, 1);
        let r2 = {
            tx.send(req(2, 120)).unwrap();
            next(&out_rx)
        };
        assert_eq!(r2.seq, 2);
        let r3 = {
            tx.send(req(3, 80)).unwrap();
            next(&out_rx)
        };
        assert_eq!(r3.seq, 3);
        assert_eq!(
            builds.load(Ordering::SeqCst),
            2,
            "the third request is a memo hit, so two builds"
        );
        assert_eq!(r3.build, r1.build, "the third result is the cached first build");
        assert_eq!(r3.key, key(1, 80, Level::Rgb));
        drop(tx);
        t.join().unwrap();
    }
}
