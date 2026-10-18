# Changelog

All releases remain experimental. Future work is separate in [the roadmap](docs/roadmap.md).

## Unreleased

- Added a unix-only SIGKILL durability test (`tests/kill_loop.rs`, helper `examples/kill_loop_writer.rs`): a child commits one document per transaction and acknowledges each after `commit` returns; the parent kills it at 8 varying points (0 to 40 acks, ~1 s total) and asserts the log reopens and verifies, recovered keys form a contiguous prefix, every acknowledged commit is present, and at most one unacknowledged commit is present. It proves process-kill recovery only, not power loss or I/O-fault behavior. No library change. Fault injection is not added: it needs a file abstraction in the commit path, tracked as an RFC.
- Added `scripts/check-vendored-core.py` (run in CI after the Node and Python builds) that fails when `bindings/node/core` or `bindings/python/core` differ from `src/`, `Cargo.toml`, `Cargo.lock` (Node) or `LICENSE`. Those copies are generated and git-ignored, so the drift was local and invisible; the check runs at build time, not on tracked files. The local copies were regenerated (they lacked the replay and limit changes). Node and Python code is unchanged.
- Reopening a log that exceeds stricter `Options` limits (`max_frame_bytes`, `max_document_bytes`, `max_state_bytes`) now returns `Error::LimitExceeded` instead of `Corruption`; CRC, length-complement and malformed-operation failures remain `Corruption`. Limits must not shrink below what the log was written with; the file is untouched. `verify` and `restore_backup` keep reporting `Corruption` for limit violations. Node and Python already surface the message as a generic error, so no binding change was needed. Format unchanged.
- Restore is now published atomically: the log is copied and synced inside a private sibling directory (`.NAME.PID.N.restoring`) which is renamed to the destination only when complete, then the parent directory is synced. The destination name never holds a partial restore; an existing destination is still refused (checked before the rename, so a concurrent creator of an empty directory at that exact name is a narrow race). A crash can leave a hidden `.restoring` directory that is safe to delete. Format unchanged. Tested for failure cleanup and for no partial state visible at the final name, not under real process kills.
- Backups are now published atomically: written to a private temporary file in the destination directory, synced, linked to the final name without replacing anything, and the directory is synced; the temporary file is removed on failure. A failed backup never leaves a file at the final name, and existing destinations are still refused. Format unchanged. Not yet tested under real process kills.
- Faster log replay: measure each document once when preparing a transaction (it was serialized twice), move documents into the state instead of cloning them while replaying the log or verifying a backup, and read each operation directly instead of through serde's internally tagged enum buffer. Reopening a 90,000-document log is about 32% faster (median 277 ms to 187 ms, alternating A/B on the same log). Behavior, errors and the log format are unchanged; the accepted operation shapes are covered by new tests.
- Added `benchmarks/sqlite-comparison`, a standalone reproducible CedroDB vs SQLite harness with verified workloads, equivalent durability settings, per-engine processes, repeated runs and published results. It shows no write advantage, faster in-memory reads and aggregates, and slower reopening and higher memory use; see `docs/benchmarks.md`.

## 0.8.0 — 2026-10-16

- Added the `cedrodb` console command to the Python package, sharing its implementation with `python -m cedrodb`.
- Added document operations, atomic batches, paginated queries, text/vector/hybrid search, profiles, collection inventory, stats, diagnostics, verification, backups, restoration and changefeed commands.
- Added strict bounded JSON input via argument, file or stdin, compact JSON output/errors, explicit database initialization and stable exit codes distinguishing absent records from stored null.
- Split CLI parsing, JSON input, database operations and isolated performance tools into dedicated modules. Added native subprocess tests and console-entry-point packaging checks.

## 0.7.0 — 2026-10-15

- Added owner-handle diagnostics with content limits, collection/index inventory, handle health and bounded session counters for queries, searches and nonempty shared commit attempts.
- Added query/search execution profiles with the ordinary result page, native monotonic timing and actual collection/candidate/scanned/returned counts from the same execution.
- Added typed Node/Python APIs and executable monitoring/index-comparison examples. Metrics reset on reopen and do not alter the log or backup format.
- Added `python -m cedrodb doctor` and an isolated deterministic benchmark with correctness checks, warmup, native/SDK timing samples, batch writes, index construction and reopening.
- Improved installation instructions with OS/architecture requirements, virtual environments, verification commands and explicit unsupported native runtimes.
- Profiles execute real work. Diagnostic counters are not complete application tracing, process RSS, external model latency or a production SLA.

## 0.6.0 — 2026-10-15

- Added native agent checkpoint stores with expected-revision fencing, scoped thread IDs and atomic document/outbox writes.
- Added text/BM25, exact vector/cosine and hybrid/RRF retrieval with metadata prefilters, equality-index selection, deterministic ties and explicit scan budgets.
- Added typed Node/Python APIs, restart/conflict/atomicity/ranking/budget tests, and model-independent agent memory examples.
- Enabled exact floating-point parsing to preserve stored numeric values and vector-search behavior across reopening and backup restoration.
- Reserved `_cedro.agents.*` against generic writes and queue acknowledgements; migrate conflicting pre-existing collections before upgrading.
- Documented AI ecosystem research and application boundaries. This is not a LangGraph saver, model runtime, authorization system or ANN engine.
- Preserved the existing transaction log and backup formats.

## 0.5.0 — 2026-10-15

- Added persistent local queues with stable job IDs, delayed availability, priorities and deterministic claim ordering.
- Added durable leases, stale-token rejection, lease extension, bounded attempts, retry delays, expired-job recovery and dead-job redrive.
- Added acknowledgements that atomically finish a job and write business documents/outbox events.
- Added shared safe-number validation for job payloads, strict internal job validation and reserved queue mutation namespaces.
- Added typed SDK APIs, executable worker examples and restart/fencing/atomicity/capacity tests.
- Kept log/backup formats unchanged; queues scan in O(n), retain completed/dead jobs and need application-managed workers/capacity.

## 0.4.0 — 2026-10-15

- Added atomic conditional batches across documents/collections, with absent/equals guards and conflict details.
- Added guarded inventory/order/outbox examples, concurrent-contender tests and null/missing/numeric edge cases.
- Added native Node/Python methods and typed results. Mismatches do not write frames or advance cursors.
- Preserved the transaction log format and all 0.3 maintenance APIs.

## 0.3.0 — 2026-10-15

- Added full log verification without silent repair.
- Added versioned backup containers with expected length and CRC32, preserving complete transaction history/cursors.
- Added validated restoration into fresh directories without overwriting existing data.
- Added native Rust/Node/Python maintenance APIs, typed reports, executable offline examples and failure tests.
- Expanded application recipes, recovery procedures, migration notes and evidence gates for 1.0.
- Kept the 0.2 transaction log format. Maintenance blocks the handle and replay needs additional RAM.

## 0.2.0 — 2026-10-15

- Native Node.js/TypeScript and Python SDKs with four-platform builds and source fallbacks.
- JSON Pointer filters, keyset pages and session-local equality indexes.
- Atomic compare-and-exchange, durable changefeed, operational stats and strict numeric interoperability.

## 0.1.0

- Initial Rust engine: JSON documents, atomic log frames, file locking, tail recovery and aggregates.
