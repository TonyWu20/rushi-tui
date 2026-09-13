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
    /// The request sequence number. It traces a result back to its
    /// request for debugging. The main loop uses result key matching
    /// for the swap, not the sequence.
    #[allow(dead_code)]
    pub seq: u64,
    pub key: BuildKey,
    pub build: TranscriptBuild,
}

/// Keep the newest queued build request, drop the older ones.
pub fn coalesce(burst: Vec<BuildRequest>) -> Option<BuildRequest> {
    burst.into_iter().max_by_key(|r| r.seq)
}

/// The background build worker, owned by the main thread.
pub struct TranscriptWorker {
    tx: mpsc::Sender<BuildRequest>,
    result_rx: mpsc::Receiver<BuildResult>,
    _thread: thread::JoinHandle<()>,
}

impl TranscriptWorker {
    /// Spawn the worker thread with an empty queue.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<BuildRequest>();
        let (res_tx, res_rx) = mpsc::channel::<BuildResult>();
        let _thread = thread::Builder::new()
            .name("tui-transcript-build".into())
            .spawn(move || worker_loop(rx, res_tx))
            .expect("the transcript build thread starts");
        Self {
            tx,
            result_rx: res_rx,
            _thread,
        }
    }

    /// Queue one build request for the worker.
    ///
    /// The request carries the full input snapshot, so the error is
    /// large. The `allow` keeps the plain channel shape without a
    /// `Box` on the error type.
    #[allow(clippy::result_large_err)]
    pub fn send(&self, req: BuildRequest) -> Result<(), mpsc::SendError<BuildRequest>> {
        self.tx.send(req)
    }

    /// The next finished result, without blocking.
    pub fn try_recv_result(&self) -> Result<BuildResult, mpsc::TryRecvError> {
        self.result_rx.try_recv()
    }
}

fn worker_loop(rx: mpsc::Receiver<BuildRequest>, out: mpsc::Sender<BuildResult>) {
    while let Ok(first) = rx.recv() {
        let mut burst = vec![first];
        while let Ok(r) = rx.try_recv() {
            burst.push(r);
        }
        // The burst always holds the request that woke the worker.
        let req = coalesce(burst).expect("a non-empty burst coalesces to its newest");
        let build = build_transcript_input(&req.input);
        if out.send(BuildResult { seq: req.seq, key: req.key, build }).is_err() {
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

    fn req(seq: u64, width: usize) -> BuildRequest {
        let mut input = small_input();
        input.width = width;
        BuildRequest {
            seq,
            key: BuildKey {
                events_version: 1,
                width,
                ext_ver: 0,
                palette_level: Level::Rgb,
                palette: Palette::builtin(Level::Rgb),
                frac_epoch: 0,
            },
            input,
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
        let key = BuildKey {
            events_version: 1,
            width: 80,
            ext_ver: 0,
            palette_level: Level::Rgb,
            palette: Palette::builtin(Level::Rgb),
            frac_epoch: 0,
        };
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
}
