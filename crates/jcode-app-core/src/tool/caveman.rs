//! Native `caveman` tool.
//!
//! Caveman compresses natural-language markdown (CLAUDE.md, todos, notes) into
//! ultra-terse "caveman" form to cut input tokens ~75% while preserving code,
//! URLs, headings, and technical substance. Compression is LLM-based: it calls
//! the Anthropic API (or the `claude` CLI) to rewrite the file in place and
//! writes a human-readable `<name>.original.md` backup.
//!
//! The compressor ships as a Python module (`caveman-compress/scripts`) rather
//! than a binary on PATH, so this tool locates the module directory and runs
//! `python3 -m scripts <file>` from there. Because compression is destructive
//! (rewrites the file) and sends bytes to a third-party API, the tool requires
//! an explicit `confirmed: true` before acting.

use super::external_cli;
use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub struct CavemanTool;

impl CavemanTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct CavemanInput {
    action: String,
    /// Path to the markdown file to compress (for action="compress").
    #[serde(default)]
    file_path: Option<String>,
    /// Must be true to actually compress (destructive: rewrites the file and
    /// sends contents to the Anthropic API).
    #[serde(default)]
    confirmed: Option<bool>,
}

/// Locate the `caveman-compress` module directory (contains `scripts/`).
fn find_caveman_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join(".gemini/extensions/caveman/caveman-compress"));
        candidates.push(home.join(".claude/plugins/caveman/caveman-compress"));
        candidates.push(home.join(".cursor/extensions/caveman/caveman-compress"));
        candidates.push(home.join(".local/share/caveman/caveman-compress"));
    }
    candidates
        .into_iter()
        .find(|dir| dir.join("scripts/__main__.py").is_file())
}

#[async_trait]
impl Tool for CavemanTool {
    fn name(&self) -> &str {
        "caveman"
    }

    fn description(&self) -> &str {
        "Compress a natural-language markdown file into ultra-terse caveman form to cut input tokens ~75%, \
         preserving code, URLs, headings, and technical substance. Destructive: rewrites the file in place \
         and writes a <name>.original.md backup; calls the Anthropic API. Actions: status (check availability), \
         compress (requires file_path and confirmed=true)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["status", "compress"],
                    "description": "caveman operation."
                },
                "file_path": {
                    "type": "string",
                    "description": "Markdown file to compress (compress action)."
                },
                "confirmed": {
                    "type": "boolean",
                    "description": "Must be true to compress: this rewrites the file and sends its contents to the Anthropic API."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: CavemanInput = serde_json::from_value(input)?;

        let Some(caveman_dir) = find_caveman_dir() else {
            return Ok(ToolOutput::new(
                "caveman compressor is not installed. Install the caveman plugin (github.com/JuliusBrussee/caveman) so caveman-compress/scripts is available, then retry.",
            ));
        };

        match params.action.as_str() {
            "status" => Ok(ToolOutput::new(format!(
                "caveman compressor available at {}.\nUse action='compress' with file_path and confirmed=true to compress a markdown file (rewrites in place, writes <name>.original.md, calls the Anthropic API).",
                caveman_dir.display()
            ))
            .with_title("caveman status")),
            "compress" => {
                let Some(file_path) = params.file_path.as_deref() else {
                    return Ok(ToolOutput::new(
                        "caveman compress requires `file_path` (the markdown file to compress).",
                    ));
                };
                if !params.confirmed.unwrap_or(false) {
                    return Ok(ToolOutput::new(format!(
                        "caveman compress is destructive: it rewrites {file_path} in place, writes a <name>.original.md backup, and sends the file contents to the Anthropic API. Re-issue with confirmed=true to proceed."
                    )));
                }

                let abs = ctx.resolve_path(Path::new(file_path));
                if !abs.is_file() {
                    return Ok(ToolOutput::new(format!("File not found: {}", abs.display())));
                }

                let Some(python) = external_cli::find_binary("python3")
                    .or_else(|| external_cli::find_binary("python"))
                else {
                    return Ok(ToolOutput::new(
                        "python3 is required to run caveman compression but was not found on PATH.",
                    ));
                };

                let args = vec![
                    "-m".to_string(),
                    "scripts".to_string(),
                    abs.to_string_lossy().to_string(),
                ];
                let result =
                    external_cli::run(&python, &args, None, Some(caveman_dir.as_path())).await?;
                Ok(external_cli::format_cli_result("caveman", "compress", result))
            }
            other => Ok(ToolOutput::new(format!(
                "Unknown caveman action '{other}'. Valid: status, compress."
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolExecutionMode;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "t".into(),
            message_id: "t".into(),
            tool_call_id: "t".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    #[test]
    fn schema_actions() {
        let schema = CavemanTool::new().parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert!(actions.iter().any(|a| a == "compress"));
        assert!(actions.iter().any(|a| a == "status"));
    }

    #[tokio::test]
    async fn compress_requires_confirmation_or_install() {
        // Without confirmed=true, must refuse (when installed) or report missing.
        let out = CavemanTool::new()
            .execute(
                json!({"action": "compress", "file_path": "README.md"}),
                ctx(),
            )
            .await
            .unwrap();
        assert!(
            out.output.contains("confirmed=true") || out.output.contains("not installed"),
            "unexpected: {}",
            out.output
        );
    }
}
