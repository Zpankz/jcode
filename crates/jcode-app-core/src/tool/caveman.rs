//! Native Jcode integration for the upstream `caveman` output-compression tool.
//!
//! caveman (https://github.com/JuliusBrussee/caveman) makes the agent's own
//! *output* terse ("smart caveman" prose) to cut ~65-75% of output tokens while
//! preserving full technical accuracy. Upstream it ships as a Claude Code
//! plugin/skill plus a system-prompt rule that activates the style.
//!
//! Jcode's equivalent native integration is a routing/style block in
//! `~/.jcode/preferred-tools.md` (injected into the system prompt by
//! `prompt.rs`) plus importing the caveman SKILL.md directories into
//! `~/.jcode/skills` so the `/caveman`, `/caveman-compress`, etc. skills are
//! discoverable.

use super::integration_support as support;
use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

const BLOCK_START: &str = "<!-- JCODE_CAVEMAN_START -->";
const BLOCK_END: &str = "<!-- JCODE_CAVEMAN_END -->";
const ENV_VAR: &str = "CAVEMAN_ROOT";
const NODE_MODULES_NAME: &str = "caveman";

pub struct CavemanTool;

impl CavemanTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Deserialize)]
struct CavemanInput {
    #[serde(default = "default_action")]
    action: String,
    #[serde(default)]
    caveman_root: Option<String>,
    #[serde(default)]
    overwrite: Option<bool>,
}

fn default_action() -> String {
    "status".to_string()
}

#[derive(Debug, Serialize)]
struct CavemanManifest {
    migrated_at: String,
    preferred_tools: bool,
    imported_skills: Vec<String>,
    source_root: Option<String>,
}

#[async_trait]
impl Tool for CavemanTool {
    fn name(&self) -> &str {
        "caveman"
    }

    fn description(&self) -> &str {
        "Set up or inspect native Jcode integration for the caveman output-compression style: a system-prompt style block that makes responses terse (~65-75% fewer output tokens) and imports of the upstream caveman skills."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["status", "setup", "import_skills"],
                    "description": "Action. setup writes the caveman style block into ~/.jcode/preferred-tools.md and imports skills if a root is found. import_skills copies caveman SKILL.md directories into ~/.jcode/skills."
                },
                "caveman_root": {
                    "type": "string",
                    "description": "Optional path to a caveman checkout or installed package root. Used for importing skills."
                },
                "overwrite": {
                    "type": "boolean",
                    "description": "Replace an existing Jcode-managed caveman style block or existing imported skill directories. Defaults to false."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let params: CavemanInput = serde_json::from_value(input)?;
        let overwrite = params.overwrite.unwrap_or(false);
        match params.action.as_str() {
            "status" => status_output().map(|out| out.with_title("caveman status")),
            "setup" => setup_caveman(params.caveman_root.as_deref(), overwrite)
                .map(|out| out.with_title("caveman setup")),
            "import_skills" => import_skills_action(params.caveman_root.as_deref(), overwrite)
                .map(|out| out.with_title("caveman skill import")),
            other => Ok(ToolOutput::new(format!(
                "Unknown action: {other}. Use status, setup, or import_skills."
            ))),
        }
    }
}

fn manifest_path() -> Result<PathBuf> {
    Ok(support::jcode_dir()?.join("caveman").join("manifest.json"))
}

fn caveman_preferred_tools_block() -> String {
    format!(
        r#"{BLOCK_START}
# caveman Output Compression Style for Jcode

Jcode is integrated with the upstream `caveman` output-compression style. When caveman mode is active, respond in terse "smart caveman" prose to cut ~65-75% of output tokens while keeping full technical accuracy.

Activation (adapted from https://github.com/JuliusBrussee/caveman):

- Activate when the user says "caveman mode", "talk like caveman", "use caveman", "less tokens", "be brief", or invokes `/caveman`. Also activate when token efficiency is explicitly requested.
- Stay active every response once on. Deactivate only on "stop caveman" or "normal mode".
- Levels: `lite`, `full` (default), `ultra`. Switch with `/caveman lite|full|ultra`.

Style rules:

- Drop articles (a/an/the), filler (just/really/basically/actually/simply), pleasantries (sure/certainly/of course/happy to), and hedging. Fragments are fine.
- Prefer short synonyms (big not extensive, fix not "implement a solution for"). Keep technical terms exact. Leave code blocks and quoted errors unchanged.
- Pattern: `[thing] [action] [reason]. [next step].`
- Not: "Sure! I'd be happy to help you with that. The issue is likely caused by..."
- Yes: "Bug in auth middleware. Token expiry check use `<` not `<=`. Fix:"

Caveman style affects prose only. Do not compress code, commands, file paths, URLs, or exact error text. Markdown structure and correctness always win over brevity.
{BLOCK_END}"#
    )
}

fn write_manifest(manifest: &CavemanManifest) -> Result<()> {
    let path = manifest_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(manifest)?)?;
    Ok(())
}

fn find_caveman_root(explicit: Option<&str>) -> Option<PathBuf> {
    support::find_root_with_skills(explicit, ENV_VAR, NODE_MODULES_NAME)
}

fn setup_caveman(caveman_root: Option<&str>, overwrite: bool) -> Result<ToolOutput> {
    let preferred_changed = support::ensure_preferred_tools_block(
        &caveman_preferred_tools_block(),
        BLOCK_START,
        BLOCK_END,
        overwrite,
    )?;
    let root = find_caveman_root(caveman_root);
    let imported_skills = if let Some(root) = root.as_ref() {
        support::import_skills_from_root(root, overwrite)?
    } else {
        Vec::new()
    };

    let manifest = CavemanManifest {
        migrated_at: chrono::Utc::now().to_rfc3339(),
        preferred_tools: true,
        imported_skills: imported_skills.clone(),
        source_root: root.as_ref().map(|p| p.display().to_string()),
    };
    write_manifest(&manifest)?;

    let mut lines = vec![
        "caveman native Jcode setup complete.".to_string(),
        format!(
            "- Style block in {}: {}",
            support::preferred_tools_path()?.display(),
            if preferred_changed {
                "written"
            } else {
                "already present"
            }
        ),
    ];
    if imported_skills.is_empty() {
        lines.push(
            "- Skills: none imported. Pass caveman_root (or set CAVEMAN_ROOT) to import the upstream skills directory."
                .to_string(),
        );
    } else {
        lines.push(format!(
            "- Skills imported into ~/.jcode/skills: {}",
            imported_skills.join(", ")
        ));
    }
    Ok(ToolOutput::new(lines.join("\n")))
}

fn import_skills_action(caveman_root: Option<&str>, overwrite: bool) -> Result<ToolOutput> {
    let root = find_caveman_root(caveman_root).ok_or_else(|| {
        anyhow::anyhow!(
            "Could not locate a caveman root with a skills/ directory. Pass caveman_root or set CAVEMAN_ROOT."
        )
    })?;
    let imported = support::import_skills_from_root(&root, overwrite)?;
    Ok(ToolOutput::new(if imported.is_empty() {
        format!(
            "No skills imported from {}. They may already exist; pass overwrite=true to replace them.",
            root.display()
        )
    } else {
        format!(
            "Imported caveman skills from {}: {}",
            root.display(),
            imported.join(", ")
        )
    }))
}

fn status_output() -> Result<ToolOutput> {
    let preferred_present = support::preferred_tools_block_present(BLOCK_START);
    let manifest = manifest_path()?;
    let manifest_present = manifest.exists();
    let skill_count = support::count_skills_matching(|name| {
        name == "caveman" || name.starts_with("caveman-") || name == "cavecrew"
    });
    let skill_dir = support::jcode_dir()?.join("skills");

    Ok(ToolOutput::new(format!(
        "caveman native Jcode status:\n- Style block installed: {} ({})\n- Imported caveman skills: {} ({})\n- Migration manifest present: {} ({})",
        support::yes_no(preferred_present),
        support::preferred_tools_path()?.display(),
        skill_count,
        skill_dir.display(),
        support::yes_no(manifest_present),
        manifest.display(),
    )))
}

#[cfg(test)]
mod tests;
