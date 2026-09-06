import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';

const win = process.platform === 'win32';
const ext = win ? '.exe' : '';
const names = ['marketrigd', 'marketrig', 'marketrig-mcp'];
const triple = execFileSync('rustc', ['-vV'], { encoding: 'utf8' }).match(/^host: (.+)$/m)[1];

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
execFileSync(win ? 'pnpm.cmd' : 'pnpm', ['exec', 'tauri', 'build', ...wdio], { stdio: 'inherit' });
