//! `context_snr` tool: a Context Signal-to-Noise routing layer.
//!
//! This does not add a new sandbox; it documents and detects the upstream CLIs
//! that improve the *signal-to-noise* of what reaches the model, then writes a
//! routing block into `~/.jcode/preferred-tools.md` (the same mechanism rtk uses)
//! so the agent prefers compressed/token-counted surfaces over raw bytes.
//!
//! Surveyed tools (all optional, detected on PATH):
//! - **repomix** / **ai-digest**: whole-repo -> single compressed, token-counted
//!   digest for retrieval.
//! - **ttok** (Simon Willison): exact token counting to drive truncation
//!   decisions (jcode's overflow guard currently estimates chars/4).
//! - **ast-grep** (`sg`): structural code search with tiny output.
//! - **jsbash repo_digest**: a built-in fallback that builds a compact tree +
//!   head-of-file digest entirely inside the `jsbash` sandbox (no external CLI).
//!
//! Actions: `setup` (write the routing block), `status` (report detection).

use super::integration_support as support;
use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

const BLOCK_START: &str = "<!-- JCODE_CONTEXT_SNR_START -->";
const BLOCK_END: &str = "<!-- JCODE_CONTEXT_SNR_END -->";

/// External CLIs surveyed for context SNR. (binary, what it does)
const SURVEYED: &[(&str, &str)] = &[
    ("repomix", "whole-repo -> single token-counted digest"),
    ("ai-digest", "repo/dir -> single markdown digest"),
    ("ttok", "exact LLM token counting for truncation decisions"),
    ("ast-grep", "structural (AST) code search, tiny output"),
    ("sg", "ast-grep alias"),
];

pub struct ContextSnrTool;

impl ContextSnrTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ContextSnrTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct Input {
    #[serde(default = "default_action")]
    action: String,
    #[serde(default)]
    overwrite: Option<bool>,
}

fn default_action() -> String {
    "status".to_string()
}

#[async_trait]
impl Tool for ContextSnrTool {
    fn name(&self) -> &str {
        "context_snr"
    }

    fn description(&self) -> &str {
        "Context signal-to-noise routing: detect and prefer token-frugal surfaces (repomix/ai-digest repo digests, ttok exact token counts, ast-grep structural search) and the built-in jsbash repo_digest fallback, keeping raw bytes out of context. Actions: setup (write routing guidance into preferred-tools.md), status (report which tools are present)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["setup", "status"],
                    "description": "setup writes the context-snr routing block into ~/.jcode/preferred-tools.md; status reports which surveyed tools are on PATH."
                },
                "overwrite": {"type": "boolean", "description": "For setup: rewrite the block even if already present."}
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let params: Input = serde_json::from_value(input)?;
        match params.action.as_str() {
            "setup" => {
                setup(params.overwrite.unwrap_or(false)).map(|o| o.with_title("context_snr setup"))
            }
            "status" => Ok(status().with_title("context_snr status")),
            other => Ok(ToolOutput::new(format!(
                "Unknown action: {other}. Use setup or status."
            ))),
        }
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_string()
    };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(&exe))
            .find(|c| c.is_file())
    })
}

fn routing_block() -> String {
    format!(
        r#"{BLOCK_START}
# Context SNR (signal-to-noise) Routing for Jcode

Prefer token-frugal surfaces so compressed/relevant content reaches the model instead of raw bytes. These tools are optional; use them when present, otherwise fall back to native jcode tools.

- Whole-repo understanding: `repomix` (or `ai-digest`) to produce a single token-counted digest, instead of cat-ing many files. Pipe the digest, not the tree.
- Token budgeting: `ttok` for exact token counts before deciding to truncate or summarize (more accurate than chars/4 estimates).
- Structural code search: `ast-grep`/`sg` for AST-level matches with tiny output, complementing jcode's `codesearch`/`agentgrep` for literal/lexical search.
- Sandboxed digest fallback (no external CLI): use the `jsbash` tool to build a compact digest inside the virtual fs, e.g. list the tree and print only the head of each file:
  `jsbash exec` with a script like:
  `find . -type f | head -200 | while read f; do echo "== $f =="; head -40 "$f"; done`
  Reads come from the workspace overlay; nothing mutates the host.

Native mapping:
- "summarize/understand this repo" -> repomix digest (or jsbash repo_digest fallback), not raw `cat`/`ls -R`.
- "find where X is defined/used structurally" -> ast-grep, then jcode `codesearch`.
- before pasting large output -> count with `ttok` and prefer rtk/caveman-filtered or jsbash-reduced output.
{BLOCK_END}"#
    )
}

fn detected_lines() -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    SURVEYED
        .iter()
        .filter_map(|(bin, desc)| {
            // ast-grep and sg are the same tool; report once.
            let canonical = if *bin == "sg" { "ast-grep" } else { *bin };
            if !seen.insert(canonical.to_string()) {
                return None;
            }
            let path = which(bin).or_else(|| {
                if *bin == "ast-grep" {
                    which("sg")
                } else {
                    None
                }
            });
            Some(format!(
                "- {canonical}: {} ({desc})",
                path.map(|p| p.display().to_string())
                    .unwrap_or_else(|| "not found".into())
            ))
        })
        .collect()
}

fn setup(overwrite: bool) -> Result<ToolOutput> {
    let changed =
        support::ensure_preferred_tools_block(&routing_block(), BLOCK_START, BLOCK_END, overwrite)?;
    let mut lines = vec![format!(
        "context-snr routing block {} in preferred-tools.md",
        if changed {
            "written"
        } else {
            "already present (use overwrite=true to refresh)"
        }
    )];
    lines.push("Surveyed tools (jsbash repo_digest fallback always available):".to_string());
    lines.extend(detected_lines());
    Ok(ToolOutput::new(lines.join("\n")))
}

fn status() -> ToolOutput {
    let present = support::preferred_tools_block_present(BLOCK_START);
    let mut lines = vec![format!(
        "context-snr routing block present: {}",
        if present {
            "yes"
        } else {
            "no (run action=setup)"
        }
    )];
    lines.push("Surveyed tools:".to_string());
    lines.extend(detected_lines());
    lines.push("- jsbash repo_digest: always available (built-in sandbox fallback)".to_string());
    ToolOutput::new(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolExecutionMode;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "s".into(),
            message_id: "m".into(),
            tool_call_id: "c".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    #[test]
    fn routing_block_has_markers_and_tools() {
        let b = routing_block();
        assert!(b.contains(BLOCK_START));
        assert!(b.contains(BLOCK_END));
        assert!(b.contains("repomix"));
        assert!(b.contains("ttok"));
        assert!(b.contains("ast-grep"));
        assert!(b.contains("jsbash"));
    }

    #[test]
    fn detected_lines_dedupe_ast_grep() {
        let lines = detected_lines();
        let ast = lines.iter().filter(|l| l.contains("ast-grep")).count();
        assert_eq!(ast, 1, "ast-grep/sg should report once");
        // No raw "sg:" line.
        assert!(!lines.iter().any(|l| l.starts_with("- sg:")));
    }

    #[tokio::test]
    async fn status_action_never_fails() {
        let _guard = crate::storage::lock_test_env();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("JCODE_HOME", tmp.path());
        }
        let tool = ContextSnrTool::new();
        let out = tool.execute(json!({"action": "status"}), ctx()).await;
        unsafe {
            std::env::remove_var("JCODE_HOME");
        }
        let out = out.unwrap();
        assert!(out.output.contains("context-snr routing block present"));
        assert!(out.output.contains("jsbash repo_digest"));
    }

    #[tokio::test]
    async fn setup_writes_block_and_is_idempotent() {
        let _guard = crate::storage::lock_test_env();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("JCODE_HOME", tmp.path());
        }
        let tool = ContextSnrTool::new();
        let first = tool
            .execute(json!({"action": "setup"}), ctx())
            .await
            .unwrap();
        let second = tool
            .execute(json!({"action": "setup"}), ctx())
            .await
            .unwrap();
        let pref = tmp.path().join("preferred-tools.md");
        let content = std::fs::read_to_string(&pref).unwrap_or_default();
        let count = content.matches(BLOCK_START).count();
        unsafe {
            std::env::remove_var("JCODE_HOME");
        }
        assert!(first.output.contains("written"));
        assert!(second.output.contains("already present"));
        assert_eq!(count, 1, "block must not be duplicated");
    }
}
