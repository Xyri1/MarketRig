---
name: desk-improvement
description: Evaluate a closed position cycle from its MarketRig EVALUATION prompt, decide whether a lesson exists, state it so memory keeps it, and improve a desk skill. Use when an EVALUATION prompt arrives or when reviewing recent cycles.
---

# Desk improvement

MarketRig seeded this skill once into desk `<name>`'s memory. It is yours: rewrite it with the
`openviking` `edit` tool as your own practice improves.

1. Read the prompt: cycle id, instrument, net realized P&L with its currency, the client order ids
   and fill ids. Then fetch what you need — `marketrig --json history cycles <desk>`,
   `history orders`, `history fills` — and search what memory already holds with the `openviking`
   `find` or `search` tool: `"<instrument> <what you did>"`.
2. Judge the outcome. Realized P&L is the reward; compare the intent you had when you acted with what
   the fills and the price path show. Separate luck from process.
3. Decide whether anything was learned. Most cycles teach nothing new; say so and stop.
4. If a lesson exists, state it once, plainly, in one or two sentences a future session can act on,
   naming the instrument; the session is captured into memory, and `remember` keeps it explicitly.
5. If the lesson changes how you would act next time, improve the procedure: `edit` an existing
   skill at `viking://~/skills/<skill>/SKILL.md` or `write` a new one, keeping the frontmatter
   `name` and `description`. The copy under `.agents/skills/` is read-only; MarketRig refreshes it.
6. Tell the user what you concluded in one paragraph, naming the cycle id.
