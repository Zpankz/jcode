use super::*;
use std::path::Path;

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        crate::env::set_var(key, value.as_os_str());
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            crate::env::set_var(self.key, previous);
        } else {
            crate::env::remove_var(self.key);
        }
    }
}

const START: &str = "<!-- TEST_START -->";
const END: &str = "<!-- TEST_END -->";

#[test]
fn upsert_appends_when_no_marker() {
    let out = upsert_marked_block("hello", "BLOCK", START, END);
    assert_eq!(out, "hello\n\nBLOCK");
}

#[test]
fn upsert_into_empty_returns_block_only() {
    let out = upsert_marked_block("   \n  ", "BLOCK", START, END);
    assert_eq!(out, "BLOCK");
}

#[test]
fn upsert_replaces_existing_block_and_preserves_surroundings() {
    let existing = format!("before\n\n{START}\nold\n{END}\n\nafter");
    let block = format!("{START}\nnew\n{END}");
    let out = upsert_marked_block(&existing, &block, START, END);
    assert!(out.contains("before"));
    assert!(out.contains("after"));
    assert!(out.contains("new"));
    assert!(!out.contains("old"));
}

#[test]
fn ensure_preferred_tools_block_is_idempotent() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let _home = EnvGuard::set_path("JCODE_HOME", home.path());
    let block = format!("{START}\nrouting\n{END}");

    assert!(ensure_preferred_tools_block(&block, START, END, false).expect("first write"));
    // Second call without overwrite must be a no-op.
    assert!(!ensure_preferred_tools_block(&block, START, END, false).expect("idempotent"));
    assert!(preferred_tools_block_present(START));

    // Overwrite should rewrite even if present.
    assert!(ensure_preferred_tools_block(&block, START, END, true).expect("overwrite"));
    let content = std::fs::read_to_string(home.path().join("preferred-tools.md")).unwrap();
    // Exactly one block, not duplicated.
    assert_eq!(content.matches(START).count(), 1);
}

#[test]
fn import_skills_copies_skill_dirs_and_is_non_destructive() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let root = tempfile::tempdir().expect("upstream root");
    let _home = EnvGuard::set_path("JCODE_HOME", home.path());

    let skill_a = root.path().join("skills").join("alpha");
    std::fs::create_dir_all(&skill_a).unwrap();
    std::fs::write(skill_a.join("SKILL.md"), "---\nname: alpha\n---\n").unwrap();
    // A non-skill directory must be ignored.
    std::fs::create_dir_all(root.path().join("skills").join("not_a_skill")).unwrap();

    let imported = import_skills_from_root(root.path(), false).expect("import");
    assert_eq!(imported, vec!["alpha".to_string()]);
    assert!(home.path().join("skills/alpha/SKILL.md").exists());

    // Re-import without overwrite must not re-copy (returns empty).
    let again = import_skills_from_root(root.path(), false).expect("reimport");
    assert!(again.is_empty());
}

#[test]
fn find_root_prefers_explicit_path() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir_all(root.path().join("skills")).unwrap();
    let found = find_root_with_skills(
        Some(root.path().to_string_lossy().as_ref()),
        "NONEXISTENT_ENV_VAR_XYZ",
        "nonexistent-package",
    );
    assert_eq!(found.as_deref(), Some(root.path()));
}

#[test]
fn count_skills_matching_filters_by_predicate() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let _home = EnvGuard::set_path("JCODE_HOME", home.path());
    for name in ["caveman", "caveman-compress", "other"] {
        let dir = home.path().join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "x").unwrap();
    }
    let count = count_skills_matching(|n| n.starts_with("caveman"));
    assert_eq!(count, 2);
}
