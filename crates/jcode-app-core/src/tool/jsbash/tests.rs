use super::*;
use crate::tool::ToolExecutionMode;

fn ctx_with(session: &str, wd: Option<&str>) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "m".into(),
        tool_call_id: "c".into(),
        working_dir: wd.map(PathBuf::from),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

#[test]
fn embedded_assets_are_present() {
    assert!(SERVER_MJS.contains("just-bash"));
    assert!(SERVER_MJS.contains("readline"));
    assert!(PACKAGE_JSON.contains("just-bash"));
}

#[test]
fn sidecar_key_distinguishes_session_and_swarm() {
    let tool = JsBashTool::new();
    let session_ctx = ctx_with("sess-1", None);
    assert_eq!(tool.sidecar_key(&session_ctx, None), "session:sess-1");
    assert_eq!(tool.sidecar_key(&session_ctx, Some("alpha")), "swarm:alpha");
    // Swarm key ignores the session, so all members share it.
    let other = ctx_with("sess-2", None);
    assert_eq!(
        tool.sidecar_key(&other, Some("alpha")),
        tool.sidecar_key(&session_ctx, Some("alpha"))
    );
}

#[test]
fn swarm_sandbox_dir_sanitizes_key() {
    let dir = swarm_sandbox_dir("../etc/passwd").unwrap();
    let name = dir.file_name().unwrap().to_string_lossy();
    assert_eq!(name, "swarm-___etc_passwd");
    assert!(!name.contains('/'));
}

#[test]
fn format_exec_combines_streams() {
    let r = sidecar::ExecResult {
        stdout: "out".into(),
        stderr: "err".into(),
        exit_code: 2,
    };
    let out = format_exec(&r);
    assert!(out.output.contains("out"));
    assert!(out.output.contains("[stderr]"));
    assert!(out.output.contains("err"));
    assert!(out.output.contains("[exit 2]"));
    assert_eq!(out.metadata.unwrap()["exitCode"], 2);
}

#[test]
fn format_exec_empty_is_no_output() {
    let r = sidecar::ExecResult {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
    };
    let out = format_exec(&r);
    assert_eq!(out.output, "(no output)");
}

#[tokio::test]
async fn exec_without_setup_reports_install_hint() {
    // With no sidecar installed under a throwaway JCODE_HOME, exec must fail
    // with a clear, non-panicking setup hint.
    let _guard = crate::storage::lock_test_env();
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded under the test env lock.
    unsafe {
        std::env::set_var("JCODE_HOME", tmp.path());
    }
    let tool = JsBashTool::new();
    let res = tool
        .execute(
            json!({"action": "exec", "script": "echo hi"}),
            ctx_with("s", None),
        )
        .await;
    unsafe {
        std::env::remove_var("JCODE_HOME");
    }
    assert!(res.is_err());
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("setup") || msg.contains("not installed"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn status_action_never_fails() {
    let _guard = crate::storage::lock_test_env();
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("JCODE_HOME", tmp.path());
    }
    let tool = JsBashTool::new();
    let out = tool
        .execute(json!({"action": "status"}), ctx_with("s", None))
        .await;
    unsafe {
        std::env::remove_var("JCODE_HOME");
    }
    let out = out.unwrap();
    assert!(out.output.contains("jsbash native Jcode status"));
    assert!(out.output.contains("server.mjs present: no"));
}

#[tokio::test]
async fn setup_materializes_server_mjs() {
    let _guard = crate::storage::lock_test_env();
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("JCODE_HOME", tmp.path());
    }
    let tool = JsBashTool::new();
    let out = tool
        .execute(json!({"action": "setup"}), ctx_with("s", None))
        .await
        .unwrap();
    let server = tmp.path().join("jsbash").join("server.mjs");
    let present = server.exists();
    unsafe {
        std::env::remove_var("JCODE_HOME");
    }
    assert!(present, "server.mjs should be written by setup");
    assert!(out.output.contains("Sidecar dir"));
}

/// Full roundtrip against a real Node sidecar. Ignored by default because it
/// spawns Node and installs just-bash. Run with:
///   cargo test -p jcode-app-core jsbash::tests::live_ -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn live_exec_and_fs_roundtrip() {
    let _guard = crate::storage::lock_test_env();
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("JCODE_HOME", tmp.path());
    }
    let tool = JsBashTool::new();

    // 1. setup installs the sidecar + just-bash.
    let setup = tool
        .execute(json!({"action": "setup"}), ctx_with("live", None))
        .await
        .unwrap();
    assert!(
        setup.output.contains("just-bash"),
        "setup: {}",
        setup.output
    );

    // 2. exec a pipeline.
    let exec = tool
        .execute(
            json!({"action": "exec", "script": "echo hello | tr a-z A-Z"}),
            ctx_with("live", None),
        )
        .await
        .unwrap();
    assert!(exec.output.contains("HELLO"), "exec: {}", exec.output);

    // 3. exec JS via QuickJS.
    let js = tool
        .execute(
            json!({"action": "exec", "script": "js-exec -c \"console.log(2+2)\""}),
            ctx_with("live", None),
        )
        .await
        .unwrap();
    assert!(js.output.contains('4'), "js: {}", js.output);

    // 4. write then read in the virtual fs (persists across requests in-session).
    tool.execute(
        json!({"action": "write_file", "path": "/tmp/note.txt", "content": "persisted"}),
        ctx_with("live", None),
    )
    .await
    .unwrap();
    let read = tool
        .execute(
            json!({"action": "read_file", "path": "/tmp/note.txt"}),
            ctx_with("live", None),
        )
        .await
        .unwrap();
    assert!(read.output.contains("persisted"), "read: {}", read.output);

    unsafe {
        std::env::remove_var("JCODE_HOME");
    }
}
