//! Shared helpers for native Jcode integrations of upstream agent tools
//! (context-mode, rtk, caveman, ...).
//!
//! These tools all integrate the same way Claude Code integrates external
//! plugins: by writing routing guidance into `~/.jcode/preferred-tools.md`
//! (which `prompt.rs` injects into the system prompt), optionally registering
//! MCP servers in `~/.jcode/mcp.json`, and importing upstream skill directories
//! into `~/.jcode/skills` (which the skill registry discovers).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Resolve `~/.jcode` (honoring `JCODE_HOME` in tests).
pub(crate) fn jcode_dir() -> Result<PathBuf> {
    crate::storage::jcode_dir().context("resolve ~/.jcode directory")
}

/// Path to the global preferred-tools routing file consumed by the prompt.
pub(crate) fn preferred_tools_path() -> Result<PathBuf> {
    Ok(jcode_dir()?.join("preferred-tools.md"))
}

/// Insert or replace a uniquely-marked block inside an existing document.
///
/// If both markers are present the region between them (inclusive) is replaced.
/// Otherwise the block is appended. Surrounding content is preserved with a
/// single blank-line separator.
pub(crate) fn upsert_marked_block(
    existing: &str,
    block: &str,
    block_start: &str,
    block_end: &str,
) -> String {
    if let (Some(start), Some(end)) = (existing.find(block_start), existing.find(block_end)) {
        let end = end + block_end.len();
        let mut out = String::with_capacity(existing.len() - (end - start) + block.len() + 2);
        out.push_str(existing[..start].trim_end());
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(block);
        let rest = existing[end..].trim_start();
        if !rest.is_empty() {
            out.push_str("\n\n");
            out.push_str(rest);
        }
        return out;
    }

    if existing.trim().is_empty() {
        block.to_string()
    } else {
        format!("{}\n\n{}", existing.trim_end(), block)
    }
}

/// Ensure a routing block is present in `preferred-tools.md`.
///
/// Returns `Ok(true)` when the file was written, `Ok(false)` when the block was
/// already present and `overwrite` was not requested.
pub(crate) fn ensure_preferred_tools_block(
    block: &str,
    block_start: &str,
    block_end: &str,
    overwrite: bool,
) -> Result<bool> {
    let path = preferred_tools_path()?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.contains(block_start) && !overwrite {
        return Ok(false);
    }
    let content = upsert_marked_block(&existing, block, block_start, block_end);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content)?;
    Ok(true)
}

/// Whether the preferred-tools file currently contains the given start marker.
pub(crate) fn preferred_tools_block_present(block_start: &str) -> bool {
    preferred_tools_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.contains(block_start))
        .unwrap_or(false)
}

/// Recursively copy a directory tree. When `overwrite` is false and `dst`
/// already exists this is a no-op.
pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path, overwrite: bool) -> Result<()> {
    if dst.exists() {
        if overwrite {
            std::fs::remove_dir_all(dst)?;
        } else {
            return Ok(());
        }
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path, overwrite)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Import every `SKILL.md`-bearing subdirectory of `<root>/skills` into
/// `~/.jcode/skills`. Returns the sorted list of imported skill names.
pub(crate) fn import_skills_from_root(root: &Path, overwrite: bool) -> Result<Vec<String>> {
    let skills_src = root.join("skills");
    let skills_dst = jcode_dir()?.join("skills");
    std::fs::create_dir_all(&skills_dst)?;

    let mut imported = Vec::new();
    for entry in std::fs::read_dir(&skills_src)
        .with_context(|| format!("read skills directory at {}", skills_src.display()))?
    {
        let entry = entry?;
        let src = entry.path();
        if !src.is_dir() || !src.join("SKILL.md").exists() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let dst = skills_dst.join(&name);
        if dst.exists() && !overwrite {
            continue;
        }
        copy_dir_recursive(&src, &dst, overwrite)?;
        imported.push(name);
    }
    imported.sort();
    Ok(imported)
}

/// Locate an upstream tool checkout/package root that contains a `skills/`
/// directory, searching explicit path, an env var, and `node_modules`.
pub(crate) fn find_root_with_skills(
    explicit: Option<&str>,
    env_var: &str,
    node_modules_name: &str,
) -> Option<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = explicit {
        roots.push(PathBuf::from(root));
    }
    if let Ok(root) = std::env::var(env_var)
        && !root.trim().is_empty()
    {
        roots.push(PathBuf::from(root));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join("node_modules").join(node_modules_name));
    }
    roots.into_iter().find(|root| root.join("skills").is_dir())
}

/// Count imported skills under `~/.jcode/skills` whose directory name matches
/// any of the supplied prefixes/exact names.
pub(crate) fn count_skills_matching(predicate: impl Fn(&str) -> bool) -> usize {
    let skill_dir = match jcode_dir() {
        Ok(dir) => dir.join("skills"),
        Err(_) => return 0,
    };
    std::fs::read_dir(&skill_dir)
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().join("SKILL.md").exists())
                .filter(|e| predicate(&e.file_name().to_string_lossy()))
                .count()
        })
        .unwrap_or(0)
}

pub(crate) fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests;
