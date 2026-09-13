//! custos — Claude Code hook entry points that continuously capture session turns into a
//! hash-chained ledger, so a session's history survives compaction/`/clear` even when nothing
//! was manually written to durable memory (`docs/DESIGN.md`).
//!
//! `custos hook <event>` reads the hook's JSON from stdin, appends it to the ledger, and NEVER
//! fails the hook: every error path is caught, logged to stderr, and the process still exits 0.
//! A hook that can crash or hang the user's actual Claude Code session is worse than one that
//! occasionally misses an event.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use custos_core as cc;

#[derive(Parser)]
#[command(
    name = "custos",
    version,
    about = "Continuous, hook-driven session capture so history survives Claude Code's own compaction/`/clear`"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Claude Code hook entry point. Reads the hook's JSON from stdin. Never fails the hook.
    Hook(HookArgs),
    /// List captured events (most recent last).
    List(ListArgs),
    /// Verify the ledger's hash chain.
    Verify(LedgerArgs),
    /// Ledger location, event count, and chain head.
    Info(LedgerArgs),
}

#[derive(Parser)]
struct HookArgs {
    #[command(subcommand)]
    event: HookEvent,
}

/// Ledger file. Default: ~/.custos/default/ledger.jsonl. Lives on each variant (not the parent
/// `hook` command) so it can follow the event name on the line, e.g. `custos hook stop --ledger X`
/// — the natural order for a `.claude/settings.json` hook command.
#[derive(Parser)]
struct HookLedgerArgs {
    #[arg(long)]
    ledger: Option<PathBuf>,
}

#[derive(Subcommand)]
enum HookEvent {
    /// UserPromptSubmit
    UserPrompt(HookLedgerArgs),
    /// Stop
    Stop(HookLedgerArgs),
    /// PostToolUse
    PostTool(HookLedgerArgs),
    /// SessionStart
    SessionStart(HookLedgerArgs),
    /// SessionEnd
    SessionEnd(HookLedgerArgs),
    /// PreCompact
    PreCompact(HookLedgerArgs),
}

impl HookEvent {
    fn kind_and_ledger(self) -> (&'static str, Option<PathBuf>) {
        match self {
            HookEvent::UserPrompt(a) => ("user_prompt_submit", a.ledger),
            HookEvent::Stop(a) => ("stop", a.ledger),
            HookEvent::PostTool(a) => ("post_tool_use", a.ledger),
            HookEvent::SessionStart(a) => ("session_start", a.ledger),
            HookEvent::SessionEnd(a) => ("session_end", a.ledger),
            HookEvent::PreCompact(a) => ("pre_compact", a.ledger),
        }
    }
}

#[derive(Parser)]
struct LedgerArgs {
    #[arg(long)]
    ledger: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct ListArgs {
    #[arg(long)]
    ledger: Option<PathBuf>,
    #[arg(long)]
    json: bool,
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Hook(a) => {
            cmd_hook(a);
            Ok(())
        }
        Cmd::List(a) => cmd_list(a),
        Cmd::Verify(a) => cmd_verify(a),
        Cmd::Info(a) => cmd_info(a),
    }
}

/// Never returns an error: every failure is logged to stderr and swallowed.
fn cmd_hook(a: HookArgs) {
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        eprintln!("custos: failed to read hook stdin: {e}");
        return;
    }
    let trimmed = raw.trim();
    let payload: serde_json::Value = if trimmed.is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("custos: hook stdin was not valid JSON ({e}); capturing it raw");
                serde_json::json!({"raw": raw})
            }
        }
    };
    let session_id = cc::extract_session_id(&payload);
    let (kind, ledger_path) = a.event.kind_and_ledger();
    let ledger = cc::Ledger::open(ledger_path.unwrap_or_else(cc::Ledger::default_path));
    if let Err(e) = ledger.append(kind, &session_id, payload) {
        eprintln!("custos: failed to append event: {e}");
    }
}

fn cmd_list(a: ListArgs) -> Result<()> {
    let ledger = cc::Ledger::open(a.ledger.unwrap_or_else(cc::Ledger::default_path));
    let events = ledger.read_all().context("reading ledger")?;
    let start = events.len().saturating_sub(a.limit);
    let tail = &events[start..];

    if a.json {
        println!("{}", serde_json::to_string_pretty(tail)?);
    } else {
        for e in tail {
            println!(
                "{:<6} {:<20} {:<36} {}",
                e.seq,
                e.kind,
                e.session_id,
                summarize_payload(&e.payload)
            );
        }
    }
    Ok(())
}

fn cmd_verify(a: LedgerArgs) -> Result<()> {
    let ledger = cc::Ledger::open(a.ledger.unwrap_or_else(cc::Ledger::default_path));
    let events = ledger.read_all().context("reading ledger")?;
    let break_at = cc::verify_chain(&events);

    if a.json {
        println!(
            "{}",
            serde_json::json!({
                "events": events.len(),
                "intact": break_at.is_none(),
                "break_at": break_at,
            })
        );
    } else {
        println!("events {}", events.len());
        match break_at {
            None => println!("chain   INTACT"),
            Some(seq) => println!("chain   BROKEN at seq {seq}"),
        }
    }

    if break_at.is_some() {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_info(a: LedgerArgs) -> Result<()> {
    let path = a.ledger.unwrap_or_else(cc::Ledger::default_path);
    let ledger = cc::Ledger::open(path.clone());
    let events = ledger.read_all().context("reading ledger")?;
    let head = ledger.head().context("reading ledger head")?;

    if a.json {
        println!(
            "{}",
            serde_json::json!({
                "path": path.display().to_string(),
                "events": events.len(),
                "head": head,
            })
        );
    } else {
        println!("ledger {}", path.display());
        println!("events {}", events.len());
        println!("head   {head}");
    }
    Ok(())
}

/// A short, human-readable preview of a raw hook payload for `custos list`. Tries the field
/// names actually seen from real hook payloads (docs/DESIGN.md §3); falls back to the raw JSON
/// rather than guessing further.
fn summarize_payload(payload: &serde_json::Value) -> String {
    let text = payload
        .get("prompt")
        .or_else(|| payload.get("last_assistant_message"))
        .or_else(|| payload.get("tool_name"))
        .or_else(|| payload.get("why"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| payload.to_string());
    let text = text.replace('\n', " ");
    let truncated: String = text.chars().take(80).collect();
    if text.chars().count() > 80 {
        format!("{truncated}…")
    } else {
        truncated
    }
}
