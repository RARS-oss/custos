//! Default signing-key location and load-or-create — same pattern as vigil-core::keys.

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::crypto::{generate_seed, seed_from_hex, seed_to_hex};

#[derive(Debug, Error)]
pub enum KeyError {
    #[error("reading key {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing key {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("creating {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("key file {path} is neither 32 raw bytes nor 64 hex chars")]
    BadFormat { path: PathBuf },
}

/// `~/.custos/default/identity.seed` (or `$TEMP/.custos/default/identity.seed`).
pub fn default_key_path() -> PathBuf {
    let base = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(".custos").join("default").join("identity.seed")
}

pub fn load_or_create_seed(path: &Path) -> Result<[u8; 32], KeyError> {
    if path.exists() {
        let raw = fs::read(path).map_err(|source| KeyError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if raw.len() == 32 {
            return Ok(<[u8; 32]>::try_from(raw).unwrap());
        }
        let txt = String::from_utf8_lossy(&raw);
        if let Some(seed) = seed_from_hex(&txt) {
            return Ok(seed);
        }
        return Err(KeyError::BadFormat {
            path: path.to_path_buf(),
        });
    }
    let seed = generate_seed();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| KeyError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(path, seed_to_hex(&seed)).map_err(|source| KeyError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_or_create_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("custos-keys-test-{}", std::process::id()));
        let path = dir.join("identity.seed");
        let _ = fs::remove_dir_all(&dir);

        let a = load_or_create_seed(&path).unwrap();
        let b = load_or_create_seed(&path).unwrap();
        assert_eq!(a, b);

        fs::remove_dir_all(&dir).ok();
    }
}
