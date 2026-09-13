//! custos-core — the hash-chained raw-event ledger. See `docs/DESIGN.md` for scope and claims.

pub mod event;
pub mod ledger;

pub use event::{extract_session_id, RawEvent};
pub use ledger::{verify_chain, Ledger, ZERO_HASH};
