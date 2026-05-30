//! Command hook: an external program invoked per matching tool call.

use super::matcher::HookMatcher;
use super::{
    CommandDecision, Hook, PostToolUseDecision, PostToolUseInput, PreToolUseDecision,
    PreToolUseInput, run_command_capture,
};
use crate::tool::ToolOutput;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    Pre,
    Post,
}

impl HookPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            HookPhase::Pre => "PreToolUse",
            HookPhase::Post => "PostToolUse",
        }
    }
}

/// A hook backed by an external program. The program receives a JSON payload on
/// stdin and returns a JSON [`CommandDecision`] on stdout. Fail-open semantics
/// are enforced by [`run_command_capture`].
pub struct CommandHook {
    name: String,
    matcher: HookMatcher,
    program: PathBuf,
    timeout: Duration,
    phase: HookPhase,
}

impl CommandHook {
    pub fn new(
        name: String,
        matcher: HookMatcher,
        program: PathBuf,
        timeout: Duration,
        phase: HookPhase,
    ) -> Self {
        Self {
            name,
            matcher,
            program,
            timeout,
            phase,
        }
    }

    async fn invoke(&self, payload: serde_json::Value) -> Option<CommandDecision> {
        let bytes = match serde_json::to_vec(&payload) {
            Ok(bytes) => bytes,
            Err(_) => return None,
        };
        let stdout = run_command_capture(&self.program, &bytes, self.timeout)
            .await
            .ok()
            .flatten()?;
        match serde_json::from_str::<CommandDecision>(&stdout) {
            Ok(decision) => Some(decision),
            Err(err) => {
                crate::logging::warn(&format!(
                    "hooks: {} returned unparseable JSON (continuing): {err}",
                    self.name
                ));
                None
            }
        }
    }
}

#[async_trait]
impl Hook for CommandHook {
    fn name(&self) -> &str {
        &self.name
    }

    fn matches(&self, tool_name: &str) -> bool {
        // A command hook only participates in its configured phase, but the
        // `matches` predicate is phase-agnostic; phase filtering happens because
        // pre()/post() are no-ops for the other phase.
        self.matcher.matches(tool_name)
    }

    async fn pre(&self, input: PreToolUseInput<'_>) -> Result<PreToolUseDecision> {
        if self.phase != HookPhase::Pre {
            return Ok(PreToolUseDecision::Continue);
        }
        let payload = json!({
            "event": "PreToolUse",
            "tool_name": input.tool_name,
            "tool_input": input.input,
            "session_id": input.ctx.session_id,
            "cwd": input.ctx.working_dir.as_ref().map(|p| p.display().to_string()),
        });
        let Some(decision) = self.invoke(payload).await else {
            return Ok(PreToolUseDecision::Continue);
        };
        // `updatedInput` (Claude Code rtk-style) is an alias for rewriteInput.
        if let Some(updated) = decision.updated_input.clone() {
            return Ok(PreToolUseDecision::RewriteInput(updated));
        }
        match decision.decision.as_deref() {
            Some("deny") | Some("block") => Ok(PreToolUseDecision::Deny {
                reason: decision
                    .reason
                    .unwrap_or_else(|| format!("blocked by hook {}", self.name)),
            }),
            Some("rewriteInput") => match decision.input {
                Some(input) => Ok(PreToolUseDecision::RewriteInput(input)),
                None => Ok(PreToolUseDecision::Continue),
            },
            _ => Ok(PreToolUseDecision::Continue),
        }
    }

    async fn post(&self, input: PostToolUseInput<'_>) -> Result<PostToolUseDecision> {
        if self.phase != HookPhase::Post {
            return Ok(PostToolUseDecision::Continue);
        }
        let payload = json!({
            "event": "PostToolUse",
            "tool_name": input.tool_name,
            "tool_input": input.input,
            "tool_output": input.output.output,
            "session_id": input.ctx.session_id,
            "cwd": input.ctx.working_dir.as_ref().map(|p| p.display().to_string()),
        });
        let Some(decision) = self.invoke(payload).await else {
            return Ok(PostToolUseDecision::Continue);
        };
        match decision.decision.as_deref() {
            Some("rewriteOutput") => match decision.output {
                Some(text) => {
                    let mut out = ToolOutput::new(text);
                    out.title = input.output.title.clone();
                    out.images = input.output.images.clone();
                    Ok(PostToolUseDecision::RewriteOutput(out))
                }
                None => Ok(PostToolUseDecision::Continue),
            },
            // Bare `output` field without explicit decision is also accepted.
            _ => match decision.output {
                Some(text) => {
                    let mut out = ToolOutput::new(text);
                    out.title = input.output.title.clone();
                    out.images = input.output.images.clone();
                    Ok(PostToolUseDecision::RewriteOutput(out))
                }
                None => Ok(PostToolUseDecision::Continue),
            },
        }
    }
}
