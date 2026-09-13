#!/usr/bin/env python3
"""Native folder navigation measurements against disposable loopback S3 listing data.

Requires an already-built isolated connector binary; never builds or installs the app.
Example:
  python3 scripts/native-listing-audit.py --binary /tmp/audit/r2 \
    --app-id com.lifefarmer.r2.audit-example --pid-file /tmp/audit/.connector.json
The fixture serves actual 1,000-entry ListObjectsV2 pages to the production SDK.
All durations ending in _ms originate inside the real webview unless labelled host/fixture.
A double-requestAnimationFrame visibility timestamp is a frame opportunity proxy, not
an OS compositor or physical-display timestamp. --smoke never closes acceptance.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import math
import mmap
import os
from pathlib import Path
import platform
import select
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request
import uuid
from xml.sax.saxutils import escape

REPO = Path(__file__).resolve().parents[1]


class ListingFixture:
    def __init__(self, trials: int):
        self.bucket = "listing-audit-" + uuid.uuid4().hex[:12]
        self.datasets = {
            f"cold-{size}-{trial:03d}/": size
            for size in (10_000, 100_000)
            for trial in range(trials)
        }
        self.datasets.update({f"cancel-{size}/": size for size in (10_000, 100_000)})
        self.datasets.update({f"control-{size}/": size for size in (10_000, 100_000)})
        # Match S3 lexical listing order while exercising the UI's different
        # natural numeric order, instead of giving it pre-sorted numeric input.
        self.suffixes = {size: sorted(f"file-{index}.txt" for index in range(size)) for size in (10_000, 100_000)}
        self.calls: list[dict] = []
        self.lock = threading.Lock()
        self.active_trial: str | None = None
        self.gates: dict[str, threading.Event] = {}
        self.stopping = threading.Event()
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):
                pass

            def do_GET(self):
                url = urllib.parse.urlsplit(self.path)
                query = dict(urllib.parse.parse_qsl(url.query))
                if url.path == "/__listing_health":
                    self.respond(200, b'{"fixture":"native-listing-audit"}', "application/json")
                    return
                if url.path.rstrip("/") != "/" + fixture.bucket or query.get("list-type") != "2":
                    self.respond(404, b"<Error><Code>NoSuchKey</Code></Error>")
                    return
                prefix = query.get("prefix", "")
                background = query.get("delimiter") != "/"
                record = {
                    "trial": fixture.active_trial,
                    "prefix": prefix,
                    "token": query.get("continuation-token"),
                    "background": background,
                    "connection_peer_port": self.client_address[1],
                    "received_monotonic": time.monotonic(),
                }
                with fixture.lock:
                    fixture.calls.append(record)
                if background:
                    # The real background job is cancelled by the harness. Blocking its
                    # fixture response prevents it from pre-warming a cold target first.
                    record["fixture_background_gate"] = True
                    self.wait_until_released(None, record)
                    return
                try:
                    body, offset = fixture.page(prefix, query.get("continuation-token"), query.get("max-keys", "1000"))
                    record["offset"] = offset
                except ValueError as error:
                    record["error"] = str(error)
                    self.respond(400, b"<Error><Code>InvalidArgument</Code></Error>")
                    return
                gate = fixture.gates.get(prefix) if offset == 1000 else None
                if gate is not None:
                    record["second_page_gated"] = True
                    if not self.wait_until_released(gate, record):
                        return
                record["response_started_monotonic"] = time.monotonic()
                try:
                    self.respond(200, body)
                    record["response_bytes"] = len(body)
                    record["response_finished_monotonic"] = time.monotonic()
                except (BrokenPipeError, ConnectionResetError, OSError):
                    record["connection_closed"] = True

            def wait_until_released(self, gate, record):
                deadline = time.monotonic() + 25
                while not fixture.stopping.is_set() and time.monotonic() < deadline:
                    if gate is not None and gate.is_set():
                        return True
                    try:
                        readable, _, _ = select.select([self.connection], [], [], 0.025)
                        if readable and not self.connection.recv(1, socket.MSG_PEEK):
                            record["client_disconnected_monotonic"] = time.monotonic()
                            return False
                    except OSError:
                        record["client_disconnected_monotonic"] = time.monotonic()
                        return False
                record["fixture_wait_expired"] = True
                self.close_connection = True
                return False

            def respond(self, status, body, content_type="application/xml"):
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "keep-alive")
                self.end_headers()
                self.wfile.write(body)
                self.close_connection = False

            def do_POST(self):
                self.respond(405, b"<Error><Code>MethodNotAllowed</Code></Error>")

            do_PUT = do_POST
            do_DELETE = do_POST

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.endpoint = f"http://127.0.0.1:{self.server.server_port}"

    def page(self, prefix: str, token: str | None, max_keys: str):
        limit = min(1000, max(1, int(max_keys)))
        offset = 0
        if token is not None:
            token_prefix, separator, token_offset = token.rpartition("|")
            if not separator or token_prefix != prefix:
                raise ValueError("Continuation token has the wrong prefix")
            offset = int(token_offset)
        if prefix == "":
            if offset:
                raise ValueError("Root fits in one page")
            contents = "".join(f"<CommonPrefixes><Prefix>{escape(key)}</Prefix></CommonPrefixes>" for key in sorted(self.datasets))
            count, total, end = len(self.datasets), len(self.datasets), len(self.datasets)
        else:
            total = self.datasets.get(prefix)
            if total is None:
                raise ValueError("Unknown fixture prefix")
            if offset < 0 or offset >= total or offset % 1000:
                raise ValueError("Invalid continuation offset")
            end = min(total, offset + limit)
            count = end - offset
            name_prefix = prefix.rstrip("/")
            contents = "".join(
                f"<Contents><Key>{escape(prefix)}{name_prefix}-{self.suffixes[total][index]}</Key>"
                f"<Size>{index + 1}</Size><ETag>&quot;fixture-{index:06d}&quot;</ETag>"
                "<LastModified>2026-09-12T00:00:00Z</LastModified><StorageClass>STANDARD</StorageClass></Contents>"
                for index in range(offset, end)
            )
        truncated = end < total
        cursor = f"<NextContinuationToken>{escape(prefix)}|{end}</NextContinuationToken>" if truncated else ""
        body = (
            '<?xml version="1.0" encoding="UTF-8"?>'
            '<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
            f"<Name>{self.bucket}</Name><Prefix>{escape(prefix)}</Prefix><MaxKeys>{limit}</MaxKeys>"
            f"<KeyCount>{count}</KeyCount><IsTruncated>{str(truncated).lower()}</IsTruncated>"
            f"{cursor}{contents}</ListBucketResult>"
        ).encode()
        return body, offset

    def start(self):
        self.thread.start()

    def release(self, prefix):
        gate = self.gates.get(prefix)
        if gate:
            gate.set()

    def stop(self):
        self.stopping.set()
        for gate in self.gates.values():
            gate.set()
        self.server.shutdown()
        self.server.server_close()

    def trial_calls(self, trial):
        with self.lock:
            return [dict(call) for call in self.calls if call["trial"] == trial]


# Installed into the real page via the connector. It observes production behavior;
# it neither invokes the folder loader directly nor substitutes React Query data.
OBSERVER_JS = r"""
(async () => {
  if (window.__nativeListingAudit) await window.__nativeListingAudit.dispose();
  const originalSort = Array.prototype.sort;
  const audit = window.__nativeListingAudit = {
    trial: null, observed: [], ipc: [], completed_prefixes: {}, otherPageEvents: 0, errors: [],
    viewport: { width: innerWidth, height: innerHeight, devicePixelRatio, visibility: document.visibilityState, focused: document.hasFocus(),
      userAgent: navigator.userAgent, timeOrigin: performance.timeOrigin,
      scripts: [...document.scripts].map(script => script.src).filter(Boolean) },
  };
  // Measure actual production FileItem sorts for this generated prefix. This
  // delegates unchanged to the native method and never traverses/copies arrays.
  Array.prototype.sort = function(...args) {
    const trial = audit.trial;
    const item = this[0];
    if (!trial || trial.started_at === null || !item || typeof item.key !== 'string' ||
        typeof item.isFolder !== 'boolean' || !item.key.startsWith(trial.prefix)) {
      return originalSort.apply(this, args);
    }
    const start = performance.now();
    try { return originalSort.apply(this, args); }
    finally { trial.sorts.push({ items: this.length, started_at: start, duration_ms: performance.now()-start }); }
  };
  const observed = event => {
    const detail = event.detail;
    if (!detail || !['get_prefix_cache','list_prefix_stream','cancel_prefix_list'].includes(detail.command)) return;
    if (detail.provider !== 'minio' || detail.account_id !== audit.account_id || detail.bucket !== audit.bucket) return;
    let row = audit.ipc.find(candidate => candidate.operation_id === detail.operation_id);
    if (detail.phase === 'start') {
      row = { operation_id: detail.operation_id, command: detail.command,
        started_at: detail.at, trial: audit.trial?.id ?? null, prefix: detail.prefix,
        request_id: detail.request_id ?? null, generation: detail.generation ?? null };
      audit.ipc.push(row);
    } else if (row) {
      row.finished_at = detail.at; row.duration_ms = detail.at - row.started_at;
      if (detail.phase === 'error') row.error = detail.error;
      else if (detail.command === 'get_prefix_cache') row.cache = detail.result;
      else if (detail.command === 'list_prefix_stream') row.summary = detail.result;
    }
  };
  window.addEventListener('r2-listing-observation', observed);
  const accumulated = event => {
    const detail = event.detail;
    const trial = audit.trial;
    if (!trial || detail.provider !== 'minio' || detail.account_id !== audit.account_id || detail.bucket !== audit.bucket || detail.prefix !== trial.prefix) return;
    if (trial.request_id && (detail.request_id !== trial.request_id || detail.generation !== trial.generation)) return;
    trial.accumulations.push(detail);
  };
  window.addEventListener('r2-listing-accumulation', accumulated);
  const visible = element => {
    if (!element || document.visibilityState !== 'visible') return false;
    const box = element.getBoundingClientRect();
    const style = getComputedStyle(element);
    return box.width > 0 && box.height > 0 && box.bottom > 0 && box.top < innerHeight &&
      box.right > 0 && box.left < innerWidth && style.visibility !== 'hidden' && style.display !== 'none';
  };
  const expectedVisible = trial => [...document.querySelectorAll('.file-area .fl-name-text')].find(
    element => element.textContent.startsWith(trial.prefix.slice(0,-1) + '-file-') && visible(element));
  const check = () => {
    const trial = audit.trial;
    if (!trial || trial.started_at === null || !expectedVisible(trial)) return;
    if (trial.first_dom_at === null) {
      trial.first_dom_at = performance.now();
      trial.first_visible_filename = expectedVisible(trial).textContent;
      const frame = attempts => requestAnimationFrame(() => requestAnimationFrame(() => {
        if (audit.trial !== trial) return;
        if (!expectedVisible(trial)) {
          if (attempts < 120) frame(attempts + 1);
          else trial.frame_error = 'Expected row did not settle within 120 frame opportunities';
          return;
        }
        trial.first_frame_at = performance.now();
        trial.focused_at_first_frame = document.hasFocus();
        trial.frame_visibility_retries = attempts;
        trial.visible_row_count = [...document.querySelectorAll('.file-area .fl-name-text')].filter(visible).length;
        trial.visible_before_final_page = trial.final_page_at === null;
      }));
      frame(0);
    }
    const countText = trial.size.toLocaleString() + ' items';
    const fullCountVisible = [...document.querySelectorAll('.statusbar .sb-stat')].some(el => el.textContent.trim() === countText);
    if (fullCountVisible && trial.full_snapshot_dom_at === null) trial.full_snapshot_dom_at = performance.now();
    if (fullCountVisible && trial.final_page_at !== null && trial.full_network_dom_at === null) {
      trial.full_network_dom_at = performance.now();
      const frame = attempts => requestAnimationFrame(() => requestAnimationFrame(() => {
        if (audit.trial !== trial) return;
        if (expectedVisible(trial)) trial.full_network_frame_at = performance.now();
        else if (attempts < 120) frame(attempts + 1);
        else trial.full_frame_error = 'Complete listing did not settle within 120 frame opportunities';
      }));
      frame(0);
    }
  };
  const observer = new MutationObserver(check);
  observer.observe(document.documentElement, { subtree: true, childList: true, characterData: true });
  const clicked = event => {
    const trial = audit.trial;
    if (!trial || trial.started_at !== null) return;
    const row = event.target.closest?.('.fl-row');
    if (row?.querySelector('.fl-name-text')?.textContent !== trial.prefix.slice(0,-1)) return;
    trial.started_at = performance.now();
    trial.click_is_trusted = event.isTrusted;
    queueMicrotask(check);
  };
  document.addEventListener('click', clicked, true);
  const lifecycle = event => {
    if (audit.trial) audit.trial.browser_lifecycle.push({ event: event.type, at: performance.now(), focused: document.hasFocus(), visibility: document.visibilityState });
  };
  window.addEventListener('focus', lifecycle);
  window.addEventListener('blur', lifecycle);
  document.addEventListener('visibilitychange', lifecycle);
  const removePages = await window.__TAURI__.event.listen('folder-page', event => {
    const page = event.payload;
    const trial = audit.trial;
    if (!trial || page.provider !== 'minio' || page.account_id !== audit.account_id ||
        page.bucket !== audit.bucket || page.prefix !== trial.prefix) { audit.otherPageEvents++; return; }
    if (trial.request_id && (trial.request_id !== page.request_id || trial.generation !== page.generation)) {
      trial.ignored_scope_pages++; return;
    }
    if (!trial.request_id) { trial.request_id = page.request_id; trial.generation = page.generation; }
    const arrival = performance.now();
    if (trial.pages.length === 0) trial.first_page_at = arrival;
    trial.pages.push({ page_index: page.page_index, arrived_at: arrival, complete: page.complete,
      files: page.files.length, folders: page.folders.length, next_cursor: page.next_cursor,
      from_cache: page.from_cache, freshness: page.freshness, timing: page.timing ?? null });
    if (page.complete) { trial.final_page_at = arrival; audit.completed_prefixes[page.prefix] = arrival; }
  });
  audit.prepare = (id, prefix, size, mode) => {
    if ([...document.querySelectorAll('.crumbs button.crumb')].some(element => element.classList.contains('current') && element.title !== audit.bucket)) {
      throw new Error('Trial must start from fixture bucket root');
    }
    audit.trial = { id, prefix, size, mode, sentinel: prefix.slice(0,-1) + '-file-0.txt',
      started_at: null, first_dom_at: null, first_frame_at: null, first_page_at: null, final_page_at: null,
      request_id: null, generation: null, pages: [], ignored_scope_pages: 0,
      full_snapshot_dom_at: null, full_network_dom_at: null, full_network_frame_at: null, sorts: [], accumulations: [], browser_lifecycle: [],
      viewport: { width: innerWidth, height: innerHeight, devicePixelRatio, visibility: document.visibilityState, focused: document.hasFocus(), timeOrigin: performance.timeOrigin } };
    return true;
  };
  audit.read = () => ({ trial: audit.trial, ipc: audit.ipc.filter(row => row.trial === audit.trial?.id),
    other_page_events: audit.otherPageEvents, errors: audit.errors });
  audit.dispose = async () => { observer.disconnect(); document.removeEventListener('click', clicked, true);
    removePages(); window.removeEventListener('r2-listing-observation', observed);
    window.removeEventListener('focus', lifecycle); window.removeEventListener('blur', lifecycle);
    document.removeEventListener('visibilitychange', lifecycle);
    window.removeEventListener('r2-listing-accumulation', accumulated);
    Array.prototype.sort = originalSort; delete window.__nativeListingAudit; };
  return { installed: true, viewport: audit.viewport };
})()
"""


class MeasurementOccluded(RuntimeError):
    """The OS hid the test window during a timed sample, so rAF cannot be measured."""


class NativeAudit:
    def __init__(self, args, fixture):
        self.args, self.fixture = args, fixture
        self.app = None
        self.awake = None
        self.port = None
        self.log = tempfile.TemporaryFile()
        self.account = None
        self.config = None
        self.host_trial_context = {}
        self.run_id = "run-" + uuid.uuid4().hex[:12]
        self.evidence = {
            "schema": 1,
            "measurement_run_id": self.run_id,
            "measurement_runs": {},
            "measurement_protocol": {"version": 2, "primary_cold": "ungated", "app_variant": "post-structural-sharing-fix",
                "gated_controls": "Separate one-per-size first-page control, excluded from the 180-sample matrix",
                "startup": "r2:shell-ready document-origin mark; initial process document and configured webview reloads are reported separately"},
            "scope": "Real Tauri main webview, production SDK/SQLite/IPC/React/Virtuoso; generated loopback ListObjectsV2 fixture only",
            "captured_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "acceptance_complete": False,
            "smoke_only": args.smoke,
            "requested_trials_per_case": args.trials,
            "app_id": args.app_id,
            "binary_path": str(args.binary),
            "declared_build_profile": getattr(args, "build_profile", "unspecified"),
            "percentile_method": "Nearest rank: sorted_values[ceil(n * fraction) - 1]",
            "binary_sha256": hashlib.file_digest(args.binary.open("rb"), "sha256").hexdigest(),
            "host": {"system": platform.system(), "release": platform.release(), "machine": platform.machine()},
            "fixture": {"endpoint": fixture.endpoint, "bucket": fixture.bucket, "page_size": 1000, "http_keep_alive": True,
                "cold_definition": "No folder snapshot in SQLite or React Query; native process/client pool may already be warm",
                "key_distribution": "Unpadded numeric filenames in S3 lexical order; the production UI performs natural numeric sorting",
                "background_policy": "Actual background sync is cancelled; fixture blocks flat background LIST until disconnect so it cannot pre-warm cold prefixes",
                "second_page_policy": "Primary cold samples are ungated. Only control-* and cancel-* prefixes hold page 2 for independent first-page/cancellation controls"},
            "metrics_unavailable": [
                "OS compositor or physical first-pixel timestamp (double-rAF is a frame opportunity proxy)",
                "Rust queue/network/DB timing: absent until a timing-enabled native binary is supplied",
                "Actual launch-to-interactive or physical-startup timing: shell-ready is a document initialization/commit proxy; webview reloads are not cold process launches",
                "Real provider/WAN latency and throughput; fixture metadata has no stored object payloads",
            ],
            "timing_interpretation": {
                "accumulation_ms": "Includes delta array sorting; do not add sort_ms to it again",
                "native_stages": "Shared-flight stage counters are cumulative snapshots; summary only is used for per-trial aggregates",
                "emit_to_js_estimate": "Browser timeOrigin + event arrival minus native Unix emit timestamp; cross-clock estimate includes serialization, IPC and JS event scheduling",
                "roundtrip_residual": "Frontend observed roundtrip minus native elapsed duration; includes framework dispatch/serialization and JS scheduling, not pure transport",
            },
            "samples": [], "gated_controls": [], "startup_samples": [], "cancellations": [], "screenshots": [], "failures": [],
        }

        if getattr(args, "resume_from", None):
            previous = json.loads(args.resume_from.read_text())
            assert previous["binary_sha256"] == self.evidence["binary_sha256"], "Resume requires the identical frozen executable"
            assert previous["app_id"] == args.app_id and previous["requested_trials_per_case"] == args.trials
            assert not previous.get("smoke_only"), "Smoke samples cannot become acceptance samples"
            assert previous.get("measurement_protocol", {}).get("version") == 2 and previous["measurement_protocol"]["primary_cold"] == "ungated", "Cannot mix gated baseline samples into the ungated protocol"
            previous_id = previous.get("measurement_run_id", "previous-" + previous["captured_at"])
            self.evidence["measurement_runs"].update(previous.get("measurement_runs", {}))
            self.evidence["measurement_runs"].setdefault(previous_id, {key: previous.get(key) for key in (
                "captured_at", "app_id", "binary_path", "binary_sha256", "declared_build_profile", "fixture", "owned_pid", "native_state_after_ui_ready", "generated_account_removed")})
            for sample in previous["samples"]:
                sample.setdefault("measurement_run_id", previous_id)
            for cancellation in previous.get("cancellations", []):
                cancellation.setdefault("measurement_run_id", previous_id)
            self.evidence["samples"] = previous["samples"]
            for entry in previous["samples"] + previous.get("gated_controls", []) + previous.get("priming_samples", []):
                fixture.datasets[entry["prefix"]] = entry["size"]
            self.evidence["cancellations"] = previous.get("cancellations", [])
            self.evidence["gated_controls"] = previous.get("gated_controls", [])
            self.evidence["startup_samples"] = previous.get("startup_samples", [])
            self.evidence["screenshots"] = previous.get("screenshots", [])
            self.evidence["priming_samples"] = previous.get("priming_samples", [])
            self.evidence["interrupted_attempts"] = previous.get("interrupted_attempts", [])
            self.evidence["owned_app_activations"] = previous.get("owned_app_activations", [])
            self.evidence["resume_history"] = previous.get("resume_history", []) + [{
                "artifact": str(args.resume_from.resolve()), "retained_valid_samples": len(previous["samples"]),
                "failures": previous.get("failures", []), "failure_observation": previous.get("failure_observation"),
                "failure_ui": previous.get("failure_ui"), "failure_native_state": previous.get("failure_native_state"),
            }]

    def save(self):
        self.args.output.parent.mkdir(parents=True, exist_ok=True)
        self.args.output.write_text(json.dumps(self.evidence, indent=2) + "\n")

    def cli(self, *command, timeout=40):
        result = subprocess.run(["tauri-connector", "--host", "127.0.0.1", "--port", str(self.port),
            "--window-id", "main", *command], capture_output=True, text=True, timeout=timeout)
        if result.returncode:
            raise RuntimeError(result.stderr.strip() or result.stdout.strip())
        try:
            return json.loads(result.stdout)
        except json.JSONDecodeError:
            # `eval` prints primitive strings without JSON quotation marks.
            return result.stdout.strip()

    def js(self, script):
        # CLI wraps its argument as an expression; indirect eval accepts both
        # instrumentation statements and async IIFE expressions unchanged.
        result = self.cli("eval", "(0,eval)(" + json.dumps(script) + ")")
        # Connector versions may wrap execute_js results rather than returning directly.
        if isinstance(result, dict) and "result" in result and set(result).issubset({"result", "executionTime", "execution_time", "success"}):
            result = result["result"]
        if isinstance(result, str):
            try:
                return json.loads(result)
            except json.JSONDecodeError:
                pass
        return result

    def ipc(self, command, payload=None):
        return self.cli("ipc", "exec", command, "-a", json.dumps(payload or {}), timeout=65)

    def wait(self, predicate, label, timeout=30):
        deadline = time.monotonic() + timeout
        latest = None
        while time.monotonic() < deadline:
            if self.app is not None and self.app.poll() is not None:
                raise RuntimeError(f"Audit application exited during {label}")
            try:
                latest = predicate()
                if latest:
                    return latest
            except MeasurementOccluded:
                raise
            except (RuntimeError, subprocess.TimeoutExpired, json.JSONDecodeError):
                pass
            time.sleep(0.04)
        raise RuntimeError(f"Timed out waiting for {label}: {str(latest)[:300]}")

    def start(self):
        self.evidence["host_process_spawn_epoch_ms"] = time.time_ns() / 1_000_000
        self.evidence["host_process_spawn_monotonic"] = time.monotonic()
        self.app = subprocess.Popen([str(self.args.binary)], cwd=REPO, stdout=self.log, stderr=self.log)
        if platform.system() == "Darwin":
            # Temporary assertions end with this owned process; no power settings change.
            self.awake = subprocess.Popen(["caffeinate", "-d", "-i", "-u", "-w", str(self.app.pid)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        def connected():
            try:
                candidates = list(dict.fromkeys([self.args.pid_file, self.args.binary.parent / ".connector.json", REPO / "src-tauri/target/.connector.json", REPO / ".connector.json"]))
                record = None
                for candidate in candidates:
                    try:
                        found = json.loads(candidate.read_text())
                    except (FileNotFoundError, json.JSONDecodeError):
                        continue
                    if found.get("pid") == self.app.pid and found.get("app_id") == self.args.app_id:
                        record = found
                        self.args.pid_file = candidate
                        self.evidence["connector_pid_file"] = str(candidate)
                        break
                if record is None:
                    # Older plugin builds omit the PID record after relocation.
                    # Inspect only sockets owned by this process, then verify
                    # the live app identifier before any action/IPC dispatch.
                    owned = subprocess.run(["lsof", "-nP", "-a", "-p", str(self.app.pid), "-iTCP", "-sTCP:LISTEN", "-Fn"], capture_output=True, text=True, timeout=5)
                    for line in owned.stdout.splitlines():
                        if not line.startswith("n127.0.0.1:"):
                            continue
                        candidate_port = int(line.rpartition(":")[2])
                        if not 9555 <= candidate_port <= 9655:
                            continue
                        self.port = candidate_port
                        try:
                            candidate_state = self.cli("state", timeout=5)
                        except (RuntimeError, subprocess.TimeoutExpired):
                            continue
                        if candidate_state["app"]["identifier"] == self.args.app_id:
                            record = {"pid": self.app.pid, "app_id": self.args.app_id, "ws_port": candidate_port}
                            self.evidence["connector_discovery"] = "Loopback listener ownership verified by lsof for launched PID, then live app identifier matched"
                            break
                    if record is None:
                        self.port = None
                        return False
                self.port = record["ws_port"]
                self.evidence["owned_pid"] = self.app.pid
                self.evidence["connector_port"] = self.port
                state = self.cli("state")
                if state["app"]["identifier"] != self.args.app_id:
                    raise AssertionError("Connector app identity mismatch")
                self.evidence["native_state"] = state
                return True
            except (FileNotFoundError, json.JSONDecodeError):
                return False
        self.wait(connected, "isolated connector", 35)
        self.wait(lambda: self.js("typeof window.__TAURI__ === 'object' && document.readyState === 'complete'"), "native page ready")
        self.collect_startup({"kind": "process-initial-document", "configuration": "before generated fixture setup"})
        self.cli("resize", "1400", "900")
        account_input = {"name": "Native listing audit " + uuid.uuid4().hex[:8],
            "access_key_id": "listing-fixture", "secret_access_key": "generated-" + uuid.uuid4().hex,
            "endpoint_scheme": "http", "endpoint_host": urllib.parse.urlsplit(self.fixture.endpoint).netloc,
            "force_path_style": True}
        self.account = self.ipc("create_minio_account", {"input": account_input})
        self.config = {**account_input, "provider": "minio", "account_id": self.account["id"], "bucket": self.fixture.bucket, "region": "us-east-1"}
        self.ipc("save_minio_bucket_configs", {"accountId": self.account["id"], "buckets": [{"name": self.fixture.bucket, "is_public": False}]})
        self.ipc("set_current_minio_bucket", {"accountId": self.account["id"], "bucketName": self.fixture.bucket})
        self.reload({"kind": "fixture-setup-reload"})
        self.evidence["generated_account_id"] = self.account["id"]
        self.evidence["measurement_runs"][self.run_id] = {key: self.evidence.get(key) for key in (
            "captured_at", "app_id", "binary_path", "binary_sha256", "declared_build_profile", "fixture", "owned_pid", "native_state_after_ui_ready", "generated_account_id")}

    def install_observer(self):
        result = self.js(OBSERVER_JS)
        if not isinstance(result, dict) or not result.get("installed"):
            raise RuntimeError(f"Observer installation did not return success: {str(result)[:400]}")
        self.js("Object.assign(window.__nativeListingAudit, " + json.dumps({"account_id": self.account["id"], "bucket": self.fixture.bucket}) + ") && true")
        self.evidence.setdefault("webview", result["viewport"])

    def collect_startup(self, context):
        self.wait(lambda: self.js("performance.getEntriesByName('r2:shell-ready').length > 0"), "r2:shell-ready initialization proxy", 30)
        value = self.js("""(() => ({
          time_origin: performance.timeOrigin,
          shell_ready_marks: performance.getEntriesByName('r2:shell-ready').map(entry => ({name:entry.name,entryType:entry.entryType,startTime:entry.startTime,duration:entry.duration})),
          navigation_entries: performance.getEntriesByType('navigation').map(entry => entry.toJSON()),
          paint_entries: performance.getEntriesByType('paint').map(entry => ({name:entry.name,entryType:entry.entryType,startTime:entry.startTime,duration:entry.duration})),
          supported_entry_types: typeof PerformanceObserver === 'undefined' ? [] : PerformanceObserver.supportedEntryTypes,
          viewport: {width:innerWidth,height:innerHeight,devicePixelRatio,visibility:document.visibilityState,focused:document.hasFocus()},
        }))()""")
        value["id"] = f"{self.run_id}:{value['time_origin']}"
        value["measurement_run_id"] = self.run_id
        value["context"] = context
        value["shell_ready_proxy_ms"] = value["shell_ready_marks"][0]["startTime"]
        if context["kind"] == "process-initial-document":
            value["host_spawn_to_shell_mark_wall_estimate_ms"] = value["time_origin"] + value["shell_ready_proxy_ms"] - self.evidence["host_process_spawn_epoch_ms"]
        value["paint_timing_available"] = bool(value["paint_entries"])
        value["interpretation"] = "Shell initialization/commit mark relative to document navigation; not a physical paint or proven user-interactivity timestamp"
        self.last_startup_id = value["id"]
        if not any(sample["id"] == value["id"] for sample in self.evidence["startup_samples"]):
            self.evidence["startup_samples"].append(value)
        return value

    def reload(self, context=None):
        old_origin = self.js("performance.timeOrigin")
        self.js("setTimeout(() => location.reload(), 100); true")
        self.wait(lambda: self.js("performance.timeOrigin") != old_origin, "new webview document")
        self.wait(lambda: self.js("Boolean(document.querySelector('.toolbar') && document.querySelector('.file-area'))"), "native shell hydrated")
        self.collect_startup(context or {"kind": "webview-reload"})
        self.ipc("cancel_background_sync")
        self.install_observer()
        self.root()
        self.evidence["native_state_after_ui_ready"] = self.cli("state")

    def activate_owned(self):
        if self.js("document.hasFocus() && document.visibilityState === 'visible'"):
            return
        assert self.app is not None and self.app.poll() is None
        # Public AppKit activation of the verified owned PID; no app capability
        # edits or interaction with any other running application/profile.
        script = f"ObjC.import('AppKit'); $.NSRunningApplication.runningApplicationWithProcessIdentifier({self.app.pid}).activateWithOptions($.NSApplicationActivateIgnoringOtherApps | $.NSApplicationActivateAllWindows)"
        result = subprocess.run(["osascript", "-l", "JavaScript", "-e", script], capture_output=True, text=True, timeout=10)
        self.evidence.setdefault("owned_app_activations", []).append({"method": "AppKit", "pid": self.app.pid, "returncode": result.returncode, "result": result.stdout.strip(), "error": result.stderr.strip()})
        time.sleep(0.1)
        if not self.js("document.hasFocus() && document.visibilityState === 'visible'"):
            # App activation can succeed without making the window key after a
            # WebKit reload. Raise only the verified process's own first window.
            script = f'tell application "System Events"\nset targetProcess to first application process whose unix id is {self.app.pid}\nset frontmost of targetProcess to true\ntell first window of targetProcess to perform action "AXRaise"\nend tell'
            raised = subprocess.run(["osascript", "-e", script], capture_output=True, text=True, timeout=10)
            self.evidence["owned_app_activations"].append({"method": "System Events owned-PID AXRaise", "pid": self.app.pid, "returncode": raised.returncode, "result": raised.stdout.strip(), "error": raised.stderr.strip()})
        self.wait(lambda: self.js("document.hasFocus() && document.visibilityState === 'visible'"), "owned native window foreground", 10)

    def root(self):
        self.activate_owned()
        bucket = json.dumps(self.fixture.bucket)
        self.wait(lambda: self.js(f"Boolean([...document.querySelectorAll('.crumbs button.crumb')].find(el => el.title === {bucket}))"), "fixture root breadcrumb")
        self.js(f"[...document.querySelectorAll('.crumbs button.crumb')].find(el => el.title === {bucket}).click(); true")
        self.js("(() => { const s=document.querySelector('.file-area [data-virtuoso-scroller]'); if(s) s.scrollTop=0; return true; })()")
        self.wait(lambda: self.js("Boolean(document.querySelector('.file-area .fl-row.folder .fl-name-text') || [...document.querySelectorAll('.file-area .fl-name-text')].some(el => el.textContent.startsWith('cold-')))"), "fixture folder rows")

    def navigate(self, trial_id, prefix, total, mode):
        # Root can contain more folder rows than the viewport. Scroll the real
        # Virtuoso scroller before starting the timed click, never inject rows.
        name = json.dumps(prefix.rstrip("/"))
        for attempt in range(30):
            found = self.js(f"[...document.querySelectorAll('.file-area .fl-name-text')].some(el => el.textContent === {name})")
            if found:
                self.js(f"[...document.querySelectorAll('.file-area .fl-name-text')].find(el => el.textContent === {name}).scrollIntoView({{block:'center'}}); true")
                break
            self.js("(() => { const scroller = document.querySelector('.file-area [data-virtuoso-scroller]') || document.querySelector('.file-area [data-testid=virtuoso-scroller]'); if(!scroller) throw new Error('Virtuoso scroller unavailable'); scroller.scrollTop += 450; return true; })()")
            time.sleep(0.025)
        else:
            raise RuntimeError('Fixture folder was not reachable through root virtual scrolling')
        self.fixture.active_trial = trial_id
        self.host_trial_context[trial_id] = {"load_average": list(os.getloadavg()), "host_started_monotonic": time.monotonic()}
        arguments = ",".join(map(json.dumps, [trial_id, prefix, total, mode]))
        # Scrolling and connector roundtrips can outlive a focus change. Recheck
        # visibility atomically with preparing the observation and the real UI
        # click, so a hidden window never becomes a measured cold trial.
        for attempt in range(3):
            self.activate_owned()
            try:
                self.js(f"(() => {{ if(!document.hasFocus() || document.visibilityState !== 'visible') throw new Error('audit-window-not-foreground'); const el=[...document.querySelectorAll('.file-area .fl-name-text')].find(el=>el.textContent==={name}); if(!el) throw new Error('Fixture directory not rendered'); window.__nativeListingAudit.prepare({arguments}); el.closest('.fl-row').click(); return true; }})()")
                return
            except RuntimeError as error:
                if 'audit-window-not-foreground' not in str(error):
                    raise
        raise RuntimeError('Measurement window repeatedly lost foreground before the timed click')

    def snapshot(self):
        return self.js("window.__nativeListingAudit.read()")

    def validated_snapshot(self):
        value = self.snapshot()
        trial = value.get("trial")
        if trial and trial.get("started_at") is not None:
            end = trial.get("first_frame_at") if trial["mode"] == "query-warm" else trial.get("full_network_frame_at")
            hidden = [event for event in trial.get("browser_lifecycle", [])
                if event["at"] >= trial["started_at"] and (end is None or event["at"] <= end) and event["visibility"] != "visible"]
            if hidden or trial["viewport"]["visibility"] != "visible":
                raise MeasurementOccluded("Owned test window was hidden during this sample; latency is not a valid foreground measurement")
        return value

    def finish_trial(self, trial_id, prefix, total, mode, expect_network):
        first = self.wait(lambda: (value if (value := self.validated_snapshot())["trial"]["first_frame_at"] is not None else False), "expected row frame", 30)
        if mode == "gated-control":
            if first["trial"]["final_page_at"] is not None:
                raise AssertionError("First-page control row was not displayed before gated page 2 completed")
            self.fixture.release(prefix)
        if expect_network:
            result = self.wait(lambda: (value if (value := self.validated_snapshot())["trial"]["final_page_at"] is not None and value["trial"]["full_network_frame_at"] is not None and (self.args.smoke or any(row.get("summary", {}).get("complete") for row in value["ipc"] if row["command"] == "list_prefix_stream" and row["prefix"] == prefix)) else False), "complete native listing", 90)
        else:
            result = first
        trial = result["trial"]
        trial["measurement_run_id"] = self.run_id
        trial["startup_sample_id"] = self.last_startup_id
        trial["host_context"] = self.host_trial_context.get(trial_id)
        start = trial["started_at"]
        if start is None or trial["viewport"]["visibility"] != "visible" or not trial["viewport"]["focused"]:
            raise AssertionError("Trial did not start in a visible native webview")
        trial["accumulation_ms"] = sum(row["duration_ms"] for row in trial["accumulations"])
        trial["accumulation_before_first_frame_ms"] = sum(row["duration_ms"] for row in trial["accumulations"] if row["started_at"] < trial["first_frame_at"])
        trial["file_item_array_sort_ms"] = sum(row["duration_ms"] for row in trial["sorts"])
        trial["file_item_sort_before_first_frame_ms"] = sum(row["duration_ms"] for row in trial["sorts"] if row["started_at"] < trial["first_frame_at"])
        trial["sort_instrumentation"] = "Reversible Array.prototype.sort wrapper; only FileItem-shaped arrays belonging to this fixture prefix; preserves comparator/return behavior and excludes Map merging"
        trial["first_dom_ms"] = trial["first_dom_at"] - start
        trial["first_visible_frame_proxy_ms"] = trial["first_frame_at"] - start
        trial["first_page_ms"] = None if trial["first_page_at"] is None else trial["first_page_at"] - start
        trial["final_page_ms"] = None if trial["final_page_at"] is None else trial["final_page_at"] - start
        trial["full_snapshot_dom_ms"] = None if trial["full_snapshot_dom_at"] is None else trial["full_snapshot_dom_at"] - start
        trial["full_network_frame_proxy_ms"] = None if trial["full_network_frame_at"] is None else trial["full_network_frame_at"] - start
        trial["ipc"] = result["ipc"]
        trial["fixture_requests"] = self.fixture.trial_calls(trial_id)
        scoped_requests = [request for request in trial["fixture_requests"] if request["prefix"] == prefix and not request["background"]]
        trial["foreground_request_count"] = len(scoped_requests)
        trial["foreground_connection_count"] = len({request["connection_peer_port"] for request in scoped_requests})
        held = next((request for request in scoped_requests if request.get("second_page_gated")), None)
        trial["fixture_second_page_hold_ms"] = None if not held or "response_started_monotonic" not in held else (held["response_started_monotonic"] - held["received_monotonic"]) * 1000
        for event in trial["pages"]:
            timing = event.get("timing") or {}
            emitted = timing.get("emit_started_unix_ms")
            event["emit_to_js_wall_estimate_ms"] = None if emitted is None else trial["viewport"]["timeOrigin"] + event["arrived_at"] - emitted
        trial["first_page_emit_to_js_estimate_ms"] = trial["pages"][0]["emit_to_js_wall_estimate_ms"] if trial["pages"] else None
        rows = [row for row in result["ipc"] if row["command"] == "get_prefix_cache" and row["prefix"] == prefix]
        trial["native_cache_read_ms"] = rows[0]["duration_ms"] if rows else None
        cache_timing = (rows[0].get("cache") or {}).get("timing") if rows else None
        trial["cache_roundtrip_outside_native_ms"] = rows[0]["duration_ms"] - cache_timing["native_elapsed_ms"] if cache_timing and "native_elapsed_ms" in cache_timing else None
        trial["cache_result"] = rows[0].get("cache") if rows else "unobserved (smoke only)" if self.args.smoke else "query-memory-hit"
        if expect_network:
            assert len(trial["pages"]) == total // 1000, f"Expected {total//1000} pages, saw {len(trial['pages'])}"
            assert [page["page_index"] for page in trial["pages"]] == list(range(total // 1000))
            assert sum(page["files"] for page in trial["pages"]) == total
            assert trial["pages"][-1]["complete"] and all(not page["complete"] for page in trial["pages"][:-1])
            summary_rows = [row for row in result["ipc"] if row["command"] == "list_prefix_stream" and row.get("summary")]
            if summary_rows:
                assert summary_rows[0]["summary"]["total_items"] == total
                trial["native_timing"] = summary_rows[0]["summary"].get("timing")
                trial["stream_roundtrip_outside_native_ms"] = summary_rows[0]["duration_ms"] - trial["native_timing"]["native_elapsed_ms"] if trial["native_timing"] else None
                trial["full_completion_ms"] = summary_rows[0]["finished_at"] - start
            else:
                assert self.args.smoke, "Native summary observation missing"
                trial["full_completion_ms"] = None
                trial["full_completion_basis"] = "SMOKE ONLY: sequential pages plus full UI item count; native invoke completion unobserved"
        else:
            assert not trial["pages"], "Memory warm navigation unexpectedly started a new listing"
            assert not rows, "Memory warm navigation unexpectedly read the native cache"
            trial["full_completion_ms"] = None
            trial["full_completion_basis"] = "Already-complete React Query snapshot from preceding verified full listing"
        if mode in ("cold", "gated-control") and (rows or not self.args.smoke):
            assert trial["cache_result"] is None, "Cold trial accidentally had a native cache snapshot"
        if mode == "sqlite-warm" and (rows or not self.args.smoke):
            assert trial["cache_result"]["complete"] and trial["cache_result"]["files"] == total
        if mode == "warm-prime":
            self.evidence.setdefault("priming_samples", []).append(trial)
        elif mode == "gated-control":
            self.evidence["gated_controls"].append(trial)
        else:
            self.evidence["samples"].append(trial)
        self.save()
        print(f"{trial_id}: row/frame={trial['first_visible_frame_proxy_ms']:.1f}ms first-page={trial['first_page_ms']} complete={trial['full_completion_ms']}", flush=True)

    def ensure_fixture_selection(self):
        current = self.ipc("get_current_config")
        assert current and current["provider"] == "minio" and current["account_id"] == self.account["id"]
        assert current["bucket"] == self.fixture.bucket
        assert current["endpoint_scheme"] == "http" and current["endpoint_host"] == urllib.parse.urlsplit(self.fixture.endpoint).netloc

    def record_interruption(self, error, attempt):
        observation = self.snapshot()
        self.evidence.setdefault("interrupted_attempts", []).append({"reason": str(error), "attempt": attempt,
            "measurement_run_id": self.run_id, "observation": observation})
        trial = observation.get("trial")
        if trial and trial.get("request_id"):
            self.ipc("cancel_prefix_list", {"requestId": trial["request_id"]})
        if trial:
            self.fixture.release(trial["prefix"])
        self.save()
        self.activate_owned()

    def refresh_root_for_alias(self, prefix, total):
        self.fixture.datasets[prefix] = total
        self.root()
        self.js("document.querySelector('button[title=Refresh]').click(); true")
        expected = json.dumps(f"{len(self.fixture.datasets):,} items")
        self.wait(lambda: self.js(f"[...document.querySelectorAll('.statusbar .sb-stat')].some(el => el.textContent.trim() === {expected})"), "new retry directory in actual root listing", 30)
        self.ipc("cancel_background_sync")

    def measured_case(self, trial_id, prefix, total, mode, expect_network=True):
        for attempt in range(5):
            actual_prefix = prefix
            try:
                if mode == "sqlite-warm":
                    self.fixture.active_trial = None
                    self.reload({"kind": "sqlite-warm-reload", "size": total, "trial_id": trial_id, "attempt": attempt + 1})
                else:
                    self.root()
                if mode in ("cold", "gated-control"):
                    if attempt:
                        actual_prefix = prefix.rstrip('/') + f"-retry-{attempt}/"
                        self.refresh_root_for_alias(actual_prefix, total)
                    self.ensure_fixture_selection()
                    self.ipc("clear_file_cache")
                if mode == "gated-control":
                    self.fixture.gates[actual_prefix] = threading.Event()
                self.navigate(trial_id, actual_prefix, total, mode)
                self.finish_trial(trial_id, actual_prefix, total, mode, expect_network)
                return actual_prefix
            except MeasurementOccluded as error:
                self.record_interruption(error, attempt + 1)
        raise MeasurementOccluded(f"Owned window was hidden in five {mode} attempts; valid samples are retained")

    def measure(self):
        completed = {sample["id"] for sample in self.evidence["samples"]}
        for total in (10_000, 100_000):
            needed = any(f"{total}-{mode}-{index:03d}" not in completed
                for mode in ("cold", "query-warm", "sqlite-warm") for index in range(self.args.trials))
            if not needed:
                if not any(case["size"] == total for case in self.evidence["gated_controls"]):
                    self.gated_control(total)
                if not any(case["size"] == total for case in self.evidence["cancellations"]):
                    self.cancel_trial(total)
                continue
            for index in range(self.args.trials):
                trial_id = f"{total}-cold-{index:03d}"
                if trial_id in completed:
                    continue
                prefix = f"cold-{total}-{index:03d}/"
                # No response gate in the primary cold latency distribution.
                self.measured_case(trial_id, prefix, total, "cold")
            last_id = f"{total}-cold-{self.args.trials - 1:03d}"
            last_prefix = next(sample["prefix"] for sample in self.evidence["samples"] if sample["id"] == last_id)
            if not self.js("Boolean(window.__nativeListingAudit.completed_prefixes[" + json.dumps(last_prefix) + "])"):
                self.root()
                prime_id = f"{total}-resume-prime"
                self.measured_case(prime_id, last_prefix, total, "warm-prime")
            for index in range(self.args.trials):
                trial_id = f"{total}-query-warm-{index:03d}"
                if trial_id in completed:
                    continue
                self.root()
                age_expression = "performance.now() - (window.__nativeListingAudit.completed_prefixes[" + json.dumps(last_prefix) + "] ?? -Infinity)"
                if self.js(age_expression) >= 20_000:
                    self.wait(lambda: self.js(age_expression) >= 30_200, "query-cache priming expiry", 15)
                    prime_id = f"{total}-query-prime-{index:03d}"
                    self.navigate(prime_id, last_prefix, total, "warm-prime")
                    self.finish_trial(prime_id, last_prefix, total, "warm-prime", True)
                    self.root()
                self.measured_case(trial_id, last_prefix, total, "query-warm", False)
            for index in range(self.args.trials):
                trial_id = f"{total}-sqlite-warm-{index:03d}"
                if trial_id in completed:
                    continue
                self.measured_case(trial_id, last_prefix, total, "sqlite-warm")
            screenshot = self.args.output.with_name(f"native-listing-{total}.png")
            try:
                capture = self.cli("screenshot", str(screenshot), "--overwrite")
                self.evidence["screenshots"].append({"path": str(screenshot), "capture": capture, "outside_timed_trial": True, "measurement_run_id": self.run_id})
            except Exception as error:
                self.evidence["screenshots"].append({"path": str(screenshot), "error": str(error)})
            if not any(case["size"] == total for case in self.evidence["gated_controls"]):
                self.gated_control(total)
            if not any(case["size"] == total for case in self.evidence["cancellations"]):
                self.cancel_trial(total)
        self.summarize()
        with self.args.binary.open("rb") as executable:
            assert hashlib.file_digest(executable, "sha256").hexdigest() == self.evidence["binary_sha256"], "Frozen executable changed during measurements"
        assert len(self.evidence["samples"]) == self.args.trials * 6
        assert len(self.evidence["gated_controls"]) == 2 and len(self.evidence["cancellations"]) == 2
        self.evidence["navigation_matrix_complete"] = not self.args.smoke and self.args.trials >= 30
        self.evidence["acceptance_complete"] = False
        self.save()

    def gated_control(self, total):
        prefix = f"control-{total}/"
        trial_id = f"{total}-gated-first-page-control"
        self.measured_case(trial_id, prefix, total, "gated-control")

    def cancel_trial(self, total):
        self.root()
        prefix = f"cancel-{total}/"
        self.fixture.gates[prefix] = threading.Event()
        trial_id = f"{total}-navigation-cancel"
        self.navigate(trial_id, prefix, total, "navigation-cancel")
        self.wait(lambda: (value if (value := self.validated_snapshot())["trial"]["first_frame_at"] is not None else False), "cancel scenario first frame")
        cancel_host_at = time.monotonic()
        self.root()
        def cancelled():
            value = self.snapshot()
            acknowledged = any(row["command"] == "cancel_prefix_list" and row.get("finished_at") is not None for row in value["ipc"])
            socket_closed = any(call["prefix"] == prefix and call.get("client_disconnected_monotonic") for call in self.fixture.trial_calls(trial_id))
            return value if acknowledged or (self.args.smoke and socket_closed) else False
        self.wait(cancelled, "native prefix cancellation")
        self.fixture.release(prefix)
        time.sleep(0.2)
        result = self.snapshot()
        calls = self.fixture.trial_calls(trial_id)
        after = [call for call in calls if not call["background"] and call["prefix"] == prefix and call["received_monotonic"] > cancel_host_at]
        self.evidence["cancellations"].append({"size": total, "measurement_run_id": self.run_id, "scope": result["trial"], "ipc": result["ipc"],
            "fixture_requests": calls, "post_navigation_requests": len(after),
            "request_window": "Fixture requests received after harness began root navigation; includes connector dispatch latency, a conservative bound",
            "old_final_page_received": result["trial"]["final_page_at"] is not None,
            "root_still_visible": self.js("document.querySelector('.crumbs button.crumb.current')?.title") == self.fixture.bucket})
        assert self.evidence["cancellations"][-1]["root_still_visible"]
        assert not self.evidence["cancellations"][-1]["old_final_page_received"]
        self.save()

    def summarize(self):
        def quantile(values, fraction):
            values = sorted(values)
            return values[min(len(values)-1, math.ceil(len(values)*fraction)-1)] if values else None
        summaries = []
        for size in (10_000, 100_000):
            for mode in ("cold", "query-warm", "sqlite-warm"):
                samples = [sample for sample in self.evidence["samples"] if sample["size"] == size and sample["mode"] == mode]
                metrics = {}
                for field in ("first_dom_ms", "first_visible_frame_proxy_ms", "first_page_ms", "final_page_ms", "full_completion_ms", "full_snapshot_dom_ms", "full_network_frame_proxy_ms", "native_cache_read_ms", "file_item_array_sort_ms", "file_item_sort_before_first_frame_ms", "accumulation_ms", "accumulation_before_first_frame_ms", "first_page_emit_to_js_estimate_ms", "cache_roundtrip_outside_native_ms", "stream_roundtrip_outside_native_ms", "fixture_second_page_hold_ms"):
                    values = [sample[field] for sample in samples if sample.get(field) is not None]
                    metrics[field] = {"n": len(values), "p50": quantile(values, .5), "p95": quantile(values, .95)}
                for field in ("queue_ms", "network_ms", "backoff_ms", "db_ms", "cache_ms", "native_elapsed_ms", "emit_ms"):
                    values = [sample["native_timing"][field] for sample in samples if sample.get("native_timing") and field in sample["native_timing"]]
                    metrics["native_" + field] = {"n": len(values), "p50": quantile(values, .5), "p95": quantile(values, .95)}
                summaries.append({"size": size, "mode": mode, "trials": len(samples), "metrics": metrics})
        self.evidence["summary"] = summaries
        selected_startups = {sample.get("startup_sample_id") for sample in self.evidence["samples"] if sample["mode"] == "sqlite-warm"}
        startup_summaries = []
        groups = [("process-initial-document", None), ("sqlite-warm-reload", 10_000), ("sqlite-warm-reload", 100_000)]
        for kind, size in groups:
            samples = [entry for entry in self.evidence["startup_samples"] if entry["context"]["kind"] == kind and
                (size is None or entry["context"].get("size") == size) and (kind == "process-initial-document" or entry["id"] in selected_startups)]
            values = [entry["shell_ready_proxy_ms"] for entry in samples]
            startup_summaries.append({"kind": kind, "size": size, "n": len(values),
                "shell_ready_proxy_ms": {"p50": quantile(values, .5), "p95": quantile(values, .95)},
                "paint_entry_samples": sum(bool(entry["paint_entries"]) for entry in samples),
                "navigation_entry_samples": sum(bool(entry["navigation_entries"]) for entry in samples),
                "interpretation": "Document initialization/commit proxy; reload values are not cold native process startup"})
        self.evidence["startup_proxy_summary"] = startup_summaries
        if any(sample.get("native_timing") for sample in self.evidence["samples"]):
            self.evidence["metrics_unavailable"] = [text for text in self.evidence["metrics_unavailable"] if not text.startswith("Rust queue/")]
            if not any(sample.get("accumulations") for sample in self.evidence["samples"]):
                gap = "JS complete accumulation timing absent from this binary; FileItem array sorting is measured by a reversible webview wrapper"
                if gap not in self.evidence["metrics_unavailable"]:
                    self.evidence["metrics_unavailable"].append(gap)

    def stop(self):
        if self.app is not None and self.app.poll() is None:
            try:
                if self.port is not None:
                    self.ipc("cancel_background_sync")
                if self.account:
                    self.ensure_fixture_selection()
                    self.ipc("clear_file_cache")
                    self.ipc("delete_minio_account", {"id": self.account["id"]})
                    self.evidence["generated_account_removed"] = True
            except Exception as error:
                self.evidence["cleanup_warning"] = str(error)
            self.app.terminate()
            try:
                self.app.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.app.kill()
                self.app.wait(timeout=5)
        if self.awake is not None and self.awake.poll() is None:
            self.awake.terminate()
            self.awake.wait(timeout=5)
        if self.run_id in self.evidence.get("measurement_runs", {}):
            self.evidence["measurement_runs"][self.run_id]["generated_account_removed"] = self.evidence.get("generated_account_removed", False)
        self.log.seek(0)
        # Logs may contain generated fixture credentials; preserve no raw app logs.
        self.evidence["native_log_bytes"] = len(self.log.read())
        self.log.close()
        self.save()


def self_test():
    fixture = ListingFixture(30)
    try:
        fixture.start()
        for size in (10_000, 100_000):
            prefix = f"cold-{size}-000/"
            token, count, pages = None, 0, 0
            while True:
                query = {"list-type": "2", "prefix": prefix, "delimiter": "/", "max-keys": "1000"}
                if token:
                    query["continuation-token"] = token
                with urllib.request.urlopen(fixture.endpoint + "/" + fixture.bucket + "?" + urllib.parse.urlencode(query)) as response:
                    import xml.etree.ElementTree as ET
                    root = ET.fromstring(response.read())
                ns = "{http://s3.amazonaws.com/doc/2006-03-01/}"
                count += len(root.findall(ns + "Contents"))
                pages += 1
                token = root.findtext(ns + "NextContinuationToken")
                if root.findtext(ns + "IsTruncated") == "false":
                    assert token is None
                    break
            assert count == size and pages == size // 1000
        try:
            fixture.page("cold-10000-000/", "foreign|1000", "1000")
            raise AssertionError("Foreign continuation token accepted")
        except ValueError:
            pass
        subprocess.run(["node", "--check"], input=OBSERVER_JS, text=True, check=True, capture_output=True)
        print("Fixture actual HTTP pagination (10/100 pages), cursor scope, and injected JavaScript syntax passed; no app launched.")
    finally:
        fixture.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--app-id")
    parser.add_argument("--build-profile", default="unspecified", help="Record the producer's build profile; runtime debug flag reflects debug_assertions only")
    parser.add_argument("--expected-sha256", help="Require the exact frozen executable supplied by the build owner")
    parser.add_argument("--pid-file", type=Path, default=REPO / "src-tauri/target/.connector.json")
    parser.add_argument("--resume-from", type=Path, help="Retain valid samples from the same frozen binary and collect only missing cases")
    parser.add_argument("--output", type=Path, default=REPO / ".omx/artifacts/native-listing-post-sharing-20260913/native-listing.json")
    parser.add_argument("--trials", type=int, default=30)
    parser.add_argument("--smoke", action="store_true", help="Development run; always marks acceptance incomplete")
    parser.add_argument("--self-test", action="store_true", help="Validate disposable HTTP pagination and JS syntax without launching an app")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if not args.binary or not args.app_id or not args.app_id.startswith("com.lifefarmer.r2.audit-"):
        parser.error("An explicitly provided isolated audit binary and audit app identifier are required")
    args.binary = args.binary.resolve(strict=True)
    if args.trials < 30 and not args.smoke:
        parser.error("Acceptance requires at least 30 trials per size/cache case; use --smoke for development only")
    if args.trials < 1:
        parser.error("trials must be positive")
    if args.expected_sha256:
        with args.binary.open("rb") as executable:
            if hashlib.file_digest(executable, "sha256").hexdigest() != args.expected_sha256:
                parser.error("Executable SHA256 differs from the frozen binary supplied by the build owner")
    with args.binary.open("rb") as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as binary:
        if binary.find(args.app_id.encode()) < 0:
            parser.error("Binary does not contain the specified isolated application identifier")
    fixture = ListingFixture(args.trials)
    audit = NativeAudit(args, fixture)
    fixture.start()
    try:
        audit.start()
        audit.measure()
    except BaseException as error:
        audit.evidence["failures"].append({"type": type(error).__name__, "message": str(error)})
        if audit.port is not None:
            try:
                audit.evidence["failure_observation"] = audit.snapshot()
                audit.evidence["failure_ui"] = audit.js("({url:location.href,visibility:document.visibilityState,focused:document.hasFocus(),body:document.body.innerText.slice(-2500),visible_rows:[...document.querySelectorAll('.file-area .fl-name-text')].filter(el=>{const r=el.getBoundingClientRect();return r.width>0&&r.height>0&&r.bottom>0&&r.top<innerHeight}).map(el=>el.textContent).slice(0,40)})")
                audit.evidence["failure_native_state"] = audit.cli("state")
                audit.evidence["failure_fixture_requests"] = fixture.trial_calls(fixture.active_trial)
            except Exception as observation_error:
                audit.evidence["failure_observation_error"] = str(observation_error)
        audit.summarize()
        audit.save()
        raise
    finally:
        audit.stop()
        fixture.stop()


if __name__ == "__main__":
    main()
