/** Disposable RustFS acceptance. Default: SDK protocol probes only, no Tauri launch.
 * Native phase requires --native plus RUSTFS_AUDIT_APP_BINARY and RUSTFS_AUDIT_APP_ID.
 * Never accepts an existing endpoint, credentials, bucket, or normal application profile.
 */
import assert from 'node:assert/strict';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createHash, randomBytes } from 'node:crypto';
import { createReadStream, createWriteStream } from 'node:fs';
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
  CopyObjectCommand,
  DeleteObjectCommand,
  CreateMultipartUploadCommand,
  UploadPartCommand,
  UploadPartCopyCommand,
  CompleteMultipartUploadCommand,
  AbortMultipartUploadCommand,
  ListPartsCommand,
  PutBucketVersioningCommand,
  GetBucketVersioningCommand,
} from '@aws-sdk/client-s3';
import { createEvidence, nativeEvidence } from './native-rustfs-report.mjs';

const run = promisify(execFile);
const repo = resolve(import.meta.dirname, '..');
const runtime = join(repo, '.omx/artifacts/rustfs-runtime');
const binary = join(runtime, 'extracted/rustfs');
const archive = join(runtime, 'rustfs-macos-aarch64-v1.0.0-rc.6.zip');
const expectedArchiveSha = 'eb3c2b8a6f4bbe2734f9545413922604ba8bc00e50a6463cef3a321c4ca988e7';
const expectedCommit = '5cd58319ed6148ed7f09f2a4d0b4e46e429f043a';
const native = process.argv.includes('--native');
assert(
  process.argv.slice(2).every((arg) => arg === '--native'),
  'Only --native is supported'
);
assert.equal(process.platform, 'darwin', 'This pinned artifact is macOS only');
assert.equal(process.arch, 'arm64', 'This pinned artifact is arm64 only');
const sha = (bytes) => createHash('sha256').update(bytes).digest('hex');
async function fileSha(path) {
  const hash = createHash('sha256');
  for await (const bytes of createReadStream(path)) hash.update(bytes);
  return hash.digest('hex');
}

async function writeDeterministicFile(path, size, seed) {
  assert(Number.isSafeInteger(size) && size >= 0, 'deterministic file size must be safe');
  const hash = createHash('sha256');
  const chunkSize = 1024 * 1024;
  const chunk = Buffer.alloc(chunkSize);
  let written = 0;
  await new Promise((resolve, reject) => {
    const stream = createWriteStream(path, { mode: 0o600 });
    stream.on('error', reject);
    stream.on('finish', resolve);
    function writeMore() {
      while (written < size) {
        const length = Math.min(chunkSize, size - written);
        for (let index = 0; index < length; index++) {
          chunk[index] = (written + index + seed * 131) % 251;
        }
        const view = chunk.subarray(0, length);
        hash.update(view);
        written += length;
        if (!stream.write(view)) {
          stream.once('drain', writeMore);
          return;
        }
      }
      stream.end();
    }
    writeMore();
  });
  return { path, size, sha256: hash.digest('hex') };
}

async function streamingObjectSha(client, key, selectedBucket = bucket) {
  const response = await send(client, new GetObjectCommand({ Bucket: selectedBucket, Key: key }));
  const hash = createHash('sha256');
  let size = 0;
  for await (const chunk of response.Body) {
    const bytes = Buffer.from(chunk);
    size += bytes.length;
    hash.update(bytes);
  }
  return { size, sha256: hash.digest('hex'), etag: response.ETag };
}
assert.equal(await fileSha(archive), expectedArchiveSha, 'RustFS archive SHA256 mismatch');
// Check extracted bytes against the verified archive before executing them.
await run('python3', [
  '-c',
  'import hashlib,sys,zipfile\nwith zipfile.ZipFile(sys.argv[1]) as z:\n expected=hashlib.sha256(z.read("rustfs")).digest()\nwith open(sys.argv[2],"rb") as f:\n actual=hashlib.file_digest(f,"sha256").digest()\nassert actual==expected,"extracted RustFS differs from verified archive"',
  archive,
  binary,
]);
const { stdout: version } = await run(binary, ['--version'], { timeout: 10_000 });
assert(version.includes('rustfs 1.0.0-rc.6') && version.includes(expectedCommit));
const root = await mkdtemp(join(tmpdir(), 'r2-real-rustfs-'));
const credentials = {
  accessKeyId: `r2-audit-${randomBytes(6).toString('hex')}`,
  secretAccessKey: randomBytes(24).toString('hex'),
};
const children = [];
const clients = [];
const createdAccounts = [];
let app;
let connectorPort;
let mountId;
let mountDetached = true;
const MiB = 1024 * 1024;
const GiB = 1024 * MiB;
const bucket = 'audit-objects';
const wrongEtag = '"00000000000000000000000000000000"';
const evidence = createEvidence({
  native,
  repo,
  rustfs: { version: '1.0.0-rc.6', commit: expectedCommit, archiveSha256: expectedArchiveSha },
  binarySha256: await fileSha(binary),
  harnessSha256: await fileSha(import.meta.filename),
  versionOutput: version.trim(),
});
const output = join(
  repo,
  `docs/engineering/r2-audit/real-rustfs${native ? '-native' : '-protocol'}.json`
);
async function freePort() {
  const server = createServer();
  await new Promise((ok, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', ok);
  });
  const port = server.address().port;
  await new Promise((ok) => server.close(ok));
  return port;
}
async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill('SIGTERM');
  for (let count = 0; count < 100 && child.exitCode === null && child.signalCode === null; count++)
    await delay(100);
  if (child.exitCode === null && child.signalCode === null) {
    const ended = new Promise((ok) => child.once('exit', ok));
    child.kill('SIGKILL');
    await ended;
  }
}
function send(client, command) {
  return client.send(command, { abortSignal: AbortSignal.timeout(30_000) });
}
async function attempt(client, command) {
  try {
    return { ok: true, value: await send(client, command), status: 200 };
  } catch (error) {
    return {
      ok: false,
      status: error.$metadata?.httpStatusCode ?? null,
      code: error.name,
      message: error.message,
    };
  }
}
async function bytes(client, key, extra = {}, selectedBucket = bucket) {
  const response = await send(
    client,
    new GetObjectCommand({ Bucket: selectedBucket, Key: key, ...extra })
  );
  return { response, bytes: Buffer.from(await response.Body.transformToByteArray()) };
}
async function exists(client, key, extra = {}, selectedBucket = bucket) {
  const response = await attempt(
    client,
    new HeadObjectCommand({ Bucket: selectedBucket, Key: key, ...extra })
  );
  if (response.ok) return true;
  if (response.status === 404) return false;
  throw new Error(`HEAD failed: ${JSON.stringify(response)}`);
}
async function put(client, key, body, extra = {}, selectedBucket = bucket) {
  return send(
    client,
    new PutObjectCommand({ Bucket: selectedBucket, Key: key, Body: body, ...extra })
  );
}
function copySource(key, selectedBucket = bucket) {
  return `${selectedBucket}/${key.split('/').map(encodeURIComponent).join('/')}`;
}
function classifyCondition(result, unchanged) {
  if (result.ok) {
    assert.equal(
      unchanged,
      false,
      'Conditional operation succeeded but expected mutation is absent'
    );
    return { behavior: 'ignored', rejection_status: null, protects_object: false };
  }
  assert(unchanged, 'Rejected conditional operation changed object data');
  if (result.status === 412)
    return { behavior: 'enforced', rejection_status: 412, protects_object: true };
  assert(
    [400, 405, 501].includes(result.status),
    `Unexpected conditional failure: ${JSON.stringify(result)}`
  );
  return {
    behavior: 'rejected_unsupported',
    rejection_status: result.status,
    error_code: result.code,
    protects_object: false,
  };
}
async function conditionalMutation(client, name, makeCommand, deleted = false) {
  const key = `conditions/${name}`;
  const original = Buffer.from(`original:${name}`);
  const receipt = await put(client, key, original);
  assert.notEqual(receipt.ETag, wrongEtag);
  const result = await attempt(client, makeCommand(key));
  const remains = await exists(client, key);
  const current = remains ? (await bytes(client, key)).bytes : null;
  const unchanged = remains && current.equals(original);
  if (result.ok && deleted) assert.equal(remains, false);
  evidence.conditions[name] = classifyCondition(result, unchanged);
  return evidence.conditions[name];
}
async function startBackend(role) {
  const apiPort = await freePort();
  const consolePort = await freePort();
  const data = join(root, role, 'data');
  const logs = join(root, role, 'logs');
  await mkdir(data, { recursive: true });
  await mkdir(logs);
  // Exclude inherited RustFS/MinIO settings, cloud credentials and network proxy configuration.
  const child = spawn(
    binary,
    [
      'server',
      '--address',
      `127.0.0.1:${apiPort}`,
      '--console-address',
      `127.0.0.1:${consolePort}`,
      data,
    ],
    {
      cwd: join(root, role),
      env: {
        PATH: process.env.PATH,
        TMPDIR: root,
        RUSTFS_ACCESS_KEY: credentials.accessKeyId,
        RUSTFS_SECRET_KEY: credentials.secretAccessKey,
        RUSTFS_CONSOLE_ENABLE: 'false',
        RUSTFS_OBS_LOGGER_LEVEL: 'warn',
        RUSTFS_OBS_LOG_DIRECTORY: logs,
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    }
  );
  const recentLogs = [];
  const capture = (chunk) => {
    recentLogs.push(chunk.toString());
    if (recentLogs.length > 40) recentLogs.shift();
  };
  child.stdout.on('data', capture);
  child.stderr.on('data', capture);
  children.push(child);
  const endpoint = `http://127.0.0.1:${apiPort}`;
  let ready = false;
  for (let count = 0; count < 300; count++) {
    assert(
      child.exitCode === null && child.signalCode === null,
      `RustFS ${role} exited: ${recentLogs.join('').replaceAll(credentials.secretAccessKey, '[redacted]')}`
    );
    try {
      if ((await fetch(`${endpoint}/health/ready`, { signal: AbortSignal.timeout(1000) })).ok) {
        ready = true;
        break;
      }
    } catch {}
    await delay(100);
  }
  assert(
    ready,
    `RustFS ${role} readiness timed out: ${recentLogs.join('').replaceAll(credentials.secretAccessKey, '[redacted]')}`
  );
  const { stdout: listening } = await run('lsof', [
    '-nP',
    '-a',
    '-p',
    String(child.pid),
    '-iTCP',
    '-sTCP:LISTEN',
  ]);
  const addresses = listening
    .trim()
    .split('\n')
    .slice(1)
    .map((line) => line.trim().split(/\s+/).at(-2));
  assert(
    addresses.length > 0 &&
      addresses.every(
        (address) => address.startsWith('127.0.0.1:') || address.startsWith('[::1]:')
      ),
    'RustFS opened a non-loopback listener'
  );
  (evidence.listeners ??= []).push({ role, addresses, loopback_only: true });
  const client = new S3Client({
    endpoint,
    credentials,
    forcePathStyle: true,
    region: 'us-east-1',
    maxAttempts: 1,
  });
  clients.push(client);
  await send(client, new CreateBucketCommand({ Bucket: bucket }));
  return { role, endpoint, client };
}

async function probeProtocol(client) {
  const sourceKey = '中文 +%?# literal%2F-source.bin';
  const content = Buffer.alloc(12 * MiB);
  for (let i = 0; i < content.length; i++) content[i] = (i * 31 + 17) % 251;
  const source = await put(client, sourceKey, content, { Metadata: { audit: 'rustfs-protocol' } });
  const range = await bytes(client, sourceKey, { Range: 'bytes=17-4112', IfMatch: source.ETag });
  assert.equal(range.response.$metadata.httpStatusCode, 206);
  assert.equal(range.response.ContentRange, `bytes 17-4112/${content.length}`);
  assert.equal(range.response.ContentLength, 4096);
  assert.equal(range.response.ETag, source.ETag);
  assert.deepEqual(range.bytes, content.subarray(17, 4113));
  evidence.range = {
    status: 206,
    content_range: range.response.ContentRange,
    sha256: sha(range.bytes),
    exact_bytes: true,
    special_key: sourceKey,
  };
  const wrongRead = await attempt(
    client,
    new GetObjectCommand({ Bucket: bucket, Key: sourceKey, Range: 'bytes=0-1', IfMatch: wrongEtag })
  );
  if (wrongRead.ok) {
    await wrongRead.value.Body.transformToByteArray();
    evidence.conditions.get_match = { behavior: 'ignored', protects_object: false };
  } else {
    assert.equal(wrongRead.status, 412);
    evidence.conditions.get_match = {
      behavior: 'enforced',
      rejection_status: 412,
      protects_object: true,
      matching_request_succeeded: true,
    };
  }
  await conditionalMutation(
    client,
    'put_absent',
    (key) =>
      new PutObjectCommand({
        Bucket: bucket,
        Key: key,
        IfNoneMatch: '*',
        Body: Buffer.from('replacement'),
      })
  );
  await conditionalMutation(
    client,
    'put_match',
    (key) =>
      new PutObjectCommand({
        Bucket: bucket,
        Key: key,
        IfMatch: wrongEtag,
        Body: Buffer.from('replacement'),
      })
  );
  await conditionalMutation(
    client,
    'copy_source_match',
    (key) =>
      new CopyObjectCommand({
        Bucket: bucket,
        Key: key,
        CopySource: copySource(sourceKey),
        CopySourceIfMatch: wrongEtag,
      })
  );
  await conditionalMutation(
    client,
    'copy_destination_absent',
    (key) =>
      new CopyObjectCommand({
        Bucket: bucket,
        Key: key,
        CopySource: copySource(sourceKey),
        IfNoneMatch: '*',
      })
  );
  await conditionalMutation(
    client,
    'copy_destination_match',
    (key) =>
      new CopyObjectCommand({
        Bucket: bucket,
        Key: key,
        CopySource: copySource(sourceKey),
        IfMatch: wrongEtag,
      })
  );
  await conditionalMutation(
    client,
    'delete_match',
    (key) => new DeleteObjectCommand({ Bucket: bucket, Key: key, IfMatch: wrongEtag }),
    true
  );
  // Prove positive conditional requests work too: a 412-only endpoint is not a usable capability.
  if (evidence.conditions.put_absent.behavior === 'enforced') {
    await put(client, 'positive/put', Buffer.from('first'), { IfNoneMatch: '*' });
    assert.equal((await bytes(client, 'positive/put')).bytes.toString(), 'first');
    evidence.conditions.put_absent.matching_request_succeeded = true;
    const current = await send(
      client,
      new HeadObjectCommand({ Bucket: bucket, Key: 'positive/put' })
    );
    if (evidence.conditions.put_match.behavior === 'enforced') {
      await put(client, 'positive/put', Buffer.from('second'), { IfMatch: current.ETag });
      assert.equal((await bytes(client, 'positive/put')).bytes.toString(), 'second');
      evidence.conditions.put_match.matching_request_succeeded = true;
    }
  }
  if (evidence.conditions.copy_source_match.behavior === 'enforced') {
    await send(
      client,
      new CopyObjectCommand({
        Bucket: bucket,
        Key: 'positive/copy',
        CopySource: copySource(sourceKey),
        CopySourceIfMatch: source.ETag,
      })
    );
    assert.equal(sha((await bytes(client, 'positive/copy')).bytes), sha(content));
    evidence.conditions.copy_source_match.matching_request_succeeded = true;
  }
  if (evidence.conditions.copy_destination_absent.behavior === 'enforced') {
    await send(
      client,
      new CopyObjectCommand({
        Bucket: bucket,
        Key: 'positive/copy-absent',
        CopySource: copySource(sourceKey),
        IfNoneMatch: '*',
      })
    );
    assert.equal(sha((await bytes(client, 'positive/copy-absent')).bytes), sha(content));
    evidence.conditions.copy_destination_absent.matching_request_succeeded = true;
  }
  if (evidence.conditions.copy_destination_match.behavior === 'enforced') {
    const current = await put(client, 'positive/copy-match', Buffer.from('old'));
    await send(
      client,
      new CopyObjectCommand({
        Bucket: bucket,
        Key: 'positive/copy-match',
        CopySource: copySource(sourceKey),
        IfMatch: current.ETag,
      })
    );
    assert.equal(sha((await bytes(client, 'positive/copy-match')).bytes), sha(content));
    evidence.conditions.copy_destination_match.matching_request_succeeded = true;
  }
  if (evidence.conditions.delete_match.behavior === 'enforced') {
    const current = await put(client, 'positive/delete', Buffer.from('delete me'));
    await send(
      client,
      new DeleteObjectCommand({ Bucket: bucket, Key: 'positive/delete', IfMatch: current.ETag })
    );
    assert.equal(await exists(client, 'positive/delete'), false);
    evidence.conditions.delete_match.matching_request_succeeded = true;
  }
  const copyUpload = await send(
    client,
    new CreateMultipartUploadCommand({ Bucket: bucket, Key: 'part-copy' })
  );
  let copyCompleted = false;
  try {
    const result = await attempt(
      client,
      new UploadPartCopyCommand({
        Bucket: bucket,
        Key: 'part-copy',
        UploadId: copyUpload.UploadId,
        PartNumber: 1,
        CopySource: copySource(sourceKey),
        CopySourceRange: `bytes=0-${5 * MiB - 1}`,
        CopySourceIfMatch: wrongEtag,
      })
    );
    const listed = await send(
      client,
      new ListPartsCommand({ Bucket: bucket, Key: 'part-copy', UploadId: copyUpload.UploadId })
    );
    evidence.conditions.part_copy_source_match = classifyCondition(
      result,
      (listed.Parts ?? []).length === 0
    );
    if (evidence.conditions.part_copy_source_match.behavior === 'enforced') {
      const copied = await send(
        client,
        new UploadPartCopyCommand({
          Bucket: bucket,
          Key: 'part-copy',
          UploadId: copyUpload.UploadId,
          PartNumber: 1,
          CopySource: copySource(sourceKey),
          CopySourceRange: `bytes=0-${5 * MiB - 1}`,
          CopySourceIfMatch: source.ETag,
        })
      );
      await send(
        client,
        new CompleteMultipartUploadCommand({
          Bucket: bucket,
          Key: 'part-copy',
          UploadId: copyUpload.UploadId,
          MultipartUpload: { Parts: [{ PartNumber: 1, ETag: copied.CopyPartResult.ETag }] },
        })
      );
      copyCompleted = true;
      assert.equal(
        sha((await bytes(client, 'part-copy')).bytes),
        sha(content.subarray(0, 5 * MiB))
      );
      evidence.conditions.part_copy_source_match.matching_request_succeeded = true;
    }
  } finally {
    if (!copyCompleted)
      await send(
        client,
        new AbortMultipartUploadCommand({
          Bucket: bucket,
          Key: 'part-copy',
          UploadId: copyUpload.UploadId,
        })
      );
  }
  const upload = await send(
    client,
    new CreateMultipartUploadCommand({
      Bucket: bucket,
      Key: 'multipart/exact',
      Metadata: { audit: 'multipart-marker' },
    })
  );
  const parts = [];
  for (let offset = 0, number = 1; offset < content.length; offset += 5 * MiB, number++) {
    const body = content.subarray(offset, Math.min(offset + 5 * MiB, content.length));
    const part = await send(
      client,
      new UploadPartCommand({
        Bucket: bucket,
        Key: 'multipart/exact',
        UploadId: upload.UploadId,
        PartNumber: number,
        Body: body,
      })
    );
    parts.push({ PartNumber: number, ETag: part.ETag });
  }
  const listedParts = [];
  const markers = new Set();
  let marker;
  do {
    const page = await send(
      client,
      new ListPartsCommand({
        Bucket: bucket,
        Key: 'multipart/exact',
        UploadId: upload.UploadId,
        MaxParts: 1,
        PartNumberMarker: marker,
      })
    );
    listedParts.push(...(page.Parts ?? []).map(({ PartNumber, ETag }) => ({ PartNumber, ETag })));
    if (!page.IsTruncated) break;
    assert(
      page.NextPartNumberMarker && !markers.has(page.NextPartNumberMarker),
      'ListParts cursor did not advance'
    );
    marker = page.NextPartNumberMarker;
    markers.add(marker);
  } while (true);
  assert.deepEqual(listedParts, parts);
  await send(
    client,
    new CompleteMultipartUploadCommand({
      Bucket: bucket,
      Key: 'multipart/exact',
      UploadId: upload.UploadId,
      MultipartUpload: { Parts: parts },
    })
  );
  const uploaded = await bytes(client, 'multipart/exact');
  assert.equal(sha(uploaded.bytes), sha(content));
  assert.equal(uploaded.response.Metadata.audit, 'multipart-marker');
  evidence.multipart = {
    bytes: content.length,
    parts: parts.length,
    list_pages: markers.size + 1,
    sha256: sha(uploaded.bytes),
    exact_bytes: true,
    metadata_preserved: true,
  };
  // Introduce the collision after Create/UploadPart, testing completion-time protection.
  const collisionKey = 'conditions/complete_absent';
  const collisionUpload = await send(
    client,
    new CreateMultipartUploadCommand({ Bucket: bucket, Key: collisionKey })
  );
  const part = await send(
    client,
    new UploadPartCommand({
      Bucket: bucket,
      Key: collisionKey,
      UploadId: collisionUpload.UploadId,
      PartNumber: 1,
      Body: Buffer.from('multipart replacement'),
    })
  );
  await put(client, collisionKey, Buffer.from('existing destination'));
  const completion = await attempt(
    client,
    new CompleteMultipartUploadCommand({
      Bucket: bucket,
      Key: collisionKey,
      UploadId: collisionUpload.UploadId,
      IfNoneMatch: '*',
      MultipartUpload: { Parts: [{ PartNumber: 1, ETag: part.ETag }] },
    })
  );
  evidence.conditions.complete_absent = classifyCondition(
    completion,
    (await bytes(client, collisionKey)).bytes.equals(Buffer.from('existing destination'))
  );
  if (!completion.ok)
    await send(
      client,
      new AbortMultipartUploadCommand({
        Bucket: bucket,
        Key: collisionKey,
        UploadId: collisionUpload.UploadId,
      })
    );
  if (evidence.conditions.complete_absent.behavior === 'enforced') {
    const key = 'positive/complete-absent';
    const mpu = await send(client, new CreateMultipartUploadCommand({ Bucket: bucket, Key: key }));
    const positivePart = await send(
      client,
      new UploadPartCommand({
        Bucket: bucket,
        Key: key,
        UploadId: mpu.UploadId,
        PartNumber: 1,
        Body: Buffer.from('positive complete'),
      })
    );
    await send(
      client,
      new CompleteMultipartUploadCommand({
        Bucket: bucket,
        Key: key,
        UploadId: mpu.UploadId,
        IfNoneMatch: '*',
        MultipartUpload: { Parts: [{ PartNumber: 1, ETag: positivePart.ETag }] },
      })
    );
    assert.equal((await bytes(client, key)).bytes.toString(), 'positive complete');
    evidence.conditions.complete_absent.matching_request_succeeded = true;
  }
  const versionBucket = 'audit-versions';
  await send(client, new CreateBucketCommand({ Bucket: versionBucket }));
  const enable = await attempt(
    client,
    new PutBucketVersioningCommand({
      Bucket: versionBucket,
      VersioningConfiguration: { Status: 'Enabled' },
    })
  );
  if (!enable.ok) {
    assert([400, 405, 501].includes(enable.status));
    evidence.versioning = { supported: false, status: enable.status, code: enable.code };
  } else {
    assert.equal(
      (await send(client, new GetBucketVersioningCommand({ Bucket: versionBucket }))).Status,
      'Enabled'
    );
    const first = await put(client, 'version-key', Buffer.from('first version'), {}, versionBucket);
    const second = await put(
      client,
      'version-key',
      Buffer.from('second version'),
      {},
      versionBucket
    );
    assert(
      first.VersionId &&
        second.VersionId &&
        first.VersionId !== second.VersionId &&
        first.VersionId !== 'null'
    );
    assert.equal(
      (
        await bytes(
          client,
          'version-key',
          { VersionId: first.VersionId, IfMatch: first.ETag },
          versionBucket
        )
      ).bytes.toString(),
      'first version'
    );
    await send(
      client,
      new DeleteObjectCommand({
        Bucket: versionBucket,
        Key: 'version-key',
        VersionId: first.VersionId,
      })
    );
    assert.equal(
      await exists(client, 'version-key', { VersionId: first.VersionId }, versionBucket),
      false
    );
    assert.equal(
      (await bytes(client, 'version-key', {}, versionBucket)).bytes.toString(),
      'second version'
    );
    evidence.versioning = {
      supported: true,
      distinct_versions: true,
      pinned_read: true,
      pinned_delete_preserves_latest: true,
    };
  }
  evidence.assertions.push(
    'Range status, boundaries, ETag and bytes verified',
    'Conditional rejections cross-checked against stored bytes',
    'Multipart pagination, metadata and full SHA256 verified'
  );
}

async function cli(...args) {
  const { stdout } = await run(
    'tauri-connector',
    ['--host', '127.0.0.1', '--port', String(connectorPort), ...args],
    { timeout: 65_000, maxBuffer: 4 * MiB }
  );
  return JSON.parse(stdout);
}
const ipc = (name, args) => cli('ipc', 'exec', name, '-a', JSON.stringify(args));

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
      const { stdout } = await run(
        'tauri-connector',
        ['--host', '127.0.0.1', '--port', String(port), 'state'],
        { timeout: 2000 }
      );
      if (
        JSON.parse(stdout).app.identifier === appId &&
        child.exitCode === null &&
        child.signalCode === null
      )
        return port;
    } catch {
      /* The owned listener may still be starting. */
    }
  }
  return null;
}

async function nativePhase(source, destination) {
  const appBinary = process.env.RUSTFS_AUDIT_APP_BINARY;
  const appId = process.env.RUSTFS_AUDIT_APP_ID;
  assert(
    appBinary && appId?.startsWith('com.lifefarmer.r2.audit-'),
    'Native phase requires an isolated audit binary and app ID'
  );
  await run('python3', [
    '-c',
    'import mmap,sys\nwith open(sys.argv[1],"rb") as f,mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as b:\n assert b.find(sys.argv[2].encode())>=0,"isolated app ID absent from binary"',
    appBinary,
    appId,
  ]);
  if (process.env.RUSTFS_AUDIT_APP_SHA256)
    assert.equal(
      await fileSha(appBinary),
      process.env.RUSTFS_AUDIT_APP_SHA256,
      'Isolated app binary SHA256 mismatch'
    );
  app = spawn(appBinary, [], { cwd: repo, stdio: 'ignore' });
  for (let count = 0; count < 150; count++) {
    connectorPort = await ownedConnectorPort(app, appId);
    if (connectorPort) break;
    assert.equal(app.exitCode, null, 'Audit application exited');
    await delay(200);
  }
  assert(connectorPort, 'Isolated connector not ready');
  assert.equal((await cli('state')).app.identifier, appId);
  evidence.native = nativeEvidence({ appId, appBinarySha256: await fileSha(appBinary) });
  for (const backend of [source, destination]) {
    const input = {
      name: `RustFS acceptance ${backend.role}`,
      access_key_id: credentials.accessKeyId,
      secret_access_key: credentials.secretAccessKey,
      endpoint_scheme: 'http',
      endpoint_host: new URL(backend.endpoint).host,
      force_path_style: true,
    };
    const account = await ipc('create_rustfs_account', { input });
    createdAccounts.push(account.id);
    backend.config = {
      ...input,
      provider: 'rustfs',
      account_id: account.id,
      bucket,
      region: 'us-east-1',
    };
  }
  const nativeSizes = [12, 128 * MiB];
  const largeBytes = process.env.RUSTFS_AUDIT_LARGE_BYTES
    ? Number.parseInt(process.env.RUSTFS_AUDIT_LARGE_BYTES, 10)
    : 0;
  if (largeBytes) {
    assert(
      largeBytes >= 6 * GiB,
      'RUSTFS_AUDIT_LARGE_BYTES must be at least 6GiB so the large app path cannot collapse to the 128MiB smoke case'
    );
    nativeSizes.push(largeBytes);
    evidence.native.large_file_mode = {
      requested_bytes: largeBytes,
      payload: 'file-backed deterministic stream; no full-size Buffer allocation',
      status: 'configured',
    };
  } else {
    evidence.native.large_file_mode = {
      minimum_bytes: 6 * GiB,
      payload: 'file-backed deterministic stream; no full-size Buffer allocation',
      status: 'not_requested',
      reason:
        'Set RUSTFS_AUDIT_LARGE_BYTES>=6442450944 with a fresh isolated app binary to execute',
    };
  }
  for (const size of nativeSizes) {
    const key = `native/中文 +%?#-${size}.bin`;
    const destKey = `moved/${key}`;
    const payloadFile =
      size >= GiB
        ? await writeDeterministicFile(join(root, `payload-${size}.bin`), size, 37)
        : null;
    const content = payloadFile ? null : Buffer.alloc(size, 37);
    const expectedSha = payloadFile ? payloadFile.sha256 : sha(content);
    await put(
      source.client,
      key,
      payloadFile ? createReadStream(payloadFile.path) : content,
      payloadFile ? { ContentLength: payloadFile.size } : {}
    );
    await put(destination.client, key, Buffer.from('wrong source on destination'));
    const started = performance.now();
    await ipc('start_batch_move', {
      sourceConfig: source.config,
      destConfig: destination.config,
      operations: [{ source_key: key, dest_key: destKey, overwrite: false }],
      deleteOriginal: true,
    });
    let task;
    for (let count = 0; count < 1200; count++) {
      const tasks = await ipc('get_move_tasks', {
        sourceBucket: bucket,
        sourceAccountId: source.config.account_id,
      });
      task = tasks.find((item) => item.source_key === key && item.dest_key === destKey);
      if (
        task &&
        ['success', 'needs_action', 'conflict', 'error', 'outcome_unknown', 'needs_auth'].includes(
          task.status
        )
      )
        break;
      await delay(200);
    }
    assert(
      task && ['success', 'needs_action'].includes(task.status),
      `Unexpected native result: ${JSON.stringify(task)}`
    );
    const published = await exists(destination.client, destKey);
    const retained = await exists(source.client, key);
    if (published) {
      const remote = payloadFile
        ? await streamingObjectSha(destination.client, destKey)
        : { sha256: sha((await bytes(destination.client, destKey)).bytes), size };
      assert.equal(remote.sha256, expectedSha);
      assert.equal(remote.size, size);
    }
    if (retained) {
      const remote = payloadFile
        ? await streamingObjectSha(source.client, key)
        : { sha256: sha((await bytes(source.client, key)).bytes), size };
      assert.equal(remote.sha256, expectedSha);
      assert.equal(remote.size, size);
    }
    assert(published || retained, 'Move lost both copies');
    if (task.status === 'success') assert(published && !retained);
    if (task.status === 'needs_action') assert(retained);
    assert.equal(
      (await bytes(destination.client, key)).bytes.toString(),
      'wrong source on destination'
    );
    evidence.native.moves.push({
      bytes: size,
      elapsed_ms: performance.now() - started,
      status: task.status,
      destination_verified: published,
      source_retained: retained,
      sha256: expectedSha,
      payload_storage: payloadFile ? 'file_stream' : 'buffer',
      error: task.error ?? null,
    });
  }
  const mountPath = join(root, 'mount');
  await mkdir(mountPath);
  const mounted = await ipc('mount_bucket', {
    input: {
      provider: 'rustfs',
      account_id: source.config.account_id,
      bucket,
      local_path: mountPath,
      access_key_id: credentials.accessKeyId,
      secret_access_key: credentials.secretAccessKey,
      endpoint_url: source.endpoint,
      force_path_style: true,
      read_only: false,
      max_staging_bytes: 256 * MiB,
    },
  });
  mountId = mounted.mount_id;
  assert(mountId);
  mountDetached = false;
  const first = 'native-nfs-中文 +%?#.txt';
  const second = 'native-nfs-renamed-中文 +%?#.txt';
  const content = Buffer.from('durable real RustFS NFS bytes');
  await writeFile(join(mountPath, first), content);
  assert.deepEqual(await readFile(join(mountPath, first)), content);
  let renamed = false;
  try {
    await rename(join(mountPath, first), join(mountPath, second));
    renamed = true;
  } catch {}
  const finalName = renamed ? second : first;
  assert.deepEqual(await readFile(join(mountPath, finalName)), content);
  // Keep this file for the post-unmount content assertion. Delete a separate fixture.
  await writeFile(join(mountPath, 'native-delete.txt'), content);
  let deleted = false;
  try {
    await unlink(join(mountPath, 'native-delete.txt'));
    deleted = true;
  } catch {}
  if (!deleted) assert.deepEqual(await readFile(join(mountPath, 'native-delete.txt')), content);
  await ipc('unmount_bucket', { mountId });
  mountId = undefined;
  mountDetached = true;
  assert.equal(sha((await bytes(source.client, finalName)).bytes), sha(content));
  assert.equal(await exists(source.client, 'native-delete.txt'), !deleted);
  evidence.native.nfs = {
    write_read_unmount: 'passed',
    rename: renamed ? 'completed' : 'safely_rejected',
    delete: deleted ? 'completed' : 'safely_rejected',
    remote_sha256: sha(content),
  };
}

try {
  const source = await startBackend('source');
  const destination = await startBackend('destination');
  await probeProtocol(source.client);
  if (native) await nativePhase(source, destination);
  evidence.completed = true;
} catch (error) {
  evidence.completed = false;
  evidence.failure = (error.stack || String(error))
    .replaceAll(credentials.secretAccessKey, '[redacted]')
    .replaceAll(credentials.accessKeyId, '[redacted]');
  process.exitCode = 1;
} finally {
  if (mountId) {
    try {
      await ipc('unmount_bucket', { mountId });
      mountDetached = true;
    } catch {
      evidence.cleanup = { retained_test_directory: root, reason: 'OS mount may remain' };
    }
  }
  if (createdAccounts.length > 0 && mountDetached && app && connectorPort) {
    try {
      assert.equal((await cli('state')).app.identifier, process.env.RUSTFS_AUDIT_APP_ID);
      assert.deepEqual(await ipc('list_mounts', {}), [], 'Owned app still has active mounts');
      for (const id of createdAccounts) await ipc('delete_rustfs_account', { id });
      const remaining = await ipc('list_rustfs_accounts', {});
      assert(!remaining.some((account) => createdAccounts.includes(account.id)));
      evidence.cleanup = {
        fixture_accounts_removed: createdAccounts,
        remaining_fixture_accounts: 0,
        active_mounts: 0,
      };
    } catch (error) {
      evidence.completed = false;
      evidence.cleanup = { error: error.message, fixture_account_ids: createdAccounts };
      process.exitCode = 1;
    }
  }
  await stop(app);
  for (const child of children) await stop(child);
  for (const client of clients) client.destroy();
  if (mountDetached) await rm(root, { recursive: true, force: true });
  evidence.cleanup = {
    ...evidence.cleanup,
    application_stopped: !app || app.exitCode !== null || app.signalCode !== null,
    rustfs_processes_stopped: children.every(
      (child) => child.exitCode !== null || child.signalCode !== null
    ),
    temporary_data_removed: mountDetached,
  };
  await writeFile(output, JSON.stringify(evidence, null, 2) + '\n');
  console.log(JSON.stringify(evidence, null, 2));
}
