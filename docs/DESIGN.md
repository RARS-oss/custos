# custos — design

## 1. Problem

A long agent session degrades in a specific, well-defined way: Claude Code keeps the working
context inside a fixed window, and when a session gets long it compacts (summarizes) or the user
runs `/clear`. Anything the agent didn't already write into durable memory before that point is
gone, or survives only as whatever lossy summary the host produced. This project exists because
of exactly that moment happening live, in a real session: hitting a `/clear to save 521.2k tokens`
prompt and realizing everything not already persisted externally would be gone the instant that
was accepted.

[`tabularium`](https://github.com/RARS-oss/tabularium) already solves "durable, checkable memory"
for whatever an agent *chooses* to write — evidence, trust levels, validity checks, a signed
ledger. But "chooses to write" is the weak link: it depends on the agent remembering to call
`memory_observe`/`memory_remember` every time something worth keeping happens, turn after turn,
for the whole session. `custos` is the layer that makes capture continuous and automatic instead
of optional — built strictly on what Claude Code's actual hook surface offers, not on an assumed
capability it turned out not to have (see §3).

## 2. What to build on (reuse, not rebuild)

| Repo | What to take from it |
|---|---|
| [`tabularium`](https://github.com/RARS-oss/tabularium) | The durable store for *curated* memory. `custos` is a **client** of tabularium (its CLI/MCP surface), not a competing memory engine — raw capture gets consolidated into tabularium facts; custos never invents its own long-term memory format or embedding model. |
| [`bulla`](https://github.com/RARS-oss/bulla) | The hash-chained, Ed25519-signed event log pattern (ported, the same way `vigil` ported it — not a Cargo dependency across repos) for custos's *own* raw-capture log, so its claim "every turn from event X to Y was captured, unaltered" is checkable, not asserted. |
| [`vigil`](https://github.com/RARS-oss/vigil) | The immediate precedent, same session: design doc → real code → a real pilot → an honest write-up including what it missed. Follow that shape here too. |

## 3. Scope — what Claude Code's hook surface actually allows (verified 2026-09-13, not assumed)

Before writing any code, the actual hook surface was checked rather than guessed at. Table below
is the honest scope this forces — the same discipline vigil's own §3 used for OWASP coverage.

| Hook | Exists? | Gives | Does NOT give |
|---|---|---|---|
| `SessionStart` | Yes | `session_id`, `cwd`, `permission_mode`, sometimes `model` | No guaranteed `transcript_path` yet |
| `SessionEnd` | Yes | `session_id`, `transcript_path`, **`why`** (`clear`\|`resume`\|`logout`\|`prompt_input_exit`\|`other`) | — this is the best lifecycle signal custos has for "about to lose this session" |
| `PreCompact` / `PostCompact` | Yes | fires around compaction, `matcher` = `auto`\|`manual` | **Notification only.** Cannot block, delay, or influence compaction. By the time it fires, the decision is already made. |
| `Stop` | Yes | `last_assistant_message` (the full text of the turn just finished) | No size/length metadata |
| `UserPromptSubmit` | Yes (already used by tabularium's own recommended hook) | the raw user prompt | No context-size metadata |
| `PostToolUse` | Yes (already used by tabularium's own recommended hook) | tool name + input + output per call | — |
| Token / context usage | **No.** Verified: no env var, no hook JSON field, no CLI query exposes it anywhere in the hook surface. | | This is a hard, checked limit, not an assumption — it rules out any "you're at N% of context" proactive warning in v1. |
| Internal transcript file (`transcript_path`, `.jsonl`) | Exists, but written **asynchronously**, can lag the live conversation, and Claude Code's own docs advise against parsing it directly (format isn't guaranteed stable). | | Not treated as ground truth here — see the model below. |

Consequently, v1's honest scope:

- **Core.** Capture every turn as it happens, built *only* from hook payloads
  (`UserPromptSubmit` + `Stop` + `PostToolUse`) — never by reading `transcript_path`. Treat
  `PreCompact` and `SessionEnd` as the two real trigger points for a checkpoint, since nothing
  better exists.
- **Explicitly out of scope for v1** (a documented gap, not a silently skipped one, same
  convention as vigil's LLM02/LLM08): proactive "approaching the context limit" warnings (no data
  source exists to base one on); preventing or steering compaction (the hooks are notification-only,
  full stop); trusting the internal transcript file as the record of what happened (unsupported,
  async, versioned — `Stop`/`UserPromptSubmit`/`PostToolUse` are the ground truth instead).

## 4. Model

- **Capture.** `UserPromptSubmit` → raw event `kind=user_turn`. `Stop` → raw event
  `kind=assistant_turn` (`last_assistant_message`). `PostToolUse` → raw event `kind=tool_call`
  (tool name + a bounded summary of input/output — not necessarily the full payload, to keep the
  log itself from becoming another unbounded-growth problem). Every event is hash-chained
  (`prev`/`hash`, bulla's construction) into a per-session log.
- **Checkpoint.** On `PreCompact` and on `SessionEnd` (any `why`), close the current chain
  segment and emit a signed receipt (Ed25519, ported from bulla) over exactly which raw events
  it covers — the checkable claim "nothing between the last checkpoint and this one was dropped
  or edited."
- **Consolidation** (deliberately outside the core, mirroring tabularium's own "LLM-free core"
  discipline): a separate `custos consolidate` command reads one session's raw log and — only
  when explicitly invoked, never automatically inside a hook — may shell out to summarize it into
  a handful of durable tabularium facts via `tabularium remember`, the same way tabularium's own
  `evals/` harness shells out to headless `claude -p` rather than embedding an LLM call in the
  engine itself.
- **Resume.** On `SessionStart`, surface the previous session's checkpoint receipt and a pointer
  to its raw log (and, if `custos consolidate` already ran, the tabularium facts it produced) —
  so a fresh agent isn't dependent on the host's own compaction summary being sufficient.

## 5. Claims (to validate for real, not assert)

- **D1 (capture completeness).** For a real session, the number of raw events custos captured
  via hooks matches the number of turns/tool-calls that actually happened — no silent drops.
- **D2 (checkpoint integrity).** custos's hash-chained log detects a dropped or edited event —
  the same tamper-evidence discipline bulla/tabularium/vigil all carry, applied here.
- **D3 (real recovery, the actual point of the project).** After a genuine `/clear`, a fresh
  session can reconstruct a coherent, accurate picture of the prior session from custos's
  checkpoint + raw log alone — validated by actually doing it on this project's own build, the
  same way vigil's C2 was validated against a real target instead of asserted against a mock.

## 6. Roadmap

1. **Week 1:** hook capture (`UserPromptSubmit`/`Stop`/`PostToolUse` → hash-chained raw log),
   session-boundary handling (`SessionStart`/`SessionEnd`).
2. **Week 2:** `PreCompact`-triggered checkpoint + signed receipt (ported from bulla);
   `custos verify` (mirrors bulla/tabularium/vigil's own verify commands).
3. **Week 3:** `custos consolidate` (optional, explicit, may be LLM-assisted — summarizes a raw
   log into tabularium facts via `tabularium remember`); `custos resume` (`SessionStart`-time
   recovery, surfaced via a hook and/or an MCP tool).
4. **Week 4:** a real dogfooding pilot — run custos on this project's own build sessions, write up
   D1–D3 honestly, misses included, the same way `vigil/docs/pilot/` did.

Status: not started. Read this file, tabularium's `DESIGN.md`/`docs/DOGFOODING-NOTES.md`, and
vigil's `docs/pilot/` first — same convention every project in this org follows before writing a
line of code.
