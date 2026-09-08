## In MarketRig, one command

This desk reaches the HiThink service through exactly one command:

```bash
marketrig research hithink <path> [--param key=value]... [--out <file>]
```

`<path>` is everything after `/api/` in the reference pages below — `GET /api/a-share/prices/snapshot`
is `marketrig research hithink a-share/prices/snapshot`. Each query parameter is one `--param`.

- The daemon holds the API key and attaches it. There is nothing to install, configure, log into,
  or update, and no key for you to hold, ask for, or write down.
- The command prints HiThink's own response envelope — `code`, `message`, `request_id`, `data` —
  unchanged. Success is `code == 0`. A nonzero `code` is still printed and the command still exits
  `0`: read `code` and `message` before you use `data`.
- A body over 256 KiB, or any call with `--out`, is written to a file instead; the command prints
  the path and the byte count. Report the path, not the contents.
- The service covers A-share (Shanghai, Shenzhen, Beijing), its indices and sectors, and public
  funds. Nothing else.
- When you have a name, a bare ticker, or an uncertain asset class, resolve it through
  `marketrig research hithink meta/tickers/search --param q=<name>` first, and use the `thscode` it
  returns. Never guess a `.SH`, `.SZ`, or `.BJ` suffix.
- This is data, and only data. Trading — quotes, the book, positions, orders — is the `marketrig`
  MCP server your constitution calls the market plane, never this command.

The pages below are HiThink's own reference contract, in Chinese, kept as upstream wrote it; only
their request examples have been rewritten into the command above.
