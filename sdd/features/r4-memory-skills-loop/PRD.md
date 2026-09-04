# R4 — Memory, skills, and the closed loop: Feature PRD

**Milestone:** [R4](../../ROADMAP.md#milestone-r4--memory-skills-and-the-closed-loop)
**Status:** Design complete — PRD, DECISIONS, and SPEC written 2026-09-04; implementation not started

This feature designs Milestone R4: the last piece of the loop. It refines `sdd/SPEC.md` §4.4, §5.1, §5.2, §13.2, §15, §16, and §17 and invents nothing beyond them.

## 1. Motivation

*Decision basis: per D16, D17, D18, D19, D20, D21, D22, D47, D49, D65.*

R1 queues an `EVALUATION` prompt for every closed position cycle and R3 delivers it to a live session, but nothing the agent learns from it survives the conversation: there is no memory surface, the seeded `AGENTS.md` is a placeholder, and the canonical skill tree does not exist. The product's differentiator is a desk that keeps improving across disposable conversations, and that claim is only true once a lesson retained in one session can be recalled in the next and a procedure improved in one session is loaded by the next. R4 is where `marketrigd` first runs the supervised Hindsight child, where `marketrig memory` exists, and where a desk is seeded with a real constitution and a real improvement skill. It is provable now because a realized outcome can reach a live session (R1 and R3 both standing).

## 2. Outcome

A round trip closes on a desk. The closing fill's unit inserts the cycle and its `EVALUATION` prompt; the dispatcher wakes or activates the desk's session and hands the prompt over as ordinary input. The agent, guided by its constitution and the seeded improvement skill, reads the cycle through `marketrig history`, judges the outcome with realized P&L as the reward, retains one desk-specific lesson through `marketrig memory retain`, and edits a skill under `.agents/skills/`. The session ends. A later session — resumed or new, on either runtime — recalls that lesson through `marketrig memory recall` and loads the improved skill from the same directory, through `.agents/skills` on Codex and `.claude/skills` on Claude Code. Another desk's recall returns nothing of it. When Hindsight is stopped, `marketrig memory status` says so, every memory command fails with one explicit code, and sessions, triggers, and paper trading continue unchanged.

## 3. Scope

R4 delivers six things, each a thin vertical of a contract `sdd/SPEC.md` already states:

1. **The memory child** (per D47, D65, D73): one supervised `hindsight-api` process per installation on a launcher the installation names, bound to loopback on a daemon-picked port, authenticated with a per-start bearer, MCP and telemetry off, embedded pg0 under a MarketRig-named instance, recorded in `children.json`, started lazily, restarted once, and explicitly `UNAVAILABLE` after that.
2. **Provider settings** (per D18, D49): one installation resource for the OpenAI-compatible base URL, API key, LLM model, and embedding model; the key in the OS credential store through `keyring`; a live model list that is never persisted; and the embedding lock.
3. **Desk banks and the memory commands** (per D16, D17, D65): one bank per desk derived from its UUID, never named by anyone outside `marketrigd`; `marketrig memory status | retain | recall | reflect` as synchronous desk-scoped pass-throughs with trigger attribution and no MarketRig-side content store.
4. **The seeds** (per D19, D20, D21, D22): the real `AGENTS.md` constitution, the `.agents/skills/` tree with the seeded improvement skill, and the `.claude/skills` link — created at desk creation, never rewritten after `READY`, the link alone reconciled at startup.
5. **Durable evidence** (per D71): the two settings rows, the memory event kinds, and the recovery step that reaps a crashed daemon's Hindsight child through the existing `children.json` mechanism.
6. **Acceptance** (per D67, D75): a stand-in memory child that speaks the consumed HTTP subset, gate scenarios in which the harness performs the agent's steps through the same CLI a session would run, and one attended scenario per cell on real Hindsight behind a real hosted endpoint.

## 4. Non-goals

- No daemon-authored memory: MarketRig never retains a transcript, a trigger result, a trade, or an evaluation on the agent's behalf, and never mutates a skill (per D17, D22).
- No decision about what the agent should learn: the evaluation prompt's payload stays the R1 history reference, and the improvement skill is guidance the agent may rewrite the day it is seeded.
- No Hindsight surface beyond retain, recall, reflect, and status: no bank profile editing, no entity or observation routes, no document import, no MCP, no control plane (per D16).
- No local models, no reranker, no embedding-model change or data erasure workflow (root §18).
- No desktop settings page: the provider resource is REST now and R5's onboarding form later.
- No installer: R4 runs the launcher the installation names; the bundled interpreter and wheel set arrive with R6 packaging (per D47), and until then the operator names a launcher built from the pinned wheel.
- No per-runtime skill trees, no skill synchronization, no skill registry (per D21).
- No reflection cadence, no scheduled evaluation, no memory of memories.

## 5. Success criteria

1. On both platforms, with the memory child configured, a closed cycle's `EVALUATION` prompt reaches a session, and a retain issued by that session lands in that desk's bank with `MEMORY_RETAINED` naming the desk and its attribution; a recall on the same desk returns it and a recall on another desk does not.
2. A desk created after R4 carries the seeded constitution, the seeded improvement skill under `.agents/skills/`, and a `.claude/skills` link through which the same file is readable; none of the three is rewritten after `READY`, and a missing link is recreated at startup without touching anything else.
3. A skill file written by one session is read by a later session on either runtime through its own path.
4. With Hindsight stopped, `memory status` reports `UNAVAILABLE` with the reason, every memory command answers `MEMORY_UNAVAILABLE`, and a trigger firing, an order, and a session activation on the same daemon succeed; one automatic restart is attempted, a second loss holds until Retry.
5. The provider API key and the child's bearer never appear in SQLite, the daemon log, an operational event, a prompt, or CLI output; the embedding model refuses to change once locked.
6. A hard-killed daemon leaves no Hindsight child alive on macOS, and the next start's recovery event says what it reaped.
7. The gate covers criteria 1, 2, 4, 5, and 6 unattended on the stand-in memory child on both platforms; the attended scenario reproduces criteria 1 and 3 once per platform-and-runtime cell on real Hindsight, with the agent-owned steps ending inconclusive rather than failed.
