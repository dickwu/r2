#!/usr/bin/env python3
"""Preserve native listing traces byte-for-byte, compress canonical provenance, and summarize.

Only explicitly verified, untracked source JSON files may be removed. The complete
raw copies live in a git-ignored directory; referenced canonical traces also get
lossless gzip copies with raw/compressed SHA256 hashes in a small manifest.
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import time

REPO = Path(__file__).resolve().parents[1]
KEEP = {"native-listing-merge-before.json", "native-listing-merge-after.json", "native-listing-query-sharing.json"}


def stream_sha256(stream) -> str:
    # Chunked rather than hashlib.file_digest, which needs Python >= 3.11.
    digest = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1 << 20), b''):
        digest.update(chunk)
    return digest.hexdigest()


def digest(path: Path) -> str:
    with path.open('rb') as stream:
        return stream_sha256(stream)


def untracked(path: Path) -> bool:
    relative = str(path.resolve().relative_to(REPO))
    result = subprocess.run(['git', 'ls-files', '--error-unmatch', '--', relative], cwd=REPO, capture_output=True)
    return result.returncode != 0


def referenced_files(canonical: Path, source_dir: Path) -> set[str]:
    pending = [canonical.name]
    found = set()
    while pending:
        name = pending.pop()
        if name in found:
            continue
        path = source_dir / name
        if path.resolve().parent != source_dir.resolve() or not path.is_file():
            raise ValueError(f'Canonical provenance trace is missing or outside its directory: {name}')
        found.add(name)
        data = json.loads(path.read_bytes())
        for record in data.get('resume_history', []):
            reference = Path(record['artifact'])
            # Historic absolute paths are retained inside the raw document;
            # resolve their basenames only within this explicit source directory.
            if reference.suffix != '.json':
                raise ValueError('Invalid provenance filename')
            pending.append(reference.name)
    return found


def number(value):
    return '—' if value is None else f'{value:.1f}'


def summary(data: dict, canonical_name: str, raw_hash: str) -> str:
    ungated = data.get('measurement_protocol', {}).get('primary_cold') == 'ungated'
    title = '# Native listing after the snapshot-sharing correction' if ungated else '# Native listing baseline'
    build = ('**Build:** ' + data.get('declared_build_profile', 'unspecified') + '. The source revision and exact executable hash define this run; the runtime debug flag alone does not describe dependency optimization.') if ungated else '**Build caveat:** release optimization with debug assertions enabled across the dependency graph to expose the connector. This may add overhead absent from a shipped release. React Query still used its default structural sharing in this baseline.'
    interpretation = ('**Interpretation:** primary cold responses were ungated. Separate gated controls demonstrate first-page rendering while the next HTTP response is withheld; they are excluded from the latency percentiles. The frame proxy is not an OS compositor or physical first-pixel timestamp.') if ungated else '**Interpretation:** cold samples held page 2 until the first matching row had two animation-frame callbacks. They prove controlled first-page delivery; they are not ungated burst-cold latency measurements. Ungated cold behavior exists only as priming diagnostics in this baseline. The frame proxy is not an OS compositor or physical first-pixel timestamp.'
    protocol = data.get('measurement_protocol', {})
    cases = protocol.get('cases') or sorted({sample.get('mode') for sample in data.get('samples', [])})
    trials = protocol.get('trials_per_case', 30)
    budget = data.get('local_fixture_first_page_budget', {})
    lines = [
        title, '',
        f"Captured {data['captured_at']}. This trace contains {len(data['samples'])} valid native navigation samples, {trials} for each size/cache case, across {len(data.get('measurement_runs', {}))} recorded run segments.", '',
        f"Cases: {', '.join(cases)}.", '',
        f"App identifier: `{data['app_id']}`. Executable SHA256: `{data['binary_sha256']}`.", '',
        build, '',
        'All data and credentials were generated for a loopback ListObjectsV2 fixture. The measurements include the real Tauri webview, production SDK, SQLite, IPC, React and Virtuoso. They do not establish real-provider/WAN performance.', '',
        '| Items | Case | First DOM p50 / p95 (ms) | First frame proxy p50 / p95 (ms) | Full native completion p50 / p95 (ms) |',
        '| ---: | --- | ---: | ---: | ---: |',
    ]
    labels = {
        'cold': 'Cold, ungated' if ungated else 'Cold, second page gated',
        'cold-ungated': 'Cold, ungated',
        'cold-gated': 'Cold, second page gated',
        'query-warm': 'React Query memory',
        'sqlite-warm': 'SQLite warm revalidate',
        'fresh-cache': 'Fresh SQLite cache, zero LIST',
        'warm-delay': 'Warm snapshot with delayed refresh',
        'warm-error': 'Warm snapshot with refresh error',
    }
    for item in data['summary']:
        metrics = item['metrics']
        pairs = []
        for key in ('first_dom_ms', 'first_visible_frame_proxy_ms', 'full_completion_ms'):
            metric = metrics[key]
            pairs.append(f"{number(metric['p50'])} / {number(metric['p95'])}")
        lines.append(f"| {item['size']:,} | {labels.get(item['mode'], item['mode'])} | " + ' | '.join(pairs) + ' |')
    lines += ['',
        interpretation, '',
        (f"Declared local first-page budget before NEXT-06 result collection: 100k fresh SQLite zero-LIST p95 <= {budget.get('sqlite_fresh_100k_p95')} ms. Basis: {budget.get('basis')}" if budget else ''), '',
        'Memory hits have no new native listing. Fresh-cache samples assert zero foreground LIST requests while reading SQLite through paged IPC. SQLite-warm/delay/error cases recreate the webview/QueryClient while preserving the database and retain per-page timing, cache/network/db/ipc/merge/react/frame/full-completion observations when emitted by the frozen binary. Percentiles use nearest rank. Raw stage counters, scope IDs, arrays of per-page timings and interrupted observations are retained in the compressed traces.', '',
        'Both 10k and 100k navigation-cancellation checks recorded zero post-navigation fixture requests, no obsolete final page and the root view still visible. Generated accounts and native processes were cleaned up for all run segments. OS-hidden or unfocused preparation/measurement attempts were preserved in provenance rather than included as valid latency samples.', '',
        ('This run uses immutable snapshot handoff with structural sharing disabled for folder queries. Comparisons with the baseline also differ in dependency debug assertions; the separate small `native-listing-query-sharing.json` benchmark isolates only the sharing option headlessly.' if ungated else 'The baseline exposed the remaining 100k SQLite/frame delay. The separate small `native-listing-query-sharing.json` benchmark isolates the cost of default QueryClient deep structural sharing; it is headless evidence, not a native speedup claim.'), '',
        f"Canonical raw JSON SHA256: `{raw_hash}`. [Lossless canonical trace]({canonical_name}.gz) and [SHA256 manifest](manifest.json) preserve exact original bytes and referenced segments. The manifest inventories additional transient raw traces copied to the ignored archive directory.", '',
        ('Overall audit acceptance remains incomplete. Actual process-launch-to-interactive startup, physical display timing and real-provider/platform acceptance are separate requirements.' if ungated else 'Overall audit acceptance remains incomplete. Native launch-to-interactive startup, ungated cold percentiles, physical display timing and real-provider/platform acceptance are separate requirements.'), '',
    ]
    if data.get('startup_proxy_summary'):
        lines += ['## Shell-ready proxy', '', 'The application mark records initialized shell commit relative to each document navigation. Reloads are not cold process launches. Navigation/Paint API availability is recorded rather than replaced by zero timings.', '', '| Context | Items | Samples | Shell-ready p50 / p95 (ms) | Navigation / Paint records |', '| --- | ---: | ---: | ---: | ---: |']
        for entry in data['startup_proxy_summary']:
            value = entry['shell_ready_proxy_ms']
            lines.append(f"| {entry['kind']} | {entry['size'] or '—'} | {entry['n']} | {number(value['p50'])} / {number(value['p95'])} | {entry['navigation_entry_samples']} / {entry['paint_entry_samples']} |")
        lines.append('')
    return '\n'.join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-dir', type=Path, default=REPO / 'docs/engineering/r2-audit')
    parser.add_argument('--raw-dir', type=Path, required=True)
    parser.add_argument('--archive-dir', type=Path, required=True)
    parser.add_argument('--canonical', default='native-listing.json')
    parser.add_argument('--remove-untracked', action='store_true')
    args = parser.parse_args()
    source = args.source_dir.resolve()
    raw_dir = args.raw_dir.resolve()
    archive = args.archive_dir.resolve()
    assert source != archive
    assert source != raw_dir or not args.remove_untracked, 'Cannot delete sources when the preserved raw directory is the source directory'
    assert Path(args.canonical).name == args.canonical, 'Canonical name must be a filename within source-dir'
    assert not (archive / 'manifest.json').exists(), 'Archive manifest already exists; preserve its provenance rather than overwriting it' 
    ignored = subprocess.run(['git', 'check-ignore', '--quiet', str(raw_dir / 'probe.json')], cwd=REPO)
    if ignored.returncode != 0:
        parser.error('The raw copy directory must already be ignored by git')
    canonical = source / args.canonical
    data = json.loads(canonical.read_bytes())
    protocol = data.get('measurement_protocol', {})
    cases = protocol.get('cases') or ['cold', 'query-warm', 'sqlite-warm']
    sizes = sorted({sample['size'] for sample in data.get('samples', [])})
    trials = protocol.get('trials_per_case', 30)
    expected_samples = len(cases) * len(sizes) * trials
    assert data.get('navigation_matrix_complete') and len(data['samples']) == expected_samples
    compressed_names = referenced_files(canonical, source)
    sources = [path for path in sorted(source.glob('native-listing*.json')) if path.name not in KEEP]
    assert compressed_names.issubset({path.name for path in sources})
    if args.remove_untracked:
        assert all(untracked(path) for path in sources), 'Refusing to remove any tracked or staged trace'
    raw_dir.mkdir(parents=True, exist_ok=True)
    archive.mkdir(parents=True, exist_ok=True)
    records = []
    for path in sources:
        raw_hash = digest(path)
        copied = raw_dir / path.name
        if copied.exists():
            assert digest(copied) == raw_hash, f'Different bytes already exist at {copied}'
        else:
            shutil.copy2(path, copied)
        assert digest(copied) == raw_hash
        record = {'original_path': str(path.relative_to(REPO)), 'raw_bytes': path.stat().st_size,
            'raw_sha256': raw_hash, 'ignored_copy': str(copied.relative_to(REPO)), 'untracked_at_archive': untracked(path)}
        if path.name in compressed_names:
            compressed = archive / (path.name + '.gz')
            if not compressed.exists():
                with compressed.open('wb') as target, gzip.GzipFile(filename=path.name, mode='wb', fileobj=target, compresslevel=9, mtime=0) as zipped, path.open('rb') as original:
                    shutil.copyfileobj(original, zipped)
            with gzip.open(compressed, 'rb') as restored:
                restored_hash = stream_sha256(restored)
            assert restored_hash == raw_hash, f'Lossless verification failed for {path.name}'
            record.update({'gzip_file': compressed.name, 'gzip_bytes': compressed.stat().st_size,
                'gzip_sha256': digest(compressed), 'decompressed_sha256': restored_hash})
        records.append(record)
    manifest = {'schema': 1, 'archived_at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
        'policy': 'Raw files were copied without parsing/reformatting. Gzip decompression was SHA256-verified. Only verified untracked source JSONs may be removed.',
        'canonical': args.canonical, 'baseline_binary_sha256': data['binary_sha256'], 'files': records}
    (archive / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    (archive / 'summary.md').write_text(summary(data, args.canonical, digest(canonical)))
    # Verify every manifest record once more before deleting any source.
    for path, record in zip(sources, records):
        assert digest(path) == record['raw_sha256'] == digest(raw_dir / path.name)
        if args.remove_untracked:
            assert untracked(path), f'{path} became tracked during archiving'
    if args.remove_untracked:
        for path in sources:
            path.unlink()
    print(json.dumps({'raw_files_copied': len(records), 'raw_bytes': sum(row['raw_bytes'] for row in records),
        'gzip_files': len(compressed_names), 'gzip_bytes': sum(row.get('gzip_bytes', 0) for row in records),
        'removed_untracked_sources': args.remove_untracked, 'manifest': str(archive / 'manifest.json')}, indent=2))


if __name__ == '__main__':
    main()
