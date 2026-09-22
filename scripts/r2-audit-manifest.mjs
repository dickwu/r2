#!/usr/bin/env node
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { arch, platform } from 'node:os';
import { isMainModule, productionSourceFingerprint } from './audit-source.mjs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptPath = fileURLToPath(import.meta.url);
const repoRoot = resolve(dirname(scriptPath), '..');
const auditDir = 'docs/engineering/r2-audit';
const manifestPath = join(auditDir, 'acceptance-manifest.json');
const jsonOutput = join(auditDir, 'acceptance-status.json');
const markdownOutput = join(auditDir, 'acceptance-status.md');

const STATUSES = new Set([
  'passed',
  'historical_pass',
  'limited',
  'historical_limited',
  'prepared_not_executed',
  'not_executed',
  'not_implemented',
  'prerequisite_only',
  'ci_only_not_reproduced_locally',
  'blocked',
  'failed',
]);

export function normalizeStatus(value) {
  assert(typeof value === 'string' && STATUSES.has(value), `Unknown acceptance status: ${value}`);
  return value;
}

function hashBytes(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

async function fileSha256(path) {
  return hashBytes(await readFile(path));
}

function fileSha256Sync(path) {
  return hashBytes(readFileSync(path));
}

function readJsonSync(path) {
  return JSON.parse(readFileSync(path, 'utf8'));
}

function pointer(document, expression) {
  if (!expression) return undefined;
  if (expression === '') return document;
  assert(expression.startsWith('/'), `JSON pointer must start with /: ${expression}`);
  return expression
    .slice(1)
    .split('/')
    .reduce((current, part) => {
      if (current === undefined || current === null) return undefined;
      const key = part.replaceAll('~1', '/').replaceAll('~0', '~');
      return current[key];
    }, document);
}

function stringifyPointerValue(value) {
  if (typeof value === 'boolean') return String(value);
  if (value === null || value === undefined) return '';
  return String(value);
}

function gateEvidencePath(gate, root) {
  if (!gate.evidence) return null;
  return resolve(root, auditDir, gate.evidence);
}

function missingHarnesses(gate, root) {
  return (gate.harnesses ?? []).filter((harness) => !existsSync(resolve(root, harness)));
}

function checkMatches(actual, expected) {
  const allowed = Array.isArray(expected) ? expected : [expected];
  return allowed.map(String).includes(stringifyPointerValue(actual));
}

function evaluateRequiredChecks(gate, evidence) {
  const checks = [];
  for (const check of gate.required_checks ?? []) {
    const actual = pointer(evidence, check.pointer);
    const passed = checkMatches(actual, check.expect);
    checks.push({ pointer: check.pointer, expected: check.expect, actual, passed });
  }
  return checks;
}

function evaluateLimited(gate, evidence) {
  return (gate.limited_when ?? []).some((check) =>
    checkMatches(pointer(evidence, check.pointer), check.equals)
  );
}

function firstPointer(evidence, expressions) {
  for (const expression of expressions.filter(Boolean)) {
    const value = pointer(evidence, expression);
    if (value !== undefined && value !== null && value !== '') return value;
  }
  return undefined;
}

function provenanceReason(gate, evidence, git, sourceFingerprint) {
  const sourcePointers = [
    gate.source_fingerprint_from,
    gate.built_source_sha256_from,
    '/source_fingerprint/production_source_sha256',
    '/source_fingerprint_before/production_source_sha256',
    '/built_source_snapshot/data/production_source_sha256',
    '/build/production_source_sha256',
  ];
  if (
    gate.require_current_source ||
    gate.source_fingerprint_from ||
    gate.built_source_sha256_from
  ) {
    const evidenceSource = firstPointer(evidence, sourcePointers);
    if (!evidenceSource) return 'evidence is missing production source fingerprint';
    if (evidenceSource !== sourceFingerprint?.production_source_sha256) {
      return `evidence production source fingerprint ${evidenceSource} does not match current ${sourceFingerprint?.production_source_sha256 ?? 'unknown'}`;
    }
  }
  if (gate.app_binary_sha256_from && !pointer(evidence, gate.app_binary_sha256_from)) {
    return `evidence is missing app binary hash at ${gate.app_binary_sha256_from}`;
  }
  if (!gate.require_current_git) return null;
  const evidenceCommit = pointer(evidence, gate.git_commit_from ?? '/git/commit');
  const evidenceTree = pointer(evidence, gate.git_tree_from ?? '/git/tree');
  if (evidenceCommit === git?.commit && evidenceTree === git?.tree) return null;
  return `evidence is not bound to current git commit/tree (${git?.commit ?? 'unknown'} / ${git?.tree ?? 'unknown'})`;
}

export function classifyGate(gate, root, git = null, sourceFingerprint = null) {
  const missingStatus = normalizeStatus(gate.status_when_missing ?? 'blocked');
  const evidencePath = root ? gateEvidencePath(gate, root) : null;
  const base = {
    id: gate.id,
    title: gate.title,
    category: gate.category,
    mode: gate.mode,
  };
  if (!evidencePath || !existsSync(evidencePath)) {
    const missing = root ? missingHarnesses(gate, root) : [];
    const preparedWithoutHarness =
      ['prepared_not_executed', 'not_executed'].includes(missingStatus) &&
      (!(gate.harnesses ?? []).length || missing.length > 0);
    return {
      ...base,
      status: preparedWithoutHarness ? 'not_implemented' : missingStatus,
      evidence: null,
      reason: preparedWithoutHarness
        ? `missing harness: ${missing.join(', ') || 'none defined'}`
        : gate.evidence
          ? `missing evidence file: ${gate.evidence}`
          : 'no evidence file defined',
    };
  }
  const evidence = readJsonSync(evidencePath);
  const checks = evaluateRequiredChecks(gate, evidence);
  const failed = checks.find((check) => !check.passed);
  let status;
  let reason;
  if (failed) {
    status = 'failed';
    reason = `${failed.pointer} expected ${JSON.stringify(failed.expected)} but found ${JSON.stringify(failed.actual)}`;
  } else if (checks.length) {
    status = evaluateLimited(gate, evidence) ? 'limited' : 'passed';
  } else {
    const raw = stringifyPointerValue(pointer(evidence, gate.status_from));
    const mapped = gate.map_status?.[raw] ?? raw;
    status = normalizeStatus(mapped);
  }
  // A 'limited' result is still real execution evidence, so it is held to
  // the same current-source/binary provenance requirements as 'passed' — a
  // stale limited artifact must not silently keep reporting 'limited' with
  // no explanation of why it is stale.
  const staleReason =
    status === 'passed' || status === 'limited'
      ? provenanceReason(gate, evidence, git, sourceFingerprint)
      : null;
  if (staleReason) {
    status = status === 'limited' ? 'historical_limited' : 'historical_pass';
    reason = staleReason;
  }
  return {
    ...base,
    status,
    checks: checks.length ? checks : undefined,
    evidence: {
      file: gate.evidence,
      sha256: fileSha256Sync(evidencePath),
    },
    reason,
    provider_version: pointer(evidence, gate.provider_version_from),
    provider_commit: pointer(evidence, gate.provider_commit_from),
    provider_binary_sha256: pointer(evidence, gate.provider_binary_sha256_from),
    app_binary_sha256: pointer(evidence, gate.app_binary_sha256_from),
    app_id: pointer(evidence, gate.app_id_from),
    captured_at: evidence.captured_at,
  };
}

function summarize(gates) {
  const summary = Object.fromEntries([...STATUSES].map((status) => [status, 0]));
  for (const gate of gates) summary[gate.status]++;
  return summary;
}

function gitMetadata(root) {
  const runGit = (args) =>
    execFileSync('git', args, {
      cwd: root,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
  try {
    const porcelain = runGit(['status', '--porcelain']);
    return {
      commit: runGit(['rev-parse', 'HEAD']),
      tree: runGit(['rev-parse', 'HEAD^{tree}']),
      dirty: porcelain.length > 0,
    };
  } catch {
    return { commit: null, tree: null, dirty: null };
  }
}

async function collectHarnesses(root, manifest) {
  const harnesses = new Set();
  for (const gate of manifest.gates ?? []) {
    for (const harness of gate.harnesses ?? []) harnesses.add(harness);
  }
  const result = {};
  for (const harness of [...harnesses].sort()) {
    const absolute = resolve(root, harness);
    result[harness] = existsSync(absolute)
      ? { sha256: await fileSha256(absolute) }
      : { missing: true };
  }
  return result;
}

export async function createAcceptanceStatus({
  repoRoot: root = repoRoot,
  manifest,
  git = gitMetadata(root),
  sourceFingerprint = productionSourceFingerprint(root),
  platform: host = { os: platform(), arch: arch() },
  capturedAt = new Date().toISOString(),
} = {}) {
  const loadedManifest =
    manifest ?? JSON.parse(await readFile(resolve(root, manifestPath), 'utf8'));
  const gates = (loadedManifest.gates ?? []).map((gate) =>
    classifyGate(gate, root, git, sourceFingerprint)
  );
  const harnesses = await collectHarnesses(root, loadedManifest);
  return {
    schema_version: loadedManifest.schema_version,
    captured_at: capturedAt,
    git,
    source_fingerprint: sourceFingerprint,
    host,
    manifest: {
      file: manifest ? null : manifestPath,
      sha256: manifest ? null : await fileSha256(resolve(root, manifestPath)),
    },
    harnesses,
    summary: summarize(gates),
    gates,
  };
}

function renderMarkdown(status) {
  const table = [
    ['id', 'status', 'mode', 'provider/build', 'evidence', 'note'],
    ...status.gates.map((gate) => [
      gate.id,
      gate.status,
      gate.mode ?? '',
      gate.provider_version ?? gate.provider_commit ?? '',
      gate.evidence?.file ?? '',
      gate.reason ?? '',
    ]),
  ].map((row) => row.map((value) => String(value).replaceAll('|', '\\|')));
  const widths = table[0].map((_, index) => Math.max(...table.map((row) => row[index].length), 3));
  const renderRow = (row) =>
    `| ${row.map((value, index) => value.padEnd(widths[index])).join(' | ')} |`;
  const separator = `| ${widths.map((width) => '-'.repeat(width)).join(' | ')} |`;
  const rows = [renderRow(table[0]), separator, ...table.slice(1).map(renderRow)].join('\n');
  const summary = Object.entries(status.summary)
    .filter(([, count]) => count)
    .map(([name, count]) => `- ${name}: ${count}`)
    .join('\n');
  return `# R2 audit acceptance status

Generated from \`${manifestPath}\`.

- Captured at: ${status.captured_at}
- Git commit: ${status.git.commit ?? 'unknown'}
- Git tree: ${status.git.tree ?? 'unknown'}
- Dirty worktree: ${status.git.dirty}
- Host: ${status.host.os} ${status.host.arch}

## Summary

${summary || '- No gates defined'}

## Gates

${rows}

## Status meanings

- passed: requested runtime behavior executed and evidence is bound to the current git tree, production-source fingerprint and app binary hash when required.
- historical_pass: older evidence passed, but it is not bound to the current git tree/build.
- limited: real execution happened, but the evidence records a compatibility or safety-retention limit, and (when the gate requires it) is bound to the current git tree/build.
- historical_limited: older evidence recorded a compatibility or safety-retention limit, but it is not bound to the current git tree/build.
- prepared_not_executed: a harness exists but isolated external inputs were not supplied.
- not_executed: a defined non-provider runtime gate has no execution evidence yet.
- not_implemented: no runnable harness is present for the gate.
- prerequisite_only: setup/prerequisite validation exists without runtime behavior execution.
- ci_only_not_reproduced_locally: CI has the runtime lane, but no local evidence artifact is present in this checkout.
- blocked: required local runtime/tooling is unavailable.
- failed: evidence exists and records a failed gate.
`;
}

async function selfTest() {
  const testModule = relative(repoRoot, join(repoRoot, 'scripts/r2-audit-manifest.test.mjs'));
  execFileSync(process.execPath, ['--test', testModule], { cwd: repoRoot, stdio: 'inherit' });
}

async function main() {
  const args = process.argv.slice(2);
  assert(
    args.length <= 1 && ['--write', '--json', '--self-test'].includes(args[0] ?? '--json'),
    'Use --json, --write, or --self-test'
  );
  if (args[0] === '--self-test') {
    await selfTest();
    return;
  }
  const status = await createAcceptanceStatus();
  if (args[0] === '--write') {
    await mkdir(resolve(repoRoot, auditDir), { recursive: true });
    await writeFile(resolve(repoRoot, jsonOutput), JSON.stringify(status, null, 2) + '\n');
    await writeFile(resolve(repoRoot, markdownOutput), renderMarkdown(status));
  } else {
    console.log(JSON.stringify(status, null, 2));
  }
}

if (isMainModule(process.argv[1], scriptPath)) {
  main().catch((error) => {
    console.error(error?.stack ?? error);
    process.exitCode = 1;
  });
}
