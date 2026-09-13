//! Crypto primitives — ported from bulla-core/vigil-core (same Ed25519 + sha256 approach, same
//! dependency versions), so a checkpoint receipt is checkable the same way a bulla or vigil
//! receipt is.

use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// 32 fresh random bytes for a new Ed25519 signing seed.
pub fn generate_seed() -> [u8; 32] {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).expect("os rng");
    seed
}

pub fn seed_to_hex(seed: &[u8; 32]) -> String {
    hex::encode(seed)
}

pub fn seed_from_hex(s: &str) -> Option<[u8; 32]> {
    hex::decode(s.trim())
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
}

pub fn pubkey_hex(seed: &[u8; 32]) -> String {
    use ed25519_dalek::SigningKey;
    hex::encode(SigningKey::from_bytes(seed).verifying_key().to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_hex_roundtrip() {
        let seed = generate_seed();
        assert_eq!(seed_from_hex(&seed_to_hex(&seed)), Some(seed));
    }

    #[test]
    fn pubkey_is_deterministic_for_a_seed() {
        let seed = generate_seed();
        assert_eq!(pubkey_hex(&seed), pubkey_hex(&seed));
    }
}
