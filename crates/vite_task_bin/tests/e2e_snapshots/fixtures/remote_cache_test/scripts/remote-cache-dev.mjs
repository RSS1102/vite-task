// Start/stop a local `wrangler dev` instance of the remote cache worker
// (services/remote-cache-worker) for this e2e fixture. Invoked as a `vp run`
// task command (see vite-task.json) so its lifetime is controlled by ordinary
// task steps:
//
//   node scripts/remote-cache-dev.mjs start --state-dir <dir>
//   node scripts/remote-cache-dev.mjs stop  --state-dir <dir>
//
// `start` copies the worker into <dir> (so no committed `.dev.vars`/state
// leaks), picks a free port, spawns `wrangler dev` detached (local simulation
// mode: KV + R2 emulated on disk), waits until it serves HTTP, then writes the
// endpoint URL and bearer token into a `.env` in the workspace root and records
// the pid. `stop` kills the recorded process tree. The values go to `.env`
// (not stdout/args) so the dynamic port and the secret never appear in the test
// snapshot — `vp run` reads VITE_REMOTE_CACHE_URL / VITE_REMOTE_CACHE_TOKEN from
// that `.env`.
//
// wrangler and the worker source are located through the harness's
// `node_modules` symlink (tmp/node_modules -> repo/packages/tools/node_modules),
// the same mechanism fixture scripts use to import vite-task-client.

import { spawn } from 'node:child_process';
import net from 'node:net';
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { setTimeout as sleep } from 'node:timers/promises';

// The harness stages this fixture at <tmp>/<fixture>_case_N and symlinks
// <tmp>/node_modules -> repo/packages/tools/node_modules. Resolve the repo root
// (and from it wrangler + the worker source) by canonicalizing that symlink.
const TOOLS_NODE_MODULES = fs.realpathSync(path.join(process.cwd(), '..', 'node_modules'));
const REPO_ROOT = path.dirname(path.dirname(path.dirname(TOOLS_NODE_MODULES)));
const WRANGLER_BIN = path.join(TOOLS_NODE_MODULES, '.bin', 'wrangler');
const WORKER_SRC = path.join(REPO_ROOT, 'services', 'remote-cache-worker');
// The value is irrelevant to the test (only that it round-trips via files), but
// it exercises the worker's bearer-auth path end to end.
const AUTH_TOKEN = 'vite-e2e-remote-cache-token';
const READY_TIMEOUT_MS = 90_000;

function parseFlags(args) {
  const flags = {};
  for (let i = 0; i < args.length; i += 2) {
    const key = args[i];
    if (!key?.startsWith('--')) throw new Error(`unexpected argument: ${key}`);
    flags[key.slice(2)] = args[i + 1];
  }
  return flags;
}

function freePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.on('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

async function waitUntilReady(port, logPath) {
  const deadline = Date.now() + READY_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${port}/`);
      if (res.ok) return;
    } catch {
      // not up yet
    }
    await sleep(250);
  }
  const log = fs.existsSync(logPath) ? fs.readFileSync(logPath, 'utf8') : '';
  throw new Error(`wrangler dev did not become ready within ${READY_TIMEOUT_MS}ms:\n${log}`);
}

async function start(flags) {
  const stateDir = path.resolve(flags['state-dir']);
  // `vp run` reads VITE_REMOTE_CACHE_URL / VITE_REMOTE_CACHE_TOKEN from a `.env`
  // in the workspace root, so write them there (process.cwd() is the workspace).
  const envFile = path.resolve('.env');

  // Copy the worker into an isolated dir (no .dev.vars / node_modules / state).
  const workerDir = path.join(stateDir, 'worker');
  fs.mkdirSync(path.join(workerDir, 'src'), { recursive: true });
  fs.copyFileSync(path.join(WORKER_SRC, 'wrangler.toml'), path.join(workerDir, 'wrangler.toml'));
  fs.copyFileSync(
    path.join(WORKER_SRC, 'src', 'index.ts'),
    path.join(workerDir, 'src', 'index.ts'),
  );
  // Configure the worker's bearer token via .dev.vars in the isolated copy.
  fs.writeFileSync(path.join(workerDir, '.dev.vars'), `AUTH_TOKEN=${AUTH_TOKEN}\n`);

  const port = await freePort();
  const persistDir = path.join(stateDir, 'persist');
  fs.mkdirSync(persistDir, { recursive: true });
  const logPath = path.join(stateDir, 'wrangler.log');
  const logFd = fs.openSync(logPath, 'w');

  // Don't let the detached wrangler/workerd inherit fspy's syscall interposer
  // from a traced parent task — the tracer's supervisor goes away when this
  // process exits.
  const childEnv = { ...process.env, CI: '1', WRANGLER_SEND_METRICS: 'false', NO_COLOR: '1' };
  delete childEnv.LD_PRELOAD;
  delete childEnv.DYLD_INSERT_LIBRARIES;

  const child = spawn(
    WRANGLER_BIN,
    [
      'dev',
      '--port',
      String(port),
      '--ip',
      '127.0.0.1',
      '--persist-to',
      persistDir,
      '--log-level',
      'error',
    ],
    {
      cwd: workerDir,
      detached: true, // own session/group: survives this process, tree-killable on stop
      stdio: ['ignore', logFd, logFd],
      env: childEnv,
    },
  );
  child.unref();
  fs.writeFileSync(path.join(stateDir, 'server.pid'), String(child.pid));

  await waitUntilReady(port, logPath);

  // Publish endpoint + token via the workspace `.env` only once the server is
  // serving. `vp run` picks these up for the remote cache tier.
  fs.writeFileSync(
    envFile,
    `VITE_REMOTE_CACHE_URL=http://127.0.0.1:${port}\nVITE_REMOTE_CACHE_TOKEN=${AUTH_TOKEN}\n`,
  );
}

function stop(flags) {
  const stateDir = path.resolve(flags['state-dir']);
  const pidFile = path.join(stateDir, 'server.pid');
  if (!fs.existsSync(pidFile)) return;
  const pid = Number.parseInt(fs.readFileSync(pidFile, 'utf8').trim(), 10);
  if (!Number.isInteger(pid)) return;

  if (process.platform === 'win32') {
    spawn('taskkill', ['/T', '/F', '/PID', String(pid)], { stdio: 'ignore' });
  } else {
    // Kill the process group (wrangler + workerd); `detached` made the child a
    // group leader, so a negative pid targets the whole group.
    try {
      process.kill(-pid, 'SIGKILL');
    } catch {
      try {
        process.kill(pid, 'SIGKILL');
      } catch {
        // already gone
      }
    }
  }
}

const [command, ...rest] = process.argv.slice(2);
const flags = parseFlags(rest);
try {
  if (command === 'start') {
    await start(flags);
  } else if (command === 'stop') {
    stop(flags);
  } else {
    throw new Error(`usage: remote-cache-dev <start|stop> [flags]; got '${command ?? ''}'`);
  }
} catch (err) {
  console.error(err instanceof Error ? err.message : String(err));
  process.exit(1);
}
