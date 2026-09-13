# custos

**Continuous, hook-driven session capture so an agent's history survives Claude Code's own
context compaction and `/clear`.** Every turn is appended to a hash-chained ledger the moment it
happens — not because the agent remembered to save it, but because a Claude Code hook did.
Lifecycle boundaries (`PreCompact`, `SessionEnd`) get an Ed25519-signed checkpoint, ported from
[`bulla`](https://github.com/RARS-oss/bulla)'s receipt shape, over everything captured since the
last one.

## Why

[`tabularium`](https://github.com/RARS-oss/tabularium) already solves durable, checkable memory
for whatever an agent *chooses* to write. But "chooses to write" is the weak link: it depends on
remembering to call it, turn after turn, for the whole session. This project exists because that
weak link showed up live — a real session hit Claude Code's `/clear to save N tokens` prompt and
the obvious question followed: what happens to everything not already saved? `custos` is a client
of tabularium, not a competing memory engine: it makes capture automatic, and leaves curation
(what's actually worth remembering) to `custos consolidate` and, ultimately, tabularium.

## Scope — grounded in Claude Code's real hook surface, not assumed capability

Checked before writing a line of code (`docs/DESIGN.md` §3):

| | |
|---|---|
| `PreCompact` / `PostCompact` | Exist, but **notification-only** — cannot block or steer compaction. Used as a checkpoint trigger anyway: it's the only "something's about to happen" signal there is. |
| `SessionEnd` | Gives a `why` (`clear`\|`resume`\|`logout`\|`prompt_input_exit`\|`other`) — the best lifecycle signal for "about to lose this session." Also a checkpoint trigger. |
| `Stop` / `UserPromptSubmit` / `PostToolUse` | The actual capture surface — `custos` never parses Claude Code's internal transcript file (async, unsupported, versioned); these three hooks are the ground truth instead. |
| Token / context-usage API | **Verified not to exist anywhere in the hook surface.** No proactive "approaching the limit" warning is possible in v1 — a real, checked gap, not an assumption. |

## Install

```sh
cargo install --path crates/custos-cli
```

## Use from Claude Code

Add to `.claude/settings.json` (alongside any other hooks — e.g. tabularium's):

```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "custos hook user-prompt" }] }],
    "PostToolUse": [{ "hooks": [{ "type": "command", "command": "custos hook post-tool" }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "custos hook stop" }] }],
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "custos hook session-start" }] }],
    "SessionEnd": [{ "hooks": [{ "type": "command", "command": "custos hook session-end" }] }],
    "PreCompact": [{ "hooks": [{ "type": "command", "command": "custos hook pre-compact" }] }]
  }
}
```

Every `custos hook <event>` reads the hook's JSON from stdin and **never fails the hook**:
invalid or empty input is logged to stderr and captured raw rather than propagated as an error —
a hook that can crash or hang the user's actual session is worse than one that occasionally
misses an event.

## Use from the shell

```sh
custos list --limit 20              # most recent captured events
custos verify                       # ledger + checkpoint chain integrity
custos info                         # ledger/checkpoint location, counts, chain head
custos checkpoint                   # manually build a signed checkpoint (usually automatic)
custos checkpoints                  # list signed checkpoints
custos resume                       # what's checkpointed vs. still pending -- orient a fresh session
custos keygen                       # print (creating if absent) the Ed25519 public key

# Digest a session's raw events; optionally condense through a real external program (e.g. a
# `claude -p` wrapper) and/or hand the result to tabularium. Never runs inside a hook.
custos consolidate --session <id>
custos consolidate --session <id> --llm-command ./summarize.sh
custos consolidate --session <id> --remember --subject session.2026-09-13
```

## What's actually verified

- **A real hook, in a real session, captured a real tool call the moment it happened** — the
  `PostToolUse` hook fired live after wiring it into `.claude/settings.json` mid-session (no
  restart needed), and the ledger shows the real `session_id`, `tool_input`, and `tool_response`.
- **Tamper detection is real**, not asserted: hand-editing a byte in the ledger file, or in a
  checkpoint, makes `custos verify` fail (exit 1) and name exactly where.
- **`custos consolidate --remember` really writes to tabularium** — piped via stdin, not a shell
  argument (this agent's own session got bitten by exactly that class of bug elsewhere; see
  `docs/DESIGN.md`).
- **A real `/clear` was survived, checkably.** Triggering an actual `/clear` on a live session
  produced `SessionEnd(reason="clear")` on the old session, a signed checkpoint over everything up
  to that point, and `SessionStart(source="clear")` on the new one, with `custos verify` confirming
  the ledger and checkpoint chains stayed intact across the boundary — and the fresh session used
  `custos info`/`resume`/`list` plus tabularium to reconstruct the prior session's exact state.
  **Honest gap:** that reconstruction was done on request, not automatically — the `SessionStart`
  hint is currently stderr-only and never reaches the agent's context by itself. See
  `docs/DESIGN.md` §5 (D3) for the full result.

## Status

Weeks 1–4 done: hash-chained capture, signed checkpoints triggered by real lifecycle hooks,
consolidation into tabularium, and a real `/clear` triggered live with recovery confirmed
end-to-end (D3) — see `docs/DESIGN.md` for the full claims (D1–D3) and the one open gap
(automatic surfacing of the resume hint, currently stderr-only).

License: MIT OR Apache-2.0.
