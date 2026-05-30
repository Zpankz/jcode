# Hook Layer + just-bash Sandbox + Context-SNR Integration — Design

Status: design approved-for-implementation
Scope: jcode selfdev. Strictly additive; non-destructive defaults.

## Goals

1. **Comprehensive hook layer** — a single, well-typed interception point for every
   tool call, mirroring Claude Code's `PreToolUse` / `PostToolUse` hooks but native
   to jcode (Rust). Hooks can: observe, block/deny, rewrite input, rewrite/replace
   output, and emit events. Configured declaratively under `~/.jcode/hooks/`.

2. **just-bash sandbox tool** — wrap `just-bash` (Vercel Labs, TS/QuickJS virtual
   bash + JS/TS code execution over a virtual or overlay filesystem) as a native
   jcode tool `jsbash`, giving the agent a deterministic, network-firewalled,
   code-execution sandbox. This is the substrate for *programmatic workflow
   orchestration between swarm agents*: agents exchange/compose code+data through a
   shared sandbox filesystem instead of dumping raw bytes into context.

3. **Swarm-native programmatic orchestration** — let swarm members share a sandbox
   workspace + run TS orchestration snippets (`jsbash exec`) whose results are
   addressable by other members via the `communicate` share store, so coordination
   logic is *code*, not prose.

4. **Context SNR survey** — identify and wire additional upstream repos that improve
   programmatic context management (raw-bytes-out-of-context, indexed retrieval),
   closing gaps not covered by context-mode / rtk / caveman.

## Architecture

### Where hooks attach

`Registry::execute` (`crates/jcode-app-core/src/tool/mod.rs:481`) is the single
chokepoint for all tool execution. It already runs a built-in post-hook
(`guard_context_overflow`). We insert:

```
execute(name, input, ctx)
  └─ resolve_tool_name
  └─ session policy gate            (existing)
  └─ HOOKS: PreToolUse  ───────────► may Deny / Rewrite(input) / Continue
  └─ tool.execute(input', ctx)
  └─ HOOKS: PostToolUse ───────────► may Rewrite(output) / Continue
  └─ guard_context_overflow         (existing, runs after PostToolUse)
```

### Hook model (`tool/hooks/`)

```rust
pub enum HookEvent { PreToolUse, PostToolUse }

pub struct PreToolUseInput<'a> {
    pub tool_name: &'a str,
    pub input: &'a Value,
    pub ctx: &'a ToolContext,
}
pub enum PreToolUseDecision {
    Continue,                 // unchanged
    RewriteInput(Value),      // replace tool input
    Deny { reason: String },  // block, return reason as tool error
}

pub struct PostToolUseInput<'a> {
    pub tool_name: &'a str,
    pub input: &'a Value,
    pub output: &'a ToolOutput,
    pub ctx: &'a ToolContext,
}
pub enum PostToolUseDecision {
    Continue,
    RewriteOutput(ToolOutput),
}

#[async_trait]
pub trait Hook: Send + Sync {
    fn name(&self) -> &str;
    fn matches(&self, tool_name: &str) -> bool;          // matcher (glob/regex/exact)
    async fn pre(&self, _: PreToolUseInput<'_>) -> Result<PreToolUseDecision> { Ok(Continue) }
    async fn post(&self, _: PostToolUseInput<'_>) -> Result<PostToolUseDecision> { Ok(Continue) }
}
```

Two hook source kinds:

- **Native hooks** — Rust impls registered in-process (e.g. an `RtkRewriteHook` that
  rewrites `bash` commands to `rtk <cmd>` — finally delivering rtk's transparent
  rewrite that was previously only advisory).
- **Command hooks** — external programs configured in `~/.jcode/hooks.json`, invoked
  with a JSON payload on stdin and returning a JSON decision on stdout (exactly the
  Claude Code hook contract). Fail-open by default (exit!=0 or timeout ⇒ Continue),
  matching upstream rtk/context-mode hook semantics.

`hooks.json` schema (Claude-Code-equivalent):

```json
{
  "PreToolUse": [
    { "matcher": "bash", "command": "~/.jcode/hooks/rtk-rewrite.sh", "timeout_ms": 3000 }
  ],
  "PostToolUse": [
    { "matcher": "bash|jsbash", "native": "context_guard" }
  ]
}
```

Decision contract (stdout JSON), superset-compatible with Claude Code:

```json
{ "decision": "continue" }
{ "decision": "deny", "reason": "blocked by policy" }
{ "decision": "rewriteInput", "input": { ... } }      // PreToolUse only
{ "decision": "rewriteOutput", "output": "..." }      // PostToolUse only
{ "updatedInput": { ... } }                            // Claude Code rtk-style alias for rewriteInput
```

A `HookRegistry` is built once (loaded from config + native registrations), stored on
`Registry`, and consulted in `execute`. Ordering: native hooks first, then command
hooks, in config order; first `Deny` wins; rewrites chain.

Safety: hooks are **fail-open** and **time-boxed**; a panicking/erroring hook never
breaks tool execution (logged, treated as Continue). Hook execution is feature-gated
off when no `hooks.json` and no native hooks ⇒ zero overhead for default installs.

### `jsbash` tool (just-bash bridge)

just-bash is a Node package; jcode is Rust. Bridge via a tiny long-lived Node
**sidecar** (`assets/jsbash/server.mjs`) that:

- boots a single `Bash` instance with `javascript: true`, `python` optional,
  `MountableFs`: read-only `OverlayFs` mount of the session working dir at
  `/workspace` + read-write `InMemoryFs` (or `ReadWriteFs` of a per-session sandbox
  dir under `~/.jcode/jsbash/<session>/`) at `/sandbox`,
- network firewalled (allow-list from config; default: none),
- speaks newline-delimited JSON over stdio: `{id, script, stdin, cwd, env, timeoutMs}`
  ⇒ `{id, stdout, stderr, exitCode}`.

Rust side: `JsBashTool` (actions: `exec`, `write_file`, `read_file`, `ls`, `reset`)
manages one sidecar per session via a pooled `tokio::process::Child`, lazily spawned,
auto-restarted on crash. If Node is unavailable, the tool reports a clear install
hint and stays inert (non-fatal).

Why a sandbox tool and not replacing `bash`: real `bash` mutates the host and is
required for builds/tests/git. `jsbash` is the *deterministic, safe, programmable*
surface — "think in code" for data shaping, multi-file synthesis, and swarm
orchestration — keeping raw bytes out of context (the context-mode philosophy, now
first-class and offline, no MCP/npx round-trip).

### Swarm programmatic orchestration

- Shared sandbox: swarm members of the same `swarm_id` mount the **same** `/sandbox`
  (a `ReadWriteFs` under `~/.jcode/jsbash/swarm-<swarm_id>/`). One agent writes
  `plan.json` / partial results as files; another runs a `jsbash exec` TS snippet to
  reduce/merge them; results are announced via `communicate share`.
- New `communicate` affordance is unnecessary at first: orchestration = (write files
  to shared sandbox) + (share a pointer key). We add a thin convention + docs and a
  `jsbash exec --swarm` flag that selects the shared mount.

### Context-SNR survey (repos to consider)

Evaluated for filling gaps beyond context-mode (sandbox/index), rtk (output filter),
caveman (output terseness):

| Repo | Gap it closes | Integration verdict |
|---|---|---|
| **just-bash** (vercel-labs) | deterministic TS/JS code-exec sandbox, virtual FS, jq/sqlite/csv/yaml builtins, network firewall | **integrate now** as `jsbash` |
| **repomix** / **ai-digest** | whole-repo → single compressed, token-counted digest for retrieval | candidate: `jsbash`-able via curl/index, or a `repo_digest` routing rule |
| **llm / ttok / tiktoken-cli** | exact token counting to drive truncation decisions | candidate: feed `guard_context_overflow` real counts (currently chars/4 estimate) |
| **ripgrep + ast-grep (sg)** | structural code search with tiny output | jcode already has agentgrep/codesearch; add ast-grep routing rule |
| **files-to-prompt** (simonw) | selective file packing with budget | overlaps repomix; routing rule only |
| **fabric** patterns | reusable prompt/skills for summarize/extract | overlaps caveman/skills; skip |

Decision: implement `jsbash` + hook layer now (this task). Add a follow-up
"context-snr" routing block documenting repo_digest (repomix) and exact token
counting (ttok) as preferred tools, wired through the same `integration_support`
pattern, plus a native `TokenCountHook` stub if `ttok` present. Keep scope bounded:
the hook layer + jsbash are the load-bearing new capabilities; the rest are routing
rules reusing existing machinery.

## Implementation phases (verifiable)

P1. Hook layer core (`tool/hooks/mod.rs`): traits, decisions, `HookRegistry`,
    config loader (`hooks.json`), command-hook runner (fail-open, timeboxed),
    native-hook registry. Unit tests for matcher, decision parsing, fail-open,
    chaining, deny-wins.

P2. Wire `HookRegistry` into `Registry::execute` (pre/post). Tests: deny blocks;
    rewriteInput reaches tool; rewriteOutput replaces output; no-config = no-op.

P3. Native `RtkRewriteHook` (delivers rtk transparent rewrite) gated on rtk presence
    + opt-in flag in rtk manifest/config. Tests.

P4. `jsbash` tool + Node sidecar asset + sidecar lifecycle mgr. Tests: exec echo,
    js-exec arithmetic, file write/read roundtrip, overlay read of workspace,
    Node-absent graceful path, timeout/cancel.

P5. Swarm shared-sandbox convention + docs + `--swarm` mount selection. Tests:
    two contexts same swarm_id share `/sandbox`.

P6. `context-snr` routing block (repomix/ttok/ast-grep) via integration_support +
    optional `jsbash`-backed `repo_digest` helper. Tests.

Each phase: scoped commit, adversarial tests, background validation script,
memex retro at the end.
