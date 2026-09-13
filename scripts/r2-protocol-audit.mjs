/** Prepared remote R2 acceptance. --plan and --self-test never read credentials or use the network.
 * --execute requires explicit dedicated-bucket inputs. No discovery or bucket-management calls.
 */
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const MiB = 1024 * 1024;
const PLAN = Object.freeze({
  status: 'prepared_not_executed',
  required_inputs: [
    'R2_AUDIT_ENDPOINT',
    'R2_AUDIT_BUCKET',
    'R2_AUDIT_ACCESS_KEY_ID',
    'R2_AUDIT_SECRET_ACCESS_KEY',
    'R2_AUDIT_PREFIX',
  ],
  prefix_format: 'r2-audit/<fresh UUIDv4>/',
  maximum_probe_requests: 50,
  maximum_cleanup_requests: 30,
  maximum_total_requests: 80,
  maximum_uploaded_object_bytes: 20 * MiB,
  maximum_consumed_get_bytes: 32 * MiB,
  maximum_created_object_keys: 9,
  maximum_created_multipart_uploads: 3,
  multipart_bytes: 6 * MiB,
  multipart_part_bytes: [5 * MiB, MiB],
  sdk_max_attempts: 1,
  request_timeout_ms: 30_000,
  probe_deadline_ms: 8 * 60_000,
  same_key_mutation_spacing_ms: 1100,
  prohibited_calls: [
    'ListBuckets',
    'ListObjects',
    'ListObjectsV2',
    'ListMultipartUploads',
    'CreateBucket',
    'DeleteBucket',
    'bucket configuration',
  ],
});
const MODES = new Set(['--plan', '--self-test', '--execute']);
const args = process.argv.slice(2);
assert(
  args.length <= 1 && (!args.length || MODES.has(args[0])),
  'Use --plan, --self-test, or --execute'
);
const mode = args[0] ?? '--plan';
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');

function validateConfig(input) {
  for (const key of ['endpoint', 'bucket', 'accessKeyId', 'secretAccessKey', 'prefix'])
    assert(typeof input[key] === 'string' && input[key].trim(), `Missing explicit ${key} input`);
  const endpoint = new URL(input.endpoint);
  assert(
    endpoint.protocol === 'https:' &&
      /^[a-f0-9]{32}(?:\.(?:eu|us|fedramp))?\.r2\.cloudflarestorage\.com$/i.test(endpoint.hostname),
    'Endpoint must be an official HTTPS R2 S3 endpoint'
  );
  assert(
    !endpoint.username &&
      !endpoint.password &&
      !endpoint.port &&
      endpoint.pathname === '/' &&
      !endpoint.search &&
      !endpoint.hash,
    'Endpoint must not contain credentials, a path, a port, or query parameters'
  );
  assert(
    /^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$/.test(input.bucket),
    'Explicit bucket name is invalid'
  );
  assert(
    /^r2-audit\/[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}\/$/i.test(
      input.prefix
    ),
    'A fresh r2-audit/<UUIDv4>/ prefix is required'
  );
  return { ...input, endpoint: endpoint.origin };
}
function classify(result, unchanged) {
  if (result.ok) {
    assert(!unchanged, 'Successful negative-condition probe did not show its expected mutation');
    return 'ignored';
  }
  assert(unchanged, 'Rejected negative-condition probe changed object data');
  if (result.status === 412) return 'enforced';
  assert(
    [400, 405, 501].includes(result.status),
    `Unexpected condition response: ${result.status ?? 'transport error'}`
  );
  return 'rejected_unsupported';
}
function canClean(receipt, head, owner) {
  return (
    !!receipt.etag && head.ETag === receipt.etag && head.Metadata?.['r2-audit-owner'] === owner
  );
}

if (mode === '--plan') {
  console.log(JSON.stringify(PLAN, null, 2));
} else if (mode === '--self-test') {
  const valid = {
    endpoint: `https://${'a'.repeat(32)}.r2.cloudflarestorage.com`,
    bucket: 'dedicated-test',
    accessKeyId: 'offline-placeholder',
    secretAccessKey: 'offline-placeholder',
    prefix: 'r2-audit/11111111-2222-4333-8444-555555555555/',
  };
  validateConfig(valid);
  for (const change of [
    { endpoint: 'http://localhost:9000' },
    { endpoint: `${valid.endpoint}/other` },
    { endpoint: `${valid.endpoint}.evil.example` },
    { bucket: '' },
    { accessKeyId: '' },
    { secretAccessKey: '' },
    { prefix: 'production/' },
    { prefix: '../escape/' },
  ])
    assert.throws(() => validateConfig({ ...valid, ...change }));
  assert.equal(classify({ ok: false, status: 412 }, true), 'enforced');
  assert.equal(classify({ ok: false, status: 501 }, true), 'rejected_unsupported');
  assert.equal(classify({ ok: true }, false), 'ignored');
  assert.throws(() => classify({ ok: false, status: 412 }, false));
  assert.throws(() => classify({ ok: false, status: 403 }, true));
  assert(canClean({ etag: 'a' }, { ETag: 'a', Metadata: { 'r2-audit-owner': 'owner' } }, 'owner'));
  assert(!canClean({ etag: 'a' }, { ETag: 'b', Metadata: { 'r2-audit-owner': 'owner' } }, 'owner'));
  assert(!canClean({ etag: 'a' }, { ETag: 'a', Metadata: { 'r2-audit-owner': 'other' } }, 'owner'));
  console.log(
    'Offline configuration, condition-classification and cleanup-ownership checks passed. No credentials read; no network calls.'
  );
} else {
  // This is the only branch that reads explicitly supplied credential inputs.
  const config = validateConfig({
    endpoint: process.env.R2_AUDIT_ENDPOINT,
    bucket: process.env.R2_AUDIT_BUCKET,
    accessKeyId: process.env.R2_AUDIT_ACCESS_KEY_ID,
    secretAccessKey: process.env.R2_AUDIT_SECRET_ACCESS_KEY,
    prefix: process.env.R2_AUDIT_PREFIX,
  });
  await execute(config);
}

async function execute(config) {
  const sdk = await import('@aws-sdk/client-s3');
  const client = new sdk.S3Client({
    endpoint: config.endpoint,
    region: 'auto',
    forcePathStyle: true,
    credentials: { accessKeyId: config.accessKeyId, secretAccessKey: config.secretAccessKey },
    maxAttempts: 1,
    requestChecksumCalculation: 'WHEN_REQUIRED',
    responseChecksumValidation: 'WHEN_REQUIRED',
  });
  const owner = randomUUID();
  const owned = new Map();
  const uploads = new Map();
  const uncertain = new Set();
  const lastMutation = new Map();
  const started = Date.now();
  const output = resolve(process.env.R2_AUDIT_OUTPUT || `.omx/artifacts/r2-protocol/${owner}.json`);
  await mkdir(dirname(output), { recursive: true });
  const report = {
    status: 'running',
    captured_at: new Date().toISOString(),
    endpoint: config.endpoint,
    bucket: config.bucket,
    prefix: config.prefix,
    owner,
    harness_sha256: hash(await readFile(import.meta.filename)),
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
  const metadata = { 'r2-audit-owner': owner };
  const small = Buffer.alloc(32, 17);
  const replacement = Buffer.alloc(32, 29);
  const content = Buffer.alloc(6 * MiB);
  for (let i = 0; i < content.length; i++) content[i] = (i * 31 + 17) % 251;
  const wrong = '"00000000000000000000000000000000"';
  const marker = `${config.prefix}_owner`;
  const source = `${config.prefix}中文 +%?# literal%2F.bin`;
  const copySource = `${encodeURIComponent(config.bucket)}/${source.split('/').map(encodeURIComponent).join('/')}`;
  const allowed = new Set([
    'PutObject',
    'GetObject',
    'HeadObject',
    'CopyObject',
    'DeleteObject',
    'CreateMultipartUpload',
    'UploadPart',
    'UploadPartCopy',
    'CompleteMultipartUpload',
    'AbortMultipartUpload',
    'ListParts',
  ]);
  async function persist() {
    report.owned = [...owned].map(([key, receipt]) => ({ key, ...receipt }));
    report.open_uploads = [...uploads].map(([upload_id, key]) => ({ upload_id, key }));
    report.uncertain_keys = [...uncertain];
    await writeFile(output, JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
  }
  async function request(name, input, { cleanup = false, headers = {} } = {}) {
    assert(
      allowed.has(name) &&
        typeof input.Key === 'string' &&
        input.Key.startsWith(config.prefix) &&
        input.Key.length > config.prefix.length,
      'Request is outside the fixed operation/prefix scope'
    );
    assert(input.Bucket === undefined, 'Bucket must come from the explicit configuration');
    if (input.CopySource)
      assert.equal(input.CopySource, copySource, "Copy source must be this run's fixture");
    if (input.UploadId)
      assert.equal(
        uploads.get(input.UploadId),
        input.Key,
        'Multipart upload is not owned by this run'
      );
    if (
      name === 'CompleteMultipartUpload' ||
      ['GetObject', 'HeadObject', 'CopyObject', 'DeleteObject'].includes(name)
    )
      assert(owned.has(input.Key), 'Object was not created by this run');
    if (name === 'PutObject' && !owned.has(input.Key))
      assert.equal(input.IfNoneMatch, '*', 'New fixtures must be created conditionally');
    const budgetKey = cleanup ? 'cleanup_requests' : 'probe_requests';
    assert(
      report.budget[budgetKey] <
        (cleanup ? PLAN.maximum_cleanup_requests : PLAN.maximum_probe_requests),
      'Request budget exhausted'
    );
    if (!cleanup) assert(Date.now() - started < PLAN.probe_deadline_ms, 'Probe deadline exhausted');
    if (Buffer.isBuffer(input.Body)) {
      assert(
        report.budget.uploaded_object_bytes + input.Body.length <=
          PLAN.maximum_uploaded_object_bytes,
        'Upload data budget exhausted'
      );
      report.budget.uploaded_object_bytes += input.Body.length;
    }
    const mutation = !['GetObject', 'HeadObject', 'ListParts'].includes(name);
    if (mutation) {
      await delay(
        Math.max(
          0,
          (lastMutation.get(input.Key) ?? 0) + PLAN.same_key_mutation_spacing_ms - Date.now()
        )
      );
      lastMutation.set(input.Key, Date.now());
    }
    report.budget[budgetKey]++;
    const entry = {
      operation: name,
      key: input.Key,
      phase: cleanup ? 'cleanup' : 'probe',
      status: 'dispatched',
    };
    report.requests.push(entry);
    await persist();
    const command = new sdk[`${name}Command`]({ Bucket: config.bucket, ...input });
    if (Object.keys(headers).length)
      command.middlewareStack.add(
        (next) => async (args) => {
          Object.assign(args.request.headers, headers);
          return next(args);
        },
        { step: 'build', name: 'r2AuditConditionalHeaders' }
      );
    try {
      const result = await client.send(command, {
        abortSignal: AbortSignal.timeout(PLAN.request_timeout_ms),
      });
      entry.status = result.$metadata?.httpStatusCode ?? 200;
      return result;
    } catch (error) {
      entry.status = error.$metadata?.httpStatusCode ?? null;
      entry.error_code = error.name;
      if (
        mutation &&
        (entry.status === null ||
          entry.status === 408 ||
          (entry.status >= 500 && entry.status !== 501))
      )
        uncertain.add(input.Key);
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
  async function claim(key, body = small) {
    assert(
      owned.size < PLAN.maximum_created_object_keys && !owned.has(key),
      'Fixture key limit or duplicate claim'
    );
    const result = await request('PutObject', {
      Key: key,
      IfNoneMatch: '*',
      Body: body,
      Metadata: metadata,
    });
    owned.set(key, { etag: result.ETag ?? null });
    assert(result.ETag, 'Created fixture returned no ETag; retained for inspection');
    await persist();
    return result.ETag;
  }
  async function read(key, expectedLimit = content.length, extra = {}) {
    const result = await request('GetObject', { Key: key, ...extra });
    if (extra.Range) {
      const match = /^bytes=(\d+)-(\d+)$/.exec(extra.Range);
      if (
        result.$metadata.httpStatusCode !== 206 ||
        result.ContentRange !== `bytes ${match[1]}-${match[2]}/${content.length}` ||
        result.ETag !== extra.IfMatch
      ) {
        result.Body.destroy?.();
        throw new Error('Range status, boundaries or source identity mismatch');
      }
    }
    const chunks = [];
    let count = 0;
    const timer = setTimeout(
      () => result.Body.destroy?.(new Error('Body deadline exceeded')),
      PLAN.request_timeout_ms
    );
    try {
      for await (const chunk of result.Body) {
        count += chunk.length;
        report.budget.consumed_get_bytes += chunk.length;
        assert(
          count <= expectedLimit &&
            report.budget.consumed_get_bytes <= PLAN.maximum_consumed_get_bytes,
          'Response data budget exceeded'
        );
        chunks.push(chunk);
      }
    } finally {
      clearTimeout(timer);
    }
    if (extra.Range) assert.equal(count, expectedLimit, 'Short range response');
    return { bytes: Buffer.concat(chunks), etag: result.ETag, metadata: result.Metadata };
  }
  function receipt(key, result) {
    assert.equal(result.metadata?.['r2-audit-owner'], owner, 'Fixture ownership metadata changed');
    assert(result.etag, 'Fixture response omitted ETag');
    owned.set(key, { etag: result.etag });
  }
  async function condition(
    name,
    operation,
    extra,
    { headers = {}, successBytes = replacement } = {}
  ) {
    const key = `${config.prefix}${name}`;
    await claim(key);
    const result = await attempt(operation, { Key: key, ...extra }, { headers });
    const current = await read(key);
    const behavior = classify(result, current.bytes.equals(small));
    assert(
      current.bytes.equals(result.ok ? successBytes : small),
      'Conditional fixture contains unexpected bytes'
    );
    receipt(key, current);
    report.conditions[name] = { behavior, status: result.status ?? 200 };
    return key;
  }
  async function createUpload(key) {
    assert(uploads.size < PLAN.maximum_created_multipart_uploads);
    const result = await request('CreateMultipartUpload', { Key: key, Metadata: metadata });
    assert(result.UploadId, 'CreateMultipartUpload response omitted the upload ID');
    uploads.set(result.UploadId, key);
    await persist();
    return result.UploadId;
  }
  async function uploadTwo(key, id) {
    const parts = [];
    for (const [index, body] of [
      content.subarray(0, 5 * MiB),
      content.subarray(5 * MiB),
    ].entries()) {
      const result = await request('UploadPart', {
        Key: key,
        UploadId: id,
        PartNumber: index + 1,
        Body: body,
      });
      assert(result.ETag, 'UploadPart omitted its ETag');
      parts.push({ PartNumber: index + 1, ETag: result.ETag });
    }
    return parts;
  }
  try {
    await claim(marker, Buffer.alloc(64, 7));
    const sourceEtag = await claim(source, content);
    const range = await read(source, 1024, { Range: 'bytes=17-1040', IfMatch: sourceEtag });
    assert.deepEqual(range.bytes, content.subarray(17, 1041));
    report.range = { exact_bytes: true, sha256: hash(range.bytes), source_etag: sourceEtag };
    const wrongRead = await attempt('GetObject', {
      Key: source,
      Range: 'bytes=0-0',
      IfMatch: wrong,
    });
    if (wrongRead.ok) wrongRead.value.Body.destroy?.();
    else
      assert([400, 405, 412, 501].includes(wrongRead.status), 'Unexpected conditional GET error');
    report.conditions.get_match = {
      behavior: wrongRead.ok
        ? 'ignored'
        : wrongRead.status === 412
          ? 'enforced'
          : 'rejected_unsupported',
      status: wrongRead.status ?? 200,
    };
    await condition('put_absent', 'PutObject', {
      IfNoneMatch: '*',
      Body: replacement,
      Metadata: metadata,
    });
    const matchKey = await condition('put_match', 'PutObject', {
      IfMatch: wrong,
      Body: replacement,
      Metadata: metadata,
    });
    if (report.conditions.put_match.behavior === 'enforced') {
      await request('PutObject', {
        Key: matchKey,
        IfMatch: owned.get(matchKey).etag,
        Body: replacement,
        Metadata: metadata,
      });
      const changed = await read(matchKey, 32);
      assert(changed.bytes.equals(replacement));
      receipt(matchKey, changed);
      report.conditions.put_match.matching_request_succeeded = true;
    }
    const copyKey = await condition(
      'copy_source_match',
      'CopyObject',
      { CopySource: copySource, CopySourceIfMatch: wrong },
      { successBytes: content }
    );
    if (report.conditions.copy_source_match.behavior === 'enforced') {
      await request('CopyObject', {
        Key: copyKey,
        CopySource: copySource,
        CopySourceIfMatch: sourceEtag,
      });
      const copied = await read(copyKey);
      assert.equal(hash(copied.bytes), hash(content));
      receipt(copyKey, copied);
      report.conditions.copy_source_match.matching_request_succeeded = true;
    }
    await condition(
      'copy_destination_absent',
      'CopyObject',
      { CopySource: copySource, CopySourceIfMatch: sourceEtag },
      { headers: { 'cf-copy-destination-if-none-match': '*' }, successBytes: content }
    );
    const deleteKey = `${config.prefix}delete_match`;
    await claim(deleteKey);
    const deletion = await attempt('DeleteObject', { Key: deleteKey, IfMatch: wrong });
    const deleteHead = await attempt('HeadObject', { Key: deleteKey });
    if (deleteHead.ok) {
      const current = await read(deleteKey, 32);
      assert(current.bytes.equals(small));
      receipt(deleteKey, current);
      report.conditions.delete_match = {
        behavior: classify(deletion, true),
        status: deletion.status ?? 200,
      };
      if (report.conditions.delete_match.behavior === 'enforced') {
        await request('DeleteObject', { Key: deleteKey, IfMatch: current.etag });
        assert.equal((await attempt('HeadObject', { Key: deleteKey })).status, 404);
        owned.delete(deleteKey);
        report.conditions.delete_match.matching_request_succeeded = true;
      }
    } else {
      assert.equal(deleteHead.status, 404);
      assert(deletion.ok);
      owned.delete(deleteKey);
      report.conditions.delete_match = { behavior: 'ignored', status: 204 };
    }
    const collision = `${config.prefix}complete_absent`;
    const collisionUpload = await createUpload(collision);
    const collisionParts = await uploadTwo(collision, collisionUpload);
    await claim(collision); // Claim after uploading parts; never publish over a preexisting key.
    const completion = await attempt('CompleteMultipartUpload', {
      Key: collision,
      UploadId: collisionUpload,
      IfNoneMatch: '*',
      MultipartUpload: { Parts: collisionParts },
    });
    const collisionBytes = await read(collision);
    report.conditions.complete_absent = {
      behavior: classify(completion, collisionBytes.bytes.equals(small)),
      status: completion.status ?? 200,
    };
    assert(collisionBytes.bytes.equals(completion.ok ? content : small));
    receipt(collision, collisionBytes);
    if (completion.ok) uploads.delete(collisionUpload);
    const exact = `${config.prefix}multipart_exact`;
    const exactUpload = await createUpload(exact);
    const parts = await uploadTwo(exact, exactUpload);
    const first = await request('ListParts', { Key: exact, UploadId: exactUpload, MaxParts: 1 });
    assert(first.IsTruncated && first.NextPartNumberMarker, 'Expected a multipart continuation');
    const last = await request('ListParts', {
      Key: exact,
      UploadId: exactUpload,
      MaxParts: 1,
      PartNumberMarker: first.NextPartNumberMarker,
    });
    assert.equal(last.IsTruncated, false);
    assert.deepEqual(
      [...(first.Parts ?? []), ...(last.Parts ?? [])].map(({ PartNumber, ETag }) => ({
        PartNumber,
        ETag,
      })),
      parts
    );
    await claim(exact); // Unconditional completion below can only replace our own seed object.
    await request('CompleteMultipartUpload', {
      Key: exact,
      UploadId: exactUpload,
      MultipartUpload: { Parts: parts },
    });
    uploads.delete(exactUpload);
    const exactBytes = await read(exact);
    assert.equal(hash(exactBytes.bytes), hash(content));
    receipt(exact, exactBytes);
    report.multipart = {
      exact_bytes: true,
      bytes: content.length,
      part_bytes: PLAN.multipart_part_bytes,
      list_pages: 2,
      sha256: hash(exactBytes.bytes),
    };
    const partKey = `${config.prefix}part_copy`;
    const partUpload = await createUpload(partKey);
    const partResult = await attempt('UploadPartCopy', {
      Key: partKey,
      UploadId: partUpload,
      PartNumber: 1,
      CopySource: copySource,
      CopySourceRange: `bytes=0-${5 * MiB - 1}`,
      CopySourceIfMatch: wrong,
    });
    const inventory = await request('ListParts', { Key: partKey, UploadId: partUpload });
    report.conditions.part_copy_source_match = {
      behavior: classify(partResult, !(inventory.Parts ?? []).length),
      status: partResult.status ?? 200,
    };
    report.status = 'observations_collected';
  } catch (error) {
    report.status = 'failed';
    report.failure = {
      code: error.name,
      status: error.$metadata?.httpStatusCode ?? null,
      detail:
        error.name === 'AssertionError' || error.constructor === Error
          ? error.message
          : 'S3 operation failed; see request code/status',
    };
    process.exitCode = 1;
  } finally {
    for (const [id, key] of uploads) {
      const result = await attempt(
        'AbortMultipartUpload',
        { Key: key, UploadId: id },
        { cleanup: true }
      );
      if (result.ok || result.status === 404) {
        uploads.delete(id);
        report.cleanup.aborted_uploads.push({ key, upload_id: id });
      } else report.cleanup.retained.push({ key, upload_id: id, reason: 'abort_not_confirmed' });
    }
    for (const [key, recorded] of [...owned].reverse()) {
      if (
        uncertain.has(key) ||
        (key === marker && (report.cleanup.retained.length || uploads.size || uncertain.size))
      ) {
        report.cleanup.retained.push({ key, reason: 'uncertain_operation_or_retained_fixture' });
        continue;
      }
      const head = await attempt('HeadObject', { Key: key }, { cleanup: true });
      if (head.status === 404) {
        owned.delete(key);
        continue;
      }
      if (!head.ok || !canClean(recorded, head.value, owner)) {
        report.cleanup.retained.push({ key, reason: 'ownership_or_etag_not_confirmed' });
        continue;
      }
      // This deletes only exact keys successfully claimed by this run, within its reserved prefix.
      // Conditional DELETE remains an observed capability, not an assumed cleanup guarantee.
      const removed = await attempt('DeleteObject', { Key: key }, { cleanup: true });
      const absent = removed.ok
        ? await attempt('HeadObject', { Key: key }, { cleanup: true })
        : null;
      if (absent?.status === 404) {
        owned.delete(key);
        report.cleanup.removed.push(key);
      } else report.cleanup.retained.push({ key, reason: 'delete_not_confirmed' });
    }
    report.cleanup_complete = !owned.size && !uploads.size && !uncertain.size;
    if (!report.cleanup_complete) process.exitCode = 1;
    await persist();
    client.destroy();
    console.log(
      JSON.stringify(
        {
          status: report.status,
          output,
          budget: report.budget,
          conditions: report.conditions,
          cleanup_complete: report.cleanup_complete,
        },
        null,
        2
      )
    );
  }
}
