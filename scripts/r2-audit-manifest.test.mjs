import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { productionSourceFingerprint } from './audit-source.mjs';
import { classifyGate, createAcceptanceStatus, normalizeStatus } from './r2-audit-manifest.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(scriptsDir, '..');

// Load the committed manifest itself (not a re-declared inline gate) so these
// tests fail if a gate's pointer or `equals`/`expect` value drifts, not just
// if classifyGate()'s logic regresses.
const committedManifest = JSON.parse(
  readFileSync(join(repoRoot, 'docs/engineering/r2-audit/acceptance-manifest.json'), 'utf8')
);

function committedGate(id) {
  const gate = committedManifest.gates.find((candidate) => candidate.id === id);
  assert.ok(gate, `gate "${id}" not found in the committed acceptance-manifest.json`);
  return gate;
}

test('normalizes status values and rejects unknown labels', () => {
  assert.equal(normalizeStatus('passed'), 'passed');
  assert.equal(normalizeStatus('prerequisite_only'), 'prerequisite_only');
  assert.equal(normalizeStatus('historical_pass'), 'historical_pass');
  assert.equal(normalizeStatus('not_implemented'), 'not_implemented');
  assert.throws(() => normalizeStatus('green'));
});

test('classifies prerequisite-only gates separately from runtime acceptance', () => {
  const gate = classifyGate(
    { id: 'windows-nfs', status_when_missing: 'prerequisite_only', evidence: 'windows.json' },
    null
  );
  assert.equal(gate.id, 'windows-nfs');
  assert.equal(gate.status, 'prerequisite_only');
  assert.equal(gate.evidence, null);
  assert.equal(gate.reason, 'missing evidence file: windows.json');
});

test('classifies real provider evidence without broadening limited results', async () => {
  const root = await mkdtemp(join(tmpdir(), 'r2-audit-manifest-'));
  try {
    const auditDir = join(root, 'docs/engineering/r2-audit');
    await mkdir(auditDir, { recursive: true });
    const evidencePath = join(auditDir, 'real-minio.json');
    await writeFile(
      evidencePath,
      JSON.stringify({
        scope: 'Real source-built MinIO servers on loopback',
        minio_commit: 'abc123',
        moves: [{ status: 'needs_action' }, { status: 'success' }],
        nfs: { write_read_unmount: 'passed', rename_delete: 'safely rejected' },
      })
    );
    const gate = classifyGate(
      {
        id: 'minio-native',
        evidence: 'real-minio.json',
        status_from: '/moves/0/status',
        map_status: { needs_action: 'limited', success: 'passed' },
        provider_version_from: '/minio_commit',
        mode: 'real_loopback_native',
      },
      root
    );
    assert.equal(gate.status, 'limited');
    assert.equal(gate.provider_version, 'abc123');
    assert.equal(gate.mode, 'real_loopback_native');
    assert.equal(gate.evidence.file, 'real-minio.json');
    assert.match(gate.evidence.sha256, /^[a-f0-9]{64}$/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('downgrades passing evidence that is not bound to the current git tree', async () => {
  const root = await mkdtemp(join(tmpdir(), 'r2-audit-stale-'));
  try {
    const auditDir = join(root, 'docs/engineering/r2-audit');
    await mkdir(auditDir, { recursive: true });
    await writeFile(join(auditDir, 'evidence.json'), JSON.stringify({ completed: true }));
    const gate = classifyGate(
      {
        id: 'rustfs-native',
        evidence: 'evidence.json',
        status_from: '/completed',
        map_status: { true: 'passed' },
        require_current_git: true,
      },
      root,
      { commit: 'current', tree: 'tree' }
    );
    assert.equal(gate.status, 'historical_pass');
    assert.match(gate.reason, /not bound to current git/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('downgrades limited evidence that is missing required provenance instead of leaving it unexplained', async () => {
  const root = await mkdtemp(join(tmpdir(), 'r2-audit-stale-limited-'));
  try {
    const auditDir = join(root, 'docs/engineering/r2-audit');
    await mkdir(auditDir, { recursive: true });
    await writeFile(
      join(auditDir, 'real-minio.json'),
      JSON.stringify({
        moves: [{ status: 'needs_action' }, { status: 'success' }],
        nfs: { write_read_unmount: 'passed' },
      })
    );
    const gate = classifyGate(
      {
        id: 'minio-native',
        evidence: 'real-minio.json',
        required_checks: [
          { pointer: '/moves/0/status', expect: ['success', 'needs_action'] },
          { pointer: '/moves/1/status', expect: 'success' },
          { pointer: '/nfs/write_read_unmount', expect: 'passed' },
        ],
        limited_when: [{ pointer: '/moves/0/status', equals: 'needs_action' }],
        require_current_source: true,
        source_fingerprint_from: '/source_fingerprint_before/production_source_sha256',
      },
      root,
      { commit: 'current', tree: 'tree' },
      { production_source_sha256: 'current-source' }
    );
    // Before the fix this evidence would classify as 'limited' with reason
    // left undefined (an empty note); provenance must be checked for
    // limited results too, and the note must explain why.
    assert.equal(gate.status, 'historical_limited');
    assert.ok(gate.reason, 'expected an explicit note, not an empty one');
    assert.match(gate.reason, /missing production source fingerprint/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('committed power-loss-matrix gate treats a smoke-mode run as limited, never a full pass', async () => {
  const gate = committedGate('power-loss-matrix');
  const root = await mkdtemp(join(tmpdir(), 'r2-audit-power-loss-smoke-'));
  try {
    const auditDir = join(root, 'docs/engineering/r2-audit');
    await mkdir(auditDir, { recursive: true });
    const sourceFingerprint = { production_source_sha256: 'current-source' };
    // A --case run: real VM execution, but a subset of the matrix, "recorded
    // as smoke, never full matrix" per vm-powerloss-audit.py's own --case
    // help text. This must never be indistinguishable from the full
    // kernel_nfs_ack_vm_powercut_matrix run.
    await writeFile(
      join(auditDir, gate.evidence),
      JSON.stringify({
        passed: true,
        mode: 'smoke',
        build: {
          production_source_sha256: sourceFingerprint.production_source_sha256,
          binary_sha256: 'bin',
        },
      })
    );
    const classified = classifyGate(gate, root, null, sourceFingerprint);
    assert.equal(classified.status, 'limited');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('committed cloudflare-r2-protocol gate treats a safely-refused condition as limited, never a full pass', async () => {
  const gate = committedGate('cloudflare-r2-protocol');
  const root = await mkdtemp(join(tmpdir(), 'r2-audit-r2-protocol-limited-'));
  try {
    const auditDir = join(root, 'docs/engineering/r2-audit');
    await mkdir(auditDir, { recursive: true });
    const git = { commit: 'current-commit', tree: 'current-tree' };
    // R2 safely refusing a conditional operation it does not support is a
    // capability boundary, not a successful enforced pass.
    await writeFile(
      join(auditDir, gate.evidence),
      JSON.stringify({
        status: 'observations_collected',
        range: { exact_bytes: true },
        multipart: { exact_bytes: true },
        cleanup_complete: true,
        conditions: {
          get_match: { behavior: 'rejected_unsupported' },
          put_absent: { behavior: 'enforced' },
          copy_source_match: { behavior: 'enforced' },
        },
        git: { commit: git.commit, tree: git.tree },
      })
    );
    const classified = classifyGate(gate, root, git, null);
    assert.equal(classified.status, 'limited');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('fails aggregate gates when any required subcheck fails', async () => {
  const root = await mkdtemp(join(tmpdir(), 'r2-audit-aggregate-'));
  try {
    const auditDir = join(root, 'docs/engineering/r2-audit');
    await mkdir(auditDir, { recursive: true });
    await writeFile(
      join(auditDir, 'real-minio.json'),
      JSON.stringify({
        moves: [{ status: 'success' }, { status: 'failed' }],
        nfs: { write_read_unmount: 'passed' },
      })
    );
    const gate = classifyGate(
      {
        id: 'minio-native',
        evidence: 'real-minio.json',
        required_checks: [
          { pointer: '/moves/0/status', expect: ['success', 'needs_action'] },
          { pointer: '/moves/1/status', expect: 'success' },
          { pointer: '/nfs/write_read_unmount', expect: 'passed' },
        ],
        limited_when: [{ pointer: '/moves/0/status', equals: 'needs_action' }],
      },
      root
    );
    assert.equal(gate.status, 'failed');
    assert(gate.reason.includes('/moves/1/status'));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('does not let observations_collected hide failed protocol expectations', async () => {
  const root = await mkdtemp(join(tmpdir(), 'r2-audit-r2-aggregate-'));
  try {
    const auditDir = join(root, 'docs/engineering/r2-audit');
    await mkdir(auditDir, { recursive: true });
    await writeFile(
      join(auditDir, 'real-r2-protocol.json'),
      JSON.stringify({
        status: 'observations_collected',
        conditions: { get_match: { behavior: 'transport_error' } },
      })
    );
    const gate = classifyGate(
      {
        id: 'r2-protocol',
        evidence: 'real-r2-protocol.json',
        required_checks: [
          { pointer: '/status', expect: 'observations_collected' },
          {
            pointer: '/conditions/get_match/behavior',
            expect: ['enforced', 'rejected_unsupported'],
          },
        ],
      },
      root
    );
    assert.equal(gate.status, 'failed');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('missing harnesses cannot claim a prepared status', async () => {
  const root = await mkdtemp(join(tmpdir(), 'r2-audit-missing-harness-'));
  try {
    const status = await createAcceptanceStatus({
      repoRoot: root,
      manifest: {
        schema_version: 1,
        gates: [
          {
            id: 'aws-s3',
            status_when_missing: 'prepared_not_executed',
            evidence: 'missing.json',
            harnesses: ['missing-harness.mjs'],
          },
        ],
      },
      git: { commit: 'current', tree: 'tree', dirty: false },
      platform: { os: 'test-os', arch: 'test-arch' },
      capturedAt: '2026-09-22T00:00:00.000Z',
    });
    assert.equal(status.gates[0].status, 'not_implemented');
    assert.match(status.gates[0].reason, /missing harness/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('creates a full status document with exact repository and harness metadata', async () => {
  const root = await mkdtemp(join(tmpdir(), 'r2-audit-status-'));
  try {
    const auditDir = join(root, 'docs/engineering/r2-audit');
    await mkdir(auditDir, { recursive: true });
    await writeFile(join(auditDir, 'evidence.json'), JSON.stringify({ completed: true }));
    await writeFile(join(root, 'harness.mjs'), 'console.log("ok")\n');
    const status = await createAcceptanceStatus({
      repoRoot: root,
      manifest: {
        schema_version: 1,
        gates: [
          {
            id: 'rustfs-protocol',
            evidence: 'evidence.json',
            status_from: '/completed',
            map_status: { true: 'passed' },
            harnesses: ['harness.mjs'],
          },
          {
            id: 'r2-protocol',
            status_when_missing: 'prepared_not_executed',
            evidence: 'missing.json',
            harnesses: ['harness.mjs'],
          },
        ],
      },
      git: {
        commit: '8bfb0b1826cb2c35a102c726c26ff30b2c54f350',
        tree: 'tree-hash',
        dirty: true,
      },
      platform: { os: 'test-os', arch: 'test-arch' },
      capturedAt: '2026-09-22T00:00:00.000Z',
    });
    assert.equal(status.git.commit, '8bfb0b1826cb2c35a102c726c26ff30b2c54f350');
    assert.equal(status.gates[0].status, 'passed');
    assert.equal(status.gates[1].status, 'prepared_not_executed');
    assert.equal(status.harnesses['harness.mjs'].sha256.length, 64);
    assert.equal(status.summary.passed, 1);
    assert.equal(status.summary.prepared_not_executed, 1);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('production source fingerprint tracks code inputs but ignores generated audit docs', async () => {
  const root = await mkdtemp(join(tmpdir(), 'r2-audit-source-'));
  try {
    await mkdir(join(root, 'src/app'), { recursive: true });
    await mkdir(join(root, 'src-tauri/src'), { recursive: true });
    await mkdir(join(root, 'docs/engineering/r2-audit'), { recursive: true });
    await writeFile(join(root, 'package.json'), '{"name":"audit"}\n');
    await writeFile(join(root, 'src/app/page.tsx'), 'export const value = 1;\n');
    await writeFile(join(root, 'src-tauri/src/lib.rs'), 'pub fn value() -> u8 { 1 }\n');
    await writeFile(
      join(root, 'docs/engineering/r2-audit/acceptance-status.json'),
      '{"generated":true}\n'
    );
    execFileSync('git', ['init'], { cwd: root, stdio: 'ignore' });
    execFileSync(
      'git',
      [
        'add',
        'package.json',
        'src/app/page.tsx',
        'src-tauri/src/lib.rs',
        'docs/engineering/r2-audit/acceptance-status.json',
      ],
      { cwd: root, stdio: 'ignore' }
    );
    const baseline = productionSourceFingerprint(root).production_source_sha256;
    await writeFile(
      join(root, 'docs/engineering/r2-audit/acceptance-status.json'),
      '{"generated":false}\n'
    );
    assert.equal(productionSourceFingerprint(root).production_source_sha256, baseline);
    await writeFile(join(root, 'src/app/page.tsx'), 'export const value = 2;\n');
    const dirty = productionSourceFingerprint(root).production_source_sha256;
    assert.notEqual(dirty, baseline);
    await writeFile(join(root, 'src-tauri/src/new_provider.rs'), 'pub fn new_provider() {}\n');
    assert.notEqual(productionSourceFingerprint(root).production_source_sha256, dirty);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('passing app gate requires current source fingerprint and binary hash', async () => {
  const root = await mkdtemp(join(tmpdir(), 'r2-audit-provenance-'));
  try {
    const auditDir = join(root, 'docs/engineering/r2-audit');
    await mkdir(auditDir, { recursive: true });
    const sourceFingerprint = { production_source_sha256: 'current-source' };
    const gate = {
      id: 'app-runtime',
      evidence: 'evidence.json',
      status_from: '/completed',
      map_status: { true: 'passed' },
      require_current_source: true,
      source_fingerprint_from: '/source/production_source_sha256',
      app_binary_sha256_from: '/app/binary_sha256',
    };
    await writeFile(join(auditDir, 'evidence.json'), JSON.stringify({ completed: true }));
    let classified = classifyGate(gate, root, null, sourceFingerprint);
    assert.equal(classified.status, 'historical_pass');
    assert.match(classified.reason, /missing production source fingerprint/);
    await writeFile(
      join(auditDir, 'evidence.json'),
      JSON.stringify({
        completed: true,
        source: { production_source_sha256: 'old-source' },
        app: { binary_sha256: 'bin' },
      })
    );
    classified = classifyGate(gate, root, null, sourceFingerprint);
    assert.equal(classified.status, 'historical_pass');
    assert.match(classified.reason, /does not match current/);
    await writeFile(
      join(auditDir, 'evidence.json'),
      JSON.stringify({ completed: true, source: { production_source_sha256: 'current-source' } })
    );
    classified = classifyGate(gate, root, null, sourceFingerprint);
    assert.equal(classified.status, 'historical_pass');
    assert.match(classified.reason, /missing app binary hash/);
    await writeFile(
      join(auditDir, 'evidence.json'),
      JSON.stringify({
        completed: true,
        source: { production_source_sha256: 'current-source' },
        app: { binary_sha256: 'bin' },
      })
    );
    classified = classifyGate(gate, root, null, sourceFingerprint);
    assert.equal(classified.status, 'passed');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('r2-audit-manifest.mjs still runs its CLI entrypoint when invoked through a symlink', async () => {
  const tempDir = await mkdtemp(join(tmpdir(), 'r2-audit-manifest-symlink-'));
  try {
    const symlinkPath = join(tempDir, 'entrypoint.mjs');
    await symlink(join(scriptsDir, 'r2-audit-manifest.mjs'), symlinkPath);
    // Before the realpathSync fix, process.argv[1] (the symlink path) never
    // strictly equals the resolved import.meta.url path, so the "is main"
    // guard silently no-ops: this prints nothing instead of the status JSON.
    const output = execFileSync(process.execPath, [symlinkPath, '--json'], {
      encoding: 'utf8',
      maxBuffer: 16 * 1024 * 1024,
    });
    const parsed = JSON.parse(output);
    assert.equal(parsed.schema_version, 1);
    assert.ok(Array.isArray(parsed.gates) && parsed.gates.length > 0);
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
});

test('audit-source.mjs still runs its CLI entrypoint when invoked through a symlink', async () => {
  const fixtureRoot = await mkdtemp(join(tmpdir(), 'r2-audit-source-symlink-root-'));
  const tempDir = await mkdtemp(join(tmpdir(), 'r2-audit-source-symlink-'));
  try {
    await mkdir(join(fixtureRoot, 'src/app'), { recursive: true });
    await writeFile(join(fixtureRoot, 'package.json'), '{"name":"audit-fixture"}\n');
    await writeFile(join(fixtureRoot, 'src/app/page.tsx'), 'export const value = 1;\n');
    const symlinkPath = join(tempDir, 'entrypoint.mjs');
    await symlink(join(scriptsDir, 'audit-source.mjs'), symlinkPath);
    const output = execFileSync(process.execPath, [symlinkPath, '--json', '--root', fixtureRoot], {
      encoding: 'utf8',
    });
    const parsed = JSON.parse(output);
    assert.match(parsed.production_source_sha256, /^[a-f0-9]{64}$/);
    assert.ok(parsed.files.includes('src/app/page.tsx'));
  } finally {
    await rm(fixtureRoot, { recursive: true, force: true });
    await rm(tempDir, { recursive: true, force: true });
  }
});
