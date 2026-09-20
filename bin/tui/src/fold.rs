use crate::event::{Event, EventKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub start: usize,
    pub end: usize,
    pub final_msg: Option<usize>,
    pub seq: u64,
    pub in_progress: bool,
}

pub fn turns(events: &[Event], base_seq: usize, loop_running: bool) -> Vec<Turn> {
    let users: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind() == EventKind::UserMessage)
        .map(|(i, _)| i)
        .collect();
    let mut out: Vec<Turn> = Vec::with_capacity(users.len());
    for (k, &start) in users.iter().enumerate() {
        let end = users.get(k + 1).copied().unwrap_or(events.len());
        let mut final_msg: Option<usize> = None;
        for (i, e) in events[start..end].iter().enumerate() {
            if e.kind() == EventKind::AssistantMessage
                && e.get_str("content").is_some_and(|c| !c.is_empty())
            {
                final_msg = Some(start + i);
            }
        }
        let is_last = k + 1 == users.len();
        out.push(Turn {
            start,
            end,
            final_msg,
            seq: (base_seq + start) as u64,
            in_progress: is_last && loop_running,
        });
    }
    out
}

pub fn turn_at(turns: &[Turn], idx: usize) -> Option<usize> {
    turns.iter().position(|t| idx >= t.start && idx < t.end)
}

pub fn event_at_line(starts: &[Option<usize>], line: usize) -> Option<usize> {
    starts.iter().rposition(|s| s.is_some_and(|l| l <= line))
}

/// The structured tally fields, in display order.
///
/// The order matches the tally line: the step count, the tool
/// histogram, the compact count, the hook histogram, then the
/// message count. The hook entries are the compact/hook results and
/// collapse together under the width decision (docs/tui-turn-fold.md
/// "Summary line").
struct TallyParts {
    steps: usize,
    tools: Vec<(String, usize)>,
    compact: usize,
    hooks: Vec<(String, usize)>,
    msgs: usize,
}

/// Count-descending order, first-appearance order on ties.
fn order_by_count_then_appearance(names: &[(String, usize)]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..names.len()).collect();
    idx.sort_by(|&a, &b| names[b].1.cmp(&names[a].1).then_with(|| a.cmp(&b)));
    idx
}

/// The hook name from a `hook_applied` marker value.
///
/// The marker carries the registered command, which may be a bare
/// name (`harness-hook-compact`) or a store path
/// (`/nix/store/.../hooks/harness-hook-compact`). The basename is the
/// hook's name.
fn hook_name(value: &str) -> String {
    value
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(value)
        .to_string()
}

fn tally_parts(events: &[Event], lo: usize, hi: usize) -> TallyParts {
    let mut steps = 0usize;
    let mut msgs = 0usize;
    let mut compact = 0usize;
    // Tool histogram, keyed by the `name` field, first-appearance order.
    let mut tools: Vec<(String, usize)> = Vec::new();
    // Hook histogram, keyed by the `hook_applied` marker's command
    // basename, first-appearance order.
    let mut hooks: Vec<(String, usize)> = Vec::new();
    for e in &events[lo..hi] {
        match e.kind() {
            EventKind::ToolCall => {
                steps += 1;
                let name = e.get_str("name").unwrap_or("unknown").to_string();
                match tools.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, c)) => *c += 1,
                    None => tools.push((name, 1)),
                }
            }
            // A triggered compact. `compaction_started` is the marker
            // the loop writes when a compaction runs (threshold,
            // overflow, or last-resort).
            EventKind::CompactionStarted => compact += 1,
            // The `hook_applied` ext_status marker names the hook
            // command that landed (docs/loop-lifecycle-hooks.md 4.5).
            EventKind::ExtStatus if e.get_str("id") == Some("hook_applied") => {
                if let Some(v) = e.get_str("value") {
                    let name = hook_name(v);
                    match hooks.iter_mut().find(|(n, _)| *n == name) {
                        Some((_, c)) => *c += 1,
                        None => hooks.push((name, 1)),
                    }
                }
            }
            EventKind::AssistantMessage => msgs += 1,
            _ => {}
        }
    }
    TallyParts {
        steps,
        tools,
        compact,
        hooks,
        msgs,
    }
}

/// Join the parts into a tally string. When `collapse_hooks` is set,
/// the compact and hook fields fold into a single `hook ×<total>`
/// field (the width-overflow decision). The step, tool, and message
/// fields are always shown in full.
fn build_tally(parts: &TallyParts, collapse_hooks: bool) -> Option<String> {
    if parts.steps == 0 && parts.msgs == 0 && parts.compact == 0 && parts.hooks.is_empty() {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    if parts.steps > 0 {
        out.push(format!(
            "{} step{}",
            parts.steps,
            if parts.steps == 1 { "" } else { "s" }
        ));
    }
    for &i in &order_by_count_then_appearance(&parts.tools) {
        out.push(format!("{} \u{d7}{}", parts.tools[i].0, parts.tools[i].1));
    }
    if collapse_hooks {
        let total = parts.compact + parts.hooks.iter().map(|(_, c)| *c).sum::<usize>();
        if total > 0 {
            out.push(format!("hook \u{d7}{}", total));
        }
    } else {
        if parts.compact > 0 {
            out.push(format!("compact \u{d7}{}", parts.compact));
        }
        for &i in &order_by_count_then_appearance(&parts.hooks) {
            out.push(format!("{} \u{d7}{}", parts.hooks[i].0, parts.hooks[i].1));
        }
    }
    if parts.msgs > 0 {
        out.push(format!(
            "{} msg{}",
            parts.msgs,
            if parts.msgs == 1 { "" } else { "s" }
        ));
    }
    if out.is_empty() {
        return None;
    }
    Some(out.join(" \u{b7} "))
}

/// The full tally string: step count, tool histogram, compact count,
/// per-hook breakdown, then the message count.
///
/// The agreed tally format (docs/tui-turn-fold.md "Summary line"):
/// middle-dot separators, the multiplication sign marks counts.
/// Example: `14 steps · read ×5 · bash ×3 · compact ×1 ·
/// harness-hook-compact ×2 · 6 msgs`.
pub fn tally_text(events: &[Event], lo: usize, hi: usize) -> Option<String> {
    build_tally(&tally_parts(events, lo, hi), false)
}

/// The tally fitted to a column budget.
///
/// When the full text (with the per-hook breakdown) would exceed
/// `budget` columns, the compact and hook fields collapse into a
/// single `hook ×<total>` field instead of enumerating every hook
/// name. The terminal cannot wrap the tally row, so the overflow
/// decision is a width check here, not a second row
/// (docs/tui-turn-fold.md "Summary line").
pub fn tally_text_fit(events: &[Event], lo: usize, hi: usize, budget: usize) -> Option<String> {
    let parts = tally_parts(events, lo, hi);
    let full = build_tally(&parts, false)?;
    if full.chars().count() <= budget {
        return Some(full);
    }
    build_tally(&parts, true)
}

pub fn tool_ids(events: &[Event], lo: usize, hi: usize) -> Vec<String> {
    events[lo..hi]
        .iter()
        .filter(|e| e.kind() == EventKind::ToolCall)
        .filter_map(|e| e.get_str("id").map(String::from))
        .collect()
}

pub type TurnFoldSet = std::collections::HashSet<u64>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldCursorTarget {
    Turn(u64),
    Block(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryLine {
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct FoldState {
    pub turns: Vec<Turn>,
    pub open: TurnFoldSet,
}

impl FoldState {
    pub fn new(events: &[Event], base_seq: usize, running: bool, open: TurnFoldSet) -> Self {
        Self {
            turns: turns(events, base_seq, running),
            open,
        }
    }

    pub fn turn_for_event(&self, i: usize) -> Option<&Turn> {
        turn_at(&self.turns, i).map(|t| &self.turns[t])
    }

    pub fn turn_open(&self, t: &Turn) -> bool {
        self.open.contains(&t.seq)
    }

    pub fn visible(&self, i: usize) -> bool {
        let Some(t) = self.turn_for_event(i) else {
            return true;
        };
        if t.in_progress {
            return i == t.start || (self.turn_open(t) && Some(i) != t.final_msg);
        }
        if self.turn_open(t) {
            return true;
        }
        i == t.start || Some(i) == t.final_msg
    }

    /// The tally row of a collapsed completed turn, fitted to a column
    /// budget of `budget` columns.
    ///
    /// The tally text is produced by [`tally_text_fit`]: when the
    /// full text (with the per-hook breakdown) would exceed `budget`,
    /// the compact and hook fields collapse into a single
    /// `hook ×<total>` field (docs/tui-turn-fold.md "Summary line").
    /// Pass `usize::MAX` for the full breakdown. Open turns emit no
    /// row. The in-progress turn emits none; its live tally merges
    /// into the working row while the loop runs.
    pub fn collapsed_summary(
        &self,
        events: &[Event],
        t: &Turn,
        budget: usize,
    ) -> Option<SummaryLine> {
        if self.turn_open(t) || t.in_progress {
            return None;
        }
        let hi = t.final_msg.unwrap_or(t.end);
        let text = tally_text_fit(events, t.start + 1, hi, budget)?;
        Some(SummaryLine { text })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(json: &str) -> Event {
        Event::parse_line(json).unwrap()
    }

    fn user(id: &str) -> Event {
        let s = serde_json::json!({
            "v": 1,
            "type": "user_message",
            "ts": "t",
            "id": id,
            "content": "hi"
        })
        .to_string();
        ev(&s)
    }

    fn asst(id: &str, content: &str) -> Event {
        let s = serde_json::json!({
            "v": 1,
            "type": "assistant_message",
            "ts": "t",
            "id": id,
            "content": content,
            "tool_calls": [],
            "stop_reason": "stop"
        })
        .to_string();
        ev(&s)
    }

    fn call(id: &str, name: &str) -> Event {
        let s = serde_json::json!({
            "v": 1,
            "type": "tool_call",
            "ts": "t",
            "id": id,
            "name": name,
            "arguments": {}
        })
        .to_string();
        ev(&s)
    }

    fn result(id: &str) -> Event {
        let s = serde_json::json!({
            "v": 1,
            "type": "tool_result",
            "ts": "t",
            "id": id,
            "value": { "text": "ok" },
            "is_error": false
        })
        .to_string();
        ev(&s)
    }

    fn compact_started() -> Event {
        ev(
            r#"{"v":1,"type":"compaction_started","ts":"t","reason":"threshold","tokens_before":100}"#,
        )
    }

    fn hook_applied(command: &str) -> Event {
        let s = serde_json::json!({
            "v": 1,
            "type": "ext_status",
            "ts": "t",
            "id": "hook_applied",
            "value": command
        })
        .to_string();
        ev(&s)
    }

    #[test]
    fn segments_two_turns() {
        let events = vec![
            user("u1"),
            asst("a1", "hi"),
            call("c1", "bash"),
            result("c1"),
            asst("a2", "done"),
            user("u2"),
            asst("a3", "yo"),
        ];
        let ts = turns(&events, 1, false);
        assert_eq!(ts.len(), 2);
        assert_eq!((ts[0].start, ts[0].end), (0, 5));
        assert_eq!(ts[0].final_msg, Some(4));
        assert_eq!((ts[1].start, ts[1].end), (5, 7));
        assert_eq!(ts[1].final_msg, Some(6));
        assert!(!ts[0].in_progress && !ts[1].in_progress);
        assert_eq!(ts[0].seq, 1);
        assert_eq!(ts[1].seq, 6);
    }

    #[test]
    fn last_turn_in_progress_only_while_running() {
        let events = vec![user("u1"), call("c1", "bash")];
        let ts = turns(&events, 1, true);
        assert!(ts[0].in_progress);
        assert_eq!(ts[0].final_msg, None);
        let ts = turns(&events, 1, false);
        assert!(!ts[0].in_progress);
    }

    #[test]
    fn base_seq_shifts_turn_seqs() {
        let events = vec![user("u1"), asst("a1", "hi")];
        let ts = turns(&events, 41, false);
        // The first event's log seq is base_seq (1-based), so the
        // turn's seq is base_seq + start index.
        assert_eq!(ts[0].seq, 41);
        let ts = turns(&events, 1, false);
        assert_eq!(ts[0].seq, 1);
    }

    #[test]
    fn turn_at_finds_the_containing_turn() {
        let events = vec![user("u1"), asst("a1", "hi"), user("u2"), asst("a2", "yo")];
        let ts = turns(&events, 1, false);
        assert_eq!(turn_at(&ts, 0), Some(0));
        assert_eq!(turn_at(&ts, 1), Some(0));
        assert_eq!(turn_at(&ts, 3), Some(1));
        let orphan = vec![asst("a0", "x"), user("u1")];
        let ts = turns(&orphan, 1, false);
        assert_eq!(turn_at(&ts, 0), None);
        assert_eq!(turn_at(&ts, 1), Some(0));
    }

    #[test]
    fn event_at_line_walks_backwards() {
        let starts = vec![Some(0), Some(4), None, Some(9), Some(12)];
        assert_eq!(event_at_line(&starts, 0), Some(0));
        assert_eq!(event_at_line(&starts, 4), Some(1));
        assert_eq!(event_at_line(&starts, 5), Some(1));
        assert_eq!(event_at_line(&starts, 9), Some(3));
        assert_eq!(event_at_line(&starts, 11), Some(3));
        assert_eq!(event_at_line(&starts, 12), Some(4));
        let empty: Vec<Option<usize>> = Vec::new();
        assert_eq!(event_at_line(&empty, 5), None);
    }

    #[test]
    fn tally_counts_steps_msgs_and_names() {
        let hidden = vec![
            asst("a1", "let me check"),
            call("c1", "bash"),
            result("c1"),
            call("c2", "read"),
            result("c2"),
            call("c3", "bash"),
            result("c3"),
            asst("a2", "another"),
            call("c4", "mymcp__ext"),
            result("c4"),
        ];
        let ts = tally_text(&hidden, 0, hidden.len()).unwrap();
        assert_eq!(ts, "4 steps \u{b7} bash \u{d7}2 \u{b7} read \u{d7}1 \u{b7} mymcp__ext \u{d7}1 \u{b7} 2 msgs");
    }

    #[test]
    fn tally_is_none_on_an_empty_range() {
        let events = vec![result("c1")];
        assert_eq!(tally_text(&events, 0, 1), None);
    }

    #[test]
    fn tally_counts_compact_events() {
        let hidden = vec![
            asst("a1", "let me check"),
            call("c1", "bash"),
            result("c1"),
            compact_started(),
            compact_started(),
            asst("a2", "done"),
        ];
        let ts = tally_text(&hidden, 0, hidden.len()).unwrap();
        assert_eq!(
            ts,
            "1 step \u{b7} bash \u{d7}1 \u{b7} compact \u{d7}2 \u{b7} 2 msgs"
        );
    }

    #[test]
    fn tally_shows_hook_names_and_counts() {
        let hidden = vec![
            call("c1", "bash"),
            result("c1"),
            hook_applied("/nix/store/abc/hooks/harness-hook-goal-arm"),
            hook_applied("/nix/store/abc/hooks/harness-hook-goal-arm"),
            hook_applied("harness-hook-simple-english"),
        ];
        // Hook names use the command basename; count-desc, first-appearance
        // on ties (goal-arm x2 then simple-english x1).
        let ts = tally_text(&hidden, 0, hidden.len()).unwrap();
        assert_eq!(
            ts,
            "1 step \u{b7} bash \u{d7}1 \u{b7} harness-hook-goal-arm \u{d7}2 \u{b7} harness-hook-simple-english \u{d7}1"
        );
    }

    #[test]
    fn tally_shows_compact_with_no_tools() {
        let hidden = vec![compact_started()];
        assert_eq!(
            tally_text(&hidden, 0, hidden.len()).as_deref(),
            Some("compact \u{d7}1")
        );
    }

    #[test]
    fn tally_fit_keeps_hook_names_when_they_fit() {
        let hidden = vec![
            call("c1", "bash"),
            result("c1"),
            hook_applied("harness-hook-goal-arm"),
        ];
        let ts = tally_text_fit(&hidden, 0, hidden.len(), 1000).unwrap();
        assert_eq!(
            ts,
            "1 step \u{b7} bash \u{d7}1 \u{b7} harness-hook-goal-arm \u{d7}1"
        );
    }

    #[test]
    fn tally_fit_collapses_to_single_hook_field_when_over_budget() {
        let hidden = vec![
            call("c1", "bash"),
            result("c1"),
            compact_started(),
            hook_applied("harness-hook-goal-arm"),
            hook_applied("harness-hook-simple-english"),
        ];
        // Total hook activity = 1 compact + 2 hook triggers = 3.
        let tight = tally_text_fit(&hidden, 0, hidden.len(), 40).unwrap();
        assert_eq!(tight, "1 step \u{b7} bash \u{d7}1 \u{b7} hook \u{d7}3");
        // A wide budget keeps the full breakdown.
        let wide = tally_text_fit(&hidden, 0, hidden.len(), 1000).unwrap();
        assert_eq!(
            wide,
            "1 step \u{b7} bash \u{d7}1 \u{b7} compact \u{d7}1 \u{b7} harness-hook-goal-arm \u{d7}1 \u{b7} harness-hook-simple-english \u{d7}1"
        );
    }

    #[test]
    fn collapsed_summary_collapse_to_hook_total_when_over_budget() {
        let events = vec![
            user("u1"),
            asst("a1", "check"),
            call("c1", "bash"),
            result("c1"),
            compact_started(),
            hook_applied("harness-hook-goal-arm"),
            asst("a2", "done"),
        ];
        let st = FoldState::new(&events, 1, false, std::collections::HashSet::new());
        let t = st.turn_for_event(0).unwrap();
        let wide = st.collapsed_summary(&events, t, 200).unwrap();
        // The tally covers hidden events [start+1, final_msg): the final
        // assistant message is not counted in `msgs`.
        assert_eq!(
            wide.text,
            "1 step \u{b7} bash \u{d7}1 \u{b7} compact \u{d7}1 \u{b7} harness-hook-goal-arm \u{d7}1 \u{b7} 1 msg"
        );
        let tight = st.collapsed_summary(&events, t, 20).unwrap();
        assert_eq!(
            tight.text,
            "1 step \u{b7} bash \u{d7}1 \u{b7} hook \u{d7}2 \u{b7} 1 msg"
        );
    }

    #[test]
    fn tool_ids_collect_the_call_ids() {
        let events = vec![
            user("u1"),
            call("c1", "bash"),
            result("c1"),
            call("c2", "read"),
        ];
        assert_eq!(
            tool_ids(&events, 0, 4),
            vec!["c1".to_string(), "c2".to_string()]
        );
    }

    #[test]
    fn hidden_line_maps_to_its_turn() {
        let events = vec![
            user("u1"),
            call("c1", "bash"),
            result("c1"),
            user("u2"),
            asst("a1", "hi"),
        ];
        let ts = turns(&events, 1, false);
        let starts = vec![Some(0), None, None, Some(4), Some(6)];
        let ev = event_at_line(&starts, 4).expect("the second user box");
        assert_eq!(turn_at(&ts, ev), Some(1));
        let ev = event_at_line(&starts, 5).expect("the summary line");
        assert_eq!(turn_at(&ts, ev), Some(1));
    }

    #[test]
    fn collapsed_turn_shows_only_boxes_and_summary() {
        let events = vec![
            user("u1"),
            asst("a1", "let me check"),
            call("c1", "bash"),
            result("c1"),
            asst("a2", "done"),
            user("u2"),
            asst("a3", "yo"),
        ];
        let st = FoldState::new(&events, 1, false, std::collections::HashSet::new());
        assert!(st.visible(0));
        assert!(!st.visible(1));
        assert!(!st.visible(2));
        assert!(!st.visible(3));
        assert!(st.visible(4));
        assert!(st.visible(5));
        assert!(st.visible(6));
        let t = st.turn_for_event(0).unwrap();
        let sl = st
            .collapsed_summary(&events, t, usize::MAX)
            .expect("a summary");
        assert_eq!(sl.text, "1 step \u{b7} bash \u{d7}1 \u{b7} 1 msg");
    }

    #[test]
    fn open_turn_shows_everything() {
        let events = vec![
            user("u1"),
            asst("a1", "let me check"),
            call("c1", "bash"),
            result("c1"),
            asst("a2", "done"),
        ];
        let st = FoldState::new(&events, 1, false, std::collections::HashSet::from([1]));
        for i in 0..events.len() {
            assert!(st.visible(i));
        }
        let t = st.turn_for_event(0).unwrap();
        assert!(st.collapsed_summary(&events, t, usize::MAX).is_none());
    }

    #[test]
    fn in_progress_collapse_hides_live_tail() {
        let events = vec![
            user("u1"),
            asst("a1", "starting"),
            call("c1", "bash"),
            result("c1"),
        ];
        let st = FoldState::new(&events, 1, true, std::collections::HashSet::new());
        let t = st.turns[0].clone();
        assert!(t.in_progress);
        assert!(st.visible(0));
        assert!(!st.visible(1));
        assert!(!st.visible(2));
        assert!(!st.visible(3));
        // No in-transcript summary row for the live turn.
        // The tally merges into the working row instead.
        assert!(st.collapsed_summary(&events, &t, usize::MAX).is_none());
        // The live tally of the in-progress range, as the working
        // row displays it.
        assert_eq!(
            tally_text(&events, 1, 4).as_deref(),
            Some("1 step \u{b7} bash \u{d7}1 \u{b7} 1 msg")
        );
    }

    #[test]
    fn in_progress_open_hides_only_live_tail() {
        let events = vec![
            user("u1"),
            asst("a1", "starting"),
            call("c1", "bash"),
            result("c1"),
            asst("a2", "still going"),
        ];
        let st = FoldState::new(&events, 1, true, std::collections::HashSet::from([1]));
        assert!(st.visible(0));
        assert!(st.visible(1));
        assert!(st.visible(2));
        assert!(st.visible(3));
        assert!(!st.visible(4));
        let t = st.turns[0].clone();
        // An open live turn hides only the live tail. No summary row
        // either: the working row carries the tally.
        assert!(st.collapsed_summary(&events, &t, usize::MAX).is_none());
        assert_eq!(
            tally_text(&events, 1, 5).as_deref(),
            Some("1 step \u{b7} bash \u{d7}1 \u{b7} 2 msgs")
        );
    }

    #[test]
    fn tally_is_none_when_nothing_hidden() {
        let events = vec![user("u1"), asst("a1", "hi")];
        let st = FoldState::new(&events, 1, false, std::collections::HashSet::new());
        let t = &st.turns[0];
        assert!(st.collapsed_summary(&events, t, usize::MAX).is_none());
    }

    #[test]
    fn tool_ids_cover_the_turn_range() {
        let events = vec![
            user("u1"),
            call("c1", "mymcp__ext"),
            result("c1"),
            call("c2", "bash"),
            user("u2"),
            call("c3", "bash"),
        ];
        assert_eq!(tool_ids(&events, 1, 4), vec!["c1", "c2"]);
        assert_eq!(tool_ids(&events, 5, 6), vec!["c3"]);
    }
}
