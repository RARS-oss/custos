//! custos-core — the hash-chained raw-event ledger + signed checkpoints. See `docs/DESIGN.md`
//! for scope and claims.

pub mod checkpoint;
pub mod crypto;
pub mod digest;
pub mod event;
pub mod external;
pub mod keys;
pub mod ledger;
mod lockfile;

pub use checkpoint::{
    sign as sign_checkpoint, verify_chain as verify_checkpoint_chain,
    verify_one as verify_checkpoint, CheckpointBody, CheckpointStore, NewCheckpoint,
    SignedCheckpoint, VerifyReport as CheckpointVerifyReport, ZERO_DIGEST,
};
pub use crypto::{generate_seed, pubkey_hex, seed_from_hex, seed_to_hex, sha256_hex};
pub use digest::build_digest;
pub use event::{extract_session_id, RawEvent};
pub use external::run_piped;
pub use keys::{default_key_path, load_or_create_seed, KeyError};
pub use ledger::{verify_chain as verify_ledger_chain, Ledger, ZERO_HASH};
