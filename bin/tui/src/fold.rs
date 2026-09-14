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

pub fn tally_text(events: &[Event], lo: usize, hi: usize) -> Option<String> {
    let mut steps = 0usize;
    let mut msgs = 0usize;
    // Histogram of the `name` field, in first-appearance order.
    let mut names: Vec<(String, usize)> = Vec::new();
    for e in &events[lo..hi] {
        match e.kind() {
            EventKind::ToolCall => {
                steps += 1;
                let name = e.get_str("name").unwrap_or("unknown").to_string();
                match names.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, c)) => *c += 1,
                    None => names.push((name, 1)),
                }
            }
            EventKind::AssistantMessage => msgs += 1,
            _ => {}
        }
    }
    if steps == 0 && msgs == 0 {
        return None;
    }
    // Count-descending, first-appearance order on ties.
    let order: Vec<usize> = {
        let mut idx: Vec<usize> = (0..names.len()).collect();
        idx.sort_by(|&a, &b| {
            names[b].1.cmp(&names[a].1).then_with(|| a.cmp(&b))
        });
        idx
    };
    // The agreed tally format (docs/tui-turn-fold.md "Summary line"):
    // the step count, the tool histogram, then the message count.
    // Middle-dot separators. The multiplication sign marks counts.
    // Example: `14 steps · read ×5 · bash ×3 · edit ×2 · 6 msgs`.
    let mut parts: Vec<String> = Vec::new();
    if steps > 0 {
        parts.push(format!(
            "{} step{}",
            steps,
            if steps == 1 { "" } else { "s" }
        ));
    }
    for &i in &order {
        parts.push(format!("{} \u{d7}{}", names[i].0, names[i].1));
    }
    if msgs > 0 {
        parts.push(format!(
            "{} msg{}",
            msgs,
            if msgs == 1 { "" } else { "s" }
        ));
    }
    Some(parts.join(" \u{b7} "))
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

    /// The tally row of a collapsed completed turn.
    /// Open turns emit no row. The in-progress turn emits none.
    /// Its live tally merges into the working row while the loop
    /// runs (docs/tui-turn-fold.md "In-progress turn").
    pub fn collapsed_summary(&self, events: &[Event], t: &Turn) -> Option<SummaryLine> {
        if self.turn_open(t) || t.in_progress {
            return None;
        }
        let hi = t.final_msg.unwrap_or(t.end);
        let text = tally_text(events, t.start + 1, hi)?;
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
    fn tool_ids_collect_the_call_ids() {
        let events = vec![user("u1"), call("c1", "bash"), result("c1"), call("c2", "read")];
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
        let sl = st.collapsed_summary(&events, t).expect("a summary");
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
        assert!(st.collapsed_summary(&events, t).is_none());
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
        assert!(st.collapsed_summary(&events, &t).is_none());
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
        assert!(st.collapsed_summary(&events, &t).is_none());
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
        assert!(st.collapsed_summary(&events, t).is_none());
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
