# R2 audit acceptance status

Generated from `docs/engineering/r2-audit/acceptance-manifest.json`.

- Captured at: 2026-09-22T18:26:36.057Z
- Git commit: 8bfb0b1826cb2c35a102c726c26ff30b2c54f350
- Git tree: 1a9b23f17610eb1eb2e0dae80df8707f5508621a
- Dirty worktree: true
- Host: darwin arm64

## Summary

- historical_pass: 2
- limited: 1
- prepared_not_executed: 4
- not_implemented: 1
- prerequisite_only: 1
- ci_only_not_reproduced_locally: 1

## Gates

| id                          | status                         | mode                                                    | provider/build                           | evidence                  | note                                                                                                                                   |
| --------------------------- | ------------------------------ | ------------------------------------------------------- | ---------------------------------------- | ------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| rustfs-protocol             | historical_pass                | real_loopback_protocol                                  | 1.0.0-rc.6                               | real-rustfs-protocol.json | evidence is not bound to current git commit/tree (8bfb0b1826cb2c35a102c726c26ff30b2c54f350 / 1a9b23f17610eb1eb2e0dae80df8707f5508621a) |
| rustfs-native               | historical_pass                | real_loopback_native                                    | 1.0.0-rc.6                               | real-rustfs-native.json   | evidence is missing production source fingerprint                                                                                      |
| minio-native                | limited                        | real_loopback_native                                    | 7aac2a2c5b7c882e68c1ce017d8256be2feea27f | real-minio.json           |                                                                                                                                        |
| cloudflare-r2-protocol      | prepared_not_executed          | prepared_remote_provider                                |                                          |                           | missing evidence file: real-r2-protocol.json                                                                                           |
| aws-s3-provider             | prepared_not_executed          | prepared_remote_provider_with_client_side_response_loss |                                          |                           | missing evidence file: real-aws-s3.json                                                                                                |
| linux-native-nfs            | ci_only_not_reproduced_locally | ci_runtime                                              |                                          |                           | missing evidence file: linux-native-nfs.json                                                                                           |
| windows-native-nfs          | prerequisite_only              | prepared_runner_required                                |                                          |                           | missing evidence file: windows-native-nfs.json                                                                                         |
| windows-native-nfs-behavior | prepared_not_executed          | prepared_windows_behavior_ci                            |                                          |                           | missing evidence file: windows-native-nfs-behavior.json                                                                                |
| network-fault-matrix        | not_implemented                | partial_fault_runtime                                   |                                          |                           | missing evidence file: network-fault-matrix.json                                                                                       |
| power-loss-matrix           | prepared_not_executed          | prepared_disposable_vm_power_loss                       |                                          |                           | missing evidence file: power-loss-matrix.json                                                                                          |

## Status meanings

- passed: requested runtime behavior executed and evidence is bound to the current git tree, production-source fingerprint and app binary hash when required.
- historical_pass: older evidence passed, but it is not bound to the current git tree/build.
- limited: real execution happened, but the evidence records a compatibility or safety-retention limit.
- prepared_not_executed: a harness exists but isolated external inputs were not supplied.
- not_executed: a defined non-provider runtime gate has no execution evidence yet.
- not_implemented: no runnable harness is present for the gate.
- prerequisite_only: setup/prerequisite validation exists without runtime behavior execution.
- ci_only_not_reproduced_locally: CI has the runtime lane, but no local evidence artifact is present in this checkout.
- blocked: required local runtime/tooling is unavailable.
- failed: evidence exists and records a failed gate.
