# Trigger invocation — Feature PRD

**Milestone:** R7 — event triggers, localization, and packaging
**Status:** PRD and [DECISIONS](DECISIONS.md) settled (2026-09-11). This session produces PRD and DECISIONS only; no SPEC, implementation slice, or implementation is authorized by this draft.

## 1. Motivation

_Decision basis: per D4, D35 and D36; ET-1–ET-2 record the accepted amendment to D34 for this feature._

A desk can already arrange work for a future time. It also needs to arrange work for when an external activity produces a result, without keeping an agent session alive to poll for it. A local research script, for example, can finish at an unpredictable time and notify the desk that its findings are ready.

MarketRig supplies durable acceptance, execution and delivery. The producer decides what happened and the agent decides what it means. Trigger invocation does not make producer data an authoritative market or trading fact.

## 2. Outcome

A trigger is the saved job: a desk-bound name, an agent-authored brief, optional context, one-off or recurring behavior, and optional approved code. A local producer invokes that trigger directly by name or ID through `marketrig`, supplying a stable request identity and optional data. The daemon accepts one firing of that trigger, preserves its input and definition, runs its optional code at most once, and queues its result through the existing structured delivery path.

For example, the agent defines `review-research`. When a local research script finishes, it invokes `review-research` with its findings. Repeating the same accepted request produces no additional work. Another distinct request can run a recurring trigger again. There is no separate event name, listener registration or match across multiple jobs. A producer needing several jobs invokes each explicitly; those invocations are independent.

Scheduled work and direct invocation share the same trigger definition (ET-7). A recurring trigger can be invoked between scheduled runs without moving its schedule. A one-off is consumed by the first accepted firing through either path, including when invocation races its scheduled deadline. A missed scheduled one-off remains terminal until explicitly rescheduled; late producer input does not revive it. Immediate conversational input remains outside triggers.

## 3. Scope

1. **Local invocation.** One desk, one trigger identified by name or ID, a stable request identity, and optional raw data enter through the CLI and authenticated daemon boundary.
2. **Existing trigger lifecycle.** Invocable definitions use the current trigger management and code-approval policies. Eligibility is evaluated at acceptance; disabled, deleted or unapproved definitions do not fire. Edits affect future firings only. A trigger may have a schedule and accept direct invocations; an extra recurring invocation leaves the schedule unchanged (ET-7).
3. **Durable acceptance.** Each accepted invocation creates one immutable firing of its target definition. One-offs are consumed atomically; recurring definitions may fire while earlier work remains queued or executing. Duplicate identities cannot refire after restart.
4. **Existing execution and delivery.** Code runs FIFO per desk through the existing executor; results are persisted before structured prompt delivery. No live agent session is required for acceptance or code execution. Delivery may activate the desk through the existing runtime path and does not interrupt an active turn.
5. **Inspectable evidence.** The producer can distinguish acceptance, duplicate submission and refusal, and discover the resulting firings. The agent and operator can trace an invocation through firing, execution, result and delivery without treating acknowledgement as execution success.
6. **Explicit failure boundaries.** Invalid submissions create no work. Execution or delivery failure never automatically retries code, replays agent input, or rearms a one-off. Late invocations are accepted on arrival subject to the normal acceptance checks; the producer owns relevance, and producer timestamps do not determine work ordering (ET-5). Backlog length does not refuse new invocations; accepted occurrences are queued individually without dropping or combining them (ET-6). A missing, disabled, consumed or unapproved target is refused with a clear reason; no work is buffered for it.

## 4. Non-goals

- Public webhooks, hosted ingress, built-in connectors or daemon-supervised producer processes.
- Price-crossing detection, payload predicates, wildcard matching, event aggregation or a strategy engine. A producer can detect its own condition and invoke a saved trigger.
- Automatic conversion of operational events, order events or realized-P&L events into trigger invocations. The existing evaluation path remains its own contract.
- A new agent runtime, delivery queue, approval policy, money-action surface or multi-agent orchestration.
- A separate event entity, event-name matching, listener subscriptions, automatic fan-out, cross-desk broadcast, attachment storage or automatic record pruning.
- Localization and packaging work; those remain separate R7 work.

## 5. Success criteria

1. One accepted invocation fires exactly its target trigger in its target desk. Other triggers, including identically named triggers in other desks, are unaffected. A recurring invocation leaves its schedule unchanged; racing schedule and invocation acceptance consumes a one-off exactly once (ET-7).
2. Repeating an identity before or after daemon restart produces no additional firing, code execution or result prompt.
3. Acceptance records immutable invocation and trigger provenance. Changing a definition afterwards cannot change accepted work.
4. Disabled and approval-pending time does not accumulate work to replay on enablement or approval. Explicitly re-enabling a consumed one-off requires a new distinct occurrence.
5. Both code-free and code-bearing firings reach the existing result and delivery path without an agent alive at submission. Failures remain inspectable and are not automatically replayed.
6. Late arrivals follow ET-5. Bursts of distinct invocations remain individually accepted while earlier work is pending, without a backlog ceiling (ET-6). Missing or ineligible targets produce explicit refusals without creating a firing.
7. Deterministic acceptance runs on macOS and Windows using local producers and isolated data roots. An attended runtime check demonstrates receipt and handling through the existing Codex and Claude delivery paths; it need not make another trade to prove direct invocation.

The later feature SPEC must turn these criteria into concrete scenarios and required checks. PRD and DECISIONS alone do not make the feature design complete.
