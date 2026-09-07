import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';

const win = process.platform === 'win32';
const ext = win ? '.exe' : '';
const names = ['marketrigd', 'marketrig', 'marketrig-mcp'];
const triple = execFileSync('rustc', ['-vV'], { encoding: 'utf8' }).match(/^host: (.+)$/m)[1];

// The bundle ships the locked OpenViking wheel set beside the daemon (feature
// SPEC openviking-continuity §1.3): `wheels.<platform>.conf.json` maps the
// directory into the bundle's resources. It is passed here rather than named
// `tauri.<platform>.conf.json`, which the CLI merges automatically — that file
// would also reach `tauri-build`, and every plain `cargo build` of the shell,
// CI's included, would then demand a directory that is not committed. The
// wheels are checked first, so a missing set is one clear line instead of the
// bundler's resource-path error.
const wheelConfig = `src-tauri/wheels.${win ? 'windows' : 'macos'}.conf.json`;
try {
  execFileSync(process.execPath, ['scripts/openviking-wheels.mjs', '--check'], { stdio: 'inherit' });
} catch {
  console.error(
    'The OpenViking wheels this platform bundles are missing or stale. Produce them first:\n' +
      '  node scripts/openviking-wheels.mjs --python <python3.12>',
  );
  process.exit(1);
}

execFileSync('cargo', ['build', '--release', ...names.flatMap((n) => ['-p', n])], { stdio: 'inherit' });
mkdirSync('src-tauri/binaries', { recursive: true });
for (const name of names) {
  copyFileSync(`target/release/${name}${ext}`, `src-tauri/binaries/${name}-${triple}${ext}`);
}
// The packaged smoke needs the embedded WebDriver server compiled in; every
// other build is the shipped artifact and never carries it (feature SPEC §7.3).
// The smoke drives the .app on macOS and the bare exe on Windows, so the dmg
// and nsis steps are skipped (bundle_dmg.sh also needs Finder).
const wdio =
  process.env.MARKETRIG_SMOKE_WIPE === '1'
    ? ['--features', 'wdio', ...(win ? ['--no-bundle'] : ['--bundles', 'app'])]
    : [];
// The CLI's own node entry, not `pnpm exec`: a Windows box whose pnpm is the
// standalone `pnpm.exe` has no `pnpm.cmd`, and `execFile` cannot run a `.cmd`
// without a shell anyway.
execFileSync(
  process.execPath,
  ['node_modules/@tauri-apps/cli/tauri.js', 'build', '--config', wheelConfig, ...wdio],
  { stdio: 'inherit' },
);
