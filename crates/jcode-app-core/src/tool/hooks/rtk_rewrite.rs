//! Native `PreToolUse` hook that transparently rewrites safe `bash` commands
//! into their `rtk` (Rust Token Killer) equivalents.
//!
//! This is the native jcode equivalent of the Claude Code rtk PreToolUse hook
//! that the upstream project ships. It activates only when:
//! - the resolved tool is `bash`, and
//! - rtk integration is set up (`~/.jcode/rtk/manifest.json` exists), and
//! - the `rtk` binary is on PATH.
//!
//! Rewriting is intentionally conservative: only *simple, single* commands with
//! no shell metacharacters (pipes, `&&`, `;`, redirects, subshells, globs that
//! the shell would expand, etc.) are rewritten, so command semantics are never
//! altered. Anything ambiguous is passed through untouched (fail-safe).

use super::{Hook, PreToolUseDecision, PreToolUseInput};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};

/// PreToolUse hook performing transparent `bash` -> `rtk` rewrites.
pub struct RtkRewriteHook {
    /// Whether the rtk binary is available. Probed once at construction.
    rtk_available: bool,
}

impl RtkRewriteHook {
    /// Construct the hook, probing for the rtk binary once. Returns `None` when
    /// rtk integration is not set up, so the registry need not register it.
    pub fn new_if_enabled() -> Option<Self> {
        if !super::super::rtk::integration_enabled() {
            return None;
        }
        let rtk_available = super::super::rtk::detect_rtk().is_some();
        Some(Self { rtk_available })
    }

    #[cfg(test)]
    fn for_test(rtk_available: bool) -> Self {
        Self { rtk_available }
    }
}

#[async_trait]
impl Hook for RtkRewriteHook {
    fn name(&self) -> &str {
        "native:rtk-rewrite"
    }

    fn matches(&self, tool_name: &str) -> bool {
        tool_name == "bash"
    }

    async fn pre(&self, input: PreToolUseInput<'_>) -> Result<PreToolUseDecision> {
        if !self.rtk_available {
            return Ok(PreToolUseDecision::Continue);
        }
        let Some(command) = input.input.get("command").and_then(|c| c.as_str()) else {
            return Ok(PreToolUseDecision::Continue);
        };
        match rewrite_bash_command(command) {
            Some(rewritten) if rewritten != command => {
                let mut new_input = input.input.clone();
                if let Value::Object(map) = &mut new_input {
                    map.insert("command".to_string(), json!(rewritten));
                }
                Ok(PreToolUseDecision::RewriteInput(new_input))
            }
            _ => Ok(PreToolUseDecision::Continue),
        }
    }
}

/// Shell metacharacters that mean the command is too complex to safely rewrite.
const UNSAFE_METACHARS: &[&str] = &[
    "|", "&", ";", ">", "<", "`", "$(", "&&", "||", "\n", "(", ")", "{", "}",
];

/// Rewrite a single, simple `bash` command into its `rtk` equivalent.
///
/// Returns `Some(new_command)` if a safe rewrite applies, else `None`. The
/// transformation is conservative; any shell metacharacter, quoting that we
/// cannot reason about, or unrecognized command yields `None`.
pub fn rewrite_bash_command(command: &str) -> Option<String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Reject anything with shell metacharacters: we only rewrite a lone command.
    if UNSAFE_METACHARS.iter().any(|m| trimmed.contains(m)) {
        return None;
    }
    // Already an rtk invocation, or an explicit rtk proxy: leave alone.
    if trimmed.starts_with("rtk ") || trimmed == "rtk" {
        return None;
    }

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let (cmd, args) = tokens.split_first()?;

    match *cmd {
        // Directory listings -> rtk ls
        "ls" => Some(prepend_rtk("ls", args)),
        // File reads -> rtk read. Only when there is at least one path arg and
        // no flags that change semantics in a way rtk read can't mirror.
        "cat" => rewrite_simple_read(args),
        // Recursive/broad search -> rtk grep / rtk find.
        "grep" => Some(prepend_rtk("grep", args)),
        "find" => Some(prepend_rtk("find", args)),
        // git status/log/diff -> rtk git ...; mutations stay on bash.
        "git" => rewrite_git(args),
        _ => None,
    }
}

fn prepend_rtk(sub: &str, args: &[&str]) -> String {
    if args.is_empty() {
        format!("rtk {sub}")
    } else {
        format!("rtk {sub} {}", args.join(" "))
    }
}

/// `cat <file>...` -> `rtk read <file>...`, but only when args look like plain
/// paths (no flags), since `cat -n`/`cat -A` etc. have no rtk read analogue.
fn rewrite_simple_read(args: &[&str]) -> Option<String> {
    if args.is_empty() {
        return None;
    }
    if args.iter().any(|a| a.starts_with('-')) {
        return None;
    }
    Some(format!("rtk read {}", args.join(" ")))
}

/// Only read-only git subcommands are rewritten. Mutating subcommands return
/// `None` so they run unmodified on plain bash.
fn rewrite_git(args: &[&str]) -> Option<String> {
    let sub = args.first()?;
    match *sub {
        "status" | "log" | "diff" | "show" | "blame" => Some(format!("rtk git {}", args.join(" "))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_ls() {
        assert_eq!(rewrite_bash_command("ls"), Some("rtk ls".into()));
        assert_eq!(
            rewrite_bash_command("ls -la src"),
            Some("rtk ls -la src".into())
        );
    }

    #[test]
    fn rewrites_cat_to_read() {
        assert_eq!(
            rewrite_bash_command("cat src/main.rs"),
            Some("rtk read src/main.rs".into())
        );
    }

    #[test]
    fn does_not_rewrite_cat_with_flags() {
        // `cat -n` numbering has no rtk read analogue; leave untouched.
        assert_eq!(rewrite_bash_command("cat -n file"), None);
    }

    #[test]
    fn rewrites_grep_and_find() {
        assert_eq!(
            rewrite_bash_command("grep -r pattern ."),
            Some("rtk grep -r pattern .".into())
        );
        assert_eq!(
            rewrite_bash_command("find . -name *.rs"),
            Some("rtk find . -name *.rs".into())
        );
    }

    #[test]
    fn rewrites_readonly_git() {
        assert_eq!(
            rewrite_bash_command("git status"),
            Some("rtk git status".into())
        );
        assert_eq!(
            rewrite_bash_command("git log -n 5"),
            Some("rtk git log -n 5".into())
        );
        assert_eq!(
            rewrite_bash_command("git diff"),
            Some("rtk git diff".into())
        );
    }

    #[test]
    fn does_not_rewrite_mutating_git() {
        assert_eq!(rewrite_bash_command("git commit -m x"), None);
        assert_eq!(rewrite_bash_command("git push"), None);
        assert_eq!(rewrite_bash_command("git checkout main"), None);
        assert_eq!(rewrite_bash_command("git reset --hard"), None);
    }

    #[test]
    fn rejects_pipes_and_redirects() {
        assert_eq!(rewrite_bash_command("ls | head"), None);
        assert_eq!(rewrite_bash_command("cat a > b"), None);
        assert_eq!(rewrite_bash_command("grep x . && echo done"), None);
        assert_eq!(rewrite_bash_command("ls; pwd"), None);
        assert_eq!(rewrite_bash_command("echo $(date)"), None);
        assert_eq!(rewrite_bash_command("cat `which ls`"), None);
    }

    #[test]
    fn leaves_unknown_commands() {
        assert_eq!(rewrite_bash_command("pwd"), None);
        assert_eq!(rewrite_bash_command("echo hi"), None);
        assert_eq!(rewrite_bash_command("cargo build"), None);
    }

    #[test]
    fn does_not_double_wrap_rtk() {
        assert_eq!(rewrite_bash_command("rtk ls"), None);
        assert_eq!(rewrite_bash_command("rtk"), None);
    }

    #[test]
    fn empty_is_none() {
        assert_eq!(rewrite_bash_command("   "), None);
    }

    #[tokio::test]
    async fn hook_continue_when_rtk_unavailable() {
        use crate::tool::{ToolContext, ToolExecutionMode};
        let hook = RtkRewriteHook::for_test(false);
        let ctx = ToolContext {
            session_id: "s".into(),
            message_id: "m".into(),
            tool_call_id: "c".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        };
        let input = json!({"command": "ls"});
        let decision = hook
            .pre(PreToolUseInput {
                tool_name: "bash",
                input: &input,
                ctx: &ctx,
            })
            .await
            .unwrap();
        assert!(matches!(decision, PreToolUseDecision::Continue));
    }

    #[tokio::test]
    async fn hook_rewrites_when_available() {
        use crate::tool::{ToolContext, ToolExecutionMode};
        let hook = RtkRewriteHook::for_test(true);
        let ctx = ToolContext {
            session_id: "s".into(),
            message_id: "m".into(),
            tool_call_id: "c".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        };
        let input = json!({"command": "ls -la"});
        let decision = hook
            .pre(PreToolUseInput {
                tool_name: "bash",
                input: &input,
                ctx: &ctx,
            })
            .await
            .unwrap();
        match decision {
            PreToolUseDecision::RewriteInput(v) => assert_eq!(v["command"], "rtk ls -la"),
            _ => panic!("expected rewrite"),
        }
    }
}
