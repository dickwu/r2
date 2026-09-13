"""Run the opt-in kernel NFS smoke test against a disposable storage fixture.

Unix may use --sudo for its temporary loopback mount. Windows requires an
installed Client for NFS and an explicitly reserved unused R2_NFS_TEST_DRIVE.
This runner never installs features, chooses a drive, or reboots the machine.
Cargo compilation always runs as the caller, preserving its toolchain/cache.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

TEST_NAME = "mount::nfs_fs::native_smoke_tests::native_nfs_write_read_rename_unmount"


def normalize_test_drive(value):
    if value is None:
        raise ValueError("Windows requires an explicit unused R2_NFS_TEST_DRIVE, such as Z:")
    drive = value.strip().rstrip("/\\")
    if len(drive) != 2 or not drive[0].isascii() or not drive[0].isalpha() or drive[1] != ":":
        raise ValueError("R2_NFS_TEST_DRIVE must be a drive letter, not a folder, wildcard, or share")
    return drive.upper()


def validate_platform(platform, sudo, drive_value, assigned_drives=None):
    if platform == "posix":
        return None
    if platform != "nt":
        raise ValueError(f"Unsupported native NFS test platform: {platform}")
    if sudo:
        raise ValueError("--sudo is Unix-only; run the Windows test in the prepared administrator session")
    drive = normalize_test_drive(drive_value)
    if assigned_drives is not None and assigned_drives & (1 << (ord(drive[0]) - ord("A"))):
        raise ValueError(f"Refusing native NFS smoke: drive {drive} is already assigned")
    return drive


def windows_drive_inventory():
    # Query assigned letters without opening an inaccessible/disconnected drive.
    import ctypes
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.GetLogicalDrives.argtypes = []
    kernel32.GetLogicalDrives.restype = ctypes.c_uint32
    mask = kernel32.GetLogicalDrives()
    if mask == 0:
        raise ctypes.WinError(ctypes.get_last_error())
    return mask


def validate_windows_environment(drive):
    validate_platform("nt", False, drive, windows_drive_inventory())
    root = os.environ.get("SystemRoot")
    if not root or not Path(root).is_absolute():
        raise ValueError("SystemRoot must identify an absolute Windows directory")
    for tool in ("mount.exe", "umount.exe"):
        path = Path(root) / "System32" / tool
        if not path.is_file():
            raise ValueError(f"Windows Client for NFS is unavailable: {path} is missing")


def test_executable(messages):
    executables = []
    for line in messages.splitlines():
        artifact = json.loads(line)
        if (artifact.get("reason") == "compiler-artifact"
                and artifact.get("target", {}).get("name") == "r2_lib"
                and artifact.get("profile", {}).get("test")
                and artifact.get("executable")):
            executables.append(artifact["executable"])
    if len(executables) != 1:
        raise ValueError(f"Expected one application test binary, found {len(executables)}")
    return executables[0]


def require_native_test(listing):
    if f"{TEST_NAME}: test" not in listing.splitlines():
        raise ValueError("The native NFS test is missing from this build; an empty filtered run is not acceptance")


def self_test():
    """Exercise guards/artifact validation without compiling or mounting."""
    import unittest

    class RunnerGuards(unittest.TestCase):
        def test_explicit_drive_is_required_and_cannot_escape_to_a_path(self):
            for value in (None, "", "*", "C:\\Users", "\\\\server\\share", "X:folder", "1:", "Å:"):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    normalize_test_drive(value)
            self.assertEqual(normalize_test_drive(" z:\\ "), "Z:")

        def test_occupied_letters_are_refused(self):
            occupied = (1 << 2) | (1 << 25)
            for value in ("C:", "z:"):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    validate_platform("nt", False, value, occupied)
            self.assertEqual(validate_platform("nt", False, "Y:", occupied), "Y:")

        def test_windows_rejects_sudo_and_unix_does_not_require_a_drive(self):
            with self.assertRaises(ValueError):
                validate_platform("nt", True, "Z:")
            self.assertIsNone(validate_platform("posix", True, None))
            with self.assertRaises(ValueError):
                validate_platform("unknown", False, None)

        def test_only_one_actual_application_test_artifact_is_accepted(self):
            artifact = {"reason": "compiler-artifact", "target": {"name": "r2_lib"},
                        "profile": {"test": True}, "executable": "test-binary.exe"}
            line = json.dumps(artifact)
            self.assertEqual(test_executable(line), "test-binary.exe")
            for messages in ("", line + "\n" + line,
                             json.dumps({**artifact, "profile": {"test": False}})):
                with self.subTest(messages=messages), self.assertRaises(ValueError):
                    test_executable(messages)

        def test_a_missing_ignored_test_is_an_error(self):
            require_native_test(f"{TEST_NAME}: test\nother: test")
            for listing in ("", "other: test", f"prefix::{TEST_NAME}: test"):
                with self.subTest(listing=listing), self.assertRaises(ValueError):
                    require_native_test(listing)

    result = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(RunnerGuards))
    return 0 if result.wasSuccessful() else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sudo", action="store_true")
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--validate-only", action="store_true", help="check platform and Windows prerequisites without compiling or mounting")
    modes.add_argument("--self-test", action="store_true", help="test runner guards without external commands")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    try:
        drive = validate_platform(os.name, args.sudo, os.environ.get("R2_NFS_TEST_DRIVE"))
        if drive is not None:
            validate_windows_environment(drive)
    except (ValueError, OSError) as error:
        parser.error(str(error))
    if args.validate_only:
        print(json.dumps({"platform": os.name, "drive": drive, "validated": True}))
        return 0

    repo = Path(__file__).resolve().parents[1]
    compiled = subprocess.run(
        ["cargo", "test", "--manifest-path", "src-tauri/Cargo.toml", "--lib",
         "--locked", "--no-run", "--message-format=json"],
        cwd=repo, capture_output=True, text=True,
    )
    if compiled.returncode:
        sys.stderr.write(compiled.stderr)
        raise RuntimeError(f"Native NFS test compilation failed with exit {compiled.returncode}")
    executable = test_executable(compiled.stdout)
    # Loader errors must fail before invoking an ignored test. This also makes
    # --no-run or a missing Windows cfg incapable of appearing as test success.
    listing = subprocess.run([executable, "--list"], capture_output=True, text=True)
    if listing.returncode:
        raise RuntimeError(
            f"Native test binary could not run --list (exit 0x{listing.returncode & 0xffffffff:08x}). "
            f"No native NFS acceptance occurred.\n{listing.stderr}"
        )
    require_native_test(listing.stdout)
    environment = os.environ.copy()
    if drive is not None:
        validate_windows_environment(drive)  # Recheck after the build.
        environment["R2_NFS_TEST_DRIVE"] = drive
    command = (["sudo", "-n", "--"] if args.sudo else []) + [
        executable, "--exact", TEST_NAME, "--ignored", "--nocapture",
    ]
    subprocess.run(command, cwd=repo, env=environment, check=True, timeout=120 if drive else 90)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
