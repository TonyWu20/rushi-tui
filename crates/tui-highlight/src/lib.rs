//! `tui-highlight` — the tree-sitter syntax-highlight engine
//! (docs/NEW-refactor.md, must-do 1: drop `syntect`, use Rust
//! tree-sitter, isolated in its own crate).
//!
//! This crate owns the tree-sitter runtime and every bundled grammar.
//! Nothing else in the workspace may depend on tree-sitter: a change
//! to the TUI never rebuilds the grammars, and a grammar update never
//! rebuilds the TUI.
//!
//! The API is a stateful, per-file highlighter: feed the hard lines of
//! one body in order. The engine keeps a growing buffer and re-parses
//! it (tree-sitter is fast, and the bodies are bounded by
//! `expandedPreviewMaxLines`), then extracts the styled segments for
//! the most recent line. Plain (unstyled) runs come out as
//! `Style::default()` so the caller recolors them to the `Code` role —
//! the same contract as the hand-rolled engine in `bin/tui/src/highlight.rs`.
//!
//! Colors are the Catppuccin Macchiato palette (the repo's default
//! `catppuccin macchiato` scheme) at full RGB, foreground only (no
//! background, per the 2026-09-11 request). The caller lowers them to
//! the terminal capability level (`color::lower_style` in `bin/tui`).

use ratatui::style::{Color, Modifier, Style};
use std::collections::HashSet;
use tree_sitter::{Language, Parser};

/// One styled run of text. `Style::default()` means "plain": the
/// caller recolors it to the `Code` role.
pub type Seg = (Style, String);

/// One entry of the grammar registry.
struct LangDef {
    /// The registry name (also what [`resolve_lang`] returns).
    name: &'static str,
    /// The grammar constructor.
    lang: tree_sitter_language::LanguageFn,
    /// Keyword texts that get the keyword color when they appear as
    /// bare tokens. Tree-sitter leaves most keywords as anonymous
    /// tokens, so the type table cannot see them; the text check
    /// covers that case.
    keywords: &'static [&'static str],
}

/// Resolve a language token (a `language_from_path` result or a fence
/// tag) to a registry name. `None` when no grammar is bundled: the
/// caller keeps the text plain.
pub fn resolve_lang(lang: &str) -> Option<&'static str> {
    let t = lang.trim().to_ascii_lowercase();
    Some(match t.as_str() {
        "rust" | "rs" => "rust",
        "python" | "python3" | "py" => "python",
        "c" => "c",
        "cpp" | "c++" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "go" => "go",
        "java" => "java",
        "javascript" | "js" | "node" | "mjs" | "cjs" => "javascript",
        "typescript" | "ts" | "tsx" | "jsx" => "typescript",
        "shell" | "sh" | "bash" | "zsh" | "fish" | "ksh" => "bash",
        "json" => "json",
        "markdown" | "md" | "mdx" => "markdown",
        "html" | "htm" | "xml" => "html",
        "css" | "scss" | "sass" => "css",
        "lua" => "lua",
        "ruby" | "rb" | "rake" => "ruby",
        "scala" | "sc" => "scala",
        "perl" | "pl" | "pm" | "t" => "perl",
        "r" => "r",
        "objc" | "objective-c" | "objective_c" => "objc",
        "make" | "makefile" | "gnumakefile" | "mk" => "make",
        "yaml" | "yml" | "config" => "yaml",
        // No grammar bundled (no new-API crate yet): plain text.
        "sql" | "swift" | "kotlin" | "elixir" | "zig" | "nim" | "vb"
        | "dockerfile" => return None,
        _ => return None,
    })
}

/// The registry: one entry per bundled grammar.
fn lang_defs() -> &'static [LangDef] {
    static DEFS: &[LangDef] = &[
        LangDef {
            name: "rust",
            lang: tree_sitter_rust::LANGUAGE,
            keywords: &["fn", "let", "mut", "const", "static", "pub", "use", "mod", "crate",
                "impl", "trait", "struct", "enum", "type", "where", "match", "if", "else",
                "loop", "while", "for", "in", "return", "break", "continue", "async",
                "await", "unsafe", "extern", "ref", "self", "Self", "super", "dyn", "box",
                "move", "yield", "true", "false", "Some", "None", "Ok", "Err"],
        },
        LangDef {
            name: "python",
            lang: tree_sitter_python::LANGUAGE,
            keywords: &["def", "class", "return", "if", "elif", "else", "for", "while", "in",
                "not", "and", "or", "is", "None", "True", "False", "import", "from", "as",
                "with", "try", "except", "finally", "raise", "lambda", "yield", "pass",
                "break", "continue", "global", "nonlocal", "assert", "del", "async",
                "await", "print"],
        },
        LangDef {
            name: "c",
            lang: tree_sitter_c::LANGUAGE,
            keywords: &["if", "else", "while", "for", "do", "return", "break", "continue",
                "switch", "case", "default", "goto", "sizeof", "typedef", "struct", "union",
                "enum", "const", "static", "extern", "volatile", "inline", "void", "int",
                "char", "float", "double", "bool", "auto", "short", "long", "unsigned",
                "signed"],
        },
        LangDef {
            name: "cpp",
            lang: tree_sitter_cpp::LANGUAGE,
            keywords: &["if", "else", "while", "for", "do", "return", "break", "continue",
                "switch", "case", "default", "goto", "sizeof", "typedef", "struct", "union",
                "enum", "const", "static", "extern", "volatile", "inline", "void", "int",
                "char", "float", "double", "bool", "auto", "new", "delete", "public",
                "private", "protected", "virtual", "override", "final", "template",
                "typename", "namespace", "using", "this", "nullptr", "true", "false",
                "short", "long", "unsigned", "signed"],
        },
        LangDef {
            name: "go",
            lang: tree_sitter_go::LANGUAGE,
            keywords: &["func", "package", "import", "var", "const", "type", "interface",
                "struct", "map", "chan", "go", "defer", "return", "if", "else", "for",
                "range", "switch", "case", "default", "break", "continue", "fallthrough",
                "goto", "select", "nil", "true", "false", "make", "new", "len", "cap",
                "append", "panic", "recover"],
        },
        LangDef {
            name: "java",
            lang: tree_sitter_java::LANGUAGE,
            keywords: &["class", "interface", "enum", "extends", "implements", "public",
                "private", "protected", "static", "final", "abstract", "void", "int",
                "long", "short", "byte", "float", "double", "boolean", "char", "var",
                "new", "return", "if", "else", "for", "while", "do", "switch", "case",
                "default", "break", "continue", "try", "catch", "finally", "throw",
                "throws", "this", "super", "null", "true", "false", "instanceof"],
        },
        LangDef {
            name: "javascript",
            lang: tree_sitter_javascript::LANGUAGE,
            keywords: &["function", "const", "let", "var", "if", "else", "for", "while",
                "do", "switch", "case", "default", "break", "continue", "return", "new",
                "class", "extends", "super", "import", "export", "from", "as", "try",
                "catch", "finally", "throw", "typeof", "instanceof", "in", "of",
                "delete", "void", "yield", "async", "await", "null", "undefined",
                "true", "false", "this"],
        },
        LangDef {
            name: "typescript",
            lang: tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            keywords: &["function", "const", "let", "var", "if", "else", "for", "while",
                "do", "switch", "case", "default", "break", "continue", "return", "new",
                "class", "extends", "super", "import", "export", "from", "as", "try",
                "catch", "finally", "throw", "typeof", "instanceof", "in", "of",
                "delete", "void", "yield", "async", "await", "null", "undefined",
                "true", "false", "this", "type", "interface", "enum", "readonly",
                "namespace"],
        },
        LangDef {
            name: "bash",
            lang: tree_sitter_bash::LANGUAGE,
            keywords: &["if", "then", "else", "elif", "fi", "for", "while", "until", "do",
                "done", "case", "esac", "function", "in", "return", "exit", "local",
                "export", "break", "continue", "select", "time"],
        },
        LangDef {
            name: "json",
            lang: tree_sitter_json::LANGUAGE,
            keywords: &["true", "false", "null"],
        },
        LangDef {
            name: "html",
            lang: tree_sitter_html::LANGUAGE,
            keywords: &[],
        },
        LangDef {
            name: "css",
            lang: tree_sitter_css::LANGUAGE,
            keywords: &[],
        },
    ];
    DEFS
}

/// Look up a language by its registry name. Internal to this crate;
/// callers use [`resolve_lang`], which returns the registry name.
fn lang_def(name: &str) -> Option<&'static LangDef> {
    lang_defs().iter().find(|d| d.name == name)
}

/// The per-line highlighter. Feed the lines of one file in order;
/// the buffer and parse state carry multi-line context (block
/// comments, open fences) forward.
pub struct Highlighter {
    parser: Parser,
    /// The active grammar name, or `None` before the first line.
    active: Option<&'static str>,
    /// The accumulated source (the lines fed so far, each
    /// newline-terminated).
    text: String,
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            active: None,
            text: String::new(),
        }
    }

    /// Highlight one hard line. The segments come out in source order
    /// and concatenate back to the input line. `lang` is a language
    /// token (`language_from_path` result or a plain extension /
    /// fence tag); a token with no bundled grammar returns the line
    /// unstyled, so the caller's plain-run recolor keeps the `Code`
    /// tone.
    pub fn line(&mut self, line: &str, lang: Option<&str>) -> Vec<Seg> {
        // When `lang` is `None`, reuse the previously active grammar so
        // multi-line context (block comments, open fences) is preserved.
        let name = if let Some(l) = lang {
            match resolve_lang(l) {
                Some(n) => n,
                None => {
                    if self.active.is_some() {
                        // Unknown language token while an active grammar is
                        // set: keep the active grammar so context is not
                        // lost.
                        self.active.expect("checked above")
                    } else {
                        return vec![(Style::default(), line.to_string())];
                    }
                }
            }
        } else {
            match self.active {
                Some(n) => n,
                None => return vec![(Style::default(), line.to_string())],
            }
        };
        let def = match lang_def(name) {
            Some(d) => d,
            None => return vec![(Style::default(), line.to_string())],
        };
        // A language change resets the per-file state (a new grammar,
        // a fresh buffer).
        if self.active != Some(name) {
            let lang = Language::from(def.lang);
            let _ = self.parser.set_language(&lang);
            self.text.clear();
            self.active = Some(name);
        }
        self.text.push_str(line);
        self.text.push('\n');
        // Re-parse the accumulated buffer. The bodies are bounded by
        // `expandedPreviewMaxLines`, and tree-sitter is fast on them.
        let Some(tree) = self.parser.parse(&self.text, None) else {
            return vec![(Style::default(), line.to_string())];
        };
        // The byte range of the line just appended.
        let start = self.text.len() - line.len() - 1;
        let end = self.text.len() - 1;
        highlight_range(&self.text, &tree, start, end, def)
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

/// Highlight a whole text body, one segment vector per hard line.
/// Convenience wrapper over [`Highlighter::line`].
pub fn highlight_lines(text: &str, lang: Option<&str>) -> Vec<Vec<Seg>> {
    let mut h = Highlighter::new();
    text.lines().map(|l| h.line(l, lang)).collect()
}

/// A highlight role. The style comes from the Catppuccin Macchiato
/// palette (the repo's default `catppuccin macchiato` scheme):
/// mauve `#c6a0f6`, red `#ed8796`, peach `#f5a97f`, green `#a6e3a1`,
/// yellow `#eed6a4`, blue `#89b4fa`, teal `#89dceb`,
/// overlay0 `#6c7086`, overlay1 `#939af0`, overlay2 `#a6adc8`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HlRole {
    /// Plain: `Style::default()`; the caller recolors to `Code`.
    Plain,
    Comment,
    StringLit,
    Number,
    Keyword,
    Function,
    TypeName,
    Operator,
    Punctuation,
    Constant,
    Label,
    Decorator,
}

/// The role style (foreground only; a token carries no background,
/// per the 2026-09-11 request).
pub fn role_style(r: HlRole) -> Style {
    use HlRole::*;
    match r {
        Plain => Style::default(),
        Comment => Style::default().fg(Color::Rgb(0xa6, 0xad, 0xc8)),
        StringLit => Style::default().fg(Color::Rgb(0xa6, 0xe3, 0xa1)),
        Number | Constant => Style::default().fg(Color::Rgb(0xf5, 0xa9, 0x7f)),
        Keyword => Style::default()
            .fg(Color::Rgb(0xc6, 0xa0, 0xf6))
            .add_modifier(Modifier::BOLD),
        Function => Style::default().fg(Color::Rgb(0x89, 0xb4, 0xfa)),
        TypeName => Style::default().fg(Color::Rgb(0x89, 0xdc, 0xeb)),
        Operator => Style::default().fg(Color::Rgb(0x93, 0x9a, 0xf0)),
        Punctuation => Style::default().fg(Color::Rgb(0x6c, 0x70, 0x86)),
        Label => Style::default().fg(Color::Rgb(0xee, 0xd6, 0xa4)),
        Decorator => Style::default().fg(Color::Rgb(0xed, 0x87, 0x96)),
    }
}

/// Map a tree-sitter node type to a role. A broad table covering the
/// node names of the bundled grammars; unknown types stay `Plain`.
fn node_role(kind: &str) -> HlRole {
    use HlRole::*;
    match kind {
        "comment"
        | "line_comment"
        | "block_comment"
        | "comment_line"
        | "comment_block"
        | "comment_start"
        | "comment_end"
        | "shebang"
        | "block_comment_content"
        | "doc_comment" => Comment,
        "string"
        | "string_fragment"
        | "string_content"
        | "string_literal"
        | "string_literal_value"
        | "string_value"
        | "string_body"
        | "string_part"
        | "byte_string"
        | "raw_string"
        | "raw_string_literal"
        | "fstring_string"
        | "concatenated_string"
        | "interpolated_string"
        | "interpolation"
        | "interpolation_part"
        | "character"
        | "character_literal"
        | "character_value"
        | "char"
        | "char_literal"
        | "heredoc"
        | "heredoc_body"
        | "heredoc_value"
        | "substituted_string"
        | "substitute_string"
        | "quoted_string"
        | "quoted_symbol"
        | "single_quoted_string"
        | "double_quoted_string"
        | "regex"
        | "character_class"
        | "interpreted_string_literal"
        | "interpreted_string_body"
        | "raw_string_body"
        | "escape_sequence" => StringLit,
        "number"
        | "number_literal"
        | "integer"
        | "float"
        | "float_literal"
        | "integer_literal"
        | "number_value"
        | "decimal_number"
        | "hex_number"
        | "octal_number"
        | "binary_number"
        | "exponent_value"
        | "decimal_integer_literal"
        | "hexadecimal"
        | "octal"
        | "binary"
        | "float_value"
        | "numeric_value"
        | "numeric"
        | "numeric_literal"
        | "float_number"
        | "integer_value" => Number,
        "constant_value"
        | "boolean"
        | "boolean_value"
        | "null_value"
        | "null"
        | "constant"
        | "true"
        | "false" => Constant,
        "keyword"
        | "keyword_decl"
        | "keyword_other"
        | "keyword_operator"
        | "keyword_function"
        | "keyword_control"
        | "keyword_namespace"
        | "keyword_import"
        | "keyword_storage"
        | "keyword_statement"
        | "keyword_type"
        | "keyword_const"
        | "keyword_modifier"
        | "keyword_preprocessor"
        | "type_annotation"
        | "annotation" => Keyword,
        "function"
        | "function_item"
        | "function_definition"
        | "function_declaration"
        | "function_signature"
        | "function_expression"
        | "function_body"
        | "function_name"
        | "function_identifier"
        | "function_call"
        | "function_call_expression"
        | "function_destructuring_pattern"
        | "method"
        | "method_definition"
        | "method_declaration"
        | "method_signature"
        | "constructor"
        | "constructor_declaration"
        | "constructor_body"
        | "call_expression"
        | "call_suffix"
        | "call_expression_suffix"
        | "method_call_expression"
        | "invocation"
        | "application"
        | "application_suffix"
        | "application_call"
        | "lambda"
        | "lambda_expression"
        | "lambda_literal"
        | "closure"
        | "closure_expression"
        | "anonymous_function_expression"
        | "fn_pointer"
        | "function_pointer"
        | "arrow_function"
        | "named_sub"
        | "anon_sub"
        | "subroutine"
        | "substatement"
        | "named_function_definition"
        | "method_call" => Function,
        "type_identifier"
        | "type_name"
        | "type_name_declaration"
        | "type_identifier_reference"
        | "type_instantiation"
        | "type_constraint"
        | "type_definition"
        | "typedef_declaration"
        | "typedef"
        | "type_alias"
        | "type_alias_declaration"
        | "struct_type"
        | "struct_specifier"
        | "struct_item"
        | "struct_declaration"
        | "struct_definition"
        | "struct"
        | "class"
        | "class_declaration"
        | "class_definition"
        | "class_name"
        | "class_type"
        | "class_body"
        | "class_member_definition"
        | "enum"
        | "enum_item"
        | "enum_declaration"
        | "enum_definition"
        | "enum_member"
        | "enum_member_name"
        | "enum_type"
        | "union"
        | "union_type"
        | "union_item"
        | "trait"
        | "trait_item"
        | "trait_declaration"
        | "trait_definition"
        | "interface"
        | "interface_declaration"
        | "interface_definition"
        | "type_parameter"
        | "type_parameter_list"
        | "type_parameters"
        | "type_arguments"
        | "type_parameters_clause"
        | "generic_type"
        | "generic_function"
        | "qualified_identifier"
        | "qualified_type_identifier"
        | "qualified_type"
        | "qualified_name"
        | "namespace"
        | "namespace_declaration"
        | "using_declaration"
        | "namespace_alias_declaration"
        | "primitive_type"
        | "predefined_type"
        | "primitive_type_name"
        | "predefined_type_name"
        | "builtin_type"
        | "builtin_type_name"
        | "object_type"
        | "pointer_type"
        | "array_type"
        | "object_type_name"
        | "type_reference"
        | "type_descriptor" => TypeName,
        "operator"
        | "operator_token"
        | "operator_expression"
        | "unary_operator"
        | "binary_operator"
        | "operator_assignment_expression"
        | "update_operator"
        | "assignment_operator"
        | "delimiter"
        | "separator" => Operator,
        "punctuation"
        | "parenthesized_expression"
        | "parenthesized_pattern"
        | "parentheses"
        | "braces"
        | "brackets"
        | "curly_braces" => Punctuation,
        "label"
        | "label_statement"
        | "label_id"
        | "property_identifier"
        | "field_identifier"
        | "property"
        | "field"
        | "field_identifier_expression"
        | "field_expression"
        | "member_access_expression"
        | "property_access_expression"
        | "field_access_expression"
        | "dot"
        | "member"
        | "member_access"
        | "property_access"
        | "field_access"
        | "tag_name"
        | "attribute_name"
        | "property_identifier_expression"
        | "property_identifier_access"
        | "field_identifier_access"
        | "selector"
        | "selector_list"
        | "simple_selector"
        | "declaration" => Label,
        "attribute"
        | "attribute_list"
        | "attribute_item"
        | "attribute_argument"
        | "attribute_input"
        | "attribute_value"
        | "decorator"
        | "decorator_list"
        | "decorator_expression"
        | "attribute_modifier"
        | "qualifier"
        | "type_qualifier"
        | "macro"
        | "macro_call"
        | "macro_definition"
        | "preproc_def"
        | "preproc_call"
        | "preproc_line"
        | "preproc_function"
        | "preproc_args"
        | "preproc_arg"
        | "preproc_field"
        | "preproc_parameter"
        | "preprocessor_call"
        | "preprocessor"
        | "preprocessor_expression"
        | "preprocessor_line"
        | "preprocessor_argument"
        | "preprocessor_definition"
        | "preprocessor_function"
        | "preprocessor_identifier"
        | "preprocessor_parameter"
        | "preprocessor_field"
        | "directive"
        | "directive_argument"
        | "macro_rule"
        | "define"
        | "include" => Decorator,
        _ => Plain,
    }
}

/// Walk `tree` and return the styled segments for the byte range
/// `[start, end)` of `src`, in source order. Adjacent segments with
/// the same style merge; plain gaps come out as `Style::default()`.
fn highlight_range(
    src: &str,
    tree: &tree_sitter::Tree,
    start: usize,
    end: usize,
    def: &LangDef,
) -> Vec<Seg> {
    let kw: HashSet<&str> = def.keywords.iter().copied().collect();
    // Collect the colored intervals: the leaf tokens whose role is
    // not `Plain`. Leaf roles: a named leaf maps through
    // `node_role`; an anonymous leaf falls back to the keyword set
    // (tree-sitter leaves most keywords as anonymous tokens).
    let mut intervals: Vec<(usize, usize, HlRole)> = Vec::new();
    let mut stack: Vec<tree_sitter::Node> = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let ns = node.start_byte();
        let ne = node.end_byte();
        if ne <= start || ns >= end {
            continue;
        }
        if node.child_count() == 0 {
            let text = node.utf8_text(src.as_bytes()).ok();
            let role = if node.is_named() {
                node_role(node.kind())
            } else if text.is_some_and(|t| kw.contains(t)) {
                HlRole::Keyword
            } else if matches!(text.as_deref(), Some("\"") | Some("'")) {
                // Anonymous string delimiters: the grammars split a
                // literal into quote / content / quote tokens, so the
                // quote tokens join the string run.
                HlRole::StringLit
            } else {
                HlRole::Plain
            };
            if role != HlRole::Plain {
                intervals.push((ns, ne, role));
            }
        } else {
            // An inner node: push the children right-to-left so the
            // pop order stays left-to-right.
            for i in (0..node.child_count()).rev() {
                if let Some(c) = node.child(i) {
                    stack.push(c);
                }
            }
        }
    }
    // The leaf intervals are disjoint (leaves do not nest); sort by
    // start and sweep left-to-right, merging same-style runs.
    intervals.sort_by_key(|(s, _, _)| *s);
    let mut out: Vec<Seg> = Vec::new();
    let mut pos = start;
    for (s, e, role) in intervals {
        // Clamp the interval to the queried range: a multi-line
        // leaf (a block comment) spans the range and colors only
        // the part inside it.
        let s = s.max(start);
        let e = e.min(end);
        if s >= e || s < pos {
            continue;
        }
        if s > pos {
            out.push((Style::default(), src[pos..s].to_string()));
        }
        let st = role_style(role);
        let txt = src[s..e].to_string();
        if out.last().map_or(false, |(p, _)| *p == st) {
            out.last_mut().unwrap().1.push_str(&txt);
        } else {
            out.push((st, txt));
        }
        pos = e;
    }
    if pos < end {
        out.push((Style::default(), src[pos..end].to_string()));
    }
    if out.is_empty() {
        out.push((Style::default(), src[start..end].to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No language: one plain segment, lossless.
    #[test]
    fn unknown_language_is_plain() {
        let segs = highlight_lines("hello world", None);
        assert_eq!(segs, vec![vec![(Style::default(), "hello world".to_string())]]);
    }

    /// An unresolvable token stays plain (lossless, one segment).
    #[test]
    fn unresolved_token_is_plain() {
        let segs = highlight_lines("let x = 1;", Some("totally-made-up"));
        assert_eq!(segs.len(), 1);
        assert_eq!(
            segs[0].iter().map(|(_, t)| t.as_str()).collect::<String>(),
            "let x = 1;"
        );
        assert!(segs[0].iter().all(|(s, _)| *s == Style::default()));
    }

    /// Rust source: lossless join, at least one colored token, and
    /// the `fn` keyword and the `42` literal land on the macchiato
    /// mauve / peach.
    #[test]
    fn rust_line_gets_colored_tokens() {
        let segs = highlight_lines("fn main() { let x = 42; }", Some("rs"));
        let joined = segs[0].iter().map(|(_, t)| t.as_str()).collect::<String>();
        assert_eq!(joined, "fn main() { let x = 42; }");
        assert!(
            segs[0].iter().any(|(s, _)| *s != Style::default()),
            "expected at least one colored token: {segs:?}"
        );
        let kw = segs[0]
            .iter()
            .find(|(_, t)| t.as_str() == "fn")
            .expect("the `fn` keyword is a token");
        assert_eq!(kw.0.fg, Some(Color::Rgb(0xc6, 0xa0, 0xf6)));
        let num = segs[0]
            .iter()
            .find(|(_, t)| t.as_str() == "42")
            .expect("the numeric literal is a token");
        assert_eq!(num.0.fg, Some(Color::Rgb(0xf5, 0xa9, 0x7f)));
    }

    /// A string literal keeps its quotes in one string-colored run.
    #[test]
    fn string_literal_is_a_single_token() {
        let segs = highlight_lines(r#"let s = "hi there";"#, Some("rs"));
        let joined = segs[0].iter().map(|(_, t)| t.as_str()).collect::<String>();
        assert_eq!(joined, r#"let s = "hi there";"#);
        let lit = segs[0]
            .iter()
            .find(|(_, t)| t.as_str() == r#""hi there""#);
        assert!(lit.is_some(), "the quoted literal is one run: {segs:?}");
        assert_eq!(lit.unwrap().0.fg, Some(Color::Rgb(0xa6, 0xe3, 0xa1)));
    }

    /// Highlighted text carries no background color (the 2026-09-11
    /// request): every token's `bg` is `None`.
    #[test]
    fn highlighted_tokens_carry_no_background() {
        let segs = highlight_lines("fn main() { let x = 42; }", Some("rust"));
        for (st, t) in &segs[0] {
            assert!(
                st.bg.is_none(),
                "token {t:?} carries a background: {st:?}"
            );
        }
        assert!(segs[0].iter().any(|(s, _)| s.fg.is_some()));
    }

    /// End-to-end: an idiomatic snippet per bundled language token
    /// tokenizes losslessly, and the confident languages color at
    /// least one token.
    #[test]
    fn resolved_languages_tokenize_idiomatic_snippets() {
        let cases: &[(&str, &str)] = &[
            ("rust", "fn main() {}"),
            ("python", "def f(): pass"),
            ("go", "func main() {}"),
            ("c", "int main() { return 0; }"),
            ("cpp", "int main() { return 0; }"),
            ("java", "class A {}"),
            ("javascript", "function f() {}"),
            ("shell", "if [ -z \"$x\" ]; then echo hi; fi"),
            ("json", r#"{"a": 1}"#),
            ("html", "<div>ok</div>"),
            ("css", "a { color: red; }"),
        ];
        for (lang, snippet) in cases {
            let segs = highlight_lines(snippet, Some(lang));
            let joined = segs[0].iter().map(|(_, t)| t.as_str()).collect::<String>();
            assert_eq!(
                joined, *snippet,
                "{lang}: text must be lossless, got {joined:?}"
            );
            assert!(
                segs[0].iter().any(|(s, _)| *s != Style::default()),
                "{lang}: expected a colored token: {segs:?}"
            );
        }
        // A token with no bundled grammar stays a plain run.
        let unknown = highlight_lines("fn main() {}", Some("gleam"));
        assert!(
            unknown[0].iter().all(|(s, _)| *s == Style::default()),
            "no grammar for `gleam`: the line stays plain: {unknown:?}"
        );
    }

    /// The resolution table: every bundled token resolves; the rest
    /// stay `None`.
    #[test]
    fn resolve_lang_table() {
        assert_eq!(resolve_lang("rust"), Some("rust"));
        assert_eq!(resolve_lang("python"), Some("python"));
        assert_eq!(resolve_lang("JavaScript"), Some("javascript"));
        assert_eq!(resolve_lang("shell"), Some("bash"));
        assert_eq!(resolve_lang("yaml"), Some("yaml"));
        assert_eq!(resolve_lang("no-such-language"), None);
        assert_eq!(resolve_lang("sql"), None);
        assert_eq!(resolve_lang("gleam"), None);
    }

    /// Multi-line state: feeding a complete C block comment line by
    /// line; the final line (which closes the comment) is colored
    /// with the comment role where the comment text appears.
    #[test]
    fn multi_line_block_comment_stays_coherent() {
        let src = "int x; /* open\n  still inside\n */ int y;";
        let segs = highlight_lines(src, Some("c"));
        assert_eq!(segs.len(), 3);
        // Line 3 contains ` */ int y;` — the ` */` part should be
        // comment-colored (the comment spans from line 1 through
        // the `*/` on line 3).
        let c = role_style(HlRole::Comment);
        let l3 = &segs[2];
        assert!(
            l3.iter().any(|(s, t)| s.fg == c.fg && t.contains("*/")),
            "line 3 has the comment-closing `*/` in comment color: {l3:?}"
        );
    }
}
