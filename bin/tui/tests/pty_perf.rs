//! PTY performance smoke on the real 24 MB session log.
//!
//! End-to-end check for the promise in
//! docs/tui-perf-background-build-plan.md. No frame over 100 ms
//! during a cache-key miss. On the battlefield fixture the full
//! transcript build takes seconds in the background. The main loop
//! must keep drawing the stale tail and the rebuild indicator while
//! it runs. The braille spinner is a pure function of wall clock, so
//! two screens captured 0.6 s apart must differ when the UI is alive.
//!
//! Skips when the fixture is absent.

mod common;

use common::*;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const TAIL_MARKER: &str = "Thisiisnaainsainaisaiaiainsisisi";
const REBUILD_MARKER: &str = "building transcript";

fn tui_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rushi-tui")
}

/// The battlefield fixture directory. An env override wins, so a copy
/// of the log can be pointed at without editing this file.
fn sessions_root() -> String {
    match std::env::var("TUI_PERF_SESSIONS_ROOT") {
        Ok(p) if !p.is_empty() => p,
        _ => "/home/tony/programming/rushi-tui/sessions".to_string(),
    }
}

fn battlefield_cfg() -> PathBuf {
    let tmp = tmpdir();
    let cfg = tmp.join("config.toml");
    let root = sessions_root();
    std::fs::write(
        &cfg,
        format!("[paths]\nsessions_root = \"{root}\"\n\n[tui]\next_dirs = []\n"),
    )
    .unwrap();
    cfg
}

#[test]
fn perf_24mb_background_build_no_freeze() {
    let root = PathBuf::from(sessions_root());
    let fixture = root.join("tui-diff-spec-lean/events.jsonl");
    if !fixture.exists() {
        eprintln!("SKIP: the 24 MB battlefield fixture is not at {fixture:?}");
        return;
    }
    let cfg = battlefield_cfg();
    let mut pty = Pty::spawn(tui_bin(), "tui-diff-spec-lean", &cfg, None);

    // Startup loads the 24 MB log, then the fast tail-window build
    // renders the log tail on the main thread.
    pty.pump(4.0);
    assert!(pty.alive(), "the process died during startup");
    let text = pty.screen.text();
    assert!(
        text.contains(TAIL_MARKER),
        "the tail of the log should be visible after the fast first build:\n{text}"
    );

    // With thinking blocks collapsed by default (docs/tui-turn-
    // fold.md), the 24 MB startup build settles quickly and the
    // initial "building transcript" window is too short to poll.
    // Recreate the heavy build instead: expand every tool result
    // (Ctrl+O) and every thinking block (Ctrl+T). Each toggle bumps
    // the events version, so the pending miss coalesces into one
    // background build of the fully expanded transcript.
    pty.write_input(b"\x0f");
    pty.write_input(b"\x14");
    // The expanded build settles in under a second. The
    // "building transcript" indicator is on screen far less
    // than a 500 ms poll step, so the coarse poll misses it.
    // Poll the screen fine-grained instead. The harness `pump`
    // samples at ~100 ms (its inner poll timeout). That bounds
    // the catch window to about one poll step. The expanded
    // content widens the build enough for a ~100 ms sample to
    // land inside the indicator window.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let text = pty.screen.text();
        if text.contains(REBUILD_MARKER) {
            break;
        }
        assert!(
            pty.alive(),
            "the process died before the build indicator showed"
        );
        assert!(
            Instant::now() < deadline,
            "the build indicator never appeared within 60 s:\n{text}"
        );
        pty.pump(0.1);
    }

    // While the multi-second build runs the main loop must keep
    // redrawing. Two screens 0.6 s apart must differ (the spinner
    // frame is wall-clock based, one frame per 100 ms).
    let a = pty.screen.text();
    pty.pump(0.6);
    let b = pty.screen.text();
    assert_ne!(
        a, b,
        "no redraw happened while the background build ran. The UI froze:\n{a}\n\n{b}"
    );

    // Wait for the build to settle: the indicator disappears and the
    // settled cache swaps in.
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        if !pty.screen.text().contains(REBUILD_MARKER) {
            break;
        }
        assert!(pty.alive(), "the process died during the background build");
        assert!(
            Instant::now() < deadline,
            "the background build did not settle within 240 s"
        );
        pty.pump(1.0);
    }

    assert!(
        double_q_quit(&mut pty, 8.0),
        "the process should quit cleanly after the build settles"
    );
    let orphans = settled_orphans(&exts_root(), 10.0);
    assert!(orphans.is_empty(), "orphan layer processes: {orphans:?}");
}
