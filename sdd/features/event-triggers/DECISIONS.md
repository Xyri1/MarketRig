# Trigger invocation — Feature Decisions

**Status:** Product decisions settled (2026-09-11); SPEC and implementation remain deferred. The operator accepted direct trigger invocation in place of named-event matching, along with late-arrival handling and no backlog ceiling. ET-7 also settles shared schedule/invocation behavior. This feature records an explicit amendment to root D34 and SPEC §8.1–§8.3; their current EVENT wording is not the target design. Root reconciliation must also update the PRD, ROADMAP and other current references before implementation. Frozen historical slices remain unchanged. No new product D number is allocated here.

## Settled decisions

### ET-1 — Invoke a saved trigger directly

**Decision:** A trigger is the single user-facing job definition. A local producer invokes one trigger by name or ID in one desk through `marketrig` and the authenticated daemon boundary, with a stable caller-supplied request identity and optional raw input. There is no separate event-name namespace, listener matching or automatic fan-out. Invoking several triggers takes independent requests. MarketRig owns no producer monitoring lifecycle and adds no public webhook or connector service. Immediate conversational input remains outside this system.

**Rationale:** Accepted by the operator on 2026-09-11: the intended product is durable jobs for an agent. Naming the job directly removes the need to route an incoming event to listeners or retain an event that has no listener. This amends D34's named-event matching and one-to-many occurrence contract, while keeping D4's CLI ownership.

**Contract:** [PRD §2](PRD.md#2-outcome), [PRD §3](PRD.md#3-scope), [PRD §4](PRD.md#4-non-goals). Intended root amendment: D34 and SPEC §8.1–§8.3.

### ET-2 — One accepted invocation creates one immutable firing

**Decision:** A distinct accepted invocation fires its targeted enabled, undeleted, authorized trigger exactly once. Acceptance captures the input, current brief, context, revision and approved code identity. ONE_OFF consumption is atomic with the firing; RECURRING permits subsequent distinct requests while previous work remains queued or executing. Repeating an accepted request creates no new firing, including after restart. Editing or disabling a definition cannot cancel accepted work. Explicitly re-enabling a consumed one-off never makes an already accepted request executable again. ET-7 governs interaction with scheduled occurrences.

**Rationale:** The durable firing remains the unit of execution and result history. Request identity prevents duplicate work without introducing a separate event product entity. This replaces D34's fan-out with direct targeting while preserving its atomicity and provenance.

**Contract:** [PRD §3](PRD.md#3-scope), [PRD §5](PRD.md#5-success-criteria). Intended root amendment: D34 and SPEC §8.

### ET-3 — Existing approval, execution and delivery semantics apply

**Decision:** Code-free definitions need no code approval. Code-bearing definitions follow Always allow or Require approval, and only approved snapshots are executable. Invocations do not approve code. Accepted code executions remain FIFO per desk and at most once; each terminal outcome produces its durable result and queued prompt through the existing pipeline. Delivery uses the existing activation and structured queue behavior without interrupting an active turn. Uncertain execution or delivery is not automatically retried.

**Rationale:** Per D35, D36 and D70, changing how a firing originates does not justify changing its execution authority or delivery guarantees. Duplicate-safe ingress is distinct from retrying code or agent input.

**Contract:** [Root SPEC §8.3](../../SPEC.md#83-trigger-lifecycle), [§9](../../SPEC.md#9-trigger-code-execution), [§11](../../SPEC.md#11-daemon-prompt-delivery); [PRD §3](PRD.md#3-scope).

### ET-4 — Refuse an invocation with no eligible target

**Decision:** A new invocation targeting a missing, deleted, disabled, consumed or unapproved trigger is refused with a clear reason and creates no firing. Nothing is buffered to run on later enablement or approval. There is no zero-match event record. Duplicate handling must preserve the result of an already accepted request even if its trigger subsequently becomes ineligible; exact refusal/reuse bookkeeping belongs in the SPEC.

**Rationale:** Direct invocation names the work requested, so an unavailable target is a refusal rather than an event with zero listeners. This resolves the former Q3 by removing event matching. Existing disablement and approval boundaries remain intact.

**Contract:** [PRD §3](PRD.md#3-scope), [PRD §5](PRD.md#5-success-criteria); [root SPEC §8.3](../../SPEC.md#83-trigger-lifecycle).

### ET-5 — Accept late invocations on arrival; the producer owns relevance

**Decision:** Input age alone does not prevent acceptance. The producer decides whether an occurrence remains relevant before submitting it. Producer timestamps are provenance, not scheduled deadlines; MarketRig adds no invocation expiry or catch-up service. Submission remains subject to validation, deduplication, eligibility and durable persistence. Acceptance order governs work ordering, not the producer's timestamp.

**Rationale:** Accepted by the operator on 2026-09-11. A delayed research report may remain useful while a delayed price alert may not. MarketRig cannot infer relevance from the trigger name. Scheduled lateness rules concern daemon-owned deadlines and do not apply to external invocation input.

**Contract:** [PRD §3](PRD.md#3-scope), [PRD §5](PRD.md#5-success-criteria); [root SPEC §8.2](../../SPEC.md#82-event-occurrence-semantics).

### ET-6 — Queue every accepted occurrence without a backlog ceiling

**Decision:** Pending-work volume does not refuse an otherwise valid distinct invocation. MarketRig imposes no per-desk backlog ceiling, drops no accepted occurrence and does not combine invocations. Each accepted invocation gets its own firing of the target trigger, and work follows the existing durable execution and prompt queues. Code remains FIFO-serialized per desk and delivery retains its existing runtime gating. Individual input validation and payload bounds still apply; persistence failure must not be reported as successful acceptance.

**Rationale:** The operator chose to let the agent handle accumulated work rather than have the harness restrict intake based on backlog. This is a queueing policy, not a throughput guarantee or a change to runtime concurrency.

`ponytail:` no backlog-based admission control; durable pending work can grow while processing lags. Revisit admission control only if observed backlog warrants a new product decision.

**Contract:** [PRD §3](PRD.md#3-scope), [PRD §5](PRD.md#5-success-criteria); [root SPEC §8.4](../../SPEC.md#84-concurrency), [§11](../../SPEC.md#11-daemon-prompt-delivery).

### ET-7 — A trigger can have a schedule and accept direct invocations

**Decision:** Direct invocation is available for an eligible trigger whether or not it has a schedule. An additional invocation of a recurring trigger leaves its schedule unchanged. A one-off is consumed by its first accepted firing through either entry path: schedule acceptance and invocation acceptance racing each other must not create two firings. The losing new invocation is refused if the schedule consumed the trigger first; if invocation won, no later scheduled firing is created. Repeating the accepted invocation remains duplicate-safe under ET-2. Existing scheduled lateness and miss semantics remain unchanged: a missed scheduled one-off is terminal and a new direct invocation is refused until it is explicitly rescheduled under the existing lifecycle. Direct invocation cannot bypass a passed deadline before miss reconciliation. ET-5 permits late producer input only when its target remains eligible; it does not revive a missed job.

**Rationale:** Accepted by the operator on 2026-09-11. The agent can define a recurring research review once and invoke it when additional findings arrive, without duplicating its brief or code. A one-off still means one accepted unit of work regardless of its entry path.

**Contract:** [PRD §2](PRD.md#2-outcome), [PRD §3](PRD.md#3-scope), [PRD §5](PRD.md#5-success-criteria). Intended root amendment: D34 and SPEC §8; scheduled miss behavior remains per D37 and SPEC §10.

## Mechanics reserved for the SPEC session

These are not additional product-scope commitments. The later SPEC must resolve them within the chosen policies:

- Exact CLI grammar and request/response framing; desk/trigger request-identity scope, caller identity requirements and same-identity/different-content conflict behavior.
- Payload representation and byte limits; immutable input available to code and retrievable by the agent, with prompt size bounded and payload never treated as executable source.
- Atomic acceptance and duplicate storage, including concurrent submissions, trigger edits and restart; response/read paths for original acceptance and refusal outcomes.
- Acceptance remains independent of backlog length (ET-6); check bursts of distinct invocations while earlier work is pending without introducing a capacity-accounting subsystem.
- Integration with firing stdin, result prompts, existing history and desktop trigger inspection; no speculative new dashboard.
- Deterministic checks and the smallest attended invocation-delivery scenario. No library choice, schema, route, limit or test result is asserted by these product documents.

Retention/pruning, public ingress, connectors and richer filtering remain deferred under [root SPEC §18](../../SPEC.md#18-implementation-deferred-contracts).
