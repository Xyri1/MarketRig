// Produces the locked OpenViking wheel set the release unit carries beside the
// daemon (feature SPEC openviking-continuity §1.3): `pip download` on Python
// 3.12 into `openviking-wheels/<platform>/`, the pinned `uv` release binary
// the daemon installs that set with, plus `<platform>.lock` listing name,
// version, and sha256 of every wheel and of `uv`, and the upstream license texts.
//
//   node scripts/openviking-wheels.mjs --python <python3.12> [--platform macos-arm64|windows-x64] [--check] [--write-lock]
//
// `--check` verifies an existing directory against its lockfile instead of
// downloading. The wheel directories are not committed; the lockfiles are. A
// lockfile is produced on its own platform — pip evaluates environment markers
// against the host, not `--platform` — and a download that drifts from the
// committed lockfile fails instead of rewriting it, until `--write-lock`.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
  copyFileSync,
} from 'node:fs';
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
// The installer: uv unpacks the set in parallel and writes no bytecode, which
// on Windows is the difference between 16 minutes under pip and under two.
const UV_VERSION = '0.11.26';
const UV = {
  'macos-arm64': { archive: 'uv-aarch64-apple-darwin.tar.gz', member: 'uv-aarch64-apple-darwin/uv', binary: 'uv' },
  'windows-x64': { archive: 'uv-x86_64-pc-windows-msvc.zip', member: 'uv.exe', binary: 'uv.exe' },
}[platform];
const dir = join(root, 'openviking-wheels', platform);
const lockPath = join(root, 'openviking-wheels', `${platform}.lock`);

const sha256 = (p) => createHash('sha256').update(readFileSync(p)).digest('hex');
const entry = (file) => {
  const [name, version] = file.split('-');
  return `${name} ${version} ${file} sha256=${sha256(join(dir, file))}`;
};
const wheels = () => readdirSync(dir).filter((f) => f.endsWith('.whl')).sort();
const uvEntry = () => `uv ${UV_VERSION} ${UV.binary} sha256=${sha256(join(dir, UV.binary))}`;
// Every locked file: the wheels, then uv.
const have = () => [...wheels().map(entry), ...(existsSync(join(dir, UV.binary)) ? [uvEntry()] : [])];

// Both directions: a wheel the lockfile names and the set lacks, and a wheel the
// set carries and the lockfile does not.
const drift = (want, have) => [
  ...want.filter((l) => !have.includes(l)).map((l) => `- ${l}`),
  ...have.filter((l) => !want.includes(l)).map((l) => `+ ${l}`),
];
const committed = () => readFileSync(lockPath, 'utf8').trim().split('\n');

if (args.includes('--check')) {
  const lines = have();
  const differs = drift(committed(), lines);
  if (differs.length) {
    console.error(`openviking-wheels/${platform} differs from its lockfile:\n${differs.join('\n')}`);
    process.exit(1);
  }
  console.log(`openviking-wheels/${platform}: ${lines.length - 1} wheels and uv ${UV_VERSION} match ${platform}.lock`);
  process.exit(0);
}

// `pip download --platform` picks the wheel tags, but environment markers
// (`sys_platform == 'win32'`) are still evaluated against the host, so a foreign
// host silently drops dependencies. Each lockfile is produced on its platform.
const host = { darwin: 'macos-arm64', win32: 'windows-x64' }[process.platform];
if (platform !== host) {
  console.error(
    `${platform}.lock is produced on ${platform}: this host is ${host ?? process.platform}, ` +
      `and pip would evaluate the dependencies' environment markers against it.`,
  );
  process.exit(1);
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
// OpenViking's own license text; every dependency's is already in the directory,
// inside its wheel as `<dist-info>/LICENSE*`.
for (const f of ['LICENSE']) copyFileSync(join(root, 'crates/marketrigd/seed/openviking', f), join(dir, `OPENVIKING-${f}`));
// uv from its GitHub release, one binary out of the archive, with its two
// license texts from the same tag. Windows' own bsdtar reads the zip.
const fetchTo = async (url, file) => {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${url}: ${res.status}`);
  writeFileSync(file, Buffer.from(await res.arrayBuffer()));
};
const release = `https://github.com/astral-sh/uv/releases/download/${UV_VERSION}`;
const archive = join(dir, UV.archive);
await fetchTo(`${release}/${UV.archive}`, archive);
const tar = process.platform === 'win32' ? join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe') : 'tar';
execFileSync(tar, ['-xf', archive, '-C', dir, UV.member], { stdio: 'inherit' });
if (UV.member !== UV.binary) {
  renameSync(join(dir, UV.member), join(dir, UV.binary));
  rmSync(join(dir, UV.member.split('/')[0]), { recursive: true, force: true });
}
rmSync(archive);
for (const f of ['LICENSE-MIT', 'LICENSE-APACHE']) {
  await fetchTo(`https://raw.githubusercontent.com/astral-sh/uv/${UV_VERSION}/${f}`, join(dir, `UV-${f}`));
}
const lines = have();
// The committed lockfile is evidence, not an output: a download that resolved
// something else says so and stops, unless the drift is the point.
const differs = existsSync(lockPath) ? drift(committed(), lines) : [];
if (differs.length && !args.includes('--write-lock')) {
  console.error(
    `openviking-wheels/${platform} resolved a set ${platform}.lock does not name; ` +
      `pass --write-lock to accept it:\n${differs.join('\n')}`,
  );
  process.exit(1);
}
const written = !existsSync(lockPath) || differs.length > 0;
if (written) writeFileSync(lockPath, lines.join('\n') + '\n');
console.log(`openviking-wheels/${platform}: ${lines.length - 1} wheels and uv ${UV_VERSION}, lockfile ${written ? 'written' : 'unchanged'}`);
