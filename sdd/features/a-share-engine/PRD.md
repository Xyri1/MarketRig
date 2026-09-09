# A-share paper-trading engine — Feature PRD

**Slice:** none yet  
**Status:** Design revised 2026-09-09 — feasibility pending; not implementation-ready  
**Next work:** [FEASIBILITY.md](FEASIBILITY.md), recorded for Claude; not yet performed.

## 1. Motivation

_Decision basis: per D4, D20, D38, D75, D76, D78, D84; proposed amendments AE-1–AE-8._

An A-share desk should not learn from a same-day round trip in shares bought that day, a fill outside its supported session, or a fill beyond the daily band. MarketRig keeps NautilusTrader as the sole producer of fills, fees, balances, and realized P&L, and enforces the supported venue restrictions around it.

This is a proposal to narrow D76's physics gaps and amend D78's form-only, phase-independent GTC contract for CN. It also refines the approval execution boundary in root SPEC §12.3. Those root contracts remain the delivered truth until feasibility passes and the changes are reconciled before implementation. US and Hong Kong are unchanged. Evidence and source links are in [RESEARCH.md](RESEARCH.md).

## 2. Outcome

The supported CN catalog trades only in continuous sessions on exchange trading days. Positions expose today's locked shares and unreserved sellable shares. Orders last for the supported trading day. HiThink execution requires a usable current-day band, and fills stay inside it. At a daily limit, the simulator conservatively forbids fills toward the limit; this is an explicit assumption, not an observed empty order queue.

## 3. Scope

1. T+1 sell eligibility from existing fills, with outstanding sells reserved and validation serialized with submission. Sale proceeds remain reusable; cash withdrawal settlement is not modeled.
2. Continuous-session execution: 09:30–11:30 and 13:00–14:57 Asia/Shanghai, on confirmed exchange trading days. Both new submissions and resting fills obey the session. CN orders expire at 14:57, the end of this simulator's supported day, rather than entering the unmodeled closing auction.
3. Daily price bands on HiThink, with current-day provenance and reference validity required before submission or matching. Missing or uncertain bands block execution instead of disabling checks.
4. Correct whole-lot buys, whole odd-remainder sells, and board/type-specific caps for main-board and ChiNext instruments.
5. Approval-time revalidation, restart ordering, and safe suspension across unavailable data or provider changes.
6. Honest agent-visible limitations, seeded only for new desks; existing agent-owned constitutions remain untouched.
7. Deterministic next-session, expiry, and recovery evidence, followed by an attended next-trading-day close and queued evaluation.

## 4. Non-goals and deliberate approximations

- No second execution engine, fork, patched crates, or rewriting Nautilus-produced facts.
- No auctions, after-hours fixed-price trading, halts/suspensions, margin, or securities lending. Being inside the supported session does not establish that an individual instrument is not halted.
- No real spread, order queue, depth, or fill probability. Blocking buys at the upper limit and sells at the lower limit is conservative snapshot-based simulation.
- Fees remain 3 bp per side, not the actual stamp-duty/commission schedule. Corporate-action cash/share accounting is not implemented; its impact on overnight P&L remains disclosed.
- No IPO/unlimited-band or delisting-period instruments, ETFs, STAR, or Beijing instruments. Adding a board with different quantity rules requires extending the contract and checks, not just a catalog row.
- No dynamic 2%/ten-tick order-price cage without a suitable reference feed.
- Yahoo CN remains explicitly simplified: T+1, sessions, quantities, and day lifetime still apply, but bands and limit-fill restrictions are unavailable. It must never be presented as the HiThink rules-enforced mode or substituted automatically.

## 5. Success criteria

- A same-day sell of newly bought shares is refused; a next-trading-session sell succeeds and produces the authoritative cycle and evaluation. Friday-to-weekend and holiday boundaries never permit an out-of-session fill.
- A crossing limit order fills at the sandbox's price; a non-crossing one rests. All fills under an active band remain within it, including market remainders and restored orders.
- HiThink data loss or an unverified current-day reference blocks new submissions and resting fills; recovery cannot match against a stale cached book.
- A sellable 250-share balance permits 50, 100, 150, 200, and 250, but not 125. Main-board and ChiNext caps differ as specified.
- Day orders expire without a new quote and cannot execute after restart on a later day. Reservations release through authoritative terminal order events.
- Pending approvals reserve nothing and rerun execution checks when approved; competing sells cannot reserve the same shares.
- Feasibility checks pass before implementation planning. An attended same-day refusal alone is partial evidence; full E7 completion requires the later close.
