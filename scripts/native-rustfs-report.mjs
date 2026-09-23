/** Evidence skeleton and guaranteed pointers for scripts/native-rustfs-audit.mjs.
 * The harness executes on import (it asserts the host, verifies the pinned
 * archive and starts servers), so the report contract that the manifest
 * self-test checks against the rustfs-* gates lives in this side-effect-free
 * module, which the harness itself uses to build its evidence.
 */
import { auditProvenance, PROVENANCE_POINTERS } from './audit-source.mjs';

// Present in every evidence file the harness writes, protocol-only or --native.
export const REPORT_POINTERS = Object.freeze([
  ...PROVENANCE_POINTERS,
  '/scope',
  '/captured_at',
  '/rustfs_version',
  '/rustfs_commit',
  '/archive_sha256',
  '/binary_sha256',
  '/harness_sha256',
  '/version_output',
  '/host',
  '/conditions',
  '/assertions',
]);

// Added once the --native phase has discovered the isolated app (real-rustfs-native.json).
export const NATIVE_REPORT_POINTERS = Object.freeze([
  ...REPORT_POINTERS,
  '/native/app_id',
  '/native/app_binary_sha256',
]);

export function createEvidence({
  native,
  repo,
  rustfs,
  binarySha256,
  harnessSha256,
  versionOutput,
}) {
  return {
    scope: native
      ? 'Disposable loopback RustFS SDK and isolated native Tauri Move/NFS'
      : 'Disposable loopback RustFS SDK only; no Tauri application launched',
    captured_at: new Date().toISOString(),
    ...auditProvenance(repo),
    rustfs_version: rustfs.version,
    rustfs_commit: rustfs.commit,
    archive_sha256: rustfs.archiveSha256,
    binary_sha256: binarySha256,
    harness_sha256: harnessSha256,
    version_output: versionOutput,
    host: { platform: process.platform, architecture: process.arch },
    conditions: {},
    assertions: [],
  };
}

export function nativeEvidence({ appId, appBinarySha256 }) {
  return {
    app_id: appId,
    app_binary_sha256: appBinarySha256,
    discovery: 'owned PID loopback listener plus live app identifier',
    moves: [],
  };
}
