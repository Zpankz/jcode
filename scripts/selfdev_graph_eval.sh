#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

repo_alias=${JCODE_GRAPH_REPO_ALIAS:-jcode}
gitnexus_scope=${JCODE_GRAPH_SCOPE:-all}
codegraph_base=${JCODE_GRAPH_BASE:-HEAD}
refresh=0
brief=0

usage() {
  cat <<'USAGE'
Usage:
  scripts/selfdev_graph_eval.sh [options]

Runs the graph-aware self-dev evaluation gate for Jcode changes.

Options:
  --refresh          Rebuild GitNexus and code-review-graph indexes first.
  --repo <alias>     GitNexus repository alias (default: jcode).
  --scope <scope>    GitNexus diff scope: unstaged, staged, all, compare (default: all).
  --base <ref>       code-review-graph diff base (default: HEAD for working tree changes).
  --brief            Use code-review-graph's brief output.
  -h, --help         Show this help.

Environment:
  JCODE_GRAPH_REPO_ALIAS   Default GitNexus alias.
  JCODE_GRAPH_SCOPE        Default GitNexus diff scope.
  JCODE_GRAPH_BASE         Default code-review-graph base ref.

This script intentionally combines two independent graph views:
  - GitNexus: changed symbols and affected execution flows for the current diff.
  - code-review-graph: risk-scored changed functions, affected flows, and test gaps.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --refresh)
      refresh=1
      ;;
    --repo)
      if [[ $# -lt 2 ]]; then
        printf 'error: --repo requires an alias\n' >&2
        exit 2
      fi
      repo_alias="$2"
      shift
      ;;
    --scope)
      if [[ $# -lt 2 ]]; then
        printf 'error: --scope requires one of unstaged, staged, all, compare\n' >&2
        exit 2
      fi
      gitnexus_scope="$2"
      shift
      ;;
    --base)
      if [[ $# -lt 2 ]]; then
        printf 'error: --base requires a git ref\n' >&2
        exit 2
      fi
      codegraph_base="$2"
      shift
      ;;
    --brief)
      brief=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'error: unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

case "$gitnexus_scope" in
  unstaged|staged|all|compare) ;;
  *)
    printf 'error: invalid --scope: %s\n' "$gitnexus_scope" >&2
    exit 2
    ;;
esac

require_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    printf 'error: required command not found: %s\n' "$cmd" >&2
    exit 127
  fi
}

section() {
  printf '\n## %s\n\n' "$1"
}

run_step() {
  local label="$1"
  shift
  printf '+ %s\n' "$*" >&2
  if "$@"; then
    return 0
  else
    local status=$?
    printf '\nerror: %s failed with exit code %s\n' "$label" "$status" >&2
    exit "$status"
  fi
}

require_cmd git
require_cmd gitnexus
require_cmd code-review-graph

current_commit=$(git rev-parse --short HEAD)
working_changes=$(git status --short | wc -l | tr -d ' ')

section "Jcode graph self-evaluation"
printf -- '- Repo: `%s`\n' "$repo_root"
printf -- '- Commit: `%s`\n' "$current_commit"
printf -- '- Working tree changes: `%s`\n' "$working_changes"
printf -- '- GitNexus alias/scope: `%s` / `%s`\n' "$repo_alias" "$gitnexus_scope"
printf -- '- code-review-graph base: `%s`\n' "$codegraph_base"

section "Git status snapshot"
if [[ "$working_changes" == "0" ]]; then
  printf 'Working tree clean.\n'
else
  git status --short | sed -n '1,40p'
  if [[ "$working_changes" -gt 40 ]]; then
    printf '... %s more change(s) omitted\n' "$((working_changes - 40))"
  fi
fi

section "Tool versions"
printf -- '- gitnexus: `%s`\n' "$(gitnexus --version 2>/dev/null || printf 'unknown')"
printf -- '- code-review-graph: `%s`\n' "$(code-review-graph --version 2>/dev/null || printf 'unknown')"

if [[ "$refresh" -eq 1 ]]; then
  section "Refreshing indexes"
  run_step "gitnexus analyze" gitnexus analyze --force --name "$repo_alias" .
  run_step "code-review-graph build" code-review-graph build --repo "$repo_root"
fi

section "Index status"
run_step "gitnexus status" gitnexus status
run_step "code-review-graph status" code-review-graph status --repo "$repo_root"

section "GitNexus changed-symbol and flow impact"
gitnexus_args=(detect-changes -r "$repo_alias" --scope "$gitnexus_scope")
if [[ "$gitnexus_scope" == "compare" ]]; then
  gitnexus_args+=(--base-ref "$codegraph_base")
fi
run_step "gitnexus detect-changes" gitnexus "${gitnexus_args[@]}"

section "code-review-graph risk and test-gap analysis"
codegraph_args=(detect-changes --repo "$repo_root" --base "$codegraph_base")
if [[ "$brief" -eq 1 ]]; then
  codegraph_args+=(--brief)
fi
run_step "code-review-graph detect-changes" code-review-graph "${codegraph_args[@]}"

section "How to use this output"
cat <<'GUIDANCE'
- Check that changed symbols match the intended task scope.
- Treat HIGH or CRITICAL GitNexus impact as a design-review trigger before editing more.
- Investigate every code-review-graph test gap unless an adjacent targeted test already covers it.
- If either index is stale, rerun with `--refresh` before trusting the analysis.
GUIDANCE
