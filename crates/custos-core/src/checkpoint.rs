//! Signed checkpoints — the tamper-evident claim "everything from `from_seq` to `to_seq` in the
//! raw ledger was captured, and this receipt was produced at a real lifecycle boundary
//! (`trigger`), not invented after the fact." Ported from bulla's receipt shape (canonical body +
//! digest + pubkey + sig), same as vigil's scan receipts.
//!
//! Checkpoints chain to each other (`prev`, the previous checkpoint's digest) the same way raw
//! events chain — so dropping or reordering a whole checkpoint is as detectable as tampering
//! with one raw event.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::crypto::sha256_hex;
use crate::lockfile::with_lock;

pub const SCHEMA: &str = "custos-checkpoint/v0";
pub const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CheckpointBody {
    pub schema: String,
    pub created_ts: u64,
    /// What triggered this checkpoint: "pre_compact" | "session_end" | "manual".
    pub trigger: String,
    /// Range of raw-ledger `seq` this checkpoint covers, inclusive on both ends.
    pub from_seq: u64,
    pub to_seq: u64,
    pub event_count: u64,
    /// The raw ledger's chain head (the last covered event's hash) — ties this checkpoint to an
    /// exact, independently-recomputable position in `ledger.jsonl`.
    pub ledger_head: String,
    /// Distinct session ids seen in the covered range.
    pub session_ids: Vec<String>,
    /// Digest of the previous checkpoint, or `ZERO_DIGEST` for the first.
    pub prev: String,
}

impl CheckpointBody {
    pub fn canonical(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("checkpoint body serializes")
    }
    pub fn digest_hex(&self) -> String {
        sha256_hex(&self.canonical())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedCheckpoint {
    pub body: CheckpointBody,
    pub body_digest: String,
    pub pubkey: String,
    pub sig: String,
}

#[derive(Debug, Clone)]
pub struct VerifyReport {
    pub sig_ok: bool,
    pub digest_ok: bool,
    pub chain_ok: bool,
    pub notes: Vec<String>,
}

impl VerifyReport {
    pub fn intact(&self) -> bool {
        self.sig_ok && self.digest_ok && self.chain_ok
    }
}

/// Sign a checkpoint body with the given 32-byte Ed25519 seed.
pub fn sign(body: CheckpointBody, seed: &[u8; 32]) -> SignedCheckpoint {
    use ed25519_dalek::{Signer, SigningKey};
    let sk = SigningKey::from_bytes(seed);
    let vk = sk.verifying_key();
    let canonical = body.canonical();
    let sig = sk.sign(&canonical);
    SignedCheckpoint {
        body_digest: sha256_hex(&canonical),
        pubkey: hex::encode(vk.to_bytes()),
        sig: hex::encode(sig.to_bytes()),
        body,
    }
}

/// Verify one checkpoint's signature and digest field (not chain continuity across checkpoints —
/// see `verify_checkpoint_chain` for that).
pub fn verify_one(sc: &SignedCheckpoint) -> (bool, bool, Vec<String>) {
    let mut notes = Vec::new();
    let canonical = sc.body.canonical();

    let digest_ok = sha256_hex(&canonical) == sc.body_digest;
    if !digest_ok {
        notes.push("body_digest does not match the checkpoint body".into());
    }

    let sig_ok = (|| -> bool {
        use ed25519_dalek::{Signature, VerifyingKey};
        let Some(pk) = hex::decode(&sc.pubkey)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
        else {
            notes.push("public key is not 32 hex bytes".into());
            return false;
        };
        let Ok(vk) = VerifyingKey::from_bytes(&pk) else {
            notes.push("public key is not a valid Ed25519 point".into());
            return false;
        };
        let Some(sig_arr) = hex::decode(&sc.sig)
            .ok()
            .and_then(|b| <[u8; 64]>::try_from(b).ok())
        else {
            notes.push("signature is not 64 hex bytes".into());
            return false;
        };
        let sig = Signature::from_bytes(&sig_arr);
        match vk.verify_strict(&canonical, &sig) {
            Ok(()) => true,
            Err(_) => {
                notes.push("Ed25519 signature does not verify against the body".into());
                false
            }
        }
    })();

    (sig_ok, digest_ok, notes)
}

/// Verify a full sequence of checkpoints: each one's signature/digest, and that `prev` correctly
/// chains to the previous checkpoint's `body_digest`. Returns the first broken checkpoint's
/// `to_seq`, if any.
pub fn verify_chain(checkpoints: &[SignedCheckpoint]) -> Vec<VerifyReport> {
    let mut prev_digest = ZERO_DIGEST.to_string();
    let mut reports = Vec::with_capacity(checkpoints.len());
    for sc in checkpoints {
        let (sig_ok, digest_ok, mut notes) = verify_one(sc);
        let chain_ok = sc.body.prev == prev_digest;
        if !chain_ok {
            notes.push(format!(
                "prev digest mismatch: expected {prev_digest}, found {}",
                sc.body.prev
            ));
        }
        reports.push(VerifyReport {
            sig_ok,
            digest_ok,
            chain_ok,
            notes,
        });
        prev_digest = sc.body_digest.clone();
    }
    reports
}

/// Append-only storage for `SignedCheckpoint`s, one JSON line each — the same lock-guarded
/// append pattern as `ledger::Ledger`.
pub struct CheckpointStore {
    path: PathBuf,
}

impl CheckpointStore {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        CheckpointStore { path: path.into() }
    }

    /// `~/.custos/default/checkpoints.jsonl`.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        base.join(".custos")
            .join("default")
            .join("checkpoints.jsonl")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock_path(&self) -> PathBuf {
        self.path.with_extension("lock")
    }

    pub fn read_all(&self) -> std::io::Result<Vec<SignedCheckpoint>> {
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
            if let Ok(sc) = serde_json::from_str::<SignedCheckpoint>(&line) {
                out.push(sc);
            }
        }
        Ok(out)
    }

    /// The digest of the last checkpoint, or `ZERO_DIGEST` if none exist yet.
    pub fn last_digest(&self) -> std::io::Result<String> {
        Ok(self
            .read_all()?
            .last()
            .map(|sc| sc.body_digest.clone())
            .unwrap_or_else(|| ZERO_DIGEST.to_string()))
    }

    /// Append an already-signed checkpoint. Callers build `body.prev` from `last_digest()`
    /// themselves (under the same lock, via `append_new`) to avoid a race between reading the
    /// tail and writing a new entry.
    fn append_signed(&self, sc: &SignedCheckpoint) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(
            f,
            "{}",
            serde_json::to_string(sc).expect("checkpoint serializes")
        )?;
        Ok(())
    }

    /// Build, sign, and append a new checkpoint under the lock (so `prev` is always the true
    /// current tail even under concurrent callers).
    pub fn append_new(
        &self,
        params: NewCheckpoint<'_>,
        seed: &[u8; 32],
    ) -> std::io::Result<SignedCheckpoint> {
        with_lock(&self.lock_path(), || {
            let prev = self.last_digest()?;
            let body = CheckpointBody {
                schema: SCHEMA.into(),
                created_ts: params.created_ts,
                trigger: params.trigger.to_string(),
                from_seq: params.from_seq,
                to_seq: params.to_seq,
                event_count: params.event_count,
                ledger_head: params.ledger_head.to_string(),
                session_ids: params.session_ids,
                prev,
            };
            let sc = sign(body, seed);
            self.append_signed(&sc)?;
            Ok(sc)
        })
    }
}

/// Parameters for `CheckpointStore::append_new` — bundled to keep the call site from turning
/// into an unreadable pile of same-typed positional arguments.
pub struct NewCheckpoint<'a> {
    pub trigger: &'a str,
    pub from_seq: u64,
    pub to_seq: u64,
    pub event_count: u64,
    pub ledger_head: &'a str,
    pub session_ids: Vec<String>,
    pub created_ts: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_seed;

    fn sample_body(prev: &str) -> CheckpointBody {
        CheckpointBody {
            schema: SCHEMA.into(),
            created_ts: 1_700_000_000,
            trigger: "manual".into(),
            from_seq: 0,
            to_seq: 4,
            event_count: 5,
            ledger_head: "deadbeef".into(),
            session_ids: vec!["s1".into()],
            prev: prev.to_string(),
        }
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let seed = generate_seed();
        let sc = sign(sample_body(ZERO_DIGEST), &seed);
        let (sig_ok, digest_ok, notes) = verify_one(&sc);
        assert!(sig_ok && digest_ok, "{notes:?}");
    }

    #[test]
    fn tampering_the_body_breaks_the_signature() {
        let seed = generate_seed();
        let mut sc = sign(sample_body(ZERO_DIGEST), &seed);
        sc.body.event_count = 999;
        let (sig_ok, digest_ok, _) = verify_one(&sc);
        assert!(!sig_ok && !digest_ok);
    }

    #[test]
    fn chain_verifies_across_several_checkpoints() {
        let seed = generate_seed();
        let a = sign(sample_body(ZERO_DIGEST), &seed);
        let b = sign(sample_body(&a.body_digest), &seed);
        let c = sign(sample_body(&b.body_digest), &seed);
        let reports = verify_chain(&[a, b, c]);
        assert!(reports.iter().all(|r| r.intact()));
    }

    #[test]
    fn a_missing_checkpoint_breaks_the_chain() {
        let seed = generate_seed();
        let a = sign(sample_body(ZERO_DIGEST), &seed);
        let b = sign(sample_body(&a.body_digest), &seed);
        let c = sign(sample_body(&b.body_digest), &seed);
        // Drop b: c's prev no longer matches a's digest.
        let reports = verify_chain(&[a, c]);
        assert!(reports[0].intact());
        assert!(!reports[1].chain_ok);
    }

    fn temp_store(name: &str) -> CheckpointStore {
        let path = std::env::temp_dir()
            .join(format!(
                "custos-checkpoint-test-{name}-{}",
                std::process::id()
            ))
            .join("checkpoints.jsonl");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        CheckpointStore::open(path)
    }

    #[test]
    fn append_new_chains_prev_to_the_true_tail() {
        let store = temp_store("chain");
        let seed = generate_seed();
        let a = store
            .append_new(
                NewCheckpoint {
                    trigger: "manual",
                    from_seq: 0,
                    to_seq: 4,
                    event_count: 5,
                    ledger_head: "head0",
                    session_ids: vec!["s1".into()],
                    created_ts: 1,
                },
                &seed,
            )
            .unwrap();
        let b = store
            .append_new(
                NewCheckpoint {
                    trigger: "pre_compact",
                    from_seq: 5,
                    to_seq: 9,
                    event_count: 5,
                    ledger_head: "head1",
                    session_ids: vec!["s1".into()],
                    created_ts: 2,
                },
                &seed,
            )
            .unwrap();
        assert_eq!(a.body.prev, ZERO_DIGEST);
        assert_eq!(b.body.prev, a.body_digest);

        let all = store.read_all().unwrap();
        assert_eq!(all.len(), 2);
        let reports = verify_chain(&all);
        assert!(reports.iter().all(|r| r.intact()), "{reports:?}");

        std::fs::remove_dir_all(store.path().parent().unwrap()).ok();
    }

    #[test]
    fn empty_store_last_digest_is_zero() {
        let store = temp_store("empty");
        assert_eq!(store.last_digest().unwrap(), ZERO_DIGEST);
        std::fs::remove_dir_all(store.path().parent().unwrap()).ok();
    }
}
