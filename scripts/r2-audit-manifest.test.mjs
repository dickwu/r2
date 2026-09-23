import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join } from 'node:path';
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

const sha256Like = (fill) => fill.repeat(64);

// Fixture repositories carry their own identity, never run user hooks (the
// hooks path is a directory that does not exist, unambiguous on every OS) and
// never let a runner's core.autocrlf decide whether LF content reads dirty.
function git(cwd, args) {
  return execFileSync(
    'git',
    [
      '-c',
      'user.name=r2-audit',
      '-c',
      'user.email=r2-audit@example.invalid',
      '-c',
      'commit.gpgsign=false',
      '-c',
      'core.autocrlf=false',
      '-c',
      `core.hooksPath=${join(cwd, 'no-hooks')}`,
      ...args,
    ],
    { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'], timeout: 30_000 }
  ).trim();
}

// A committed, clean repository with a production input, an audit input
// (the manifest) and nothing else, so provenance values are deterministic.
async function committedFixtureRepo() {
  const root = await mkdtemp(join(tmpdir(), 'r2-audit-git-'));
  const files = {
    'package.json': '{"name":"audit-fixture"}\n',
    'src/app/page.tsx': 'export const value = 1;\n',
    'docs/engineering/r2-audit/acceptance-manifest.json': '{"gates":[]}\n',
  };
  for (const [path, content] of Object.entries(files)) {
    await mkdir(dirname(join(root, path)), { recursive: true });
    await writeFile(join(root, path), content);
  }
  git(root, ['init', '-q']);
  git(root, ['add', '-A']);
  git(root, ['commit', '-q', '-m', 'fixture']);
  return root;
}

async function auditRoot(prefix) {
  const root = await mkdtemp(join(tmpdir(), prefix));
  await mkdir(join(root, 'docs/engineering/r2-audit'), { recursive: true });
  return root;
}

function writeEvidence(root, file, document) {
  return writeFile(join(root, 'docs/engineering/r2-audit', file), JSON.stringify(document));
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
    await writeFile(
      join(auditDir, 'evidence.json'),
      JSON.stringify({
        completed: true,
        git: { commit: 'older', tree: 'older-tree', dirty: false },
      })
    );
    const gate = classifyGate(
      {
        id: 'rustfs-native',
        evidence: 'evidence.json',
        status_from: '/completed',
        map_status: { true: 'passed' },
        require_current_git: true,
      },
      root,
      { commit: 'current', tree: 'tree', dirty: false, dirty_paths: [] }
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
          binary_sha256: sha256Like('b'),
        },
      })
    );
    const classified = classifyGate(gate, root, null, sourceFingerprint);
    assert.equal(classified.status, 'limited');
    // The note must say why the run is only limited, not stay empty.
    assert.ok(classified.reason, 'expected an explicit note for a current limited result');
    assert.match(classified.reason, /\/mode/);
    assert.match(classified.reason, /smoke/);
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
    const git = { commit: 'current-commit', tree: 'current-tree', dirty: false, dirty_paths: [] };
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
        git: { commit: git.commit, tree: git.tree, dirty: false },
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
    execFileSync('git', ['init'], { cwd: root, stdio: 'ignore', timeout: 30_000 });
    execFileSync(
      'git',
      [
        'add',
        'package.json',
        'src/app/page.tsx',
        'src-tauri/src/lib.rs',
        'docs/engineering/r2-audit/acceptance-status.json',
      ],
      { cwd: root, stdio: 'ignore', timeout: 30_000 }
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
        app: { binary_sha256: sha256Like('b') },
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
        app: { binary_sha256: sha256Like('b') },
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
      timeout: 60_000,
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
      timeout: 60_000,
    });
    const parsed = JSON.parse(output);
    assert.match(parsed.production_source_sha256, /^[a-f0-9]{64}$/);
    assert.ok(parsed.files.includes('src/app/page.tsx'));
  } finally {
    await rm(fixtureRoot, { recursive: true, force: true });
    await rm(tempDir, { recursive: true, force: true });
  }
});

test('require_current_git never passes vacuously on absent or null git metadata', async () => {
  const root = await auditRoot('r2-audit-null-git-');
  try {
    const gate = {
      id: 'rustfs-protocol',
      evidence: 'evidence.json',
      required_checks: [{ pointer: '/completed', expect: true }],
      require_current_git: true,
    };
    // Before the fix `undefined === undefined` bound evidence without a git
    // field to a generator without git metadata, and null equalled null.
    await writeEvidence(root, 'evidence.json', { completed: true });
    const withoutGit = classifyGate(gate, root, null, null);
    assert.equal(withoutGit.status, 'historical_pass');
    assert.match(withoutGit.reason, /current git metadata unavailable/);
    await writeEvidence(root, 'evidence.json', {
      completed: true,
      git: { commit: null, tree: null, dirty: null },
    });
    const nullGit = classifyGate(
      gate,
      root,
      { commit: null, tree: null, dirty: null, dirty_paths: null },
      null
    );
    assert.equal(nullGit.status, 'historical_pass');
    assert.match(nullGit.reason, /current git metadata unavailable/);
    const nullEvidence = classifyGate(
      gate,
      root,
      { commit: 'current', tree: 'tree', dirty: false, dirty_paths: [] },
      null
    );
    assert.equal(nullEvidence.status, 'historical_pass');
    assert.match(nullEvidence.reason, /evidence records no git commit\/tree/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('fresh evidence that records no git field is reported as unbound, never as a commit mismatch', async () => {
  const root = await auditRoot('r2-audit-no-git-field-');
  try {
    await writeEvidence(root, 'evidence.json', { completed: true });
    const classified = classifyGate(
      {
        id: 'rustfs-protocol',
        evidence: 'evidence.json',
        required_checks: [{ pointer: '/completed', expect: true }],
        require_current_git: true,
      },
      root,
      { commit: 'current', tree: 'tree', dirty: false, dirty_paths: [] }
    );
    assert.equal(classified.status, 'historical_pass');
    // A run on the release SHA that simply has no git field is not "not bound
    // to the current commit"; it records nothing to bind.
    assert.match(classified.reason, /evidence records no git commit\/tree/);
    assert.doesNotMatch(classified.reason, /not bound to current git/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('evidence bound to the current commit cannot pass while either worktree was dirty', async () => {
  const root = await auditRoot('r2-audit-dirty-');
  try {
    const gate = {
      id: 'rustfs-protocol',
      evidence: 'evidence.json',
      required_checks: [{ pointer: '/completed', expect: true }],
      require_current_git: true,
    };
    const commit = sha256Like('c').slice(0, 40);
    const tree = sha256Like('7').slice(0, 40);
    const clean = { commit, tree, dirty: false, dirty_paths: [] };
    await writeEvidence(root, 'evidence.json', {
      completed: true,
      git: { commit, tree, dirty: false },
    });
    assert.equal(classifyGate(gate, root, clean, null).status, 'passed');
    const dirtyGenerator = classifyGate(
      gate,
      root,
      { ...clean, dirty: true, dirty_paths: ['src/app/page.tsx'] },
      null
    );
    assert.equal(dirtyGenerator.status, 'historical_pass');
    assert.match(dirtyGenerator.reason, /current worktree is dirty/);
    await writeEvidence(root, 'evidence.json', {
      completed: true,
      git: { commit, tree, dirty: true },
    });
    const dirtyEvidence = classifyGate(gate, root, clean, null);
    assert.equal(dirtyEvidence.status, 'historical_pass');
    assert.match(dirtyEvidence.reason, /captured on a dirty worktree/);
    await writeEvidence(root, 'evidence.json', { completed: true, git: { commit, tree } });
    const unknownEvidence = classifyGate(gate, root, clean, null);
    assert.equal(unknownEvidence.status, 'historical_pass');
    assert.match(unknownEvidence.reason, /does not record whether its worktree was clean/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('gitProvenance records the commit and tree and treats only audit outputs as safe to leave uncommitted', async () => {
  const { gitProvenance } = await import('./audit-source.mjs');
  const repo = await committedFixtureRepo();
  const plain = await mkdtemp(join(tmpdir(), 'r2-audit-not-a-repo-'));
  try {
    const clean = gitProvenance(repo);
    assert.equal(clean.commit, git(repo, ['rev-parse', 'HEAD']));
    assert.equal(clean.tree, git(repo, ['rev-parse', 'HEAD^{tree}']));
    assert.match(clean.commit, /^[a-f0-9]{40}$/);
    assert.equal(clean.dirty, false);
    assert.deepEqual(clean.dirty_paths, []);
    // Evidence and generated status are written before they are committed,
    // so they never make the tree dirty; the manifest is an acceptance input.
    await writeFile(join(repo, 'docs/engineering/r2-audit/real-rustfs-protocol.json'), '{}\n');
    await writeFile(join(repo, 'docs/engineering/r2-audit/acceptance-status.json'), '{}\n');
    assert.deepEqual(gitProvenance(repo), clean);
    await writeFile(
      join(repo, 'docs/engineering/r2-audit/acceptance-manifest.json'),
      '{"gates":[{}]}\n'
    );
    const manifestEdit = gitProvenance(repo);
    assert.equal(manifestEdit.dirty, true);
    assert.deepEqual(manifestEdit.dirty_paths, [
      'docs/engineering/r2-audit/acceptance-manifest.json',
    ]);
    git(repo, ['checkout', '--', 'docs/engineering/r2-audit/acceptance-manifest.json']);
    await mkdir(join(repo, 'scripts'));
    await writeFile(join(repo, 'scripts/new-harness.mjs'), 'export const value = 1;\n');
    const harnessEdit = gitProvenance(repo);
    assert.equal(harnessEdit.dirty, true);
    assert.deepEqual(harnessEdit.dirty_paths, ['scripts/new-harness.mjs']);
    await writeFile(join(repo, 'src/app/page.tsx'), 'export const value = 2;\n');
    const sourceEdit = gitProvenance(repo);
    assert.equal(sourceEdit.dirty, true);
    assert.deepEqual(sourceEdit.dirty_paths, ['scripts/new-harness.mjs', 'src/app/page.tsx']);
    assert.equal(sourceEdit.commit, clean.commit);
    assert.equal(sourceEdit.tree, clean.tree);
    // Outside a repository nothing is fabricated.
    await writeFile(join(plain, '.git'), 'gitdir: nowhere\n');
    assert.deepEqual(gitProvenance(plain), {
      commit: null,
      tree: null,
      dirty: null,
      dirty_paths: null,
    });
  } finally {
    await rm(repo, { recursive: true, force: true });
    await rm(plain, { recursive: true, force: true });
  }
});

test('required checks and limited_when compare typed values, never their string forms', async () => {
  const root = await auditRoot('r2-audit-typed-');
  try {
    const gate = {
      id: 'typed',
      evidence: 'evidence.json',
      required_checks: [
        { pointer: '/ok', expect: true },
        { pointer: '/count', expect: 1 },
      ],
      limited_when: [{ pointer: '/partial', equals: true }],
    };
    await writeEvidence(root, 'evidence.json', { ok: 'true', count: 1, partial: false });
    const stringBoolean = classifyGate(gate, root);
    assert.equal(stringBoolean.status, 'failed');
    assert.match(stringBoolean.reason, /\/ok/);
    await writeEvidence(root, 'evidence.json', { ok: true, count: '1', partial: false });
    const stringNumber = classifyGate(gate, root);
    assert.equal(stringNumber.status, 'failed');
    assert.match(stringNumber.reason, /\/count/);
    await writeEvidence(root, 'evidence.json', { ok: true, count: 1, partial: 'true' });
    assert.equal(classifyGate(gate, root).status, 'passed');
    await writeEvidence(root, 'evidence.json', { ok: true, count: 1, partial: true });
    assert.equal(classifyGate(gate, root).status, 'limited');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

// Which importable module declares the report shape each provider gate reads.
// native-rustfs-audit.mjs executes on import (host asserts, pinned-archive
// verification, servers), so its contract is the side-effect-free builder
// module that produces its evidence skeleton.
const GATE_REPORT_CONTRACTS = {
  'rustfs-protocol': { module: './native-rustfs-report.mjs', pointers: 'REPORT_POINTERS' },
  'rustfs-native': { module: './native-rustfs-report.mjs', pointers: 'NATIVE_REPORT_POINTERS' },
  'cloudflare-r2-protocol': { module: './r2-protocol-audit.mjs', pointers: 'REPORT_POINTERS' },
  'aws-s3-provider': { module: './aws-s3-protocol-audit.mjs', pointers: 'REPORT_POINTERS' },
};

// The pointers classifyGate() resolves for a gate's provenance requirements.
function provenancePointersRead(gate) {
  const pointers = [];
  if (gate.require_current_git) {
    pointers.push(
      gate.git_commit_from ?? '/git/commit',
      gate.git_tree_from ?? '/git/tree',
      gate.git_dirty_from ?? '/git/dirty'
    );
  }
  if (gate.require_current_source) {
    assert.ok(
      gate.source_fingerprint_from,
      `${gate.id} must name the fingerprint pointer it reads`
    );
    pointers.push(gate.source_fingerprint_from);
  }
  if (gate.app_binary_sha256_from) pointers.push(gate.app_binary_sha256_from);
  return pointers;
}

test('every provider gate reads provenance only from pointers its harness report guarantees', async () => {
  for (const [id, contract] of Object.entries(GATE_REPORT_CONTRACTS)) {
    const gate = committedGate(id);
    assert.ok(
      gate.harnesses.includes(`scripts/${basename(contract.module)}`),
      `${id} must list ${contract.module} among its harnesses`
    );
    const guaranteed = (await import(contract.module))[contract.pointers];
    assert.ok(
      Array.isArray(guaranteed) && guaranteed.length,
      `${contract.module} exports no ${contract.pointers}`
    );
    const read = provenancePointersRead(gate);
    assert.ok(read.length, `${id} declares no provenance requirement`);
    for (const expression of read) {
      assert.ok(
        guaranteed.includes(expression),
        `${id} reads ${expression}, which ${contract.module} does not guarantee`
      );
    }
  }
});

function assertReportCarries(report, pointers, jsonPointer) {
  for (const expression of pointers) {
    assert.notEqual(jsonPointer(report, expression), undefined, `report omits ${expression}`);
  }
}

test('RustFS evidence built by the harness builder passes its committed gates only when bound to the current commit', async () => {
  const { createEvidence, nativeEvidence } = await import('./native-rustfs-report.mjs');
  const { jsonPointer } = await import('./audit-source.mjs');
  const { REPORT_POINTERS, NATIVE_REPORT_POINTERS } = await import('./native-rustfs-report.mjs');
  const repo = await committedFixtureRepo();
  const root = await auditRoot('r2-audit-rustfs-contract-');
  try {
    const build = (native) =>
      createEvidence({
        native,
        repo,
        rustfs: {
          version: '1.0.0-rc.6',
          commit: sha256Like('5').slice(0, 40),
          archiveSha256: sha256Like('a'),
        },
        binarySha256: sha256Like('b'),
        harnessSha256: sha256Like('e'),
        versionOutput: 'rustfs 1.0.0-rc.6',
      });
    const protocol = build(false);
    assertReportCarries(protocol, REPORT_POINTERS, jsonPointer);
    Object.assign(protocol, {
      completed: true,
      range: { exact_bytes: true },
      multipart: { exact_bytes: true },
      versioning: { supported: true },
    });
    await writeEvidence(root, 'real-rustfs-protocol.json', protocol);
    const protocolGate = committedGate('rustfs-protocol');
    assert.equal(
      classifyGate(protocolGate, root, protocol.git, protocol.source_fingerprint).status,
      'passed'
    );
    const stale = classifyGate(
      protocolGate,
      root,
      { ...protocol.git, commit: sha256Like('0').slice(0, 40), tree: sha256Like('1').slice(0, 40) },
      protocol.source_fingerprint
    );
    assert.equal(stale.status, 'historical_pass');
    assert.match(stale.reason, /not bound to current git/);
    const native = build(true);
    native.native = {
      ...nativeEvidence({
        appId: 'com.lifefarmer.r2.audit-test',
        appBinarySha256: sha256Like('d'),
      }),
      moves: [{ status: 'success' }, { status: 'success' }],
      nfs: { write_read_unmount: 'passed', rename: 'completed', delete: 'completed' },
    };
    native.completed = true;
    assertReportCarries(native, NATIVE_REPORT_POINTERS, jsonPointer);
    await writeEvidence(root, 'real-rustfs-native.json', native);
    const nativeGate = committedGate('rustfs-native');
    assert.equal(
      classifyGate(nativeGate, root, native.git, native.source_fingerprint).status,
      'passed'
    );
    const otherSource = classifyGate(nativeGate, root, native.git, {
      production_source_sha256: sha256Like('f'),
    });
    assert.equal(otherSource.status, 'historical_pass');
    assert.match(otherSource.reason, /does not match current/);
  } finally {
    await rm(repo, { recursive: true, force: true });
    await rm(root, { recursive: true, force: true });
  }
});

test('an R2 report built by the harness passes the committed cloudflare-r2-protocol gate when every probe is enforced', async () => {
  const { createReport, REPORT_POINTERS } = await import('./r2-protocol-audit.mjs');
  const { jsonPointer } = await import('./audit-source.mjs');
  const repo = await committedFixtureRepo();
  const root = await auditRoot('r2-audit-r2-contract-');
  try {
    const report = createReport(
      {
        endpoint: `https://${'a'.repeat(32)}.r2.cloudflarestorage.com`,
        bucket: 'dedicated-test',
        prefix: 'r2-audit/11111111-2222-4333-8444-555555555555/',
      },
      { owner: 'offline-owner', harnessSha256: sha256Like('e'), repo }
    );
    assertReportCarries(report, REPORT_POINTERS, jsonPointer);
    Object.assign(report, {
      status: 'observations_collected',
      range: { exact_bytes: true },
      multipart: { exact_bytes: true },
      cleanup_complete: true,
      conditions: {
        get_match: { behavior: 'enforced' },
        put_absent: { behavior: 'enforced' },
        copy_source_match: { behavior: 'enforced' },
      },
    });
    await writeEvidence(root, 'real-r2-protocol.json', report);
    const gate = committedGate('cloudflare-r2-protocol');
    assert.equal(classifyGate(gate, root, report.git, null).status, 'passed');
  } finally {
    await rm(repo, { recursive: true, force: true });
    await rm(root, { recursive: true, force: true });
  }
});

const awsConfig = {
  region: 'us-east-1',
  sourceBucket: 'fault-audit',
  destBucket: 'fault-audit',
  accessKeyId: 'offline-placeholder',
  secretAccessKey: 'offline-placeholder',
  prefix: 'r2-audit/aws/11111111-2222-4333-8444-555555555555/',
};

// What execute() records once every AWS probe and both response-loss faults succeed.
function awsProbeResults() {
  return {
    status: 'observations_collected',
    range: { exact_bytes: true },
    multipart: { exact_bytes: true },
    cleanup_complete: true,
    bucket_matrix_mode: 'same_bucket',
    conditions: {
      copy_source_wrong_etag_precondition: 'protected',
      complete_response_loss: 'reconciled_after_client_side_response_drop',
      complete_response_loss_fault: { fired: true, request_count: 1 },
      delete_response_loss: 'reconciled_after_client_side_response_drop',
      delete_response_loss_fault: { fired: true, request_count: 1 },
    },
  };
}

test('an AWS local-fixture report can never satisfy the committed aws-s3-provider gate', async () => {
  const { createReport } = await import('./aws-s3-protocol-audit.mjs');
  const repo = await committedFixtureRepo();
  const root = await auditRoot('r2-audit-aws-local-');
  try {
    // --local-response-loss-test: execute() against a disposable loopback RustFS.
    const report = {
      ...createReport(
        { ...awsConfig, endpoint: 'http://127.0.0.1:9000', localFixture: true },
        { owner: 'local-owner', harnessSha256: sha256Like('e'), repo }
      ),
      ...awsProbeResults(),
    };
    assert.equal(report.local_fixture, true);
    assert.equal(report.provider_kind, 'local-rustfs');
    assert.equal(report.endpoint, 'http://127.0.0.1:9000');
    await writeEvidence(root, 'real-aws-s3.json', report);
    const classified = classifyGate(
      committedGate('aws-s3-provider'),
      root,
      report.git,
      report.source_fingerprint
    );
    assert.equal(classified.status, 'failed');
    assert.match(classified.reason, /\/local_fixture|\/provider_kind/);
  } finally {
    await rm(repo, { recursive: true, force: true });
    await rm(root, { recursive: true, force: true });
  }
});

test('an AWS report without an explicit local-fixture marker never passes the committed aws-s3-provider gate', async () => {
  const root = await auditRoot('r2-audit-aws-unmarked-');
  try {
    // Exactly the fields the harness wrote before it recorded where it ran: a
    // loopback RustFS run looked like this, so a run without the marker must
    // never read as AWS acceptance evidence.
    const git = {
      commit: sha256Like('c').slice(0, 40),
      tree: sha256Like('7').slice(0, 40),
      dirty: false,
    };
    await writeEvidence(root, 'real-aws-s3.json', {
      captured_at: '2026-09-23T00:00:00.000Z',
      region: 'us-east-1',
      source_bucket: 'fault-audit',
      dest_bucket: 'fault-audit',
      prefix: awsConfig.prefix,
      owner: 'unmarked-owner',
      harness_sha256: sha256Like('e'),
      plan: {},
      git,
      ...awsProbeResults(),
    });
    const classified = classifyGate(
      committedGate('aws-s3-provider'),
      root,
      { ...git, dirty_paths: [] },
      null
    );
    assert.equal(classified.status, 'failed');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('a real AWS report built by the harness passes the committed aws-s3-provider gate when bound to the current commit', async () => {
  const { createReport, REPORT_POINTERS } = await import('./aws-s3-protocol-audit.mjs');
  const { jsonPointer } = await import('./audit-source.mjs');
  const repo = await committedFixtureRepo();
  const root = await auditRoot('r2-audit-aws-real-');
  try {
    const report = {
      ...createReport(awsConfig, { owner: 'aws-owner', harnessSha256: sha256Like('e'), repo }),
      ...awsProbeResults(),
    };
    assertReportCarries(report, REPORT_POINTERS, jsonPointer);
    assert.equal(report.local_fixture, false);
    assert.equal(report.provider_kind, 'aws');
    assert.equal(report.endpoint, null);
    await writeEvidence(root, 'real-aws-s3.json', report);
    const gate = committedGate('aws-s3-provider');
    assert.equal(classifyGate(gate, root, report.git, report.source_fingerprint).status, 'passed');
  } finally {
    await rm(repo, { recursive: true, force: true });
    await rm(root, { recursive: true, force: true });
  }
});

test('a passing app gate requires a well-formed app binary SHA-256, not just any value', async () => {
  const root = await auditRoot('r2-audit-binary-hash-');
  try {
    const gate = {
      id: 'app-runtime',
      evidence: 'evidence.json',
      status_from: '/completed',
      map_status: { true: 'passed' },
      app_binary_sha256_from: '/app/binary_sha256',
    };
    for (const placeholder of ['bin', true, 'unknown', sha256Like('b').slice(0, 63)]) {
      await writeEvidence(root, 'evidence.json', {
        completed: true,
        app: { binary_sha256: placeholder },
      });
      const classified = classifyGate(gate, root);
      assert.equal(classified.status, 'historical_pass', JSON.stringify(placeholder));
      assert.match(classified.reason, /app binary hash/);
      assert.match(classified.reason, /not a SHA-256/);
    }
    await writeEvidence(root, 'evidence.json', {
      completed: true,
      app: { binary_sha256: sha256Like('b') },
    });
    assert.equal(classifyGate(gate, root).status, 'passed');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('every committed gate classifies an empty evidence document as failed instead of throwing or passing', async () => {
  const root = await auditRoot('r2-audit-empty-evidence-');
  try {
    const git = { commit: 'current', tree: 'tree', dirty: false, dirty_paths: [] };
    for (const gate of committedManifest.gates) {
      await writeEvidence(root, gate.evidence, {});
      const classified = classifyGate(gate, root, git, { production_source_sha256: 'source' });
      assert.equal(classified.status, 'failed', gate.id);
      assert.ok(classified.reason, `${gate.id} reports no reason`);
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('a status_from value with no mapping classifies as failed with a reason instead of throwing', async () => {
  const root = await auditRoot('r2-audit-unmapped-status-');
  try {
    await writeEvidence(root, 'evidence.json', { completed: false });
    const classified = classifyGate(
      {
        id: 'mapped',
        evidence: 'evidence.json',
        status_from: '/completed',
        map_status: { true: 'passed' },
      },
      root
    );
    assert.equal(classified.status, 'failed');
    assert.match(classified.reason, /\/completed/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
