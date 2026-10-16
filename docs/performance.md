# Inspect and profile local operations

CedroDB exposes handle-local counters and plans for the operations actually
executed. They help explain candidate counts and inspect an application session.
They are not a benchmark result or a production monitoring service.

## Diagnostics

`db.diagnostics()` returns:

| Field | Meaning |
| --- | --- |
| `stats` | Existing durable log/live-state counts from `db.stats()` |
| `limits` | Configured document, frame and state encoded-content limits |
| `collections` | Sorted collection names, record/content counts and equality-index fields |
| `session` | Query, search and shared-commit counters for this open handle |
| `poisoned` | Whether an uncertain I/O outcome has disabled the handle |
| `writer_active` | Whether an uncommitted write transaction is active |

Diagnostics never include document values, query text, embeddings or query
arguments. Collection and field names are included. `content_bytes` has the same
encoded-content accounting as database statistics, including collection/key
bytes per record. It is not an RSS measurement. Empty collections that have a
handle-local index are listed with zero records; `stats.collections` continues
to count only nonempty live collections.

The snapshot takes the database mutex while reading live counts and index
definitions; computing per-collection totals traverses live records. It does not
replay history or read document contents from disk. A handle poisoned by an I/O
failure can still report diagnostics if metadata is readable. A mutex poisoned
by a Rust panic returns an error. I/O poisoning means the durable commit outcome
is uncertain: close and reopen before making decisions about persisted state.

## Query and search profiles

`profile_query(collection, Query)` and `profile_search(collection, SearchQuery)`
execute the real operation once and return `{page, elapsed_micros, plan}`. The
page has exactly the ordinary query/search result schema. The plan is captured
under the same database lock as that page:

| Plan field | Meaning |
| --- | --- |
| `collection_records` | All live records in the collection before a key cursor |
| `candidate_records` | Selected candidates before metadata filtering, after query `after` |
| `scanned` | Candidates inspected by the operation; query includes lookahead |
| `returned` | Number of records/hits in this page |
| `used_index` | Actual selected equality-index field, or null |

Ordinary queries, ordinary searches and both profilers share equality-index
selection. A range, inequality or vector predicate does not use an equality
index merely because an index exists on that field. There are no hypothetical
index recommendations or approximate search plans. Search retains its scan
budget, prefiltering, scoring and deterministic tie behavior.

For a query with `after`, counting all remaining BTree candidates requires an
additional traversal. Only profiling requests this count; ordinary limited
queries keep their previous traversal behavior. This extra counting is included
in profile timing but not `scanned`, which counts record evaluation. Profiles
can therefore cost more than ordinary calls. A profile is a result from one
execution, not an explanation of a previously returned page or a stored snapshot.

```rust
use cedrodb::{Database, Query};

let db = Database::open("application.cedro")?;
let profile = db.profile_query("events", Query::default())?;
println!("{} candidates, {} scanned, {} returned in {} us",
    profile.plan.candidate_records, profile.plan.scanned,
    profile.plan.returned, profile.elapsed_micros);
```

## Session counters and timing boundaries

Each of `session.query`, `session.search` and `session.commit` contains
`attempts`, `successes`, `failures`, `total_elapsed_micros` and
`max_elapsed_micros`. Counters use a fixed number of saturating u64 atomics. They
do not retain requests, samples, histograms or document data, and reset when a
database is opened again. Backups preserve database history, not these counters.

- Query/search count each native core invocation, including input-validation,
  scan-budget and poisoned-handle errors. A profiled call counts once. Invalid
  input rejected by a language wrapper before calling the core is outside these
  counters. `get`, `scan`, `aggregate`, `changes` and diagnostics are not tracked.
- Commit counts a **nonempty attempt entering the shared commit path**. Ordinary
  transactions, successful CAS/conditional batches, checkpoints and queue
  transitions all use this path. A success means write and `sync_all` returned
  successfully and live state was published. Failures in this path include
  state/frame validation and I/O errors; an I/O error can have an uncertain
  durable outcome despite being counted as a failed call.
- Rejected staging input, revision/condition mismatches, duplicate enqueue,
  inactive/stale lease operations, writer-busy errors and empty/no-op batches
  that do not enter that path are not commit attempts. These are not counters
  of all public mutation requests.

Elapsed time uses Rust's monotonic [`Instant`](https://doc.rust-lang.org/std/time/struct.Instant.html),
converted to whole microseconds with saturation. Very fast operations can report
zero. Clock behavior during suspension depends on the platform; this is elapsed
time, not a wall-clock timestamp or CPU time.

Query/search timings start before core validation and include mutex waiting,
planning, scoring, page cloning and requested profile metadata. Commit timing
starts after its caller acquired the mutex and excludes that wait and earlier
public API/staging validation; it includes shared validation, serialization,
write, sync and state publication. SDK JSON conversion, network transport,
application processing and external model/tool calls are excluded.

Atomic updates and clock reads add overhead even to ordinary tracked calls.
Counter fields are sampled independently: concurrent completions can race a
diagnostics call, and attempts can include work still running. The session is
not a transactionally simultaneous snapshot of all metrics. When collecting a
quiescent session, totals can support an average; they cannot recover a median,
P95 or P99. Export application measurements if you need distributions, process
resources, traces or cross-session monitoring. Do not infer hardware durability
or application throughput from these counters alone.

## Node.js and Python

Node exposes `db.diagnostics()`, `collection.profileQuery(options)` and
`collection.profileSearch(options)`, with camelCase report fields. Python
exposes `db.diagnostics()`, `collection.profile_query(filters, limit=..., after=...)`
and `collection.profile_search(...)`, with snake_case fields. Profiles include
ordinary document values even though diagnostics do not; choose what you log.

Use the existing application owner handle when collecting snapshots. Another
process cannot open a database concurrently just to monitor it. The
[Python example](../bindings/python/examples/performance_monitor.py) shows
application-managed periodic logging. The [Node inspection example](../bindings/node/examples/diagnostics.mjs)
opens an existing directory using ordinary recovery rules and reports the new
handle; its session counters do not describe an earlier application session.

Python also includes `python -m cedrodb doctor` and
`python -m cedrodb benchmark --records 1000 --iterations 30`. The benchmark
uses its own temporary data, checks equality of results before/after indexing,
and reports empirical latency summaries from actual measurements. See
[the workload and comparison methodology](benchmarks.md).
