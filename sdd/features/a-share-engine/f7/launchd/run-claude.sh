#!/bin/bash
# Fresh non-interactive Claude Code session for the R2 follow-up (FEASIBILITY.md).
# $1 = prompt file. Runs from the feasibility worktree; logs beside the samples.
set -u
export PATH="/Users/xyril/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
export TZ=Asia/Shanghai
export CARGO_TARGET_DIR=/Users/xyril/Projects/MarketRig/target
cd /Users/xyril/Projects/MarketRig/.worktrees/a-share-feasibility || exit 2
LOG=sdd/features/a-share-engine/f7/intraday/claude-$(basename "$1" .md)-$(date +%Y%m%dT%H%M%S%z).log
claude -p "$(cat "$1")" --model claude-opus-5 \
  --allowedTools "Bash,Read,Edit,Write,Glob,Grep" \
  > "$LOG" 2>&1
echo "exit=$?" >> "$LOG"
