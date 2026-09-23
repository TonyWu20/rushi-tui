//! The session-log tree model and the `tui-treelistview` glue layer.
//!
//! The `tree` palette stage renders the active session's event log
//! (docs/tree-ui-design-from-human.md) as a tree table built on the
//! `tui-treelistview` crate:
//!
//! - [`SessionTree`] is the `TreeModel`: one node per visible event,
//!   with parent edges following the branch structure of the log.
//! - [`TreeRowFilter`] is the `TreeFilter`: the `Ctrl+F` event-type
//!   filter AND the typed fuzzy query narrow the projection (the
//!   crate keeps matches plus the ancestor path, auto-expanded).
//! - [`TreeRowLabel`] is the `TreeLabelRenderer`: the type-tagged,
//!   truncated one-line label, drawn after the crate's indent
//!   glyphs.
//! - [`spec_glyphs`] is the `TreeGlyphs` set of the design doc:
//!   3 spaces per level, box-drawing `└─` branch marker, no
//!   continuation lines, no expand-state glyph (the tree is always
//!   fully expanded; the filter narrows it).
//!
//! The tree indent (docs/tree-ui-design-from-human.md "Tree indent"):
//! after any fork (rewind marker) exists, rows are indented by their
//! tree depth level: top-level (trunk) rows carry no marker; level-1
//! rows start with `└─ `; each deeper level adds three spaces. The
//! top of the active branch stays left aligned.
//!
//! Depth is derived from the `rewind` markers with the active-path
//! semantics of `rushi_common::rewind::active_ranges`:
//!
//! - A rewind branch is rooted at the marker's `target_seq`, the
//!   rewound event. The branch's span is the rows strictly after the
//!   target, up to the branch's close.
//! - A marker forks when its abandoned tail (the rows between the
//!   target and the marker) holds no marker row: nothing nested was
//!   abandoned. A forked span sits one level below the target's row:
//!   the indent starts right under the rewound event, the branch
//!   point.
//! - A marker that abandons a complete nested branch re-enters the
//!   target's branch: its span keeps the target's level, so the
//!   branch's continuation sits at the branch's own depth.
//! - A branch closes at the first later marker that rewinds back to
//!   its target or earlier: the span stops one seq before it.
//! - The depth of a row is the deepest covering span level, 0 on the
//!   trunk. A marker row renders at the depth of the branch it heads,
//!   the fork-boundary line of that branch.
//! - A marker whose target sits outside the in-memory window falls
//!   back to the trunk (depth 0), the same limitation as
//!   `rewind_active_ranges`.

use std::collections::{HashMap, HashSet};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Cell;
use serde_json::Value;
use tui_treelistview::{
    ColumnDef, ColumnWidth, NoSort, TreeChildren, TreeColumnSet, TreeFilterConfig, TreeGlyphs,
    TreeHorizontalScroll, TreeLabelRenderer, TreeListViewState, TreeListViewStyle, TreeModel,
    TreeQuery, TreeRevision, TreeRootVisibility, TreeRowContext, TreeScrollPolicy,
};

use crate::event::{Event, EventKind};
use crate::palette::items::PaletteItem;
use crate::palette::state::TreeFilter as TypeFilter;

/// The synthetic trunk root id. Log seqs are 1-based, so `0` is never
/// a real event seq. The trunk root is the hidden parent of every
/// depth-0 row: top-level rows render at level 0 with no marker
/// (docs/tree-ui-design-from-human.md "Tree indent"), and the root
/// itself never renders (projection root visibility `Hidden`).
pub const TRUNK_ROOT: usize = 0;

/// The `tui-treelistview` model of one session's event log: the visible
/// (non-`ext_status`) rows, their branch-depth tree, and the palette
/// items one per row.
pub struct SessionTree {
    /// 1-based log seqs of the visible rows, in log order.
    pub seqs: Vec<usize>,
    /// The palette items, one per seq in `seqs` order.
    pub items: Vec<PaletteItem>,
    /// The visible roots: always the single hidden [`TRUNK_ROOT`].
    pub roots: Vec<usize>,
    /// seq -> children seqs, in log order. Absent means leaf.
    children: HashMap<usize, Vec<usize>>,
    /// seq -> index into `items`.
    row: HashMap<usize, usize>,
    /// Row index -> event kind.
    kinds: Vec<EventKind>,
    /// seq -> branch depth (0 = trunk). Diagnostic and test surface;
    /// the renderer reads depth through the crate's projection.
    #[allow(dead_code)]
    depth: HashMap<usize, usize>,
    /// The app's `events_version`: the model revision, so the crate's
    /// projection cache invalidates when the log grows.
    pub revision: u64,
}

/// One parsed `rewind` marker, with the display-depth facts derived
/// from it.
struct Marker {
    /// The wire `target_seq`: the event the branch is rooted at. In
    /// `on` mode the target stays in the context; in `before` mode it
    /// is the excluded user message (restored to the input box).
    target: usize,
    /// A fork when the abandoned tail (`target + 1` to `seq - 1`)
    /// holds no marker row: nothing nested was abandoned, so the
    /// whole span after the target indents one level. A marker that
    /// abandons a complete nested branch re-enters instead and
    /// indents nothing.
    fork: bool,
    /// The last in-window seq this marker's span covers: the seq
    /// before the first later marker that rewinds back to this
    /// marker's target or earlier, or the log's last seq when none
    /// does. The branch closes there.
    close: usize,
}

/// The depth derivation over one marker set (the display rules in the
/// module docs): a marker's span carries a level — one below the
/// target's level when the marker forks, the target's level when it
/// re-enters — and the depth of a row is the deepest covering level
/// (0 on the trunk).
struct DepthSolver {
    /// The in-memory window's first 1-based log seq.
    base: usize,
    /// The markers in log order, with `fork` and `close` derived.
    markers: Vec<Marker>,
    /// Memo: position (in-window, relative to `base`) -> the display
    /// depth of the row at that position.
    depth: Vec<Option<usize>>,
}

impl DepthSolver {
    fn new(base: usize, markers: Vec<Marker>, positions: usize) -> Self {
        Self {
            base,
            markers,
            depth: vec![None; positions],
        }
    }

    /// The display depth of the row at `pos`: the deepest level among
    /// the marker spans that cover it, or 0 on the trunk. Rows
    /// before the in-memory window fall back to the trunk. The
    /// recursion is strictly decreasing in log seq (a span covers its
    /// rows only past its target), so every walk ends at the trunk.
    fn depth_at(&mut self, pos: usize) -> usize {
        if pos < self.base {
            return 0;
        }
        let p = pos - self.base;
        if let Some(d) = self.depth[p] {
            return d;
        }
        let mut d = 0;
        let count = self.markers.len();
        for i in 0..count {
            let m = &self.markers[i];
            // A marker whose target sits outside the in-memory window
            // contributes nothing: the documented trunk fallback.
            if m.target < self.base {
                continue;
            }
            // The marker's span: the rows strictly after its target,
            // up to its close.
            if m.target >= pos || pos > m.close {
                continue;
            }
            // The span's level: one below the target's level when the
            // marker forks, the target's level when it re-enters. The
            // fields are copied out so the immutable borrow ends
            // before the recursive step.
            let (target, fork) = (m.target, m.fork);
            let lvl = self.depth_at(target) + usize::from(fork);
            d = d.max(lvl);
        }
        self.depth[p] = Some(d);
        d
    }
}

impl SessionTree {
    /// Build the tree over the in-memory event window.
    ///
    /// `base_seq` is the 1-based log seq of `events[0]`; `revision` is
    /// the app's `events_version`.
    pub fn build(
        events: &[Event],
        base_seq: usize,
        revision: u64,
        call_names: &HashMap<String, String>,
        call_details: &HashMap<String, (String, Value)>,
    ) -> Self {
        let n = events.len();

        // The parsed rewind markers (the shared kernel parser: a
        // malformed marker is projected out, like a corrupt
        // compaction boundary), with the display-depth facts derived.
        // A marker forks when its abandoned tail holds no marker row,
        // and a branch closes at the first later marker that rewinds
        // back to its target or earlier.
        let raw: Vec<rushi_common::rewind::RewindRef> = events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                e.obj()
                    .and_then(|obj| rushi_common::rewind::parse_rewind_event(obj, base_seq + i))
            })
            .collect();
        let marker_seqs: HashSet<usize> = raw.iter().map(|r| r.seq).collect();
        let last_seq = base_seq + n.saturating_sub(1);
        let markers: Vec<Marker> = raw
            .iter()
            .map(|r| {
                let seq = r.seq;
                let target = r.target;
                let fork = (target + 1..seq).all(|s| !marker_seqs.contains(&s));
                let close = raw
                    .iter()
                    .find(|m| m.seq > seq && m.target <= target)
                    .map(|m| m.seq - 1)
                    .unwrap_or(last_seq);
                Marker {
                    target,
                    fork,
                    close,
                }
            })
            .collect();

        // The display depth of every position: marker rows and event
        // rows alike (a marker row renders at the depth of the
        // branch it heads, so its branch events follow at the same
        // level).
        let mut solver = DepthSolver::new(base_seq, markers, n);
        let pos_depth: Vec<usize> = (0..n).map(|p| solver.depth_at(base_seq + p)).collect();

        // The rows: log order, `ext_status` rows skipped. The parent
        // of a depth-d row is the most recent visible depth-(d-1)
        // row; depth-0 rows are children of the synthetic trunk root
        // [`TRUNK_ROOT`] (a hidden root, `TreeRootVisibility::Hidden`):
        // every top-level row renders at level 0 with no marker, and
        // the hidden root never appears in the projection. A missing
        // parent level after a window truncation falls back to the
        // trunk root.
        let mut seqs: Vec<usize> = Vec::with_capacity(n);
        let mut items: Vec<PaletteItem> = Vec::with_capacity(n);
        let mut kinds: Vec<EventKind> = Vec::with_capacity(n);
        let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut row: HashMap<usize, usize> = HashMap::with_capacity(n);
        let mut depth: HashMap<usize, usize> = HashMap::with_capacity(n);
        let mut last_at_depth: Vec<Option<usize>> = Vec::new();

        for (i, e) in events.iter().enumerate() {
            let kind = e.kind();
            if kind == EventKind::ExtStatus {
                continue;
            }
            let seq = base_seq + i;
            let d = pos_depth[i];
            row.insert(seq, items.len());
            depth.insert(seq, d);
            seqs.push(seq);
            kinds.push(kind);
            items.push(crate::app::tree_event_item(
                e,
                seq,
                call_names,
                call_details,
            ));
            if d == 0 {
                children.entry(TRUNK_ROOT).or_default().push(seq);
            } else {
                match last_at_depth.get(d - 1).copied().flatten() {
                    Some(parent) => children.entry(parent).or_default().push(seq),
                    None => children.entry(TRUNK_ROOT).or_default().push(seq),
                }
            }
            if d >= last_at_depth.len() {
                last_at_depth.resize(d + 1, None);
            }
            last_at_depth[d] = Some(seq);
        }

        Self {
            seqs,
            items,
            roots: vec![TRUNK_ROOT],
            children,
            row,
            kinds,
            depth,
            revision,
        }
    }

    /// The branch depth of a log seq: `None` when the seq is not a
    /// visible row. Exposed for diagnostics and the unit tests; the
    /// renderer reads depth through the crate's projection.
    #[allow(dead_code)]
    pub fn depth_of(&self, seq: usize) -> Option<usize> {
        self.depth.get(&seq).copied()
    }

    /// The row index of a log seq.
    pub fn row_index(&self, seq: usize) -> Option<usize> {
        self.row.get(&seq).copied()
    }

    /// The palette item of a log seq.
    pub fn item_by_seq(&self, seq: usize) -> Option<&PaletteItem> {
        self.row.get(&seq).map(|&i| &self.items[i])
    }

    /// The event kind of a row index.
    pub fn kind_of(&self, row: usize) -> EventKind {
        self.kinds[row]
    }
}

impl TreeModel for SessionTree {
    type Id = usize;

    fn roots(&self) -> impl Iterator<Item = usize> + '_ {
        self.roots.iter().copied()
    }

    fn children(&self, id: usize) -> TreeChildren<'_, usize> {
        match self.children.get(&id) {
            Some(c) if !c.is_empty() => TreeChildren::loaded(c),
            _ => TreeChildren::Leaf,
        }
    }

    fn revision(&self) -> TreeRevision {
        TreeRevision::new(self.revision)
    }

    fn size_hint(&self) -> usize {
        self.seqs.len()
    }
}

/// The tree-row filter: the event-type filter (cycled by `Ctrl+F`)
/// AND the fuzzy query, narrowing candidates before the projection is
/// built (docs/tree-ui-design-from-human-phase-2.md item 3).
#[derive(Clone, Debug)]
pub struct TreeRowFilter {
    /// The fuzzy query, trimmed. Empty: no fuzzy narrowing.
    pub query: String,
    /// The active event-type filter.
    pub type_filter: TypeFilter,
    /// Row indices that fuzzy-match the query (sorted by score), or
    /// `None` when the query is empty (everything matches).
    matched: Option<HashSet<usize>>,
}

impl TreeRowFilter {
    /// Build the filter over a model's rows: one fuzzy rank over all
    /// row labels (the same `rank_fuzzy` the flat palette reuses).
    pub fn new(query: &str, type_filter: TypeFilter, model: &SessionTree) -> Self {
        let query = query.trim().to_string();
        let matched = if query.is_empty() {
            None
        } else {
            let labels: Vec<String> = model.items.iter().map(|i| i.label.clone()).collect();
            let ranked = crate::picker::fuzzy::rank_fuzzy(&labels, &query);
            Some(ranked.into_iter().collect())
        };
        Self {
            query,
            type_filter,
            matched,
        }
    }

    /// A filter revision for the crate's projection cache: it changes
    /// exactly when the filter input changes.
    pub fn revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.query.hash(&mut h);
        self.type_filter.hash(&mut h);
        h.finish()
    }
}

impl tui_treelistview::TreeFilter<SessionTree> for TreeRowFilter {
    fn is_match(&self, model: &SessionTree, id: usize) -> bool {
        let Some(row) = model.row_index(id) else {
            return false;
        };
        if !self.type_filter.keeps(model.kind_of(row)) {
            return false;
        }
        match &self.matched {
            None => true,
            Some(set) => set.contains(&row),
        }
    }
}

/// The crate filter config for a filter: unfiltered (full tree, all
/// expanded) or match-plus-ancestors with auto-expansion.
pub fn filter_config(f: &TreeRowFilter) -> TreeFilterConfig {
    if f.query.is_empty() && f.type_filter == TypeFilter::Full {
        TreeFilterConfig::default()
    } else {
        TreeFilterConfig::enabled()
    }
}

/// The query for the tree projection: the filter (type AND fuzzy)
/// with no sibling sort (log order is the display order).
pub fn build_query(filter: &TreeRowFilter) -> TreeQuery<TreeRowFilter, NoSort> {
    TreeQuery::new()
        .with_filter(
            filter.clone(),
            filter_config(filter),
            TreeRevision::new(filter.revision()),
        )
        // The trunk root is synthetic: its children (the depth-0
        // rows) are the visible top-level rows, and the root itself
        // never renders (docs/tree-ui-design-from-human.md "Tree
        // indent").
        .with_root_visibility(TreeRootVisibility::Hidden)
}

/// The design-doc indent glyphs
/// (docs/tree-ui-design-from-human.md "Tree indent"): 3 spaces per
/// level, the `└─` branch marker, no vertical continuation lines, and
/// empty expansion-state glyphs (every branch is shown expanded; the
/// filter narrows it, the crate never collapses).
pub const fn spec_glyphs() -> TreeGlyphs<'static> {
    TreeGlyphs {
        indent: "   ",
        branch_last: "└─",
        branch: "└─",
        vert: "   ",
        empty: "   ",
        leaf: "",
        expanded: "",
        collapsed: "",
        unloaded: "",
        loading: "",
    }
}

/// The table style of the tree pane: the float chrome (outer border,
/// title, input bar) stays owned by the palette renderer; the
/// borderless table draws the rows. The selection highlight mirrors
/// the flat list's accent-background cursor row.
pub fn build_style(highlight: Color) -> TreeListViewStyle<'static> {
    let mut s = TreeListViewStyle::borderless();
    s.highlight_symbol = "❯ ";
    s.highlight_style = Style::default()
        .bg(highlight)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    s.column_spacing = 1;
    s.horizontal_scroll = TreeHorizontalScroll::Disabled;
    s.scroll_policy = TreeScrollPolicy::KeepInView;
    s
}

/// The two validated columns: the tree column (glyphs + label,
/// flexible) and the `#seq` hint column (fixed 5, the design doc's
/// `#N` marker hint).
pub fn build_columns(hint_color: Color) -> TreeColumnSet<'static, SessionTree> {
    let tree = ColumnDef::tree("", ColumnWidth::flexible(8, 8).expect("8 <= 8"));
    let hint = ColumnDef::data_owned(
        "",
        ColumnWidth::fixed(5),
        move |model: &SessionTree, id: usize, _ctx: &TreeRowContext| -> Cell<'static> {
            let seq = model.row_index(id).map_or(0, |row| model.seqs[row]);
            Cell::from(Line::from(Span::styled(
                format!("#{seq}"),
                Style::default().fg(hint_color),
            )))
        },
    );
    TreeColumnSet::new([tree, hint])
        .expect("tree + data columns")
        .without_header()
}

/// The tree-row label renderer: the type tag up front in its class
/// color (docs/tree-ui-design-from-human-phase-2.md item 1), the
/// rest plain, truncated with `…` when the message is too long
/// (docs/tree-ui-design-from-human.md "Show ... when the message is
/// too long").
pub struct TreeRowLabel<'a> {
    /// The palette that resolves the tag color roles.
    pub palette: &'a crate::color::Palette,
    /// The label character budget; longer labels truncate with `…`.
    pub max_label_chars: usize,
}

impl<'p> TreeLabelRenderer<SessionTree> for TreeRowLabel<'p> {
    fn cell<'a>(
        &'a self,
        model: &'a SessionTree,
        id: usize,
        context: &TreeRowContext<'_>,
        glyphs: &TreeGlyphs<'a>,
    ) -> Cell<'a> {
        let item = model
            .item_by_seq(id)
            .expect("projected ids come from the model");
        let mut spans: Vec<Span<'a>> = Vec::with_capacity(8);

        // 1) The indent glyphs: the same rules as
        //    `tui_treelistview::tree_label_line` (with the spec glyph
        //    set the state glyphs are empty, so only the indent and
        //    branch marks are emitted).
        if context.level > 0 && context.render.draw_lines {
            let branch_level = context.level - 1;
            for (lvl, &is_last) in context.is_tail_stack.iter().enumerate() {
                let glyph = if lvl == branch_level {
                    if is_last {
                        glyphs.branch_last
                    } else {
                        glyphs.branch
                    }
                } else if is_last {
                    glyphs.indent
                } else {
                    glyphs.vert
                };
                spans.push(Span::styled(glyph, context.line_style));
            }
        }
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }

        // 2) The label: tag + rest, truncated to the budget. The tag
        // keeps its class tone; the rest of the line takes the pane's
        // plain-text tone, like the flat list rows.
        let label = truncate_label(&item.label, self.max_label_chars);
        let (tag, rest) = split_tag(&label);
        let plain = self.palette.color(crate::color::Role::PlainText);
        match item.tag_fg {
            Some(role) => {
                let fg = self.palette.color(role);
                spans.push(Span::styled(tag.to_string(), Style::default().fg(fg)));
            }
            None => spans.push(Span::styled(tag.to_string(), Style::default().fg(plain))),
        }
        if !rest.is_empty() {
            spans.push(Span::styled(rest.to_string(), Style::default().fg(plain)));
        }
        Cell::from(Line::from(spans))
    }
}

/// Split a tree-row label into its leading type tag (the first
/// whitespace-delimited token, brackets included) and the rest of the
/// label, delimiter and all. A label without a space is its own tag
/// and the rest is empty. (The same split the flat renderer uses.)
fn split_tag(label: &str) -> (&str, &str) {
    match label.find(' ') {
        Some(i) => (&label[..i], &label[i..]),
        None => (label, ""),
    }
}

/// Truncate a label to `budget` chars, appending `…` when cut
/// (docs/tree-ui-design-from-human.md "No wrapping needed. Show `...`
/// when the message is too long to be displayed in the current window
/// size.").
fn truncate_label(label: &str, budget: usize) -> String {
    let chars = label.chars().collect::<Vec<_>>();
    if chars.len() <= budget {
        label.to_string()
    } else {
        let head: String = chars[..budget.saturating_sub(1)].iter().collect();
        format!("{head}…")
    }
}

/// Keep every branch of a model expanded. Idempotent: it advances the
/// expansion revision only when it changes something, so calling it
/// each frame is free once the tree is stable.
pub fn expand_all(state: &mut TreeListViewState<usize>, model: &SessionTree) {
    state.expand_all(model);
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tui_treelistview::TreeFilter;

    fn user(content: &str) -> Event {
        Event::parse_line(&format!(
            r#"{{"v":1,"type":"user_message","ts":"t","id":"u","content":"{content}"}}"#
        ))
        .expect("user event")
    }

    fn asst(content: &str) -> Event {
        Event::parse_line(&format!(
            r#"{{"v":1,"type":"assistant_message","ts":"t","content":"{content}","tool_calls":[],"stop_reason":"stop","usage":{{"input_tokens":1,"output_tokens":1}}}}"#
        ))
        .expect("assistant event")
    }

    fn tool_call(name: &str) -> Event {
        Event::parse_line(&format!(
            r#"{{"v":1,"type":"tool_call","ts":"t","id":"c","name":"{name}","arguments":{{}}}}"#
        ))
        .expect("tool_call event")
    }

    fn rewind(target: u64, mode: &str) -> Event {
        Event::parse_line(&format!(
            r#"{{"v":1,"type":"rewind","ts":"t","target_seq":{target},"mode":"{mode}"}}"#
        ))
        .expect("rewind event")
    }

    fn empty_maps() -> (
        std::collections::HashMap<String, String>,
        std::collections::HashMap<String, (String, Value)>,
    ) {
        (
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        )
    }

    /// The design-doc example: trunk `A1..A4`, fork `M1` to A4 with
    /// branch B (4 events), fork `M2` to B4 with branch C (2
    /// events), re-entry `M3` into B with one more event, re-entry
    /// `M4` to the trunk with one more event.
    ///
    /// ```text
    /// A1 A2 A3 A4
    /// M1(->4)  B1 B2 B3 B4
    /// M2(->9)  C1 C2
    /// M3(->9)  B5
    /// M4(->4)  A5
    /// ```
    fn design_example() -> (Vec<Event>, HashMap<usize, usize>) {
        let ev = vec![
            user("a1"),
            asst("a2"),
            tool_call("read"),
            tool_call("bash"),
            rewind(4, "on"),
            user("b1"),
            asst("b2"),
            tool_call("read"),
            asst("b4"),
            rewind(9, "on"),
            asst("c1"),
            user("c2"),
            rewind(9, "on"),
            asst("b5"),
            rewind(4, "on"),
            asst("a5"),
        ];
        let (names, details) = empty_maps();
        let tree = SessionTree::build(&ev, 1, 1, &names, &details);
        let mut depth = HashMap::new();
        for seq in 1..=16usize {
            depth.insert(seq, tree.depth_of(seq).expect("every row has a depth"));
        }
        (ev, depth)
    }

    #[test]
    fn design_example_depths() {
        let (_ev, depth) = design_example();
        // Trunk rows and the re-entered trunk: level 0.
        for seq in [1, 2, 3, 4, 15, 16] {
            assert_eq!(depth[&seq], 0, "seq {seq} stays on the trunk");
        }
        // The first fork's branch (and its marker): level 1.
        for seq in [5, 6, 7, 8, 9, 13, 14] {
            assert_eq!(depth[&seq], 1, "seq {seq} is branch B (level 1)");
        }
        // The second fork's branch (and its marker): level 2.
        for seq in [10, 11, 12] {
            assert_eq!(depth[&seq], 2, "seq {seq} is branch C (level 2)");
        }
    }

    #[test]
    fn design_example_tree_shape() {
        let ev = design_example().0;
        let (names, details) = empty_maps();
        let tree = SessionTree::build(&ev, 1, 1, &names, &details);
        assert_eq!(
            tree.roots,
            vec![TRUNK_ROOT],
            "the hidden trunk root is the only root"
        );
        // Every depth-0 row is a child of the trunk root: the trunk
        // chain and the two re-entered trunk segments, in log order.
        assert_eq!(
            tree.children.get(&TRUNK_ROOT),
            Some(&vec![1, 2, 3, 4, 15, 16])
        );
        // Under A4: the fork marker, branch B, the re-entry marker
        // and its event, in log order.
        assert_eq!(tree.children.get(&4), Some(&vec![5, 6, 7, 8, 9, 13, 14]));
        // B4 forks C: the marker and both C events.
        assert_eq!(tree.children.get(&9), Some(&vec![10, 11, 12]));
        // Branch rows attach under the last trunk row, not under the
        // marker row that heads the branch: the re-entry marker 13
        // has no children of its own; its event 14 sits under 4.
        assert_eq!(tree.children.get(&13), None);
        // Trunk rows have no children of their own.
        assert_eq!(tree.children.get(&1), None);
        // A DFS over the real rows must agree with the log order.
        let mut order: Vec<usize> = Vec::new();
        let mut stack: Vec<usize> = tree
            .children
            .get(&TRUNK_ROOT)
            .map(|c| c.iter().rev().copied().collect())
            .unwrap_or_default();
        while let Some(id) = stack.pop() {
            order.push(id);
            if let Some(children) = tree.children.get(&id) {
                for c in children.iter().rev() {
                    stack.push(*c);
                }
            }
        }
        assert_eq!(
            order,
            (1..=16).collect::<Vec<_>>(),
            "DFS order is log order"
        );
    }

    /// The kernel's nested-fork case
    /// (`rushi_common::rewind::nested_forks_mask_the_abandoned_branch`):
    /// A (1..3), fork B (5..6), re-entry A' (8..9) via a marker to
    /// seq 3, continuation of A' via a marker to seq 9.
    #[test]
    fn nested_forks_match_the_kernel_ranges() {
        let ev = vec![
            user("a1"),
            asst("a2"),
            rewind(2, "on"), // seq 3: fork B at 4
            user("b1"),
            asst("b2"),
            rewind(2, "on"), // seq 6: re-enter A at 7
            user("a1'"),
            asst("a2'"),
            rewind(8, "on"), // seq 9: fork D at 10 (a re-entry into C's branch)
            user("d1"),
        ];
        let (names, details) = empty_maps();
        let tree = SessionTree::build(&ev, 1, 1, &names, &details);
        let d = |s: usize| tree.depth_of(s).expect("depth");
        assert_eq!((d(1), d(2), d(3)), (0, 0, 1), "trunk, then B's marker");
        assert_eq!((d(4), d(5)), (1, 1), "branch B");
        assert_eq!(
            (d(6), d(7), d(8)),
            (0, 0, 0),
            "the re-entry A' is trunk-level"
        );
        // Marker 9 targets seq 8, the active leaf just before it: a
        // true fork, so branch D heads one level deeper than A'.
        assert_eq!(d(9), 1, "the fork marker heads its branch");
        assert_eq!(d(10), 1, "branch D is one level under the trunk");
    }

    /// The kernel's re-entry case
    /// (`rushi_common::rewind::reentering_a_branch_rebuilds_its_path`):
    /// a rewind to the tail of B rebuilds B; B's continuation keeps
    /// B's depth.
    #[test]
    fn reentering_a_branch_keeps_its_depth() {
        let ev = vec![
            user("a1"),
            asst("a2"),
            rewind(2, "on"), // seq 3: fork B at 4
            user("b1"),
            asst("b2"),
            rewind(2, "on"), // seq 6: re-enter A at 7
            user("a1'"),
            asst("a2'"),
            rewind(5, "on"), // seq 9: re-enter B's tail at 10
            user("b3"),
        ];
        let (names, details) = empty_maps();
        let tree = SessionTree::build(&ev, 1, 1, &names, &details);
        let d = |s: usize| tree.depth_of(s).expect("depth");
        assert_eq!(d(4), 1, "branch B");
        // Marker 9's span re-roots its abandoned tail under the fork
        // point (b2): the re-entered trunk rows a1'/a2' now nest at
        // B's depth instead of the trunk.
        assert_eq!(d(7), 1, "the abandoned A' rows nest under the fork point");
        assert_eq!(d(8), 1, "the abandoned A' rows nest under the fork point");
        assert_eq!(d(9), 1, "the marker re-enters B (depth 1, not 2)");
        assert_eq!(d(10), 1, "B's continuation keeps B's depth");
    }

    /// A rewind that targets a marker row roots its branch under that
    /// row: the marker is a row of the log like any other, so the
    /// new branch indents one level below it.
    #[test]
    fn rewind_targeting_a_marker_roots_the_branch_under_it() {
        let ev = vec![
            user("a1"),
            rewind(1, "on"), // seq 2: fork B under a1
            user("b1"),
            rewind(2, "on"), // seq 4: roots the branch under marker 2
            user("c1"),
        ];
        let (names, details) = empty_maps();
        let tree = SessionTree::build(&ev, 1, 1, &names, &details);
        let d = |s: usize| tree.depth_of(s).expect("depth");
        assert_eq!(d(2), 1, "marker 2 heads branch B under a1");
        // Marker 4 targets row 2 (the marker row, depth 1). Its whole
        // span — the abandoned b1, marker 4 itself, and c1 — indents
        // one level below row 2.
        assert_eq!(d(3), 2, "the abandoned b1 nests under marker 2");
        assert_eq!(d(4), 2, "marker 4 sits at its branch's depth");
        assert_eq!(d(5), 2, "c1 nests under marker 2");
    }

    /// `before` mode: the target is excluded from the context. The
    /// first rewind forks: its abandoned tail holds a row of the
    /// target context's own trunk branch, so the new sibling branch
    /// indents one level below the trunk — the indent the design doc
    /// promises after a fork, and the one the "retry this user
    /// message" flow relies on. The second rewind forks too: its
    /// target (b1) is a row of branch B, so the new retry branches
    /// off b1 and nests one level below it.
    #[test]
    fn before_mode_retries_nest_under_their_target() {
        let ev = vec![
            user("a1"),
            asst("a2"),
            rewind(2, "before"), // seq 3: branch B forks under a2
            user("b1"),
            rewind(4, "before"), // seq 5: branch C forks under b1
            asst("a3"),
        ];
        let (names, details) = empty_maps();
        let tree = SessionTree::build(&ev, 1, 1, &names, &details);
        let d = |s: usize| tree.depth_of(s).expect("depth");
        // Marker 3: the abandoned tail (event 2, a trunk row) belongs
        // to the target context's own branch, so the new sibling
        // branch forks one level below the target row.
        assert_eq!(d(3), 1, "a before-rewind forks a sibling branch");
        assert_eq!(d(4), 1, "branch B's event sits at level 1");
        // Marker 5: target 4 is b1, a row of branch B at level 1.
        // Its abandoned tail is empty, so it forks: the marker and a3
        // indent one level below b1.
        assert_eq!(d(5), 2, "the retry marker nests under b1");
        assert_eq!(d(6), 2, "the retried event nests under b1");
    }

    /// The shape of the real sessions: a long trunk, a `before`-mode
    /// rewind onto the user message the user retried, then the old
    /// assistant tail abandoned between the target and the marker.
    /// The indent starts right under the rewound event: the target
    /// row stays flat, everything after it (the abandoned tail, the
    /// marker, the new events) indents one level below it.
    #[test]
    fn retry_indent_starts_right_under_the_rewound_event() {
        let ev = vec![
            user("a1"),
            asst("a2"),
            user("retry me"), // seq 3: the rewound event
            asst("old answer 1"),
            asst("old answer 2"),
            rewind(3, "before"), // seq 6: retry the user message
            user("retry me"),
            asst("new answer"),
        ];
        let (names, details) = empty_maps();
        let tree = SessionTree::build(&ev, 1, 1, &names, &details);
        let d = |s: usize| tree.depth_of(s).expect("depth");
        assert_eq!(d(3), 0, "the rewound event stays at the trunk");
        assert_eq!(d(4), 1, "the abandoned tail indents under the target");
        assert_eq!(d(5), 1, "the abandoned tail indents under the target");
        assert_eq!(d(6), 1, "the marker sits at the branch's depth");
        assert_eq!(d(7), 1, "the retried event indents under the target");
        assert_eq!(d(8), 1, "the new answer indents under the target");
    }

    /// A target outside the in-memory window falls back to the trunk,
    /// the same documented limitation as `rewind_active_ranges`.
    #[test]
    fn out_of_window_targets_fall_back_to_the_trunk() {
        // The window starts at seq 51: a fork to seq 50 is out of
        // window, so the branch is trunk-level in the display.
        let ev = vec![user("x51"), rewind(50, "on"), user("x53")];
        let (names, details) = empty_maps();
        let tree = SessionTree::build(&ev, 51, 1, &names, &details);
        assert_eq!(tree.depth_of(51), Some(0));
        assert_eq!(
            tree.depth_of(52),
            Some(0),
            "out-of-window target falls back"
        );
        assert_eq!(tree.depth_of(53), Some(0));
    }

    /// The filter keeps its type set and ANDs with the query
    /// (docs/tree-ui-design-from-human-phase-2.md item 3).
    #[test]
    fn filter_type_and_query_and_semantics() {
        let ev = vec![
            user("deploy the service"),
            asst("checking the log"),
            tool_call("bash"),
            rewind(3, "on"),
            user("retry it"),
        ];
        let (names, details) = empty_maps();
        let tree = SessionTree::build(&ev, 1, 1, &names, &details);

        // The user filter keeps user rows and their ancestor path.
        let f = TreeRowFilter::new("", TypeFilter::User, &tree);
        assert!(f.is_match(&tree, 1), "user row matches");
        assert!(!f.is_match(&tree, 2), "assistant row filtered");
        assert!(!f.is_match(&tree, 3), "tool row filtered");
        assert!(f.is_match(&tree, 5), "the second user row matches");

        // The query ANDs with the type filter: `retry` only matches
        // row 5, which is a user row. `is_match` is the direct-match
        // test; keeping ancestor rows visible is the projection's
        // job, not the filter's.
        let f = TreeRowFilter::new("retry", TypeFilter::User, &tree);
        assert!(f.is_match(&tree, 5), "the fuzzy match survives the AND");
        assert!(
            !f.is_match(&tree, 1),
            "a non-matching user row does not direct-match"
        );
        assert!(!f.is_match(&tree, 2), "unmatched rows drop");

        // The projection keeps the match plus the ancestor path: the
        // trunk rows that lead to row 5 stay visible even though the
        // tool row's kind is filtered out.
        let mut view = tui_treelistview::TreeListViewState::new();
        expand_all(&mut view, &tree);
        let q = build_query(&f);
        view.ensure_projection(&tree, &q);
        let ids: Vec<usize> = view.visible_ids().collect();
        assert!(ids.contains(&5), "the match is visible");
        assert!(
            ids.contains(&3),
            "the ancestor path of a match stays visible"
        );
        assert!(!ids.contains(&1), "a filtered-out trunk row drops");
        assert!(!ids.contains(&2), "a filtered-out row drops");

        // The revision changes exactly when the input changes.
        let a = TreeRowFilter::new("retry", TypeFilter::User, &tree);
        let b = TreeRowFilter::new("retry", TypeFilter::User, &tree);
        let c = TreeRowFilter::new("retry", TypeFilter::Assistant, &tree);
        assert_eq!(a.revision(), b.revision(), "same input, same revision");
        assert_ne!(a.revision(), c.revision(), "a filter change bumps it");
        assert_eq!(
            filter_config(&a),
            TreeFilterConfig::enabled(),
            "a narrow filter enables the filter config"
        );
        let full = TreeRowFilter::new("", TypeFilter::Full, &tree);
        assert_eq!(
            filter_config(&full),
            TreeFilterConfig::default(),
            "the unfiltered tree disables the filter config"
        );
    }

    /// Render the view into a plain buffer: the rewind branches must
    /// carry the design-doc indent (docs/tree-ui-design-from-human.md
    /// "Tree indent") — a `└─` marker plus 3 spaces per depth level,
    /// and no marker on top-level rows.
    #[test]
    fn tree_view_renders_the_branch_indent() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let ev = vec![
            user("a1"),
            asst("a2"),
            rewind(2, "on"), // seq 3: fork marker of branch B.
            user("b1"),      // seq 4: depth 1.
            asst("b2"),      // seq 5: depth 1.
            rewind(5, "on"), // seq 6: fork marker of branch C, under B.
            user("c1"),      // seq 7: depth 2.
        ];
        let (names, details) = empty_maps();
        let model = SessionTree::build(&ev, 1, 1, &names, &details);
        let filter = TreeRowFilter::new("", TypeFilter::Full, &model);
        let query = build_query(&filter);

        let backend = TestBackend::new(80, 12);
        let mut term = Terminal::new(backend).expect("test backend");
        let mut state = tui_treelistview::TreeListViewState::new();
        expand_all(&mut state, &model);
        state.ensure_projection(&model, &query);
        let palette = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let label = TreeRowLabel {
            palette: &palette,
            max_label_chars: 40,
        };
        let style = build_style(palette.color(crate::color::Role::Border4));
        let columns = build_columns(palette.color(crate::color::Role::Hint));
        let view = tui_treelistview::TreeListView::new(&model, &query, &label, &columns, style)
            .glyphs(spec_glyphs());
        let _ = term.draw(|f| {
            f.render_stateful_widget(view, ratatui::layout::Rect::new(0, 0, 80, 12), &mut state);
        });
        let buf = term.backend().buffer().clone();
        let lines: Vec<String> = (0..12u16)
            .map(|y| {
                (0..80u16)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        let joined = lines.join("\n");
        // Depth 1: the `└─` marker, one level in.
        assert!(
            lines.iter().any(|l| l.contains("└─ <user> b1")),
            "the depth-1 branch row carries the marker: {joined}"
        );
        // Depth 2: one more 3-space indent, then the marker.
        assert!(
            lines.iter().any(|l| l.contains("   └─ <user> c1")),
            "the depth-2 branch row gets a second indent level: {joined}"
        );
        // Top-level rows start the tree column right after the
        // highlight symbol column, with no marker; indented rows
        // start with a glyph instead.
        assert!(
            lines
                .iter()
                .any(|l| l.trim_start().starts_with("<user> a1")),
            "the top-level row has no marker: {joined}"
        );
    }
}
