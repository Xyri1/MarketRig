#!/usr/bin/env bash
# Selects one attended experiment cell on macOS and seeds E6's environment
# (EXPERIMENT.md §1, §2, §7).
#
#   source crates/marketrig-acceptance/experiment-env.sh codex   # or claude
#   cargo test -p marketrig-acceptance --test experiment -- --nocapture
#
# E1–E4 need only the cell. E6 additionally needs a Python that reports exactly
# 3.12, a Node 22 or newer, the locked wheel directory
# `node scripts/openviking-wheels.mjs --python <python3.12>` produced, and one
# OpenAI-compatible provider. Anything missing leaves E6's variables unset,
# which skips E6 with evidence and still runs the rest of the cell. The key is
# taken from MARKETRIG_EXPERIMENT_MEMORY_API_KEY if already set, else read
# silently from the prompt; it is never echoed or written anywhere. Must be
# sourced, not run.

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  echo "source this file; do not run it" >&2
  exit 2
fi

case "${1:-codex}" in codex | claude) export MARKETRIG_EXPERIMENT="${1:-codex}" ;; *)
  echo "usage: source experiment-env.sh codex|claude" >&2
  return 2
  ;;
esac
unset MARKETRIG_ACCEPTANCE_OUT
echo "cell=$MARKETRIG_EXPERIMENT" >&2

_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
_python="${MARKETRIG_EXPERIMENT_PYTHON:-$(command -v python3.12)}"
_node="${MARKETRIG_EXPERIMENT_NODE:-$(command -v node)}"
_wheels="${MARKETRIG_EXPERIMENT_WHEELS:-$_root/openviking-wheels/macos-arm64}"

# The same two probes the daemon runs (feature SPEC §1.2), so a wrong
# interpreter costs a line here instead of an attended hour.
_python_minor="$([[ -x $_python ]] && "$_python" -c 'import sys;print("%d.%d"%sys.version_info[:2])' 2>/dev/null)"
_node_major="$([[ -x $_node ]] && "$_node" --version 2>/dev/null)"
_node_major="${_node_major#v}"
_node_major="${_node_major%%.*}"
_ready=1
[[ "$_python_minor" == "3.12" ]] || _ready=0
[[ -n "$_node_major" ]] && ((_node_major >= 22)) || _ready=0
[[ -d "$_wheels" ]] || _ready=0

if ((_ready)) && [[ -z "${MARKETRIG_EXPERIMENT_MEMORY_API_KEY:-}" ]]; then
  printf "provider API key for E6 (not echoed; empty skips E6): " >&2
  read -rs MARKETRIG_EXPERIMENT_MEMORY_API_KEY
  echo >&2
fi
[[ -n "${MARKETRIG_EXPERIMENT_MEMORY_API_KEY:-}" ]] || _ready=0

if ((_ready)); then
  export MARKETRIG_EXPERIMENT_PYTHON="$_python"
  export MARKETRIG_EXPERIMENT_NODE="$_node"
  export MARKETRIG_EXPERIMENT_WHEELS="$_wheels"
  export MARKETRIG_EXPERIMENT_MEMORY_BASE_URL="${MARKETRIG_EXPERIMENT_MEMORY_BASE_URL:-https://openrouter.ai/api/v1}"
  export MARKETRIG_EXPERIMENT_MEMORY_API_KEY
  export MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL="${MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL:-z-ai/glm-5.3-flash}"
  export MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL="${MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL:-openai/text-embedding-3-small}"
  echo "E6: python=$MARKETRIG_EXPERIMENT_PYTHON node=$MARKETRIG_EXPERIMENT_NODE wheels=$MARKETRIG_EXPERIMENT_WHEELS" >&2
  echo "E6: base_url=$MARKETRIG_EXPERIMENT_MEMORY_BASE_URL llm=$MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL embedding=$MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL key=set" >&2
else
  unset MARKETRIG_EXPERIMENT_PYTHON MARKETRIG_EXPERIMENT_NODE MARKETRIG_EXPERIMENT_WHEELS
  echo "E6 will skip: python=${_python:-none} (${_python_minor:-?}) node=${_node:-none} (${_node_major:-?}) wheels=$_wheels key=${MARKETRIG_EXPERIMENT_MEMORY_API_KEY:+set}" >&2
fi
unset _root _python _node _wheels _python_minor _node_major _ready
