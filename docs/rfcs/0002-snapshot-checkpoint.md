# RFC 0002: Snapshot / checkpoint file for faster reopen

Status: Draft. Date: 2026-10-18. Nothing here is implemented, and the expected gain is an UNMEASURED hypothesis.

## Summary

Optionally write a side file `cedro.snapshot` holding the materialized state as of a known log position, so `open` can load it, verify it against the log, and replay only the frames after it. The log stays the single source of truth; the snapshot is a disposable cache that is never trusted over the log.

## Motivation

Reopen replays the whole log. Current measurement (parallel replay, 100,000 documents, Apple M2, median of 3): 101.7 ms, versus 0.4 ms for SQLite (docs/benchmarks.md). Cost grows with log length, and the log is never compacted (L9 in the readiness doc), so an agent that overwrites the same keys for months replays every historical frame at each open.

## Non-goals

No compaction (the snapshot does not shrink the log), no change to cursors, no replication, no persisted indexes (indexes remain handle-local and derived), no speedup claim for logs that are small.

## Design

### File and format

Path: `<db>/cedro.snapshot`. Layout (little-endian):

| Field | Bytes | Note |
| --- | --- | --- |
| magic `CEDROSNP` | 8 | |
| snapshot format version | 4 | starts at 1; unknown version means ignore |
| log header copy | 16 | must equal the log's header |
| covered_log_len | 8 | byte offset in `cedro.log` just after the last covered frame |
| covered_frames | 8 | frame index (count), equals `frame_offsets.len()` at that point |
| prefix_crc | 4+ | CRC (or a stronger hash, open question) over log bytes `[0, covered_log_len)` |
| state_len | 8 | |
| state payload | n | documents and collections in a stable order (the same record JSON the log uses) |
| frame_offsets | 8 * covered_frames | needed for `changes()` and cursors |
| trailer CRC | 4 | over everything above |

`frame_offsets` is stored because `changes()` (src/advanced.rs) and every cursor read `inner.frame_offsets`; without it a snapshot reopen could not serve cursors.

### Atomic write sequence

1. Under the database lock (no writer active), note `covered_log_len` and `covered_frames`.
2. Write `cedro.snapshot.tmp.<pid>` in the same directory, `sync_all` the file.
3. `rename` over `cedro.snapshot` (atomic replace is fine here because the snapshot is a cache; contrast backups, which refuse to overwrite).
4. `fsync` the directory.
5. On failure remove the temp file. A crash leaves at most a stale temp file, safe to delete, and either the old or the new snapshot at the final name.

### Never trusted over the log

On open: read the snapshot; if anything is wrong (magic, version, trailer CRC, header mismatch, `covered_log_len` greater than the log length, frame count inconsistent) ignore it and do a full replay. If it parses, verify it against the log prefix: recompute the checksum of log bytes `[0, covered_log_len)` and compare with `prefix_crc`, and check that a frame boundary exists at `covered_log_len` (the stored last offset plus its frame length). Reading the prefix for the checksum is a sequential read and hash of the file, which is cheaper than parse and JSON decode but not free; this verification cost is part of the measurement. Then replay frames from `covered_log_len` with the existing strict recovery rule (or the relaxed one of RFC 0001). Any error in the verified path discards the snapshot and falls back to full replay; a snapshot can only make open faster, never change what is recovered. A debug/test assertion mode replays both ways and compares state.

A weaker alternative (verify only the last covered frame header and CRC) is faster but trusts the prefix; rejected by default because a log edited or restored underneath would pass. Open question 1.

### Old versions

Old CedroDB does not know `cedro.snapshot` and ignores it; the log is unchanged and fully valid, so downgrade works. A newer snapshot version read by older newer code is ignored (full replay). No log format change, so tests/format_fixtures.rs stays valid.

### Interaction with other features

- `backup()`: backs up only the log (raw verified copy). Snapshots are not included; the restored database has none and builds one on demand. Restore never copies a snapshot, so a stale one cannot be attached to a different log (and the prefix checksum would reject it anyway).
- `verify` always checks the log and ignores the snapshot, so verification never depends on the cache. A separate `verify_snapshot` may compare the two.
- `changes()` and cursors: unchanged in meaning, because covered_frames and frame_offsets are restored exactly. Cursors remain ordinal.
- `frame_offsets` memory: still 8 bytes per frame in RAM; a snapshot does not reduce that (see compaction).
- Future compaction (L9): once the log can be truncated or rebased, a snapshot at the base becomes the natural base record, so this format should be designed so `covered_*` can later mean "relative to a base cursor". A compaction RFC must define base cursor and `CursorCompacted`; this RFC deliberately does not, and snapshots bring no retention benefit alone.
- Restore-and-diverge: two logs with equal prefix checksum are the same prefix, so no history identity is needed for correctness here.

### When snapshots are written

Explicit only in the first version: `Database::write_snapshot()` (and CLI/binding equivalents). No automatic snapshot, because it adds latency spikes, a hidden disk write, a disk-space surprise, and an RSS peak while serializing a large state. Possible later policy (every N frames or bytes, or on clean `close`) is an open question and would be opt-in.

### Crash matrix

| Crash point | Result |
| --- | --- |
| during temp write | stale temp file; old snapshot still valid |
| after temp sync, before rename | same |
| after rename, before directory fsync | old or new snapshot; both verify against the log |
| log truncated by recovery below covered_log_len (torn tail) | snapshot ignored (covered_log_len > log length), full replay |
| log gains frames after snapshot | tail replay |
| snapshot bit rot | trailer CRC fails, full replay |
| snapshot from a different or restored log | prefix checksum mismatch, full replay |

### Cost

Disk: roughly the size of the state payload plus 8 bytes per frame, up to about the compacted size of the live data, which may be much smaller or similar to the log depending on overwrite rate. Memory: serializing needs a buffer unless streamed; stream to the file to avoid doubling state RSS (the 100k-doc run is already 140.9 MiB RSS versus SQLite 13.6 MiB). Write time is proportional to state size and holds the lock.

## Expected gain: UNMEASURED hypothesis

Hypothesis: loading a snapshot (read plus decode of documents in one pass, with no per-frame CRC/parse/plan and no replay of overwritten history) plus a prefix checksum could beat 101.7 ms for logs with a high overwrite ratio, and may be roughly equal or WORSE for an append-only log of 100k unique documents, where the state payload is about as large as the log and JSON decoding dominates either way. No number is claimed. Measurement plan, per README of benchmarks: same harness (`benchmarks/sqlite-comparison --reopen`), same Apple M2, hot and cold cache, 9 alternating A/B runs, medians, peak RSS recorded, at 10k and 100k documents, and with overwrite ratios 0%, 90%, 99%; report unfavorable cases. Gate: ship only if open improves materially in the overwrite-heavy cases without more than a small RSS increase; otherwise Reject.

## Alternatives considered

- Compact in-memory representation (interned keys, store documents as raw bytes or a binary encoding, decode lazily): attacks both reopen time and the roughly 10x RSS gap, with no new file. Larger code change across query/search. Likely the better long-term investment and may make the snapshot unnecessary; evaluate alongside.
- mmap of a binary snapshot with lazy parse: fastest possible open but needs unsafe or a dependency, and conflicts with "no unsafe, no new dependencies in core"; rejected for now.
- Compaction / log rewrite: solves growth and replay together but changes cursor semantics and tokens; needs its own RFC (readiness doc section 12, item 1). A snapshot is the cheaper step that does not touch cursors.
- More parallel replay: already done (101.7 ms from 175.8 ms); a single huge frame cannot be parallelized.

## Open questions

1. Prefix verification: full CRC32 of the prefix (cost) versus a stronger hash versus last-frame-only.
2. Should `close` write a snapshot opt-in?
3. Include indexes definitions? (No: they are not persisted today.)
4. Does the snapshot payload reuse the log's JSON, or a binary encoding (ties to the compact representation alternative)?
5. Interaction with RFC 0001: snapshot must only cover synced bytes; in a relaxed database `write_snapshot` flushes first.

## Rollout

Explicit API behind an experimental cargo feature in a 0.x release; fallback to full replay is always on; the replay-both-ways comparison test runs in CI. No change to published benchmark numbers until the measurement above exists.
