# Changelog

All releases remain experimental. Future work is separate in [the roadmap](docs/roadmap.md).

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
