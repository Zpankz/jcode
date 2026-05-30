//! Native `context-mode` tool.
//!
//! `context-mode` is a session-context manager that ships an MCP server plus a
//! few maintenance subcommands. jcode already runs the MCP server natively, so
//! this tool exposes the operational CLI surface the MCP transport does not:
//! `doctor` (diagnose runtime/hooks/FTS5/version), `upgrade` (fix hooks,
//! permissions, settings), and `statusline` (status line output).

use super::external_cli;
use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

pub struct ContextModeTool;

impl ContextModeTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct ContextModeInput {
    action: String,
}

#[async_trait]
impl Tool for ContextModeTool {
    fn name(&self) -> &str {
        "context_mode"
    }

    fn description(&self) -> &str {
        "Manage context-mode session-context tooling. Actions: doctor (diagnose runtime, hooks, FTS5, version), \
         upgrade (fix hooks/permissions/settings), statusline (print the context-mode status line)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["doctor", "upgrade", "statusline"],
                    "description": "context-mode operation."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: ContextModeInput = serde_json::from_value(input)?;

        let Some(binary) = external_cli::find_binary("context-mode") else {
            return Ok(ToolOutput::new(
                "context-mode CLI is not installed. Install it with `npm install -g context-mode` (or `bun add -g context-mode`), then retry.",
            ));
        };

        let args = match params.action.as_str() {
            "doctor" => vec!["doctor".to_string()],
            "upgrade" => vec!["upgrade".to_string()],
            "statusline" => vec!["statusline".to_string()],
            other => {
                return Ok(ToolOutput::new(format!(
                    "Unknown context_mode action '{other}'. Valid: doctor, upgrade, statusline."
                )));
            }
        };

        let result = external_cli::run(&binary, &args, None, ctx.working_dir.as_deref()).await?;
        Ok(external_cli::format_cli_result(
            "context_mode",
            &params.action,
            result,
        ))
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
        let schema = ContextModeTool::new().parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert!(actions.iter().any(|a| a == "doctor"));
    }

    #[tokio::test]
    async fn unknown_action_is_graceful() {
        let out = ContextModeTool::new()
            .execute(json!({"action": "bogus"}), ctx())
            .await
            .unwrap();
        assert!(
            out.output.contains("Unknown context_mode action")
                || out.output.contains("not installed"),
            "unexpected: {}",
            out.output
        );
    }
}
