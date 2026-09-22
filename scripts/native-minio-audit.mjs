/** Opt-in integration: two real, disposable loopback MinIO servers + isolated Tauri app.
 * Run with the audit-profile connector binary built and MINIO_AUDIT_BINARY set.
 * Never accepts existing endpoints, buckets, credentials, or the normal app profile.
 */
import assert from 'node:assert/strict';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createHash, randomBytes } from 'node:crypto';
import { mkdtemp, mkdir, readFile, writeFile, rename, unlink, rm } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, resolve, join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import {
  S3Client,
  CreateBucketCommand,
  PutObjectCommand,
  GetObjectCommand,
  HeadObjectCommand,
  PutBucketVersioningCommand,
} from '@aws-sdk/client-s3';
import { productionSourceFingerprint } from './audit-source.mjs';

const run = promisify(execFile);
const repo = resolve(import.meta.dirname, '..');
const appId = process.env.MINIO_AUDIT_APP_ID;
const binary = process.env.MINIO_AUDIT_APP_BINARY
  ? resolve(repo, process.env.MINIO_AUDIT_APP_BINARY)
  : '';
const expectedAppSha256 = process.env.MINIO_AUDIT_APP_SHA256;
const explicitBuildJson = process.env.MINIO_AUDIT_BUILD_JSON;
const minio = process.env.MINIO_AUDIT_BINARY;
assert(minio, 'MINIO_AUDIT_BINARY is required');
assert(binary, 'MINIO_AUDIT_APP_BINARY is required');
assert(
  appId?.startsWith('com.lifefarmer.r2.audit-'),
  'MINIO_AUDIT_APP_ID must be an isolated audit app id'
);
const root = await mkdtemp(join(tmpdir(), 'r2-real-minio-'));
const auditPrefix = `minio-audit/${randomBytes(8).toString('hex')}/`;
const credentials = { accessKeyId: 'r2-audit', secretAccessKey: randomBytes(24).toString('hex') };
const children = [];
let app;
let port;
let mountId;
let mountDetached = true;
const digest = (bytes) => createHash('sha256').update(bytes).digest('hex');
async function fileSha(path) {
  return digest(await readFile(path));
}
async function loadBuildSnapshot(appBinary) {
  const candidates = explicitBuildJson
    ? [resolve(repo, explicitBuildJson)]
    : [
        join(dirname(appBinary), 'build.json'),
        join(dirname(dirname(appBinary)), 'build.json'),
        join(dirname(dirname(dirname(appBinary))), 'build.json'),
      ];
  for (const candidate of candidates) {
    try {
      const bytes = await readFile(candidate);
      return { path: candidate, sha256: digest(bytes), data: JSON.parse(bytes.toString('utf8')) };
    } catch {}
  }
  throw new Error('MINIO_AUDIT_BUILD_JSON or colocated build.json is required');
}
function validateBuildBinding(buildSnapshot, { appBinarySha256, sourceFingerprint }) {
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
    sourceFingerprint.production_source_sha256,
    'build.json source hash must match current production source fingerprint'
  );
}

async function freePort() {
  const server = createServer();
  await new Promise((ok, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', ok);
  });
  const value = server.address().port;
  await new Promise((ok) => server.close(ok));
  return value;
}
async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill('SIGTERM');
  for (let count = 0; count < 50 && child.exitCode === null && child.signalCode === null; count++)
    await delay(100);
  if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
  if (child.exitCode === null && child.signalCode === null)
    await new Promise((ok) => child.once('exit', ok));
}
async function cli(...args) {
  const { stdout } = await run(
    'tauri-connector',
    ['--host', '127.0.0.1', '--port', String(port), ...args],
    { timeout: 65_000, maxBuffer: 4 * 1024 * 1024 }
  );
  return JSON.parse(stdout);
}
const ipc = (name, args) => cli('ipc', 'exec', name, '-a', JSON.stringify(args));
async function bytes(client, key) {
  const result = await client.send(new GetObjectCommand({ Bucket: 'photos', Key: key }));
  return result.Body.transformToByteArray();
}
async function missing(client, key) {
  try {
    await client.send(new HeadObjectCommand({ Bucket: 'photos', Key: key }));
    return false;
  } catch (error) {
    if (error.$metadata?.httpStatusCode === 404) return true;
    throw error;
  }
}
async function ownedConnectorPort(child, expectedAppId) {
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
    const candidate = Number(match[1]);
    if (candidate < 9555 || candidate > 9655) continue;
    try {
      const { stdout } = await run(
        'tauri-connector',
        ['--host', '127.0.0.1', '--port', String(candidate), 'state'],
        { timeout: 2000, maxBuffer: 1024 * 1024 }
      );
      const state = JSON.parse(stdout);
      if (
        state.app.identifier === expectedAppId &&
        child.exitCode === null &&
        child.signalCode === null
      ) {
        return candidate;
      }
    } catch {}
  }
  return null;
}

async function waitTask(config, sourceKey, destKey, expectedStatus) {
  for (let count = 0; count < 600; count++) {
    const tasks = await ipc('get_move_tasks', {
      sourceBucket: 'photos',
      sourceAccountId: config.account_id,
    });
    const task = tasks.find((item) => item.source_key === sourceKey && item.dest_key === destKey);
    if (task?.status === expectedStatus) return task;
    if (
      task &&
      ['error', 'needs_auth', 'needs_action', 'conflict', 'outcome_unknown'].includes(task.status)
    )
      throw new Error(JSON.stringify(task));
    await delay(200);
  }
  throw new Error('Move did not converge within 120 seconds');
}
const evidence = {
  scope:
    'Real source-built MinIO servers on loopback, real Tauri IPC/SQLite, real macOS NFS client; disposable test data only',
  minio_commit: '7aac2a2c5b7c882e68c1ce017d8256be2feea27f',
  app_id: appId,
  captured_at: new Date().toISOString(),
  audit_prefix: auditPrefix,
  moves: [],
};
try {
  const sourceFingerprintBefore = productionSourceFingerprint(repo);
  const appBinarySha256 = await fileSha(binary);
  if (expectedAppSha256)
    assert.equal(appBinarySha256, expectedAppSha256, 'MINIO_AUDIT_APP_SHA256 mismatch');
  await run('python3', [
    '-c',
    'import mmap,sys\nwith open(sys.argv[1],"rb") as f, mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as b:\n assert b.find(sys.argv[2].encode()) >= 0, "isolated audit build required"',
    binary,
    appId,
  ]);
  const buildSnapshot = await loadBuildSnapshot(binary);
  validateBuildBinding(buildSnapshot, {
    appBinarySha256,
    sourceFingerprint: sourceFingerprintBefore,
  });
  Object.assign(evidence, {
    app_binary_sha256: appBinarySha256,
    source_fingerprint_before: sourceFingerprintBefore,
    built_source_snapshot: buildSnapshot,
  });
  const backends = [];
  for (const role of ['source', 'destination']) {
    const listen = await freePort();
    const consolePort = await freePort();
    const data = join(root, role);
    await mkdir(data);
    const process = spawn(
      minio,
      [
        'server',
        '--address',
        `127.0.0.1:${listen}`,
        '--console-address',
        `127.0.0.1:${consolePort}`,
        data,
      ],
      {
        env: {
          PATH: globalThis.process.env.PATH,
          TMPDIR: root,
          MINIO_ROOT_USER: credentials.accessKeyId,
          MINIO_ROOT_PASSWORD: credentials.secretAccessKey,
          MINIO_BROWSER: 'off',
        },
        stdio: 'ignore',
      }
    );
    children.push(process);
    const endpoint = `http://127.0.0.1:${listen}`;
    let ready = false;
    for (let count = 0; count < 100; count++) {
      if (process.exitCode !== null) throw new Error(`MinIO ${role} exited`);
      try {
        if (
          (await fetch(endpoint + '/minio/health/live', { signal: AbortSignal.timeout(1000) })).ok
        ) {
          ready = true;
          break;
        }
      } catch {}
      await delay(100);
    }
    assert(ready, 'MinIO did not start');
    const client = new S3Client({
      endpoint,
      credentials,
      forcePathStyle: true,
      region: 'us-east-1',
      maxAttempts: 1,
    });
    await client.send(new CreateBucketCommand({ Bucket: 'photos' }));
    backends.push({ role, endpoint, client });
  }
  app = spawn(binary, [], { cwd: repo, stdio: 'ignore' });
  for (let count = 0; count < 150; count++) {
    port = await ownedConnectorPort(app, appId);
    if (port) break;
    if (app.exitCode !== null) throw new Error('Isolated app exited');
    await delay(200);
  }
  assert(port, 'Isolated connector not ready');
  assert.equal((await cli('state')).app.identifier, appId);
  for (const backend of backends) {
    const input = {
      name: `Real MinIO audit ${backend.role}`,
      access_key_id: credentials.accessKeyId,
      secret_access_key: credentials.secretAccessKey,
      endpoint_scheme: 'http',
      endpoint_host: new URL(backend.endpoint).host,
      force_path_style: true,
    };
    const account = await ipc('create_minio_account', { input });
    backend.config = {
      ...input,
      provider: 'minio',
      account_id: account.id,
      bucket: 'photos',
      region: 'us-east-1',
    };
  }
  const [source, destination] = backends;
  for (const size of [12, 128 * 1024 * 1024]) {
    const versioned = size > 12;
    if (versioned) {
      await source.client.send(
        new PutBucketVersioningCommand({
          Bucket: 'photos',
          VersioningConfiguration: { Status: 'Enabled' },
        })
      );
    }
    const baseKey = `中文 +%?# literal%2F-${size}.bin`;
    const key = `${auditPrefix}${baseKey}`;
    const destKey = `${auditPrefix}moved/${baseKey}`;
    const content = Buffer.alloc(size);
    for (let i = 0; i < content.length; i++) content[i] = (i * 31 + 17) % 251;
    const expected = digest(content);
    await source.client.send(new PutObjectCommand({ Bucket: 'photos', Key: key, Body: content }));
    await destination.client.send(
      new PutObjectCommand({
        Bucket: 'photos',
        Key: key,
        Body: Buffer.from('wrong-source-on-destination'),
      })
    );
    const start = performance.now();
    await ipc('start_batch_move', {
      sourceConfig: source.config,
      destConfig: destination.config,
      operations: [{ source_key: key, dest_key: destKey, overwrite: false }],
      deleteOriginal: true,
    });
    const task = await waitTask(
      source.config,
      key,
      destKey,
      versioned ? 'success' : 'needs_action'
    );
    const elapsed = performance.now() - start;
    assert.equal(digest(await bytes(destination.client, destKey)), expected);
    assert.equal(await missing(source.client, key), versioned);
    if (!versioned) {
      assert.equal(digest(await bytes(source.client, key)), expected);
      assert.match(task.error, /conditional deletion has not been verified/);
    }
    assert.equal(
      Buffer.from(await bytes(destination.client, key)).toString(),
      'wrong-source-on-destination'
    );
    evidence.moves.push({
      bytes: size,
      status: task.status,
      elapsed_ms: elapsed,
      verified_sha256: expected,
      source_versioning: versioned,
      source_removed: versioned,
      destination_same_named_source_preserved: true,
    });
  }
  const mountPath = join(root, 'mount');
  await mkdir(mountPath);
  const mounted = await ipc('mount_bucket', {
    input: {
      provider: 'minio',
      account_id: source.config.account_id,
      bucket: 'photos',
      local_path: mountPath,
      access_key_id: credentials.accessKeyId,
      secret_access_key: credentials.secretAccessKey,
      region: 'us-east-1',
      endpoint_url: source.endpoint,
      force_path_style: true,
      read_only: false,
      max_staging_bytes: 256 * 1024 * 1024,
    },
  });
  mountId = mounted.mount_id;
  assert(mountId, 'Mount command returned no mount identity');
  mountDetached = false;
  await mkdir(join(mountPath, dirname(auditPrefix)), { recursive: true });
  const original = `${auditPrefix}原始 +%?#.txt`;
  const renamed = `${auditPrefix}改名 +%?#.txt`;
  const content = Buffer.from('durable real MinIO NFS content');
  await writeFile(join(mountPath, original), content);
  assert.deepEqual(await readFile(join(mountPath, original)), content);
  // This MinIO revision ignores conditional DELETE. Unsupported namespace
  // mutations must fail before freezing the original staged data.
  await assert.rejects(rename(join(mountPath, original), join(mountPath, renamed)));
  assert.deepEqual(await readFile(join(mountPath, original)), content);
  await assert.rejects(unlink(join(mountPath, original)));
  assert(await missing(source.client, renamed));
  await ipc('unmount_bucket', { mountId });
  mountDetached = true;
  mountId = undefined;
  assert.equal(digest(await bytes(source.client, original)), digest(content));
  evidence.nfs = {
    os: globalThis.process.platform,
    write_read_unmount: 'passed',
    rename_delete: 'safely rejected: conditional deletion unsupported by this MinIO revision',
    remote_content_verified: true,
  };
  evidence.source_fingerprint_after = productionSourceFingerprint(repo);
  evidence.app_binary_sha256_after = await fileSha(binary);
  evidence.provenance_stable =
    evidence.app_binary_sha256_after === evidence.app_binary_sha256 &&
    evidence.source_fingerprint_after.production_source_sha256 ===
      evidence.source_fingerprint_before.production_source_sha256;
  assert(evidence.provenance_stable, 'source or app binary changed during MinIO audit');
  await writeFile(
    join(repo, 'docs/engineering/r2-audit/real-minio.json'),
    JSON.stringify(evidence, null, 2) + '\n'
  );
  console.log(JSON.stringify(evidence, null, 2));
} finally {
  if (mountId) {
    try {
      await ipc('unmount_bucket', { mountId });
      mountDetached = true;
    } catch (error) {
      console.error('Unmount failed; test directory retained:', root, error.message);
    }
  }
  await stop(app);
  for (const child of children) await stop(child);
  // Never recursively remove a path still backed by an OS mount.
  if (mountDetached) await rm(root, { recursive: true, force: true });
}
