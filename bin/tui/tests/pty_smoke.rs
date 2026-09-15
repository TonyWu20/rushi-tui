//! PTY smoke cases for the TUI.
//! They drive it on a pseudo-terminal.

mod common;

use common::*;
use std::time::{Duration, Instant};

const WHEEL_UP: &[u8] = b"\x1b[<64;5;5M";

fn tui_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tui")
}

fn repo_cfg() -> std::path::PathBuf {
    repo_root().join("config.toml")
}

fn skip_if_missing(path: &std::path::Path, why: &str) -> bool {
    if !path.exists() {
        eprintln!("SKIP: {why}");
        return true;
    }
    false
}

fn wheel_data(n: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity(WHEEL_UP.len() * n as usize);
    for _ in 0..n {
        data.extend_from_slice(WHEEL_UP);
    }
    data
}

#[test]
fn baseline_double_q() {
    let mut pty = Pty::spawn(tui_bin(), "tui-test", &repo_cfg(), None);
    pty.pump(1.5);
    assert!(pty.alive(), "process died during startup");
    assert!(double_q_quit(&mut pty, 8.0), "still running after double-q");
    let orphans = settled_orphans(&exts_root(), 10.0);
    assert!(orphans.is_empty(), "orphan layer processes: {orphans:?}");
}

#[test]
fn burst_300_then_double_q() {
    burst_case(300);
}

#[test]
fn burst_1000_then_double_q() {
    burst_case(1000);
}

fn burst_case(burst: u32) {
    let mut pty = Pty::spawn(tui_bin(), "tui-test", &repo_cfg(), None);
    pty.pump(1.5);
    assert!(pty.alive(), "process died during startup");
    let data = wheel_data(burst);
    assert!(
        write_burst(&mut pty, &data, 60.0),
        "burst write blocked after the deadline"
    );
    pty.pump(0.5);
    assert!(double_q_quit(&mut pty, 8.0), "still running after double-q");
}

#[test]
fn scroll_burst_reaches_head() {
    const HEAD: &str = "This is the first time";
    const TAIL: &str = "Want me to fix #1 and #2";
    let log = repo_root().join("sessions/tui-test/events.jsonl");
    let has_tail = std::fs::read_to_string(&log)
        .map(|s| s.contains(TAIL))
        .unwrap_or(false);
    if !has_tail {
        eprintln!("SKIP scroll-burst: the tui-test log lacks the tail marker");
        return;
    }
    let mut pty = Pty::spawn(tui_bin(), "tui-test", &repo_cfg(), None);
    pty.pump(1.5);
    assert!(pty.alive(), "process died during startup");
    let text = pty.screen.text();
    assert!(
        text.contains(TAIL),
        "tail marker missing at startup:\n{text}"
    );
    assert!(
        !text.contains(HEAD),
        "head marker already visible at startup"
    );
    let data = wheel_data(30000);
    assert!(
        write_burst(&mut pty, &data, 60.0),
        "burst write blocked after the deadline"
    );
    pty.pump(1.5);
    assert!(pty.alive(), "process died during the burst");
    let text = pty.screen.text();
    assert!(
        text.contains(HEAD),
        "head marker not visible after the burst:\n{text}"
    );
    pty.kill(libc::SIGTERM);
    let end = Instant::now() + Duration::from_secs(10);
    while pty.alive() && Instant::now() < end {
        pty.pump(0.2);
    }
    assert!(!pty.alive(), "TUI still running after SIGTERM");
    pty.reap();
    let orphans = settled_orphans(&exts_root(), 10.0);
    assert!(orphans.is_empty(), "orphan layer processes: {orphans:?}");
}

#[test]
fn ext_stub_alive() {
    ensure_ext_bins();
    purge_strays();
    let layer = fixture_root().join("stub");
    if skip_if_missing(&layer, "the ext-fixture 'stub' layer is not found") {
        return;
    }
    let (cfg, _sessions) = layer_cfg(&layer, None);
    let mut pty = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, None);
    let missing = wait_markers(&mut pty, &["EXT stub alive"], 6.0, 10.0);
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "marker not seen: {missing:?}\n{}",
        pty.screen.text()
    );
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let orphans = settled_orphans(&layer, 6.0);
    assert!(orphans.is_empty(), "orphan fixture processes: {orphans:?}");
}

#[test]
fn ext_frame_commandline() {
    ensure_ext_bins();
    purge_strays();
    let layer = fixture_root().join("frame");
    if skip_if_missing(&layer, "the ext-fixture 'frame' layer is not found") {
        return;
    }
    let (cfg, _sessions) = layer_cfg(&layer, None);
    let mut pty = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, None);
    let missing = wait_markers(&mut pty, &["fx-[INSERT]"], 6.0, 10.0);
    assert!(
        missing.is_empty(),
        "frame not shown on startup: {missing:?}\n{}",
        pty.screen.text()
    );
    pty.write_input(b"\x1b");
    pty.pump(0.4);
    pty.write_input(b"/");
    pty.pump(0.4);
    pty.write_input(b"ab");
    pty.pump(1.0);
    let text = pty.screen.text();
    assert!(
        text.contains("/ab"),
        "draft not shown in the frame:\n{text}"
    );
    assert!(
        text.contains('\u{2588}'),
        "cursor block missing from the frame:\n{text}"
    );
    pty.write_input(b"\x1b");
    let missing = wait_markers(&mut pty, &["fx-[NORMAL]"], 6.0, 10.0);
    assert!(
        missing.is_empty(),
        "NORMAL frame not shown after Esc: {missing:?}\n{}",
        pty.screen.text()
    );
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let orphans = settled_orphans(&layer, 6.0);
    assert!(orphans.is_empty(), "orphan fixture processes: {orphans:?}");
}

#[test]
fn ext_dying_hint() {
    ensure_ext_bins();
    purge_strays();
    let layer = fixture_root().join("dying");
    if skip_if_missing(&layer, "the ext-fixture 'dying' layer is not found") {
        return;
    }
    let (cfg, _sessions) = layer_cfg(&layer, None);
    let mut pty = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, None);
    let missing = wait_markers(&mut pty, &["ext dying dead after 3 restarts"], 15.0, 10.0);
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "hint not shown: {missing:?}\n{}",
        pty.screen.text()
    );
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let orphans = settled_orphans(&layer, 6.0);
    assert!(orphans.is_empty(), "orphan fixture processes: {orphans:?}");
}

#[test]
fn ext_badjsonl() {
    ensure_ext_bins();
    purge_strays();
    let layer = fixture_root().join("badjsonl");
    if skip_if_missing(&layer, "the ext-fixture 'badjsonl' layer is not found") {
        return;
    }
    let (cfg, _sessions) = layer_cfg(&layer, None);
    let mut pty = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, None);
    let missing = wait_markers(&mut pty, &[" (no events yet"], 4.0, 8.0);
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "empty state not shown: {missing:?}\n{}",
        pty.screen.text()
    );
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let orphans = settled_orphans(&layer, 6.0);
    assert!(orphans.is_empty(), "orphan fixture processes: {orphans:?}");
}

#[test]
fn ext_append_reject() {
    ensure_ext_bins();
    purge_strays();
    let layer = fixture_root().join("append-reject");
    if skip_if_missing(&layer, "the ext-fixture 'append-reject' layer is not found") {
        return;
    }
    let (cfg, sessions) = layer_cfg(&layer, None);
    let mut pty = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, None);
    let markers = [
        "append rejected: type `tool_result`",
        "ext append-reject appended ext_status",
    ];
    let missing = wait_markers(&mut pty, &markers, 12.0, 10.0);
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "reject markers not seen: {missing:?}\n{}",
        pty.screen.text()
    );
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let orphans = settled_orphans(&layer, 6.0);
    assert!(orphans.is_empty(), "orphan fixture processes: {orphans:?}");
    let log = sessions.join("tui-test-ext/events.jsonl");
    let s = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        s.contains("\"type\":\"ext_status\"") || s.contains("\"type\": \"ext_status\""),
        "ext_status line missing from the log:\n{s}"
    );
}

#[test]
fn ext_statusline_real() {
    ensure_ext_bins();
    purge_strays();
    let layer = exts_root().join("ui_extensions");
    if skip_if_missing(&layer, "the ui_extensions layer is not found") {
        return;
    }
    let (cfg, sessions) = layer_cfg(&layer, Some("smoke-model"));
    seed_session(&sessions, "tui-test-ext", &seed_events());
    let stats = "in:125 out:55 sum:180";
    let mut pty = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, None);
    let missing = wait_markers(&mut pty, &["git:none", "smoke-model", stats], 15.0, 10.0);
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "markers not seen: {missing:?}\n{}",
        pty.screen.text()
    );
    let mut bell = false;
    let mut osc = false;
    let end = Instant::now() + Duration::from_secs(6);
    while (!bell || !osc) && Instant::now() < end {
        pty.pump(0.25);
        bell = pty.screen.raw.contains(&0x07);
        osc = String::from_utf8_lossy(&pty.screen.raw).contains("turn finished");
    }
    assert!(bell, "no terminal bell in the pty stream");
    assert!(osc, "no OSC title in the pty stream");
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let orphans = settled_orphans(&layer, 6.0);
    assert!(orphans.is_empty(), "orphan layer processes: {orphans:?}");
    let mut pty2 = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, None);
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        pty2.pump(0.2);
        assert!(pty2.alive(), "process died on restart");
        if pty2.screen.text().contains(stats) {
            break;
        }
    }
    assert!(
        pty2.screen.text().contains(stats),
        "usage stats did not survive restart:\n{}",
        pty2.screen.text()
    );
    pty2.write_input(b"q");
    pty2.pump(0.4);
    pty2.write_input(b"q");
    let end = Instant::now() + Duration::from_secs(4);
    while pty2.alive() && Instant::now() < end {
        pty2.pump(0.2);
    }
    if pty2.alive() {
        pty2.kill(libc::SIGKILL);
    }
    pty2.reap();
    let orphans = settled_orphans(&layer, 6.0);
    assert!(
        orphans.is_empty(),
        "orphan layer processes after restart: {orphans:?}"
    );
}

#[test]
fn ext_statusline_repo() {
    ensure_ext_bins();
    purge_strays();
    let repo = repo_root();
    let layer = exts_root().join("ui_extensions");
    if skip_if_missing(&layer, "the ui_extensions layer is not found") {
        return;
    }
    let cfg = repo_config_with_ext_dir(&layer);
    let mut pty = Pty::spawn(tui_bin(), "tui-test", &cfg, None);
    let mut markers: Vec<String> = Vec::new();
    if let Some(name) = repo.file_name() {
        let n = name.to_string_lossy();
        if n.len() >= 12 {
            markers.push(n[n.len() - 12..].to_string());
        }
    }
    if let Some(b) = git_branch(&repo) {
        markers.push(format!("git:{b}"));
    }
    if let Some(m) = active_model_from_config(&repo.join("config.toml")) {
        markers.push(m);
    }
    markers.push(String::from("in:"));
    let refs: Vec<&str> = markers.iter().map(String::as_str).collect();
    let missing = wait_markers(&mut pty, &refs, 15.0, 10.0);
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "markers not seen: {missing:?}\n{}",
        pty.screen.text()
    );
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let orphans = settled_orphans(&layer, 6.0);
    assert!(orphans.is_empty(), "orphan layer processes: {orphans:?}");
    let _ = std::fs::remove_file(&cfg);
}

#[test]
fn side_by_side_package_layout() {
    // Issue #11: a binary delivered in a package layout
    // (`pkg/bin/tui` next to `pkg/config.toml` + `pkg/ui_extensions/`)
    // must load that config and its bundled extension layer with no
    // --config flag and no $CONFIG env var, run from a CWD that has
    // no config.toml.
    purge_strays();
    use std::os::unix::fs::PermissionsExt;
    let root = tmpdir();
    let pkg = root.join("pkg");
    let bin_dir = pkg.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let exe = bin_dir.join("tui");
    std::fs::copy(tui_bin(), &exe).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let sessions = pkg.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    // Package-root config: no [tui] ext_dirs, so discovery falls
    // back to the side-by-side `<config-dir>/ui_extensions`.
    std::fs::write(
        pkg.join("config.toml"),
        format!("[paths]\nsessions_root = \"{}\"\n", sessions.display()),
    )
    .unwrap();
    // The bundled extension layer (self-contained stub).
    let stub = pkg.join("ui_extensions").join("stub");
    std::fs::create_dir_all(&stub).unwrap();
    std::fs::write(
        stub.join("stub.sh"),
        r#"#!/usr/bin/env bash
# A stub status extension: it answers every tick op with a
# fixed status row.
while IFS= read -r line; do
  case "$line" in
    *'"op":"tick"'*)
      printf '{"v":1,"op":"status","lines":[["EXT pkg alive",{"fg":"green"}]]}\n'
      ;;
  esac
done
"#,
    )
    .unwrap();
    std::fs::write(
        stub.join("ext.toml"),
        "[ext]\ncommand = \"bash\"\nargs = [\"stub.sh\"]\ncaps = [\"status\"]\ntick_ms = 300\nprotocol_v = 1\n",
    )
    .unwrap();

    // Empty CWD: no config.toml here, and CONFIG is unset in the
    // child — the binary's only route is the side-by-side step.
    let cwd = root.join("empty-cwd");
    std::fs::create_dir_all(&cwd).unwrap();

    let mut pty = Pty::spawn_no_config(exe.to_str().unwrap(), "tui-test-pkg", None, &cwd);
    let missing = wait_markers(&mut pty, &["EXT pkg alive"], 10.0, 10.0);
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "bundled extension marker not seen: {missing:?}\n{}",
        pty.screen.text()
    );
    assert!(double_q_quit(&mut pty, 8.0), "still running after double-q");
    pty.reap();
    let orphans = settled_orphans(&pkg.join("ui_extensions"), 6.0);
    assert!(orphans.is_empty(), "orphan layer processes: {orphans:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ext_statusline_slowgit() {
    ensure_ext_bins();
    purge_strays();
    let layer = exts_root().join("ui_extensions");
    if skip_if_missing(&layer, "the ui_extensions layer is not found") {
        return;
    }
    let real_git = system_git().expect("no system git for the wrapper");
    let bindir = tmpdir().join("bindir");
    std::fs::create_dir_all(&bindir).unwrap();
    let script = format!(
        "#!/bin/sh\nsleep 4\nexec \"{}\" \"$@\"\n",
        real_git.display()
    );
    let gpath = bindir.join("git");
    std::fs::write(&gpath, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gpath, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (cfg, sessions) = layer_cfg(&layer, Some("smoke-model"));
    seed_session(&sessions, "tui-test-ext", &seed_events());
    let extra = bindir.to_string_lossy().to_string();
    let mut pty = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, Some(extra.as_str()));
    let stats = "in:125 out:55 sum:180";
    let t0 = Instant::now();
    let end = t0 + Duration::from_secs(18);
    let mut row_at: Option<f64> = None;
    let mut stale_at: Option<f64> = None;
    loop {
        pty.pump(0.25);
        assert!(pty.alive(), "process died during startup");
        let text = pty.screen.text();
        if row_at.is_none() && text.contains(stats) {
            row_at = Some(t0.elapsed().as_secs_f64());
        }
        if String::from_utf8_lossy(&pty.screen.raw).contains("status stale") {
            stale_at = Some(t0.elapsed().as_secs_f64());
            break;
        }
        if Instant::now() >= end {
            break;
        }
    }
    assert!(
        stale_at.is_none(),
        "stale hint shown at {stale_at:?}s; a tick reply must not wait on the git call"
    );
    assert!(
        row_at.is_some(),
        "the status row never showed:\n{}",
        pty.screen.text()
    );
    assert!(
        row_at.unwrap() < 4.0,
        "the row took {row_at:?}s; the first tick reply must stay fast"
    );
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let orphans = settled_orphans(&layer, 6.0);
    assert!(orphans.is_empty(), "orphan layer processes: {orphans:?}");
}

/// Open every fold so the full transcript is visible on screen.
/// The editor starts in insert mode, so escape to normal first.
/// The double-s gate then enters browse mode. There, `z R` runs
/// `fold_open_all` (docs/tui-turn-fold.md). The shared `turn_fold`
/// set drives both views, so the main view shows the content too.
fn open_all_folds(pty: &mut Pty) {
    pty.write_input(b"\x1b");
    pty.pump(0.3);
    pty.write_input(b"s");
    pty.pump(0.3);
    pty.write_input(b"s");
    pty.pump(0.4);
    pty.write_input(b"z");
    pty.pump(0.2);
    pty.write_input(b"R");
    pty.pump(0.4);
}

/// Leave browse mode with the double-s exit gate. The fold state
/// survives: the `turn_fold` set is shared by both views.
fn leave_browse(pty: &mut Pty) {
    pty.write_input(b"s");
    pty.pump(0.3);
    pty.write_input(b"s");
    pty.pump(0.4);
}

#[test]
fn ext_tool_result_kill() {
    ensure_ext_bins();
    purge_strays();
    let exts = exts_root();
    let src = exts.join("ui_extensions-demos/tool_result");
    if skip_if_missing(&src, "the tool_result demo is not found") {
        return;
    }
    let tmp = tmpdir();
    let layer = tool_result_only_layer(&tmp, &src);
    let tr_dir = layer.join("tool_result");
    let (cfg, sessions) = layer_cfg(&layer, Some("smoke-model"));
    seed_session(&sessions, "tui-test-ext", &seed_events());
    let log = sessions.join("tui-test-ext/events.jsonl");
    let mut pty = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, None);
    // The turn-fold rescope (docs/tui-turn-fold.md) leaves turns
    // collapsed by default. Open every fold so the ext-rendered
    // body is on screen.
    open_all_folds(&mut pty);
    let missing = wait_markers(&mut pty, &["[ext] tool:call_1"], 15.0, 10.0);
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "tool_result ext state not shown: {missing:?}\n{}",
        pty.screen.text()
    );
    // Kill the ext and latch the "dead" flash as it fires. The
    // flash has a 4 s TTL, so latching during the loop keeps the
    // catch reliable.
    let mut saw_dead = false;
    let kill_end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < kill_end {
        for p in procs_with_cwd_under(&tr_dir, true) {
            unsafe {
                libc::kill(p as i32, libc::SIGKILL);
            }
        }
        pty.pump(0.25);
        if pty.screen.text().contains("ext tool_result is dead") {
            saw_dead = true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let ts = "2026-08-27T10:05:00Z";
    let extra = format!(
        "{}\n{}\n",
        serde_json::json!({"v": 1, "type": "tool_call", "ts": ts, "id": "call_9", "name": "bash", "arguments": {"command": "true"}}),
        serde_json::json!({"v": 1, "type": "tool_result", "ts": ts, "id": "call_9", "value": {"text": "late result"}, "is_error": false})
    );
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(extra.as_bytes()).unwrap();
    }
    // The late call_9 block arrives folded, at the transcript
    // tail. Each pass re-opens every fold and jumps to the tail.
    // The z R catches the block once the tailer ingests it. The G
    // keeps the tail in view, so the builtin body reaches the
    // screen.
    let mut saw_builtin = false;
    let end = Instant::now() + Duration::from_secs(15);
    while !saw_builtin {
        pty.write_input(b"z");
        pty.pump(0.1);
        pty.write_input(b"R");
        pty.pump(0.1);
        pty.write_input(b"G");
        pty.pump(0.25);
        let text = pty.screen.text();
        if text.contains("late result") && !text.contains("[ext] tool:call_9") {
            saw_builtin = true;
        }
        if Instant::now() >= end {
            break;
        }
    }
    assert!(
        saw_dead,
        "the dead hint did not show:\n{}",
        pty.screen.text()
    );
    assert!(
        saw_builtin,
        "the builtin tool_result row did not show:\n{}",
        pty.screen.text()
    );
    // Back to the main view, so the q q quit gate is live.
    leave_browse(&mut pty);
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let orphans = settled_orphans(&layer, 6.0);
    assert!(orphans.is_empty(), "orphan layer processes: {orphans:?}");
}

#[test]
fn ext_mermaid() {
    ensure_ext_bins();
    purge_strays();
    let layer = exts_root().join("ui_extensions");
    if skip_if_missing(&layer, "the ui_extensions layer is not found") {
        return;
    }
    let ts = "2026-08-27T10:00:00Z";
    let events = vec![
        serde_json::json!({
            "v": 1,
            "type": "user_message",
            "ts": ts,
            "content": "draw the flow"
        })
        .to_string(),
        serde_json::json!({
            "v": 1,
            "type": "assistant_message",
            "ts": ts,
            "content": "Here is the flow:\n\n```mermaid\ngraph TD\n  A-->B\n```\n",
            "stop_reason": "stop",
        })
        .to_string(),
        serde_json::json!({
            "v": 1,
            "type": "assistant_message",
            "ts": ts,
            "content": "Broken one:\n\n```mermaid\nthis is %% not a diagram\n```\n",
            "stop_reason": "stop",
        })
        .to_string(),
    ];
    let (cfg, sessions) = layer_cfg(&layer, Some("smoke-model"));
    seed_session(&sessions, "tui-test-mmd", &events);
    // The diagram plus both messages span more than the default 24
    // rows, so spawn a taller window.
    let mut pty = Pty::spawn_sized(tui_bin(), "tui-test-mmd", &cfg, None, 50, 80);
    // The turn-fold rescope (docs/tui-turn-fold.md) leaves turns
    // collapsed by default. Open every fold so both messages sit on
    // screen.
    open_all_folds(&mut pty);
    let missing = wait_markers(
        &mut pty,
        &["\u{2502} A \u{2502}", "not a diagram"],
        15.0,
        10.0,
    );
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "mermaid markers not seen: {missing:?}\n{}",
        pty.screen.text()
    );
    let text = pty.screen.text();
    assert!(
        !text.contains("graph TD"),
        "the raw fence leaked into the screen:\n{text}"
    );
    // Back to the main view, so the q q quit gate is live.
    leave_browse(&mut pty);
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let orphans = settled_orphans(&layer, 6.0);
    assert!(orphans.is_empty(), "orphan layer processes: {orphans:?}");
}

#[test]
fn ext_rus() {
    ensure_ext_bins();
    purge_strays();
    let layer = exts_root().join("ext-rs");
    if skip_if_missing(&layer, "the ext-rs layer is not found") {
        return;
    }
    let (cfg, sessions) = layer_cfg(&layer, Some("smoke-model"));
    seed_session(&sessions, "tui-test-ext", &seed_events());
    let stats = "in:125 out:55 sum:180";
    let mut pty = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, None);
    let missing = wait_markers(
        &mut pty,
        &["\u{E725} none", "smoke-model", stats],
        15.0,
        10.0,
    );
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "markers not seen: {missing:?}\n{}",
        pty.screen.text()
    );
    let mut bell = false;
    let end = Instant::now() + Duration::from_secs(7);
    while !bell && Instant::now() < end {
        pty.pump(0.25);
        bell = pty.screen.raw.contains(&0x07);
    }
    assert!(bell, "no terminal bell in the pty stream");
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let orphans = settled_orphans(&layer, 6.0);
    assert!(orphans.is_empty(), "orphan layer processes: {orphans:?}");
}

#[test]
fn ext_signal_orphan() {
    ensure_ext_bins();
    purge_strays();
    let layer = exts_root().join("ext-rs");
    if skip_if_missing(&layer, "the ext-rs layer is not found") {
        return;
    }
    let (cfg, sessions) = layer_cfg(&layer, Some("smoke-model"));
    seed_session(&sessions, "tui-test-ext", &seed_events());
    let mut pty = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, None);
    let missing = wait_markers(&mut pty, &["\u{E725} none", "in:"], 15.0, 10.0);
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "statusline not shown: {missing:?}\n{}",
        pty.screen.text()
    );
    pty.kill(libc::SIGTERM);
    let end = Instant::now() + Duration::from_secs(10);
    while pty.alive() && Instant::now() < end {
        pty.pump(0.2);
    }
    assert!(!pty.alive(), "TUI still running after SIGTERM");
    pty.reap();
    let orphans = settled_orphans(&layer, 10.0);
    assert!(
        orphans.is_empty(),
        "orphan layer processes after SIGTERM: {orphans:?}"
    );
}

#[test]
fn ext_goal_row_installed() {
    ensure_ext_bins();
    purge_strays();
    let goal = exts_root().join("ui_extensions/goal");
    if skip_if_missing(&goal, "the goal ext is not found") {
        return;
    }
    let tmp = tmpdir();
    let layer = goal_only_layer(&tmp, &goal);
    let (cfg, sessions) = layer_cfg(&layer, Some("smoke-model"));
    seed_session(&sessions, "tui-test-ext", &seed_events());
    seed_goal(&sessions, "tui-test-ext");
    let mut pty = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, None);
    let missing = wait_markers(
        &mut pty,
        &["\u{26a1}", "shrink the smoke pty", "12.4k"],
        15.0,
        10.0,
    );
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "goal row markers not seen: {missing:?}\n{}",
        pty.screen.text()
    );
    assert!(double_q_quit(&mut pty, 8.0), "still running after double-q");
    let goal_real = goal.canonicalize().unwrap_or_else(|_| goal.clone());
    let orphans = settled_orphans(&goal_real, 6.0);
    assert!(orphans.is_empty(), "orphan goal ext processes: {orphans:?}");
}

#[test]
fn ext_goal_row_bare() {
    let tmp = tmpdir();
    let layer = empty_layer(&tmp);
    let (cfg, sessions) = layer_cfg(&layer, None);
    seed_session(&sessions, "tui-test-ext", &seed_events());
    seed_goal(&sessions, "tui-test-ext");
    let mut pty = Pty::spawn(tui_bin(), "tui-test-ext", &cfg, None);
    pty.pump(6.0);
    assert!(pty.alive(), "process died during startup");
    assert!(
        !pty.screen.text().contains("\u{26a1}"),
        "goal row shown without the goal ext:\n{}",
        pty.screen.text()
    );
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
}

/// A fixture cwd that is not under a git worktree. `collect_in`
/// switches to `git ls-files` when a `.git` sits up the tree. This
/// sandbox carries a stray `.git` in /tmp, which makes git fail and
/// the picker enumerate zero items. HOME is a clean tree, mirroring
/// `non_git_fixture` in picker/items.rs.
fn non_git_fix_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let p = home.join(format!(".tui-preview-fix-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Wait until the TUI reaches the idle editor. The statusline names
/// the session, so the marker is deterministic. The picker then
/// opens on a typed `@`.
fn wait_ready(pty: &mut Pty, session: &str) {
    let marker = format!("Session: {session}");
    let missing = wait_markers(pty, &[&marker], 10.0, 10.0);
    assert!(
        pty.alive(),
        "process died during startup:\n{}",
        pty.screen.text()
    );
    assert!(
        missing.is_empty(),
        "TUI did not reach the idle editor:\n{}",
        pty.screen.text()
    );
}

/// Plan 8: files over `PREVIEW_MAX_BYTES` are never read. The pane
/// shows the size-guard placeholder and the TUI stays responsive
/// (docs/tui-preview-pane-plan.md, layer 1).
#[test]
fn picker_preview_size_guard_no_freeze() {
    let tmp = tmpdir();
    let fix = non_git_fix_dir();
    // A 50 MiB sparse file, over the 10 MiB guard cap.
    let mut big = std::fs::File::create(fix.join("big.log")).unwrap();
    use std::io::Seek;
    big.seek(std::io::SeekFrom::Start(50 * 1024 * 1024))
        .unwrap();
    big.set_len(50 * 1024 * 1024).unwrap();
    drop(big);
    std::fs::write(fix.join("small1.txt"), "alpha\n").unwrap();
    std::fs::write(fix.join("small2.txt"), "beta\n").unwrap();

    let layer = empty_layer(&tmp);
    let (cfg, sessions) = layer_cfg(&layer, None);
    seed_session(&sessions, "preview-guard", &[]);

    // 120 cols keeps the float above the compact threshold, so the
    // preview pane renders.
    let mut pty = Pty::spawn_in_sized(tui_bin(), "preview-guard", &cfg, None, &fix, 24, 120);
    wait_ready(&mut pty, "preview-guard");
    // Open the picker. `big.log` is item 0 (alphabetical order).
    pty.write_input(b"@");
    pty.pump(1.0);
    // The fixture has fewer than `PREVIEW_CUTOFF` (4) items, so force
    // the pane on with Ctrl+P.
    pty.write_input(b"\x10");
    let missing = wait_markers(&mut pty, &["too large to preview"], 15.0, 10.0);
    assert!(
        missing.is_empty(),
        "size-guard placeholder not shown: {missing:?}\n{}",
        pty.screen.text()
    );
    assert!(pty.alive(), "process died at the size guard");
    // Move to a small file: its content must settle, proving the TUI
    // is not frozen by the oversized file.
    pty.write_input(b"\x1b[B");
    let missing = wait_markers(&mut pty, &["alpha"], 15.0, 10.0);
    assert!(
        missing.is_empty(),
        "small-file content not shown after move: {missing:?}\n{}",
        pty.screen.text()
    );
    assert!(pty.alive(), "process died during the pane move");
    // Drop the leftover `@`: the quit gate wants an empty draft.
    pty.write_input(b"\x7f");
    pty.pump(0.3);
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let _ = std::fs::remove_dir_all(&fix);
}

/// Plan 2 end-to-end: a cursor move cancels the in-flight preview
/// load. `a0.txt` is a multi-megabyte regular file, so its background
/// read has a real in-flight window. A blocking FIFO would jam the
/// serial worker, so the case uses a file
/// (docs/tui-preview-pane-plan.md, layer 2).
#[test]
fn picker_preview_cancel_inflight() {
    let tmp = tmpdir();
    let fix = non_git_fix_dir();
    // ~3.2 MB: a real background read, still under the 10 MiB cap.
    let filler: String = "filler-line-0123456789\n".repeat(150_000);
    std::fs::write(fix.join("a0.txt"), format!("a0-head-marker\n{filler}")).unwrap();
    std::fs::write(fix.join("b1.txt"), "b-line-1\nb-line-2\n").unwrap();

    let layer = empty_layer(&tmp);
    let (cfg, sessions) = layer_cfg(&layer, None);
    seed_session(&sessions, "preview-cancel", &[]);

    let mut pty = Pty::spawn_in_sized(tui_bin(), "preview-cancel", &cfg, None, &fix, 24, 120);
    wait_ready(&mut pty, "preview-cancel");
    // `a0.txt` is item 0 (alphabetical order): its read is in flight
    // right after the picker opens.
    pty.write_input(b"@");
    let missing = wait_markers(&mut pty, &["a0.txt", "b1.txt"], 10.0, 10.0);
    assert!(
        missing.is_empty(),
        "fixture files not listed: {missing:?}\n{}",
        pty.screen.text()
    );
    // Fewer than `PREVIEW_CUTOFF` items: force the pane on, then move
    // down so the `a0` load is cancelled and `b1` dispatches.
    pty.write_input(b"\x10");
    pty.write_input(b"\x1b[B");
    let missing = wait_markers(&mut pty, &["b-line-1"], 20.0, 10.0);
    assert!(
        missing.is_empty(),
        "new item content not shown after cancel: {missing:?}\n{}",
        pty.screen.text()
    );
    let text = pty.screen.text().to_string();
    assert!(
        !text.contains("a0-head-marker"),
        "stale item content still visible after cancel:\n{text}"
    );
    assert!(pty.alive(), "process died during the cancel");
    // Drop the leftover `@` so the quit gate (empty draft) opens.
    pty.write_input(b"\x7f");
    pty.pump(0.3);
    assert!(double_q_quit(&mut pty, 4.0), "still running after double-q");
    let _ = std::fs::remove_dir_all(&fix);
}

/// Item 4 end-to-end: the TUI enables the kitty keyboard protocol at
/// startup, so a `Ctrl+Shift+P` press encoded as CSI-u (`\x1b[80;6u`,
/// codepoint 80 = `P`, modifier 6 = 1+shift+ctrl) toggles the float's
/// focus to the preview pane.
#[test]
fn csi_u_ctrl_shift_p_toggles_preview_focus() {
    // Wide terminal so the full input-bar hint stays visible.
    let mut pty = Pty::spawn_in_sized(
        tui_bin(),
        "tui-test",
        &repo_cfg(),
        None,
        std::path::Path::new("."),
        40,
        200,
    );
    pty.pump(1.5);
    assert!(pty.alive(), "process died during startup");
    // The TUI boots in insert mode. Esc returns to normal mode;
    // only in normal mode does `:` open the command palette.
    pty.write_input(b"\x1b");
    pty.pump(0.3);
    pty.write_input(b":");
    pty.pump(0.7);
    let text = pty.screen.text().to_string();
    assert!(
        text.contains("enter ok"),
        "the palette did not open:\n{text}"
    );
    // Protocol-form `Ctrl+Shift+P`: focus moves to the preview pane.
    pty.write_input(b"\x1b[80;6u");
    pty.pump(0.7);
    let text = pty.screen.text().to_string();
    assert!(
        text.contains("preview focus"),
        "the focus hint is missing after CSI-u Ctrl+Shift+P:\n{text}"
    );
    // Press it again: focus returns to the list, the hint disappears.
    pty.write_input(b"\x1b[80;6u");
    pty.pump(0.7);
    let text = pty.screen.text().to_string();
    assert!(
        !text.contains("preview focus"),
        "the focus hint must be gone after toggling back:\n{text}"
    );
    pty.kill(libc::SIGTERM);
    let end = Instant::now() + Duration::from_secs(10);
    while pty.alive() && Instant::now() < end {
        pty.pump(0.2);
    }
    pty.reap();
}
