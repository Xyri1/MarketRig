You are a fresh Claude Code session started by launchd at 09:32 Asia/Shanghai on 2026-09-10 for the R2 follow-up of the A-share feasibility check. Working directory: /Users/xyril/Projects/MarketRig/.worktrees/a-share-feasibility (branch codex/a-share-feasibility). Read crates/marketrigd/src/feasibility/README.md (rules: never print or write the HiThink key; no subagents), then sdd/features/a-share-engine/FEASIBILITY.md "Follow-up handoff → R2", then the R2 section of sdd/features/a-share-engine/F7-EVIDENCE.md.

launchd has just run `sdd/features/a-share-engine/f7/capture-intraday.sh open-0931` (file under sdd/features/a-share-engine/f7/intraday/open-0931-*.json). If it is missing or empty, run that command yourself once. Then:

1. Confirm from its calendar block that 20260910 is `max_date`; if not, record that the day is a holiday, that all windows are void, and stop.
2. Read the sample and answer, for the opening window only: does today's daily bar exist at 09:31; does its close_price track the snapshot last_price across the three reads; do volume/turnover move; what do auction_phase/data_status say; does data.timestamp track the wall clock. Keep discrepancies; do not smooth them.
3. Append a short dated subsection "R2 — opening window 2026-09-10 (interim)" to F7-EVIDENCE.md with the file path, the answers, and what remains for the later windows. No conclusions about source delay: it stays unknown.
4. `git add sdd/features/a-share-engine/F7-EVIDENCE.md sdd/features/a-share-engine/f7/intraday/` and commit "spike(a-share): R2 opening-window interim sample". Never `git add -A`, never amend, never push.

Be literal and short.
