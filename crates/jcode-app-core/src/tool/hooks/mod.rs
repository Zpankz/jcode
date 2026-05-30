//! Native tool-call hook layer for Jcode.
//!
//! Mirrors Claude Code's `PreToolUse` / `PostToolUse` hooks but native to
//! Jcode's Rust runtime. Hooks intercept every tool call at the single
//! chokepoint `Registry::execute`. A hook may observe a call, deny it, rewrite
//! its input, or rewrite its output.
//!
//! Two hook sources are supported:
//! - **Native hooks**: in-process Rust implementations of [`Hook`].
//! - **Command hooks**: external programs configured in `~/.jcode/hooks.json`,
//!   invoked with a JSON payload on stdin and returning a JSON decision on
//!   stdout (the Claude Code hook contract). Command hooks are fail-open and
//!   time-boxed: any error, non-zero exit, or timeout is treated as `Continue`
//!   so a misconfigured hook can never break tool execution.
//!
//! Default installs have no `hooks.json` and no native hooks, so the layer is a
//! cheap no-op (`HookRegistry::is_empty()` short-circuits in `execute`).

use crate::tool::{ToolContext, ToolOutput};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

mod command_hook;
mod config;
mod matcher;
mod rtk_rewrite;

pub use command_hook::CommandHook;
pub use config::{HookConfig, HookConfigEntry};
pub use matcher::HookMatcher;
pub use rtk_rewrite::{RtkRewriteHook, rewrite_bash_command};

/// Input to a `PreToolUse` hook.
pub struct PreToolUseInput<'a> {
    pub tool_name: &'a str,
    pub input: &'a Value,
    pub ctx: &'a ToolContext,
}

/// Decision returned by a `PreToolUse` hook.
#[derive(Debug, Clone)]
pub enum PreToolUseDecision {
    /// Proceed unchanged.
    Continue,
    /// Replace the tool input with this value.
    RewriteInput(Value),
    /// Block the tool call; `reason` becomes the tool error.
    Deny { reason: String },
}

/// Input to a `PostToolUse` hook.
pub struct PostToolUseInput<'a> {
    pub tool_name: &'a str,
    pub input: &'a Value,
    pub output: &'a ToolOutput,
    pub ctx: &'a ToolContext,
}

/// Decision returned by a `PostToolUse` hook.
#[derive(Debug, Clone)]
pub enum PostToolUseDecision {
    /// Keep the output unchanged.
    Continue,
    /// Replace the tool output text.
    RewriteOutput(ToolOutput),
}

/// A hook intercepting tool calls. Both methods default to `Continue`.
#[async_trait]
pub trait Hook: Send + Sync {
    /// Stable identifier for logging.
    fn name(&self) -> &str;

    /// Whether this hook applies to the given (resolved) tool name.
    fn matches(&self, tool_name: &str) -> bool;

    async fn pre(&self, _input: PreToolUseInput<'_>) -> Result<PreToolUseDecision> {
        Ok(PreToolUseDecision::Continue)
    }

    async fn post(&self, _input: PostToolUseInput<'_>) -> Result<PostToolUseDecision> {
        Ok(PostToolUseDecision::Continue)
    }
}

/// Ordered collection of hooks consulted by `Registry::execute`.
///
/// Ordering: native hooks first (in registration order), then command hooks (in
/// config order). For `PreToolUse`, the first `Deny` short-circuits; otherwise
/// `RewriteInput` results chain (each hook sees the prior hook's rewrite). For
/// `PostToolUse`, `RewriteOutput` results chain similarly.
#[derive(Clone, Default)]
pub struct HookRegistry {
    hooks: Vec<Arc<dyn Hook>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a native hook (runs before command hooks).
    pub fn register_native(&mut self, hook: Arc<dyn Hook>) {
        // Native hooks must precede command hooks. Insert after the last native
        // hook. We track this by keeping natives at the front; since command
        // hooks are only ever added via `extend_from_config` after natives, a
        // simple push preserves the invariant as long as natives are registered
        // first. To be robust we insert at the partition point.
        let insert_at = self
            .hooks
            .iter()
            .position(|h| h.name().starts_with("command:"))
            .unwrap_or(self.hooks.len());
        self.hooks.insert(insert_at, hook);
    }

    /// Append command hooks loaded from a [`HookConfig`].
    pub fn extend_from_config(&mut self, config: &HookConfig) {
        for hook in config.command_hooks() {
            self.hooks.push(Arc::new(hook));
        }
    }

    /// Load `~/.jcode/hooks.json` (if present) and append its command hooks.
    /// Missing/invalid config is non-fatal (logged, ignored).
    pub fn load_global_command_hooks(&mut self) {
        match Self::global_config_path().and_then(|p| HookConfig::load_optional(&p)) {
            Ok(Some(config)) => self.extend_from_config(&config),
            Ok(None) => {}
            Err(err) => crate::logging::warn(&format!("hooks: failed to load hooks.json: {err:#}")),
        }
    }

    fn global_config_path() -> Result<PathBuf> {
        Ok(crate::storage::jcode_dir()
            .context("resolve ~/.jcode")?
            .join("hooks.json"))
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// Run all matching `PreToolUse` hooks. Returns the (possibly rewritten)
    /// input, or a `Deny` decision. Hook errors are logged and treated as
    /// `Continue` (fail-open).
    pub async fn run_pre(&self, tool_name: &str, input: Value, ctx: &ToolContext) -> PreOutcome {
        let mut current = input;
        for hook in &self.hooks {
            if !hook.matches(tool_name) {
                continue;
            }
            let decision = hook
                .pre(PreToolUseInput {
                    tool_name,
                    input: &current,
                    ctx,
                })
                .await;
            match decision {
                Ok(PreToolUseDecision::Continue) => {}
                Ok(PreToolUseDecision::RewriteInput(next)) => {
                    crate::logging::info(&format!(
                        "hooks: {} rewrote input for tool {}",
                        hook.name(),
                        tool_name
                    ));
                    current = next;
                }
                Ok(PreToolUseDecision::Deny { reason }) => {
                    crate::logging::info(&format!(
                        "hooks: {} denied tool {}: {}",
                        hook.name(),
                        tool_name,
                        reason
                    ));
                    return PreOutcome::Deny { reason };
                }
                Err(err) => {
                    crate::logging::warn(&format!(
                        "hooks: {} pre() errored for {} (continuing fail-open): {err:#}",
                        hook.name(),
                        tool_name
                    ));
                }
            }
        }
        PreOutcome::Proceed { input: current }
    }

    /// Run all matching `PostToolUse` hooks. Returns the (possibly rewritten)
    /// output. Hook errors are logged and treated as `Continue`.
    pub async fn run_post(
        &self,
        tool_name: &str,
        input: &Value,
        output: ToolOutput,
        ctx: &ToolContext,
    ) -> ToolOutput {
        let mut current = output;
        for hook in &self.hooks {
            if !hook.matches(tool_name) {
                continue;
            }
            let decision = hook
                .post(PostToolUseInput {
                    tool_name,
                    input,
                    output: &current,
                    ctx,
                })
                .await;
            match decision {
                Ok(PostToolUseDecision::Continue) => {}
                Ok(PostToolUseDecision::RewriteOutput(next)) => {
                    crate::logging::info(&format!(
                        "hooks: {} rewrote output for tool {}",
                        hook.name(),
                        tool_name
                    ));
                    current = next;
                }
                Err(err) => {
                    crate::logging::warn(&format!(
                        "hooks: {} post() errored for {} (continuing fail-open): {err:#}",
                        hook.name(),
                        tool_name
                    ));
                }
            }
        }
        current
    }
}

/// Result of running the `PreToolUse` chain.
pub enum PreOutcome {
    /// Proceed with this (possibly rewritten) input.
    Proceed { input: Value },
    /// Block the call with this reason.
    Deny { reason: String },
}

/// Spawn a command hook process, write `payload` JSON to stdin, and read its
/// stdout, enforcing a timeout. Returns stdout on success. Any failure
/// (spawn error, non-zero exit, timeout, empty output) yields `Ok(None)` so the
/// caller can fail open.
pub(crate) async fn run_command_capture(
    program: &Path,
    payload: &[u8],
    timeout: Duration,
) -> Result<Option<String>> {
    use tokio::io::AsyncWriteExt;

    let mut cmd = tokio::process::Command::new(program);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            crate::logging::warn(&format!(
                "hooks: failed to spawn command hook {}: {err}",
                program.display()
            ));
            return Ok(None);
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        // Best-effort write; ignore broken pipe.
        let _ = stdin.write_all(payload).await;
        let _ = stdin.shutdown().await;
    }

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            crate::logging::warn(&format!(
                "hooks: command hook {} failed: {err}",
                program.display()
            ));
            return Ok(None);
        }
        Err(_) => {
            crate::logging::warn(&format!(
                "hooks: command hook {} timed out after {:?}",
                program.display(),
                timeout
            ));
            return Ok(None);
        }
    };

    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        Ok(None)
    } else {
        Ok(Some(stdout))
    }
}

/// Decision payload returned by a command hook on stdout. Superset of the
/// Claude Code contract (`updatedInput` is accepted as an alias for
/// `rewriteInput`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommandDecision {
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub updated_input: Option<Value>,
    #[serde(default)]
    pub output: Option<String>,
}

#[cfg(test)]
mod tests;
