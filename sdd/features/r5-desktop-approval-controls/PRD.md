# R5 — Desktop and approval controls: Feature PRD

**Milestone:** [R5](../../ROADMAP.md#milestone-r5--desktop-and-approval-controls)
**Status:** Design complete — PRD, DECISIONS, and SPEC written 2026-09-04; implementation not started

This feature designs Milestone R5: the user's control plane over desks that are already running, and the approval boundary the daemon has carried as a fixed **Always allow** since R1 and R2. It refines `sdd/SPEC.md` §4.3, §4.4, §6.5, §8.3, §11.2, §12.3, §13.2, §14, §15, and §17 and invents nothing beyond them.

## 1. Motivation

*Decision basis: per D10, D26, D29, D30, D33, D52, D55–D62, D66, D70, D71, D72.*

R0–R4 proved the loop headless: a real agent trades, is woken by its own triggers, evaluates a closed cycle, and retains a lesson a later session recalls. Everything the user can do about it today is a `curl` against the loopback API. There is no window on the desk's terminal, no view of its history, and no way to say *ask me first*: both approval policies are fixed at **Always allow**, so the user who wants to supervise trigger code that runs as their own account, or paper orders during an experiment, has nothing to turn. R5 is where the Tauri shell, the Vue frontend, and the two approval policies land together, because an approval boundary is worth little until something can present the prompt and a desktop is worth little without the boundary it exists to serve.

## 2. Outcome

A user opens MarketRig. The shell finds or starts `marketrigd`, the window shows the desks on the left, the selected desk's real terminal in the center, and its market state, triggers, approvals, activity, and settings on the right. They close the window; the terminal keeps running under the daemon and the presentation stays warm in the tray. They reopen from the tray or by launching again, and the same screen is there with the bytes that arrived meanwhile. They set paper orders to **Require approval**. The agent submits an order; it commits as pending, answers the agent with `approval: PENDING` and no order, appears in the Approvals tab and in a notification, and reaches the sandbox only when the user approves it; a denied one is terminal and records nothing in the book. A trigger with code created under **Require approval** is never due until approved; a denied one never fires. Quit stops every managed process and the daemon, then the shell exits. The packaged application does all of this from a freshly wiped per-user root.

## 3. Scope

R5 delivers seven things, each a thin vertical of a contract the root SPEC already states:

1. **The policy resource** (per D70): one installation row with the trigger-code, paper-order, and delivery-mode columns; `GET`/`PUT /settings/policies`; `STEER` refused as a visibly disabled mode; one `POLICY_CHANGED` record per field that changes.
2. **Trigger-code approval** (per D70, root §8.3): the code snapshot's `approval` state, the projection rule that keeps an unapproved trigger from ever being due, and the decision that re-computes it.
3. **Paper-order approval** (per D10, D70, root §12.3): the pending trading action that never reaches the sandbox, its idempotent replay, the decision that re-enters the accept path exactly where acceptance left it, terminal denial, an ungated cancel, and the agent's read-only view of its own record.
4. **The live-event tail and browser-grade sockets** (per D66, D71): `WS /events` over the one `operational_events` table with a client-held cursor, a newest-first listing with keyset cursors, `marketrig desk events`, and first-frame bearer authentication plus an exact-origin allowlist on every WebSocket the browser opens.
5. **The shell** (per D26, D30, D52, D66): the Tauri 2 crate in `src-tauri/` — window, tray, single instance, daemon bootstrap through endpoint discovery, close-hides, Quit, the notification and autostart plugins, and four commands; no HTTP of its own.
6. **The frontend** (per D29, D33, D55–D59, D62, D72): the Vue 3 application over the client generated from the daemon's own OpenAPI document, the three panels, one warm ghostty-web presentation per live terminal, the events-driven refetch model, the approval and notification surfaces, and the one token set with its concrete values.
7. **Verification** (per D60, D61, D75): the daemon's OpenAPI emission and the CI check that the committed client matches it; gate scenarios for the approval and event mechanics; the frontend's own checks; and the WebdriverIO packaged smoke from a wiped per-user root.

## 4. Non-goals

- No risk policy: approval is the one control, installation-wide, with no per-desk override, no size or notional threshold, no allowlist of instruments, and no automatic denial (per D10, D70).
- No approval by the agent, through the runtime's own approval dialog, or through the `marketrig` CLI; no daemon prompt on a decision (per D70).
- No `STEER` delivery, no interrupt on Claude Code, no session control the agent can invoke (per D69, root §11.2).
- No Simplified Chinese: R5 ships the `en` catalog through the localization mechanism R6 fills; the locale setting, detection, and `set_locale` arrive with R6 (per D68).
- No onboarding wizard: first launch opens the same window with its Settings tab, whose runtime, memory-provider, autostart, and policy controls are the onboarding.
- No packaging beyond what the smoke needs: signing, notarization, the installer's CLI PATH registration, and the bundled interpreter are R6 (root §4.1, §18).
- No renderer-loss reconstruction, no transcript, no second emulator (per D33).
- No approval history view beyond the records that already exist: the code snapshot and the trading action carry their decision, and the Activity tab shows the events.
- No URL routing, client store, query cache, or theme switcher (per D62, D72).

## 5. Success criteria

1. On both platforms, the packaged application started from a wiped per-user root finds no daemon, starts one, verifies it through authenticated health and the UUID match, and shows the window; a second launch focuses that window instead of starting another.
2. A desk's terminal attaches in the center panel through the first-frame-authenticated socket, keeps consuming output while the window is hidden to the tray, and shows those bytes on reopen without reattaching; a page reload reattaches and replays the ring.
3. With paper orders set to **Require approval**, a `submit_order` from a session or from trigger code commits `PENDING`, answers no order projection, replays idempotently, is listed under `GET /approvals` and in the Approvals tab, raises one notification, and reaches the sandbox only on approval — after which fills, positions, and history are exactly what an ungated order produces; a denied one records `DENIED`, never reaches the sandbox, and a repeated `action_id` returns that record.
4. With trigger code set to **Require approval**, a code-bearing trigger is created complete with a `PENDING` snapshot and no next occurrence, stays undue through enable, disable, and elapsed occurrences, fires after approval, and a denied one leaves no firing, no execution, and no prompt behind.
5. `WS /events` delivers every committed operational event once and in `(occurred_at_ns, id)` order; a client reconnecting with its last cursor misses nothing and duplicates nothing, sees the new daemon's `RECOVERY` row after a restart, and is closed `4408` when it stops reading.
6. Quit from the window or the tray ends every open process row `QUIT`, stops the daemon through `POST /quit`, removes the endpoint file, and exits the shell; Close does none of those.
7. The frontend REST client is generated from the document `marketrigd` emits from its own routes, and CI fails when the committed client no longer matches it; Prettier, correctness-only ESLint, `vue-tsc`, and Vitest are green beside the Rust checks on both platforms.
8. The gate covers criteria 3, 4, and 5 and the socket authentication unattended on both platforms; the packaged smoke covers criteria 1, 2, and 6 once per platform, operator-run.
