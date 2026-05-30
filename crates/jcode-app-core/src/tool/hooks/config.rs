//! `~/.jcode/hooks.json` configuration model and loader.

use super::command_hook::{CommandHook, HookPhase};
use super::matcher::HookMatcher;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

const DEFAULT_TIMEOUT_MS: u64 = 5000;

/// Top-level `hooks.json` document.
///
/// ```json
/// {
///   "PreToolUse":  [ { "matcher": "bash", "command": "~/.jcode/hooks/rtk-rewrite.sh", "timeout_ms": 3000 } ],
///   "PostToolUse": [ { "matcher": "bash|jsbash", "command": "/path/to/filter" } ]
/// }
/// ```
#[derive(Debug, Default, Deserialize)]
pub struct HookConfig {
    #[serde(default, rename = "PreToolUse")]
    pub pre_tool_use: Vec<HookConfigEntry>,
    #[serde(default, rename = "PostToolUse")]
    pub post_tool_use: Vec<HookConfigEntry>,
}

/// A single configured command hook.
#[derive(Debug, Clone, Deserialize)]
pub struct HookConfigEntry {
    /// Tool-name matcher (exact, alternation, or `*`). Defaults to `*`.
    #[serde(default = "default_matcher")]
    pub matcher: HookMatcher,
    /// Path to the external program (supports a leading `~`).
    pub command: String,
    /// Per-invocation timeout. Defaults to 5000ms.
    #[serde(default = "default_timeout_ms", rename = "timeout_ms")]
    pub timeout_ms: u64,
    /// Optional human-readable name for logging.
    #[serde(default)]
    pub name: Option<String>,
}

fn default_matcher() -> HookMatcher {
    HookMatcher::parse("*")
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

impl HookConfig {
    /// Load and parse a hooks.json file. Returns `Ok(None)` if it does not
    /// exist. Parse/IO errors are surfaced to the caller (logged upstream).
    pub fn load_optional(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read hooks config {}", path.display()))?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        let config: HookConfig = serde_json::from_str(&text)
            .with_context(|| format!("parse hooks config {}", path.display()))?;
        Ok(Some(config))
    }

    /// Materialize the configured command hooks (both phases).
    pub fn command_hooks(&self) -> Vec<CommandHook> {
        let mut hooks = Vec::with_capacity(self.pre_tool_use.len() + self.post_tool_use.len());
        for (idx, entry) in self.pre_tool_use.iter().enumerate() {
            hooks.push(entry.to_command_hook(HookPhase::Pre, idx));
        }
        for (idx, entry) in self.post_tool_use.iter().enumerate() {
            hooks.push(entry.to_command_hook(HookPhase::Post, idx));
        }
        hooks
    }
}

impl HookConfigEntry {
    fn to_command_hook(&self, phase: HookPhase, idx: usize) -> CommandHook {
        let label = self.name.clone().unwrap_or_else(|| {
            format!("command:{}:{}:{}", phase.as_str(), idx, self.matcher.raw())
        });
        CommandHook::new(
            label,
            self.matcher.clone(),
            expand_tilde(&self.command),
            Duration::from_millis(self.timeout_ms),
            phase,
        )
    }
}

/// Expand a leading `~` to the user's home directory.
fn expand_tilde(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    if path == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    std::path::PathBuf::from(path)
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let json = r#"{
            "PreToolUse": [
                { "matcher": "bash", "command": "/bin/echo", "timeout_ms": 1000, "name": "rewrite" }
            ],
            "PostToolUse": [
                { "matcher": "bash|jsbash", "command": "/bin/cat" }
            ]
        }"#;
        let config: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.pre_tool_use.len(), 1);
        assert_eq!(config.post_tool_use.len(), 1);
        assert_eq!(config.pre_tool_use[0].timeout_ms, 1000);
        // PostToolUse default timeout.
        assert_eq!(config.post_tool_use[0].timeout_ms, DEFAULT_TIMEOUT_MS);
        assert_eq!(config.command_hooks().len(), 2);
    }

    #[test]
    fn matcher_defaults_to_wildcard() {
        let json = r#"{ "PreToolUse": [ { "command": "/bin/echo" } ] }"#;
        let config: HookConfig = serde_json::from_str(json).unwrap();
        assert!(config.pre_tool_use[0].matcher.matches("anything"));
    }

    #[test]
    fn missing_file_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nope.json");
        assert!(HookConfig::load_optional(&path).unwrap().is_none());
    }

    #[test]
    fn empty_file_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hooks.json");
        std::fs::write(&path, "   \n").unwrap();
        assert!(HookConfig::load_optional(&path).unwrap().is_none());
    }

    #[test]
    fn invalid_json_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hooks.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(HookConfig::load_optional(&path).is_err());
    }

    #[test]
    fn tilde_expansion() {
        // SAFETY: single-threaded test mutating process env.
        unsafe {
            std::env::set_var("HOME", "/home/test");
        }
        let p = expand_tilde("~/.jcode/hooks/x.sh");
        assert_eq!(p, std::path::PathBuf::from("/home/test/.jcode/hooks/x.sh"));
        let abs = expand_tilde("/abs/path");
        assert_eq!(abs, std::path::PathBuf::from("/abs/path"));
    }
}
