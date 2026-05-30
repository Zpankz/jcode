//! Native `memex` tool.
//!
//! Wraps the `memex` Zettelkasten agent-memory CLI in-process so the agent can
//! recall and persist durable knowledge without an MCP round-trip. Exposes the
//! full command surface (search, read, write, links, backlinks, archive,
//! organize, doctor, sync) through a single `action` enum.
//!
//! Learned best-practice defaults are baked in so the agent does not flood its
//! context: `search` defaults to compact one-line output, and `links` defaults
//! to summary stats rather than the full per-card link table. The agent can
//! opt into verbose output explicitly when it needs the detail.

use super::external_cli;
use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

pub struct MemexTool;

impl MemexTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct MemexInput {
    action: String,
    /// Search query or, for read/backlinks/archive, the card slug.
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    slug: Option<String>,
    /// Card content for the `write` action (passed via stdin to memex).
    #[serde(default)]
    content: Option<String>,
    /// Max search results.
    #[serde(default)]
    limit: Option<u32>,
    /// Use embedding-based semantic search for `search`.
    #[serde(default)]
    semantic: Option<bool>,
    /// Return verbose (non-compact) output for `search`/`links`.
    #[serde(default)]
    verbose: Option<bool>,
    /// Filter by frontmatter category/tag/author for `search`.
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    /// Only cards modified since this date (YYYY-MM-DD) for search/organize.
    #[serde(default)]
    since: Option<String>,
    /// For `links`: filter by "orphan" or "hub".
    #[serde(default)]
    filter: Option<String>,
    /// Request JSON output for organize/links (machine-readable).
    #[serde(default)]
    json: Option<bool>,
}

#[async_trait]
impl Tool for MemexTool {
    fn name(&self) -> &str {
        "memex"
    }

    fn description(&self) -> &str {
        "Zettelkasten agent memory. Recall durable knowledge before a task and persist insights after. \
         Actions: search (ranked keyword/semantic, compact by default), read, write (content via `content`), \
         links (graph stats by default), backlinks, archive, organize (network analysis), doctor, sync. \
         Prefer compact search + targeted read to keep context lean."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["search", "read", "write", "links", "backlinks", "archive", "organize", "doctor", "sync"],
                    "description": "Memex operation."
                },
                "query": { "type": "string", "description": "Search query (search) or card slug (read/backlinks/archive)." },
                "slug": { "type": "string", "description": "Card slug; alternative to `query` for read/write/backlinks/archive." },
                "content": { "type": "string", "description": "Card markdown body for the `write` action." },
                "limit": { "type": "integer", "description": "Max search results (default 10)." },
                "semantic": { "type": "boolean", "description": "Use embedding-based semantic search." },
                "verbose": { "type": "boolean", "description": "Return full output instead of the lean default (search/links)." },
                "category": { "type": "string", "description": "Filter search by frontmatter category." },
                "tag": { "type": "string", "description": "Filter search by frontmatter tag." },
                "since": { "type": "string", "description": "Only cards modified since YYYY-MM-DD (search/organize)." },
                "filter": { "type": "string", "enum": ["orphan", "hub"], "description": "Filter cards for `links`." },
                "json": { "type": "boolean", "description": "Request machine-readable JSON (organize/links)." }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: MemexInput = serde_json::from_value(input)?;

        let Some(binary) = external_cli::find_binary("memex") else {
            return Ok(ToolOutput::new(
                "memex CLI is not installed. Install it with `npm install -g @touchskyer/memex` (or `bun add -g @touchskyer/memex`), then retry.",
            ));
        };

        let slug = params.slug.clone().or_else(|| params.query.clone());
        let verbose = params.verbose.unwrap_or(false);

        let (args, stdin): (Vec<String>, Option<String>) = match params.action.as_str() {
            "search" => {
                let mut a = vec!["search".to_string()];
                if let Some(q) = &params.query {
                    a.push(q.clone());
                }
                // Lean by default: one line per result unless verbose requested.
                if !verbose {
                    a.push("--compact".to_string());
                }
                if params.semantic.unwrap_or(false) {
                    a.push("--semantic".to_string());
                }
                if let Some(l) = params.limit {
                    a.push("--limit".to_string());
                    a.push(l.to_string());
                }
                if let Some(c) = &params.category {
                    a.push("--category".to_string());
                    a.push(c.clone());
                }
                if let Some(t) = &params.tag {
                    a.push("--tag".to_string());
                    a.push(t.clone());
                }
                if let Some(s) = &params.since {
                    a.push("--since".to_string());
                    a.push(s.clone());
                }
                (a, None)
            }
            "read" => {
                let Some(s) = slug else {
                    return Ok(ToolOutput::new("memex read requires `slug` (or `query`)."));
                };
                (vec!["read".to_string(), s], None)
            }
            "write" => {
                let Some(s) = slug else {
                    return Ok(ToolOutput::new("memex write requires `slug`."));
                };
                let Some(body) = params.content.clone() else {
                    return Ok(ToolOutput::new(
                        "memex write requires `content` (the card markdown body).",
                    ));
                };
                (vec!["write".to_string(), s], Some(body))
            }
            "links" => {
                let mut a = vec!["links".to_string()];
                // Lean by default: summary stats, not the full per-card table.
                if let Some(f) = &params.filter {
                    a.push("--filter".to_string());
                    a.push(f.clone());
                } else if !verbose {
                    a.push("--stats".to_string());
                }
                if params.json.unwrap_or(false) {
                    a.push("--json".to_string());
                }
                (a, None)
            }
            "backlinks" => {
                let Some(s) = slug else {
                    return Ok(ToolOutput::new(
                        "memex backlinks requires `slug` (or `query`).",
                    ));
                };
                (vec!["backlinks".to_string(), s], None)
            }
            "archive" => {
                let Some(s) = slug else {
                    return Ok(ToolOutput::new(
                        "memex archive requires `slug` (or `query`).",
                    ));
                };
                (vec!["archive".to_string(), s], None)
            }
            "organize" => {
                let mut a = vec!["organize".to_string()];
                if let Some(s) = &params.since {
                    a.push("--since".to_string());
                    a.push(s.clone());
                }
                if params.json.unwrap_or(false) {
                    a.push("--json".to_string());
                }
                (a, None)
            }
            "doctor" => (vec!["doctor".to_string()], None),
            "sync" => (vec!["sync".to_string()], None),
            other => {
                return Ok(ToolOutput::new(format!(
                    "Unknown memex action '{other}'. Valid: search, read, write, links, backlinks, archive, organize, doctor, sync."
                )));
            }
        };

        let result =
            external_cli::run(&binary, &args, stdin.as_deref(), ctx.working_dir.as_deref()).await?;
        Ok(external_cli::format_cli_result(
            "memex",
            &params.action,
            result,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolExecutionMode;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "t".into(),
            message_id: "t".into(),
            tool_call_id: "t".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    #[test]
    fn schema_is_object_with_action() {
        let schema = MemexTool::new().parameters_schema();
        assert_eq!(schema["type"], "object");
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert!(actions.iter().any(|a| a == "search"));
        assert!(actions.iter().any(|a| a == "write"));
    }

    #[tokio::test]
    async fn write_without_content_is_graceful() {
        let out = MemexTool::new()
            .execute(json!({"action": "write", "slug": "x"}), ctx())
            .await
            .unwrap();
        // Either the binary is missing (install hint) or content is required.
        assert!(
            out.output.contains("requires `content`") || out.output.contains("not installed"),
            "unexpected: {}",
            out.output
        );
    }

    #[tokio::test]
    async fn unknown_action_is_graceful() {
        let out = MemexTool::new()
            .execute(json!({"action": "bogus"}), ctx())
            .await
            .unwrap();
        assert!(
            out.output.contains("Unknown memex action") || out.output.contains("not installed"),
            "unexpected: {}",
            out.output
        );
    }

    /// End-to-end against the real `memex` CLI when installed. `doctor` is
    /// read-only and safe. Skips silently when memex is not on the machine so
    /// CI without the binary still passes.
    #[tokio::test]
    async fn doctor_runs_against_real_binary_when_present() {
        if external_cli::find_binary("memex").is_none() {
            return;
        }
        let out = MemexTool::new()
            .execute(json!({"action": "doctor"}), ctx())
            .await
            .unwrap();
        assert!(!out.output.is_empty());
        assert!(
            out.title.as_deref() == Some("memex doctor")
                || out.title.as_deref() == Some("memex doctor failed"),
            "title was {:?}",
            out.title
        );
    }
}
