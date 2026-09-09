# A-share engine research — 2026-09-09

Research snapshot collected before the 2026-09-09 design revision. References to defects describe the original draft; the revised PRD, DECISIONS, and SPEC now address them. This file preserves the evidence, not the current contract. Sources were read online; pinned Rust source was also inspected locally. No service credentials, trading actions, or runtime tests were used. Outstanding execution/provider work is logged in [FEASIBILITY.md](FEASIBILITY.md).

## Exchange findings

| Topic                   | Finding and consequence                                                                                                                                                                                                                                                                                                                                                                            |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Odd lots                | SSE explicitly allows a 299-share holding to sell as 199 plus 100, or 99 plus 200. Applying that rule, 250 → sell 150 is valid. AE-4's modulo expression agrees; SPEC §3.1 and the PRD examples are wrong. This application assumes unreserved, sellable holdings. [SSE clarification, 2014-06-27](https://www.sse.com.cn/lawandrules/guide/stock/jyglywznylc/tz/c/c_20230209_5716007.shtml).      |
| Quantity caps           | SZSE §3.3.9: general cap 1,000,000; ChiNext LIMIT 300,000 and MARKET 150,000. The universal cap is wrong for catalogued 300750. [2026 rules](https://docs.static.szse.cn/www/lawrules/rule/trade/current/W020260424690713155663.pdf).                                                                                                                                                              |
| Settlement and sessions | SZSE §3.1.4 prohibits selling before settlement except turnaround products. §§2.3, 3.3.5 restrict sessions and market orders; §3.3.21 makes orders day-valid. Calendar-midnight unlocking with phase-independent GTC execution is therefore a simulator approximation. [Rules](https://docs.static.szse.cn/www/lawrules/rule/trade/current/W020260424690713155663.pdf).                            |
| Empty opposite side     | SZSE §3.3.6 automatically cancels most market types without opposing orders. §3.4.4 makes a crossing buy execute at the ask. Inference: touching a price limit does not establish absent liquidity; nor does cancellation mean rejection. [Rules](https://docs.static.szse.cn/www/lawrules/rule/trade/current/W020260424690713155663.pdf).                                                         |
| Daily bands             | Main-board 10%, ChiNext 20%, with tick rounding and exceptions (§§3.3.13–19). The feature correctly reflects main-board risk-warning stocks moving from 5% to 10% in July 2026. [SZSE rules](https://docs.static.szse.cn/www/lawrules/rule/trade/current/W020260424690713155663.pdf), [SSE revision explanation](https://star.sse.com.cn/aboutus/mediacenter/hotandd/c/c_20260424_10816474.shtml). |
| Effective text          | Both 2026 revisions took effect July 6. SSE's notice also retains deferred clauses: a rule's appearance in the document alone is not evidence every clause operates. [SSE notice and attachments](https://www.sse.com.cn/lawandrules/sselawsrules2025/stocks/exchange/c/c_20260424_10816482.shtml), [SZSE notice](https://investor.szse.cn/lawrules/rule/trade/t20260424_620190.html).             |

## Provider limits

HiThink's snapshot contract includes `prev_price`, but no bid/ask sizes or order queue. Explicit `thscodes` batching returns a null timestamp. The contract calls `prev_price` previous close without explicitly resolving ex-dividend reference semantics. A separate corporate-actions endpoint supplies dividends and bonus shares. [Official contract at commit 44b7aa3](https://github.com/HiThink-Tech/Financial-API/blob/44b7aa34dd504675f3ddaa15b3d478ea16f97884/docs/api/endpoints-prices.md).

SSE §§4.3.2–3 require the ex-rights/ex-dividend reference as the displayed previous close and price-band basis. [SSE rules attachment](https://www.sse.com.cn/lawandrules/sselawsrules2025/stocks/exchange/c/c_20260424_10816482.shtml).

Consequences and open evidence:

- `last == limit_up` supports a price-at-limit label, not a claim that the ask queue is empty. Zeroing that side must be described as a conservative fill assumption.
- Verify `prev_price` on an actual ex-dividend date before calling the derived band authoritative.
- Define current-day band readiness. A received-at time does not by itself establish which trading day a null-timestamp snapshot describes.
- Missing `prev_price` currently disables enforcement. Decide whether this is acceptable degraded simulation or should block affected orders.
- Corporate-action accounting remains a separate gap for positions held across dates; deriving a band does not credit dividends or bonus shares.

## Fees and the actual Rust seam

SSE's investor service currently lists seller-only stamp duty at 0.05%, transfer fees at 0.001% each side, and brokerage commission separately. Its listed trading regulatory charges are included in brokerage commission, so a future model must avoid double counting. [SSE investor service](https://one.sse.com.cn/onething/gptz/). The tax reduction took effect August 28, 2023. [Finance Ministry notice](https://www.mof.gov.cn/jrttts/202308/t20230828_3904235.htm).

The feature's 3 bp each side is a simulator rate, not a faithful fee schedule. Preserve that distinction.

Pinned `nautilus-execution 0.62.0` already contains `FeeModelHandle(Rc<dyn FeeModel>)`, accepting downstream implementations. Its `FeeModelAny` remains six built-in variants. `nautilus-sandbox 0.62.0` config exposes `Option<FeeModelAny>` and internally converts it into the handle when creating matching engines. Thus AE-5's deferral holds for the selected sandbox configuration; the missing seam is in sandbox configuration, not the entire matching engine. [Fee source](https://docs.rs/crate/nautilus-execution/0.62.0/source/src/models/fee.rs), [sandbox config](https://docs.rs/crate/nautilus-sandbox/0.62.0/source/src/config.rs), [sandbox execution](https://docs.rs/crate/nautilus-sandbox/0.62.0/source/src/execution.rs).

Context7 returned current development documentation, not a 0.62.0-specific index; it was not treated as version proof. Online source and the installed pinned crate agree. Upstream discussions distinguish backtest custom models from sandbox support; a closed issue does not establish release support. [Upstream issue 4806](https://github.com/nautechsystems/nautilus_trader/issues/4806).

## Matching and acceptance consequences

Pinned source confirms zero-size L1 additions clear a ladder, and market orders reject when the engine's opposing price is absent. That supports the mechanism, pending an end-to-end check of QuoteTick → book → matching-core propagation. [Ladder source](https://docs.rs/crate/nautilus-model/0.62.0/source/src/orderbook/ladder.rs), [matching source](https://docs.rs/crate/nautilus-execution/0.62.0/source/src/matching_engine/mod.rs).

The matching source also slips remaining L1 market quantity by one tick after exhausting displayed volume. Band correctness therefore needs fill-level checks, not only order-price validation. No runtime result is claimed here.

The following are deductions from the proposed SPEC and root contracts:

1. A2's buy limit of 1856.80 against an ask of 1700 crosses; change the expected outcome or use a non-crossing order.
2. The PRD says market orders at struck limits rest, while SPEC §2.4 says they reject. Pick the intended simulator behavior and state its difference from exchange cancellation.
3. Revalidate sellability and the current band at approval/execution time; the root pending-approval path currently promises not to consult the node.
4. Serialize sell reservation checks with submission, using mutually consistent persisted fills and node state; cover partial fills, cancellation, and restart.
5. SQLite's native `SUM` over decimal text is not an exact-decimal guarantee. Sum parsed decimal values or use explicitly checked integer share arithmetic.
6. Define whether live sellability belongs only on current positions. Adding read-time sellability to historical closed positions needs a separate meaning.
7. A T+1 refusal is useful evidence but does not complete the round trip. The optional next-day E7 sitting leaves full cycle/evaluation acceptance outstanding until actually performed.

Recommended discussion order: correct odd lots/caps/A2; settle session and order-expiry behavior; define the conservative limit-fill assumption and missing-band behavior; then finalize acceptance. No new engine abstraction is justified by this research alone.
