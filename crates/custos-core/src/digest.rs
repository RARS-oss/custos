//! A mechanical (no LLM required) digest of a raw event range — what `custos consolidate` sends
//! onward, either straight to a human/tabularium, or through `--llm-command` for a genuinely
//! condensed summary first. Builds no meaning beyond grouping by kind and pulling the field
//! `main.rs`'s `summarize_payload` also uses — the same honest "no assumed schema beyond what's
//! actually there" stance as `RawEvent` itself (`docs/DESIGN.md` §3).

use crate::event::RawEvent;

pub fn build_digest(events: &[RawEvent]) -> String {
    let mut user = 0usize;
    let mut assistant = 0usize;
    let mut tool = 0usize;
    let mut other = 0usize;
    for e in events {
        match e.kind.as_str() {
            "user_prompt_submit" => user += 1,
            "stop" => assistant += 1,
            "post_tool_use" => tool += 1,
            _ => other += 1,
        }
    }

    let mut out = String::new();
    out.push_str(&format!(
        "{} events ({user} user turns, {assistant} assistant turns, {tool} tool calls, {other} other)\n\n",
        events.len()
    ));

    for (kind, label) in [
        ("user_prompt_submit", "User turns"),
        ("stop", "Assistant turns"),
        ("post_tool_use", "Tool calls"),
    ] {
        let matching: Vec<&RawEvent> = events.iter().filter(|e| e.kind == kind).collect();
        if matching.is_empty() {
            continue;
        }
        out.push_str(&format!("--- {label} ---\n"));
        for e in &matching {
            out.push_str(&format!("[{}] {}\n", e.seq, one_liner(kind, &e.payload)));
        }
        out.push('\n');
    }
    out
}

fn one_liner(kind: &str, payload: &serde_json::Value) -> String {
    let field = match kind {
        "user_prompt_submit" => "prompt",
        "stop" => "last_assistant_message",
        "post_tool_use" => "tool_name",
        _ => "",
    };
    let text = payload
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| payload.to_string());
    let text = text.replace('\n', " ");
    let truncated: String = text.chars().take(200).collect();
    if text.chars().count() > 200 {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, kind: &str, payload: serde_json::Value) -> RawEvent {
        RawEvent {
            seq,
            kind: kind.to_string(),
            session_id: "s1".to_string(),
            ts: 0,
            payload,
            prev: crate::ledger::ZERO_HASH.to_string(),
            hash: "x".to_string(),
        }
    }

    #[test]
    fn counts_are_correct_by_kind() {
        let events = vec![
            ev(0, "user_prompt_submit", serde_json::json!({"prompt": "hi"})),
            ev(1, "post_tool_use", serde_json::json!({"tool_name": "Read"})),
            ev(
                2,
                "stop",
                serde_json::json!({"last_assistant_message": "done"}),
            ),
            ev(3, "session_start", serde_json::json!({})),
        ];
        let digest = build_digest(&events);
        assert!(
            digest.starts_with("4 events (1 user turns, 1 assistant turns, 1 tool calls, 1 other)")
        );
        assert!(digest.contains("[0] hi"));
        assert!(digest.contains("[1] Read"));
        assert!(digest.contains("[2] done"));
    }

    #[test]
    fn empty_events_produce_a_zero_summary_line() {
        let digest = build_digest(&[]);
        assert!(
            digest.starts_with("0 events (0 user turns, 0 assistant turns, 0 tool calls, 0 other)")
        );
    }

    #[test]
    fn long_content_is_truncated() {
        let long = "x".repeat(500);
        let events = vec![ev(
            0,
            "stop",
            serde_json::json!({"last_assistant_message": long}),
        )];
        let digest = build_digest(&events);
        assert!(digest.contains('…'));
    }
}
