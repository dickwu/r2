"""Run the opt-in kernel NFS smoke test on a disposable local storage fixture.

Use --sudo on a Unix CI host where creating a loopback NFS mount requires root.
Cargo compilation always runs as the caller, preserving its toolchain/cache.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--sudo", action="store_true")
args = parser.parse_args()
if os.name != "posix":
    parser.error("This smoke test currently requires a Unix NFS client")

repo = Path(__file__).resolve().parents[1]
test_name = "mount::nfs_fs::native_smoke_tests::native_nfs_write_read_rename_unmount"
compiled = subprocess.run(
    ["cargo", "test", "--manifest-path", "src-tauri/Cargo.toml", "--lib",
     "--locked", "--no-run", "--message-format=json"],
    cwd=repo, capture_output=True, text=True, check=True,
)
executables = []
for line in compiled.stdout.splitlines():
    artifact = json.loads(line)
    if (artifact.get("reason") == "compiler-artifact"
            and artifact.get("target", {}).get("name") == "r2_lib"
            and artifact.get("profile", {}).get("test")
            and artifact.get("executable")):
        executables.append(artifact["executable"])
if len(executables) != 1:
    raise RuntimeError(f"Expected one application test binary, found {len(executables)}")
executable = executables[0]
listing = subprocess.run([executable, "--list"], capture_output=True, text=True, check=True)
if f"{test_name}: test" not in listing.stdout.splitlines():
    raise RuntimeError("The native NFS test is missing from this build")
command = (["sudo", "-n", "--"] if args.sudo else []) + [
    executable, "--exact", test_name, "--ignored", "--nocapture",
]
subprocess.run(command, cwd=repo, check=True, timeout=90)
