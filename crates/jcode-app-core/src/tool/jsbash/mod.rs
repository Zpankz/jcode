//! `jsbash` tool: a deterministic, sandboxed virtual-bash surface backed by the
//! `just-bash` (Vercel Labs) Node package, bridged through a long-lived sidecar.
//!
//! This is the native jcode equivalent of giving the agent a programmable
//! "think in code" environment: a virtual bash with an in-memory/overlay
//! filesystem and builtins (`jq`, `sqlite3`, `awk`, `sed`, JS/TS via QuickJS,
//! optional Python) that never mutates the host, keeping raw bytes out of
//! context. Real `bash` stays for host-mutating builds/tests/git; `jsbash` is
//! the safe, scriptable companion and the substrate for swarm orchestration.
//!
//! ## Filesystem model (set by the sidecar)
//! - Default (a session working dir is known): OverlayFs over that dir -- reads
//!   hit disk, writes stay in memory. Host is never mutated.
//! - Swarm shared sandbox: ReadWriteFs onto `~/.jcode/jsbash/swarm-<id>/` so
//!   members of the same swarm see each other's files.
//! - Otherwise: pure in-memory.
//!
//! ## Lifecycle
//! One sidecar per session (and per swarm sandbox), lazily spawned and cached.
//! If Node or the installed sidecar is missing, the tool stays inert and reports
//! a setup hint (non-fatal).

mod sidecar;

use super::integration_support as support;
use super::{Tool, ToolContext, ToolOutput};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use sidecar::SidecarClient;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Bundled sidecar sources, embedded so `setup` can materialize them offline.
const SERVER_MJS: &str = include_str!("../../../../../assets/jsbash/server.mjs");
const PACKAGE_JSON: &str = include_str!("../../../../../assets/jsbash/package.json");

pub struct JsBashTool {
    /// session_id (or swarm key) -> live sidecar.
    sidecars: Arc<Mutex<HashMap<String, Arc<SidecarClient>>>>,
}

impl JsBashTool {
    pub fn new() -> Self {
        Self {
            sidecars: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for JsBashTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct JsBashInput {
    #[serde(default = "default_action")]
    action: String,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    stdin: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    /// Optional swarm sandbox key; when set, the sidecar uses a shared
    /// ReadWriteFs under `~/.jcode/jsbash/swarm-<key>/` so swarm members share
    /// a filesystem.
    #[serde(default)]
    swarm: Option<String>,
    #[serde(default)]
    overwrite: Option<bool>,
}

fn default_action() -> String {
    "status".to_string()
}

#[async_trait]
impl Tool for JsBashTool {
    fn name(&self) -> &str {
        "jsbash"
    }

    fn description(&self) -> &str {
        "Deterministic sandboxed virtual bash (just-bash): run shell scripts, JS/TS (QuickJS) and data tools (jq/sqlite3/awk/sed) against an in-memory/overlay filesystem that never mutates the host. Use for data shaping, multi-file synthesis, and swarm orchestration; keep real `bash` for host-mutating builds/tests/git. Actions: exec, write_file, read_file, ls, reset, setup, status."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["exec", "write_file", "read_file", "ls", "reset", "setup", "status"],
                    "description": "exec runs a script; write_file/read_file/ls operate on the virtual fs; reset clears the virtual fs; setup installs the Node sidecar into ~/.jcode/jsbash; status reports availability."
                },
                "script": {"type": "string", "description": "Shell script for exec. Supports pipes, redirects, &&/||/;, functions, loops, and builtins. Use `js-exec -c \"<code>\"` for JS/TS."},
                "stdin": {"type": "string", "description": "Optional stdin passed to the exec script."},
                "path": {"type": "string", "description": "Virtual filesystem path for write_file/read_file/ls."},
                "content": {"type": "string", "description": "Content for write_file."},
                "cwd": {"type": "string", "description": "Working directory override for exec."},
                "swarm": {"type": "string", "description": "Swarm sandbox key. When set, uses a shared ReadWriteFs under ~/.jcode/jsbash/swarm-<key>/ so swarm members share files."},
                "overwrite": {"type": "boolean", "description": "For setup: reinstall even if already present."}
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: JsBashInput = serde_json::from_value(input)?;
        match params.action.as_str() {
            "status" => status_output().map(|o| o.with_title("jsbash status")),
            "setup" => {
                setup(params.overwrite.unwrap_or(false)).map(|o| o.with_title("jsbash setup"))
            }
            "exec" => {
                let script = params
                    .script
                    .ok_or_else(|| anyhow!("exec requires `script`"))?;
                let client = self.client_for(&ctx, params.swarm.as_deref()).await?;
                let result = client
                    .exec(
                        &script,
                        params.stdin.as_deref(),
                        params.cwd.as_deref(),
                        None,
                    )
                    .await?;
                Ok(format_exec(&result).with_title("jsbash exec"))
            }
            "write_file" => {
                let path = params
                    .path
                    .ok_or_else(|| anyhow!("write_file requires `path`"))?;
                let content = params.content.unwrap_or_default();
                let client = self.client_for(&ctx, params.swarm.as_deref()).await?;
                client.write_file(&path, &content).await?;
                Ok(
                    ToolOutput::new(format!("wrote {} ({} bytes)", path, content.len()))
                        .with_title("jsbash write_file"),
                )
            }
            "read_file" => {
                let path = params
                    .path
                    .ok_or_else(|| anyhow!("read_file requires `path`"))?;
                let client = self.client_for(&ctx, params.swarm.as_deref()).await?;
                let content = client.read_file(&path).await?;
                Ok(ToolOutput::new(content).with_title("jsbash read_file"))
            }
            "ls" => {
                let client = self.client_for(&ctx, params.swarm.as_deref()).await?;
                let listing = client.ls(params.path.as_deref()).await?;
                Ok(ToolOutput::new(listing).with_title("jsbash ls"))
            }
            "reset" => {
                let key = self.sidecar_key(&ctx, params.swarm.as_deref());
                let mut map = self.sidecars.lock().await;
                if let Some(client) = map.get(&key) {
                    client.reset().await?;
                }
                map.remove(&key);
                Ok(ToolOutput::new("jsbash sandbox reset").with_title("jsbash reset"))
            }
            other => Ok(ToolOutput::new(format!(
                "Unknown action: {other}. Use exec, write_file, read_file, ls, reset, setup, or status."
            ))),
        }
    }
}

impl JsBashTool {
    fn sidecar_key(&self, ctx: &ToolContext, swarm: Option<&str>) -> String {
        match swarm {
            Some(s) => format!("swarm:{s}"),
            None => format!("session:{}", ctx.session_id),
        }
    }

    /// Get (or lazily spawn) the sidecar for this session/swarm.
    async fn client_for(
        &self,
        ctx: &ToolContext,
        swarm: Option<&str>,
    ) -> Result<Arc<SidecarClient>> {
        let key = self.sidecar_key(ctx, swarm);
        // Fast path: reuse a running sidecar.
        let existing = {
            let map = self.sidecars.lock().await;
            map.get(&key).cloned()
        };
        if let Some(client) = existing
            && client.is_running().await
        {
            return Ok(client);
        }
        // Spawn a fresh one.
        let client = Arc::new(self.spawn_sidecar(ctx, swarm).await?);
        let mut map = self.sidecars.lock().await;
        map.insert(key, client.clone());
        Ok(client)
    }

    async fn spawn_sidecar(&self, ctx: &ToolContext, swarm: Option<&str>) -> Result<SidecarClient> {
        let server = server_path()?;
        if !server.exists() {
            return Err(anyhow!(
                "jsbash sidecar not installed. Run the `jsbash` tool with action=setup first (installs into {}).",
                jsbash_dir()?.display()
            ));
        }
        let node = resolve_node()
            .ok_or_else(|| anyhow!("`node` not found on PATH. Install Node.js to use jsbash."))?;

        let mut env: Vec<(String, String)> = Vec::new();
        if let Some(swarm_key) = swarm {
            let dir = swarm_sandbox_dir(swarm_key)?;
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("create swarm sandbox {}", dir.display()))?;
            env.push(("JSBASH_SANDBOX".into(), dir.display().to_string()));
        } else if let Some(wd) = ctx.working_dir.as_ref() {
            env.push(("JSBASH_WORKSPACE".into(), wd.display().to_string()));
        }

        let client = SidecarClient::spawn(&node, &server, &env).await?;
        // Validate just-bash actually loaded; surface a clear error if not.
        client
            .ready()
            .await
            .context("jsbash sidecar failed readiness check (is just-bash installed? run setup)")?;
        Ok(client)
    }
}

fn format_exec(result: &sidecar::ExecResult) -> ToolOutput {
    let mut body = String::new();
    if !result.stdout.is_empty() {
        body.push_str(&result.stdout);
    }
    if !result.stderr.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[stderr]\n");
        body.push_str(&result.stderr);
    }
    if result.exit_code != 0 {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&format!("[exit {}]", result.exit_code));
    }
    if body.is_empty() {
        body.push_str("(no output)");
    }
    ToolOutput::new(body).with_metadata(json!({"exitCode": result.exit_code}))
}

// --- paths & detection -----------------------------------------------------

fn jsbash_dir() -> Result<PathBuf> {
    Ok(support::jcode_dir()?.join("jsbash"))
}

fn server_path() -> Result<PathBuf> {
    Ok(jsbash_dir()?.join("server.mjs"))
}

fn swarm_sandbox_dir(key: &str) -> Result<PathBuf> {
    // Sanitize the key to a safe directory component.
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(jsbash_dir()?.join(format!("swarm-{safe}")))
}

/// Resolve the `node` binary on PATH.
fn resolve_node() -> Option<String> {
    let exe = if cfg!(windows) { "node.exe" } else { "node" };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(exe))
            .find(|c| c.is_file())
            .map(|p| p.display().to_string())
    })
}

// --- setup / status ---------------------------------------------------------

fn setup(overwrite: bool) -> Result<ToolOutput> {
    let dir = jsbash_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

    let server = dir.join("server.mjs");
    let pkg = dir.join("package.json");
    let server_existed = server.exists();
    if !server_existed || overwrite {
        std::fs::write(&server, SERVER_MJS).context("write server.mjs")?;
    }
    if !pkg.exists() || overwrite {
        std::fs::write(&pkg, PACKAGE_JSON).context("write package.json")?;
    }

    let node = resolve_node();
    let mut lines = vec![
        "jsbash native Jcode setup:".to_string(),
        format!("- Sidecar dir: {}", dir.display()),
        format!(
            "- server.mjs: {}",
            if server_existed && !overwrite {
                "already present"
            } else {
                "written"
            }
        ),
    ];

    // Install just-bash if node + a package manager are available.
    let modules_present = dir.join("node_modules").join("just-bash").exists();
    match (&node, modules_present) {
        (Some(node_bin), false) => {
            lines.push(format!("- node detected: {node_bin}"));
            match install_deps(&dir) {
                Ok(pm) => lines.push(format!("- installed just-bash via {pm}")),
                Err(e) => lines.push(format!(
                    "- could NOT install just-bash automatically ({e}). Run `npm install` (or pnpm/bun) in {}.",
                    dir.display()
                )),
            }
        }
        (Some(node_bin), true) => {
            lines.push(format!("- node detected: {node_bin}"));
            lines.push("- just-bash already installed".to_string());
        }
        (None, _) => {
            lines.push(
                "- node NOT found on PATH. Install Node.js, then re-run setup to install just-bash."
                    .to_string(),
            );
        }
    }
    Ok(ToolOutput::new(lines.join("\n")))
}

/// Try common package managers to install the sidecar deps. Returns the manager
/// name used. Network/permission failures are surfaced to the caller.
fn install_deps(dir: &std::path::Path) -> Result<&'static str> {
    for (pm, args) in [
        ("pnpm", &["install", "--prod"][..]),
        ("bun", &["install"][..]),
        ("npm", &["install", "--no-audit", "--no-fund"][..]),
    ] {
        if which(pm).is_some() {
            let status = std::process::Command::new(pm)
                .args(args)
                .current_dir(dir)
                .status()
                .with_context(|| format!("run {pm} install"))?;
            if status.success() {
                return Ok(pm);
            }
            return Err(anyhow!("{pm} install exited with {status}"));
        }
    }
    Err(anyhow!("no package manager (pnpm/bun/npm) found on PATH"))
}

fn which(bin: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_string()
    };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(&exe))
            .find(|c| c.is_file())
    })
}

fn status_output() -> Result<ToolOutput> {
    let dir = jsbash_dir()?;
    let server = dir.join("server.mjs");
    let modules = dir.join("node_modules").join("just-bash");
    let node = resolve_node();
    let body = format!(
        "jsbash native Jcode status:\n- Sidecar dir: {}\n- server.mjs present: {}\n- just-bash installed: {}\n- node on PATH: {}",
        dir.display(),
        yes_no(server.exists()),
        yes_no(modules.exists()),
        node.as_deref().unwrap_or("NO"),
    );
    Ok(ToolOutput::new(body))
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

#[cfg(test)]
mod tests;
