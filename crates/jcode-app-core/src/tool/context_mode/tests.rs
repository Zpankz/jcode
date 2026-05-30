use super::*;
use crate::env;
use crate::tool::ToolExecutionMode;

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        env::set_var(key, value.as_os_str());
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            env::set_var(self.key, previous);
        } else {
            env::remove_var(self.key);
        }
    }
}

fn make_ctx(working_dir: std::path::PathBuf) -> ToolContext {
    ToolContext {
        session_id: "test-session".to_string(),
        message_id: "test-message".to_string(),
        tool_call_id: "test-call".to_string(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn write_test_context_mode_skill(root: &Path, name: &str) {
    let dir = root.join("skills").join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Test context mode skill\n---\n\n# {name}\n"),
    )
    .expect("write skill");
}

#[test]
fn marked_block_upsert_replaces_existing_context_mode_block() {
    let first = format!("before\n\n{BLOCK_START}\nold\n{BLOCK_END}\n\nafter");
    let second = format!("{BLOCK_START}\nnew\n{BLOCK_END}");

    let out = upsert_marked_block(&first, &second);

    assert!(out.contains("before"));
    assert!(out.contains("new"));
    assert!(out.contains("after"));
    assert!(!out.contains("old"));
}

#[test]
fn setup_writes_mcp_preferred_tools_manifest_and_imports_skills() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let context_mode = tempfile::tempdir().expect("context-mode root");
    let _home = EnvGuard::set_path("JCODE_HOME", home.path());
    write_test_context_mode_skill(context_mode.path(), "context-mode");
    write_test_context_mode_skill(context_mode.path(), "ctx-stats");

    let output = setup_context_mode(Some(context_mode.path().to_string_lossy().as_ref()), false)
        .expect("setup succeeds");

    assert!(output.output.contains("setup complete"));
    let mcp = McpConfig::load_from_file(&home.path().join("mcp.json")).expect("mcp config");
    let server = mcp.servers.get(SERVER_NAME).expect("context-mode server");
    assert_eq!(server.command, "npx");
    assert_eq!(server.args, vec!["-y", "context-mode"]);

    let preferred =
        std::fs::read_to_string(home.path().join("preferred-tools.md")).expect("preferred tools");
    assert!(preferred.contains(BLOCK_START));
    assert!(preferred.contains("ctx_execute"));
    assert!(preferred.contains("https://github.com/mksglu/context-mode"));

    assert!(home.path().join("skills/context-mode/SKILL.md").exists());
    assert!(home.path().join("skills/ctx-stats/SKILL.md").exists());
    assert!(home.path().join("context-mode/manifest.json").exists());
}

#[tokio::test]
async fn context_mode_tool_status_reports_missing_setup() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let _home = EnvGuard::set_path("JCODE_HOME", home.path());
    let tool = ContextModeTool::new();

    let output = tool
        .execute(
            json!({"action": "status"}),
            make_ctx(home.path().to_path_buf()),
        )
        .await
        .expect("status succeeds");

    assert!(
        output
            .output
            .contains("MCP server `context-mode` configured: no")
    );
    assert!(
        output
            .output
            .contains("Preferred-tool routing block installed: no")
    );
}
