# RFC 0001: Relaxed durability mode (bounded loss, explicit flush)

Status: Draft. Date: 2026-10-18. Nothing here is implemented.

## Summary

Add an opt-in `Durability::RelaxedUnsafe` mode in which `commit` appends the frame to the log without calling `sync_all`, and a new `flush()` makes everything appended so far durable. `Durability::Sync` (today's behavior) stays the default and the only mode for queue, agent, lease and fencing transitions. A relaxed database may lose an unsynced suffix of commits on power loss; it never loses or reorders data that a `flush()` acknowledged.

## Motivation, and an honest question

Today every commit does `write_all` + `sync_all` (`persist_operations`, src/lib.rs). In the SQLite comparison (docs/benchmarks.md) fsync-per-commit inserts are a technical tie with SQLite WAL+FULL, so the fsync is the cost floor, not the engine. Bulk imports, rebuildable caches and benchmarks would benefit from skipping it.

But CedroDB is sold as durable local state for agents. A mode where an acknowledged `commit` can vanish is the opposite of that promise. Position of this RFC: the mode is justified only for data that can be regenerated or re-imported (bulk load then `flush`, derived caches, tests). It is NOT justified for agent checkpoints, jobs or leases, which is why those paths never use it (below). If the maintainers conclude the product should not offer any acknowledged-but-volatile commit, the correct outcome is `Rejected`; that is a legitimate result and the recommended default stance until a concrete user need is documented. The existing `Transaction` batching (many puts, one fsync) already covers much bulk-load need; this RFC must show it does not before being accepted.

## Non-goals

No background durability, no group commit, no async API, no change to the Sync path, no weaker integrity checks (CRC and length-complement stay).

## Design

### Naming and API

`pub enum Durability { Sync, RelaxedUnsafe }`, default `Sync`. The word `Unsafe` is deliberate so the risk shows in every call site and code review. Methods: `Database::flush(&self) -> Result<()>`, `Database::durability(&self) -> Durability`, and `Database::durable_cursor(&self) -> u64` (see cursors).

`Options` today has public fields, derives `Copy`, `Serialize`, `Deserialize` with `deny_unknown_fields`, and is built with struct literals; adding a field is a source-breaking change. Decision: mark `Options` `#[non_exhaustive]` and add builder-style `with_*` methods in the same release as the field; call this out in the changelog. Durability is NOT stored only in `Options`, because it must also be known to recovery (below), so the file is the source of truth.

### Exact semantics

- Sync database: unchanged. A returned `commit` is on stable storage (subject to the existing platform caveats; real power-loss behavior is unverified, see the readiness doc).
- Relaxed database: `commit` returns after `write_all` (OS page cache). `flush()` runs `sync_all`; when it returns Ok, every commit that returned before the call is durable.
- Power loss may lose a suffix of the log, i.e. the commits after the last successful flush (and possibly some earlier unflushed ones are kept). It must never lose a commit that precedes a flush-acknowledged commit while keeping that later one, and must never reorder. Caveat: this depends on the OS/disk honoring `fsync` as the Sync mode already assumes; pages written without fsync may reach disk in any order and with holes, which drives the recovery rule below.
- Process crash (SIGKILL) alone loses nothing, because the page cache survives; the loss window exists only for OS crash / power loss.

### How the mode is known at open

The 16-byte header is `CEDRODB\0 01 00*7` (src/lib.rs HEADER). Relaxed databases set a flag in the reserved bytes (byte 9, bit 0). Consequences: current readers compare the whole header and refuse such a file as "unsupported or invalid database header", which is the desired behavior (an old reader would apply the strict rule and could misreport a lawful relaxed tail as Corruption). The flag is set when a relaxed database is created or converted via an explicit `set_durability` that syncs the header; it is never inferred from the `Options` of the opener. Opening a flagged file with `Durability::Sync` options is allowed and keeps the file's relaxed recovery rule until converted back (conversion back = `flush`, then clear the flag, fsync). Format version stays 1 only if the compatibility fixtures (tests/format_fixtures.rs) for flag-clear files stay byte identical; otherwise a bump is required. Open question 2.

### Recovery

Sync databases keep the strict rule exactly (truncate only a short incomplete tail; a complete invalid frame is `Corruption`, pinned by tests/durability.rs).

Relaxed databases: after an unsynced crash the tail can hold a torn frame, or a later frame whose pages landed while an earlier one did not (a hole). Rule: at the first frame that fails (short header, short payload, bad length complement, bad CRC), scan the rest of the file for any offset at which a valid frame parses (header, complement, CRC and payload bounds all valid). If none does, truncate at the first bad frame and fsync (a lost suffix). If one does, return `Corruption`, because valid data after damage means the damage is not a plain lost suffix (bit rot or tampering) and silent truncation would drop data that was durable. Cost: the scan is linear in the bad tail only. A scan must treat a valid-looking frame inside a payload as possible false positives; with a 32-bit CRC and the complement check the false-positive chance is small but nonzero, so the safe direction (report Corruption) is chosen, and an explicit opt-in repair mode (readiness doc item 8) remains the escape hatch.

Zero-filled tails (common after a crash on some filesystems) have length 0 and complement 0, which fails the complement check; they fall under the same rule and truncate if nothing valid follows.

### Operations that always sync

Queue enqueue/claim/ack/fail/retry, lease acquire/renew/release, agent checkpoint commits and anything that issues or fences a token or revision (src/queue.rs, src/agent.rs, conditional batches) call `commit_operations` with a forced `Sync` regardless of the database mode, which also makes everything appended before them durable (fsync covers the whole file). Reason: if a lease token were issued, lost with an unsynced suffix and issued again after reopen, two workers could hold the same fencing token. Ordering consequence: a forced sync flushes earlier relaxed commits too, which is fine for the guarantee. Plain `Transaction::commit`, `put`, `delete` are the only relaxed paths. Mixed use is documented: relaxed document commits interleaved with synced queue commits have the synced ones durable and the earlier relaxed ones durable as a side effect.

### Other interactions

- `backup()` performs an implicit `flush()` before verifying and copying, so a backup never contains less than the log it claims to represent, and the backup check sees only synced data.
- `Drop`: best-effort `sync_all`, errors ignored (cannot be reported). It is not a guarantee; documented as such. Process abort or `mem::forget` skips it.
- Failed `flush()` poisons the handle (same reason as today: after a failed fsync the kernel may have dropped dirty pages, so retrying and succeeding would be a lie). The caller reopens; recovery then decides what survived.
- `changes()` cursors are ordinal frame indexes. In relaxed mode a cursor handed to a consumer may point past what survives a power loss; after reopen `latest_cursor` can be lower than a cursor the consumer saved, which today is an `InvalidInput` error. Story: `durable_cursor()` returns the frame count as of the last successful flush or synced commit, and consumers that must not outrun durability read `changes` only up to it. Consumers should treat the cursor-exceeds-latest error after a crash as "log lost a suffix, reset from `durable_cursor`", and docs say so. Checkpoint revisions and tokens are safe because those paths sync.
- Bindings: Node `openDatabase({ durability: 'relaxedUnsafe' })` and `db.flush()`; Python `Database(path, durability="relaxed_unsafe")` and `db.flush()`. Strings are long on purpose; no short alias. Defaults unchanged. The vendored core check (scripts/check-vendored-core.py) applies.

### Benchmark labelling

Relaxed numbers go in a separate table titled "relaxed (acknowledged commits may be lost on power loss)", never in the same row set or chart as the Sync, SQLite WAL+FULL or other durability-equivalent columns, never in README headline figures, and always with the flush policy used (flush every N commits or only at end). A comparison against SQLite `synchronous=OFF` is the only fair counterpart.

## Failure and crash matrix

| Event | Sync | Relaxed |
| --- | --- | --- |
| SIGKILL mid-commit | short tail truncated, nothing acked lost | same (page cache survives) |
| Power loss after commit, before flush | n/a (already synced) | suffix after last flush may be lost; reopen truncates it |
| Power loss, pages landed out of order | n/a | hole then valid frame: `Corruption` (conservative) |
| Bit rot in a middle frame | `Corruption` | `Corruption` (valid frames follow) |
| fsync error in `flush` | poisoned | poisoned |

## Test plan

1. Property test: for random commit/flush schedules and a simulated crash that keeps an arbitrary prefix of unsynced bytes plus optional reordered pages, recovery returns either a prefix of the commits that includes every flush-acknowledged commit, or `Corruption`, never a different state.
2. Fault injection needs the file-abstraction seam listed as item 9 in the readiness doc; a `Storage` trait with a `CrashFile` that models a page cache and drops unsynced writes. This RFC depends on that work for its strongest tests; SIGKILL tests (tests/kill_loop.rs) alone prove nothing about this mode.
3. Existing strict-recovery tests must pass unchanged for Sync files; add the mirror cases for relaxed (torn tail truncates, hole-then-valid is Corruption, zero tail truncates).
4. Queue/lease tests: after a simulated crash following a relaxed commit then a lease claim, no token repeats.
5. Fixtures: flag-clear v1 files stay byte-identical; a flagged fixture is refused by the current reader.

## Alternatives considered

- Group commit (coalesce concurrent commits into one fsync): rejected for now. The engine is a single process with one mutex and fail-fast writers, so there is little concurrency to coalesce, and it needs either waiting threads or an async layer, both against the "no threads in core" decision. It keeps full durability, so it is the better idea if contention ever appears; revisit then.
- Background flush thread (fsync every N ms): rejected. It adds a thread and shutdown ordering to the core crate, makes the loss window time-based and hard to test, and failure of a background fsync has no caller to report to, so poisoning becomes asynchronous. Explicit `flush()` keeps the policy with the application.
- Simply batch in `Transaction`: already available and fully durable; the baseline this RFC must beat in a measurement.
- Per-call durability argument: rejected; too easy to mix by accident and breaks the recovery rule being a per-file property.

## Open questions

1. Is any acknowledged-but-volatile commit acceptable for this product (see Motivation)? A `Rejected` outcome is valid.
2. Header flag with version stays 1, or bump to 2?
3. Should conversion between modes exist, or is mode fixed at creation?
4. Does the hole scan need a bound to avoid scanning huge garbage tails?
5. Real measured gain over large `Transaction` batches (unmeasured).

## Rollout

Behind a cargo feature `experimental-relaxed-durability` and documented as experimental in a 0.x release; not in default builds, not in published npm/PyPI binaries until the crash-injection suite passes. No benchmark claims until measured. Stabilization requires a separate decision.
