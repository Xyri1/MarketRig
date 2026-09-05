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
execFileSync(win ? 'pnpm.cmd' : 'pnpm', ['exec', 'tauri', 'build'], { stdio: 'inherit' });
