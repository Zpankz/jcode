use super::integration_support as support;
use super::{Tool, ToolContext, ToolOutput};
use crate::mcp::{McpConfig, McpServerConfig};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const SERVER_NAME: &str = "context-mode";
const BLOCK_START: &str = "<!-- JCODE_CONTEXT_MODE_START -->";
const BLOCK_END: &str = "<!-- JCODE_CONTEXT_MODE_END -->";

pub struct ContextModeTool;

impl ContextModeTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Deserialize)]
struct ContextModeInput {
    #[serde(default = "default_action")]
    action: String,
    #[serde(default)]
    context_mode_root: Option<String>,
    #[serde(default)]
    overwrite: Option<bool>,
}

fn default_action() -> String {
    "status".to_string()
}

#[derive(Debug, Serialize)]
struct ContextModeManifest {
    migrated_at: String,
    mcp_server: bool,
    preferred_tools: bool,
    imported_skills: Vec<String>,
    source_root: Option<String>,
}

#[async_trait]
impl Tool for ContextModeTool {
    fn name(&self) -> &str {
        "context_mode"
    }

    fn description(&self) -> &str {
        "Set up or inspect native Jcode integration for the context-mode MCP plugin: MCP tools, preferred-tool routing rules, and skill imports."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["status", "setup", "import_skills"],
                    "description": "Action. setup writes ~/.jcode/mcp.json and ~/.jcode/preferred-tools.md; import_skills copies context-mode SKILL.md directories into ~/.jcode/skills."
                },
                "context_mode_root": {
                    "type": "string",
                    "description": "Optional path to a context-mode checkout or installed package root. Used for importing skills."
                },
                "overwrite": {
                    "type": "boolean",
                    "description": "Replace an existing Jcode-managed context-mode routing block or existing imported skill directories. Defaults to false."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let params: ContextModeInput = serde_json::from_value(input)?;
        let overwrite = params.overwrite.unwrap_or(false);
        match params.action.as_str() {
            "status" => status_output().map(|out| out.with_title("context-mode status")),
            "setup" => setup_context_mode(params.context_mode_root.as_deref(), overwrite)
                .map(|out| out.with_title("context-mode setup")),
            "import_skills" => import_skills_action(params.context_mode_root.as_deref(), overwrite)
                .map(|out| out.with_title("context-mode skill import")),
            other => Ok(ToolOutput::new(format!(
                "Unknown action: {other}. Use status, setup, or import_skills."
            ))),
        }
    }
}

fn mcp_config_path() -> Result<PathBuf> {
    Ok(support::jcode_dir()?.join("mcp.json"))
}

fn preferred_tools_path() -> Result<PathBuf> {
    support::preferred_tools_path()
}

fn manifest_path() -> Result<PathBuf> {
    Ok(support::jcode_dir()?
        .join("context-mode")
        .join("manifest.json"))
}

fn context_mode_server_config() -> McpServerConfig {
    McpServerConfig {
        command: "npx".to_string(),
        args: vec!["-y".to_string(), "context-mode".to_string()],
        env: Default::default(),
        shared: true,
    }
}

fn load_existing_mcp(path: &Path) -> Result<McpConfig> {
    if path.exists() {
        McpConfig::load_from_file(path)
            .with_context(|| format!("load existing MCP config from {}", path.display()))
    } else {
        Ok(McpConfig::default())
    }
}

fn mcp_config_contains_context_mode(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    Ok(load_existing_mcp(path)?.servers.contains_key(SERVER_NAME))
}

fn ensure_context_mode_mcp(overwrite: bool) -> Result<bool> {
    let path = mcp_config_path()?;
    let mut config = load_existing_mcp(&path)?;
    let exists = config.servers.contains_key(SERVER_NAME);
    if exists && !overwrite {
        return Ok(false);
    }
    config
        .servers
        .insert(SERVER_NAME.to_string(), context_mode_server_config());
    config.save_to_file(&path)?;
    Ok(!exists || overwrite)
}

fn context_mode_preferred_tools_block() -> String {
    format!(
        r#"{BLOCK_START}
# Context Mode Native Routing for Jcode

Jcode is integrated with the upstream `context-mode` MCP plugin. Prefer the `mcp__context-mode__ctx_*` tools for data-heavy work so raw bytes stay in context-mode storage or a sandbox instead of entering the model transcript.

Rules adapted from https://github.com/mksglu/context-mode:

- Use `mcp__context-mode__ctx_execute` for commands that derive answers from potentially large output: tests, builds, logs, `gh`, `kubectl`, `docker`, cloud CLIs, repo-wide scans, JSON parsing, and API calls.
- Use `mcp__context-mode__ctx_execute_file` when the input is already a local file and you need counts, filters, parsing, extraction, or summaries. Print only the derived answer.
- Use `mcp__context-mode__ctx_fetch_and_index` for web pages or documentation, then `mcp__context-mode__ctx_search` for specific sections. Avoid raw `webfetch` for large pages.
- Use `mcp__context-mode__ctx_index` for large docs that will need repeated lookup, then query with `ctx_search` instead of rereading the whole source.
- Direct `bash`, `read`, `grep`, and `webfetch` remain fine for guaranteed-small observations, mutations, and narrow line ranges.
- If output size is uncertain, route through context-mode first. Treat the LLM as a code generator for analysis, not as the data processor.

Jcode native mapping:
- `bash` with curl/wget/inline HTTP/build tools -> `ctx_execute` or `ctx_fetch_and_index`.
- large `read`/PDF/log inspection -> `ctx_execute_file` or `ctx_index` + `ctx_search`.
- broad `grep`/recursive scans -> `ctx_execute` with filtering and capped printed results.
- external MCP tools with bulky payloads -> save/index/process via context-mode, then return only targeted results.
{BLOCK_END}"#
    )
}

fn ensure_preferred_tools_block(overwrite: bool) -> Result<bool> {
    support::ensure_preferred_tools_block(
        &context_mode_preferred_tools_block(),
        BLOCK_START,
        BLOCK_END,
        overwrite,
    )
}

fn find_context_mode_root(explicit: Option<&str>) -> Option<PathBuf> {
    support::find_root_with_skills(explicit, "CONTEXT_MODE_ROOT", "context-mode")
}

fn import_context_mode_skills(root: &Path, overwrite: bool) -> Result<Vec<String>> {
    support::import_skills_from_root(root, overwrite)
}

fn write_manifest(manifest: &ContextModeManifest) -> Result<()> {
    let path = manifest_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(manifest)?)?;
    Ok(())
}

fn setup_context_mode(context_mode_root: Option<&str>, overwrite: bool) -> Result<ToolOutput> {
    let mcp_changed = ensure_context_mode_mcp(overwrite)?;
    let preferred_changed = ensure_preferred_tools_block(overwrite)?;
    let root = find_context_mode_root(context_mode_root);
    let imported_skills = if let Some(root) = root.as_ref() {
        import_context_mode_skills(root, overwrite)?
    } else {
        Vec::new()
    };

    let manifest = ContextModeManifest {
        migrated_at: chrono::Utc::now().to_rfc3339(),
        mcp_server: true,
        preferred_tools: true,
        imported_skills: imported_skills.clone(),
        source_root: root.as_ref().map(|p| p.display().to_string()),
    };
    write_manifest(&manifest)?;

    let mut lines = vec![
        "Context Mode native Jcode setup complete.".to_string(),
        format!(
            "- MCP server `{SERVER_NAME}` in {}: {}",
            mcp_config_path()?.display(),
            if mcp_changed {
                "written"
            } else {
                "already present"
            }
        ),
        format!(
            "- Routing rules in {}: {}",
            preferred_tools_path()?.display(),
            if preferred_changed {
                "written"
            } else {
                "already present"
            }
        ),
    ];
    if imported_skills.is_empty() {
        lines.push("- Skills: none imported. Pass context_mode_root to import the upstream skills directory.".to_string());
    } else {
        lines.push(format!(
            "- Skills imported into ~/.jcode/skills: {}",
            imported_skills.join(", ")
        ));
    }
    lines.push(
        "Run `mcp` with action=`reload`, or restart Jcode, to connect the new MCP tools."
            .to_string(),
    );

    Ok(ToolOutput::new(lines.join("\n")))
}

fn import_skills_action(context_mode_root: Option<&str>, overwrite: bool) -> Result<ToolOutput> {
    let root = find_context_mode_root(context_mode_root).ok_or_else(|| {
        anyhow::anyhow!(
            "Could not locate a context-mode root with a skills/ directory. Pass context_mode_root or set CONTEXT_MODE_ROOT."
        )
    })?;
    let imported = import_context_mode_skills(&root, overwrite)?;
    Ok(ToolOutput::new(if imported.is_empty() {
        format!(
            "No skills imported from {}. They may already exist; pass overwrite=true to replace them.",
            root.display()
        )
    } else {
        format!(
            "Imported context-mode skills from {}: {}",
            root.display(),
            imported.join(", ")
        )
    }))
}

fn status_output() -> Result<ToolOutput> {
    let mcp_path = mcp_config_path()?;
    let preferred_path = preferred_tools_path()?;
    let manifest = manifest_path()?;
    let mcp_present = mcp_config_contains_context_mode(&mcp_path)?;
    let preferred_present = std::fs::read_to_string(&preferred_path)
        .map(|s| s.contains(BLOCK_START))
        .unwrap_or(false);
    let manifest_present = manifest.exists();
    let skill_dir = support::jcode_dir()?.join("skills");
    let skill_count =
        support::count_skills_matching(|name| name.starts_with("ctx-") || name == "context-mode");

    Ok(ToolOutput::new(format!(
        "Context Mode native Jcode status:\n- MCP server `{SERVER_NAME}` configured: {} ({})\n- Preferred-tool routing block installed: {} ({})\n- Imported context-mode skills: {} ({})\n- Migration manifest present: {} ({})",
        support::yes_no(mcp_present),
        mcp_path.display(),
        support::yes_no(preferred_present),
        preferred_path.display(),
        skill_count,
        skill_dir.display(),
        support::yes_no(manifest_present),
        manifest.display(),
    )))
}

#[cfg(test)]
mod tests;
