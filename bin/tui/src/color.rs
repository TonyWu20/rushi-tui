//! Terminal color capability: detection and palette lowering.
//!
//! The TUI is built around the native 16 ANSI colors (`Color::Green`,
//! `Color::Cyan`, ...). crossterm emits those as 256-color SGR indices
//! (`38;5;0..15`), which is safe on every 16/256/truecolor terminal and
//! resolves to the ANSI swatches on each.
//!
//! Free-form RGB only appears from ui-extension hex colors
//! (`"style": {"fg": "#c3e88d"}` — docs/ui-extension.md wire color
//! grammar). Those must be lowered to what the terminal can actually
//! show:
//!
//! - `Rgb` — emit 24-bit SGR (`38;2;r;g;b`); needs a truecolor terminal.
//! - `C256` — quantize to the 6x6x6 cube + grayscale ramp
//!   (`38;5;N`).
//! - `C16` — snap to the 16 ANSI swatches (`38;5;0..15`).
//!
//! Default is `Rgb` (truecolor on). `Level::detect()` falls back to
//! what the terminal environment says: `COLORTERM=truecolor|24bit`
//! forces `Rgb`; a `TERM` advertising 256 colors (`*256*`) drops to
//! `C256`; a plain-16 TERM (`xterm`, `vt100`, `linux`, `ansi`,
//! `screen`, `tmux`, `dumb`) drops to `C16`; anything else (alacritty,
//! kitty, `xterm-256color` absent) keeps the `Rgb` default. The
//! harness config `[tui] color` overrides the detection (the user
//! knows their terminal).
//!
//! The built-in palette and the `catppuccin macchiato` scheme are the
//! element colors of the `pi` TUI (docs/tui-color-pi-alignment.md):
//! the no-scheme palette mirrors pi's built-in `dark` theme, and the
//! scheme mirrors the pi `catppuccin-macchiato` theme JSON
//! (`~/.pi/agent/themes` lookup). One color role per pi theme token
//! the TUI paints, so a scheme table can carry the pi theme values
//! verbatim.

use ratatui::style::{Color, Modifier, Style};

/// The terminal's color capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    /// 24-bit truecolor: RGB colors emit as-is (`38;2`).
    Rgb,
    /// 256-color: RGB colors quantize to the 256 palette.
    C256,
    /// 16-color: RGB colors snap to the 16 ANSI swatches.
    C16,
}

impl Level {
    /// The default: truecolor on.
    pub const DEFAULT: Level = Level::Rgb;

    /// Detect from the environment: `COLORTERM` first, then `TERM`
    /// (see the module docs).
    pub fn detect() -> Level {
        Self::detect_with(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }

    /// The pure decision table (env var access split out for tests).
    fn detect_with(colorterm: Option<&str>, term: Option<&str>) -> Level {
        let colorterm_lc = colorterm.map(|s| s.to_ascii_lowercase());
        if matches!(colorterm_lc.as_deref(), Some("truecolor") | Some("24bit")) {
            return Level::Rgb;
        }
        let term = term.unwrap_or_default().to_ascii_lowercase();
        if term.contains("256") {
            return Level::C256;
        }
        const PLAIN16: [&str; 8] = [
            "xterm", "vt100", "vt102", "linux", "ansi", "screen", "tmux", "dumb",
        ];
        if PLAIN16.contains(&term.as_str()) {
            return Level::C16;
        }
        Self::DEFAULT
    }

    /// The harness config override (`[tui] color = "truecolor"`).
    pub fn from_cfg(v: &str) -> Option<Level> {
        Some(match v.to_ascii_lowercase().as_str() {
            "rgb" | "truecolor" | "24bit" | "24-bit" => Level::Rgb,
            "256" | "256color" | "256-color" => Level::C256,
            "16" | "8" | "16color" | "8color" | "16-color" | "8-color" => Level::C16,
            _ => return None,
        })
    }

    /// The wire name the status row advertises: what the TUI is
    /// actually emitting.
    pub fn name(self) -> &'static str {
        match self {
            Level::Rgb => "truecolor",
            Level::C256 => "256",
            Level::C16 => "16",
        }
    }

    /// The color for unstyled transcript prose (model messages, user
    /// message bodies, the input draft). The pi built-in `dark` theme
    /// text var (`#d4d4d4`): 16-color snaps to the `Gray` swatch
    /// (xterm 7, `#c0c0c0`).
    pub fn plain_text(self) -> Color {
        match self {
            Level::Rgb => Color::Rgb(0xd4, 0xd4, 0xd4),
            Level::C256 => lower(Color::Rgb(0xd4, 0xd4, 0xd4), Level::C256),
            Level::C16 => Color::Gray,
        }
    }

    /// The color for tool/command output (bash and tool-result bodies).
    /// The pi `toolOutput` role (`#808080` gray in the built-in `dark`
    /// theme): distinct from the prose tone, the 16-color `DarkGray`
    /// swatch exactly.
    pub fn tool_output(self) -> Color {
        match self {
            Level::Rgb => Color::Rgb(0x80, 0x80, 0x80),
            Level::C256 => lower(Color::Rgb(0x80, 0x80, 0x80), Level::C256),
            Level::C16 => Color::DarkGray,
        }
    }

    /// The color for the *command* text of a tool call — what was
    /// invoked (a bash line, a tool name + args). The pi `toolTitle`
    /// role: the `dark` theme text var (`#d4d4d4`), the same tone as
    /// the prose. The `catppuccin macchiato` scheme lifts it to the
    /// theme `mauve`, where title and prose read distinct.
    pub fn tool_command(self) -> Color {
        match self {
            Level::Rgb => Color::Rgb(0xd4, 0xd4, 0xd4),
            Level::C256 => lower(Color::Rgb(0xd4, 0xd4, 0xd4), Level::C256),
            Level::C16 => Color::Gray,
        }
    }

    /// The color for the model's thinking (reasoning) block. The pi
    /// `thinkingText` role: the `dark` theme gray (`#808080`), the
    /// `catppuccin macchiato` scheme the theme `subtext1` (`#b8c0e0`).
    pub fn thinking(self) -> Color {
        match self {
            Level::Rgb => Color::Rgb(0x80, 0x80, 0x80),
            Level::C256 => lower(Color::Rgb(0x80, 0x80, 0x80), Level::C256),
            Level::C16 => Color::DarkGray,
        }
    }
}

/// Lower one color to `level`.
///
/// Native named colors (the 16 ANSI swatches) pass through unchanged:
/// crossterm emits them as `38;5;0..15`-style SGR, which is valid at
/// every level and resolves to the ANSI swatches. Only free-form RGB
/// needs work, and only downward.
pub fn lower(c: Color, level: Level) -> Color {
    match (c, level) {
        (Color::Rgb(..), Level::Rgb) => c,
        (Color::Rgb(r, g, b), Level::C256) => Color::Indexed(nearest_256(r, g, b)),
        (Color::Rgb(r, g, b), Level::C16) => nearest_16(r, g, b),
        _ => c,
    }
}

/// Lower the colors of a whole style (fg + bg); modifiers stay.
pub fn lower_style(s: Style, level: Level) -> Style {
    Style {
        fg: s.fg.map(|c| lower(c, level)),
        bg: s.bg.map(|c| lower(c, level)),
        ..s
    }
}

// ── color schemes (docs/tui-color-scheme.md) ──────────────────

/// One color role the TUI paints. Each role is one `pi` theme token
/// the TUI element consumes (docs/tui-color-pi-alignment.md):
/// the built-in tones, the markdown and syntax highlight styles
/// (the pi `syntax*` token set, which the JSON token walk colors
/// through, like pi's highlight.js JSON scope mapping), the
/// thinking-level border palette, the status row, the diff accents,
/// the warning accent, and the tool-result box backgrounds (one per
/// pi box state).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// Transcript prose (model messages, user message bodies, the
    /// input draft). pi `text` / `userMessageText`.
    PlainText,
    /// Tool and command output bodies. pi `toolOutput`.
    ToolOutput,
    /// The command text of a tool call. pi `toolTitle`.
    ToolCommand,
    /// The pi accent: the `user` message label, the tool-call path
    /// and the list bullets of the `catppuccin macchiato` theme.
    /// pi `accent`.
    Accent,
    /// Fenced-code content (the known-language fallback tone). pi
    /// `mdCodeBlock`.
    Code,
    /// A ``` / ~~~ fence marker line. pi `mdCodeBlockBorder`.
    Fence,
    /// A markdown heading line (the marker run drops; the style
    /// stays). pi `mdHeading`.
    Heading,
    /// A blockquote line. pi `mdQuote`.
    Quote,
    /// A list marker (`-`, `1.`). pi `mdListBullet`.
    List,
    /// An inline `code` span (the backticks drop; the style stays).
    /// pi `mdCode`.
    InlineCode,
    /// The text of a markdown link. pi `mdLink`.
    Link,
    /// The URL of a markdown link (dimmed). pi `mdLinkUrl`.
    LinkUrl,
    /// A comment token. pi `syntaxComment`.
    SyntaxComment,
    /// A keyword token. pi `syntaxKeyword`.
    SyntaxKeyword,
    /// A function-call token. pi `syntaxFunction`.
    SyntaxFunction,
    /// A variable token; the JSON object key colors through it (the
    /// pi highlight.js `attr` scope mapping). pi `syntaxVariable`.
    SyntaxVariable,
    /// A string token; the JSON string value colors through it. pi
    /// `syntaxString`.
    SyntaxString,
    /// A number token; the JSON number and the `true` / `false` /
    /// `null` literals color through it (the pi `literal` scope
    /// mapping). pi `syntaxNumber`.
    SyntaxNumber,
    /// A type token. pi `syntaxType`.
    SyntaxType,
    /// An operator token. pi `syntaxOperator`.
    SyntaxOperator,
    /// Structural punctuation token; the JSON braces, colons, commas
    /// and brackets color through it. pi `syntaxPunctuation`.
    SyntaxPunctuation,
    /// The model thinking block. pi `thinkingText`.
    Thinking,
    /// Fold/expand hints and other muted text (`… +N more lines`).
    /// pi `muted`.
    Hint,
    /// The built-in status/help row and the last-loop-line row. pi
    /// `dim`.
    Status,
    /// Error accents (error status, failed lines). pi `error`.
    Error,
    /// Success accents (allow decisions, added diff lines). pi
    /// `success`.
    Success,
    /// Warning accents (truncation marks, the approval banner). pi
    /// `warning`.
    Warning,
    /// An added diff line. pi `toolDiffAdded`.
    DiffAdded,
    /// A removed diff line. pi `toolDiffRemoved`.
    DiffRemoved,
    /// A diff context line. pi `toolDiffContext`.
    DiffContext,
    /// The background shade of an added diff line (docs/tui-tool-
    /// display-fancy.md section 7): a dark green tint that spans the
    /// whole line so the syntax colors of the text stay readable.
    DiffAddedBg,
    /// The background shade of a removed diff line: a dark red tint
    /// that spans the whole line.
    DiffRemovedBg,
    /// The input-area border of thinking level 0 (no thinking). pi
    /// `thinkingOff`.
    Border0,
    /// The input-area border of thinking level 1 (low). pi
    /// `thinkingLow`.
    Border1,
    /// The input-area border of thinking level 2 (medium). pi
    /// `thinkingMedium`.
    Border2,
    /// The input-area border of thinking level 3 (high). pi
    /// `thinkingHigh`.
    Border3,
    /// The input-area border of thinking level 4+ (highest). pi
    /// `thinkingXhigh`.
    Border4,
    /// The background of a running (pending) tool-result box. pi
    /// `toolPendingBg`.
    ToolBoxBg,
    /// The light background of a successful tool-result box. pi
    /// `toolSuccessBg`.
    ToolBoxBgSuccess,
    /// The light background of a failed tool-result box. pi
    /// `toolErrorBg`.
    ToolBoxBgError,
    /// The browse-mode visual selection shading (docs/tui-
    /// conversation-browsing.md section 11.4): a background tone
    /// distinct from the search-highlight tone (section 7.3).
    Selection,
    /// The browse-mode cursorline background (section 4.1): a dark
    /// shade so the cursor row is visible without overpowering the
    /// selection tone.
    CursorLine,
}

impl Role {
    /// Every role, in declaration order. A scheme may leave any role
    /// unset: the unset roles keep the built-in palette value.
    pub const ALL: &[Role] = &[
        Role::PlainText,
        Role::ToolOutput,
        Role::ToolCommand,
        Role::Accent,
        Role::Code,
        Role::Fence,
        Role::Heading,
        Role::Quote,
        Role::List,
        Role::InlineCode,
        Role::Link,
        Role::LinkUrl,
        Role::SyntaxComment,
        Role::SyntaxKeyword,
        Role::SyntaxFunction,
        Role::SyntaxVariable,
        Role::SyntaxString,
        Role::SyntaxNumber,
        Role::SyntaxType,
        Role::SyntaxOperator,
        Role::SyntaxPunctuation,
        Role::Thinking,
        Role::Hint,
        Role::Status,
        Role::Error,
        Role::Success,
        Role::Warning,
        Role::DiffAdded,
        Role::DiffRemoved,
        Role::DiffContext,
        Role::DiffAddedBg,
        Role::DiffRemovedBg,
        Role::Border0,
        Role::Border1,
        Role::Border2,
        Role::Border3,
        Role::Border4,
        Role::ToolBoxBg,
        Role::ToolBoxBgSuccess,
        Role::ToolBoxBgError,
        Role::Selection,
        Role::CursorLine,
    ];

    /// The built-in value of the role at every capability level. The
    /// default palette (no scheme selected) mirrors the pi built-in
    /// `dark` theme element colors (docs/tui-color-pi-alignment.md):
    /// every role is a truecolor target, lowered to the active
    /// capability level (quantized at 256, snapped at 16).
    pub fn builtin(self, level: Level) -> Color {
        use Role::*;
        let c = match self {
            PlainText => level.plain_text(),
            ToolOutput => level.tool_output(),
            ToolCommand => level.tool_command(),
            Accent => Color::Rgb(0x8a, 0xbe, 0xb7),
            Code => Color::Rgb(0xb5, 0xbd, 0x68),
            Fence => Color::Rgb(0x80, 0x80, 0x80),
            Heading => Color::Rgb(0xf0, 0xc6, 0x74),
            Quote => Color::Rgb(0x80, 0x80, 0x80),
            List | InlineCode => Color::Rgb(0x8a, 0xbe, 0xb7),
            Link => Color::Rgb(0x81, 0xa2, 0xbe),
            LinkUrl => Color::Rgb(0x66, 0x66, 0x66),
            SyntaxComment => Color::Rgb(0x6a, 0x99, 0x55),
            SyntaxKeyword => Color::Rgb(0x56, 0x9c, 0xd6),
            SyntaxFunction => Color::Rgb(0xdc, 0xdc, 0xaa),
            SyntaxVariable => Color::Rgb(0x9c, 0xdc, 0xfe),
            SyntaxString => Color::Rgb(0xce, 0x91, 0x78),
            SyntaxNumber => Color::Rgb(0xb5, 0xce, 0xa8),
            SyntaxType => Color::Rgb(0x4e, 0xc9, 0xb0),
            SyntaxOperator => Color::Rgb(0xd4, 0xd4, 0xd4),
            SyntaxPunctuation => Color::Rgb(0xd4, 0xd4, 0xd4),
            Thinking => level.thinking(),
            Hint => Color::Rgb(0x80, 0x80, 0x80),
            Status => Color::Rgb(0x66, 0x66, 0x66),
            Error | DiffRemoved => Color::Rgb(0xcc, 0x66, 0x66),
            Success | DiffAdded => Color::Rgb(0xb5, 0xbd, 0x68),
            Warning => Color::Rgb(0xff, 0xff, 0x00),
            DiffContext => Color::Rgb(0x80, 0x80, 0x80),
            // The diff-line background shades: a dark green tint for
            // added lines and a dark red tint for removed lines. They
            // are deliberately darker than the accent `DiffAdded` /
            // `DiffRemoved` foregrounds so the syntax colors of the
            // text stay readable on top of the tint.
            DiffAddedBg => Color::Rgb(0x1e, 0x33, 0x2a),
            DiffRemovedBg => Color::Rgb(0x3a, 0x20, 0x26),
            Border0 => Color::Rgb(0x50, 0x50, 0x50),
            Border1 => Color::Rgb(0x5f, 0x87, 0xaf),
            Border2 => Color::Rgb(0x81, 0xa2, 0xbe),
            Border3 => Color::Rgb(0xb2, 0x94, 0xbb),
            Border4 => Color::Rgb(0xd1, 0x83, 0xe8),
            ToolBoxBg => Color::Rgb(0x28, 0x28, 0x32),
            ToolBoxBgSuccess => Color::Rgb(0x28, 0x32, 0x28),
            ToolBoxBgError => Color::Rgb(0x3c, 0x28, 0x28),
            // The cursorline background: a dark shade so the cursor
            // row is visible without overpowering text.
            CursorLine => Color::Rgb(0x1e, 0x20, 0x30),
            // The selection background: a muted blue-gray, distinct
            // from the search highlight (the `Hint` bold tone) so a
            // selected span that also matches a search reads as two
            // layers.
            // A blue selection tone (distinct from the `Hint`
            // search-highlight gray: lowers to Blue vs DarkGray at C16).
            Selection => Color::Rgb(0x36, 0x45, 0x73),
        };
        lower(c, level)
    }

    /// The wire name of the role in the config scheme table
    /// (`[tui] color_schemes.<name>`).
    pub fn key(self) -> &'static str {
        use Role::*;
        match self {
            PlainText => "plain_text",
            ToolOutput => "tool_output",
            ToolCommand => "tool_command",
            Accent => "accent",
            Code => "code",
            Fence => "fence",
            Heading => "heading",
            Quote => "quote",
            List => "list",
            InlineCode => "inline_code",
            Link => "link",
            LinkUrl => "link_url",
            SyntaxComment => "syntax_comment",
            SyntaxKeyword => "syntax_keyword",
            SyntaxFunction => "syntax_function",
            SyntaxVariable => "syntax_variable",
            SyntaxString => "syntax_string",
            SyntaxNumber => "syntax_number",
            SyntaxType => "syntax_type",
            SyntaxOperator => "syntax_operator",
            SyntaxPunctuation => "syntax_punctuation",
            Thinking => "thinking",
            Hint => "hint",
            Status => "status",
            Error => "error",
            Success => "success",
            Warning => "warning",
            DiffAdded => "diff_added",
            DiffRemoved => "diff_removed",
            DiffContext => "diff_context",
            DiffAddedBg => "diff_added_bg",
            DiffRemovedBg => "diff_removed_bg",
            Border0 => "border0",
            Border1 => "border1",
            Border2 => "border2",
            Border3 => "border3",
            Border4 => "border4",
            ToolBoxBg => "tool_box_bg",
            ToolBoxBgSuccess => "tool_box_bg_success",
            ToolBoxBgError => "tool_box_bg_error",
            Selection => "selection",
            CursorLine => "cursor_line",
        }
    }

    /// Look up a role by its wire key, accepting both snake_case
    /// (`cursor_line`) and PascalCase (`CursorLine`) forms.
    pub fn from_key(k: &str) -> Option<Role> {
        let norm = |s: &str| -> String {
            s.to_lowercase().replace('_', "")
        };
        let nk = norm(k);
        Role::ALL.iter().find(|r| norm(r.key()) == nk).copied()
    }
}

/// Parse one hex color value (`#rgb` or `#rrggbb`) of a scheme
/// table. The same grammar as the extension hex wire colors
/// (ext.rs `parse_color`); a bad value is a hard error at load, like
/// the `[tui] color` level.
pub fn parse_scheme_color(v: &str) -> Option<Color> {
    let hex = v.trim().strip_prefix('#')?;
    if !hex.is_ascii() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |s: &str| u8::from_str_radix(s, 16).ok();
    match hex.len() {
        3 => {
            let r = byte(&hex[0..1])? * 17;
            let g = byte(&hex[1..2])? * 17;
            let b = byte(&hex[2..3])? * 17;
            Some(Color::Rgb(r, g, b))
        }
        6 => {
            let r = byte(&hex[0..2])?;
            let g = byte(&hex[2..4])?;
            let b = byte(&hex[4..6])?;
            Some(Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

/// The built-in named scheme. The first internal scheme is
/// `catppuccin macchiato` (docs/tui-color-scheme.md section 3):
/// every role maps to a Macchiato hex value. The values lower to
/// the active capability level, like the extension hex wire colors.
pub const SCHEME_CATPPUCCIN_MACCHIATO: &str = "catppuccin macchiato";

/// The role-to-hex table of the `catppuccin macchiato` scheme:
/// the pi `catppuccin-macchiato` theme JSON values, role by role
/// (docs/tui-color-pi-alignment.md). A scheme maps every color
/// role to a hex value; the TUI ships this one as the first named
/// internal scheme.
pub fn catppuccin_macchiato() -> std::collections::HashMap<Role, &'static str> {
    use Role::*;
    let pairs: [(Role, &str); 42] = [
        (PlainText, "#cad3f5"),
        (ToolOutput, "#cad3f5"),
        (ToolCommand, "#c6a0f6"),
        (Accent, "#c6a0f6"),
        (Code, "#cad3f5"),
        (Fence, "#5b6078"),
        (Heading, "#f5bde6"),
        (Quote, "#a5adcb"),
        (List, "#c6a0f6"),
        (InlineCode, "#91d7e3"),
        (Link, "#8aadf4"),
        (LinkUrl, "#a5adcb"),
        (SyntaxComment, "#8087a2"),
        (SyntaxKeyword, "#c6a0f6"),
        (SyntaxFunction, "#8aadf4"),
        (SyntaxVariable, "#cad3f5"),
        (SyntaxString, "#a6da95"),
        (SyntaxNumber, "#f5a97f"),
        (SyntaxType, "#eed49f"),
        (SyntaxOperator, "#91d7e3"),
        (SyntaxPunctuation, "#939ab7"),
        (Thinking, "#b8c0e0"),
        (Hint, "#a5adcb"),
        (Status, "#8087a2"),
        (Error, "#ed8796"),
        (Success, "#a6da95"),
        (Warning, "#eed49f"),
        (DiffAdded, "#a6da95"),
        (DiffRemoved, "#ed8796"),
        (DiffContext, "#a5adcb"),
        (DiffAddedBg, "#26402f"),
        (DiffRemovedBg, "#3d2830"),
        (Border0, "#8087a2"),
        (Border1, "#8bd5ca"),
        (Border2, "#a6da95"),
        (Border3, "#eed49f"),
        (Border4, "#f5a97f"),
        (ToolBoxBg, "#363a4f"),
        (ToolBoxBgSuccess, "#363a4f"),
        (ToolBoxBgError, "#363a4f"),
        (Selection, "#5b6078"),
        (CursorLine, "#1e2030"),
    ];
    pairs.iter().cloned().collect()
}

/// The resolved palette of one TUI run: every role lowered to the
/// active capability level. The no-scheme default is the `catppuccin
/// macchiato` scheme (the reference pi theme, docs/tui-color-pi-
/// alignment.md). The `builtin` constructor is the pi built-in `dark`
/// theme element colors, the fallback for an unset role in a user
/// table; a scheme constructor maps roles to hex values and lowers
/// them, an unset role keeps the built-in value of the level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    level: Level,
    colors: std::collections::HashMap<Role, Color>,
}

impl Palette {
    /// The built-in palette of `level`: no scheme selected.
    pub fn builtin(level: Level) -> Self {
        let mut colors = std::collections::HashMap::new();
        for role in Role::ALL {
            colors.insert(*role, role.builtin(level));
        }
        Palette { level, colors }
    }

    /// The palette of a named internal scheme at `level`. `None`
    /// when the name is not a built-in scheme.
    pub fn named(level: Level, name: &str) -> Option<Self> {
        let hexes: &std::collections::HashMap<Role, &'static str> = match name {
            SCHEME_CATPPUCCIN_MACCHIATO => &catppuccin_macchiato(),
            _ => return None,
        };
        let hexes: std::collections::HashMap<Role, String> =
            hexes.iter().map(|(r, h)| (*r, h.to_string())).collect();
        Some(Self::custom(level, &hexes))
    }

    /// The palette of a user-supplied role-to-hex table at `level`
    /// (docs/tui-color-scheme.md section 3: custom scheme input).
    /// Every value is a scheme hex (`#rgb`, `#rrggbb`), validated by
    /// the config layer before this call. An unset role keeps the
    /// built-in value of the level: a partial table overlays the
    /// built-in palette.
    pub fn custom(level: Level, hexes: &std::collections::HashMap<Role, String>) -> Self {
        let mut colors = std::collections::HashMap::new();
        for role in Role::ALL {
            let c = match hexes.get(role) {
                Some(h) => {
                    let c = parse_scheme_color(h)
                        .expect("the config layer validates the scheme hex values");
                    lower(c, level)
                }
                None => role.builtin(level),
            };
            colors.insert(*role, c);
        }
        Palette { level, colors }
    }

    /// Overlay a partial hex table on an existing palette: set roles
    /// get their hex values lowered to `level`, unset roles keep the
    /// base palette's values.
    pub fn overlay(base: &Self, hexes: &std::collections::HashMap<Role, String>) -> Self {
        let mut colors = base.colors.clone();
        for (role, hex) in hexes {
            let c = parse_scheme_color(hex)
                .expect("the config layer validates the scheme hex values");
            colors.insert(*role, lower(c, base.level));
        }
        Palette { level: base.level, colors }
    }

    /// The capability level the palette lowers to.
    pub fn level(&self) -> Level {
        self.level
    }

    /// The lowered color of one role.
    pub fn color(&self, role: Role) -> Color {
        self.colors[&role]
    }

    /// The style of one role: the color plus extra modifiers.
    pub fn style(&self, role: Role, mods: Modifier) -> Style {
        Style::default().fg(self.color(role)).add_modifier(mods)
    }

    /// The thinking-border color of a level (0-4): one palette
    /// lookup instead of a match table in the render code.
    pub fn thinking_border(&self, level: u32) -> Color {
        use Role::*;
        match level {
            0 => self.color(Border0),
            1 => self.color(Border1),
            2 => self.color(Border2),
            3 => self.color(Border3),
            _ => self.color(Border4),
        }
    }
}

/// The palette of the loaded TUI config (docs/tui-color-scheme.md
/// section 3, the scheme switch point: config load). A `None` scheme
/// picks the default scheme, `catppuccin macchiato` (the reference
/// pi theme, docs/tui-color-pi-alignment.md). A named scheme is a
/// built-in name or a user table; the values lower to `level`, and a
/// bad hex is a hard error like the `[tui] color` level.
pub fn palette_from_config(
    level: Level,
    scheme: Option<&str>,
    custom: &std::collections::HashMap<String, std::collections::HashMap<Role, String>>,
) -> Result<Palette, String> {
    let name = scheme.unwrap_or(SCHEME_CATPPUCCIN_MACCHIATO);
    // Resolve the base palette: a built-in scheme or the built-in
    // dark palette (user-defined schemes overlay on it).
    let base = if name == SCHEME_CATPPUCCIN_MACCHIATO
        || name.replace('-', " ") == SCHEME_CATPPUCCIN_MACCHIATO
    {
        Palette::named(level, SCHEME_CATPPUCCIN_MACCHIATO)
            .unwrap_or_else(|| Palette::builtin(level))
    } else {
        Palette::builtin(level)
    };
    // Overlay a user table (either `color_schemes` or `custom_schemes`
    // in the config) on the base palette.  The table name is looked
    // up by the original name and by the hyphen→space normalised form,
    // so both `catppuccin-macchiato` and `catppuccin macchiato` work.
    let norm = |s: &str| s.replace('-', " ");
    let norm_name = norm(name);
    let table = custom
        .get(name)
        .or_else(|| custom.get(&norm_name))
        .or_else(|| {
            custom.iter().find_map(|(k, t)| (norm(k) == norm_name).then(|| t))
        });
    match table {
        Some(hexes) => Ok(Palette::overlay(&base, hexes)),
        None => Ok(base),
    }
}

/// The xterm 256-palette: indices 0-15 (the ANSI swatches, `#c0c0c0`
/// for 7 and `#808080` for 8 per xterm), 16-231 the 6x6x6 cube with
/// channel values `{55,95,135,175,215,255}`, 232-255 the grayscale
/// ramp `8 + 10n`.
fn palette256(idx: u8) -> (u8, u8, u8) {
    let cube = |v: u8| match v {
        0 => 0,
        1 => 95,
        2 => 135,
        3 => 175,
        4 => 215,
        _ => 255,
    };
    match idx {
        0..=15 => {
            const V: [(u8, u8, u8); 16] = [
                (0, 0, 0),
                (128, 0, 0),
                (0, 128, 0),
                (128, 128, 0),
                (0, 0, 128),
                (128, 0, 128),
                (0, 128, 128),
                (192, 192, 192),
                (128, 128, 128),
                (255, 0, 0),
                (0, 255, 0),
                (255, 255, 0),
                (0, 0, 255),
                (255, 0, 255),
                (0, 255, 255),
                (255, 255, 255),
            ];
            V[idx as usize]
        }
        16..=231 => {
            let i = idx as u32 - 16;
            (
                cube((i / 36) as u8),
                cube(((i / 6) % 6) as u8),
                cube((i % 6) as u8),
            )
        }
        _ => {
            let n = idx as u32 - 232;
            let v = (8 + 10 * n) as u8;
            (v, v, v)
        }
    }
}

/// Nearest index in the 256 palette.
///
/// Free-form RGB is quantized into the 6x6x6 cube (16-231) and the
/// grayscale ramp (232-255); the basic 0-15 swatches are reserved for
/// the native ANSI colors, so a true-color value never collapses onto
/// a basic swatch even when one happens to be an exact match. Ties
/// keep the lower index. Exact ramp values (the 24 grays) win with a
/// distance of 0, since no cube corner coincides with them.
fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    let mut best: (u8, u32) = (16, u32::MAX);
    for idx in 16u32..256u32 {
        let (pr, pg, pb) = palette256(idx as u8);
        let d = dist(r, g, b, pr, pg, pb);
        // A strict improvement: ties keep the earlier (lower) index.
        if d < best.1 {
            best = (idx as u8, d);
        }
    }
    best.0
}

/// The 16 ANSI swatches (xterm's 0-15) for `C16` lowering.
fn swatch16(i: u8) -> (u8, u8, u8) {
    palette256(i)
}

/// Nearest of the 16 ANSI swatches (ties: the lower index).
fn nearest_16(r: u8, g: u8, b: u8) -> Color {
    let mut best: (u8, u32) = (0, u32::MAX);
    for i in 0..16u32 {
        let (pr, pg, pb) = swatch16(i as u8);
        let d = dist(r, g, b, pr, pg, pb);
        if d < best.1 {
            best = (i as u8, d);
        }
    }
    match best.0 {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

fn dist(r: u8, g: u8, b: u8, pr: u8, pg: u8, pb: u8) -> u32 {
    let dr = r as i32 - pr as i32;
    let dg = g as i32 - pg as i32;
    let db = b as i32 - pb as i32;
    // Squared Euclidean distance; the sum of squares is non-negative.
    (dr * dr + dg * dg + db * db) as u32
}

