# A-share paper-trading engine — Feature Decisions

**Status:** Proposed design revised 2026-09-09; feasibility pending. AE-1–AE-8 are the intended feature contract, not claims of supported implementation. See [FEASIBILITY.md](FEASIBILITY.md).

Exchange and pinned-source evidence is in [RESEARCH.md](RESEARCH.md). SSE and SZSE article numbers differ; use each linked source rather than assuming identical numbering. Root D76/D78 and SPEC §12.3 must be reconciled after feasibility and before implementation; no product decision number is allocated here.

### AE-1 — One engine, with execution restrictions proven before implementation

**Decision:** NautilusTrader remains the sole execution/accounting authority. MarketRig validates venue eligibility and controls what execution can observe through supported integration points. No second matcher, wrapper engine, fork, private mutation, or post-fill correction. Session suspension, expiry, and band-safe matching are requirements, not assumed capabilities of quote suppression.

**Rationale:** A form check does not stop an already-resting order; withholding quotes does not necessarily clear cached executable liquidity. The feasibility check must demonstrate the whole path on the pinned sandbox. If a required restriction cannot be enforced, report the blocker and revise this proposal instead of silently weakening it.

**Contract:** [SPEC](SPEC.md) §1–§3, §5; [FEASIBILITY](FEASIBILITY.md).

### AE-2 — T+1 is derived eligibility checked at execution

**Decision:** Today's BUY fills determine locked shares. Current position minus locks minus outstanding SELL leaves determines sellable shares, floored at zero. Read and validate consistently on the desk node's serialized action boundary. Pending approvals reserve nothing; approved execution reruns all dynamic checks. Cash from a sell remains reusable.

**Rationale:** The exchange prohibits selling before settlement except exempt products. Existing fills provide the facts without a new settlement ledger. Shanghai-midnight unlocking is sufficient only alongside AE-7's execution gate: an unlocked weekend balance still cannot trade. The sandbox retains cash/holdings sufficiency checks.

**Contract:** [SPEC](SPEC.md) §1, §5.2.

### AE-3 — Bands constrain fills; price-at-limit is not liquidity evidence

**Decision:** Main-board bands are 10%, ChiNext 20%, using a verified current-day reference and exchange tick rounding. Out-of-band LIMIT submissions fail. At the upper limit no BUY fills occur; at the lower limit no SELL fills occur. Name this a conservative simulation policy. The observation reports price conditions `AT_UPPER_LIMIT` / `AT_LOWER_LIMIT`, not an empty queue. HiThink without a usable band blocks execution. Yahoo explicitly lacks these restrictions.

**Rationale:** Snapshot prices do not reveal counterparties. Zero-size quote shaping is a candidate mechanism requiring proof for active and resting orders, not an exchange fact. Market rejection by the sandbox is not the exchange's automatic cancellation. Current-day and ex-dividend reference attribution are unresolved provider evidence, and receipt time alone is insufficient.

`ponytail:` conservative no-fill-at-limit policy omits possible real fills; upgrade to observed bid/ask and queue-aware simulation only when the feed and experiment need it.

**Contract:** [SPEC](SPEC.md) §2; [RESEARCH](RESEARCH.md).

### AE-4 — Quantity rules follow the supported board and order type

**Decision:** Main-board and ChiNext BUY quantities are positive multiples of 100. SELL quantities consume whole lots or the whole available odd remainder, subject to sellability. Main-board cap: 1,000,000; ChiNext LIMIT: 300,000; ChiNext MARKET: 150,000. STAR and Beijing are unsupported.

**Rationale:** SSE's clarification allows whole lots plus the entire odd remainder while retaining other whole lots. SZSE specifies separate ChiNext caps. A single CN cap and the old refusal of 150 from 250 were wrong.

**Contract:** [SPEC](SPEC.md) §3.

### AE-5 — Fees remain an explicitly approximate sandbox rate

**Decision:** Keep 3 bp per side through the sandbox's MakerTakerFeeModel. Do not label this the actual fee schedule or compute corrections in MarketRig.

**Rationale:** The pinned execution crate already has FeeModelHandle for custom models, but the selected sandbox configuration accepts only FeeModelAny built-ins. The missing public seam is sandbox configuration. Custom backtest support does not prove sandbox support.

`ponytail:` symmetric fees omit actual side-specific charges and brokerage minimums; upgrade when a verified supported sandbox configuration accepts a custom fee model, with dedicated accounting checks.

**Contract:** [SPEC](SPEC.md) §3.3; [RESEARCH](RESEARCH.md).

### AE-6 — Limitations remain visible and desk ownership stays intact

**Decision:** Seed new desks with the supported-session boundary, day lifetime, T+1, quantities, HiThink band requirements, and conservative fill policy. Name Yahoo simplification, approximate fees, unmodeled halts/auctions/corporate actions, and synthesized liquidity. Never rewrite existing desks' AGENTS.md. Runtime resources expose the active restrictions even for existing desks.

**Rationale:** A static constitution cannot substitute for current execution availability. Existing agent ownership remains per D20.

**Contract:** [SPEC](SPEC.md) §4.

### AE-7 — Continuous sessions and simulator day orders

**Decision:** CN execution is allowed only during [09:30,11:30) and [13:00,14:57) Asia/Shanghai on a confirmed trading day. New submissions outside those windows fail; resting orders cannot fill there. Orders survive lunch but expire at 14:57, even without a quote. Prior-day orders terminate before restart replay. Cancellation remains available while execution is blocked.

**Rationale:** The closing auction is deliberately unsupported, so the simulator ends its day at 14:57. This earlier expiry is a stated approximation. New-order checks alone do not enforce session correctness. The HiThink weekday fallback can label awareness but does not authorize execution without a confirmed exchange date.

**Contract:** [SPEC](SPEC.md) §5.1, §5.3.

### AE-8 — Missing execution prerequisites fail closed; acceptance closes the loop

**Decision:** Missing current-day reference, uncertain calendar, feed failure, or node failure blocks affected CN execution, including resting fills. Reopening requires readiness and removal of stale executable state. Keep Yahoo's explicitly selected simplified mode distinguishable, without automatic fallback. Deterministic tests advance time through the actual integration; full attended E7 requires a next-session close.

**Rationale:** Silently disabling enforcement defeats the feature's promise. Provider evidence and the sandbox clock/lifecycle seam must be resolved before calling the feature design complete.

**Contract:** [SPEC](SPEC.md) §2.1, §5–§6; [FEASIBILITY](FEASIBILITY.md).
