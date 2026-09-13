//! An append-only, hash-chained log of `RawEvent`s — one JSON line per event, in the same
//! construction bulla/tabularium/vigil all use elsewhere in this org (`prev`/`hash`, sha256).
//!
//! Storage is a flat JSONL file, not SQLite (tabularium's choice, made for exactly the
//! concurrent-writer problem this file also has to solve): a lightweight create-new lock file
//! guards the read-then-append critical section against two hook processes racing on the same
//! ledger. It backs off with a deadline and then proceeds anyway rather than blocking forever —
//! consistent with "never fail the hook" (`docs/DESIGN.md` §4): a stuck lock must never turn into
//! a hung or broken Claude Code session. A lock this simple does not guarantee perfect ordering
//! under heavy concurrent load; that is a documented v1 limitation, not a silent one.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::event::RawEvent;

pub const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub struct Ledger {
    path: PathBuf,
}

impl Ledger {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Ledger { path: path.into() }
    }

    /// `~/.custos/default/ledger.jsonl` (or `$TEMP/.custos/default/ledger.jsonl` if neither
    /// `HOME` nor `USERPROFILE` is set).
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        base.join(".custos").join("default").join("ledger.jsonl")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock_path(&self) -> PathBuf {
        self.path.with_extension("lock")
    }

    /// Every event in the ledger, in order. A line that fails to parse (a partial write from a
    /// crashed process, hand-editing) is skipped rather than aborting the whole read — reading
    /// the ledger must never itself become a way to fail a hook.
    pub fn read_all(&self) -> std::io::Result<Vec<RawEvent>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let f = File::open(&self.path)?;
        let reader = BufReader::new(f);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<RawEvent>(&line) {
                out.push(ev);
            }
        }
        Ok(out)
    }

    /// The hash of the last event in the ledger, or `ZERO_HASH` if it's empty.
    pub fn head(&self) -> std::io::Result<String> {
        Ok(self
            .read_all()?
            .last()
            .map(|e| e.hash.clone())
            .unwrap_or_else(|| ZERO_HASH.to_string()))
    }

    /// Append one new event, computing `seq`/`prev`/`hash` from the current chain tail under a
    /// short-lived lock. Returns the event that was written.
    pub fn append(
        &self,
        kind: &str,
        session_id: &str,
        payload: Value,
    ) -> std::io::Result<RawEvent> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        with_lock(&self.lock_path(), || {
            let existing = self.read_all()?;
            let seq = existing.len() as u64;
            let prev = existing
                .last()
                .map(|e| e.hash.clone())
                .unwrap_or_else(|| ZERO_HASH.to_string());
            let ts = now_epoch();
            let hash = compute_hash(seq, kind, session_id, &payload, &prev, ts);
            let ev = RawEvent {
                seq,
                kind: kind.to_string(),
                session_id: session_id.to_string(),
                ts,
                payload,
                prev,
                hash,
            };
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            writeln!(
                f,
                "{}",
                serde_json::to_string(&ev).expect("event serializes")
            )?;
            Ok(ev)
        })
    }
}

/// Recompute the chain from genesis and report whether every `prev`/`hash` matches. `Ok(None)`
/// means intact; `Ok(Some(seq))` names the first event whose hash doesn't recompute (dropped,
/// reordered, or edited).
pub fn verify_chain(events: &[RawEvent]) -> Option<u64> {
    let mut prev = ZERO_HASH.to_string();
    for (i, e) in events.iter().enumerate() {
        let recomputed = compute_hash(e.seq, &e.kind, &e.session_id, &e.payload, &prev, e.ts);
        if e.seq != i as u64 || e.prev != prev || e.hash != recomputed {
            return Some(e.seq);
        }
        prev = e.hash.clone();
    }
    None
}

fn compute_hash(
    seq: u64,
    kind: &str,
    session_id: &str,
    payload: &Value,
    prev: &str,
    ts: u64,
) -> String {
    #[derive(Serialize)]
    struct Core<'a> {
        seq: u64,
        kind: &'a str,
        session_id: &'a str,
        payload: &'a Value,
        prev: &'a str,
        ts: u64,
    }
    let core = Core {
        seq,
        kind,
        session_id,
        payload,
        prev,
        ts,
    };
    let bytes = serde_json::to_vec(&core).expect("event core serializes");
    sha256_hex(&bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs()
}

/// A create-new-based spin lock: atomic on both POSIX and Windows. Backs off with a deadline and
/// then proceeds WITHOUT the lock rather than hanging — a stuck/stale lock file (from a crashed
/// process) must never turn into a hook that never returns.
fn with_lock<T>(lock_path: &Path, f: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if Instant::now() > deadline {
                    break; // proceed unlocked rather than hang
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break, // couldn't even create the lock file -- proceed unlocked
        }
    }
    let result = f();
    let _ = fs::remove_file(lock_path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ledger(name: &str) -> Ledger {
        let path = std::env::temp_dir()
            .join(format!("custos-ledger-test-{name}-{}", std::process::id()))
            .join("ledger.jsonl");
        let _ = fs::remove_dir_all(path.parent().unwrap());
        Ledger::open(path)
    }

    #[test]
    fn append_and_read_roundtrip() {
        let l = temp_ledger("roundtrip");
        l.append(
            "user_prompt_submit",
            "s1",
            serde_json::json!({"prompt": "hi"}),
        )
        .unwrap();
        l.append(
            "stop",
            "s1",
            serde_json::json!({"last_assistant_message": "hello"}),
        )
        .unwrap();
        let events = l.read_all().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 0);
        assert_eq!(events[1].seq, 1);
        assert_eq!(events[1].prev, events[0].hash);
        fs::remove_dir_all(l.path().parent().unwrap()).ok();
    }

    #[test]
    fn empty_ledger_head_is_zero_hash() {
        let l = temp_ledger("empty");
        assert_eq!(l.head().unwrap(), ZERO_HASH);
    }

    #[test]
    fn chain_verifies_intact_after_several_appends() {
        let l = temp_ledger("intact");
        for i in 0..5 {
            l.append("post_tool_use", "s1", serde_json::json!({"i": i}))
                .unwrap();
        }
        let events = l.read_all().unwrap();
        assert_eq!(verify_chain(&events), None);
        fs::remove_dir_all(l.path().parent().unwrap()).ok();
    }

    #[test]
    fn tampering_an_event_is_detected() {
        let l = temp_ledger("tamper");
        l.append(
            "user_prompt_submit",
            "s1",
            serde_json::json!({"prompt": "a"}),
        )
        .unwrap();
        l.append(
            "stop",
            "s1",
            serde_json::json!({"last_assistant_message": "b"}),
        )
        .unwrap();
        let mut events = l.read_all().unwrap();
        events[0].payload = serde_json::json!({"prompt": "TAMPERED"});
        assert_eq!(verify_chain(&events), Some(0));
        fs::remove_dir_all(l.path().parent().unwrap()).ok();
    }

    #[test]
    fn dropping_an_event_breaks_the_chain() {
        let l = temp_ledger("drop");
        for i in 0..3 {
            l.append("post_tool_use", "s1", serde_json::json!({"i": i}))
                .unwrap();
        }
        let mut events = l.read_all().unwrap();
        events.remove(1);
        assert!(verify_chain(&events).is_some());
        fs::remove_dir_all(l.path().parent().unwrap()).ok();
    }

    #[test]
    fn session_id_is_extracted_when_present() {
        let l = temp_ledger("session-id");
        let ev = l
            .append(
                "user_prompt_submit",
                "abc-123",
                serde_json::json!({"session_id": "abc-123", "prompt": "hi"}),
            )
            .unwrap();
        assert_eq!(ev.session_id, "abc-123");
        fs::remove_dir_all(l.path().parent().unwrap()).ok();
    }
}
