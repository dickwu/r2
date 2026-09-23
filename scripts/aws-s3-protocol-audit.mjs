#!/usr/bin/env node
/** AWS S3 acceptance harness.
 *
 * --plan and --self-test are offline. --execute uses only explicit
 * AWS_AUDIT_* inputs and a fresh dedicated prefix in an already-created test
 * bucket. It runs bounded SDK probes, verifies digests/source identity and
 * performs owned cleanup; it never lists buckets, creates buckets, deletes
 * buckets or changes bucket configuration.
 */
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer as createNetServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import {
  auditProvenance,
  isMainModule,
  jsonPointer,
  PROVENANCE_POINTERS,
} from './audit-source.mjs';

const MiB = 1024 * 1024;
const repoRoot = resolve(import.meta.dirname, '..');
const PLAN = Object.freeze({
  status: 'prepared_not_executed',
  required_inputs: [
    'AWS_AUDIT_REGION',
    'AWS_AUDIT_SOURCE_BUCKET',
    'AWS_AUDIT_DEST_BUCKET',
    'AWS_AUDIT_ACCESS_KEY_ID',
    'AWS_AUDIT_SECRET_ACCESS_KEY',
    'AWS_AUDIT_PREFIX',
  ],
  prefix_format: 'r2-audit/aws/<fresh UUIDv4>/',
  bucket_matrix:
    'same bucket when AWS_AUDIT_SOURCE_BUCKET == AWS_AUDIT_DEST_BUCKET; cross bucket otherwise',
  maximum_probe_requests: 90,
  maximum_cleanup_requests: 50,
  maximum_uploaded_object_bytes: 64 * MiB,
  maximum_consumed_get_bytes: 96 * MiB,
  multipart_bytes: 6 * MiB,
  multipart_part_bytes: [5 * MiB, MiB],
  sdk_max_attempts: 1,
  request_timeout_ms: 30_000,
  prohibited_calls: ['ListBuckets', 'CreateBucket', 'DeleteBucket', 'bucket policy changes'],
  response_loss_faults: 'controlled_client_side_drop_after_real_response',
});

const MODES = new Set(['--plan', '--self-test', '--execute', '--local-response-loss-test']);

function hash(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

// Pointers every report this harness writes carries from creation; probe
// results (/range, /multipart, /cleanup_complete) are added as observed.
export const REPORT_POINTERS = Object.freeze([
  ...PROVENANCE_POINTERS,
  '/status',
  '/captured_at',
  '/provider_kind',
  '/local_fixture',
  '/endpoint',
  '/region',
  '/source_bucket',
  '/dest_bucket',
  '/bucket_matrix_mode',
  '/prefix',
  '/owner',
  '/harness_sha256',
  '/plan',
  '/budget',
  '/conditions',
  '/requests',
  '/cleanup',
]);

// The report skeleton execute() persists, bound to the checkout it ran from.
// A --local-response-loss-test run against a disposable loopback RustFS is
// marked as such so it can never be read as AWS acceptance evidence; a real
// run records no endpoint override.
export function createReport(config, { owner, harnessSha256, repo = repoRoot }) {
  const localFixture = config.localFixture === true;
  return {
    status: 'running',
    captured_at: new Date().toISOString(),
    ...auditProvenance(repo),
    provider_kind: localFixture ? 'local-rustfs' : 'aws',
    local_fixture: localFixture,
    endpoint: config.endpoint ?? null,
    region: config.region,
    source_bucket: config.sourceBucket,
    dest_bucket: config.destBucket,
    bucket_matrix_mode: config.sourceBucket === config.destBucket ? 'same_bucket' : 'cross_bucket',
    prefix: config.prefix,
    owner,
    harness_sha256: harnessSha256,
    plan: PLAN,
    budget: {
      probe_requests: 0,
      cleanup_requests: 0,
      uploaded_object_bytes: 0,
      consumed_get_bytes: 0,
    },
    conditions: {},
    requests: [],
    cleanup: { removed: [], retained: [], aborted_uploads: [] },
  };
}

export function validateAwsConfig(input) {
  for (const key of [
    'region',
    'sourceBucket',
    'destBucket',
    'accessKeyId',
    'secretAccessKey',
    'prefix',
  ]) {
    assert(typeof input[key] === 'string' && input[key].trim(), `Missing explicit ${key} input`);
  }
  assert(
    /^[a-z]{2}(?:-gov)?-[a-z]+-\d$/.test(input.region),
    'Region must be an explicit AWS region name'
  );
  assert(
    /^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$/.test(input.sourceBucket) &&
      !input.sourceBucket.includes('..') &&
      !/^\d+\.\d+\.\d+\.\d+$/.test(input.sourceBucket) &&
      /^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$/.test(input.destBucket) &&
      !input.destBucket.includes('..') &&
      !/^\d+\.\d+\.\d+\.\d+$/.test(input.destBucket),
    'Explicit bucket name is invalid'
  );
  assert(
    /^r2-audit\/aws\/[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}\/$/i.test(
      input.prefix
    ),
    'A fresh r2-audit/aws/<UUIDv4>/ prefix is required'
  );
  return { ...input };
}

function canClean(receipt, head, owner) {
  return (
    !!receipt.etag && head.ETag === receipt.etag && head.Metadata?.['r2-audit-owner'] === owner
  );
}

async function drainBody(body) {
  if (!body) return;
  if (typeof body.transformToByteArray === 'function') {
    await body.transformToByteArray();
    return;
  }
  if (Symbol.asyncIterator in Object(body)) {
    for await (const _ of body) {
    }
    return;
  }
  if (typeof body.resume === 'function') {
    await new Promise((resolve, reject) => {
      body.once?.('error', reject);
      body.once?.('end', resolve);
      body.resume();
    });
  }
}

function encodedKeyPath(key) {
  return key.split('/').map(encodeURIComponent).join('/');
}

function installResponseLossFaults(client, faults) {
  const original = client.config.requestHandler;
  const state = Object.fromEntries(
    faults.map((fault) => [fault.id, { ...fault, fired: false, request_count: 0 }])
  );
  client.config.requestHandler = {
    ...original,
    async handle(request, options) {
      const match = Object.values(state).find((fault) => {
        if (fault.fired) return false;
        if (request.method !== fault.method) return false;
        if (!request.path?.includes(encodedKeyPath(fault.key))) return false;
        if (fault.queryKey && !(request.query && fault.queryKey in request.query)) return false;
        return true;
      });
      if (!match) return original.handle(request, options);
      match.request_count++;
      const result = await original.handle(request, options);
      await drainBody(result?.response?.body);
      match.fired = true;
      const error = new Error(`Injected client-side response loss after ${match.id}`);
      error.name = 'InjectedResponseLoss';
      error.$metadata = { httpStatusCode: null, attempts: 1 };
      throw error;
    },
  };
  return state;
}

function assertFaultFiredOnce(state, id) {
  assert.equal(state[id]?.fired, true, `${id} response-loss fault did not fire`);
  assert.equal(state[id]?.request_count, 1, `${id} targeted request count must be exactly one`);
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

async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill('SIGTERM');
  for (let count = 0; count < 50 && child.exitCode === null && child.signalCode === null; count++) {
    await delay(100);
  }
  if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
}

async function startRustfs(binary, root, credentials) {
  const apiPort = await freePort();
  const consolePort = await freePort();
  await mkdir(join(root, 'data'), { recursive: true });
  const endpoint = `http://127.0.0.1:${apiPort}`;
  const child = spawn(
    binary,
    [
      'server',
      '--address',
      `127.0.0.1:${apiPort}`,
      '--console-address',
      `127.0.0.1:${consolePort}`,
      join(root, 'data'),
    ],
    {
      cwd: root,
      env: {
        PATH: process.env.PATH,
        TMPDIR: root,
        RUSTFS_ACCESS_KEY: credentials.accessKeyId,
        RUSTFS_SECRET_KEY: credentials.secretAccessKey,
        RUSTFS_CONSOLE_ENABLE: 'false',
        RUSTFS_OBS_LOGGER_LEVEL: 'warn',
      },
      stdio: ['ignore', 'ignore', 'ignore'],
    }
  );
  try {
    for (let count = 0; count < 300; count++) {
      assert.equal(child.exitCode, null, 'RustFS response-loss fixture exited');
      try {
        if ((await fetch(`${endpoint}/health/ready`, { signal: AbortSignal.timeout(1000) })).ok) {
          return { child, endpoint };
        }
      } catch {}
      await delay(100);
    }
    throw new Error('RustFS response-loss fixture readiness timed out');
  } catch (error) {
    await stop(child);
    throw error;
  }
}

// Importing this module (the manifest self-test checks its report contract)
// must run nothing; only a direct invocation dispatches a mode.
if (isMainModule(process.argv[1], import.meta.filename)) await main();

async function main() {
  const args = process.argv.slice(2);
  assert(
    args.length <= 1 && (!args.length || MODES.has(args[0])),
    'Use --plan, --self-test, --execute, or --local-response-loss-test'
  );
  const mode = args[0] ?? '--plan';
  if (mode === '--plan') {
    console.log(JSON.stringify(PLAN, null, 2));
  } else if (mode === '--self-test') {
    const valid = {
      region: 'us-east-1',
      sourceBucket: 'dedicated-r2-audit-source',
      destBucket: 'dedicated-r2-audit-dest',
      accessKeyId: 'offline-placeholder',
      secretAccessKey: 'offline-placeholder',
      prefix: 'r2-audit/aws/11111111-2222-4333-8444-555555555555/',
    };
    validateAwsConfig(valid);
    for (const change of [
      { region: '' },
      { region: 'auto' },
      { sourceBucket: '' },
      { sourceBucket: '192.168.0.1' },
      { sourceBucket: 'bad..bucket' },
      { destBucket: '' },
      { destBucket: '192.168.0.1' },
      { destBucket: 'bad..bucket' },
      { accessKeyId: '' },
      { secretAccessKey: '' },
      { prefix: 'production/' },
      { prefix: 'r2-audit/11111111-2222-4333-8444-555555555555/' },
    ]) {
      assert.throws(() => validateAwsConfig({ ...valid, ...change }));
    }
    assert(
      canClean({ etag: 'a' }, { ETag: 'a', Metadata: { 'r2-audit-owner': 'owner' } }, 'owner')
    );
    assert(
      !canClean({ etag: 'a' }, { ETag: 'b', Metadata: { 'r2-audit-owner': 'owner' } }, 'owner')
    );
    assert.equal(PLAN.response_loss_faults, 'controlled_client_side_drop_after_real_response');
    const skeleton = { owner: 'offline-owner', harnessSha256: 'offline' };
    const report = createReport(valid, skeleton);
    for (const expression of REPORT_POINTERS)
      assert.notEqual(jsonPointer(report, expression), undefined, `report omits ${expression}`);
    assert.equal(report.provider_kind, 'aws');
    assert.equal(report.local_fixture, false);
    assert.equal(report.endpoint, null);
    const local = createReport(
      { ...valid, endpoint: 'http://127.0.0.1:9000', localFixture: true },
      skeleton
    );
    assert.equal(local.provider_kind, 'local-rustfs');
    assert.equal(local.local_fixture, true);
    assert.equal(local.endpoint, 'http://127.0.0.1:9000');
    console.log(
      'Offline AWS configuration, cleanup guard and report-shape checks passed. No credentials read; no network calls.'
    );
  } else if (mode === '--local-response-loss-test') {
    await localResponseLossTest();
  } else {
    const config = validateAwsConfig({
      region: process.env.AWS_AUDIT_REGION,
      sourceBucket: process.env.AWS_AUDIT_SOURCE_BUCKET,
      destBucket: process.env.AWS_AUDIT_DEST_BUCKET,
      accessKeyId: process.env.AWS_AUDIT_ACCESS_KEY_ID,
      secretAccessKey: process.env.AWS_AUDIT_SECRET_ACCESS_KEY,
      prefix: process.env.AWS_AUDIT_PREFIX,
    });
    await execute(config);
  }
}

async function localResponseLossTest() {
  const binary = resolve(
    process.env.FAULT_AUDIT_PROVIDER_BINARY || '.omx/artifacts/rustfs-runtime/extracted/rustfs'
  );
  const root = await mkdtemp(join(tmpdir(), 'aws-response-loss-rustfs-'));
  const credentials = {
    accessKeyId: `aws-loss-${randomBytes(6).toString('hex')}`,
    secretAccessKey: randomBytes(24).toString('hex'),
  };
  let provider;
  try {
    provider = await startRustfs(binary, root, credentials);
    const result = await execute({
      region: 'us-east-1',
      sourceBucket: 'fault-audit',
      destBucket: 'fault-audit',
      prefix: `r2-audit/aws/${randomUUID()}/`,
      accessKeyId: credentials.accessKeyId,
      secretAccessKey: credentials.secretAccessKey,
      endpoint: provider.endpoint,
      localFixture: true,
    });
    return result;
  } finally {
    if (provider) await stop(provider.child);
    await rm(root, { recursive: true, force: true });
  }
}

async function execute(config) {
  const sdk = await import('@aws-sdk/client-s3');
  const client = new sdk.S3Client({
    region: config.region,
    endpoint: config.endpoint,
    forcePathStyle: !!config.endpoint,
    credentials: { accessKeyId: config.accessKeyId, secretAccessKey: config.secretAccessKey },
    maxAttempts: 1,
    requestChecksumCalculation: 'WHEN_REQUIRED',
    responseChecksumValidation: 'WHEN_REQUIRED',
  });
  if (config.localFixture) {
    await client.send(new sdk.CreateBucketCommand({ Bucket: config.sourceBucket }));
    if (config.destBucket !== config.sourceBucket)
      await client.send(new sdk.CreateBucketCommand({ Bucket: config.destBucket }));
    await client.send(
      new sdk.PutBucketVersioningCommand({
        Bucket: config.sourceBucket,
        VersioningConfiguration: { Status: 'Enabled' },
      })
    );
  }
  const owner = randomUUID();
  // Loopback-fixture reports default to their own file name so they are not
  // mistaken for a real run even before their marker is read.
  const output = resolve(
    process.env.AWS_AUDIT_OUTPUT ||
      `.omx/artifacts/aws-protocol/${config.localFixture ? 'local-' : ''}${owner}.json`
  );
  await mkdir(dirname(output), { recursive: true });
  const owned = new Map();
  const uploads = new Map();
  const uncertain = new Set();
  const report = createReport(config, {
    owner,
    harnessSha256: hash(await readFile(import.meta.filename)),
  });
  const metadata = { 'r2-audit-owner': owner };
  const small = Buffer.alloc(32, 17);
  const replacement = Buffer.alloc(32, 29);
  const content = Buffer.alloc(PLAN.multipart_bytes);
  for (let i = 0; i < content.length; i++) content[i] = (i * 31 + 17) % 251;
  const source = `${config.prefix}中文 +%?# literal%2F.bin`;
  const marker = `${config.prefix}_owner`;
  const wrong = '"00000000000000000000000000000000"';
  const copySource = `${encodeURIComponent(config.sourceBucket)}/${source
    .split('/')
    .map(encodeURIComponent)
    .join('/')}`;
  async function persist() {
    report.owned = [...owned].map(([, receipt]) => ({ ...receipt }));
    report.open_uploads = [...uploads].map(([upload_id, upload]) => ({ upload_id, ...upload }));
    report.uncertain_keys = [...uncertain];
    await writeFile(output, JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
  }
  async function request(name, input, { cleanup = false } = {}) {
    assert(
      typeof input.Key === 'string' &&
        input.Key.startsWith(config.prefix) &&
        input.Key.length > config.prefix.length,
      'Request is outside the fixed prefix scope'
    );
    if (input.UploadId) {
      const upload = uploads.get(input.UploadId);
      assert(upload, 'Unknown multipart upload id');
      assert.equal(upload.bucket, input.Bucket);
      assert.equal(upload.key, input.Key);
    }
    const budgetKey = cleanup ? 'cleanup_requests' : 'probe_requests';
    report.budget[budgetKey]++;
    assert(
      report.budget[budgetKey] <=
        (cleanup ? PLAN.maximum_cleanup_requests : PLAN.maximum_probe_requests),
      'Request budget exhausted'
    );
    if (Buffer.isBuffer(input.Body)) {
      report.budget.uploaded_object_bytes += input.Body.length;
      assert(report.budget.uploaded_object_bytes <= PLAN.maximum_uploaded_object_bytes);
    }
    assert(
      [config.sourceBucket, config.destBucket].includes(input.Bucket),
      'Request uses an undeclared audit bucket'
    );
    const entry = {
      operation: name,
      bucket: input.Bucket,
      key: input.Key,
      phase: cleanup ? 'cleanup' : 'probe',
    };
    report.requests.push(entry);
    await persist();
    try {
      const result = await client.send(new sdk[`${name}Command`](input), {
        abortSignal: AbortSignal.timeout(PLAN.request_timeout_ms),
      });
      entry.status = result.$metadata?.httpStatusCode ?? 200;
      return result;
    } catch (error) {
      entry.status = error.$metadata?.httpStatusCode ?? null;
      entry.error_code = error.name;
      if (
        !cleanup &&
        !['HeadObject', 'GetObject', 'ListParts'].includes(name) &&
        (entry.status === null || entry.status === 408 || entry.status >= 500)
      ) {
        uncertain.add(`${input.Bucket}\0${input.Key}`);
      }
      throw error;
    }
  }
  async function attempt(name, input, options) {
    try {
      return { ok: true, value: await request(name, input, options) };
    } catch (error) {
      return { ok: false, status: error.$metadata?.httpStatusCode ?? null, code: error.name };
    }
  }
  async function claim(bucketName, key, body = small) {
    const result = await request('PutObject', {
      Bucket: bucketName,
      Key: key,
      Body: body,
      IfNoneMatch: '*',
      Metadata: metadata,
    });
    assert(result.ETag, 'PutObject returned no ETag');
    assert(
      result.VersionId && result.VersionId !== 'null',
      'PutObject must return immutable VersionId'
    );
    owned.set(`${bucketName}\0${key}`, {
      bucket: bucketName,
      key,
      etag: result.ETag,
      version_id: result.VersionId,
    });
    return result;
  }
  async function read(bucketName, key, expectedLimit = content.length, extra = {}) {
    const result = await request('GetObject', { Bucket: bucketName, Key: key, ...extra });
    const bytes = Buffer.from(await result.Body.transformToByteArray());
    report.budget.consumed_get_bytes += bytes.length;
    assert(report.budget.consumed_get_bytes <= PLAN.maximum_consumed_get_bytes);
    if (extra.Range) assert.equal(bytes.length, expectedLimit);
    return { bytes, etag: result.ETag, metadata: result.Metadata, version_id: result.VersionId };
  }
  try {
    const markerPut = await claim(config.sourceBucket, marker, Buffer.alloc(64, 7));
    report.versioning = {
      marker_version_id: markerPut.VersionId ?? null,
      required: true,
    };
    assert(markerPut.VersionId, 'Dedicated AWS audit bucket must have versioning enabled');
    const sourcePut = await claim(config.sourceBucket, source, content);
    const range = await read(config.sourceBucket, source, 1024, {
      Range: 'bytes=17-1040',
      IfMatch: sourcePut.ETag,
    });
    assert.deepEqual(range.bytes, content.subarray(17, 1041));
    report.range = {
      exact_bytes: true,
      sha256: hash(range.bytes),
      source_etag: sourcePut.ETag,
      source_version_id: sourcePut.VersionId,
    };
    const sourceCompetition = await attempt('CopyObject', {
      Bucket: config.destBucket,
      Key: `${config.prefix}copy-wrong-source`,
      CopySource: copySource,
      CopySourceIfMatch: wrong,
      Metadata: metadata,
      IfNoneMatch: '*',
    });
    assert.equal(sourceCompetition.status, 412);
    report.conditions.copy_source_wrong_etag_precondition = 'protected';
    report.conditions.source_version_competition = 'not_run';
    const copiedKey = `${config.prefix}copy-ok`;
    const copied = await request('CopyObject', {
      Bucket: config.destBucket,
      Key: copiedKey,
      CopySource: copySource,
      CopySourceIfMatch: sourcePut.ETag,
      MetadataDirective: 'REPLACE',
      Metadata: metadata,
      IfNoneMatch: '*',
    });
    assert(
      copied.VersionId && copied.VersionId !== 'null',
      'CopyObject must return immutable VersionId'
    );
    owned.set(`${config.destBucket}\0${copiedKey}`, {
      bucket: config.destBucket,
      key: copiedKey,
      etag: copied.CopyObjectResult?.ETag ?? null,
      version_id: copied.VersionId,
    });
    assert.equal(hash((await read(config.destBucket, copiedKey)).bytes), hash(content));
    const collision = `${config.prefix}complete-response-loss`;
    const collisionUpload = await request('CreateMultipartUpload', {
      Bucket: config.destBucket,
      Key: collision,
      Metadata: metadata,
    });
    uploads.set(collisionUpload.UploadId, { bucket: config.destBucket, key: collision });
    const parts = [];
    for (const [index, body] of [
      content.subarray(0, 5 * MiB),
      content.subarray(5 * MiB),
    ].entries()) {
      const part = await request('UploadPart', {
        Bucket: config.destBucket,
        Key: collision,
        UploadId: collisionUpload.UploadId,
        PartNumber: index + 1,
        Body: body,
      });
      parts.push({ PartNumber: index + 1, ETag: part.ETag });
    }
    const deleteKey = `${config.prefix}delete-response-loss`;
    const faultState = installResponseLossFaults(client, [
      { id: 'complete-response-loss', method: 'POST', key: collision, queryKey: 'uploadId' },
      { id: 'delete-response-loss', method: 'DELETE', key: deleteKey },
    ]);
    const completed = await attempt('CompleteMultipartUpload', {
      Bucket: config.destBucket,
      Key: collision,
      UploadId: collisionUpload.UploadId,
      MultipartUpload: { Parts: parts },
      IfNoneMatch: '*',
    });
    assert.equal(
      completed.ok,
      false,
      'CompleteMultipartUpload response-loss fault must surface as an error'
    );
    assert.equal(completed.code, 'InjectedResponseLoss');
    assertFaultFiredOnce(faultState, 'complete-response-loss');
    uploads.delete(collisionUpload.UploadId);
    const collisionRead = await read(config.destBucket, collision);
    assert.equal(hash(collisionRead.bytes), hash(content));
    assert(collisionRead.version_id && collisionRead.version_id !== 'null');
    owned.set(`${config.destBucket}\0${collision}`, {
      bucket: config.destBucket,
      key: collision,
      etag: collisionRead.etag ?? null,
      version_id: collisionRead.version_id,
    });
    uncertain.delete(`${config.destBucket}\0${collision}`);
    report.multipart = { exact_bytes: true, bytes: content.length, sha256: hash(content) };
    report.conditions.complete_response_loss = 'reconciled_after_client_side_response_drop';
    report.conditions.complete_response_loss_fault = faultState['complete-response-loss'];
    const deletePut = await claim(config.destBucket, deleteKey, replacement);
    const deleted = await attempt('DeleteObject', {
      Bucket: config.destBucket,
      Key: deleteKey,
      VersionId: deletePut.VersionId,
    });
    assert.equal(deleted.ok, false, 'DeleteObject response-loss fault must surface as an error');
    assert.equal(deleted.code, 'InjectedResponseLoss');
    assertFaultFiredOnce(faultState, 'delete-response-loss');
    const deletedHead = await attempt('HeadObject', {
      Bucket: config.destBucket,
      Key: deleteKey,
      VersionId: deletePut.VersionId,
    });
    assert.equal(deletedHead.status, 404);
    owned.delete(`${config.destBucket}\0${deleteKey}`);
    uncertain.delete(`${config.destBucket}\0${deleteKey}`);
    report.conditions.delete_response_loss = 'reconciled_after_client_side_response_drop';
    report.conditions.delete_response_loss_fault = faultState['delete-response-loss'];
    report.status = 'observations_collected';
  } catch (error) {
    report.status = 'failed';
    report.failure = {
      name: error.name,
      status: error.$metadata?.httpStatusCode ?? null,
      message: error.message,
    };
    process.exitCode = 1;
  } finally {
    for (const [id, key] of uploads) {
      const aborted = await attempt(
        'AbortMultipartUpload',
        { Bucket: key.bucket, Key: key.key, UploadId: id },
        { cleanup: true }
      );
      if (aborted.ok || aborted.status === 404) {
        report.cleanup.aborted_uploads.push({ bucket: key.bucket, key: key.key, upload_id: id });
        uploads.delete(id);
      } else {
        report.cleanup.retained.push({
          bucket: key.bucket,
          key: key.key,
          upload_id: id,
          reason: 'abort_not_confirmed',
        });
      }
    }
    for (const [ownedKey, receipt] of [...owned].reverse()) {
      if (uncertain.has(ownedKey)) {
        report.cleanup.retained.push({
          bucket: receipt.bucket,
          key: receipt.key,
          reason: 'uncertain_mutation',
        });
        continue;
      }
      const head = await attempt(
        'HeadObject',
        { Bucket: receipt.bucket, Key: receipt.key, VersionId: receipt.version_id ?? undefined },
        { cleanup: true }
      );
      if (head.status === 404) {
        owned.delete(ownedKey);
        continue;
      }
      if (!head.ok || !canClean(receipt, head.value, owner)) {
        report.cleanup.retained.push({
          bucket: receipt.bucket,
          key: receipt.key,
          reason: 'ownership_or_etag_not_confirmed',
        });
        continue;
      }
      const removed = await attempt(
        'DeleteObject',
        { Bucket: receipt.bucket, Key: receipt.key, VersionId: receipt.version_id },
        { cleanup: true }
      );
      if (removed.ok || removed.status === 404) {
        owned.delete(ownedKey);
        report.cleanup.removed.push({ bucket: receipt.bucket, key: receipt.key });
      } else
        report.cleanup.retained.push({
          bucket: receipt.bucket,
          key: receipt.key,
          reason: 'delete_not_confirmed',
        });
    }
    report.cleanup_complete = !owned.size && !uploads.size && !uncertain.size;
    if (!report.cleanup_complete) process.exitCode = 1;
    await persist();
  }
  console.log(JSON.stringify(report, null, 2));
}
