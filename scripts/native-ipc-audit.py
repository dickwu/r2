"""Exercise an isolated desktop profile against the local Rust S3 fixture.

Requires a connector-enabled binary built with the supplied audit identifier
and the ignored ipc_storage_fixture_daemon test running separately. Never use
this script with the normal application profile or a real storage endpoint.
"""
import argparse
import json
import mmap
import subprocess
import time
import urllib.parse
import urllib.request
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--app-id", required=True)
parser.add_argument("--server-ready", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
repo = Path(__file__).resolve().parents[1]
binary = repo / "src-tauri/target/debug/r2"
pid_file = repo / "src-tauri/target/.connector.json"
assert args.app_id.startswith("com.lifefarmer.r2.audit-"), "isolated profile required"
with binary.open("rb") as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as data:
    assert data.find(args.app_id.encode()) >= 0, "binary does not contain the expected audit profile"
endpoints = json.loads(args.server_ready.read_text())
for endpoint in endpoints.values():
    url = urllib.parse.urlsplit(endpoint)
    assert url.scheme == "http" and url.hostname == "127.0.0.1", "only the local fixture is allowed"


def storage(endpoint):
    with urllib.request.urlopen(endpoint + "/__fixture_status", timeout=5) as response:
        return json.load(response)


assert storage(endpoints["source"])["objects"]["source.txt"] == "correct-data"
assert storage(endpoints["destination"])["objects"]["source.txt"] == "wrong---data"
log = (repo / ".omx/artifacts/native-ipc-audit.log").open("ab")
app = None
port = None


def stop():
    global app
    if app is not None and app.poll() is None:
        app.terminate()
        try:
            app.wait(timeout=10)
        except subprocess.TimeoutExpired:
            app.kill()
            app.wait(timeout=5)
    app = None


def cli(*command):
    result = subprocess.run(["tauri-connector", "--host", "127.0.0.1", "--port", str(port), *command], capture_output=True, text=True, timeout=20)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    return json.loads(result.stdout)


def start():
    global app, port
    app = subprocess.Popen([str(binary)], cwd=repo, stdout=log, stderr=log)
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if app.poll() is not None:
            raise RuntimeError("isolated application stopped during startup")
        try:
            record = json.loads(pid_file.read_text())
            if record["pid"] == app.pid and record["app_id"] == args.app_id:
                port = record["ws_port"]
                state = cli("state")
                assert state["app"]["identifier"] == args.app_id
                return
        except (FileNotFoundError, json.JSONDecodeError, RuntimeError):
            pass
        time.sleep(0.2)
    raise RuntimeError("isolated connector did not become ready")


def ipc(command, payload):
    return cli("ipc", "exec", command, "-a", json.dumps(payload))


def wait_status(source, expected):
    deadline = time.monotonic() + 20
    tasks = []
    while time.monotonic() < deadline:
        tasks = ipc("get_move_tasks", {"sourceBucket": "photos", "sourceAccountId": source["account_id"]})
        if tasks and tasks[0]["status"] == expected:
            return tasks[0]
        time.sleep(0.2)
    raise RuntimeError(f"expected {expected}, got {tasks}")


try:
    start()
    configs = {}
    accounts = {}
    for role, endpoint in endpoints.items():
        account_input = {"name": "Audit " + role, "access_key_id": "fixture-access", "secret_access_key": "fixture-secret", "endpoint_scheme": "http", "endpoint_host": urllib.parse.urlsplit(endpoint).netloc, "force_path_style": True}
        account = ipc("create_minio_account", {"input": account_input})
        accounts[role] = {**account_input, "id": account["id"]}
        configs[role] = {**account_input, "provider": "minio", "account_id": account["id"], "bucket": "photos", "region": None}
    ipc("start_batch_move", {"sourceConfig": configs["source"], "destConfig": configs["destination"], "operations": [{"source_key": "source.txt", "dest_key": "copy.txt", "overwrite": False}], "deleteOriginal": True})
    blocked = wait_status(configs["source"], "needs_auth")
    before_source = storage(endpoints["source"])
    before_destination = storage(endpoints["destination"])
    assert before_source["objects"]["source.txt"] == "correct-data"
    assert before_destination["objects"]["copy.txt"] == "correct-data"
    first_pid = app.pid
    stop()
    start()
    assert app.pid != first_pid
    accounts["source"]["secret_access_key"] = "fixture-secret-refreshed"
    configs["source"]["secret_access_key"] = "fixture-secret-refreshed"
    ipc("update_minio_account", {"input": accounts["source"]})
    ipc("resume_move", {"taskId": blocked["id"], "sourceConfig": configs["source"], "destConfig": configs["destination"]})
    finished = wait_status(configs["source"], "success")
    after_source = storage(endpoints["source"])
    after_destination = storage(endpoints["destination"])
    assert "source.txt" not in after_source["objects"]
    assert after_destination["objects"]["copy.txt"] == "correct-data"
    source_gets = sum(call == ["GET", "/photos/source.txt"] for call in after_source["calls"])
    destination_puts = sum(call == ["PUT", "/photos/copy.txt"] for call in after_destination["calls"])
    assert source_gets == 1 and destination_puts == 1, "resume retransferred an already verified copy"
    evidence = {"scope": "Actual Tauri IPC and persistent SQLite across app restart; two local HTTP storage fixtures, not real provider deployments", "app_id": args.app_id, "before_restart": blocked["status"], "after_restart": finished["status"], "source_gets": source_gets, "destination_puts": destination_puts, "destination_content": after_destination["objects"]["copy.txt"], "source_removed_only_after_resume": True}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps(evidence, indent=2))
finally:
    stop()
    log.close()
