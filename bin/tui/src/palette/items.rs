//! Palette item types, built-in command registration, and the
//! extension-command conversion (docs/tui-command-palette.md sections 5, 6, 10, 11).

use crate::ext::ExtCommand;

/// The command kind. Drives the preview-pane content and the
/// commit behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdKind {
    /// Fire one `Action`. No arguments. Enter executes and closes.
    Run,
    /// A value with a choice. The preview pane lists options; the
    /// user selects one and Enter applies it.
    Set,
    /// Open a sub-list. The query keeps feeding the sub-list filter.
    Goto,
    /// An extension-owned command. Enter sends an `invoke` op.
    Ext,
}

/// The preview-pane content pipeline for a palette item
/// (docs/tree-ui-design-from-human-phase-2.md item 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewKind {
    /// Help text, one plain line per row. The default for `Run` /
    /// `Set` / `Goto` / `Ext` items and the tree option list.
    #[default]
    Plain,
    /// `help` is raw JSON source. The tree-pane pipeline parses it
    /// with `jaq-json` and pretty-prints it. Then it highlights the
    /// text with the JSON pass. A parse failure shows the raw text.
    /// It never crashes.
    Json,
    /// `help` is markdown source. The tree-pane pipeline highlights
    /// it with the tree-sitter markdown pass.
    Markdown,
    /// Tool-call / tool-result tree events. The pane renders the
    /// decoded payload in `tool_payload` through the transcript's
    /// tool-result display (`crate::tool_display::body_rows`), so
    /// result text shows as real lines instead of escaped JSON
    /// (docs/tree-ui-design-from-human-phase-2.md item 2,
    /// 2026-07-09 refinement). `tool_payload` is always `Some` for
    /// this kind.
    Tool,
}

/// One option of a `Set` or extension setting item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOption {
    pub value: String,
    /// Whether this is the currently active value (marked in the
    /// preview pane).
    pub current: bool,
}

/// The decoded payload of one tool tree event
/// (docs/tree-ui-design-from-human-phase-2.md item 2, 2026-07-09
/// refinement). The pane renders it through the transcript's
/// tool-result display instead of re-serializing compact JSON, so
/// string fields show as real lines instead of `\n`-escaped text.
///
/// A `tool_call` event carries `is_call = true`, its own
/// `call_args`, and a `value` of `Value::Null`. A `tool_result`
/// event carries `is_call = false`, the result `value`, and the
/// call arguments resolved through the call `id`
/// (`App::call_details`) so the read / write / edit bodies can
/// reach their `content` / `old_string` / `new_string` sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPayload {
    /// The tool name. For results it resolves from the call `id`;
    /// an unresolvable id falls back to `"tool"`.
    pub name: String,
    /// Whether the source event is a `tool_call` (no result yet).
    pub is_call: bool,
    /// The result value. `Value::Null` for calls.
    pub value: serde_json::Value,
    /// The call arguments, resolved for results through the call
    /// `id`. `Value::Null` when the call is absent from the log.
    pub call_args: serde_json::Value,
    /// The result `is_error` flag. Always `false` for calls.
    pub err: bool,
}

/// One palette entry.
#[derive(Debug, Clone, PartialEq)]
pub struct PaletteItem {
    /// Stable key, e.g. `"thinking-level"`, `"b"`, `"myext.reload"`.
    pub id: String,
    /// Display label shown in the list.
    pub label: String,
    pub kind: CmdKind,
    /// A short hint: the keybinding, the current value, or empty.
    pub hint: String,
    /// Help text shown in the preview pane.
    pub help: String,
    /// Non-empty only for `Set` items and extension settings.
    pub options: Vec<CmdOption>,
    /// The extension name that owns this item, or `None` for built-in.
    pub ext: Option<String>,
    /// The preview-pane content pipeline (docs/tree-ui-design-from-
    /// human-phase-2.md item 2). `Plain` for every non-tree item.
    pub preview_kind: PreviewKind,
    /// The fg color role of the row's leading type tag, set for the
    /// tree event rows (docs/tree-ui-design-from-human-phase-2.md
    /// item 1). The list renderer colors the first label token with
    /// it. `None` for every other row kind.
    pub tag_fg: Option<crate::color::Role>,
    /// The decoded tool payload, set only on tree tool events
    /// (`PreviewKind::Tool`). `None` everywhere else.
    pub tool_payload: Option<ToolPayload>,
}

/// The thinking-level values in cycle order
/// (docs/tui-thinking-block.md section 4). The palette
/// `thinking-level` item offers these as options; `Ctrl+L` walks the
/// same order.
pub const THINKING_LEVEL_ORDER: &[&str] =
    &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The built-in palette commands (docs/tui-command-palette.md section 6).
///
/// `current_effort` is the active model's current thinking level,
/// used to mark the `thinking-level` item's current option.
pub fn builtins(current_effort: &str) -> Vec<PaletteItem> {
    let effort_opts: Vec<CmdOption> = THINKING_LEVEL_ORDER
        .iter()
        .map(|v| CmdOption {
            value: (*v).to_string(),
            current: v.eq_ignore_ascii_case(current_effort),
        })
        .collect();

    vec![
        PaletteItem {
            id: "toggle-tools".into(),
            label: "toggle-tools".into(),
            kind: CmdKind::Run,
            hint: "Ctrl+O".into(),
            help: "Toggle tool-result fold/expand (all blocks).".into(),
            options: Vec::new(),
            ext: None,
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        },
        PaletteItem {
            id: "toggle-thinking".into(),
            label: "toggle-thinking".into(),
            kind: CmdKind::Run,
            hint: "Ctrl+X".into(),
            help: "Show or hide thinking blocks.".into(),
            options: Vec::new(),
            ext: None,
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        },
        PaletteItem {
            id: "expand-thinking".into(),
            label: "expand-thinking".into(),
            kind: CmdKind::Run,
            hint: "Ctrl+T".into(),
            help: "Collapse or expand thinking blocks.".into(),
            options: Vec::new(),
            ext: None,
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        },
        PaletteItem {
            id: "thinking-level".into(),
            label: "thinking level".into(),
            kind: CmdKind::Set,
            hint: current_effort.to_string(),
            help: format!(
                "Set the thinking level for the active model. Current: {current_effort}."
            ),
            options: effort_opts,
            ext: None,
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        },
        PaletteItem {
            id: "b".into(),
            label: "b".into(),
            kind: CmdKind::Goto,
            hint: String::new(),
            help: "Open the session buffer list. Type to filter.".into(),
            options: Vec::new(),
            ext: None,
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        },
        PaletteItem {
            id: "tree".into(),
            label: "tree".into(),
            kind: CmdKind::Goto,
            hint: String::new(),
            help: "Open the session log tree. Type to filter events. Enter picks one and offers 4 options (docs/tree-ui-design-from-human.md).".into(),
            options: Vec::new(),
            ext: None,
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        },
        PaletteItem {
            id: "bn".into(),
            label: "bn".into(),
            kind: CmdKind::Run,
            hint: String::new(),
            help: "Cycle to the next session.".into(),
            options: Vec::new(),
            ext: None,
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        },
        PaletteItem {
            id: "bp".into(),
            label: "bp".into(),
            kind: CmdKind::Run,
            hint: String::new(),
            help: "Cycle to the previous session.".into(),
            options: Vec::new(),
            ext: None,
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        },
        PaletteItem {
            id: "new-session".into(),
            label: "new-session".into(),
            kind: CmdKind::Run,
            hint: String::new(),
            help: "Start a new session. Opens the name input.".into(),
            options: Vec::new(),
            ext: None,
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        },
        PaletteItem {
            id: "edit-queue".into(),
            label: "edit-queue".into(),
            kind: CmdKind::Run,
            hint: String::new(),
            help: "Recall all pending user messages into the editor for editing.".into(),
            options: Vec::new(),
            ext: None,
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        },
        PaletteItem {
            id: "e".into(),
            label: "e".into(),
            kind: CmdKind::Run,
            hint: "Ctrl+E".into(),
            help: "Open the external editor on the draft.".into(),
            options: Vec::new(),
            ext: None,
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        },
        PaletteItem {
            id: "q".into(),
            label: "q".into(),
            kind: CmdKind::Run,
            hint: String::new(),
            help: "Quit the TUI. Loops keep running.".into(),
            options: Vec::new(),
            ext: None,
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        },
    ]
}

/// Convert an extension's raw commands into palette items.
///
/// Each `ExtCommand` becomes a `PaletteItem` with `kind = Ext` and
/// `ext` set to the owning extension name. Settings (commands with
/// `options`) get non-empty `options` so the preview pane shows the
/// picker.
pub fn from_extension(ext_name: &str, cmds: &[ExtCommand]) -> Vec<PaletteItem> {
    cmds.iter()
        .map(|c| PaletteItem {
            id: format!("{}.{}", ext_name, c.id),
            label: c.label.clone(),
            kind: CmdKind::Ext,
            hint: c.hint.clone(),
            help: c.help.clone(),
            options: if c.options.is_empty() {
                Vec::new()
            } else {
                c.options
                    .iter()
                    .map(|o| CmdOption {
                        value: o.clone(),
                        current: false,
                    })
                    .collect()
            },
            ext: Some(ext_name.to_string()),
            preview_kind: PreviewKind::Plain,
            tag_fg: None,
            tool_payload: None,
        })
        .collect()
}
