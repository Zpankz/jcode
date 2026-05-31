use super::*;
use crate::env;
use crate::tool::ToolExecutionMode;
use std::path::Path;

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

    fn set_value(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        env::set_var(key, std::ffi::OsStr::new(value));
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

#[test]
fn setup_writes_routing_block_and_manifest() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let _home = EnvGuard::set_path("JCODE_HOME", home.path());
    // Ensure rtk is not "found" by pointing PATH at an empty dir.
    let empty = tempfile::tempdir().expect("empty path dir");
    let _path = EnvGuard::set_value("PATH", empty.path().to_string_lossy().as_ref());

    let output = setup_rtk(false).expect("setup succeeds");
    assert!(output.output.contains("setup complete"));
    assert!(output.output.contains("rtk binary NOT found"));

    let preferred =
        std::fs::read_to_string(home.path().join("preferred-tools.md")).expect("preferred tools");
    assert!(preferred.contains(BLOCK_START));
    assert!(preferred.contains("rtk git status"));
    assert!(preferred.contains("https://github.com/rtk-ai/rtk"));

    assert!(home.path().join("rtk/manifest.json").exists());
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join("rtk/manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["preferred_tools"], serde_json::json!(true));
    assert_eq!(manifest["rtk_detected"], serde_json::json!(false));
}

#[test]
fn detect_rtk_finds_binary_on_path() {
    // Serialize with other tests that mutate the global PATH env.
    let _lock = crate::storage::lock_test_env();
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let exe = if cfg!(windows) { "rtk.exe" } else { "rtk" };
    let exe_path = bin_dir.path().join(exe);
    // Create a fake rtk that prints a version.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&exe_path, "#!/bin/sh\necho 'rtk 9.9.9'\n").unwrap();
        let mut perms = std::fs::metadata(&exe_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&exe_path, perms).unwrap();
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&exe_path, "echo rtk 9.9.9").unwrap();
    }

    let _path = EnvGuard::set_value("PATH", bin_dir.path().to_string_lossy().as_ref());
    let detected = detect_rtk();
    assert!(detected.is_some(), "rtk should be detected on PATH");
    #[cfg(unix)]
    {
        let (path, version) = detected.unwrap();
        assert_eq!(path, exe_path);
        assert_eq!(version.as_deref(), Some("rtk 9.9.9"));
    }
}

#[tokio::test]
async fn rtk_tool_status_reports_missing_setup() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let _home = EnvGuard::set_path("JCODE_HOME", home.path());
    let empty = tempfile::tempdir().expect("empty path dir");
    let _path = EnvGuard::set_value("PATH", empty.path().to_string_lossy().as_ref());
    let tool = RtkTool::new();

    let output = tool
        .execute(
            json!({"action": "status"}),
            make_ctx(home.path().to_path_buf()),
        )
        .await
        .expect("status succeeds");

    assert!(output.output.contains("Routing block installed: no"));
    assert!(output.output.contains("rtk binary on PATH: no"));
}

#[tokio::test]
async fn rtk_tool_check_reports_absence() {
    let _lock = crate::storage::lock_test_env();
    let empty = tempfile::tempdir().expect("empty path dir");
    let _path = EnvGuard::set_value("PATH", empty.path().to_string_lossy().as_ref());
    let tool = RtkTool::new();
    let output = tool
        .execute(
            json!({"action": "check"}),
            make_ctx(empty.path().to_path_buf()),
        )
        .await
        .expect("check succeeds");
    assert!(output.output.contains("rtk not found on PATH"));
}
