#!/usr/bin/env node
// jcode jsbash sidecar.
//
// A long-lived Node process bridging jcode's native `jsbash` tool to the
// `just-bash` virtual bash sandbox (https://github.com/vercel-labs/just-bash).
//
// Protocol: newline-delimited JSON (NDJSON) over stdio.
//   Request  (stdin):  {"id": <string>, "op": <string>, ...op-specific fields}
//   Response (stdout): {"id": <string>, "ok": true, ...}  or
//                      {"id": <string>, "ok": false, "error": <string>}
//
// One sidecar instance owns ONE `Bash` instance, so the virtual filesystem and
// any created files persist across requests for the life of the process. jcode
// spawns one sidecar per session (and per swarm shared sandbox).
//
// Ops:
//   ready                                  -> {ok, version}
//   exec   {script, stdin?, cwd?, env?, timeoutMs?, args?}
//                                          -> {ok, stdout, stderr, exitCode}
//   write  {path, content}                 -> {ok}
//   read   {path}                          -> {ok, content}
//   ls     {path?}                         -> {ok, stdout} (tree listing)
//   reset                                  -> {ok}  (fresh Bash instance)
//
// Boot configuration is taken from argv/env:
//   JSBASH_WORKSPACE   absolute path mounted read-only (OverlayFs) at the cwd
//   JSBASH_SANDBOX     absolute path mounted read-write (ReadWriteFs) -- optional
//   JSBASH_PYTHON      "1" to enable python3/python
//   JSBASH_NETWORK     comma-separated allow-list of hosts (default: none)
//
// All failures are reported as structured JSON; the process only exits when
// stdin closes. This keeps the Rust side simple and fail-safe.

import process from "node:process";
import readline from "node:readline";

let Bash, OverlayFs, ReadWriteFs, InMemoryFs;
let importError = null;
try {
  const mod = await import("just-bash");
  ({ Bash, OverlayFs, ReadWriteFs, InMemoryFs } = mod);
} catch (err) {
  importError = err && err.message ? err.message : String(err);
}

const WORKSPACE = process.env.JSBASH_WORKSPACE || null;
const SANDBOX = process.env.JSBASH_SANDBOX || null;
const ENABLE_PYTHON = process.env.JSBASH_PYTHON === "1";
const NETWORK_ALLOW = (process.env.JSBASH_NETWORK || "")
  .split(",")
  .map((s) => s.trim())
  .filter(Boolean);

const DEFAULT_TIMEOUT_MS = 30_000;

function buildBash() {
  if (importError) {
    throw new Error(`just-bash unavailable: ${importError}`);
  }
  const opts = { javascript: true };
  if (ENABLE_PYTHON) opts.python = true;

  // Single filesystem (MountableFs double-prefixes mount paths, so we pick one
  // backing fs per sidecar). Precedence:
  //  - SANDBOX set  -> ReadWriteFs onto a real dir (swarm shared sandbox; writes
  //    hit disk so other members see them). Trusted-code separation is the
  //    caller's responsibility (jcode points this at ~/.jcode/jsbash/...).
  //  - WORKSPACE set -> OverlayFs over the session working dir: reads come from
  //    disk, writes stay in memory (host never mutated). Default safe mode.
  //  - neither       -> pure InMemoryFs.
  let fs;
  if (SANDBOX) {
    fs = new ReadWriteFs({ root: SANDBOX });
  } else if (WORKSPACE) {
    fs = new OverlayFs({ root: WORKSPACE });
  } else {
    fs = new InMemoryFs();
  }
  opts.fs = fs;
  // OverlayFs/ReadWriteFs expose their mount point; InMemoryFs does not.
  if (typeof fs.getMountPoint === "function") {
    opts.cwd = fs.getMountPoint();
  }

  if (NETWORK_ALLOW.length > 0) {
    opts.network = { allow: NETWORK_ALLOW };
  }
  return new Bash(opts);
}

let bash = null;
function ensureBash() {
  if (!bash) bash = buildBash();
  return bash;
}

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}

async function handle(req) {
  const id = req.id;
  try {
    switch (req.op) {
      case "ready": {
        if (importError) {
          return send({ id, ok: false, error: `just-bash unavailable: ${importError}` });
        }
        return send({ id, ok: true, version: "just-bash@3.0.1" });
      }
      case "exec": {
        const env = ensureBash();
        const timeoutMs = Number.isFinite(req.timeoutMs) ? req.timeoutMs : DEFAULT_TIMEOUT_MS;
        const controller = new AbortController();
        const timer = setTimeout(() => controller.abort(), timeoutMs);
        try {
          const result = await env.exec(req.script || "", {
            stdin: req.stdin,
            cwd: req.cwd,
            env: req.env,
            args: Array.isArray(req.args) ? req.args : undefined,
            signal: controller.signal,
          });
          return send({
            id,
            ok: true,
            stdout: result.stdout ?? "",
            stderr: result.stderr ?? "",
            exitCode: result.exitCode ?? 0,
          });
        } finally {
          clearTimeout(timer);
        }
      }
      case "write": {
        const env = ensureBash();
        // Use the virtual fs directly so binary-safe and path-normalized.
        await env.fs.writeFile(req.path, req.content ?? "");
        return send({ id, ok: true });
      }
      case "read": {
        const env = ensureBash();
        const bytes = await env.fs.readFile(req.path);
        const content = typeof bytes === "string" ? bytes : Buffer.from(bytes).toString("utf8");
        return send({ id, ok: true, content });
      }
      case "ls": {
        const env = ensureBash();
        const target = req.path || ".";
        const result = await env.exec(`tree ${JSON.stringify(target)}`);
        return send({ id, ok: true, stdout: result.stdout ?? "" });
      }
      case "reset": {
        bash = null;
        ensureBash();
        return send({ id, ok: true });
      }
      default:
        return send({ id, ok: false, error: `unknown op: ${req.op}` });
    }
  } catch (err) {
    return send({ id, ok: false, error: err && err.message ? err.message : String(err) });
  }
}

const rl = readline.createInterface({ input: process.stdin });
// Track in-flight requests so a stdin EOF drains pending work before exiting.
const inflight = new Set();
rl.on("line", (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let req;
  try {
    req = JSON.parse(trimmed);
  } catch (err) {
    return send({ id: null, ok: false, error: `bad JSON request: ${err.message}` });
  }
  // Requests carry an id, so out-of-order completion is fine; track the promise
  // so `close` can await all outstanding work.
  const p = handle(req).finally(() => inflight.delete(p));
  inflight.add(p);
});
rl.on("close", async () => {
  await Promise.allSettled([...inflight]);
  process.exit(0);
});
