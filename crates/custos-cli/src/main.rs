//! custos — Claude Code hook entry points that continuously capture session turns into a
//! hash-chained ledger, so a session's history survives compaction/`/clear` even when nothing
//! was manually written to durable memory (`docs/DESIGN.md`).
//!
//! `custos hook <event>` reads the hook's JSON from stdin, appends it to the ledger, and NEVER
//! fails the hook: every error path is caught, logged to stderr, and the process still exits 0.
//! `pre-compact` and `session-end` additionally trigger a signed checkpoint over everything new
//! since the last one — the two real lifecycle signals `docs/DESIGN.md` §3 found in Claude Code's
//! actual hook surface, and nothing more than that.

use std::io::Read;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// List captured raw events (most recent last).
    List(ListArgs),
    /// Verify the ledger's and checkpoints' hash chains.
    Verify(LedgerArgs),
    /// Ledger/checkpoint location, counts, and chain heads.
    Info(LedgerArgs),
    /// Manually build a signed checkpoint over everything new since the last one.
    Checkpoint(CheckpointArgs),
    /// List signed checkpoints.
    Checkpoints(CheckpointsArgs),
    /// Print the public key for a signing seed (creating it if absent).
    Keygen(KeygenArgs),
    /// Build a digest of raw events and, optionally, condense it further (--llm-command) and/or
    /// hand it to tabularium (--remember). Never runs automatically inside a hook.
    Consolidate(ConsolidateArgs),
    /// Show what's checkpointed vs. still pending, for a fresh session to orient itself.
    Resume(ResumeArgs),
}

#[derive(Parser)]
struct HookArgs {
    #[command(subcommand)]
    event: HookEvent,
}

/// Paths every hook variant can override. Lives on each variant (not the parent `hook` command)
/// so it can follow the event name on the line, e.g. `custos hook stop --ledger X` — the natural
/// order for a `.claude/settings.json` hook command. `checkpoints`/`key` only matter for
/// `pre-compact`/`session-end`, which are the only variants that trigger a checkpoint.
#[derive(Parser, Clone)]
struct HookLedgerArgs {
    #[arg(long)]
    ledger: Option<PathBuf>,
    #[arg(long)]
    checkpoints: Option<PathBuf>,
    #[arg(long)]
    key: Option<PathBuf>,
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
    /// SessionEnd -- also triggers a checkpoint.
    SessionEnd(HookLedgerArgs),
    /// PreCompact -- also triggers a checkpoint.
    PreCompact(HookLedgerArgs),
}

impl HookEvent {
    fn kind_and_args(self) -> (&'static str, HookLedgerArgs) {
        match self {
            HookEvent::UserPrompt(a) => ("user_prompt_submit", a),
            HookEvent::Stop(a) => ("stop", a),
            HookEvent::PostTool(a) => ("post_tool_use", a),
            HookEvent::SessionStart(a) => ("session_start", a),
            HookEvent::SessionEnd(a) => ("session_end", a),
            HookEvent::PreCompact(a) => ("pre_compact", a),
        }
    }
}

#[derive(Parser)]
struct LedgerArgs {
    #[arg(long)]
    ledger: Option<PathBuf>,
    #[arg(long)]
    checkpoints: Option<PathBuf>,
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

#[derive(Parser)]
struct CheckpointArgs {
    #[arg(long)]
    ledger: Option<PathBuf>,
    #[arg(long)]
    checkpoints: Option<PathBuf>,
    #[arg(long)]
    key: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct CheckpointsArgs {
    #[arg(long)]
    checkpoints: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct KeygenArgs {
    #[arg(long)]
    key: Option<PathBuf>,
}

#[derive(Parser)]
struct ConsolidateArgs {
    /// Only include events for this session id. Default: every session in range.
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    ledger: Option<PathBuf>,
    /// Only events with seq >= this. Default: 0.
    #[arg(long)]
    from_seq: Option<u64>,
    /// Only events with seq <= this. Default: the last event.
    #[arg(long)]
    to_seq: Option<u64>,
    /// Pipe the mechanical digest through this program (stdin -> stdout) for an LLM-assisted
    /// summary instead of the raw digest -- e.g. a wrapper around `claude -p`. Spawned directly,
    /// never through a shell string.
    #[arg(long)]
    llm_command: Option<PathBuf>,
    /// Hand the resulting summary to `tabularium remember --kind fact` (requires `tabularium` on
    /// PATH), piped via stdin -- never as a shell argument.
    #[arg(long)]
    remember: bool,
    #[arg(long)]
    subject: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct ResumeArgs {
    #[arg(long)]
    ledger: Option<PathBuf>,
    #[arg(long)]
    checkpoints: Option<PathBuf>,
    /// Only consider checkpoints touching this session id.
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    json: bool,
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
        Cmd::Checkpoint(a) => cmd_checkpoint(a),
        Cmd::Checkpoints(a) => cmd_checkpoints(a),
        Cmd::Keygen(a) => cmd_keygen(a),
        Cmd::Consolidate(a) => cmd_consolidate(a),
        Cmd::Resume(a) => cmd_resume(a),
    }
}

/// Never returns an error: every failure is logged to stderr and swallowed.
fn cmd_hook(a: HookArgs) {
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        eprintln!("custos: failed to read hook stdin: {e}");
        return;
    }
    // Strip a leading UTF-8 BOM before trimming whitespace: `str::trim` doesn't remove it, and a
    // caller that writes one (a PowerShell `Get-Content -Encoding UTF8` pipe does) would
    // otherwise make an otherwise-valid JSON payload fail to parse for no real reason.
    let trimmed = raw.trim_start_matches('\u{FEFF}').trim();
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
    let (kind, hook_args) = a.event.kind_and_args();

    let ledger_path = hook_args
        .ledger
        .clone()
        .unwrap_or_else(cc::Ledger::default_path);
    let ledger = cc::Ledger::open(&ledger_path);
    if let Err(e) = ledger.append(kind, &session_id, payload) {
        eprintln!("custos: failed to append event: {e}");
        return;
    }

    let checkpoints_path = hook_args
        .checkpoints
        .clone()
        .unwrap_or_else(cc::CheckpointStore::default_path);

    if matches!(kind, "pre_compact" | "session_end") {
        let key_path = hook_args.key.clone().unwrap_or_else(cc::default_key_path);
        match do_checkpoint(kind, ledger_path, checkpoints_path, key_path) {
            Ok(Some(sc)) => eprintln!(
                "custos: checkpoint {} covers seq {}..={} ({} events)",
                &sc.body_digest[..12.min(sc.body_digest.len())],
                sc.body.from_seq,
                sc.body.to_seq,
                sc.body.event_count
            ),
            Ok(None) => {}
            Err(e) => eprintln!("custos: checkpoint failed: {e:#}"),
        }
    } else if kind == "session_start" {
        // Unfiltered by session: this session is brand new and has no history of its own yet --
        // the useful thing to show is the most recent checkpoint from whatever came before.
        // stderr only: whether SessionStart's stdout is parsed for a control protocol wasn't
        // verified (docs/DESIGN.md §3 only confirms fields it provides, not what it accepts
        // back), so a plain informational hint stays on the side channel rather than risking it.
        match build_resume_report(&ledger_path, &checkpoints_path, None, false) {
            Ok(report) => eprintln!("custos resume:\n{report}"),
            Err(e) => eprintln!("custos: resume report failed: {e:#}"),
        }
    }
}

/// Build, sign, and append a checkpoint over every raw event since the last one. `Ok(None)`
/// means there was nothing new to cover (not an error -- a hook firing twice with no events in
/// between must be a silent no-op, not a logged failure).
fn do_checkpoint(
    trigger: &str,
    ledger_path: PathBuf,
    checkpoints_path: PathBuf,
    key_path: PathBuf,
) -> Result<Option<cc::SignedCheckpoint>> {
    let ledger = cc::Ledger::open(&ledger_path);
    let events = ledger.read_all().context("reading ledger")?;

    let store = cc::CheckpointStore::open(&checkpoints_path);
    let existing = store.read_all().context("reading checkpoints")?;
    let from_seq = existing.last().map(|c| c.body.to_seq + 1).unwrap_or(0);

    let covered: Vec<&cc::RawEvent> = events.iter().filter(|e| e.seq >= from_seq).collect();
    let Some(last) = covered.last() else {
        return Ok(None);
    };
    let to_seq = last.seq;
    let ledger_head = last.hash.clone();
    let event_count = covered.len() as u64;
    let mut session_ids: Vec<String> = covered
        .iter()
        .map(|e| e.session_id.clone())
        .filter(|s| !s.is_empty())
        .collect();
    session_ids.sort();
    session_ids.dedup();

    let seed = cc::load_or_create_seed(&key_path).context("loading signing key")?;
    let sc = store
        .append_new(
            cc::NewCheckpoint {
                trigger,
                from_seq,
                to_seq,
                event_count,
                ledger_head: &ledger_head,
                session_ids,
                created_ts: now_epoch(),
            },
            &seed,
        )
        .context("appending checkpoint")?;
    Ok(Some(sc))
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
    let ledger_break_at = cc::verify_ledger_chain(&events);

    let store = cc::CheckpointStore::open(
        a.checkpoints
            .unwrap_or_else(cc::CheckpointStore::default_path),
    );
    let checkpoints = store.read_all().context("reading checkpoints")?;
    let checkpoint_reports = cc::verify_checkpoint_chain(&checkpoints);
    let first_broken_checkpoint = checkpoint_reports.iter().position(|r| !r.intact());

    if a.json {
        println!(
            "{}",
            serde_json::json!({
                "ledger_events": events.len(),
                "ledger_intact": ledger_break_at.is_none(),
                "ledger_break_at": ledger_break_at,
                "checkpoints": checkpoints.len(),
                "checkpoints_intact": first_broken_checkpoint.is_none(),
                "first_broken_checkpoint": first_broken_checkpoint,
            })
        );
    } else {
        println!("ledger events    {}", events.len());
        match ledger_break_at {
            None => println!("ledger chain     INTACT"),
            Some(seq) => println!("ledger chain     BROKEN at seq {seq}"),
        }
        println!("checkpoints      {}", checkpoints.len());
        match first_broken_checkpoint {
            None => println!("checkpoint chain INTACT"),
            Some(i) => println!("checkpoint chain BROKEN at checkpoint #{i}"),
        }
    }

    if ledger_break_at.is_some() || first_broken_checkpoint.is_some() {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_info(a: LedgerArgs) -> Result<()> {
    let ledger_path = a.ledger.unwrap_or_else(cc::Ledger::default_path);
    let ledger = cc::Ledger::open(&ledger_path);
    let events = ledger.read_all().context("reading ledger")?;
    let head = ledger.head().context("reading ledger head")?;

    let checkpoints_path = a
        .checkpoints
        .unwrap_or_else(cc::CheckpointStore::default_path);
    let store = cc::CheckpointStore::open(&checkpoints_path);
    let checkpoints = store.read_all().context("reading checkpoints")?;

    if a.json {
        println!(
            "{}",
            serde_json::json!({
                "ledger_path": ledger_path.display().to_string(),
                "events": events.len(),
                "head": head,
                "checkpoints_path": checkpoints_path.display().to_string(),
                "checkpoints": checkpoints.len(),
            })
        );
    } else {
        println!("ledger      {}", ledger_path.display());
        println!("events      {}", events.len());
        println!("head        {head}");
        println!(
            "checkpoints {} ({})",
            checkpoints.len(),
            checkpoints_path.display()
        );
    }
    Ok(())
}

fn cmd_checkpoint(a: CheckpointArgs) -> Result<()> {
    let ledger_path = a.ledger.unwrap_or_else(cc::Ledger::default_path);
    let checkpoints_path = a
        .checkpoints
        .unwrap_or_else(cc::CheckpointStore::default_path);
    let key_path = a.key.unwrap_or_else(cc::default_key_path);

    match do_checkpoint("manual", ledger_path, checkpoints_path, key_path)? {
        Some(sc) => {
            if a.json {
                println!("{}", serde_json::to_string_pretty(&sc)?);
            } else {
                println!("checkpoint {}", sc.body_digest);
                println!(
                    "covers     seq {}..={} ({} events)",
                    sc.body.from_seq, sc.body.to_seq, sc.body.event_count
                );
                println!("sessions   {}", sc.body.session_ids.join(", "));
            }
        }
        None => println!("nothing new to checkpoint"),
    }
    Ok(())
}

fn cmd_checkpoints(a: CheckpointsArgs) -> Result<()> {
    let store = cc::CheckpointStore::open(
        a.checkpoints
            .unwrap_or_else(cc::CheckpointStore::default_path),
    );
    let all = store.read_all().context("reading checkpoints")?;

    if a.json {
        println!("{}", serde_json::to_string_pretty(&all)?);
    } else {
        for sc in &all {
            println!(
                "{:<12} {:<12} seq {}..={} ({} events, {} sessions)",
                &sc.body_digest[..12.min(sc.body_digest.len())],
                sc.body.trigger,
                sc.body.from_seq,
                sc.body.to_seq,
                sc.body.event_count,
                sc.body.session_ids.len()
            );
        }
    }
    Ok(())
}

fn cmd_keygen(a: KeygenArgs) -> Result<()> {
    let key_path = a.key.unwrap_or_else(cc::default_key_path);
    let seed = cc::load_or_create_seed(&key_path)?;
    println!("pubkey {}", cc::pubkey_hex(&seed));
    println!("seed   {}", key_path.display());
    Ok(())
}

/// Deliberately never called from a hook: consolidation is explicit and opt-in, may shell out to
/// an LLM and/or tabularium, and neither of those belongs inside a "never fail the hook" path.
fn cmd_consolidate(a: ConsolidateArgs) -> Result<()> {
    let ledger = cc::Ledger::open(a.ledger.unwrap_or_else(cc::Ledger::default_path));
    let all = ledger.read_all().context("reading ledger")?;
    let events: Vec<cc::RawEvent> = all
        .into_iter()
        .filter(|e| a.session.as_deref().is_none_or(|s| s == e.session_id))
        .filter(|e| e.seq >= a.from_seq.unwrap_or(0))
        .filter(|e| a.to_seq.is_none_or(|t| e.seq <= t))
        .collect();

    if events.is_empty() {
        println!("no events matched");
        return Ok(());
    }

    let digest = cc::build_digest(&events);
    let summary = match &a.llm_command {
        Some(cmd) => cc::run_piped(cmd, &[], &digest)
            .map_err(anyhow::Error::msg)
            .context("running --llm-command")?,
        None => digest.clone(),
    };

    if a.remember {
        let mut args: Vec<String> = vec!["remember".into(), "--kind".into(), "fact".into()];
        if let Some(s) = &a.subject {
            args.push("--subject".into());
            args.push(s.clone());
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = cc::run_piped(std::path::Path::new("tabularium"), &arg_refs, &summary)
            .map_err(anyhow::Error::msg)
            .context("calling `tabularium remember` -- is `tabularium` on PATH?")?;
        println!("{}", out.trim());
        return Ok(());
    }

    if a.json {
        println!(
            "{}",
            serde_json::json!({"digest": digest, "summary": summary})
        );
    } else {
        println!("{summary}");
    }
    Ok(())
}

fn cmd_resume(a: ResumeArgs) -> Result<()> {
    let ledger_path = a.ledger.unwrap_or_else(cc::Ledger::default_path);
    let checkpoints_path = a
        .checkpoints
        .unwrap_or_else(cc::CheckpointStore::default_path);
    println!(
        "{}",
        build_resume_report(
            &ledger_path,
            &checkpoints_path,
            a.session.as_deref(),
            a.json
        )?
    );
    Ok(())
}

/// Shared by `custos resume` and the `session-start` hook's stderr hint.
fn build_resume_report(
    ledger_path: &PathBuf,
    checkpoints_path: &PathBuf,
    session: Option<&str>,
    json: bool,
) -> Result<String> {
    let ledger = cc::Ledger::open(ledger_path);
    let events = ledger.read_all().context("reading ledger")?;
    let store = cc::CheckpointStore::open(checkpoints_path);
    let checkpoints = store.read_all().context("reading checkpoints")?;

    let relevant: Vec<&cc::SignedCheckpoint> = checkpoints
        .iter()
        .filter(|c| session.is_none_or(|s| c.body.session_ids.iter().any(|x| x == s)))
        .collect();
    let last = relevant.last();
    let last_to_seq = last.map(|c| c.body.to_seq);
    let pending: Vec<&cc::RawEvent> = events
        .iter()
        .filter(|e| last_to_seq.is_none_or(|t| e.seq > t))
        .collect();

    if json {
        return Ok(serde_json::json!({
            "total_checkpoints": checkpoints.len(),
            "last_checkpoint": last,
            "total_events": events.len(),
            "pending_since_last_checkpoint": pending.len(),
        })
        .to_string());
    }

    let mut out = String::new();
    out.push_str(&format!("checkpoints        {}\n", checkpoints.len()));
    match last {
        Some(c) => {
            out.push_str(&format!(
                "last checkpoint    {} ({})\n",
                c.body_digest, c.body.trigger
            ));
            out.push_str(&format!(
                "covers             seq {}..={} ({} events)\n",
                c.body.from_seq, c.body.to_seq, c.body.event_count
            ));
            out.push_str(&format!(
                "sessions           {}\n",
                c.body.session_ids.join(", ")
            ));
        }
        None => out.push_str("last checkpoint    none yet\n"),
    }
    out.push_str(&format!("total raw events   {}\n", events.len()));
    out.push_str(&format!("pending (uncheckpointed) {}", pending.len()));
    Ok(out)
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

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs()
}
