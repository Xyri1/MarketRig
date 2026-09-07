#!/usr/bin/env bash
# Selects one attended experiment cell on macOS (EXPERIMENT.md §1, §2).
#
#   source crates/marketrig-acceptance/experiment-env.sh codex   # or claude
#   cargo test -p marketrig-acceptance --test experiment -- --nocapture
#
# E5's Hindsight and provider variables went with Hindsight; E6's arrive with
# slice 011's C58. Must be sourced, not run.

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
