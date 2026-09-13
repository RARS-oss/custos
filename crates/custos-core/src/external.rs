//! Spawn an external program, pipe text in via stdin, capture stdout — never through a shell
//! string, so captured content (a raw event's payload, an LLM-produced summary) can never be
//! interpreted as shell syntax. Same safety principle as vigil's `ExternalCommandTransform`
//! (`crates/vigil-core/src/sink.rs`), reused here for the same reason: this agent's own session
//! got bitten by exactly this class of bug writing to tabularium from a shell string (see the
//! project.custos-status memory) — custos's own consolidation step must not repeat it.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Run `program args...`, write `input` to its stdin, and return its stdout as a `String`.
/// Fails on a non-zero exit or non-UTF-8 output; the process's stderr is folded into the error
/// message for diagnosis.
pub fn run_piped(program: &Path, args: &[&str], input: &str) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start {}: {e}", program.display()))?;

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(input.as_bytes())
        .map_err(|e| format!("failed to write to {}: {e}", program.display()))?;

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for {}: {e}", program.display()))?;

    if !output.status.success() {
        return Err(format!(
            "{} exited with {}: {}",
            program.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|e| format!("{} produced non-UTF-8 output: {e}", program.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn echo_program() -> Option<PathBuf> {
        // Cross-platform-ish smoke test: prefer a real `cat`-like passthrough if present.
        which("cat").or_else(|| which("more"))
    }

    fn which(name: &str) -> Option<PathBuf> {
        let path_var = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
            let candidate_exe = dir.join(format!("{name}.exe"));
            if candidate_exe.is_file() {
                return Some(candidate_exe);
            }
        }
        None
    }

    #[test]
    fn run_piped_roundtrips_through_a_real_passthrough_command() {
        let Some(prog) = echo_program() else {
            eprintln!("skipping: no cat/more found on PATH");
            return;
        };
        let out = run_piped(&prog, &[], "hello custos\n").unwrap();
        assert!(out.contains("hello custos"));
    }

    #[test]
    fn nonexistent_program_is_a_clean_error_not_a_panic() {
        let err = run_piped(Path::new("this-program-does-not-exist-custos"), &[], "x").unwrap_err();
        assert!(err.contains("failed to start"));
    }
}
