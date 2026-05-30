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

fn write_test_caveman_skill(root: &Path, name: &str) {
    let dir = root.join("skills").join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Test caveman skill\n---\n\n# {name}\n"),
    )
    .expect("write skill");
}

#[test]
fn setup_writes_style_block_manifest_and_imports_skills() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let caveman = tempfile::tempdir().expect("caveman root");
    let _home = EnvGuard::set_path("JCODE_HOME", home.path());
    write_test_caveman_skill(caveman.path(), "caveman");
    write_test_caveman_skill(caveman.path(), "caveman-compress");

    let output = setup_caveman(Some(caveman.path().to_string_lossy().as_ref()), false)
        .expect("setup succeeds");

    assert!(output.output.contains("setup complete"));
    let preferred =
        std::fs::read_to_string(home.path().join("preferred-tools.md")).expect("preferred tools");
    assert!(preferred.contains(BLOCK_START));
    assert!(preferred.contains("smart caveman"));
    assert!(preferred.contains("https://github.com/JuliusBrussee/caveman"));

    assert!(home.path().join("skills/caveman/SKILL.md").exists());
    assert!(
        home.path()
            .join("skills/caveman-compress/SKILL.md")
            .exists()
    );
    assert!(home.path().join("caveman/manifest.json").exists());
}

#[test]
fn setup_without_root_still_writes_style_block() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let _home = EnvGuard::set_path("JCODE_HOME", home.path());
    // Make sure no CAVEMAN_ROOT leaks in.
    let _clear = EnvGuard {
        key: "CAVEMAN_ROOT",
        previous: {
            let prev = std::env::var_os("CAVEMAN_ROOT");
            env::remove_var("CAVEMAN_ROOT");
            prev
        },
    };

    let output = setup_caveman(None, false).expect("setup succeeds");
    assert!(output.output.contains("none imported"));
    let preferred =
        std::fs::read_to_string(home.path().join("preferred-tools.md")).expect("preferred tools");
    assert!(preferred.contains(BLOCK_START));
}

#[tokio::test]
async fn caveman_tool_status_reports_missing_setup() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let _home = EnvGuard::set_path("JCODE_HOME", home.path());
    let tool = CavemanTool::new();

    let output = tool
        .execute(
            json!({"action": "status"}),
            make_ctx(home.path().to_path_buf()),
        )
        .await
        .expect("status succeeds");

    assert!(output.output.contains("Style block installed: no"));
    assert!(output.output.contains("Imported caveman skills: 0"));
}

#[tokio::test]
async fn caveman_import_skills_errors_without_root() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let _home = EnvGuard::set_path("JCODE_HOME", home.path());
    let _clear = EnvGuard {
        key: "CAVEMAN_ROOT",
        previous: {
            let prev = std::env::var_os("CAVEMAN_ROOT");
            env::remove_var("CAVEMAN_ROOT");
            prev
        },
    };
    let tool = CavemanTool::new();
    let result = tool
        .execute(
            json!({"action": "import_skills"}),
            make_ctx(home.path().to_path_buf()),
        )
        .await;
    assert!(result.is_err());
}
