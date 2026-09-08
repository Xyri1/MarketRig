---
name: hithink-finance
description: Read HiThink A-share market, financial, valuation, index, sector, fund and special data through `marketrig research hithink <path>`; use it to resolve a thscode, price a name, or read a report before deciding.
---

# hithink finance

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

## 直接描述需求

允许用户使用自然语言开始，不要求用户先理解命令、接口、`thscode` 或复权参数。例如：

- “查一下贵州茅台今天的价格。”
- “比较茅台和平安银行最近一年的走势。”
- “查沪深 300 当前成分股。”
- “看看今天有哪些涨停股。”
- “把全市场历史行情导出到文件。”
- “检查我的本地行情库是否需要更新。”

## 任务与能力路由

| 用户意图 | 任务类别 | 处理重点 |
| --- | --- | --- |
| 股票名称、简称、代码或资产类别确认 | 标的消歧 | 转换为唯一 `thscode` 后再取数 |
| 最新价格、历史行情、公司行动、复权 | 行情 | 明确时间窗口与复权口径 |
| 利润表、资产负债表、现金流、财务指标 | 财务 | 明确报告期与频率 |
| 市盈率、市净率、市销率、市现率 | 估值 | 批量查询最新快照，保留 null 与负数 |
| 指数、概念板块、行业板块、成分股 | 指数与板块 | 区分股票、标准指数和 `.TI` 板块 |
| 集合竞价快照、竞价短期基准 | 集合竞价 | 明确标的、实时/终态阶段或查询日期 |
| 基金资料、基金公司、基金经理、净值、收益、财务、持仓、持有人、基金资讯、ETF/LOF 行情 | 公募基金 | 先区分 `fund-otc/fund-etf/fund-lof/fund-reits` 与能力边界 |
| 涨停、跌停、炸板、连板、异动、热榜、龙虎榜 | 特色数据 | 先确认是否为 today-only 能力 |
| 全市场数据、本地库、SQL、同步、导出 | 数据管理 | 检查数据新鲜度并让大结果落盘 |

## 路由流程

1. 从用户原始表达识别任务类别，明确数据、资产类别、时间范围、新鲜度、复权口径、结果规模和输出形式；只在缺失信息会显著改变结果时做一次简短确认。
2. 处理名称、代码和口径等用户输入，不要求用户先提供技术参数。
3. 执行后报告数据源、时间范围、口径、行数、输出路径与线上验证边界。

## 通用执行契约

- 不要求用户先提供完整 `thscode`。用户给名称、简称、不完整代码或不确定资产类别时，先搜索并消歧为唯一 `thscode`；只有多个可信候选会改变结果时才请用户确认，不要猜 `.SH`、`.SZ`、`.BJ` 或指数类型。
- 首次需要向用户展示 `thscode` 时，用一句话说明它是带交易所或指数后缀的唯一证券代码；后续不重复科普。
- 最新快照、财报和指数任务不追问复权。A 股历史行情未指定复权时，使用所选接入方式当前契约声明的默认值（当前为 `forward`，即前复权）并在结果中明示；用户要求原始成交价格时使用 `none`。口径会显著影响结论且用户意图仍不明确时，简要解释“前复权保持当前价格、后复权保持起始价格、none 保留原始价格”，再做一次确认。
- 最新行情、财报、估值、指数和特色数据走远端；本地已有且足够新的历史 OHLCV、复权、面板和 SQL 优先走本地数据库。
- 远端调用不设累计次数上限，但必须合理控制请求节奏，避免短时间集中请求或使用过高并发；批量数据任务优先使用专用批量能力或本地数据库，不得拆成高并发逐条请求。
- 全市场、分页全集、长时间窗口或多标的结果必须落盘，只报告路径、行数、窗口和摘要。
- 真实数据不可用时报告原因；不得使用相似数据、静态示例或模拟数据冒充。
- 分析结果注明数据源、时间、报告期、复权口径和“非投资建议”。
- 离线契约只能证明支持范围，不能证明当前会话已连接或账号有权限；线上可用性必须通过实际授权请求验证。

## 失败输出契约

失败时按固定顺序向用户报告：失败阶段、原始错误摘要、是否重试及原因、唯一的下一步动作、尚未完成的验证。不要只返回错误码或泛化为“服务不可用”。

- 参数、标的或能力不支持：修正可确定的输入；存在多个有效语义时再请用户确认，不要盲目重试。
- 触发动态限流：降低请求频率和并发度，等待后再做有界退避重试；不得立即并发重放请求。
- 网络错误、`4001` 或 `5xxx`：只做有界退避重试；仍失败时报告尝试次数和最后错误。
- 空数据：先判断非交易日、today-only、报告期或筛选条件是否导致预期空结果，不要直接宣称服务故障。
- 本地数据缺失或过旧：报告数据库路径和最新日期，给出初始化或同步建议，不静默切换为全市场远端逐股请求。

## 适用对象与结果偏好

- 普通用户直接说股票名称和想知道的问题；Skill 负责代码、工具和参数转换。
- Agent/自动化默认使用结构化输出、稳定错误语义和明确退出状态。
- 用户可指定“只给摘要 / 返回表格 / 保存 CSV 或 Parquet / 给出可复现命令”；未指定时，小结果摘要展示，大结果落盘。

## 常见避错

- 错误：先要求用户提供完整 `thscode`；正确：先用名称或代码搜索并消歧。
- 错误：为验证认证下载全市场数据；正确：使用目标能力的最小有界真实请求。

## 常见问题

- **能查基金吗？** 支持公募基金资料、公司、经理、披露、财务、净值、收益、持有人结构、公开资讯元数据、ETF/LOF 快照和 ETF 日线；不支持申赎交易或基金推荐。
- **能查估值吗？** 支持批量查询 A 股最新五项估值快照；当前不提供历史估值、自选指标或指数/基金估值。
- **能查港股或分钟行情吗？** 当前不能；明确说明边界，仅在数据含义等价时给出替代入口。

## 能力边界

- **擅长处理**：A 股行情与复权、集合竞价、财报与指标、最新估值、指数/板块/特色数据、公募基金资料、经理、披露与场内行情、本地 DuckDB 同步与导出。
- **需要用户素材或确认**：多个同名标的无法唯一消歧、投资组合或自有清单、非默认时间/复权/输出要求。
- **超出范围**：分钟 K/tick/Level-2，港股/美股、基金申赎交易/推荐、期货/期权，宏观数据/新闻公告原文/研报/回测引擎。
- 超出范围时明确说明；只有数据含义等价时才提供替代路径，不得用近似数据、静态示例或模拟数据冒充真实结果。
