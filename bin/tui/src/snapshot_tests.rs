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
        schemas_dir: None,
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
    app.set_active(SessionId::new("s1"), events);
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
    let mut app = app_with_session(events);
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
