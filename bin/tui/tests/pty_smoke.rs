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
        bell = pty.screen.raw.iter().any(|&b| b == 0x07);
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
    let missing = wait_markers(&mut pty, &["[ext] tool:call_1"], 15.0, 10.0);
    assert!(pty.alive(), "process died during startup");
    assert!(
        missing.is_empty(),
        "tool_result ext state not shown: {missing:?}\n{}",
        pty.screen.text()
    );
    let kill_end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < kill_end {
        for p in procs_with_cwd_under(&tr_dir, true) {
            unsafe {
                libc::kill(p as i32, libc::SIGKILL);
            }
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
    let mut saw_dead = false;
    let mut saw_builtin = false;
    let end = Instant::now() + Duration::from_secs(15);
    while !(saw_dead && saw_builtin) {
        pty.pump(0.25);
        let text = pty.screen.text();
        if text.contains("ext tool_result is dead") {
            saw_dead = true;
        }
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
    let mut pty = Pty::spawn(tui_bin(), "tui-test-mmd", &cfg, None);
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
        bell = pty.screen.raw.iter().any(|&b| b == 0x07);
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
