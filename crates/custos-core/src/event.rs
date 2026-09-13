//! One captured raw event — verbatim hook input, never a parsed/trusted-schema subset.
//!
//! `docs/DESIGN.md` §3 found no reliable, version-stable schema to depend on beyond the
//! documented common fields (`session_id`, `hook_event_name`). Rather than guess at exact field
//! names for `UserPromptSubmit`'s prompt or `Stop`'s `last_assistant_message` and silently drop
//! anything that doesn't match, `RawEvent` stores the entire hook JSON payload verbatim. Whoever
//! reads it later (a human, `custos consolidate`, an LLM) pulls out what they need from the raw
//! JSON — nothing is lost to a brittle field extraction that might not match this Claude Code
//! version's actual shape.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawEvent {
    pub seq: u64,
    /// Which hook produced this: "user_prompt_submit" | "stop" | "post_tool_use" |
    /// "session_start" | "session_end" | "pre_compact". Not an enum: a future Claude Code hook
    /// custos doesn't know about yet should still be capturable, not rejected.
    pub kind: String,
    /// Best-effort extraction of the hook payload's `session_id` field. Empty string if absent.
    pub session_id: String,
    /// Unix epoch seconds when custos captured this event (hooks don't reliably carry their own).
    pub ts: u64,
    /// The hook's JSON input, verbatim.
    pub payload: serde_json::Value,
    /// Hash of the previous event, or `ledger::ZERO_HASH` for the first.
    pub prev: String,
    /// sha256 over (seq, kind, session_id, payload, prev, ts) — see `ledger::compute_hash`.
    pub hash: String,
}

/// Pull `session_id` out of a raw hook payload, if present and a string.
pub fn extract_session_id(payload: &serde_json::Value) -> String {
    payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}
