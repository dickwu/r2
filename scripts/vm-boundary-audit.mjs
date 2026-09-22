#!/usr/bin/env node
/** Prepared VM boundary durability harness.
 *
 * This script describes and validates the interface for real VM power-loss
 * acceptance at stable NFS ACK boundaries. It does not power off any machine
 * by default. --execute only records a validated isolated VM contract unless a
 * future runner implements the destructive crash steps.
 */
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';

const BOUNDARIES = Object.freeze([
  'before-write-intent-fsync',
  'after-write-intent-before-data-fsync',
  'after-data-fsync-before-manifest',
  'after-manifest-before-ack',
  'immediately-after-ack',
  'during-recovery-before-republish',
]);

const PLAN = Object.freeze({
  status: 'prepared_not_executed',
  required_inputs: [
    'VM_AUDIT_RUNNER',
    'VM_AUDIT_TARGET',
    'VM_AUDIT_SSH_HOST',
    'VM_AUDIT_SSH_USER',
    'VM_AUDIT_APP_BINARY',
    'VM_AUDIT_APP_ID',
  ],
  allowed_runners: ['qemu-monitor', 'utmctl', 'tart', 'multipass', 'manual-ssh'],
  boundaries: BOUNDARIES,
  output_contract:
    'A successful run must include boundary ID, acknowledged byte range, pre-crash durable LSN, crash method, boot identity, recovery result and remote object hash.',
});

const MODES = new Set(['--plan', '--self-test', '--execute']);
const args = process.argv.slice(2);
assert(
  args.length <= 1 && (!args.length || MODES.has(args[0])),
  'Use --plan, --self-test, or --execute'
);
const mode = args[0] ?? '--plan';

function sha(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

export function validateVmConfig(input) {
  for (const key of ['runner', 'target', 'sshHost', 'sshUser', 'appBinary', 'appId']) {
    assert(typeof input[key] === 'string' && input[key].trim(), `Missing explicit ${key} input`);
  }
  assert(PLAN.allowed_runners.includes(input.runner), 'Unsupported VM audit runner');
  assert(
    !/[;&|`$]/.test(input.target),
    'VM target must be an opaque runner target, not shell text'
  );
  assert(/^[A-Za-z0-9_.:@-]+$/.test(input.sshHost), 'SSH host contains unsupported characters');
  assert(/^[A-Za-z0-9_.-]+$/.test(input.sshUser), 'SSH user contains unsupported characters');
  assert(/^[A-Za-z0-9_.-]+$/.test(input.appId), 'App id must be an isolated identifier');
  return { ...input };
}

if (mode === '--plan') {
  console.log(JSON.stringify(PLAN, null, 2));
} else if (mode === '--self-test') {
  const valid = {
    runner: 'manual-ssh',
    target: 'r2-audit-vm',
    sshHost: '127.0.0.1:2222',
    sshUser: 'audit',
    appBinary: '/opt/r2-audit/r2',
    appId: 'com.lifefarmer.r2.audit-vm',
  };
  validateVmConfig(valid);
  for (const change of [
    { runner: 'production-cloud' },
    { target: 'vm; rm -rf /' },
    { sshHost: 'host $(bad)' },
    { sshUser: 'bad user' },
    { appId: 'bad app id' },
  ]) {
    assert.throws(() => validateVmConfig({ ...valid, ...change }));
  }
  assert.equal(BOUNDARIES.length, 6);
  assert(BOUNDARIES.includes('immediately-after-ack'));
  console.log('Offline VM boundary harness guard checks passed. No SSH or VM commands run.');
} else {
  const config = validateVmConfig({
    runner: process.env.VM_AUDIT_RUNNER,
    target: process.env.VM_AUDIT_TARGET,
    sshHost: process.env.VM_AUDIT_SSH_HOST,
    sshUser: process.env.VM_AUDIT_SSH_USER,
    appBinary: process.env.VM_AUDIT_APP_BINARY,
    appId: process.env.VM_AUDIT_APP_ID,
  });
  const owner = randomUUID();
  const output = resolve(process.env.VM_AUDIT_OUTPUT || `.omx/artifacts/vm-boundary/${owner}.json`);
  await mkdir(dirname(output), { recursive: true });
  const report = {
    status: 'prepared_not_executed',
    reason:
      'VM contract validated; destructive crash/reboot orchestration still needs runner implementation before this can close power-loss acceptance.',
    captured_at: new Date().toISOString(),
    owner,
    runner: config.runner,
    target: config.target,
    ssh_host: config.sshHost,
    ssh_user: config.sshUser,
    app_binary: config.appBinary,
    app_id: config.appId,
    harness_sha256: sha(await readFile(import.meta.filename)),
    boundaries: BOUNDARIES,
  };
  await writeFile(output, JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
  console.log(JSON.stringify(report, null, 2));
}
