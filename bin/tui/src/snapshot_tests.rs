//! Snapshot tests for the TUI using ratatui `TestBackend` + `insta`.
//!
//! Follows the official ratatui recipe
//! <https://ratatui.rs/recipes/testing/snapshots/>.
//!
//! Each test drives the `App` into a concrete user-visible state, renders one
//! frame onto a `TestBackend`, and snapshots the resulting terminal grid.
//!
//! Generate or review snapshots with:
//! ```sh
//! cargo insta test -p tui && cargo insta review
//! ```

#![cfg(test)]

use std::collections::HashMap;

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tempfile::TempDir;

use crate::app::{App, Key};
use crate::color::{Level, Palette};
use crate::event::Event;
use crate::ext::{discover, ExtHost};
use crate::port::SessionId;
use crate::render::draw;
use crate::tool_display::Preset;
use crate::tool_display::ToolDisplay;
use serde_json;

// ── shared harness ──────────────────────────────────────────────────

/// Build an `ExtHost` with no extension processes (same shape as the
/// helper that previously lived in `render.rs` tests).
fn empty_host() -> (ExtHost, TempDir) {
    let tmp = TempDir::new().unwrap();
    let cfg = crate::config::TuiConfig {
        clipboard_unnamed: false,
        sessions_root: tmp.path().join("sessions"),
        loop_cmd: None,
        config_dir: tmp.path().to_path_buf(),
        config_path: tmp.path().join("config.toml"),
        ext_dirs: vec![tmp.path().join("ui_extensions")],
        active_model: None,
        color: None,
        color_scheme: None,
        custom_schemes: HashMap::new(),
        tool_display: ToolDisplay::preset(Preset::OpenCode),
    };
    let disc = discover(&cfg).unwrap();
    let host = ExtHost::new(&disc, &cfg);
    (host, tmp)
}

/// Render one frame of the app at `(w × h)` and return the terminal
/// grid as a `String` (the `Display` repr of `TestBackend`).
fn render(app: &mut App, host: &ExtHost, w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).expect("test backend");
    let _ = term.draw(|f| {
        let mut cursor = None;
        draw(f, app, &mut cursor, host);
    });
    term.backend().to_string()
}

/// Build an `App` with a single active session holding the given events.
fn app_with_session(events: Vec<Event>) -> App {
    let mut app = App::new();
    let log_lines = events.len() as u64;
    app.set_active(SessionId::new("s1"), events, log_lines);
    app
}

/// Parse a JSON event line into an `Event`.
fn ev(json: &str) -> Event {
    Event::parse_line(json).unwrap()
}

// ── main screen ─────────────────────────────────────────────────────

#[test]
fn snap_empty_app() {
    let mut app = App::new();
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_session_with_conversation() {
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"Hello, world"}"#),
        ev(r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"Hi! How can I help?","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":5},"reasoning":{}}"#),
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u2","content":"What is Rust?"}"#),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_running_loop_indicator() {
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"run build"}"#),
        ev(
            r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"Building…","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":5},"reasoning":{}}"#,
        ),
    ];
    let mut app = app_with_session(events);
    let sid = app.active().cloned().unwrap();
    app.attach_external_loop(sid);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    // The spinner frame is time-dependent. Mask the whole braille
    // block so the snapshot is deterministic (render.rs frames).
    let spinner_re = regex::Regex::new(r"[\u{2800}-\u{28ff}]+").unwrap();
    let out = spinner_re.replace_all(&out, "[SPINNER]").into_owned();
    insta::assert_snapshot!(out);
}

#[test]
fn snap_pending_approval_banner() {
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"delete file"}"#),
        ev(
            r#"{"v":1,"type":"tool_call","ts":"t","id":"c1","name":"bash","arguments":{"command":"rm -rf /tmp/x"}}"#,
        ),
        ev(
            r#"{"v":1,"type":"approval_request","ts":"t","id":"appr-1","call_id":"c1","prompt":"Allow rm -rf /tmp/x?"}"#,
        ),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_pending_user_messages() {
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"first question"}"#),
        ev(
            r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"answering…","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":5},"reasoning":{}}"#,
        ),
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u2","content":"second question","queue":"follow"}"#),
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u3","content":"third question","queue":"follow"}"#),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_assistant_thinking_block() {
    // The expanded thinking block: the reasoning body starts at the
    // left edge with no content gutter (the 12-space indent was the
    // last remnant of the content gutter), and the whole panel keeps
    // the one-column left/right margin.
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"explain"}"#),
        ev(
            r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"The answer is 42.","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":5},"reasoning":[{"content":[{"type":"reasoning_text","text":"Step one: read the question.\nStep two: compute the answer."}]}]}"#,
        ),
    ];
    // Thinking blocks are collapsed by default (2026-09-15 re-scope).
    // This snapshot pins the expanded form, so expand explicitly.
    let mut app = app_with_session(events);
    app.thinking_expanded = true;
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_tool_result_collapsed() {
    let events = vec![
        ev(r#"{"v":1,"type":"tool_call","ts":"t","id":"c1","name":"bash","arguments":{"command":"make"}}"#),
        ev(
            r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"text":"Compiling tui v0.1.0\nFinished in 3.2s\n","exit_code":0,"stdout":"Compiling tui v0.1.0\nFinished in 3.2s\n","stderr":"","timed_out":false,"truncated":false},"is_error":false}"#,
        ),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_tool_result_expanded() {
    let events = vec![
        ev(r#"{"v":1,"type":"tool_call","ts":"t","id":"c1","name":"bash","arguments":{"command":"make"}}"#),
        ev(
            r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"text":"Compiling tui v0.1.0\nFinished in 3.2s\n","exit_code":0,"stdout":"Compiling tui v0.1.0\nFinished in 3.2s\n","stderr":"","timed_out":false,"truncated":false},"is_error":false}"#,
        ),
    ];
    let mut app = app_with_session(events);
    app.toggle_block_expand("c1");
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_tool_result_read_shows_path_in_header() {
    // The `read` result panel header shows the tool name and the
    // file it read (the dim label after the name, the call's
    // `file_path` argument). The success background already signals
    // the outcome, so no `ok` word.
    let events = vec![
        ev(
            r#"{"v":1,"type":"tool_call","ts":"t","id":"r1","name":"read","arguments":{"file_path":"bin/tui/src/main.rs"}}"#,
        ),
        ev(
            r#"{"v":1,"type":"tool_result","ts":"t","id":"r1","value":{"text":"fn main() {}\n","lines":["fn main() {}"],"total_lines":1},"is_error":false}"#,
        ),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_tool_result_edit() {
    // The `edit` result panel: the adaptive diff
    // (docs/tui-tool-result-truncation.md). The value carries `before`
    // (the old content) and `after` (the new content); the panel shows
    // a diff stat row, the file path, and the diff body (split layout
    // at this 80-column pane, unified below `DIFF_SPLIT_MIN_WIDTH*2`).
    // Snapshot so the user can edit the terminal grid to describe the
    // desired `tool:edit` display.
    let events = vec![
        ev(
            r#"{"v":1,"type":"tool_call","ts":"t","id":"e1","name":"edit","arguments":{"file_path":"bin/tui/src/main.rs"}}"#,
        ),
        ev(
            r#"{"v":1,"type":"tool_result","ts":"t","id":"e1","value":{"text":"The file bin/tui/src/main.rs has been updated.","path":"bin/tui/src/main.rs","before":"fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\nfn main() {\n    let total = add(1, 2);\n    let unused = add(0, 0);\n}","after":"fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\nfn main() {\n    let total = add(1, 2);\n    let doubled = add(total, total);\n}","replace_all":false},"is_error":false}"#,
        ),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_error_event() {
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"do something"}"#),
        ev(r#"{"v":1,"type":"error","ts":"t","message":{"code":"E404","detail":"file not found"}}"#),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_long_transcript_scrolled_back() {
    let events: Vec<Event> = (0..20)
        .map(|i| {
            ev(&format!(
                r#"{{"v":1,"type":"user_message","ts":"t","id":"u{i}","content":"message {i}"}}"#
            ))
        })
        .collect();
    let mut app = app_with_session(events);
    app.set_viewport_height(24);
    app.scroll_up(15);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 30);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_narrow_terminal() {
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"Hello in a narrow pane"}"#),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 60, 20);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_wide_terminal() {
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"Hello in a wide pane"}"#),
        ev(
            r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"Wide response","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":5},"reasoning":{}}"#,
        ),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 120, 40);
    insta::assert_snapshot!(out);
}

// ── editor states ───────────────────────────────────────────────────

#[test]
fn snap_editor_insert_mode() {
    let mut app = App::new();
    app.set_draft(String::from("hello world"));
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_editor_normal_mode() {
    let mut app = App::new();
    app.set_draft(String::from("hello world"));
    app.editor_press(Key::Esc);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_editor_multiline_draft() {
    let mut app = App::new();
    app.set_draft(String::from("line one\nline two\nline three"));
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_editor_search_command_line() {
    let mut app = App::new();
    app.set_draft(String::from("alpha beta gamma"));
    app.editor_press(Key::Esc);
    app.editor_press(Key::Char('/'));
    app.editor_press(Key::Char('b'));
    app.editor_press(Key::Char('e'));
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_editor_visual_selection() {
    let mut app = App::new();
    app.set_draft(String::from("hello world\nfoo bar"));
    app.editor_press(Key::Esc);
    // visual line: V selects the whole line
    app.editor_press(Key::Char('V'));
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_editor_w_motion() {
    let mut app = App::new();
    app.set_draft(String::from("foo bar baz qux"));
    app.editor_press(Key::Esc);
    app.editor_press(Key::Char('w'));
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

// ── browse mode ─────────────────────────────────────────────────────

#[test]
fn snap_browse_mode_active() {
    let events: Vec<Event> = (0..50)
        .map(|i| {
            ev(&format!(
                r#"{{"v":1,"type":"user_message","ts":"t","id":"u{i}","content":"line {i}"}}"#
            ))
        })
        .collect();
    let mut app = app_with_session(events);
    app.set_viewport_height(24);
    // Simulate double-s to enter browse mode.
    app.press(Key::Char('s'));
    app.press(Key::Char('s'));
    // Prime the browse layout so the renderer has data.
    app.set_browse_layout(50, 24, 76, vec!["line".to_string(); 50], Vec::new());
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 30);
    insta::assert_snapshot!(out);
}

// ── turn fold (docs/tui-turn-fold.md) ─────────────────────────────

/// Two completed turns.
/// Each is user, mid assistant, tool call, tool result, final assistant.
/// The second turn uses an extension tool name so the tally picks it up.
fn fold_session_events() -> Vec<Event> {
    // Multi-line result bodies so the L2 (results folded to the
    // collapsed cap, plus the "... more lines" hint) and L3
    // (result fully expanded) frames differ.
    let c1_body: String = (0..14)
        .map(|i| format!("out {i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let c2_body: String = (0..12)
        .map(|i| format!("ext {i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"first question"}"#),
        ev(r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"let me look","tool_calls":[],"stop_reason":"stop"}"#),
        ev(r#"{"v":1,"type":"tool_call","ts":"t","id":"c1","name":"bash","arguments":{"command":"ls"}}"#),
        ev(&format!(
            r#"{{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{{"text":{},"exit_code":0,"stdout":{},"stderr":"","timed_out":false,"truncated":false}},"is_error":false}}"#,
            serde_json::to_string(&c1_body).unwrap(),
            serde_json::to_string(&c1_body).unwrap(),
        )),
        ev(r#"{"v":1,"type":"assistant_message","ts":"t","id":"a2","content":"final one","tool_calls":[],"stop_reason":"stop"}"#),
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u2","content":"second question"}"#),
        ev(r#"{"v":1,"type":"assistant_message","ts":"t","id":"a3","content":"thinking out loud","tool_calls":[],"stop_reason":"stop"}"#),
        ev(r#"{"v":1,"type":"tool_call","ts":"t","id":"c2","name":"mymcp__ext","arguments":{}}"#),
        ev(&format!(
            r#"{{"v":1,"type":"tool_result","ts":"t","id":"c2","value":{{"text":{}}},"is_error":false}}"#,
            serde_json::to_string(&c2_body).unwrap(),
        )),
        ev(r#"{"v":1,"type":"assistant_message","ts":"t","id":"a4","content":"final two","tool_calls":[],"stop_reason":"stop"}"#),
    ]
}

/// The settled fold frame: mirror the main loop, where the first
/// draw registers the cache miss, `dispatch_transcript_build`
/// rebuilds the cache on the main thread (no worker in tests), and
/// the final draw shows the fresh build with no build indicator.
fn render_fold(app: &mut App, host: &ExtHost) -> String {
    let _ = render(app, host, 80, 24);
    // Let the 75 ms width-debounce window elapse so the dispatch is
    // not held (docs/tui-perf-background-build-plan.md stage 4).
    std::thread::sleep(std::time::Duration::from_millis(100));
    app.dispatch_transcript_build(Some(host));
    render(app, host, 80, 24)
}

/// Enter browse on a fold session and run one settled frame.
/// The frame primes the layout, the event starts, and the block spans.
fn primed_browse_fold_app(events: Vec<Event>) -> (App, ExtHost, TempDir) {
    let mut app = app_with_session(events);
    app.set_viewport_height(24);
    app.browse().enter();
    let (host, tmp) = empty_host();
    let _ = render_fold(&mut app, &host);
    (app, host, tmp)
}

/// The L1 default. Entering browse renders every turn folded.
/// Each turn shows the user box, a summary tally, and the final box.
#[test]
fn snap_turn_fold_all_folded() {
    let (mut app, host, _tmp) = primed_browse_fold_app(fold_session_events());
    let out = render_fold(&mut app, &host);
    insta::assert_snapshot!(out);
}

/// `zo` opens the turn under the cursor.
/// The intermediate message shows and the result stays a one-line header.
#[test]
fn snap_turn_fold_l2_open_results_folded() {
    let (mut app, host, _tmp) = primed_browse_fold_app(fold_session_events());
    app.press(Key::Char('z'));
    app.press(Key::Char('o'));
    // Settle: apply the pending fold-cursor remap so the next move is
    // not clobbered by it.
    let _ = render_fold(&mut app, &host);
    // `G` to the tail: the folded result's "more lines" hint shows.
    app.press(Key::Char('G'));
    let out = render_fold(&mut app, &host);
    insta::assert_snapshot!(out);
}

/// `zA` toggles every tool-result fold in the cursor turn.
/// The cursor turn result expands while the other turn stays folded.
#[test]
fn snap_turn_fold_l3_open() {
    let (mut app, host, _tmp) = primed_browse_fold_app(fold_session_events());
    app.press(Key::Char('z'));
    app.press(Key::Char('o'));
    app.press(Key::Char('z'));
    app.press(Key::Char('A'));
    // Settle: apply the pending fold-cursor remap so the next move is
    // not clobbered by it.
    let _ = render_fold(&mut app, &host);
    // `G` to the tail: the expanded result body shows in full.
    app.press(Key::Char('G'));
    let out = render_fold(&mut app, &host);
    insta::assert_snapshot!(out);
}

/// A running loop renders the in-progress turn with the spinner line.
/// The time-dependent braille frame is masked for determinism.
#[test]
fn snap_turn_fold_in_progress_spinner() {
    let mut app = app_with_session(fold_session_events());
    app.set_viewport_height(24);
    let sid = app.active().cloned().unwrap();
    app.attach_external_loop(sid);
    app.browse().enter();
    let (host, _tmp) = empty_host();
    let out = render_fold(&mut app, &host);
    let out = regex::Regex::new(r"[\u{2800}-\u{28ff}]+")
        .unwrap()
        .replace_all(&out, "[SPINNER]")
        .into_owned();
    insta::assert_snapshot!(out);
}

/// The cursor sits on the second turn summary; `zM` remaps it to
/// the containing turn top (the remap the fold-state change owes).
#[test]
fn snap_turn_fold_cursor_remap() {
    let (mut app, host, _tmp) = primed_browse_fold_app(fold_session_events());
    app.press(Key::Char('j'));
    app.press(Key::Char('j'));
    app.press(Key::Char('j'));
    app.press(Key::Char('j'));
    app.press(Key::Char('j'));
    app.press(Key::Char('z'));
    app.press(Key::Char('M'));
    let out = render_fold(&mut app, &host);
    insta::assert_snapshot!(out);
}

// ── picker ──────────────────────────────────────────────────────────

#[test]
fn snap_picker_open() {
    let mut app = App::new();
    // Open the picker via the `@` token: type `@src` into the editor.
    app.editor_press(Key::Char('@'));
    app.editor_press(Key::Char('s'));
    app.editor_press(Key::Char('r'));
    app.editor_press(Key::Char('c'));
    // The picker opens automatically on typing `@`.
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

// ── palette ─────────────────────────────────────────────────────────

#[test]
fn snap_palette_open() {
    let mut app = App::new();
    app.open_palette();
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

/// The `tree` picker's option screen: the picked event's four outcome
/// options in place of the event list, with the option's help in the
/// preview pane (docs/tree-ui-design-from-human.md).
#[test]
fn snap_tree_options_screen() {
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"hello"}"#),
        ev(
            r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"hi","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":1,"output_tokens":1},"reasoning":{}}"#,
        ),
    ];
    let mut app = app_with_session(events);
    app.open_palette();
    app.palette_state_mut().goto_tree_options(1);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

// ── color schemes ───────────────────────────────────────────────────

#[test]
fn snap_palette_dark() {
    let events = vec![ev(
        r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"colored"}"#,
    )];
    let mut app = app_with_session(events);
    app.set_palette(Palette::builtin(Level::Rgb));
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_palette_light() {
    let events = vec![ev(
        r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"colored"}"#,
    )];
    let mut app = app_with_session(events);
    app.set_palette(Palette::builtin(Level::C16));
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

// ── tool display presets ────────────────────────────────────────────

#[test]
fn snap_tool_display_balanced() {
    let events = vec![
        ev(r#"{"v":1,"type":"tool_call","ts":"t","id":"c1","name":"bash","arguments":{"command":"make"}}"#),
        ev(
            r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"text":"line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nline11\nline12\n","exit_code":0},"is_error":false}"#,
        ),
    ];
    let mut app = app_with_session(events);
    app.set_tool_display(ToolDisplay::preset(Preset::Balanced));
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_tool_display_verbose() {
    let events = vec![
        ev(r#"{"v":1,"type":"tool_call","ts":"t","id":"c1","name":"bash","arguments":{"command":"make"}}"#),
        ev(
            r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"text":"line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nline11\nline12\n","exit_code":0},"is_error":false}"#,
        ),
    ];
    let mut app = app_with_session(events);
    app.set_tool_display(ToolDisplay::preset(Preset::Verbose));
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

// ── session naming ──────────────────────────────────────────────────

#[test]
fn snap_pending_name_input() {
    let mut app = App::new();
    app.start_naming();
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

// ── markdown rendering (ratatui-markdown) ─────────────────────────

#[test]
fn snap_markdown_headings_and_lists() {
    let md = "## Overview\n\n- First item\n- Second item\n\n1. Numbered\n2. Second";
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"show the plan"}"#),
        ev(&format!(
            r#"{{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":{},"tool_calls":[],"stop_reason":"stop","usage":{{"input_tokens":10,"output_tokens":5}},"reasoning":{{}}}}"#,
            serde_json::to_string(md).unwrap()
        )),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_markdown_code_block() {
    let md = "Here is some code:\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\nDone.";
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"show code"}"#),
        ev(&format!(
            r#"{{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":{},"tool_calls":[],"stop_reason":"stop","usage":{{"input_tokens":10,"output_tokens":5}},"reasoning":{{}}}}"#,
            serde_json::to_string(md).unwrap()
        )),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_markdown_table() {
    let md = "Some results:\n\n| Name  | Value |\n|-------|-------|\n| alpha | 1     |\n| beta  | 2     |\n| gamma | 3     |";
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"show table"}"#),
        ev(&format!(
            r#"{{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":{},"tool_calls":[],"stop_reason":"stop","usage":{{"input_tokens":10,"output_tokens":5}},"reasoning":{{}}}}"#,
            serde_json::to_string(md).unwrap()
        )),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

/// §4.1 bug 1 — `|` disambiguation: a `|`-prefixed line with no
/// following separator (e.g. a closure like `|x| y => x`) must NOT
/// be drawn as a grid table. The pipe line renders as plain prose and
/// the output carries no box-drawing borders.
#[test]
fn snap_table_pipe_not_a_table() {
    // A `|` line that is a table *row* but is NOT followed by the
    // GFM separator. Before the fix this was swallowed into a
    // spurious one-row grid; now it stays prose.
    let md = "Consider the rule\n|x| y => x\napplied inline.";
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"show pipes"}"#),
        ev(&format!(
            r#"{{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":{},"tool_calls":[],"stop_reason":"stop","usage":{{"input_tokens":10,"output_tokens":5}},"reasoning":{{}}}}"#,
            serde_json::to_string(md).unwrap()
        )),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    // The `|` line stays text — no spurious grid.
    assert!(out.contains("|x| y => x"), "pipe line must render as prose: {out}");
    assert!(!out.contains('┌'), "no grid border expected: {out}");
    assert!(!out.contains('└'), "no grid border expected: {out}");
    insta::assert_snapshot!(out);
}

/// §4.1 bug 2 — cell truncation: a table cell wider than its column
/// wraps onto multiple visual lines instead of being elided with a
/// trailing `…`. No data is lost.
#[test]
fn snap_table_wide_cell_wraps() {
    // The `Description` cell is wide enough to exceed the even-share
    // column width at 80 columns, so it wraps.
    let md = "Some results:\n\n| Name  | Description                          |\n|-------|------------------------------------|\n| alpha | This value is very long and should wrap to a second visual line in the grid |";
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"show wide table"}"#),
        ev(&format!(
            r#"{{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":{},"tool_calls":[],"stop_reason":"stop","usage":{{"input_tokens":10,"output_tokens":5}},"reasoning":{{}}}}"#,
            serde_json::to_string(md).unwrap()
        )),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    // The full cell content survives (wraps, no truncation).
    // The last words would be lost if the cell were truncated.
    for word in [
        "This", "value", "very", "long", "should", "wrap", "second", "visual", "line", "grid",
    ] {
        assert!(out.contains(word), "word `{word}` must survive: {out}");
    }
    assert!(
        !out.contains('…'),
        "no truncation ellipsis expected: {out}"
    );
    insta::assert_snapshot!(out);
}

#[test]
fn snap_markdown_inline() {
    let md = "Use **bold text** and *italic text* and `inline code` and [a link](https://example.com).";
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"show formatting"}"#),
        ev(&format!(
            r#"{{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":{},"tool_calls":[],"stop_reason":"stop","usage":{{"input_tokens":10,"output_tokens":5}},"reasoning":{{}}}}"#,
            serde_json::to_string(md).unwrap()
        )),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

#[test]
fn snap_markdown_blockquote() {
    let md = "A note:\n\n> This is a quoted block\n> spanning two lines.\n\nAfter the quote.";
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"show quote"}"#),
        ev(&format!(
            r#"{{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":{},"tool_calls":[],"stop_reason":"stop","usage":{{"input_tokens":10,"output_tokens":5}},"reasoning":{{}}}}"#,
            serde_json::to_string(md).unwrap()
        )),
    ];
    let mut app = app_with_session(events);
    let (host, _tmp) = empty_host();
    let out = render(&mut app, &host, 80, 24);
    insta::assert_snapshot!(out);
}

// ── §4.8 preview-pane wrapping ───────────────────────────────────────

/// A file line wider than the picker preview pane's inner width
/// wraps onto extra display rows instead of being clipped
/// (docs/tui-ratatui-ecosystem-audit.md §4.8). Drives the real
/// `render_picker` on a `TestBackend` at 100 columns: the
/// narrow-orientation pane is 58 cells wide, 56 inner.
#[test]
fn picker_preview_wide_line_wraps_not_truncates() {
    use crate::float::compute_float_layout;
    use crate::picker::fuzzy::Snapshot;
    use crate::picker::items::PickerItem;
    use crate::picker::preview::FilePreviewer;
    use crate::picker::render::render_picker;
    use crate::picker::state::PickerState;
    use ratatui::layout::Rect;

    // A 20-word line (79 chars) wider than the 56-cell pane.
    let words: Vec<String> = (0..20).map(|i| format!("w{i:02}")).collect();
    let line = words.join(" ");
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("long.txt");
    std::fs::write(&file, format!("{line}\n")).unwrap();

    let snap = Snapshot {
        items: vec![PickerItem {
            label: "long.txt".into(),
            value: "long.txt".into(),
            payload: file.to_str().unwrap().to_string(),
        }],
        query: "long".into(),
        settled: true,
    };
    let mut state = PickerState::new();
    state.open("long", 5);

    let previewer = FilePreviewer::new(50);
    let layout = compute_float_layout(Rect::new(0, 0, 100, 30), true);
    // Narrow orientation: the preview pane is 58 cells wide, 56
    // inner. The 79-char line cannot fit on one display row.
    assert!(layout.preview.is_some(), "the preview pane must show");

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).expect("test backend");
    let palette = Palette::builtin(Level::Rgb);
    let mut cursor = None;
    term.draw(|f| {
        render_picker()
            .f(f)
            .state(&mut state)
            .snapshot(&snap)
            .layout(&layout)
            .previewer(&previewer)
            .hints("hints")
            .palette(&palette)
            .cursor(&mut cursor)
            .call();
    })
    .unwrap();
    let out: String = term.backend().to_string();

    // Every word of the wide line survives: it wrapped onto a
    // second display row instead of being clipped at 56 cells.
    for w in &words {
        assert!(
            out.contains(w),
            "word `{w}` must be visible in the preview pane:\n{out}"
        );
    }
}

/// Same guarantee for the palette preview pane: a help line wider
/// than the pane wraps instead of being clipped.
#[test]
fn palette_preview_wide_help_line_wraps() {
    use crate::float::compute_float_layout;
    use crate::palette::items::{CmdKind, CmdOption, PaletteItem};
    use crate::palette::render::render_palette;
    use crate::palette::state::PaletteState;
    use ratatui::layout::Rect;

    let help: String = (0..20)
        .map(|i| format!("w{i:02}"))
        .collect::<Vec<_>>()
        .join(" ");
    let item = PaletteItem {
        id: "demo".into(),
        label: "demo".into(),
        kind: CmdKind::Set,
        hint: String::new(),
        help: help,
        options: vec![CmdOption {
            value: "a".into(),
            current: true,
        }],
        ext: None,
    };
    let items = vec![item];
    let mut state = PaletteState::new();
    state.open(5);

    let layout = compute_float_layout(Rect::new(0, 0, 100, 30), true);
    assert!(layout.preview.is_some(), "the preview pane must show");

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).expect("test backend");
    let palette = Palette::builtin(Level::Rgb);
    let mut cursor = None;
    term.draw(|f| {
        render_palette()
            .f(f)
            .state(&mut state)
            .items(&items)
            .layout(&layout)
            .palette(&palette)
            .cursor(&mut cursor)
            .call();
    })
    .unwrap();
    let out: String = term.backend().to_string();

    // The help line is 79 chars, the pane inner width is 56. Every
    // word must survive via wrapping, not clipping.
    for w in (0..20).map(|i| format!("w{i:02}")) {
        assert!(
            out.contains(&w),
            "word `{w}` must be visible in the preview pane:\n{out}"
        );
    }
}

// ── tree picker: view-only and rewind without summary ──────────────
// docs/tree-ui-design-from-human.md. These drive the palette state
// machine directly and assert the commit outcomes.

/// Drive the palette to the `TreeOptions` stage for a picked log seq,
/// with the cursor parked on the named outcome option (by index).
fn tree_options_at(events: Vec<Event>, seq: usize, opt: usize) -> App {
    let mut app = app_with_session(events);
    let st = app.palette_state_mut();
    st.open(10);
    st.goto_tree_options(seq);
    for _ in 0..opt {
        st.move_down(4, 0);
    }
    app
}

fn two_events() -> Vec<Event> {
    vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"hello"}"#),
        ev(r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"hi","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":1,"output_tokens":1},"reasoning":{}}"#),
    ]
}

/// View-only commit: the action is `TreeViewOnly` and the one-shot
/// scroll target is the in-memory index of the picked event (log seq 2
/// with base 1 → index 1). No marker is appended.
#[test]
fn tree_view_only_commit() {
    use crate::app::Action;
    let mut app = tree_options_at(two_events(), 2, 0);
    let actions = app.commit_palette();
    assert_eq!(actions, vec![Action::TreeViewOnly]);
    assert_eq!(app.take_view_only_target(), Some(1));
    // Committing closed the palette and cleared the tree seq.
    assert!(!app.palette_state().open);
    assert_eq!(app.palette_state().tree_seq(), None);
}

/// Rewind without summary on a user-message target uses `before` mode
/// and restores the message text to the input box, unsent.
#[test]
fn tree_rewind_before_user_message_restores() {
    use crate::app::Action;
    let mut events = two_events();
    events.push(ev(
        r#"{"v":1,"type":"user_message","ts":"t","id":"u2","content":"question"}"#,
    ));
    let mut app = tree_options_at(events, 3, 1);
    let actions = app.commit_palette();
    match actions.as_slice() {
        [Action::RewindNoSummary {
            target_seq,
            mode,
            restore_text,
        }] => {
            assert_eq!(*target_seq, 3);
            assert_eq!(mode.as_str(), "before");
            assert_eq!(restore_text.as_deref(), Some("question"));
        }
        other => panic!("unexpected actions {other:?}"),
    }
    // The fork also scrolls to the picked event (in-memory index 2).
    assert_eq!(app.take_view_only_target(), Some(2));
}

/// Rewind without summary on a non-user target uses `on` mode and
/// restores no text.
#[test]
fn tree_rewind_on_tool_result_no_restore() {
    use crate::app::Action;
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"hi"}"#),
        ev(r#"{"v":1,"type":"tool_call","ts":"t","id":"c1","name":"bash","arguments":{"command":"ls"}}"#),
        ev(r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":"ok"}"#),
    ];
    let mut app = tree_options_at(events, 3, 1);
    let actions = app.commit_palette();
    match actions.as_slice() {
        [Action::RewindNoSummary {
            target_seq,
            mode,
            restore_text,
        }] => {
            assert_eq!(*target_seq, 3);
            assert_eq!(mode.as_str(), "on");
            assert!(restore_text.is_none());
        }
        other => panic!("unexpected actions {other:?}"),
    }
    // The fork also scrolls to the picked event (in-memory index 2).
    assert_eq!(app.take_view_only_target(), Some(2));
}

/// A busy loop blocks the fork outcomes: committing Rewind without
/// summary while a loop runs keeps the options open and flashes a hint.
#[test]
fn tree_rewind_blocked_while_loop_runs() {
    let events = two_events();
    let mut app = tree_options_at(events, 2, 1);
    let sid = app.active().cloned().unwrap();
    app.attach_external_loop(sid);
    let actions = app.commit_palette();
    assert!(actions.is_empty(), "busy loop yields no port action: {actions:?}");
    assert!(app.status().is_some(), "the busy hint must flash");
    assert!(
        app.palette_state().open,
        "the options must stay open while the loop runs"
    );
}

/// The two summarize options are shown but not wired yet: committing
/// one keeps the options open and flashes the pending-kernel hint.
#[test]
fn tree_summarize_options_flash_pending() {
    let events = two_events();
    let mut app = tree_options_at(events, 1, 2);
    let actions = app.commit_palette();
    assert!(
        actions.is_empty(),
        "summarize options yield no port action: {actions:?}"
    );
    assert!(
        app.status().is_some(),
        "the pending-kernel hint must flash"
    );
    assert!(
        app.palette_state().open,
        "the options must stay open for the fallback pick"
    );
}

/// `transcript_event_line_starts` maps each in-memory event to its first
/// rendered transcript line; out-of-window and suppressed events are
/// `None`.
#[test]
fn transcript_event_line_starts_maps() {
    use crate::render::build_transcript;
    let events = two_events();
    let app = app_with_session(events);
    let build = build_transcript(&app, 80, None);
    let starts = &build.event_line_starts;
    assert_eq!(starts.len(), 2);
    assert!(starts[0].is_some(), "event 0 must be rendered");
    assert!(starts[1].is_some(), "event 1 must be rendered");
    // Event 0 starts at line 0; event 1 starts after event 0 plus a
    // separator, strictly after event 0's first line.
    assert_eq!(starts[0], Some(0));
    assert!(starts[1] > Some(0));
}

/// A rewind marker masks the abandoned branch: events outside the
/// active-path ranges render dim; active-path events do not.
#[test]
fn rewind_marker_masks_abandoned_branch() {
    use crate::render::build_transcript;
    use ratatui::style::Modifier;
    let events = vec![
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"hello"}"#),
        ev(r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"hi","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":1,"output_tokens":1},"reasoning":{}}"#),
        ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u2","content":"question"}"#),
        ev(r#"{"v":1,"type":"rewind","ts":"t","id":"w1","target_seq":2,"mode":"on","reason":"tui_pick"}"#),
    ];
    let app = app_with_session(events);
    let build = build_transcript(&app, 80, None);
    let dim = |li: usize| build.lines[li]
        .spans
        .iter()
        .any(|s| s.style.add_modifier.contains(Modifier::DIM));
    // Events 0 and 1 are on the active path (seqs 1..=2): not dimmed.
    for idx in 0..2 {
        let li = build.event_line_starts[idx].expect("active-path event rendered");
        assert!(!dim(li), "active-path event {idx} must not be dimmed");
    }
    // Event 2 (seq 3, the abandoned "question") is off the active path.
    let li = build.event_line_starts[2].expect("abandoned event rendered");
    assert!(dim(li), "the abandoned event must be dimmed");
}
