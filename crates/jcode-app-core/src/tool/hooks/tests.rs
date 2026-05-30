use super::*;
use crate::tool::{ToolContext, ToolExecutionMode, ToolOutput};
use serde_json::json;

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

/// A native hook used in tests with configurable behavior.
struct TestHook {
    name: String,
    target: String,
    pre: fn(&Value) -> PreToolUseDecision,
    post: fn(&ToolOutput) -> PostToolUseDecision,
}

#[async_trait]
impl Hook for TestHook {
    fn name(&self) -> &str {
        &self.name
    }
    fn matches(&self, tool_name: &str) -> bool {
        self.target == "*" || self.target == tool_name
    }
    async fn pre(&self, input: PreToolUseInput<'_>) -> Result<PreToolUseDecision> {
        Ok((self.pre)(input.input))
    }
    async fn post(&self, input: PostToolUseInput<'_>) -> Result<PostToolUseDecision> {
        Ok((self.post)(input.output))
    }
}

fn cont_pre(_: &Value) -> PreToolUseDecision {
    PreToolUseDecision::Continue
}
fn cont_post(_: &ToolOutput) -> PostToolUseDecision {
    PostToolUseDecision::Continue
}

#[tokio::test]
async fn empty_registry_proceeds_unchanged() {
    let reg = HookRegistry::new();
    assert!(reg.is_empty());
    let outcome = reg.run_pre("bash", json!({"command": "ls"}), &ctx()).await;
    match outcome {
        PreOutcome::Proceed { input } => assert_eq!(input, json!({"command": "ls"})),
        PreOutcome::Deny { .. } => panic!("should not deny"),
    }
}

#[tokio::test]
async fn pre_rewrite_chains_across_hooks() {
    let mut reg = HookRegistry::new();
    reg.register_native(Arc::new(TestHook {
        name: "a".into(),
        target: "bash".into(),
        pre: |_| PreToolUseDecision::RewriteInput(json!({"command": "step1"})),
        post: cont_post,
    }));
    reg.register_native(Arc::new(TestHook {
        name: "b".into(),
        target: "bash".into(),
        // Sees the prior rewrite and appends.
        pre: |v| {
            let prev = v.get("command").and_then(|c| c.as_str()).unwrap_or("");
            PreToolUseDecision::RewriteInput(json!({"command": format!("{prev}+step2")}))
        },
        post: cont_post,
    }));
    let outcome = reg
        .run_pre("bash", json!({"command": "orig"}), &ctx())
        .await;
    match outcome {
        PreOutcome::Proceed { input } => {
            assert_eq!(input["command"], "step1+step2");
        }
        _ => panic!("expected proceed"),
    }
}

#[tokio::test]
async fn deny_short_circuits() {
    let mut reg = HookRegistry::new();
    reg.register_native(Arc::new(TestHook {
        name: "denier".into(),
        target: "*".into(),
        pre: |_| PreToolUseDecision::Deny {
            reason: "nope".into(),
        },
        post: cont_post,
    }));
    let outcome = reg.run_pre("bash", json!({}), &ctx()).await;
    matches!(outcome, PreOutcome::Deny { .. });
    if let PreOutcome::Deny { reason } = outcome {
        assert_eq!(reason, "nope");
    } else {
        panic!("expected deny");
    }
}

#[tokio::test]
async fn non_matching_hook_is_skipped() {
    let mut reg = HookRegistry::new();
    reg.register_native(Arc::new(TestHook {
        name: "only_jsbash".into(),
        target: "jsbash".into(),
        pre: |_| PreToolUseDecision::Deny {
            reason: "should not fire".into(),
        },
        post: cont_post,
    }));
    let outcome = reg.run_pre("bash", json!({"x": 1}), &ctx()).await;
    assert!(matches!(outcome, PreOutcome::Proceed { .. }));
}

#[tokio::test]
async fn post_rewrite_replaces_output() {
    let mut reg = HookRegistry::new();
    reg.register_native(Arc::new(TestHook {
        name: "compress".into(),
        target: "bash".into(),
        pre: cont_pre,
        post: |_| PostToolUseDecision::RewriteOutput(ToolOutput::new("compressed")),
    }));
    let out = reg
        .run_post(
            "bash",
            &json!({}),
            ToolOutput::new("raw huge output"),
            &ctx(),
        )
        .await;
    assert_eq!(out.output, "compressed");
}

#[tokio::test]
async fn erroring_hook_fails_open() {
    struct Boom;
    #[async_trait]
    impl Hook for Boom {
        fn name(&self) -> &str {
            "boom"
        }
        fn matches(&self, _: &str) -> bool {
            true
        }
        async fn pre(&self, _: PreToolUseInput<'_>) -> Result<PreToolUseDecision> {
            Err(anyhow::anyhow!("kaboom"))
        }
    }
    let mut reg = HookRegistry::new();
    reg.register_native(Arc::new(Boom));
    let outcome = reg.run_pre("bash", json!({"command": "ls"}), &ctx()).await;
    // Fail-open: proceed unchanged.
    assert!(matches!(outcome, PreOutcome::Proceed { .. }));
}

#[tokio::test]
async fn native_hooks_run_before_command_hooks() {
    let mut reg = HookRegistry::new();
    // Register a command hook via config first...
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("noop.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\necho '{\"decision\":\"continue\"}'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let config: HookConfig = serde_json::from_str(&format!(
        r#"{{ "PreToolUse": [ {{ "matcher": "*", "command": "{}" }} ] }}"#,
        script.display()
    ))
    .unwrap();
    reg.extend_from_config(&config);
    // ...then register a native hook; it must be inserted before the command hook.
    reg.register_native(Arc::new(TestHook {
        name: "native".into(),
        target: "*".into(),
        pre: cont_pre,
        post: cont_post,
    }));
    // First hook should be the native one.
    assert_eq!(reg.hooks[0].name(), "native");
    assert_eq!(reg.len(), 2);
}

#[cfg(unix)]
#[tokio::test]
async fn command_hook_deny_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("deny.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\necho '{\"decision\":\"deny\",\"reason\":\"policy\"}'\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut reg = HookRegistry::new();
    let config: HookConfig = serde_json::from_str(&format!(
        r#"{{ "PreToolUse": [ {{ "matcher": "bash", "command": "{}" }} ] }}"#,
        script.display()
    ))
    .unwrap();
    reg.extend_from_config(&config);

    let outcome = reg
        .run_pre("bash", json!({"command": "rm -rf /"}), &ctx())
        .await;
    match outcome {
        PreOutcome::Deny { reason } => assert_eq!(reason, "policy"),
        _ => panic!("expected deny"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn command_hook_updated_input_rewrites() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("rewrite.sh");
    // Emulate rtk-style transparent rewrite using the Claude Code `updatedInput` alias.
    std::fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\necho '{\"updatedInput\":{\"command\":\"rtk git status\"}}'\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut reg = HookRegistry::new();
    let config: HookConfig = serde_json::from_str(&format!(
        r#"{{ "PreToolUse": [ {{ "matcher": "bash", "command": "{}" }} ] }}"#,
        script.display()
    ))
    .unwrap();
    reg.extend_from_config(&config);

    let outcome = reg
        .run_pre("bash", json!({"command": "git status"}), &ctx())
        .await;
    match outcome {
        PreOutcome::Proceed { input } => assert_eq!(input["command"], "rtk git status"),
        _ => panic!("expected proceed with rewrite"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn command_hook_timeout_fails_open() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("slow.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nsleep 5\necho '{\"decision\":\"deny\"}'\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut reg = HookRegistry::new();
    let config: HookConfig = serde_json::from_str(&format!(
        r#"{{ "PreToolUse": [ {{ "matcher": "bash", "command": "{}", "timeout_ms": 200 }} ] }}"#,
        script.display()
    ))
    .unwrap();
    reg.extend_from_config(&config);

    let outcome = reg.run_pre("bash", json!({"command": "ls"}), &ctx()).await;
    // Timed out -> fail open -> proceed.
    assert!(matches!(outcome, PreOutcome::Proceed { .. }));
}

#[cfg(unix)]
#[tokio::test]
async fn command_hook_post_rewrites_output() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("filter.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\necho '{\"decision\":\"rewriteOutput\",\"output\":\"filtered\"}'\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut reg = HookRegistry::new();
    let config: HookConfig = serde_json::from_str(&format!(
        r#"{{ "PostToolUse": [ {{ "matcher": "bash", "command": "{}" }} ] }}"#,
        script.display()
    ))
    .unwrap();
    reg.extend_from_config(&config);

    let out = reg
        .run_post("bash", &json!({}), ToolOutput::new("noisy"), &ctx())
        .await;
    assert_eq!(out.output, "filtered");
}
