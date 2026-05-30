//! Shared helpers for native tools that wrap an external CLI binary.
//!
//! Several jcode-native tools (memex, rtk, context-mode, caveman) drive an
//! external program in-process rather than going through an MCP round-trip.
//! They all need the same two things:
//!
//! 1. Robust binary discovery that works even when the agent's `PATH` does not
//!    include the user's nvm / cargo / homebrew / local bin directories (the
//!    agent process often inherits a reduced environment).
//! 2. A small async command runner that captures stdout/stderr/exit status and
//!    formats failures consistently.
//!
//! Keeping this in one module avoids duplicating the discovery + run logic in
//! each tool and gives every wrapper the same graceful "binary not installed"
//! behavior.

use crate::tool::ToolOutput;
use anyhow::Result;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

/// Directories that commonly hold user-installed CLIs but are frequently
/// missing from a spawned agent's `PATH`. Searched in order after `PATH`.
fn extra_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs_home() {
        // Cargo / rustup installed binaries (rtk).
        dirs.push(home.join(".cargo/bin"));
        // pipx / user pip installs and generic local bin (caveman, dw, etc.).
        dirs.push(home.join(".local/bin"));
        // pyenv shims.
        dirs.push(home.join(".pyenv/shims"));
        // nvm: search every installed node version's bin (memex, context-mode).
        let nvm_versions = home.join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(&nvm_versions) {
            for entry in entries.flatten() {
                dirs.push(entry.path().join("bin"));
            }
        }
    }
    // Homebrew (Apple silicon + Intel) and standard system locations.
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/usr/bin"));
    dirs
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Locate an executable by name. Honors `PATH` first, then a set of common
/// user install directories. Returns the absolute path if found.
pub fn find_binary(name: &str) -> Option<PathBuf> {
    // 1. Honor PATH via the `which` semantics implemented inline (no extra dep).
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    // 2. Fall back to known install directories.
    for dir in extra_search_dirs() {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(meta) => meta.permissions().mode() & 0o111 != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Result of running an external CLI command.
pub struct CliResult {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CliResult {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }

    /// Combine stdout and stderr into a single human-readable string, trimming
    /// trailing whitespace. Used when the caller wants everything the CLI
    /// emitted regardless of stream.
    pub fn combined(&self) -> String {
        let mut out = String::new();
        let stdout = self.stdout.trim_end();
        let stderr = self.stderr.trim_end();
        if !stdout.is_empty() {
            out.push_str(stdout);
        }
        if !stderr.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(stderr);
        }
        out
    }
}

/// Run an external binary with the given arguments. Optionally pipe `stdin`
/// into the process. Captures stdout/stderr and the exit code.
pub async fn run(
    binary: &std::path::Path,
    args: &[String],
    stdin: Option<&str>,
    cwd: Option<&std::path::Path>,
) -> Result<CliResult> {
    let mut command = Command::new(binary);
    command.args(args);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }

    let mut child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to launch {}: {e}", binary.display()))?;

    if let Some(input) = stdin {
        use tokio::io::AsyncWriteExt;
        if let Some(mut handle) = child.stdin.take() {
            handle.write_all(input.as_bytes()).await?;
            handle.shutdown().await.ok();
        }
    }

    let output = child.wait_with_output().await?;
    Ok(CliResult {
        status: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Format a [`CliResult`] into a [`ToolOutput`] with a consistent title and a
/// clear failure message that includes the exit code. Shared by every native
/// CLI wrapper so success/failure rendering is uniform.
pub fn format_cli_result(tool: &str, action: &str, result: CliResult) -> ToolOutput {
    let body = result.combined();
    if result.success() {
        let body = if body.is_empty() {
            format!("{tool} {action}: done (no output).")
        } else {
            body
        };
        ToolOutput::new(body).with_title(format!("{tool} {action}"))
    } else {
        let code = result
            .status
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        let detail = if body.is_empty() {
            "(no output)".to_string()
        } else {
            body
        };
        ToolOutput::new(format!("{tool} {action} failed (exit {code}):\n{detail}"))
            .with_title(format!("{tool} {action} failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_binary_resolves_common_unix_tool() {
        // `sh` exists on every unix CI runner and is on PATH.
        #[cfg(unix)]
        {
            let found = find_binary("sh");
            assert!(found.is_some(), "expected to find sh on PATH");
        }
    }

    #[test]
    fn find_binary_returns_none_for_nonexistent() {
        assert!(find_binary("this-binary-does-not-exist-xyz123").is_none());
    }

    #[tokio::test]
    async fn run_captures_stdout_and_status() {
        #[cfg(unix)]
        {
            let sh = find_binary("sh").expect("sh present");
            let result = run(&sh, &["-c".into(), "printf hello".into()], None, None)
                .await
                .expect("run sh");
            assert!(result.success());
            assert_eq!(result.stdout.trim(), "hello");
        }
    }

    #[tokio::test]
    async fn run_pipes_stdin() {
        #[cfg(unix)]
        {
            let cat = find_binary("cat").expect("cat present");
            let result = run(&cat, &[], Some("piped-input"), None)
                .await
                .expect("run cat");
            assert!(result.success());
            assert_eq!(result.stdout.trim(), "piped-input");
        }
    }
}
