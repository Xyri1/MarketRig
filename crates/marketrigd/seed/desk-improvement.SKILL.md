---
name: desk-improvement
description: Evaluate a closed position cycle from its MarketRig EVALUATION prompt, decide whether a lesson exists, retain it in desk memory, and improve a desk skill. Use when an EVALUATION prompt arrives or when reviewing recent cycles.
---

# Desk improvement

MarketRig seeded this skill once. It is yours: rewrite it as your own practice improves.

1. Read the prompt: cycle id, instrument, net realized P&L with its currency, the client order ids
   and fill ids. Then fetch what you need — `marketrig --json history cycles <desk>`,
   `history orders`, `history fills` — and recall what memory already says:
   `marketrig memory recall <desk> --query "<instrument> <what you did>"`.
2. Judge the outcome. Realized P&L is the reward; compare the intent you had when you acted with what
   the fills and the price path show. Separate luck from process.
3. Decide whether anything was learned. Most cycles teach nothing new; say so and stop.
4. If a lesson exists, retain it once, in one or two sentences a future session can act on, tagged so
   it can be found: `marketrig memory retain <desk> --content "<lesson>" --tag lesson --tag <instrument>`.
5. If the lesson changes how you would act next time, improve the procedure: edit an existing skill
   under `.agents/skills/` or create one, keeping the frontmatter `name` and `description`.
6. Tell the user what you concluded in one paragraph, naming the cycle id.
