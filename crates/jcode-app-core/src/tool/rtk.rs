//! Native Jcode integration for the upstream `rtk` (Rust Token Killer) CLI proxy.
//!
//! rtk (https://github.com/rtk-ai/rtk) is a CLI that wraps common dev commands
//! (`ls`, `cat`, `grep`, `find`, `git`, build/test runners) and filters,
//! groups, truncates, and deduplicates their output before it reaches the
//! model, cutting 60-90% of tokens on data-heavy shell operations.
//!
//! In Claude Code rtk installs a `PreToolUse` hook that transparently rewrites
//! `Bash` commands to `rtk <cmd>`. Jcode has no general PreToolUse rewrite
//! hook, so the equivalent native integration is a routing block in
//! `~/.jcode/preferred-tools.md` that instructs the agent to invoke `rtk`
//! explicitly for the same command families, plus binary detection so the
//! agent knows whether rtk is available.

use super::integration_support as support;
use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;

const BLOCK_START: &str = "<!-- JCODE_RTK_START -->";
const BLOCK_END: &str = "<!-- JCODE_RTK_END -->";

pub struct RtkTool;

impl RtkTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Deserialize)]
struct RtkInput {
    #[serde(default = "default_action")]
    action: String,
    #[serde(default)]
    overwrite: Option<bool>,
}

fn default_action() -> String {
    "status".to_string()
}

#[derive(Debug, Serialize)]
struct RtkManifest {
    migrated_at: String,
    preferred_tools: bool,
    rtk_detected: bool,
    rtk_version: Option<String>,
    rtk_path: Option<String>,
}

#[async_trait]
impl Tool for RtkTool {
    fn name(&self) -> &str {
        "rtk"
    }

    fn description(&self) -> &str {
        "Set up or inspect native Jcode integration for the rtk (Rust Token Killer) CLI proxy: command-routing rules and binary detection so data-heavy shell output is compressed before it enters context."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["status", "setup", "check"],
                    "description": "Action. setup writes the rtk routing block into ~/.jcode/preferred-tools.md and records detection. check probes for the rtk binary. status reports current integration state."
                },
                "overwrite": {
                    "type": "boolean",
                    "description": "Replace an existing Jcode-managed rtk routing block. Defaults to false."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let params: RtkInput = serde_json::from_value(input)?;
        let overwrite = params.overwrite.unwrap_or(false);
        match params.action.as_str() {
            "status" => status_output().map(|out| out.with_title("rtk status")),
            "check" => Ok(check_output()).map(|out| out.with_title("rtk check")),
            "setup" => setup_rtk(overwrite).map(|out| out.with_title("rtk setup")),
            other => Ok(ToolOutput::new(format!(
                "Unknown action: {other}. Use status, setup, or check."
            ))),
        }
    }
}

fn manifest_path() -> Result<PathBuf> {
    Ok(support::jcode_dir()?.join("rtk").join("manifest.json"))
}

/// True when rtk integration has been set up (manifest present). Used by the
/// PreToolUse rewrite hook to decide whether to activate.
pub(crate) fn integration_enabled() -> bool {
    manifest_path().map(|p| p.exists()).unwrap_or(false)
}

/// Probe for the rtk binary. Returns (path, version) when found.
pub(crate) fn detect_rtk() -> Option<(PathBuf, Option<String>)> {
    // `which`-style resolution via the OS, but avoid a hard `which` dependency.
    let exe = if cfg!(windows) { "rtk.exe" } else { "rtk" };
    let path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(exe))
            .find(|candidate| candidate.is_file())
    })?;

    let version = Command::new(&path)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    Some((path, version))
}

fn rtk_preferred_tools_block() -> String {
    format!(
        r#"{BLOCK_START}
# rtk (Rust Token Killer) Routing for Jcode

Jcode is integrated with the upstream `rtk` CLI proxy. When `rtk` is available on PATH, prefer routing data-heavy shell operations through it so filtered/compressed output (60-90% fewer tokens) reaches the model instead of raw bytes.

Rules adapted from https://github.com/rtk-ai/rtk:

- Files: `rtk ls <dir>`, `rtk read <file>` (add `-l aggressive` for signatures only), `rtk find "<glob>" <dir>`, `rtk grep "<pattern>" <dir>`, `rtk diff <a> <b>`.
- Git: `rtk git status`, `rtk git log -n <N>`, `rtk git diff`. Mutating git subcommands collapse to a short confirmation.
- Builds/tests: prefix the runner (e.g. `rtk cargo test`, `rtk npm test`) to get grouped errors and deduplicated logs.
- Analytics: `rtk gain` for token savings, `rtk discover` to find missed opportunities, `rtk proxy <cmd>` to run a command unfiltered for debugging.

Jcode native mapping:
- `bash` invocations of `ls`/`cat`/`head`/`tail` over large trees or files -> `rtk ls` / `rtk read`.
- broad `grep`/`find`/recursive scans in `bash` -> `rtk grep` / `rtk find`.
- `git status`/`git log`/`git diff` in `bash` -> the `rtk git ...` equivalents.
- test/build runs whose logs are large -> prefix with `rtk`.
- Mutations, tiny outputs, and exact line ranges can stay on plain `bash`/`read`/`grep`.

If `rtk` is not installed, install it (`brew install rtk`, `cargo install --git https://github.com/rtk-ai/rtk`, or the install script) and fall back to plain tools until then. Name-collision guard: `rtk gain` must work; if it errors you have the unrelated "Rust Type Kit" crate instead.
{BLOCK_END}"#
    )
}

fn write_manifest(manifest: &RtkManifest) -> Result<()> {
    let path = manifest_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(manifest)?)?;
    Ok(())
}

fn setup_rtk(overwrite: bool) -> Result<ToolOutput> {
    let preferred_changed = support::ensure_preferred_tools_block(
        &rtk_preferred_tools_block(),
        BLOCK_START,
        BLOCK_END,
        overwrite,
    )?;
    let detected = detect_rtk();
    let manifest = RtkManifest {
        migrated_at: chrono::Utc::now().to_rfc3339(),
        preferred_tools: true,
        rtk_detected: detected.is_some(),
        rtk_version: detected.as_ref().and_then(|(_, v)| v.clone()),
        rtk_path: detected.as_ref().map(|(p, _)| p.display().to_string()),
    };
    write_manifest(&manifest)?;

    let mut lines = vec![
        "rtk native Jcode setup complete.".to_string(),
        format!(
            "- Routing rules in {}: {}",
            support::preferred_tools_path()?.display(),
            if preferred_changed {
                "written"
            } else {
                "already present"
            }
        ),
    ];
    match detected {
        Some((path, Some(version))) => {
            lines.push(format!(
                "- rtk binary detected: {} ({version})",
                path.display()
            ));
        }
        Some((path, None)) => {
            lines.push(format!(
                "- rtk binary detected: {} (version unknown; ensure `rtk gain` works)",
                path.display()
            ));
        }
        None => {
            lines.push(
                "- rtk binary NOT found on PATH. Install via `brew install rtk`, `cargo install --git https://github.com/rtk-ai/rtk`, or the upstream install script. The routing rules activate once rtk is available."
                    .to_string(),
            );
        }
    }
    Ok(ToolOutput::new(lines.join("\n")))
}

fn check_output() -> ToolOutput {
    match detect_rtk() {
        Some((path, Some(version))) => ToolOutput::new(format!(
            "rtk available: {} ({version})",
            path.display()
        )),
        Some((path, None)) => ToolOutput::new(format!(
            "rtk binary found at {} but `--version` did not return output. Verify `rtk gain` works (possible name collision with the unrelated Rust Type Kit crate).",
            path.display()
        )),
        None => ToolOutput::new(
            "rtk not found on PATH. Install via `brew install rtk`, `cargo install --git https://github.com/rtk-ai/rtk`, or the upstream install script.".to_string(),
        ),
    }
}

fn status_output() -> Result<ToolOutput> {
    let preferred_present = support::preferred_tools_block_present(BLOCK_START);
    let manifest = manifest_path()?;
    let manifest_present = manifest.exists();
    let detected = detect_rtk();

    Ok(ToolOutput::new(format!(
        "rtk native Jcode status:\n- Routing block installed: {} ({})\n- rtk binary on PATH: {}{}\n- Migration manifest present: {} ({})",
        support::yes_no(preferred_present),
        support::preferred_tools_path()?.display(),
        support::yes_no(detected.is_some()),
        match &detected {
            Some((path, Some(version))) => format!(" ({}, {version})", path.display()),
            Some((path, None)) => format!(" ({})", path.display()),
            None => String::new(),
        },
        support::yes_no(manifest_present),
        manifest.display(),
    )))
}

#[cfg(test)]
mod tests;
