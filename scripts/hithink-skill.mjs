// Rewrites the vendored HiThink skill into the one MarketRig seeds every desk
// with (feature SPEC hithink-a-share §5.2, per HT-5):
//
//   vendor/hithink-finance/  ->  crates/marketrigd/seed/skills/hithink-finance/
//                            ->  crates/marketrigd/src/research_paths.rs
//
//   node scripts/hithink-skill.mjs [--check]
//
// The reference pages upstream wrote stay as upstream wrote them, in Chinese;
// what changes is everything naming a surface this desk cannot reach — the CLI,
// the MCP servers, the Python SDK, the API key the agent would hold itself —
// and every request example, which becomes the one command it has. `--check`
// regenerates into memory and exits 1 on any difference; CI runs it.
import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, posix } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const vendor = join(root, 'vendor', 'hithink-finance');
const seed = 'crates/marketrigd/seed/skills/hithink-finance';
const pathsRs = 'crates/marketrigd/src/research_paths.rs';

const DESCRIPTION =
  'Read HiThink A-share market, financial, valuation, index, sector, fund and special data through `marketrig research hithink <path>`; use it to resolve a thscode, price a name, or read a report before deciding.';
// Upstream's other three surfaces, and the agent card of a fourth product.
const DROP_PAGES = /^references\/(cli|mcp|python-sdk)(\.md$|\/)|^agents\//;
const DROP_SECTIONS = [
  '## Skill 低频自更新引导',
  '## CLI 低频静默更新自检',
  '## 接入方式决策',
  '## 统一 API Key',
  '## CLI 推荐与联动',
  '## 故障路由',
  // api.md: edit this directory, then run upstream's mirror script. The desk's
  // copy of the skill is a read-only projection (root SPEC §16, per D83).
  '## 维护规则',
];
// Residual lines naming a surface, a credential or a page that is now gone.
// Each pattern drops the whole line — and, when the line is a list item, its
// indented children — so each is one upstream only ever writes on a line of its
// own: a table row, a list item, a numbered step.
const DROP_LINES = [
  /X-api-key/,
  /HITHINK_FINANCE_API_KEY|credentials\.env|FUYAO_TOKEN/,
  /references\/(cli|mcp|python-sdk)/,
  /\bMCP\b|\bCLI\b|\bPython\b|marketdb|Keyring/,
  /`hithink-finance[ `]|npx |pip install/,
  /fuyao\.aicubes\.cn\/admin|API Key|统一凭据|统一来源|凭据文件/,
  // The four-surface routing itself: probe the environment, choose a surface,
  // open its first-level reference. There is one surface here.
  /选择接入方式|只做无副作用的当前环境探测|选择一种主路径|只读取下表对应的一个一级 reference/,
];
// `curl [-flags] 'https://fuyao.aicubes.cn/api/<path>[?<query>]' \` plus its
// key header. Whatever wraps the block — a `$(` capture, a trailing pipe —
// falls outside the match and survives.
const CURL =
  /curl(?:\s+-[A-Za-z]+)*\s+(['"])https:\/\/fuyao\.aicubes\.cn\/api\/([^'"]+)\1[ \t]*\\\n[ \t]*-H[ \t]+(['"])X-api-key:[^'"]*\3/g;

const lf = (text) => text.replaceAll('\r\n', '\n');
const read = (file) => lf(readFileSync(file, 'utf8'));
const walk = (dir, rel = '') =>
  readdirSync(dir, { withFileTypes: true })
    .sort((a, b) => (a.name < b.name ? -1 : 1))
    .flatMap((e) =>
      e.isDirectory()
        ? walk(join(dir, e.name), posix.join(rel, e.name))
        : [rel ? posix.join(rel, e.name) : e.name],
    );

let rewrote = 0;
const toCommand = (_match, _quote, target) => {
  rewrote += 1;
  const [path, query] = target.split('?');
  const params = query ? query.split('&').map((pair) => ` --param ${pair}`) : [];
  return `marketrig research hithink ${path}${params.join('')}`;
};
const dropSections = (text) => {
  const kept = [];
  let dropping = false;
  for (const line of text.split('\n')) {
    if (line.startsWith('## ')) dropping = DROP_SECTIONS.includes(line.trim());
    if (!dropping) kept.push(line);
  }
  return kept.join('\n');
};
// Drops a matching line with the list children it introduces, then renumbers
// every ordered list — a step 3 that named the CLI took steps 4 and 5 with it.
const dropLines = (text) => {
  const kept = [];
  let children = -1;
  let n = 0;
  for (const line of text.split('\n')) {
    const indent = line.length - line.trimStart().length;
    if (children >= 0 && (line.trim() === '' || indent <= children)) children = -1;
    if (children >= 0) continue;
    if (DROP_LINES.some((p) => p.test(line))) {
      if (/^\s*(?:[-*]|\d+\.)\s/.test(line)) children = indent;
      continue;
    }
    if (!/^\d+\. /.test(line)) n = 0;
    else n += 1;
    kept.push(n ? line.replace(/^\d+\. /, `${n}. `) : line);
  }
  return kept.join('\n');
};

// One repo-relative path -> its whole content; nothing is written until the end,
// so `--check` is the same generation compared instead of stored.
const outputs = new Map();
const preamble = read(join(root, 'scripts', 'hithink-skill-preamble.md')).trim();
for (const rel of walk(vendor)) {
  if (rel === 'VENDOR.md' || DROP_PAGES.test(rel)) continue;
  let text = read(join(vendor, rel));
  if (rel === 'SKILL.md') {
    const [, front, body] = text.match(/^---\n([\s\S]*?)\n---\n([\s\S]*)$/);
    text =
      `---\n${front.replace(/^description:.*$/m, `description: ${DESCRIPTION}`)}\n---\n` +
      dropLines(dropSections(body)).replace(
        /^# hithink finance\n/m,
        `# hithink finance\n\n${preamble}\n`,
      );
  } else if (rel.endsWith('.md')) {
    text = dropLines(dropSections(text.replace(CURL, toCommand)));
  }
  // Upstream is MIT: the license travels with the copy.
  outputs.set(posix.join(seed, rel), text.replace(/\n{3,}/g, '\n\n'));
}

const capabilities = read(join(vendor, 'references/api/capability-map.md'));
const paths = [...new Set([...capabilities.matchAll(/`GET \/api\/([^`?\s]+)`/g)].map((m) => m[1]))];
outputs.set(
  pathsRs,
  `//! Generated by scripts/hithink-skill.mjs from vendor/hithink-finance/references/api/capability-map.md; do not edit.
//!
//! The allowlist \`GET /research/hithink/{path}\` admits a request against
//! (feature SPEC hithink-a-share §4.1, per HT-4).

/// Every endpoint the vendored capability map documents, \`/api/\` stripped, sorted.
pub const RESEARCH_PATHS: &[&str] = &[
${paths
  .sort()
  .map((p) => `    "${p}",\n`)
  .join('')}];

#[cfg(test)]
mod research;
#[cfg(test)]
mod skill;
`,
);

let left = 0;
for (const text of outputs.values()) {
  for (const line of text.split('\n')) if (line.trimStart().startsWith('curl')) left += 1;
}

if (process.argv.includes('--check')) {
  const stale = existsSync(join(root, seed))
    ? walk(join(root, seed), seed).filter((rel) => !outputs.has(rel))
    : [];
  const differs = [...outputs.keys()].filter((rel) => {
    const file = join(root, rel);
    return !existsSync(file) || read(file) !== outputs.get(rel);
  });
  for (const rel of [...stale.map((r) => `stale   ${r}`), ...differs.map((r) => `differs ${r}`)]) {
    console.error(rel);
  }
  if (stale.length || differs.length) {
    console.error('run `node scripts/hithink-skill.mjs` and commit the result');
    process.exit(1);
  }
  console.log(`up to date: ${outputs.size} files, ${paths.length} research paths`);
} else {
  rmSync(join(root, seed), { recursive: true, force: true });
  for (const [rel, text] of outputs) {
    mkdirSync(dirname(join(root, rel)), { recursive: true });
    writeFileSync(join(root, rel), text);
  }
  console.log(`wrote ${outputs.size} files, ${paths.length} research paths`);
}
console.log(`rewrote ${rewrote} curl blocks, left ${left}`);
