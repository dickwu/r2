# Native listing baseline

Captured 2026-09-13T14:53:07Z. This baseline contains 180 valid native navigation samples, 30 for each size/cache case, across 3 recorded run segments.

App identifier: `com.lifefarmer.r2.audit-listing-20260913`. Executable SHA256: `71c876f9bbf4b7bd1dc84e7ccb2cee5c0d9822508144c86c01a3416c67ae5590`.

**Build caveat:** release optimization with debug assertions enabled across the dependency graph to expose the connector. This may add overhead absent from a shipped release. React Query still used its default structural sharing in this baseline.

All data and credentials were generated for a loopback ListObjectsV2 fixture. The measurements include the real Tauri webview, production SDK, SQLite, IPC, React and Virtuoso. They do not establish real-provider/WAN performance.

|   Items | Case                    | First DOM p50 / p95 (ms) | First frame proxy p50 / p95 (ms) | Full native completion p50 / p95 (ms) |
| ------: | ----------------------- | -----------------------: | -------------------------------: | ------------------------------------: |
|  10,000 | Cold, second page gated |              48.0 / 66.0 |                     87.0 / 107.0 |                         324.0 / 482.0 |
|  10,000 | React Query memory      |              16.0 / 52.0 |                      30.0 / 71.0 |                                 — / — |
|  10,000 | SQLite cache            |              59.0 / 88.0 |                    118.0 / 136.0 |                         257.0 / 283.0 |
| 100,000 | Cold, second page gated |              45.0 / 56.0 |                      81.0 / 99.0 |                       2575.0 / 3134.0 |
| 100,000 | React Query memory      |              20.0 / 23.0 |                      44.0 / 54.0 |                                 — / — |
| 100,000 | SQLite cache            |           406.0 / 1303.0 |                  2526.0 / 2713.0 |                       2507.0 / 2962.0 |

**Interpretation:** cold samples held page 2 until the first matching row had two animation-frame callbacks. They prove controlled first-page delivery; they are not ungated burst-cold latency measurements. Ungated cold behavior exists only as priming diagnostics in this baseline. The frame proxy is not an OS compositor or physical first-pixel timestamp.

Memory hits have no new native listing. SQLite-warm samples recreate the webview/QueryClient while preserving the database; they include native cache transfer and concurrent revalidation. Percentiles use nearest rank. Raw stage counters, scope IDs, arrays of per-page timings and interrupted observations are retained in the compressed traces.

Both 10k and 100k navigation-cancellation checks recorded zero post-navigation fixture requests, no obsolete final page and the root view still visible. Generated accounts and native processes were cleaned up for all run segments. OS-hidden or unfocused preparation/measurement attempts were preserved in provenance rather than included as valid latency samples.

The baseline exposed the remaining 100k SQLite/frame delay. The separate small `native-listing-query-sharing.json` benchmark isolates the cost of default QueryClient deep structural sharing; it is headless evidence, not a native speedup claim.

Canonical raw JSON SHA256: `f1be9c495c8236580f70b2806fb84d7c0b6c32d1d266790a0767a7b4832aad39`. [Lossless canonical trace](native-listing.json.gz) and [SHA256 manifest](manifest.json) preserve exact original bytes and referenced segments. The manifest inventories additional transient raw traces copied to the ignored archive directory.

Overall audit acceptance remains incomplete. Native launch-to-interactive startup, ungated cold percentiles, physical display timing and real-provider/platform acceptance are separate requirements.
