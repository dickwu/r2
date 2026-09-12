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
import { resolve, join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import {
  S3Client,
  CreateBucketCommand,
  PutObjectCommand,
  GetObjectCommand,
  HeadObjectCommand,
  PutBucketVersioningCommand,
} from '@aws-sdk/client-s3';

const run = promisify(execFile);
const repo = resolve(import.meta.dirname, '..');
const appId = 'com.lifefarmer.r2.audit-01a096c3-20260912';
const binary = join(repo, 'src-tauri/target/debug/r2');
const minio = process.env.MINIO_AUDIT_BINARY;
assert(minio, 'MINIO_AUDIT_BINARY is required');
await run('python3', [
  '-c',
  'import mmap,sys\nwith open(sys.argv[1],"rb") as f, mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as b:\n assert b.find(sys.argv[2].encode()) >= 0, "isolated audit build required"',
  binary,
  appId,
]);
const root = await mkdtemp(join(tmpdir(), 'r2-real-minio-'));
const credentials = { accessKeyId: 'r2-audit', secretAccessKey: randomBytes(24).toString('hex') };
const children = [];
let app;
let port;
let mountId;
let mountDetached = true;
const digest = (bytes) => createHash('sha256').update(bytes).digest('hex');
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
  moves: [],
};
try {
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
          ...globalThis.process.env,
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
  for (let count = 0; count < 100; count++) {
    try {
      const connector = JSON.parse(
        await readFile(join(repo, 'src-tauri/target/.connector.json'), 'utf8')
      );
      if (connector.pid === app.pid && connector.app_id === appId) {
        port = connector.ws_port;
        break;
      }
    } catch {}
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
    const key = `中文 +%?# literal%2F-${size}.bin`;
    const destKey = `moved/${key}`;
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
  const original = '原始 +%?#.txt';
  const renamed = '改名 +%?#.txt';
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
