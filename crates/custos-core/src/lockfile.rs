//! A create-new-based spin lock, shared by `ledger::Ledger` and `checkpoint::CheckpointStore`:
//! atomic on both POSIX and Windows. Backs off with a deadline and then proceeds WITHOUT the
//! lock rather than hanging — a stuck/stale lock file (from a crashed process) must never turn
//! into a hook that never returns ("never fail the hook", docs/DESIGN.md §4).

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::time::{Duration, Instant};

pub fn with_lock<T>(
    lock_path: &Path,
    f: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
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
