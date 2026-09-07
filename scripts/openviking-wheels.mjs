// Produces the locked OpenViking wheel set the release unit carries beside the
// daemon (feature SPEC openviking-continuity §1.3): `pip download` on Python
// 3.12 into `openviking-wheels/<platform>/`, plus `<platform>.lock` listing
// name, version, and sha256 of every wheel, and the upstream license texts.
//
//   node scripts/openviking-wheels.mjs --python <python3.12> [--platform macos-arm64|windows-x64] [--check]
//
// `--check` verifies an existing directory against its lockfile instead of
// downloading. The wheel directories are not committed; the lockfiles are.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync, copyFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const VERSION = '0.4.17.1';
const PLATFORMS = {
  'macos-arm64': ['macosx_14_0_arm64', 'macosx_11_0_arm64', 'macosx_10_13_universal2', 'macosx_10_9_universal2'],
  'windows-x64': ['win_amd64'],
};
const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : fallback;
};
const platform = opt('--platform', process.platform === 'win32' ? 'windows-x64' : 'macos-arm64');
const python = opt('--python');
const tags = PLATFORMS[platform];
if (!tags) throw new Error(`unknown platform ${platform}`);
const dir = join(root, 'openviking-wheels', platform);
const lockPath = join(root, 'openviking-wheels', `${platform}.lock`);

const sha256 = (p) => createHash('sha256').update(readFileSync(p)).digest('hex');
const entry = (file) => {
  const [name, version] = file.split('-');
  return `${name} ${version} ${file} sha256=${sha256(join(dir, file))}`;
};
const wheels = () => readdirSync(dir).filter((f) => f.endsWith('.whl')).sort();

if (args.includes('--check')) {
  const want = readFileSync(lockPath, 'utf8').trim().split('\n');
  const have = wheels().map(entry);
  const missing = want.filter((l) => !have.includes(l));
  if (missing.length) {
    console.error(`openviking-wheels/${platform} differs from its lockfile:\n${missing.join('\n')}`);
    process.exit(1);
  }
  console.log(`openviking-wheels/${platform}: ${have.length} wheels match ${platform}.lock`);
  process.exit(0);
}

if (!python) throw new Error('--python <python3.12> is required');
rmSync(dir, { recursive: true, force: true });
mkdirSync(dir, { recursive: true });
execFileSync(
  python,
  [
    '-m', 'pip', 'download', '--quiet', '--only-binary=:all:', '--python-version', '3.12',
    ...tags.flatMap((t) => ['--platform', t]),
    '--dest', dir, `openviking==${VERSION}`,
  ],
  { stdio: 'inherit' },
);
for (const f of ['LICENSE']) copyFileSync(join(root, 'crates/marketrigd/seed/openviking', f), join(dir, `OPENVIKING-${f}`));
const lines = wheels().map(entry);
writeFileSync(lockPath, lines.join('\n') + '\n');
console.log(`openviking-wheels/${platform}: ${lines.length} wheels, lockfile written`);
