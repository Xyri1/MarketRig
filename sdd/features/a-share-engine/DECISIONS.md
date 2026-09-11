# A-share paper-trading engine — Feature Decisions

**Status:** Product choices settled 2026-09-10 under the user’s delegated judgment, with AE-10 added 2026-09-11 from slice 014's E7 procedure. AE-1–AE-10 define the completed feature design; F8 establishes native feasibility on macOS with the MARKET exception. [Slice 014](../../slices/014-a-share-engine.md) owns production integration and remaining verification. See [FEASIBILITY.md](FEASIBILITY.md).

Exchange and pinned-source evidence is in [RESEARCH.md](RESEARCH.md). SSE and SZSE article numbers differ; use each linked source rather than assuming identical numbering. AE-1–AE-10 record amendments to root D76/D78/D84 and SPEC §12.3 for slice 014. Root delivered contracts are merged back only after slice exit; no product decision number is allocated here.

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

**Rationale:** Snapshot prices do not reveal counterparties. F4/F8 establish zero-size idle books and temporary crossing publications as supported native mechanisms, not exchange facts. Market rejection by the sandbox is not the exchange's automatic cancellation. Current-day snapshot/reference attribution is accepted as an inference; ex-dividend reference behavior is observed but undocumented. Receipt time alone is insufficient evidence of source age.

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

**Decision:** A positive provider reference, today’s per-instrument daily bar, a calendar containing today, and a usable snapshot establish HiThink simulation readiness. Accept snapshot/reference date attribution as inference and source delay as unknown; neither equality with a bar nor volume movement certifies freshness. Missing prerequisites, feed failure, or node failure blocks affected CN execution, including resting fills. A confirmed calendar remains valid for that Shanghai day despite a later request refusal, but rollover invalidates it. Auction status is never a session/halts gate; halts remain unsupported. Reopening requires readiness and removal of stale executable state. Keep Yahoo's explicitly selected simplified mode distinguishable, without automatic fallback. Deterministic tests advance time through the actual integration; the attended E7 reaches its next-session close inside one sitting by staging its own trading day through the same seam (AE-10).

**Rationale:** R2 establishes usable sampled data, not guaranteed source freshness or reference semantics. These are accepted paper-simulation ceilings, including possible stale successful responses and undocumented reference behavior. F1–F6 and R1 establish native lifecycle seams; F8 establishes the chosen fill model with AE-9’s MARKET exception; production integration and cross-platform verification remain.

**Contract:** [SPEC](SPEC.md) §2.1, §5–§6; [FEASIBILITY](FEASIBILITY.md).

### AE-9 — Sampled-price limit execution and immediate synthetic market execution

**Decision:** For HiThink CN, except for the MARKET publication rule below, a LIMIT waits for a subsequent accepted snapshot with increased cumulative volume relative to the preceding accepted snapshot and last price compatible with its limit (BUY <= limit, SELL >= limit). The observation must also follow the order's execution-time admission baseline. Fill the whole remaining quantity at the order's limit price, without price improvement or queue/volume allocation. Native cash/holdings checks still apply. MARKET requests fill immediately at the latest usable snapshot last price, for the full requested quantity, subject to session, readiness, band, and direction restrictions; they do not wait for a future trade. A MARKET's observation is itself a qualifying observation for every compatible resting LIMIT on that instrument, regardless of volume: those LIMITs fill first at their own limit price, then the MARKET fills at last (settled 2026-09-10 from F8 item 3; the pinned engine iterates resting orders on the publish a MARKET needs, and no seam separates them). No synthetic spread or slippage is added in this HiThink model. NautilusTrader alone emits fills and accounting; F8 proved both policies with supported seams.

**Rationale:** The feed offers snapshots, not timestamped individual transactions. This is an explicit approximation of a subsequent compatible trade, never proof of one. Limit-price execution is conservative on price; full quantity is deliberately optimistic on liquidity. Market orders express immediate execution, so keeping them waiting for volume would misrepresent their intent. The policies differ visibly. Yahoo retains its explicitly simplified native quote matching, not a claim to this HiThink model.

`ponytail:` retain batched polling at 10 seconds while exposed and 30 seconds while idle, per desk node. Crossings between polls are missed; no inferred intrapoll fills, faster polling, or tick storage. No numeric provider quota is established. Existing bounded backoff handles HTTP 429 and must also recognize envelope 429/4001 as rate limiting (F8 found envelope-only 429 outside the retryable set; the slice adds it); exhaustion blocks affected execution. Revisit shared polling only if multi-desk request load requires it.

**Contract:** [SPEC](SPEC.md) §2.1–§2.5, §5.3, §6; [FEASIBILITY](FEASIBILITY.md) F8.

### AE-10 — The attended cell stages both its trading days, backwards

**Decision:** E7 completes in one sitting, at any wall-clock hour — after the Shanghai close, on a weekend, on a holiday. Its daemon seeds every node at 10:00 Asia/Shanghai on the buy day through §6's controlled-clock seam, and `PUT /test/clock` moves them to 10:00 on the sell day once the buy has filled and the same-day sell has been refused; the same session then sells, the cycle closes, and evaluation is queued. The sell day is the most recent weekday whose 09:30 Shanghai has passed, so the provider already holds its own bar, and the buy day is the weekday before it. Only the trading dates are staged: the provider's calendar, each day's bar and every snapshot are the real service's on both legs. The seam lifts the A-share cadence gate the way the HiThink stand-in does, so a staged node keeps polling the real service while the wall-clock market is closed and the moved-on day re-establishes on the next cycle. Outside real Shanghai hours the snapshot is the day's last and does not move, so both legs are MARKET orders; a LIMIT waits for volume growth only a trading market produces. Both staged days step over the weekend; a holiday leaves the instrument `UNAVAILABLE`, which the cell prints and records rather than driving a sitting that cannot buy. The realized figure is recorded as a harness artifact beside the unknown source delay and the unadjusted corporate action.

**Rationale:** The real-calendar rollover is gate A1's evidence and needs no second, attended proof; E7's own evidence is the real provider, the real research passthrough and the real key handling, and a second day adds to none of them. Staging forward is not available: AE-8 admits CN execution only against a current-day bar dated the node's own Shanghai day, and no provider publishes tomorrow's bar, so a node moved into the future reads `DATE_UNPROVEN` and nothing can be sold. Staging backwards holds because `prices/historical` truncates at the window's `end` ([F7-EVIDENCE](F7-EVIDENCE.md) §2.2), so a past day's own bar is the newest one the query returns, and the selling leg is then entirely real. A two-day attended protocol costs the operator a second sitting and a second console for a rollover that is already proven deterministically. Any-hour costs one boolean in the daemon: the `CN` cadence gate reads the wall clock, so without lifting it under the seam a staged node polls nothing after the real close, the moved-on day never re-establishes, and the cell strands on a `NO_CALENDAR` readiness it can no longer clear. Confining the sitting to a real session instead would cost every operator a scheduling constraint the evidence never needed.

`ponytail:` both staged days are *weekdays*, not trading days: the harness does not read the exchange calendar itself, and a holiday is reported by the desk's own readiness view instead. Upgrade to walking the provider's list only if holiday sittings become common.

**Contract:** [SPEC](SPEC.md) §5.1, §6 (the seam paragraph and the E7 paragraph); [`hithink-a-share` SPEC](../hithink-a-share/SPEC.md) §1.3, §3 (the cadence gate the seam lifts); `crates/marketrig-acceptance/EXPERIMENT.md` §8.
