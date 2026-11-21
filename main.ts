import { spawn, type ChildProcess } from 'node:child_process';
import { accessSync, chmodSync, constants, copyFileSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ENV_URL = (process.env.MAKEFUN_ENV_URL ?? '').replace(/\/+$/, '');
if (ENV_URL === '') {
  process.stderr.write('[entry] MAKEFUN_ENV_URL is not set: there is no env to register with\n');
  process.exit(1);
}

const REQUEST_TIMEOUT_MS = 30_000;
const RETRY_INITIAL_MS = 250;
const RETRY_MAX_MS = 2_000;

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

function retryDelay(attempt: number): number {
  return Math.min(RETRY_MAX_MS, RETRY_INITIAL_MS * 2 ** Math.min(attempt, 8));
}

function resolveBinaryPath(): string {
  const root = dirname(fileURLToPath(import.meta.url));
  if (process.platform === 'linux') {
    const bundled = join(root, 'bin', 'forge');
    if (existsSync(bundled)) {
      try {
        accessSync(bundled, constants.X_OK);
        return bundled;
      } catch {
        const tmpBin = '/tmp/forge-bin';
        if (!existsSync(tmpBin)) {
          copyFileSync(bundled, tmpBin);
          chmodSync(tmpBin, 0o755);
        }
        return tmpBin;
      }
    }
  }
  const winBin = join(root, 'bin', 'forge.exe');
  if (existsSync(winBin)) return winBin;
  throw new Error(`Native forge binary not found in ${join(root, 'bin')}`);
}

function startForgeBinary(): Promise<{ child: ChildProcess; port: number }> {
  return new Promise((resolve, reject) => {
    const binPath = resolveBinaryPath();
    const child = spawn(binPath, [], {
      env: { ...process.env, FORGE_BIND: '0.0.0.0:0' },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdoutBuf = '';
    let settled = false;

    child.stdout.on('data', (chunk: Buffer) => {
      stdoutBuf += chunk.toString('utf8');
      const match = stdoutBuf.match(/PORT\s+(\d+)/);
      if (match && !settled) {
        settled = true;
        resolve({ child, port: parseInt(match[1], 10) });
      }
    });
    child.stderr.on('data', (chunk: Buffer) => {
      process.stderr.write(chunk);
    });
    child.once('error', (err) => {
      if (!settled) {
        settled = true;
        reject(err);
      }
    });
    child.once('exit', (code) => {
      if (!settled) {
        settled = true;
        reject(new Error(`forge binary exited early with code ${code}`));
      }
    });
  });
}

async function register(port: number): Promise<void> {
  for (let attempt = 0; ; attempt += 1) {
    try {
      const res = await fetch(`${ENV_URL}/register`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ port }),
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      });
      if (res.status === 200 || res.status === 409) return;
      if (res.status === 400 || res.status === 405) {
        throw new Error(`register answered HTTP ${res.status}: ${await res.text()}`);
      }
    } catch (err) {
      if (err instanceof Error && err.message.startsWith('register answered HTTP ')) throw err;
    }
    await sleep(retryDelay(attempt));
  }
}

async function waitForDone(): Promise<void> {
  for (let attempt = 0; ; attempt += 1) {
    try {
      const res = await fetch(`${ENV_URL}/done`, { signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS) });
      if (res.ok) {
        const body = (await res.json()) as { done?: unknown };
        if (body.done === true) return;
      }
    } catch {
      // Retry until done
    }
    await sleep(retryDelay(attempt));
  }
}

async function main(): Promise<void> {
  const { child, port } = await startForgeBinary();
  process.stdout.write(`[entry] native rust forge listening on port ${port}\n`);

  await register(port);
  process.stdout.write('[entry] registered; waiting for the env to say done\n');

  await waitForDone();
  child.kill('SIGTERM');
  process.stdout.write('[entry] done\n');
}

try {
  await main();
  process.exit(0);
} catch (err) {
  process.stderr.write(`[entry] ${err instanceof Error ? err.message : String(err)}\n`);
  process.exit(1);
}
