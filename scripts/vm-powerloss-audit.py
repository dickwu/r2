#!/usr/bin/env python3
"""Witness native NFS ACKs, abruptly power-cut an owned VM, then verify recovery.

The Swift VM runner and guest build are explicit inputs. This script never
controls host power or discovers production storage. Evidence is scoped to
the tested guest, binary, filesystem and cut points, not Windows/host power.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import selectors
import shlex
import subprocess
import sys
import time
import uuid

WRITER = "mount::nfs_fs::native_smoke_tests::vm_powercut_tests::vm_nfs_powercut_writer"
RECOVER = "mount::nfs_fs::native_smoke_tests::vm_powercut_tests::vm_nfs_powercut_recover"


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def command(runtime, name, *args, timeout=30):
    result = subprocess.run([sys.executable, str(runtime / name), *args], capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"{name} failed ({result.returncode}): {result.stderr[-3000:]}")
    return result.stdout


def control(runtime, action, owner):
    current = json.loads(command(runtime, "control.py", "status"))
    if not current.get("ok") or current["status"]["owner"] != owner:
        raise RuntimeError("VM control owner changed; refusing control action")
    result = json.loads(command(runtime, "control.py", action))
    if not result.get("ok") or result["status"]["owner"] != owner:
        raise RuntimeError("VM control operation failed or owner changed")
    return result


def guest_command(binary, environment, test):
    args = ["sudo", "-n", "env", *(f"{key}={value}" for key, value in environment.items()),
            binary, "--exact", test, "--ignored", "--nocapture", "--test-threads=1"]
    return " ".join(shlex.quote(arg) for arg in args)


def cases():
    result = []
    for size in (4096, 131072, 1048576):
        result.append({"name": f"new-{size}", "base_size": 0,
                       "mutations": [{"kind": "write", "offset": 0, "size": size, "seed": 19}], "cut_after": 1})
    operations = [
        {"kind": "write", "offset": 131072, "size": 4096, "seed": 23},
        {"kind": "write", "offset": 132000, "size": 4096, "seed": 47},
        {"kind": "truncate", "size": 65536},
        {"kind": "truncate", "size": 262144},
    ]
    for cut in range(1, len(operations) + 1):
        result.append({"name": f"overlap-truncate-{cut}", "base_size": 1048576,
                       "mutations": operations, "cut_after": cut})
    # Cross the production record-count checkpoint/compaction boundary and
    # then append again; LSN reuse cannot hide the last acknowledged write.
    operations = [{"kind": "write", "offset": index * 4096, "size": 4096, "seed": index % 251}
                  for index in range(129)]
    for cut in (64, 65, 129):
        result.append({"name": f"checkpoint-{cut}", "base_size": 0,
                       "mutations": operations, "cut_after": cut})
    return result


def wait_for_ack(process, cutoff, log, timeout):
    deadline = time.monotonic() + timeout
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    pending = b""
    witnesses = []
    try:
        while time.monotonic() < deadline:
            events = selector.select(min(1, max(0, deadline - time.monotonic())))
            if not events:
                if process.poll() is not None:
                    raise RuntimeError("Guest writer exited before the client ACK")
                continue
            chunk = os.read(process.stdout.fileno(), 65536)
            if not chunk:
                raise RuntimeError("Guest writer output closed before the client ACK")
            log.write(chunk)
            log.flush()
            pending += chunk
            while b"\n" in pending:
                line, pending = pending.split(b"\n", 1)
                marker = b"R2_VM_ACK "
                if marker not in line:
                    continue
                witness = json.loads(line.split(marker, 1)[1])
                if witness.get("kind") != "kernel_nfs_fsync_returned" or witness.get("operation") != len(witnesses) + 1:
                    raise RuntimeError("Invalid or out-of-order client ACK witness")
                if witness.get("lsn", 0) < 1 or not witness.get("sha256"):
                    raise RuntimeError("ACK witness lacks durable identity")
                witnesses.append(witness)
                if len(witnesses) == cutoff:
                    return witnesses
                process.stdin.write(b"continue\n")
                process.stdin.flush()
        raise TimeoutError("Native client did not reach the requested ACK boundary")
    finally:
        selector.close()


def wait_for_boot(runtime, old_boot, timeout=120):
    deadline = time.monotonic() + timeout
    last_error = ""
    while time.monotonic() < deadline:
        try:
            boot = command(runtime, "ssh.py", "cat /proc/sys/kernel/random/boot_id", timeout=10).strip()
            if boot and boot != old_boot:
                return boot
        except (RuntimeError, subprocess.TimeoutExpired) as error:
            last_error = str(error)
        time.sleep(1)
    raise TimeoutError(f"Guest did not boot with a new boot ID: {last_error}")


def validate_recovered(value, expected):
    if not all(value.get(field) is True for field in ("passed", "mount_restore", "provider_published")):
        raise ValueError("Recovery must include production mount restore and verified publication")
    for field in ("bucket", "key", "size", "sha256"):
        if value.get(field) != expected.get(field):
            raise ValueError(f"Recovered {field} differs from the client ACK")
    if value.get("checkpoint_lsn", 0) < expected["lsn"]:
        raise ValueError("Recovery omitted an acknowledged LSN")


def self_test():
    expected = {"bucket": "photos", "key": "key", "size": 4, "sha256": "abc", "lsn": 8}
    recovered = {**expected, "checkpoint_lsn": 8, "passed": True, "mount_restore": True, "provider_published": True}
    validate_recovered(recovered, expected)
    for change in ({"passed": False}, {"mount_restore": False}, {"provider_published": False},
                   {"bucket": "other"}, {"key": "other"}, {"size": 3}, {"sha256": "wrong"}, {"checkpoint_lsn": 7}):
        try:
            validate_recovered({**recovered, **change}, expected)
        except ValueError:
            pass
        else:
            raise AssertionError(f"Broken recovery was accepted: {change}")
    assert all(1 <= case["cut_after"] <= len(case["mutations"]) for case in cases())
    print("VM acceptance guards: 9 checks passed; no VM operations performed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="Offline negative tests; never controls a VM")
    parser.add_argument("--runtime", type=Path)
    parser.add_argument("--build-json", type=Path, help="Immutable guest binary/source build evidence")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--case", action="append", help="Subset of named cases; recorded as smoke, never full matrix")
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if not args.runtime or not args.build_json or not args.output:
        parser.error("--runtime, --build-json and --output are required for execution")
    runtime = args.runtime.resolve(strict=True)
    build = json.loads(args.build_json.read_text())
    binary, expected_hash = build["guest_binary"], build["binary_sha256"]
    assert build["production_source_sha256"] and binary.startswith("/home/audit/")
    selected = [case for case in cases() if not args.case or case["name"] in args.case]
    if not selected or (args.case and set(args.case) != {case["name"] for case in selected}):
        parser.error("Unknown or empty case selection")
    status = json.loads(command(runtime, "control.py", "status"))
    assert status["ok"] and status["status"]["state_raw"] == 1
    owner = status["status"]["owner"]
    actual_hash = command(runtime, "ssh.py", f"sha256sum {shlex.quote(binary)}").split()[0]
    if actual_hash != expected_hash:
        raise RuntimeError("Guest test binary differs from the immutable build record")
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    evidence = {"mode": "smoke" if args.case else "kernel_nfs_ack_vm_powercut_matrix",
                "scope": "client-observed stable NFS fsync followed by guest power cut; no claim of physical host or Windows power-loss behavior",
                "build": build, "build_record_sha256": sha256(args.build_json),
                "harness_sha256": sha256(__file__), "vm": status["status"], "cases": [], "passed": False}

    def save():
        output.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n")

    try:
        for case in selected:
            case_id = "case-" + uuid.uuid4().hex
            root = "/var/lib/r2-audit/" + case_id
            boot = command(runtime, "ssh.py", "cat /proc/sys/kernel/random/boot_id").strip()
            row = {"name": case["name"], "case_id": case_id, "root": root, "before_boot_id": boot,
                   "scenario": case, "passed": False}
            evidence["cases"].append(row)
            environment = {"R2_VM_OWNER": owner, "R2_VM_AUDIT_ROOT": root,
                           "R2_VM_SCENARIO": json.dumps(case, separators=(",", ":"))}
            with (output.parent / (case_id + "-writer.log")).open("wb") as log:
                process = subprocess.Popen([sys.executable, str(runtime / "ssh.py"), guest_command(binary, environment, WRITER)],
                                           stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
                try:
                    witnesses = wait_for_ack(process, case["cut_after"], log, args.timeout)
                    row["acknowledgements"] = witnesses
                    save()  # Host evidence, never a guest filesystem sync.
                    cut_started = time.monotonic()
                    row["forced_stop"] = control(runtime, "stop", owner)
                    if row["forced_stop"]["status"]["state_raw"] != 0:
                        raise RuntimeError("Guest did not stop abruptly")
                    row["stop_elapsed_ms"] = (time.monotonic() - cut_started) * 1000
                    process.wait(timeout=15)
                except BaseException:
                    # This terminates only our SSH client. The VM remains owned;
                    # errors do not trigger an unreviewed second power cut.
                    if process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
                    raise
            row["restart"] = control(runtime, "start", owner)
            row["after_boot_id"] = wait_for_boot(runtime, boot)
            actual_hash = command(runtime, "ssh.py", f"sha256sum {shlex.quote(binary)}").split()[0]
            if actual_hash != expected_hash:
                raise RuntimeError("Guest binary changed across the power cut")
            environment = {"R2_VM_OWNER": owner, "R2_VM_AUDIT_ROOT": root,
                           "R2_VM_SCENARIO": json.dumps(case, separators=(",", ":")),
                           "R2_VM_EXPECTED_ACK": json.dumps(witnesses[-1], ensure_ascii=False, separators=(",", ":"))}
            result = command(runtime, "ssh.py", guest_command(binary, environment, RECOVER), timeout=args.timeout)
            (output.parent / (case_id + "-recovery.log")).write_text(result)
            recovered = [json.loads(line.split("R2_VM_RECOVERED ", 1)[1]) for line in result.splitlines() if "R2_VM_RECOVERED " in line]
            if len(recovered) != 1:
                raise RuntimeError("Production recovery did not report exactly one verified result")
            validate_recovered(recovered[0], witnesses[-1])
            row["recovered"] = recovered[0]
            row["passed"] = True
            save()
        evidence["passed"] = all(case["passed"] for case in evidence["cases"])
    except BaseException as error:
        evidence["failure"] = {"type": type(error).__name__, "message": str(error)}
        # Recover an owned, authoritatively stopped guest after a harness
        # failure. Never infer stopped state from a timeout, and don't restart
        # after an explicit user interrupt.
        if isinstance(error, Exception) and evidence["cases"]:
            row = evidence["cases"][-1]
            if row.get("forced_stop", {}).get("ok"):
                try:
                    current = control(runtime, "status", owner)
                    if current["status"]["state_raw"] == 0:
                        row["restart_after_harness_failure"] = control(runtime, "start", owner)
                except Exception as restart_error:
                    row["restart_after_harness_failure_error"] = str(restart_error)
        raise
    finally:
        save()
    print(json.dumps({"passed": evidence["passed"], "mode": evidence["mode"], "cases": len(selected), "evidence": str(output)}))


if __name__ == "__main__":
    main()
