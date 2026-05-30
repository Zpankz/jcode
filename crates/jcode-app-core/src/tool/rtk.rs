//! Native `rtk` tool.
//!
//! `rtk` is a token-optimizing CLI proxy: it runs a real command (ls, git,
//! grep, cargo, pytest, ...) and filters/summarizes the output before it
//! reaches the agent's context, typically cutting tokens by 90%+.
//!
//! Rather than enumerate rtk's ~60 subcommands, this tool is a faithful
//! passthrough: the agent supplies `args` (the rtk subcommand and its
//! arguments) and we forward them verbatim. A small set of convenience flags
//! (`ultra_compact`) map to rtk's global options. This keeps full
//! functionality while staying a single, stable tool surface.

use super::external_cli;
use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

pub struct RtkTool;

impl RtkTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct RtkInput {
    /// rtk subcommand and arguments, e.g. ["ls", "-la"], ["git", "status"],
    /// ["grep", "TODO", "src"], or ["gain", "--quota"].
    #[serde(default)]
    args: Vec<String>,
    /// Convenience: a whole command line as a single string (split on
    /// whitespace). Use `args` for anything with quoting/spaces in arguments.
    #[serde(default)]
    command: Option<String>,
    /// Enable rtk ultra-compact mode (ASCII icons, inline format).
    #[serde(default)]
    ultra_compact: Option<bool>,
}

#[async_trait]
impl Tool for RtkTool {
    fn name(&self) -> &str {
        "rtk"
    }

    fn description(&self) -> &str {
        "Token-optimizing CLI proxy. Run a real command through rtk to get filtered/summarized output \
         that costs far fewer tokens. Pass the rtk subcommand and args via `args`, e.g. \
         args=[\"ls\",\"-la\"], [\"git\",\"status\"], [\"grep\",\"TODO\",\"src\"], [\"tree\",\".\"], \
         [\"cargo\",\"build\"], [\"test\"], or [\"gain\",\"--quota\"] for savings stats. \
         Prefer rtk over raw bash for noisy commands (ls/tree/git/grep/build/test/logs)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "rtk subcommand and arguments, forwarded verbatim. e.g. [\"git\",\"status\"]."
                },
                "command": {
                    "type": "string",
                    "description": "Whole command line as one string (whitespace-split). Use `args` if arguments contain spaces/quotes."
                },
                "ultra_compact": {
                    "type": "boolean",
                    "description": "Enable rtk ultra-compact output mode."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: RtkInput = serde_json::from_value(input)?;

        let Some(binary) = external_cli::find_binary("rtk") else {
            return Ok(ToolOutput::new(
                "rtk CLI is not installed. Install it with `cargo install rtk` (or see github.com/rtk-ai/rtk), then retry.",
            ));
        };

        let mut sub_args: Vec<String> = if !params.args.is_empty() {
            params.args.clone()
        } else if let Some(cmd) = &params.command {
            cmd.split_whitespace().map(|s| s.to_string()).collect()
        } else {
            Vec::new()
        };

        if sub_args.is_empty() {
            return Ok(ToolOutput::new(
                "rtk requires `args` (the rtk subcommand and arguments), e.g. args=[\"git\",\"status\"].",
            ));
        }

        let mut args: Vec<String> = Vec::new();
        if params.ultra_compact.unwrap_or(false) {
            args.push("--ultra-compact".to_string());
        }
        let label = sub_args.first().cloned().unwrap_or_default();
        args.append(&mut sub_args);

        let result = external_cli::run(&binary, &args, None, ctx.working_dir.as_deref()).await?;
        Ok(external_cli::format_cli_result("rtk", &label, result))
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
    fn schema_has_args_array() {
        let schema = RtkTool::new().parameters_schema();
        assert_eq!(schema["properties"]["args"]["type"], "array");
    }

    #[tokio::test]
    async fn empty_args_is_graceful() {
        let out = RtkTool::new().execute(json!({}), ctx()).await.unwrap();
        assert!(
            out.output.contains("requires `args`") || out.output.contains("not installed"),
            "unexpected: {}",
            out.output
        );
    }
}
