'use strict';

const fs = require('node:fs');
const childProcess = require('node:child_process');
const { Worker } = require('node:worker_threads');

function visibleFile(path, nulReplacement = '') {
  try {
    return fs.readFileSync(path, 'utf8').replaceAll('\0', nulReplacement).trim();
  } catch (error) {
    return `<error:${error.code || error.message}>`;
  }
}

async function main() {
  const worker = await new Promise((resolve) => {
    const instance = new Worker(
      `const { parentPort } = require('node:worker_threads'); parentPort.postMessage(6 * 7)`,
      { eval: true },
    );
    instance.once('message', (value) => resolve({ value }));
    instance.once('error', (error) => resolve({ error: error.message }));
  });

  const shell = childProcess.spawnSync('/bin/sh', ['-c', 'printf node-shell-child'], {
    encoding: 'utf8',
  });
  const self = childProcess.spawnSync(
    process.execPath,
    ['-e', 'process.stdout.write("node-self-child")'],
    { encoding: 'utf8', timeout: 3000 },
  );

  console.log(
    JSON.stringify({
      node: 'compat-v1',
      pid: process.pid,
      ppid: process.ppid,
      argv: process.argv,
      argv0: process.argv0,
      execPath: process.execPath,
      procExe: fs.readlinkSync('/proc/self/exe'),
      procCmdline: visibleFile('/proc/self/cmdline', '|'),
      procComm: visibleFile('/proc/self/comm'),
      hostnameBytes: fs.readFileSync('/etc/hostname').length,
      worker,
      shell: {
        status: shell.status,
        signal: shell.signal,
        stdout: shell.stdout,
        stderr: shell.stderr,
      },
      self: {
        status: self.status,
        signal: self.signal,
        error: self.error && self.error.message,
        stdout: self.stdout,
        stderr: self.stderr,
      },
    }),
  );
}

main().catch((error) => {
  console.error(error.stack || error);
  process.exitCode = 1;
});
