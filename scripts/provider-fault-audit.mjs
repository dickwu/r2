#!/usr/bin/env node
/** Local provider fault harness.
 *
 * --execute starts a disposable RustFS or MinIO process, places an HTTP proxy
 * in front of it, injects S3-level network faults and verifies retry,
 * identity, cancellation and cleanup behavior against real SDK requests.
 *
 * This is intentionally not the full NEXT-07 network matrix yet: the 5-minute
 * app task recovery case needs a current app binary plus task orchestration, so
 * this harness records that scenario as incomplete instead of claiming
 * acceptance.
 */
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { createServer as createHttpServer, request as httpRequest } from 'node:http';
import { access, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { productionSourceFingerprint } from './audit-source.mjs';
import { createServer as createNetServer } from 'node:net';
import { homedir, tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';

const run = promisify(execFile);

const SCENARIOS = Object.freeze([
  'first-list-503-then-success',
  'ten-second-outage-recovers',
  'retry-after-honored',
  'body-stall-reloads-identity',
  'cancel-stops-new-dispatch',
  'five-minute-outage-persists',
]);

const PLAN = Object.freeze({
  status: 'partial_harness',
  required_inputs: ['FAULT_AUDIT_PROVIDER_BINARY', 'FAULT_AUDIT_PROVIDER_KIND'],
  optional_inputs: [
    'FAULT_AUDIT_OUTPUT',
    'FAULT_AUDIT_SHORT_OUTAGE_MS',
    'FAULT_AUDIT_PROVIDER_BINARY_SHA256',
    'FAULT_AUDIT_APP_BINARY',
    'FAULT_AUDIT_APP_ID',
    'FAULT_AUDIT_APP_SHA256',
    'FAULT_AUDIT_BUILD_JSON',
    'FAULT_AUDIT_PROFILE=smoke|acceptance',
    'FAULT_AUDIT_APP_OUTAGE_MS',
    'FAULT_AUDIT_APP_NFS=0 (explicit skip only)',
  ],
  allowed_provider_kinds: ['rustfs', 'minio'],
  scenarios: SCENARIOS,
  incomplete_scenarios: ['five-minute-outage-persists'],
  app_task_scenarios: [
    'app-move-outage-restart-resume',
    'app-cancel-no-new-requests',
    'app-nfs-outage-retains-stage',
    'app-nfs-stop-retains-recovery',
  ],
});

const MODES = new Set(['--plan', '--self-test', '--execute', '--execute-app']);
const args = process.argv.slice(2);
assert(
  args.length <= 1 && (!args.length || MODES.has(args[0])),
  'Use --plan, --self-test, --execute, or --execute-app'
);
const mode = args[0] ?? '--plan';

const MiB = 1024 * 1024;
const repo = resolve(import.meta.dirname, '..');
const bucket = 'fault-audit';
function sha(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

async function fileSha(path) {
  return sha(await readFile(path));
}

async function freePort() {
  const server = createNetServer();
  await new Promise((ok, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', ok);
  });
  const port = server.address().port;
  await new Promise((ok) => server.close(ok));
  return port;
}

export function validateFaultConfig(input) {
  for (const key of ['providerBinary', 'providerKind']) {
    assert(typeof input[key] === 'string' && input[key].trim(), `Missing explicit ${key} input`);
  }
  assert(
    PLAN.allowed_provider_kinds.includes(input.providerKind),
    'Provider kind must be one of the supported local disposable runtimes'
  );
  const shortOutageMs = Number(input.shortOutageMs ?? 10_000);
  assert(Number.isInteger(shortOutageMs) && shortOutageMs >= 100 && shortOutageMs <= 310_000);
  const providerBinary = resolve(repo, input.providerBinary);
  return { ...input, providerBinary, shortOutageMs };
}

async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill('SIGTERM');
  for (let count = 0; count < 50 && child.exitCode === null && child.signalCode === null; count++) {
    await delay(100);
  }
  if (child.exitCode === null && child.signalCode === null) {
    const exited = new Promise((ok) => child.once('exit', ok));
    child.kill('SIGKILL');
    await exited;
  }
}

async function startProvider(config, root, credentials) {
  const apiPort = await freePort();
  const consolePort = await freePort();
  const data = join(root, 'data');
  await mkdir(data, { recursive: true });
  await mkdir(join(root, 'logs'), { recursive: true });
  const endpoint = `http://127.0.0.1:${apiPort}`;
  const env =
    config.providerKind === 'rustfs'
      ? {
          PATH: process.env.PATH,
          TMPDIR: root,
          RUSTFS_ACCESS_KEY: credentials.accessKeyId,
          RUSTFS_SECRET_KEY: credentials.secretAccessKey,
          RUSTFS_CONSOLE_ENABLE: 'false',
          RUSTFS_OBS_LOGGER_LEVEL: 'warn',
          RUSTFS_OBS_LOG_DIRECTORY: join(root, 'logs'),
        }
      : {
          PATH: process.env.PATH,
          TMPDIR: root,
          MINIO_ROOT_USER: credentials.accessKeyId,
          MINIO_ROOT_PASSWORD: credentials.secretAccessKey,
          MINIO_BROWSER: 'off',
        };
  const args =
    config.providerKind === 'rustfs'
      ? [
          'server',
          '--address',
          `127.0.0.1:${apiPort}`,
          '--console-address',
          `127.0.0.1:${consolePort}`,
          data,
        ]
      : [
          'server',
          '--address',
          `127.0.0.1:${apiPort}`,
          '--console-address',
          `127.0.0.1:${consolePort}`,
          data,
        ];
  const child = spawn(config.providerBinary, args, {
    cwd: root,
    env,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const logs = [];
  const capture = (chunk) => {
    logs.push(chunk.toString());
    if (logs.length > 30) logs.shift();
  };
  child.stdout.on('data', capture);
  child.stderr.on('data', capture);
  const health =
    config.providerKind === 'rustfs' ? `${endpoint}/health/ready` : `${endpoint}/minio/health/live`;
  for (let count = 0; count < 300; count++) {
    assert(
      child.exitCode === null && child.signalCode === null,
      `provider exited: ${logs.join('').replaceAll(credentials.secretAccessKey, '[redacted]')}`
    );
    try {
      if ((await fetch(health, { signal: AbortSignal.timeout(1000) })).ok) {
        return { child, endpoint };
      }
    } catch {}
    await delay(100);
  }
  await stop(child);
  throw new Error(
    `provider readiness timed out: ${logs.join('').replaceAll(credentials.secretAccessKey, '[redacted]')}`
  );
}

function startProxy(backendEndpoint) {
  const backend = new URL(backendEndpoint);
  const state = {
    active: null,
    first503Sent: false,
    retryAfterSent: false,
    bodyStallSent: false,
    bodyStallMs: 1000,
    outageUntil: 0,
    failAllUntil: 0,
    logs: [],
  };
  const server = createHttpServer((clientReq, clientRes) => {
    const entry = {
      scenario: state.active,
      method: clientReq.method,
      url: redactRequestUrl(clientReq.url),
      at_ms: Date.now(),
      injected: null,
    };
    state.logs.push(entry);
    const isList = clientReq.method === 'GET' && clientReq.url.includes('list-type=2');
    const isGet = clientReq.method === 'GET' && !clientReq.url.includes('list-type=2');
    if (
      [
        'app-move-outage-restart-resume',
        'app-cancel-no-new-requests',
        'app-nfs-outage-retains-stage',
        'app-nfs-stop-retains-recovery',
      ].includes(state.active) &&
      Date.now() < state.failAllUntil
    ) {
      entry.injected = `${state.active}-503`;
      clientReq.resume();
      clientRes.writeHead(503, { 'content-type': 'application/xml' });
      clientRes.end('<Error><Code>ServiceUnavailable</Code></Error>');
      return;
    }
    if (state.active === 'first-list-503-then-success' && isList && !state.first503Sent) {
      state.first503Sent = true;
      entry.injected = '503';
      clientReq.resume();
      clientRes.writeHead(503, { 'content-type': 'application/xml' });
      clientRes.end('<Error><Code>SlowDown</Code></Error>');
      return;
    }
    if (state.active === 'ten-second-outage-recovers' && isList && Date.now() < state.outageUntil) {
      entry.injected = 'outage-503';
      clientReq.resume();
      clientRes.writeHead(503, { 'content-type': 'application/xml' });
      clientRes.end('<Error><Code>ServiceUnavailable</Code></Error>');
      return;
    }
    if (state.active === 'retry-after-honored' && isList && !state.retryAfterSent) {
      state.retryAfterSent = true;
      entry.injected = 'retry-after';
      clientReq.resume();
      clientRes.writeHead(503, { 'content-type': 'application/xml', 'retry-after': '1' });
      clientRes.end('<Error><Code>SlowDown</Code></Error>');
      return;
    }
    if (state.active === 'cancel-stops-new-dispatch' && isList) {
      entry.injected = 'cancel-window-503';
      clientReq.resume();
      clientRes.writeHead(503, { 'content-type': 'application/xml' });
      clientRes.end('<Error><Code>ServiceUnavailable</Code></Error>');
      return;
    }
    const upstream = httpRequest(
      {
        protocol: backend.protocol,
        hostname: backend.hostname,
        port: backend.port,
        method: clientReq.method,
        path: clientReq.url,
        headers: clientReq.headers,
      },
      (upstreamRes) => {
        if (state.active === 'body-stall-reloads-identity' && isGet && !state.bodyStallSent) {
          state.bodyStallSent = true;
          entry.injected = 'body-stall';
          clientRes.writeHead(upstreamRes.statusCode ?? 200, upstreamRes.headers);
          let sent = false;
          upstreamRes.on('data', (chunk) => {
            if (!sent) {
              sent = true;
              clientRes.write(chunk.subarray(0, Math.min(128, chunk.length)));
              entry.stall_started_ms = Date.now();
              setTimeout(() => {
                entry.stalled_ms = Date.now() - entry.stall_started_ms;
                clientRes.destroy(new Error('injected timed body stall'));
                upstream.destroy();
              }, state.bodyStallMs);
            }
          });
          upstreamRes.on('end', () => {
            if (!sent) clientRes.destroy(new Error('injected empty body stall'));
          });
          return;
        }
        clientRes.writeHead(upstreamRes.statusCode ?? 502, upstreamRes.headers);
        upstreamRes.pipe(clientRes);
      }
    );
    upstream.on('error', (error) => {
      entry.injected = entry.injected ?? `upstream-error:${error.code ?? error.name}`;
      if (!clientRes.headersSent) clientRes.writeHead(502);
      clientRes.end(String(error.message ?? error));
    });
    clientReq.pipe(upstream);
  });
  return {
    state,
    async listen() {
      await new Promise((ok, reject) => {
        server.once('error', reject);
        server.listen(0, '127.0.0.1', ok);
      });
      return `http://127.0.0.1:${server.address().port}`;
    },
    close() {
      return new Promise((ok) => server.close(ok));
    },
  };
}

function retryAfterDelayMs(error) {
  const headers = error?.$response?.headers ?? error?.$metadata?.headers ?? {};
  const raw = headers['retry-after'] ?? headers['Retry-After'];
  if (!raw) return 0;
  const seconds = Number(raw);
  if (Number.isFinite(seconds)) return Math.max(0, seconds * 1000);
  const date = Date.parse(raw);
  return Number.isFinite(date) ? Math.max(0, date - Date.now()) : 0;
}

async function retry(operation, { attempts = 8, signal, onDelay } = {}) {
  let last;
  for (let index = 0; index < attempts; index++) {
    if (signal?.aborted) throw signal.reason ?? new Error('aborted');
    try {
      return await operation(signal);
    } catch (error) {
      last = error;
      const status = error.$metadata?.httpStatusCode;
      const retryable = status === 503 || status === 502 || status === 500 || status === undefined;
      if (!retryable || index === attempts - 1) throw error;
      const delayMs = Math.max(retryAfterDelayMs(error), Math.min(1000, 100 * 2 ** index));
      onDelay?.(delayMs, error);
      await delay(delayMs, undefined, { signal });
    }
  }
  throw last;
}

async function streamBytes(body) {
  return Buffer.from(await body.transformToByteArray());
}

function sourceFingerprint() {
  return productionSourceFingerprint(repo);
}

async function loadBuildSnapshot(appBinary) {
  const explicit = process.env.FAULT_AUDIT_BUILD_JSON;
  const candidates = explicit
    ? [resolve(repo, explicit)]
    : [
        join(dirname(appBinary), 'build.json'),
        join(dirname(dirname(appBinary)), 'build.json'),
        join(dirname(dirname(dirname(appBinary))), 'build.json'),
      ];
  for (const candidate of candidates) {
    try {
      const bytes = await readFile(candidate);
      const data = JSON.parse(bytes.toString('utf8'));
      return { path: candidate, sha256: sha(bytes), data };
    } catch {}
  }
  throw new Error(
    'FAULT_AUDIT_BUILD_JSON or colocated build.json is required for app-mode evidence'
  );
}

function redactRequestUrl(url) {
  try {
    const parsed = new URL(url, 'http://127.0.0.1');
    for (const key of [...parsed.searchParams.keys()]) {
      if (/signature|credential|security-token|access-key|x-amz/i.test(key)) {
        parsed.searchParams.set(key, '[redacted]');
      }
    }
    return `${parsed.pathname}${parsed.search}`;
  } catch {
    return String(url).replace(
      /([?&][^=]*(?:signature|credential|token|key)[^=]*=)[^&]*/gi,
      '$1[redacted]'
    );
  }
}

async function waitFullOutage(startedAt, outageMs) {
  const remaining = startedAt + outageMs - Date.now();
  if (remaining > 0) await delay(remaining);
  return Date.now() - startedAt;
}

function validateBuildBinding(buildSnapshot, { appId, appBinarySha256, sourceFingerprintBefore }) {
  const data = buildSnapshot.data;
  assert.equal(data.app_id, appId, 'build.json app_id must match tested isolated app id');
  assert.equal(
    data.binary_sha256,
    appBinarySha256,
    'build.json binary_sha256 must match tested app binary'
  );
  const sourceHash = data.production_source_sha256 ?? data.source_sha256;
  assert(sourceHash, 'build.json must record production_source_sha256 or source_sha256');
  assert.equal(
    sourceHash,
    sourceFingerprintBefore.production_source_sha256,
    'build.json source hash must match current production source fingerprint including untracked inputs'
  );
}

async function cli(port, ...args) {
  const { stdout } = await run(
    'tauri-connector',
    ['--host', '127.0.0.1', '--port', String(port), ...args],
    { timeout: 65_000, maxBuffer: 4 * MiB }
  );
  return JSON.parse(stdout);
}

const ipc = (port, name, args) => cli(port, 'ipc', 'exec', name, '-a', JSON.stringify(args));

async function ownedConnectorPort(child, appId) {
  let sockets;
  try {
    ({ stdout: sockets } = await run(
      'lsof',
      ['-nP', '-a', '-p', String(child.pid), '-iTCP', '-sTCP:LISTEN', '-Fn'],
      { timeout: 5000 }
    ));
  } catch {
    return null;
  }
  for (const line of sockets.split('\n')) {
    const match = /^n127\.0\.0\.1:(\d+)$/.exec(line);
    if (!match) continue;
    const port = Number(match[1]);
    if (port < 9555 || port > 9655) continue;
    try {
      const state = await cli(port, 'state');
      if (state.app.identifier === appId && child.exitCode === null && child.signalCode === null) {
        return port;
      }
    } catch {}
  }
  return null;
}

async function startApp(appBinary, appId) {
  const child = spawn(appBinary, [], { cwd: repo, stdio: 'ignore' });
  for (let count = 0; count < 150; count++) {
    const port = await ownedConnectorPort(child, appId);
    if (port) return { child, port };
    assert.equal(child.exitCode, null, 'Audit application exited');
    await delay(200);
  }
  await stop(child);
  throw new Error('Isolated connector not ready');
}

async function waitTask(port, config, sourceKey, destKey, predicate, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs;
  let latest;
  while (Date.now() < deadline) {
    const tasks = await ipc(port, 'get_move_tasks', {
      sourceBucket: config.bucket,
      sourceAccountId: config.account_id,
    });
    latest = tasks.find((task) => task.source_key === sourceKey && task.dest_key === destKey);
    if (latest && predicate(latest)) return latest;
    await delay(250);
  }
  throw new Error(`Timed out waiting for task state; latest=${JSON.stringify(latest)}`);
}

async function waitFor(description, probe, { timeoutMs = 60_000, intervalMs = 500 } = {}) {
  const deadline = Date.now() + timeoutMs;
  let latest;
  while (Date.now() < deadline) {
    latest = await probe();
    if (latest) return latest;
    await delay(intervalMs);
  }
  throw new Error(`Timed out waiting for ${description}; latest=${JSON.stringify(latest)}`);
}

async function objectSha(client, key) {
  try {
    const result = await client.send(
      new (await import('@aws-sdk/client-s3')).GetObjectCommand({ Bucket: bucket, Key: key })
    );
    const bytes = Buffer.from(await result.Body.transformToByteArray());
    return { exists: true, sha256: sha(bytes), bytes: bytes.length };
  } catch {
    return { exists: false, sha256: null, bytes: 0 };
  }
}

async function mountInfo(port, mountId) {
  const mounts = await ipc(port, 'list_mounts', {});
  return mounts.find((mount) => mount.mount_id === mountId) ?? null;
}

function appDataCandidates(appId) {
  const home = homedir();
  return [
    join(home, 'Library', 'Application Support', appId),
    join(home, '.local', 'share', appId),
    join(home, 'AppData', 'Roaming', appId),
  ];
}

async function inspectIsolatedAppStorage(appId) {
  const candidates = appDataCandidates(appId);
  for (const root of candidates) {
    try {
      await access(root);
      const db = join(root, 'uploads-turso.db');
      let sqlite = null;
      try {
        const [{ stdout: tables }, { stdout: integrity }] = await Promise.all([
          run('sqlite3', [db, '.tables'], { timeout: 5000, maxBuffer: MiB }),
          run('sqlite3', [db, 'PRAGMA integrity_check;'], { timeout: 5000, maxBuffer: MiB }),
        ]);
        sqlite = {
          path: db,
          tables: tables.trim().split(/\s+/).filter(Boolean).sort(),
          integrity_check: integrity.trim(),
        };
      } catch (error) {
        sqlite = { path: db, error: error.message };
      }
      const stageRoot = join(root, 'mount-stage');
      let stages = [];
      try {
        stages = await readdir(stageRoot);
      } catch {}
      return { root, sqlite, mount_stage_entries: stages.sort() };
    } catch {}
  }
  return { root: null, candidates, sqlite: null, mount_stage_entries: [] };
}

async function appDataRoot(appId) {
  for (const root of appDataCandidates(appId)) {
    try {
      await access(root);
      return root;
    } catch {}
  }
  return null;
}

async function readMoveJournal(appId, taskId) {
  const root = await appDataRoot(appId);
  if (!root) return null;
  const db = join(root, 'uploads-turso.db');
  try {
    const { stdout } = await run(
      'sqlite3',
      [
        '-json',
        db,
        `SELECT data FROM move_journal WHERE task_id = '${taskId.replaceAll("'", "''")}'`,
      ],
      { timeout: 5000, maxBuffer: MiB }
    );
    const rows = JSON.parse(stdout || '[]');
    if (!rows[0]?.data) return null;
    return JSON.parse(rows[0].data);
  } catch {
    return null;
  }
}

function journalHasDurableCheckpoint(journal) {
  return !!(
    journal &&
    journal.source &&
    typeof journal.source.etag === 'string' &&
    journal.stage &&
    ['transferring', 'copied', 'delete_pending', 'delete_unknown', 'outcome_unknown'].includes(
      journal.stage
    )
  );
}

async function runNfsOutageScenarios({
  app,
  appBinary,
  appId,
  credentials,
  proxy,
  proxyEndpoint,
  sourceClient,
  profile,
  outageMs,
  root,
  owner,
}) {
  const result = {
    status: 'running',
    outage_ms: outageMs,
    source_expected_remote_state: {},
  };
  let mountId;
  let recoveryMountId;
  try {
    const content = Buffer.alloc(3 * MiB);
    for (let index = 0; index < content.length; index++) content[index] = (index * 13 + 41) % 251;
    const expectedSha = sha(content);
    const mountPath = join(root, 'nfs-outage-mount');
    await mkdir(mountPath, { recursive: true });
    const nfsKey = `nfs-outage-${profile}-${owner}.bin`;
    const mounted = await ipc(app.port, 'mount_bucket', {
      input: {
        provider: 'rustfs',
        account_id: `fault-nfs-${owner}`,
        bucket,
        local_path: mountPath,
        access_key_id: credentials.accessKeyId,
        secret_access_key: credentials.secretAccessKey,
        endpoint_url: proxyEndpoint,
        force_path_style: true,
        read_only: false,
        max_staging_bytes: 128 * MiB,
      },
    });
    mountId = mounted.mount_id;
    proxy.state.active = 'app-nfs-outage-retains-stage';
    const nfsOutageStarted = Date.now();
    proxy.state.failAllUntil = nfsOutageStarted + outageMs;
    await writeFile(join(mountPath, nfsKey), content);
    assert.equal(
      sha(await readFile(join(mountPath, nfsKey))),
      expectedSha,
      'NFS write ACK did not round-trip staged bytes'
    );
    const dirty = await waitFor(
      'NFS dirty stage to be visible',
      async () => {
        const info = await mountInfo(app.port, mountId);
        return info && (info.pending_uploads > 0 || info.dirty_bytes >= content.length)
          ? info
          : null;
      },
      { timeoutMs: Math.min(Math.max(outageMs, 10_000), 60_000), intervalMs: 500 }
    );
    const duringOutage = await objectSha(sourceClient, nfsKey);
    assert.equal(
      duringOutage.exists,
      false,
      'remote object appeared while source outage was active'
    );
    result.source_expected_remote_state[nfsKey] = {
      during_outage: 'absent',
      dirty_bytes: dirty.dirty_bytes,
      pending_uploads: dirty.pending_uploads,
      oldest_dirty_ms: dirty.oldest_dirty_ms,
    };
    result.source_expected_remote_state[nfsKey].actual_outage_elapsed_ms = await waitFullOutage(
      nfsOutageStarted,
      outageMs
    );
    proxy.state.failAllUntil = 0;
    const published = await waitFor(
      'NFS staged upload to publish after source restoration',
      async () => {
        const remote = await objectSha(sourceClient, nfsKey);
        return remote.exists && remote.sha256 === expectedSha ? remote : null;
      },
      { timeoutMs: 90_000, intervalMs: 1000 }
    );
    result.source_expected_remote_state[nfsKey].after_restoration = 'published';
    result.source_expected_remote_state[nfsKey].remote_sha256 = published.sha256;
    await ipc(app.port, 'unmount_bucket', { mountId });
    mountId = undefined;

    const recoveryContent = Buffer.alloc(2 * MiB);
    for (let index = 0; index < recoveryContent.length; index++) {
      recoveryContent[index] = (index * 19 + 7) % 251;
    }
    const recoverySha = sha(recoveryContent);
    const recoveryKey = `nfs-recovery-${profile}-${owner}.bin`;
    const interruptedMountPath = join(root, 'nfs-interrupted-mount');
    await mkdir(interruptedMountPath, { recursive: true });
    const interrupted = await ipc(app.port, 'mount_bucket', {
      input: {
        provider: 'rustfs',
        account_id: `fault-nfs-${owner}`,
        bucket,
        local_path: interruptedMountPath,
        access_key_id: credentials.accessKeyId,
        secret_access_key: credentials.secretAccessKey,
        endpoint_url: proxyEndpoint,
        force_path_style: true,
        read_only: false,
        max_staging_bytes: 128 * MiB,
      },
    });
    mountId = interrupted.mount_id;
    proxy.state.active = 'app-nfs-stop-retains-recovery';
    const nfsStopOutageStarted = Date.now();
    proxy.state.failAllUntil = nfsStopOutageStarted + outageMs;
    await writeFile(join(interruptedMountPath, recoveryKey), recoveryContent);
    assert.equal(sha(await readFile(join(interruptedMountPath, recoveryKey))), recoverySha);
    await waitFor(
      'interrupted NFS dirty stage to be visible',
      async () => {
        const info = await mountInfo(app.port, mountId);
        return info && (info.pending_uploads > 0 || info.dirty_bytes >= recoveryContent.length)
          ? info
          : null;
      },
      { timeoutMs: Math.min(Math.max(outageMs, 10_000), 60_000), intervalMs: 500 }
    );
    const stopSettledAt = proxy.state.logs.length;
    result.stop_retained_recovery_outage_elapsed_ms = await waitFullOutage(
      nfsStopOutageStarted,
      outageMs
    );
    await stop(app.child);
    mountId = undefined;
    await delay(1500);
    const afterStopSettled = proxy.state.logs.length;
    await delay(1500);
    const dispatchesAfterStopSettled = proxy.state.logs.length - afterStopSettled;
    const storage = await inspectIsolatedAppStorage(appId);
    result.stop_retained_recovery = {
      proxy_requests_before_stop: stopSettledAt,
      proxy_requests_while_stop_settled: afterStopSettled - stopSettledAt,
      proxy_requests_after_stop_settled: dispatchesAfterStopSettled,
      app_storage: storage,
    };
    assert.equal(
      dispatchesAfterStopSettled,
      0,
      'proxy saw new source requests after stopped app settled'
    );

    app = await startApp(appBinary, appId);
    proxy.state.failAllUntil = 0;
    const recoveries = await ipc(app.port, 'list_mount_recoveries', {});
    result.stop_retained_recovery.recoveries_after_restart = recoveries;
    assert(recoveries.length > 0, 'expected retained NFS recovery after interrupted app stop');
    const recovery = recoveries[0];
    const recoveryMountPath = join(root, 'nfs-recovery-mount');
    await mkdir(recoveryMountPath, { recursive: true });
    const recoveredMount = await ipc(app.port, 'mount_bucket', {
      input: {
        provider: 'rustfs',
        account_id: `fault-nfs-${owner}`,
        bucket,
        local_path: recoveryMountPath,
        access_key_id: credentials.accessKeyId,
        secret_access_key: credentials.secretAccessKey,
        endpoint_url: proxyEndpoint,
        force_path_style: true,
        read_only: false,
        recovery_id: recovery.recovery_id,
        max_staging_bytes: 128 * MiB,
      },
    });
    recoveryMountId = recoveredMount.mount_id;
    const recovered = await waitFor(
      'retained NFS recovery to publish after remount',
      async () => {
        const remote = await objectSha(sourceClient, recoveryKey);
        return remote.exists && remote.sha256 === recoverySha ? remote : null;
      },
      { timeoutMs: 90_000, intervalMs: 1000 }
    );
    await ipc(app.port, 'unmount_bucket', { mountId: recoveryMountId });
    recoveryMountId = undefined;
    result.recovery_key = recoveryKey;
    result.recovery_sha256 = recovered.sha256;
    result.status = 'passed';
    return { app, scenario: result };
  } catch (error) {
    result.status = 'failed';
    result.failure = { name: error.name, message: error.message };
    throw Object.assign(error, { scenario: result, app });
  } finally {
    if (recoveryMountId) {
      try {
        await ipc(app.port, 'unmount_bucket', { mountId: recoveryMountId });
      } catch {}
    }
    if (mountId) {
      try {
        await ipc(app.port, 'unmount_bucket', { mountId });
      } catch {}
    }
  }
}

async function executeApp(config) {
  assert.equal(
    config.providerKind,
    'rustfs',
    'app task fault mode currently requires RustFS provider kind and create_rustfs_account IPC'
  );
  const sdk = await import('@aws-sdk/client-s3');
  const appBinary = resolve(repo, process.env.FAULT_AUDIT_APP_BINARY ?? '');
  const appId = process.env.FAULT_AUDIT_APP_ID;
  assert(appId?.startsWith('com.lifefarmer.r2.audit-'), 'isolated app id required');
  await access(appBinary);
  const appBytes = await readFile(appBinary);
  const appBinarySha256 = sha(appBytes);
  const buildSnapshot = await loadBuildSnapshot(appBinary);
  const sourceFingerprintBefore = await sourceFingerprint();
  validateBuildBinding(buildSnapshot, { appId, appBinarySha256, sourceFingerprintBefore });
  assert(appBytes.includes(Buffer.from(appId)), 'isolated app id absent from binary');
  if (process.env.FAULT_AUDIT_APP_SHA256) {
    assert.equal(appBinarySha256, process.env.FAULT_AUDIT_APP_SHA256);
  }
  const profile = process.env.FAULT_AUDIT_PROFILE ?? 'smoke';
  assert(
    ['smoke', 'acceptance'].includes(profile),
    'FAULT_AUDIT_PROFILE must be smoke or acceptance'
  );
  const outageMs = Number(
    process.env.FAULT_AUDIT_APP_OUTAGE_MS ?? (profile === 'acceptance' ? 5 * 60_000 : 10_000)
  );
  assert(Number.isInteger(outageMs) && outageMs >= 1000 && outageMs <= 310_000);
  const root = await mkdtemp(join(tmpdir(), 'r2-app-fault-'));
  const credentials = {
    accessKeyId: `r2-app-fault-${randomBytes(6).toString('hex')}`,
    secretAccessKey: randomBytes(24).toString('hex'),
  };
  let source;
  let destination;
  let sourceProxy;
  let destinationProxy;
  let app;
  const owner = randomUUID();
  const output = resolve(
    process.env.FAULT_AUDIT_OUTPUT || `.omx/artifacts/provider-fault/app-${profile}-${owner}.json`
  );
  const report = {
    status: 'running',
    captured_at: new Date().toISOString(),
    owner,
    profile,
    outage_ms: outageMs,
    source_fingerprint_before: sourceFingerprintBefore,
    built_source_snapshot: buildSnapshot,
    app_id: appId,
    app_binary_sha256_before: appBinarySha256,
    provider_kind: config.providerKind,
    provider_binary_sha256: await fileSha(config.providerBinary),
    harness_sha256: await fileSha(import.meta.filename),
    scenarios: {},
  };
  await mkdir(dirname(output), { recursive: true });
  const persist = () => writeFile(output, JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
  await persist();
  try {
    source = await startProvider(config, join(root, 'source'), credentials);
    destination = await startProvider(config, join(root, 'destination'), credentials);
    sourceProxy = startProxy(source.endpoint);
    destinationProxy = startProxy(destination.endpoint);
    const sourceProxyEndpoint = await sourceProxy.listen();
    const destinationProxyEndpoint = await destinationProxy.listen();
    const sourceClient = new sdk.S3Client({
      endpoint: source.endpoint,
      credentials,
      forcePathStyle: true,
      region: 'us-east-1',
      maxAttempts: 1,
    });
    const destinationClient = new sdk.S3Client({
      endpoint: destination.endpoint,
      credentials,
      forcePathStyle: true,
      region: 'us-east-1',
      maxAttempts: 1,
    });
    for (const client of [sourceClient, destinationClient]) {
      await client.send(new sdk.CreateBucketCommand({ Bucket: bucket }));
    }
    app = await startApp(appBinary, appId);
    const makeConfig = async (role, endpoint) => {
      const input = {
        name: `Fault ${role}`,
        access_key_id: credentials.accessKeyId,
        secret_access_key: credentials.secretAccessKey,
        endpoint_scheme: 'http',
        endpoint_host: new URL(endpoint).host,
        force_path_style: true,
      };
      const account = await ipc(app.port, 'create_rustfs_account', { input });
      return {
        ...input,
        provider: 'rustfs',
        account_id: account.id,
        bucket,
        region: 'us-east-1',
      };
    };
    const sourceConfig = await makeConfig('source', sourceProxyEndpoint);
    const destConfig = await makeConfig('destination', destinationProxyEndpoint);
    const content = Buffer.alloc(16 * MiB);
    for (let index = 0; index < content.length; index++) content[index] = (index * 17 + 3) % 251;
    const sourceKey = `app-fault/${profile}-source.bin`;
    const destKey = `app-fault/${profile}-dest.bin`;
    await sourceClient.send(
      new sdk.PutObjectCommand({ Bucket: bucket, Key: sourceKey, Body: content })
    );
    await ipc(app.port, 'start_batch_move', {
      sourceConfig,
      destConfig,
      operations: [{ source_key: sourceKey, dest_key: destKey, overwrite: false }],
      deleteOriginal: true,
    });
    const checkpoint = await waitFor(
      'durable move journal checkpoint before source outage',
      async () => {
        const tasks = await ipc(app.port, 'get_move_tasks', {
          sourceBucket: sourceConfig.bucket,
          sourceAccountId: sourceConfig.account_id,
        });
        const task = tasks.find(
          (item) => item.source_key === sourceKey && item.dest_key === destKey
        );
        if (!task?.id) return null;
        const journal = await readMoveJournal(appId, task.id);
        return journalHasDurableCheckpoint(journal) ? { task, journal } : null;
      },
      { timeoutMs: 60_000, intervalMs: 500 }
    );
    sourceProxy.state.active = 'app-move-outage-restart-resume';
    const moveOutageStarted = Date.now();
    sourceProxy.state.failAllUntil = moveOutageStarted + outageMs;
    const failed = await waitTask(
      app.port,
      sourceConfig,
      sourceKey,
      destKey,
      (task) =>
        task.id === checkpoint.task.id &&
        (['error', 'paused', 'cancelled', 'outcome_unknown', 'needs_action'].includes(
          task.status
        ) ||
          /retry scheduled|transient|ServiceUnavailable|unavailable/i.test(task.error ?? '')),
      Math.max(30_000, outageMs + 30_000)
    );
    const sourceRetainedDuringOutage = await sourceClient
      .send(new sdk.HeadObjectCommand({ Bucket: bucket, Key: sourceKey }))
      .then(
        () => true,
        () => false
      );
    assert(sourceRetainedDuringOutage, 'source object was not retained during outage');
    report.scenarios['app-move-outage-restart-resume'] = {
      status: 'interrupted',
      interrupted_status: failed.status,
      interrupted_error: failed.error ?? null,
      source_retained_during_outage: sourceRetainedDuringOutage,
      retry_metadata_visible: /retry scheduled|transient|ServiceUnavailable|unavailable/i.test(
        failed.error ?? ''
      ),
      checkpoint_before_outage: checkpoint.journal,
      retry_journal_directly_observed: true,
      retry_journal_observation_gap: null,
    };
    const firstPid = app.child.pid;
    await stop(app.child);
    app = await startApp(appBinary, appId);
    assert.notEqual(app.child.pid, firstPid, 'restart did not create a new app process');
    report.scenarios['app-move-outage-restart-resume'].actual_outage_elapsed_ms =
      await waitFullOutage(moveOutageStarted, outageMs);
    sourceProxy.state.failAllUntil = 0;
    await ipc(app.port, 'resume_move', {
      taskId: failed.id,
      sourceConfig,
      destConfig,
    });
    const finished = await waitTask(
      app.port,
      sourceConfig,
      sourceKey,
      destKey,
      (task) => ['success', 'needs_action', 'error', 'outcome_unknown'].includes(task.status),
      90_000
    );
    const destBytes = await destinationClient
      .send(new sdk.GetObjectCommand({ Bucket: bucket, Key: destKey }))
      .then((result) => result.Body.transformToByteArray())
      .then(
        (bytes) => Buffer.from(bytes),
        () => null
      );
    assert(
      destBytes && sha(destBytes) === sha(content),
      'destination did not recover expected bytes'
    );
    report.scenarios['app-move-outage-restart-resume'] = {
      ...report.scenarios['app-move-outage-restart-resume'],
      status: finished.status === 'success' ? 'passed' : 'failed',
      after_restart_pid_changed: true,
      final_status: finished.status,
      destination_sha256: sha(destBytes),
      source_retained_after_resume: await sourceClient
        .send(new sdk.HeadObjectCommand({ Bucket: bucket, Key: sourceKey }))
        .then(
          () => true,
          () => false
        ),
    };
    const cancelSource = `app-fault/${profile}-cancel-source.bin`;
    const cancelDest = `app-fault/${profile}-cancel-dest.bin`;
    await sourceClient.send(
      new sdk.PutObjectCommand({ Bucket: bucket, Key: cancelSource, Body: content })
    );
    sourceProxy.state.active = 'app-cancel-no-new-requests';
    destinationProxy.state.active = 'app-cancel-no-new-requests';
    sourceProxy.state.failAllUntil = Date.now() + outageMs;
    destinationProxy.state.failAllUntil = sourceProxy.state.failAllUntil;
    const beforeCancelSourceLogs = sourceProxy.state.logs.length;
    const beforeCancelDestLogs = destinationProxy.state.logs.length;
    await ipc(app.port, 'start_batch_move', {
      sourceConfig,
      destConfig,
      operations: [{ source_key: cancelSource, dest_key: cancelDest, overwrite: false }],
      deleteOriginal: true,
    });
    const cancelTask = await waitTask(
      app.port,
      sourceConfig,
      cancelSource,
      cancelDest,
      (task) => !!task.id,
      20_000
    );
    await ipc(app.port, 'cancel_move', { taskId: cancelTask.id });
    const atCancelSourceLogs = sourceProxy.state.logs.length;
    const atCancelDestLogs = destinationProxy.state.logs.length;
    await delay(3000);
    const sourceAfterCancel = sourceProxy.state.logs.length - atCancelSourceLogs;
    const destAfterCancel = destinationProxy.state.logs.length - atCancelDestLogs;
    report.scenarios['app-cancel-no-new-requests'] = {
      status: sourceAfterCancel === 0 && destAfterCancel === 0 ? 'passed' : 'failed',
      task_id: cancelTask.id,
      dispatches_before_cancel_source: atCancelSourceLogs - beforeCancelSourceLogs,
      dispatches_before_cancel_destination: atCancelDestLogs - beforeCancelDestLogs,
      dispatches_after_cancel_source: sourceAfterCancel,
      dispatches_after_cancel_destination: destAfterCancel,
      all_endpoints_observed: true,
    };
    if (process.env.FAULT_AUDIT_APP_NFS === '0') {
      report.scenarios['app-nfs-outage-retains-stage'] = {
        status: 'skipped',
        reason: 'FAULT_AUDIT_APP_NFS=0 explicitly disabled the native mount outage scenario',
      };
    } else {
      try {
        const nfs = await runNfsOutageScenarios({
          app,
          appBinary,
          appId,
          credentials,
          proxy: sourceProxy,
          proxyEndpoint: sourceProxyEndpoint,
          sourceClient,
          profile,
          outageMs,
          root,
          owner,
        });
        app = nfs.app;
        report.scenarios['app-nfs-outage-retains-stage'] = nfs.scenario;
      } catch (error) {
        if (error.app) app = error.app;
        report.scenarios['app-nfs-outage-retains-stage'] = error.scenario ?? {
          status: 'failed',
          failure: { name: error.name, message: error.message },
        };
      }
    }
    report.proxy_requests = {
      source: sourceProxy.state.logs,
      destination: destinationProxy.state.logs,
    };
    report.source_fingerprint_after = await sourceFingerprint();
    report.app_binary_sha256_after = await fileSha(appBinary);
    report.provenance_immutable =
      report.app_binary_sha256_after === report.app_binary_sha256_before &&
      report.source_fingerprint_after.production_source_sha256 ===
        report.source_fingerprint_before.production_source_sha256;
    const moveAndCancelPassed = [
      report.scenarios['app-move-outage-restart-resume'],
      report.scenarios['app-cancel-no-new-requests'],
    ].every((scenario) => scenario?.status === 'passed');
    const nfsStatus = report.scenarios['app-nfs-outage-retains-stage']?.status;
    const durableSchedulerObserved =
      report.scenarios['app-move-outage-restart-resume']?.retry_journal_directly_observed === true;
    const allEndpointCancelObserved =
      report.scenarios['app-cancel-no-new-requests']?.all_endpoints_observed === true;
    const allAppScenariosPassed =
      moveAndCancelPassed &&
      nfsStatus === 'passed' &&
      durableSchedulerObserved &&
      allEndpointCancelObserved &&
      report.provenance_immutable === true &&
      profile === 'acceptance';
    report.status = allAppScenariosPassed ? 'observations_collected' : 'failed';
    report.acceptance_closed = allAppScenariosPassed;
    report.incomplete_scenarios = Object.fromEntries(
      Object.entries(report.scenarios).filter(([, scenario]) =>
        ['skipped', 'not_implemented', 'incomplete'].includes(scenario.status)
      )
    );
    if (!moveAndCancelPassed || nfsStatus !== 'passed') process.exitCode = 1;
  } catch (error) {
    report.status = 'failed';
    report.failure = { name: error.name, message: error.message };
    process.exitCode = 1;
  } finally {
    await persist();
    if (app) await stop(app.child);
    if (sourceProxy) await sourceProxy.close();
    if (destinationProxy) await destinationProxy.close();
    if (source) await stop(source.child);
    if (destination) await stop(destination.child);
    await rm(root, { recursive: true, force: true });
  }
  console.log(JSON.stringify(report, null, 2));
}

async function execute(config) {
  const sdk = await import('@aws-sdk/client-s3');
  const root = await mkdtemp(join(tmpdir(), 'r2-provider-fault-'));
  const credentials = {
    accessKeyId: `r2-fault-${randomBytes(6).toString('hex')}`,
    secretAccessKey: randomBytes(24).toString('hex'),
  };
  let provider;
  let proxy;
  const owner = randomUUID();
  const bucket = 'fault-audit';
  const output = resolve(
    process.env.FAULT_AUDIT_OUTPUT || `.omx/artifacts/provider-fault/${owner}.json`
  );
  const report = {
    status: 'running',
    captured_at: new Date().toISOString(),
    owner,
    provider_kind: config.providerKind,
    provider_binary_sha256: await fileSha(config.providerBinary),
    harness_sha256: await fileSha(import.meta.filename),
    short_outage_ms: config.shortOutageMs,
    scenarios: {},
  };
  await mkdir(dirname(output), { recursive: true });
  const persist = () => writeFile(output, JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
  await persist();
  try {
    provider = await startProvider(config, root, credentials);
    proxy = startProxy(provider.endpoint);
    const proxyEndpoint = await proxy.listen();
    const client = new sdk.S3Client({
      endpoint: proxyEndpoint,
      credentials,
      forcePathStyle: true,
      region: 'us-east-1',
      maxAttempts: 1,
    });
    await client.send(new sdk.CreateBucketCommand({ Bucket: bucket }));
    const body = Buffer.alloc(2 * MiB);
    for (let index = 0; index < body.length; index++) body[index] = (index * 29 + 11) % 251;
    const fixture = await client.send(
      new sdk.PutObjectCommand({ Bucket: bucket, Key: 'fixture.bin', Body: body })
    );
    const list = () => client.send(new sdk.ListObjectsV2Command({ Bucket: bucket, MaxKeys: 2 }));
    proxy.state.active = 'first-list-503-then-success';
    await retry(list);
    report.scenarios['first-list-503-then-success'] = {
      status: 'passed',
      injected_requests: proxy.state.logs.filter((entry) => entry.injected === '503').length,
      total_matching_requests: proxy.state.logs.filter(
        (entry) => entry.scenario === 'first-list-503-then-success'
      ).length,
    };
    proxy.state.active = 'ten-second-outage-recovers';
    proxy.state.outageUntil = Date.now() + config.shortOutageMs;
    const outageStarted = Date.now();
    await retry(list, { attempts: 20 });
    const outageElapsedMs = Date.now() - outageStarted;
    const outageInjected = proxy.state.logs.filter(
      (entry) => entry.injected === 'outage-503'
    ).length;
    report.scenarios['ten-second-outage-recovers'] = {
      status: fullOutageClaimPasses({
        configuredMs: config.shortOutageMs,
        elapsedMs: outageElapsedMs,
        restoredAfterFailure: outageInjected > 0,
      })
        ? 'passed'
        : 'failed',
      outage_ms: config.shortOutageMs,
      elapsed_ms: outageElapsedMs,
      injected_requests: outageInjected,
    };
    proxy.state.active = 'retry-after-honored';
    const delays = [];
    await retry(list, { onDelay: (ms) => delays.push(ms) });
    const retryAfterResponses = proxy.state.logs.filter(
      (entry) => entry.injected === 'retry-after'
    ).length;
    report.scenarios['retry-after-honored'] = {
      status: retryAfterClaimPasses({ retryAfterMs: 1000, observedDelaysMs: delays })
        ? 'passed'
        : 'failed',
      reason: retryAfterClaimPasses({ retryAfterMs: 1000, observedDelaysMs: delays })
        ? undefined
        : 'retry helper did not wait at least the advertised Retry-After duration',
      retry_after_responses: retryAfterResponses,
      advertised_retry_after_ms: 1000,
      harness_backoff_ms: delays,
    };
    proxy.state.active = 'body-stall-reloads-identity';
    const read = async () => {
      const result = await client.send(
        new sdk.GetObjectCommand({ Bucket: bucket, Key: 'fixture.bin', IfMatch: fixture.ETag })
      );
      return streamBytes(result.Body);
    };
    const recovered = await retry(read, { attempts: 4 });
    assert.equal(sha(recovered), sha(body));
    const bodyStallEntries = proxy.state.logs.filter((entry) => entry.injected === 'body-stall');
    const maxStalledMs = Math.max(0, ...bodyStallEntries.map((entry) => entry.stalled_ms ?? 0));
    const bodyStallPassed = timedBodyStallClaimPasses({
      stalledMs: maxStalledMs,
      minimumStallMs: proxy.state.bodyStallMs,
      recoveredIdentity: sha(recovered) === sha(body),
    });
    report.scenarios['body-stall-reloads-identity'] = {
      status: bodyStallPassed ? 'passed' : 'failed',
      reason: bodyStallPassed ? undefined : 'timed body stall was not observed before recovery',
      injected_stalls: bodyStallEntries.length,
      minimum_stall_ms: proxy.state.bodyStallMs,
      observed_stall_ms: maxStalledMs,
      sha256: sha(recovered),
      source_etag: fixture.ETag,
    };
    proxy.state.active = 'cancel-stops-new-dispatch';
    const controller = new AbortController();
    setTimeout(() => controller.abort(new Error('audit cancellation')), 200);
    const before = proxy.state.logs.length;
    let cancelled = false;
    try {
      await retry(list, { attempts: 20, signal: controller.signal });
    } catch (error) {
      cancelled = /audit cancellation|aborted|Abort/.test(String(error.message ?? error));
    }
    const atCancel = proxy.state.logs.length;
    await delay(500);
    report.scenarios['cancel-stops-new-dispatch'] = {
      status: cancelled && proxy.state.logs.length === atCancel ? 'passed' : 'failed',
      cancelled,
      dispatches_before_cancel_window: atCancel - before,
      dispatches_after_cancel_window: proxy.state.logs.length - atCancel,
    };
    report.scenarios['five-minute-outage-persists'] = {
      status: 'incomplete',
      reason:
        'requires current app task runner and persistent retry-state assertions; this proxy-only harness does not close it',
    };
    report.proxy_requests = proxy.state.logs;
    const scenarioStatuses = Object.values(report.scenarios).map((scenario) => scenario.status);
    report.status = scenarioStatuses.every((status) => status === 'passed')
      ? 'observations_collected'
      : 'failed';
    if (report.status === 'failed') process.exitCode = 1;
  } catch (error) {
    report.status = 'failed';
    report.failure = { name: error.name, message: error.message };
    process.exitCode = 1;
  } finally {
    await persist();
    if (proxy) await proxy.close();
    if (provider) await stop(provider.child);
    await rm(root, { recursive: true, force: true });
  }
  console.log(JSON.stringify(report, null, 2));
}

export function retryAfterClaimPasses({ retryAfterMs, observedDelaysMs }) {
  return observedDelaysMs.some((delayMs) => delayMs >= retryAfterMs);
}

export function responseLossClaimPasses({ injectedResponseDrop, reconciled }) {
  return injectedResponseDrop === true && reconciled === true;
}

export function timedBodyStallClaimPasses({ stalledMs, minimumStallMs, recoveredIdentity }) {
  return stalledMs >= minimumStallMs && recoveredIdentity === true;
}

export function fullOutageClaimPasses({ configuredMs, elapsedMs, restoredAfterFailure }) {
  return elapsedMs >= configuredMs && restoredAfterFailure === true;
}

if (mode === '--plan') {
  console.log(JSON.stringify(PLAN, null, 2));
} else if (mode === '--self-test') {
  const valid = {
    providerBinary: '.omx/artifacts/rustfs-runtime/extracted/rustfs',
    providerKind: 'rustfs',
    shortOutageMs: '1000',
  };
  validateFaultConfig(valid);
  for (const change of [{ providerBinary: '' }, { providerKind: 'aws' }, { shortOutageMs: '0' }]) {
    assert.throws(() => validateFaultConfig({ ...valid, ...change }));
  }
  assert.equal(SCENARIOS.length, 6);
  assert.equal(retryAfterClaimPasses({ retryAfterMs: 1000, observedDelaysMs: [100] }), false);
  assert.equal(responseLossClaimPasses({ injectedResponseDrop: false, reconciled: true }), false);
  assert.equal(responseLossClaimPasses({ injectedResponseDrop: true, reconciled: true }), true);
  assert.equal(
    timedBodyStallClaimPasses({ stalledMs: 0, minimumStallMs: 1000, recoveredIdentity: true }),
    false
  );
  assert.equal(
    timedBodyStallClaimPasses({ stalledMs: 1000, minimumStallMs: 1000, recoveredIdentity: true }),
    true
  );
  assert.equal(
    fullOutageClaimPasses({ configuredMs: 300_000, elapsedMs: 10_000, restoredAfterFailure: true }),
    false
  );
  assert.equal(
    journalHasDurableCheckpoint({
      stage: 'transferring',
      source: { etag: '"abc"', size: 1, version_id: 'v1' },
    }),
    true
  );
  assert.equal(journalHasDurableCheckpoint({ stage: 'transferring' }), false);
  assert(MODES.has('--execute-app'));
  assert(PLAN.app_task_scenarios.includes('app-move-outage-restart-resume'));
  console.log('Offline provider fault harness guard checks passed. No runtimes launched.');
} else {
  const defaultRustfs = resolve(repo, '.omx/artifacts/rustfs-runtime/extracted/rustfs');
  const defaultMinio = resolve(repo, '.omx/artifacts/minio-runtime/bin/minio');
  const providerKind =
    process.env.FAULT_AUDIT_PROVIDER_KIND ??
    (await access(defaultRustfs).then(
      () => 'rustfs',
      () => 'minio'
    ));
  const providerBinary =
    process.env.FAULT_AUDIT_PROVIDER_BINARY ??
    (providerKind === 'rustfs' ? defaultRustfs : defaultMinio);
  const config = validateFaultConfig({
    providerBinary,
    providerKind,
    shortOutageMs: process.env.FAULT_AUDIT_SHORT_OUTAGE_MS,
  });
  if (mode === '--execute-app') await executeApp(config);
  else await execute(config);
}
