# Changelog

All releases remain experimental. Future work is separate in [the roadmap](docs/roadmap.md).

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
