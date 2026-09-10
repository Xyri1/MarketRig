# A-share paper-trading engine — Feature Decisions

**Status:** Product choices settled 2026-09-10 under the user’s delegated judgment. AE-1–AE-9 define the revised feature contract; F8 must prove its new fill mechanism before implementation. See [FEASIBILITY.md](FEASIBILITY.md).

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

**Decision:** Main-board bands are 10%, ChiNext 20%, using the provider reference under AE-8’s disclosed attribution assumption and exchange tick rounding. Out-of-band LIMIT submissions fail. When the observed last price equals the upper limit no BUY fills occur; when it equals the lower limit no SELL fills occur. A fill priced at a boundary may occur when the triggering observation is inside the band. Name this a conservative simulation policy. The observation reports price conditions `AT_UPPER_LIMIT` / `AT_LOWER_LIMIT`, not an empty queue. HiThink without a usable band blocks execution. Yahoo explicitly lacks these restrictions.

**Rationale:** Snapshot prices do not reveal counterparties. Zero-size quote shaping is a candidate mechanism requiring proof for active and resting orders, not an exchange fact. Market rejection by the sandbox is not the exchange's automatic cancellation. Current-day snapshot/reference attribution is accepted as an inference; ex-dividend reference behavior is observed but undocumented. Receipt time alone is insufficient evidence of source age.

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

**Decision:** CN execution is allowed only during [09:30,11:30) and [13:00,14:57) Asia/Shanghai on a confirmed trading day. New submissions outside those windows fail; resting orders cannot fill there. Orders survive lunch. Native TIF is GTC; a clock-driven CancelOrder at 14:57 produces OrderCanceled, even without a quote. Prior-day orders terminate before restart replay. Cancellation remains available while execution is blocked.

**Rationale:** The closing auction is deliberately unsupported, so the simulator ends its day at 14:57. This earlier expiry is a stated approximation. New-order checks alone do not enforce session correctness. The HiThink weekday fallback can label awareness but does not authorize execution without a confirmed exchange date.

**Contract:** [SPEC](SPEC.md) §5.1, §5.3.

### AE-8 — Missing execution prerequisites fail closed; acceptance closes the loop

**Decision:** A positive provider reference, today’s per-instrument daily bar, a calendar containing today, and a usable snapshot establish HiThink simulation readiness. Accept snapshot/reference date attribution as inference and source delay as unknown; neither equality with a bar nor volume movement certifies freshness. Missing prerequisites, feed failure, or node failure blocks affected CN execution, including resting fills. A confirmed calendar remains valid for that Shanghai day despite a later request refusal, but rollover invalidates it. Auction status is never a session/halts gate; halts remain unsupported. Reopening requires readiness and removal of stale executable state. Keep Yahoo's explicitly selected simplified mode distinguishable, without automatic fallback. Deterministic tests advance time through the actual integration; full attended E7 requires a next-session close.

**Rationale:** R2 establishes usable sampled data, not guaranteed source freshness or reference semantics. These are accepted paper-simulation ceilings, including possible stale successful responses and undocumented reference behavior. F1–F6 and R1 establish native lifecycle seams; F8 must establish the newly chosen fill model.

**Contract:** [SPEC](SPEC.md) §2.1, §5–§6; [FEASIBILITY](FEASIBILITY.md).

### AE-9 — Sampled-price limit execution and immediate synthetic market execution

**Decision:** For HiThink CN, a LIMIT waits for a subsequent accepted snapshot with increased cumulative volume relative to the preceding accepted snapshot and last price compatible with its limit (BUY <= limit, SELL >= limit). The observation must also follow the order's execution-time admission baseline. Fill the whole remaining quantity at the order's limit price, without price improvement or queue/volume allocation. Native cash/holdings checks still apply. MARKET requests fill immediately at the latest usable snapshot last price, for the full requested quantity, subject to session, readiness, band, and direction restrictions; they do not wait for a future trade. No synthetic spread or slippage is added in this HiThink model. NautilusTrader alone emits fills and accounting; F8 must prove both policies with supported seams.

**Rationale:** The feed offers snapshots, not timestamped individual transactions. This is an explicit approximation of a subsequent compatible trade, never proof of one. Limit-price execution is conservative on price; full quantity is deliberately optimistic on liquidity. Market orders express immediate execution, so keeping them waiting for volume would misrepresent their intent. The policies differ visibly. Yahoo retains its explicitly simplified native quote matching, not a claim to this HiThink model.

`ponytail:` retain batched polling at 10 seconds while exposed and 30 seconds while idle, per desk node. Crossings between polls are missed; no inferred intrapoll fills, faster polling, or tick storage. No numeric provider quota is established. Existing bounded backoff handles HTTP 429 and must also recognize envelope 429/4001; exhaustion blocks affected execution. Revisit shared polling only if multi-desk request load requires it.

**Contract:** [SPEC](SPEC.md) §2.1–§2.5, §5.3, §6; [FEASIBILITY](FEASIBILITY.md) F8.
