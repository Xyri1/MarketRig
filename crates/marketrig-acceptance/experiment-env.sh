#!/usr/bin/env bash
# Seeds one attended experiment cell's environment on macOS (EXPERIMENT.md §1, §7).
#
#   source crates/marketrig-acceptance/experiment-env.sh codex   # or claude
#   cargo test -p marketrig-acceptance --test experiment -- --nocapture
#
# Builds the Hindsight launcher venv once under $HINDSIGHT_VENV (default
# ~/.hindsight), verifies its capability marker, and exports the five R4
# variables. The key is taken from MARKETRIG_EXPERIMENT_MEMORY_API_KEY if
# already set, else read silently from the prompt; it is never echoed or
# written anywhere. Must be sourced, not run.

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  echo "source this file; do not run it" >&2
  exit 2
fi

_cell="${1:-codex}"
case "$_cell" in codex | claude) ;; *)
  echo "usage: source experiment-env.sh codex|claude" >&2
  return 2
  ;;
esac

_venv="${HINDSIGHT_VENV:-$HOME/.hindsight}"
_launcher="$_venv/bin/hindsight-api"
if [[ ! -x "$_launcher" ]]; then
  echo "building $_venv (hindsight-api-slim[embedded-db]==0.9.2)…" >&2
  uv venv "$_venv" --python 3.12 || return 1
  uv pip install --python "$_venv/bin/python" 'hindsight-api-slim[embedded-db]==0.9.2' || return 1
fi
if ! "$_launcher" --help 2>/dev/null | grep -q HINDSIGHT_API_PORT; then
  echo "$_launcher does not print the HINDSIGHT_API_PORT marker; discovery would refuse it" >&2
  return 1
fi

if [[ -z "${MARKETRIG_EXPERIMENT_MEMORY_API_KEY:-}" ]]; then
  printf "provider API key (not echoed): " >&2
  read -rs MARKETRIG_EXPERIMENT_MEMORY_API_KEY
  echo >&2
fi
[[ -n "$MARKETRIG_EXPERIMENT_MEMORY_API_KEY" ]] || { echo "no key given" >&2; return 1; }

export MARKETRIG_EXPERIMENT="$_cell"
export MARKETRIG_EXPERIMENT_HINDSIGHT="$_launcher"
export MARKETRIG_EXPERIMENT_MEMORY_BASE_URL="${MARKETRIG_EXPERIMENT_MEMORY_BASE_URL:-https://openrouter.ai/api/v1}"
export MARKETRIG_EXPERIMENT_MEMORY_API_KEY
export MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL="${MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL:-z-ai/glm-5.3-flash}"
export MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL="${MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL:-openai/text-embedding-3-small}"
unset MARKETRIG_ACCEPTANCE_OUT

echo "cell=$MARKETRIG_EXPERIMENT launcher=$MARKETRIG_EXPERIMENT_HINDSIGHT" >&2
echo "base_url=$MARKETRIG_EXPERIMENT_MEMORY_BASE_URL llm=$MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL embedding=$MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL key=set" >&2
unset _cell _venv _launcher
