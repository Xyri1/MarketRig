import { spawn, execFile } from 'node:child_process';
import { mkdir, copyFile, readFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

const root = fileURLToPath(new URL('../', import.meta.url));
const win = process.platform === 'win32';

export async function runDev(args = []) {
  const dataRoot = resolve(process.env.MARKETRIG_TEST_DATA_ROOT ?? join(root, 'target/dev-data'));
  const bin = join(dataRoot, 'bin');
  const endpointPath = join(dataRoot, 'data/runtime/endpoint.json');
  const env = { ...process.env, MARKETRIG_TEST_DATA_ROOT: dataRoot, MARKETRIG_DEV_SUPERVISED: '1' };
  const children = [];
  let daemon;
  let stopping;
  let exitCode = 0;

  function start(program, argv, stdin = 'ignore') {
    const child = spawn(program, argv, {
      cwd: root, env, detached: false,
      stdio: [stdin, 'inherit', 'inherit'],
    });
    const exited = new Promise((resolve) => {
      child.once('exit', (code) => resolve(code ?? 1));
      child.once('error', (error) => { console.error(error.message); resolve(1); });
    });
    const entry = { child, exited };
    children.push(entry);
    return entry;
  }

  async function killTree(entry) {
    const { child, exited } = entry;
    if (!child.pid) return;
    if (win) {
      if (child.exitCode === null && child.signalCode === null) {
        await new Promise((resolve) => execFile('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true }, () => resolve()));
      }
    } else {
      try { process.kill(-child.pid, 'SIGINT'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
      await Promise.race([exited, delay(3000)]);
      try { process.kill(-child.pid, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
    }
    await exited;
  }

  function stop(code) {
    if (stopping) return stopping;
    exitCode = code;
    stopping = (async () => {
      daemon?.child.stdin.end();
      await Promise.all(children.filter((entry) => entry !== daemon).map(killTree));
      if (daemon) {
        const exited = await Promise.race([daemon.exited.then(() => true), delay(10000, false)]);
        if (!exited) await killTree(daemon);
      }
    })();
    return stopping;
  }

  const interrupt = () => { void stop(130); };
  const terminate = () => { void stop(143); };
  process.on('SIGINT', interrupt);
  process.on('SIGTERM', terminate);
  try {
    // Never reuse another command's daemon, including an installed instance.
    try {
      const endpoint = JSON.parse(await readFile(endpointPath, 'utf8'));
      const response = await fetch(`http://127.0.0.1:${endpoint.port}/health`, {
        headers: { Authorization: `Bearer ${endpoint.credential}` }, signal: AbortSignal.timeout(500),
      });
      if ((await response.json()).daemon_uuid === endpoint.daemon_uuid) {
        throw new Error(`A dev daemon is already running in ${dataRoot}. Stop its dev command first.`);
      }
    } catch (error) {
      if (error.message.startsWith('A dev daemon')) throw error;
      if (error.code !== 'ENOENT' && !(error instanceof TypeError) && error.name !== 'TimeoutError') throw error;
    }

    const names = ['marketrigd', 'marketrig', 'marketrig-mcp'];
    const build = start('cargo', ['build', ...names.flatMap((name) => ['-p', name])]);
    const built = await build.exited;
    if (stopping) return exitCode;
    if (built !== 0) throw new Error(`Dev build failed (${built}).`);
    await mkdir(bin, { recursive: true });
    const suffix = win ? '.exe' : '';
    for (const name of names) {
      await copyFile(join(root, 'target/debug', name + suffix), join(bin, name + suffix));
    }
    if (stopping) return exitCode;
    daemon = start(join(bin, 'marketrigd' + suffix), ['--exit-on-stdin-close'], 'pipe');
    const deadline = Date.now() + 30000;
    while (!stopping) {
      if (daemon.child.exitCode !== null || daemon.child.signalCode !== null) throw new Error('Dev daemon exited during startup.');
      try {
        const endpoint = JSON.parse(await readFile(endpointPath, 'utf8'));
        if (endpoint.pid === daemon.child.pid) {
          const response = await fetch(`http://127.0.0.1:${endpoint.port}/health`, {
            headers: { Authorization: `Bearer ${endpoint.credential}` }, signal: AbortSignal.timeout(500),
          });
          if ((await response.json()).daemon_uuid === endpoint.daemon_uuid) break;
        }
      } catch (error) {
        if (error.code !== 'ENOENT' && !(error instanceof TypeError) && error.name !== 'TimeoutError') throw error;
      }
      if (Date.now() >= deadline) throw new Error('Dev daemon did not become ready within 30 seconds.');
      await Promise.race([daemon.exited, delay(100)]);
    }
    if (stopping) return exitCode;
    console.log(`Dev data: ${dataRoot}`);
    const vite = start(process.execPath, [join(root, 'node_modules/vite/bin/vite.js'), '--strictPort']);
    const tauri = start(process.execPath, [join(root, 'node_modules/@tauri-apps/cli/tauri.js'), 'dev', '--config', JSON.stringify({ identifier: 'dev.marketrig.desktop.dev', build: { beforeDevCommand: '' } }), ...args]);
    const code = await Promise.race([daemon, vite, tauri].map((entry) => entry.exited));
    await stop(code);
  } catch (error) {
    if (!stopping) console.error(error.message);
    await stop(1);
  } finally {
    await stop(exitCode);
    process.off('SIGINT', interrupt);
    process.off('SIGTERM', terminate);
  }
  return exitCode;
}

if (import.meta.main) process.exitCode = await runDev(process.argv.slice(2));
