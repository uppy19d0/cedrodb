# CedroDB for Python

<img src="https://raw.githubusercontent.com/uppy19d0/cedrodb/main/docs/assets/cedrodb-logo.png" alt="CedroDB logo" width="128" />


**Keep agent checkpoints, searchable memory and local jobs in one database.**
CedroDB is an embedded document engine written in Rust, with a native Python API.
Save agent state and its result/outbox writes atomically with expected-revision
checks. Retrieve memory using text/BM25, supplied vectors/cosine or hybrid/RRF,
with metadata prefilters and explicit scan budgets. No model service is required.
A form submission and its outbox event can commit together, indexed queries find
pending work, and a durable changefeed lets your application resume processing
after restarting. Durable queues schedule local work with leases, retries and
dead-job recovery. Conditional batches reserve resources and append events only
when all expected documents still match. Integrity verification and portable
backups preserve the recovery history.

Use it for offline upload queues, report generation, file-processing tools,
desktop application state, or small services that need JSON documents without running a database
server. The Python package calls the Rust engine directly; it does not spawn a
process or maintain a second Python storage implementation.

**Experimental software.** Active documents and indexes live in RAM, a directory
has one owning handle/process, and the log currently has no compaction. See the
limits below before choosing it for important data. There are no performance
claims or production-readiness guarantees.

## Install on your operating system

Use CPython 3.9+ in a virtual environment. These commands use the environment's
interpreter directly, so activation and shell execution-policy changes are unnecessary.

**macOS / Linux**

```sh
python3 -m venv .venv
.venv/bin/python -m pip install cedrodb==0.8.0
.venv/bin/python -m cedrodb doctor
```

**Windows PowerShell** (64-bit CPython)

```powershell
py -3 -m venv .venv
.\.venv\Scripts\python.exe -m pip install cedrodb==0.8.0
.\.venv\Scripts\python.exe -m cedrodb doctor
```

With an environment already selected, the installation command is simply:

```sh
python -m pip install cedrodb==0.8.0
```

`doctor` prints JSON containing the actual interpreter, architecture and package
version after loading the native extension. It opens no database. If your command
is `python3` or an environment-specific path, use that same command consistently.

| System / CPU | Native wheel | Requirement |
| --- | --- | --- |
| Linux x86_64 | `manylinux_2_34_x86_64` | glibc 2.34+; standard CPython 3.9+ ABI |
| macOS Apple Silicon | `macosx_11_0_arm64` | macOS 11+; ARM64 CPython |
| macOS Intel | `macosx_10_12_x86_64` | Wheel tag 10.12+; compatible CPython runtime |
| Windows x86_64 | `win_amd64` | 64-bit Windows CPython |
| Linux ARM64, Alpine/musl, other CPUs | No included wheel | Source build may be possible; not part of the tested binary matrix |
| Browser, iOS, Android | No supported native package | Use a compatible server/desktop runtime |

The interpreter's architecture must match the wheel; an ARM64 machine running
x86_64 Python uses an x86_64 wheel. Free-threaded Python, PyPy and mobile Python
are outside the tested ABI matrix. A compatible wheel installs without Rust.
To require a binary and fail clearly if none matches:

```sh
python -m pip install --only-binary=:all: cedrodb==0.8.0
```

A source build needs Python 3.9+, Rust 1.85+, Cargo and a platform C linker
(Xcode command-line tools on macOS, a C toolchain on Linux, or compatible MSVC
build tools on Windows). Source-build availability is not a support guarantee.
The self-contained source archive bundles the Rust core:

```sh
python -m pip install ./cedrodb-0.8.0.tar.gz
```

Check [GitHub Releases](https://github.com/uppy19d0/cedrodb/releases) for actual
assets and checksums. A missing package version, unavailable wheel, missing
compiler and a database-lock error are different problems; changing database
files does not fix installation. Keep the same Python interpreter for pip and execution.

## Inspect performance and monitor a live application

```python
from cedrodb import Database

with Database('./app.cedro') as db:
    memory = db.collection('memory')
    memory.put_many({
        'a': {'tenant': 'shop-1', 'text': 'refund policy'},
        'b': {'tenant': 'shop-2', 'text': 'shipping policy'},
    })
    filters = [{'field': 'tenant', 'op': 'eq', 'value': 'shop-1'}]
    before = memory.profile_query(filters)
    memory.create_index('tenant')
    after = memory.profile_query(filters)
    assert before['page']['records'] == after['page']['records']
    print(before['plan'], after['plan'])
    retrieval = memory.profile_search(text='refund', filters=filters)
    print(retrieval['elapsed_micros'], retrieval['plan'])
    print(db.diagnostics())
```

`profile_query(filters=None, *, limit=100, after=None)` and
`profile_search(...)` execute once, returning `{page, elapsed_micros, plan}`.
Search accepts exactly the same options as `search()`. The plan contains
`collection_records`, `candidate_records`, `scanned`, `returned`, `used_index`.
A keyset query may stop early, so candidate count and scanned count can differ.
The returned `page` contains normal documents; redact it before exporting logs.

`db.diagnostics()` returns `{stats, limits, collections, session, poisoned,
writer_active}` without document values. Collection entries contain `collection`,
`records`, `content_bytes`, `index_fields`. `session.query`, `.search`, `.commit`
contain `attempts`, `successes`, `failures`, `total_elapsed_micros`,
`max_elapsed_micros`. Encoded content bytes are not process RSS. Snapshots may
race concurrent metric completions; there is no global wall-clock snapshot.

Query/search timers include native validation and waiting for the engine mutex;
commit timers start in the nonempty shared commit path and exclude waiting for
that mutex. Python/JSON transport and external model calls are excluded. Calls
rejected by Python validation never enter native counters. Commit metrics exclude
conflicts, empty commits, and write errors rejected before the shared commit path.
Metrics are handle-local, fixed-size and reset on reopening; no histogram or
percentile is inferred from their totals.

Run a fresh temporary workload with correctness checks and measured timings:

```sh
python -m cedrodb benchmark --records 1000 --iterations 30
python -m cedrodb benchmark --records 10000 --iterations 100 > benchmark.json
```

The JSON report includes environment/workload settings, native and Python-call
latency samples summarized with empirical nearest-rank p50/p95/p99, scan/index
plans, batch writes, index build and reopen time. Small samples cannot establish
tail-latency guarantees. Vectors are synthetic and no retrieval-quality claim is
made. OS caches remain uncontrolled; results describe this machine/run. The tool
opens no application database and removes its temporary database on exit.

For periodic JSON snapshots, see
[`examples/performance_monitor.py`](https://github.com/uppy19d0/cedrodb/blob/main/bindings/python/examples/performance_monitor.py).
Sample the application's existing owner handle; a second process cannot open the
same database concurrently. Choose your sampling interval and logging policy.

## Quickstart: save and query documents

```python
from cedrodb import Database

with Database("./app.cedro") as db:
    orders = db.collection("orders")
    orders.put_many({
        "order:1": {"status": "paid", "total": 120},
        "order:2": {"status": "pending", "total": 35},
    })
    orders.create_index("status")
    page = orders.find_page(
        [{"field": "status", "op": "eq", "value": "paid"}], limit=20
    )
    print(page["records"])       # [{"key": "order:1", "value": {...}}]
    print(page["used_index"])    # status
    print(orders.get("order:1"))
    print(db.stats())
```

Collections store any supported JSON value, including arrays and null. `put`
replaces the complete value for a key; `delete` removes it. `put_many` accepts a
mapping or an iterable of `(key, value)` pairs and commits them atomically.

`find()` returns one list of `{key, value}` records. `find_page()` also returns
`next_after`, `scanned` and `used_index`. To continue, pass `next_after` as `after`;
`None` means there is no continuation. `iter_records(page_size=100)` iterates
bounded result pages. Results are ordered by key; pages do not share a snapshot.
Writes between requests can change subsequent results. Limits are 1..1000 records.

Filters are combined with AND. Operations are `eq`, `ne`, `gt`, `gte`, `lt`,
`lte`, and `exists` (with a boolean value). A field is a top-level name or an
RFC 6901 JSON Pointer, such as `/customer/city`. Missing fields differ from JSON
null; use `exists` to test absence. Equality indexes stay consistent with writes
and must be recreated after reopening or restoring a database.

## Offline writes and the atomic outbox

Commit the document and the event in the same transaction. Your uploader can
then find the event after a restart without losing the corresponding form.

```python
from cedrodb import Database

with Database("./field-app.cedro") as db:
    with db.transaction() as tx:
        tx.put("forms", "inspection:1", {
            "site": "Santo Domingo", "status": "pending_upload"
        })
        tx.put("outbox", "inspection:1:submitted", {
            "event_id": "inspection:1:submitted",
            "type": "inspection.submitted", "form_id": "inspection:1"
        })

    page = db.changes(since=0, limit=100)
    for transaction in page["transactions"]:
        for operation in transaction["operations"]:
            if operation["collection"] == "outbox" and operation["kind"] == "put":
                print(operation["value"])
    # Save page["next_cursor"] only after processing succeeds.
```

The feed survives close/reopen and contains complete put/delete operations in
transaction order. `since` is an exclusive transaction cursor, starting at zero,
and belongs to this database history. `limit` counts transactions (1..1000), not
operations. `has_more` tells you whether another page is available. Pages also
cap encoded frame bytes at `max_frame_bytes + 12` without splitting a transaction;
decoded Python objects require additional memory.

Your application owns the uploader, checkpoint storage and event deduplication.
A crash after an external effect but before its checkpoint can repeat delivery;
CedroDB does not promise exactly-once delivery or provide a background worker.

`db.transaction()` buffers writes and copies staged values. A successful context
exit commits one batch; an exception rolls it back. Reads continue to observe
committed data, without read-your-writes, snapshot isolation or automatic conflict
detection. Alternatively, pass operations directly to `db.batch([...])`:

```python
with Database("./app.cedro") as db:
    db.batch([
        {"kind": "put", "collection": "orders", "key": "order:3", "value": {"total": 10}},
        {"kind": "delete", "collection": "drafts", "key": "order:3"},
    ])
```

## Run durable local jobs

Create an application-managed worker loop without adding a separate queue
server. Job state survives reopening and is included in database backups.

```python
from cedrodb import Database

with Database("./worker.cedro") as db:
    queue = db.queue("reports")
    enqueued = queue.enqueue(
        "report:1", {"order_id": "order:1"}, max_attempts=3, priority=10
    )
    job = queue.claim("worker-1", lease_ms=30_000)
    if job is not None:
        # Generate the local result while the lease is active.
        completed = queue.ack(job["id"], job["lease_token"], operations=[
            {"kind": "put", "collection": "reports", "key": job["id"],
             "value": {"status": "ready"}},
            {"kind": "put", "collection": "outbox", "key": job["id"] + ":ready",
             "value": {"event_id": job["id"] + ":ready", "type": "report.ready"}},
        ])
        print("Completed:", completed)
    print(queue.stats())
```

`enqueue()` returns `{inserted, job}`. Job IDs are retained after completion or
failure: enqueueing an existing ID returns its existing job, without replacing
its payload or resetting its state. This supports application-level deduplication
while that job is retained. Use a new ID for new work, or `retry_dead()` to reset
the attempts of a dead job explicitly.

`claim()` returns a `Job` or `None`, prioritizing eligible jobs by descending
priority. `delay_ms` controls when a newly queued job becomes available. Each
claim consumes one attempt and returns an opaque `lease_token`. Acknowledgment,
failure and lease extension require the matching token and a lease that has not
expired. A stale or nonmatching token returns `False` without changing the job.

`ack(..., operations=[...])` writes business documents, outbox events and the
job's `done` state in one durable transaction. The operation list uses the same
put/delete dictionaries as `batch()`. Invalid operations leave the job leased
and apply no business writes. Operations cannot write to any `_cedro.jobs.*`
collection; that namespace is reserved for the queue implementation.

```python
with Database("./worker.cedro") as db:
    queue = db.queue("reports")
    job = queue.claim("worker-1", lease_ms=30_000)
    if job is not None:
        queue.fail(
            job["id"], job["lease_token"], "temporary upload failure",
            retry_delay_ms=5_000,
        )
    recovery = queue.recover_expired(limit=10)
    print(recovery)  # requeued, dead, has_more
```

`fail()` schedules a retry or marks the job `dead` when its attempt budget is
exhausted. `claim()` can reclaim an expired lease if another attempt remains.
Run `recover_expired()` periodically to requeue expired work and mark exhausted
leases dead; repeat while `has_more` is true. Recovery processes 1..1000 jobs per
call, additionally bounded by the transaction frame limit. `extend()` renews an
active lease and never shortens its existing deadline. `retry_dead()` resets
attempts to zero and makes a dead job available again.

All queue methods accept an optional `now_ms` where applicable. The default is
the current Unix epoch time in milliseconds; explicit timestamps make testing
deterministic. Times and durations must be integers in `0..MAX_SAFE_INTEGER`
(`2**53 - 1`); leases must be positive, and timestamp-plus-duration must also
stay within that bound. Booleans are rejected as numeric options. Use a
consistent clock across your workers: backward or forward clock jumps affect
lease eligibility.

Queue payload numbers are finite and restricted recursively to
`[-MAX_SAFE_INTEGER, MAX_SAFE_INTEGER]` so Rust, Node and Python can exchange
jobs safely. Ordinary collection documents and Python `ack()` business writes
retain the full `[-2**63, 2**64 - 1]` integer range. Encode larger identifiers or
exact decimal quantities as strings in job payloads.
The job envelope also counts toward document and transaction limits, so the
available payload budget is smaller than `max_document_bytes`.
Allow headroom for lease tokens, worker names, errors and acknowledgment writes:
a queued job fitting the configured limits does not mean every later transition
will fit. Size-limit errors apply no partial transition or business writes. If a
transition reaches capacity, close and reopen the store with higher document,
frame or state limits as needed before retrying it.

Queue names contain 1..128 ASCII letters, digits, `_`, `.` or `-`. Job IDs
contain 1..4096 UTF-8 bytes, workers 1..128 bytes and errors 1..4096 bytes.
IDs, workers, errors and tokens must be nonempty without NUL. `max_attempts` is
1..1000 and priority is 0..1000. `Job` dictionaries include `schema_version`,
`id`, `payload`, `status`, `attempts`, `max_attempts`, `priority`, `created_at`,
`available_at`, `lease_until`, `lease_token`, `worker` and `last_error`.
Statuses are `queued`, `leased`, `done` and `dead`. Lease fields are null when
the job is not leased. `stats()` reports queued/leased/done/dead counts plus
ready and expired counts; ready can include reclaimable expired leases.

The application owns the worker loop, error handling, lease renewal and expired
recovery. Operations are synchronous and scheduling scans jobs in O(n) time.
One process owns the database directory; share its handle across threads.
There is no background worker, retention policy or distributed scheduler.
Completed/dead jobs remain stored and the append-only log continues growing.
Generic CRUD, batch, CAS and conditional write operations targeting
`_cedro.jobs.*` are rejected. Read-only collection inspection and conditions on
that namespace remain available. Use the queue API to change job state.

Lease tokens fence changes within the same database history. Restoring or
copying a backup creates a history branch, where an old token may still exist;
tokens do not fence external systems or independently restored stores. A worker
can perform an external effect and crash before acknowledgment. Consumers must
deduplicate effects by stable job/event ID; delivery is not exactly-once.

Run `python examples/local_worker.py` for a deterministic retry, reopen and
atomic acknowledgment with a business result and outbox event.

## Reserve inventory and publish its event atomically

Use a conditional batch when a decision depends on more than one document. The
engine checks every condition against the same committed state while holding
the write lock, then commits all operations together. This closes the gap
between checking inventory and updating inventory plus its outbox event.

```python
from cedrodb import Database

with Database("./shop.cedro") as db:
    db.collection("inventory").put("desk", {"available": 10})
    result = db.conditional_batch([
        {"kind": "equals", "collection": "inventory", "key": "desk", "value": {"available": 10}},
        {"kind": "absent", "collection": "outbox", "key": "order:1"},
    ], [
        {"kind": "put", "collection": "inventory", "key": "desk", "value": {"available": 9}},
        {"kind": "put", "collection": "outbox", "key": "order:1",
         "value": {"event_id": "order:1", "type": "inventory.reserved"}},
    ])
    print(result)  # {"applied": True, "failed_condition": None, "cursor": 2}
```

Conditions are `absent` or `equals` for a complete JSON value, with at most 64
conditions per call. `absent` forbids a `value` field. `equals` requires `value`;
`value=None` matches stored null and does not match a missing key. Operations
use the same put/delete dictionaries as `batch()`.

On a mismatch, the result has `applied=False`, the zero-based index of the first
failing condition, and the unchanged current `cursor`; no writes or log frame
are produced. On success, `failed_condition` is `None` and `cursor` identifies
the committed transaction. An empty operation list checks conditions without
advancing the cursor. Empty conditions provide an unconditional atomic batch.

The entire request is validated before mutation, even if a condition would
fail. Unsupported JSON, extra fields, invalid locations or limits raise an
exception. A conditional batch provides atomic optimistic checks within one
database handle; it does not provide a distributed lock or prevent repeated
external effects. Retry stale inventory decisions after reading current state,
and deduplicate event deliveries in the consumer.

Run `python examples/inventory_outbox.py` for a successful reservation, a rejected
stale request, and its durable history after reopening.

## Verify, back up and restore

A backup contains the committed log inside a versioned container with a length
and CRC checksum. It preserves transaction history and changefeed cursors.

```python
from pathlib import Path
from cedrodb import Database

backup = Path("./field-app.backup")      # Must not already exist.
restored = Path("./recovered.cedro")      # Must not already exist, even empty.

with Database("./field-app.cedro") as db:
    print(db.verify())
    print(db.backup(backup))

report = Database.restore_backup(backup, restored)
print(report)  # log_bytes, transactions, records, content_bytes

with Database(restored) as db:
    db.collection("forms").create_index("status")
    print(db.changes(since=0))
```

`verify()` reads and validates the committed log; it does not repair data.
`backup(path)` creates a new file and never overwrites an existing file or
folder. `restore_backup(backup_path, destination_path)` validates the complete
container and log before creating a new destination directory. Parent directories
must already exist. Static restore returns an `IntegrityReport` dictionary,
without retaining an open handle or file lock. Index definitions are handle-local
and must be recreated; the committed documents and history are retained.

For custom content limits, pass the same configuration to restore and open:

```python
limits = {"max_document_bytes": 2_097_152, "max_state_bytes": 134_217_728}
Database.restore_backup("./large.backup", "./large-restored.cedro", **limits)
with Database("./large-restored.cedro", **limits) as db:
    print(db.verify())
```

All three methods are synchronous and perform disk I/O; the native operation
releases Python's GIL. Verification replays state in memory and can require
another copy of the live state plus a frame buffer. CRC detects accidental
corruption; it does not authenticate or encrypt a backup. Interrupted I/O can
require inspecting and removing an incomplete output before retrying. See
[backup format and recovery details](https://github.com/uppy19d0/cedrodb/blob/main/docs/backups.md).

Run the complete offline-form example with `python examples/offline_forms.py`.

## Conditional updates and null

```python
from cedrodb import Database, DELETE, MISSING

with Database("./app.cedro") as db:
    claims = db.collection("claims")
    created = claims.compare_exchange(
        "job:1", expected=MISSING, replacement={"owner": "worker-1"}
    )
    removed = claims.compare_exchange(
        "job:1", expected={"owner": "worker-1"}, replacement=DELETE
    )
    missing = claims.get("unknown", MISSING) is MISSING
```

`replacement` is required. `None` stores JSON null; `DELETE` removes the record.
`MISSING` as expected means the key must not exist, while `expected=None` requires
a stored null. A mismatch returns `False` without appending a transaction. The
comparison covers the complete JSON value. Exact integers and integral floats
compare equally only within the safe range ±2**53; booleans remain distinct.

## Persist agent state and its outbox together

`db.agent(namespace)` stores framework-neutral JSON checkpoints. A revision
check prevents a stale worker from replacing newer state. Checkpoint and
business/outbox operations are committed in the same durable transaction.

```python
from cedrodb import Database

with Database("./assistant.cedro") as db:
    agent = db.agent("support")
    current = agent.get("conversation:1")
    result = agent.checkpoint(
        "conversation:1", {"messages": [{"role": "assistant", "text": "Saved."}]},
        expected_revision=None if current is None else current["revision"],
        status="completed", metadata={"owner": "ana", "state_format": "support.v1"},
        operations=[{
            "kind": "put", "collection": "outbox", "key": "conversation:1:answered",
            "value": {"event_id": "conversation:1:answered", "type": "answer.ready"},
        }],
    )
    if not result["applied"]:
        print("Reload before retrying:", result["checkpoint"])
```

`expected_revision=None` means **create only if absent**. It is never an
unconditional overwrite. Pass the revision returned by `get()` when updating.
On conflict, `applied` is false, `cursor` stays unchanged and `checkpoint` is
the current checkpoint (or `None` for an absent thread). No checkpoint, outbox
or other business write is applied. Invalid requests raise before mutation,
including when a revision would conflict.

Success returns `applied=True`, a `cursor` and the new `checkpoint`. Its
`revision` is the durable transaction cursor: it increases with database history
and need not be consecutive for a particular thread. The envelope contains
`schema_version=1`, `thread_id`, `revision`, `state`, `metadata`, `status` and
`updated_at`. Status is `running` (default), `waiting` or `completed`; metadata
defaults to `{}` and must be a JSON object. State may be any supported portable
JSON value, including null. `now_ms` defaults to Unix epoch milliseconds.
Checkpoints survive reopening and are included in backups and the changefeed.

Namespace names contain 1..128 ASCII letters, digits, `_`, `.` or `-`; thread
IDs contain 1..4096 UTF-8 bytes without NUL. State and metadata numbers must be
finite and within ±`MAX_SAFE_INTEGER`, recursively. Revisions and timestamps
use integers in the safe range; booleans are rejected as numeric options.
Optional business operations keep ordinary collection integer limits. The
checkpoint envelope and all operations count toward document/frame/state limits;
an oversized request applies no partial writes.

Generic writes to `_cedro.agents.*` and `_cedro.jobs.*` are rejected. Use their
dedicated APIs. Namespaces organize data; they are not an authorization boundary.
The application owns state schemas, orchestration, tool permissions and external
effect deduplication. A checkpoint records a completed persistence step; it does
not resume a model/tool call or promise exactly-once external delivery. Restoring
a backup creates a history branch; revisions do not fence independently restored
copies or external systems. This API is not a LangGraph `BaseCheckpointSaver`.

## Retrieve local knowledge by text, vector or both

`Collection.search()` ranks committed documents with Unicode alphanumeric
lowercase tokenization and BM25, exact cosine similarity, or reciprocal rank
fusion (RRF) when both queries are provided. Embeddings come from your application;
the SDK does not call a model or a network service.

```python
with Database("./assistant.cedro") as db:
    knowledge = db.collection("knowledge")
    # Synthetic vectors illustrate the API; these are not model embeddings.
    knowledge.put_many({
        "backup": {"owner": "ana", "text": "local backup recovery", "embedding": [1, 0]},
        "jobs": {"owner": "ana", "text": "durable worker retries", "embedding": [0.6, 0.8]},
    })
    knowledge.create_index("owner")
    page = knowledge.search(
        text="backup", vector=[1, 0],
        filters=[{"field": "owner", "op": "eq", "value": "ana"}],
        limit=5, max_scanned=1000,
    )
    for hit in page["hits"]:
        print(hit["key"], hit["score"], hit["text_score"], hit["vector_score"])
```

Use `text` alone, `vector` alone or both. The public signature is:

```python
collection.search(
    *, text=None, vector=None, vector_field="embedding", text_fields=None,
    filters=None, limit=10, max_scanned=10_000, min_vector_score=None,
)
```

`text_fields=None` means `["text"]`; fields can be top-level names or JSON
Pointers such as `/content/body`. Supply 1..8 unique text fields. Text must
contain 1..4096 UTF-8 bytes and 1..64 distinct alphanumeric terms. Vectors contain
1..4096 finite numeric coordinates and must have nonzero magnitude. Present
eligible document vectors must be valid and match the query dimensions; missing
vectors are skipped. An invalid eligible vector raises instead of silently
removing a possible match. `min_vector_score`, when supplied, requires a vector
query and a finite value in `[-1, 1]`.

Metadata filters use the same ANDed predicates as `find_page()` and run before
document vector validation/scoring. Apply the appropriate owner/tenant filter
on every request; the database does not authorize access. An equality index can
reduce the candidate set and remains local to the open handle. Recreate it on
reopen. `max_scanned` (1..100000) counts candidates visited **before** filters;
exceeding it raises, without returning a partial top-k. `limit` is 1..1000.

The result contains `hits`, `scanned`, `matched`, `used_index` and `mode`
(`text`, `vector` or `hybrid`). Hits include the original `key`/`value`, final
`score` and nullable `text_score`/`vector_score`. `matched` counts matches before
the result limit. Text mode uses BM25 (`k1=1.2`, `b=0.75`), vector mode uses
cosine, and hybrid mode fuses complete channel rankings with RRF (`k=60`). Ties
are ordered by key. Scores from different modes are not directly comparable.

This is exact bounded scanning over a small local corpus, without a full-text
inverted index or approximate nearest-neighbor index. All live documents and
ranking buffers use memory; retain the existing database content limits and use
`max_scanned` to bound work. It does not extract facts, generate embeddings,
split documents, manage model versions or provide a RAG pipeline.

Run `python examples/agent_memory.py` for a network-free retrieval,
checkpoint/outbox, stale-write rejection and reopen example.

### Optional PydanticAI conversation recipe

The package has no agent-framework dependency. An optional recipe uses the
published `pydantic-ai-slim==2.56.0` API with Python >=3.11 (tested with Python
3.12.15). The base CedroDB SDK continues to support Python >=3.9:

```sh
python -m pip install 'pydantic-ai-slim==2.56.0'
python examples/pydantic_ai_history.py
```

It serializes `result.conversation` using `ConversationTypeAdapter`, closes and
reopens CedroDB, and passes the restored bundle as `conversation=` to a fresh
agent. Assertions check preserved conversation identity, accumulated request
usage, and growing message history. The updated conversation and an outbox
event are then committed together. Its `TestModel` makes no network calls and
needs no model credentials. This is conversation storage, not a durable-execution
backend: it does not resume interrupted model/tool calls or authorize tool
approvals. Configure usage limits in your application and load conversations
only from trusted application storage. Pin the framework version and retest
serialization when upgrading. See the upstream
[conversation documentation](https://pydantic.dev/docs/ai/core-concepts/message-history/).

## API reference

| Object | Methods |
| --- | --- |
| `Database` | `collection`, `agent`, `queue`, `batch`, `conditional_batch`, `transaction`, `changes`, `stats`, `verify`, `backup`, `close` |
| `Database.restore_backup` | Static restore with optional content-limit keywords |
| `Collection` | `put`, `get`, `delete`, `put_many`, `find`, `find_page`, `search`, `iter_records`, `create_index`, `compare_exchange`, `aggregate` |
| `Transaction` | `put(collection, key, value)`, `delete(collection, key)`, `commit`, `rollback`, context manager |
| `Queue` | `enqueue`, `claim`, `ack`, `fail`, `extend`, `get`, `stats`, `recover_expired`, `retry_dead` |
| `AgentStore` | `get(thread_id)`, `checkpoint(thread_id, state, *, expected_revision, now_ms, status, metadata, operations)` |

`Database(path, *, max_document_bytes=1_048_576, max_frame_bytes=16_777_216,
max_state_bytes=67_108_864)` opens or creates a directory. Paths accept strings
and `os.PathLike` objects returning strings. Use a context manager or `close()`;
closing is idempotent and releases the native lock immediately. `db.closed`
reports its state. Existing collection handles fail with `DatabaseClosedError`
after close.

Engine failures raise `CedroError`. Unsupported JSON raises `TypeError` or
`ValueError`; integers outside `[-2**63, 2**64 - 1]` raise `OverflowError`. NaN,
infinity, cycles, non-string object keys and unsupported objects are rejected
before mutation. Supported integers round-trip exactly. JSON depth is limited
to 64 levels, and the transport always uses `allow_nan=False`. Type annotations
and a `py.typed` marker are included.

## Current limits

- One open owning handle/process per directory; its native calls are serialized.
- Documents, equality indexes and transaction offsets are held in RAM. Encoded
  content limits exclude allocator overhead, indexes and temporary buffers.
- Append-only log with no compaction, retention or bounded total disk use.
- Handle-local indexes and key-based query pagination, without snapshot isolation.
- `aggregate(field)` uses floating-point arithmetic on a top-level numeric field
  and can lose integer precision above 2**53; stored documents remain exact.
- No SQL, MVCC, replication, encryption or automatic synchronization.
- Queue scheduling scans jobs synchronously, without background execution,
  retention or distributed fencing; job numbers use the cross-SDK safe range.
- Agent state uses revision checks within one database history, without an
  agent runtime or framework checkpointer protocol implementation.
- Retrieval uses exact scans, without ANN or full-text inverted indexes;
  embedding generation and corpus lifecycle are application responsibilities.

## Upgrade from 0.2 to 0.3

Existing document, batch, query, CAS and changefeed APIs keep their signatures.
The database log format is unchanged; 0.2 stores can be opened directly. Version
0.3 adds `db.verify()`, `db.backup(path)` and `Database.restore_backup(...)`.
Backup containers are distinct from raw `cedro.log` files: use `backup()` to
create them. Restore requires a new destination and preserves the original
history/cursor sequence. Recreate equality indexes after opening the restored
store, and configure matching limits when your store exceeds the defaults.

## Upgrade from 0.3 to 0.4

Existing APIs and the database/backup formats stay compatible. Version 0.4 adds
`db.conditional_batch(conditions, operations)` and typed `Condition` and
`ConditionalResult` definitions. Replace application-level get/check/batch
sequences with conditional batches when a stale read could overwrite another
update. Conditions test complete pre-state documents; they do not observe writes
earlier in the operation list. A failed condition is a normal result, while an
invalid request raises an exception.

## Upgrade from 0.4 to 0.5

Existing APIs and the database/backup formats remain compatible. Version 0.5
adds `db.queue(name)` and the typed `Queue`, `Job`, `EnqueueResult`, `QueueStats`
and `RecoveryResult` APIs. Jobs use schema version 1 in reserved
`_cedro.jobs.<name>` collections and participate in the existing durable log,
changefeed and backup format. Generic writes to that namespace are now rejected.
If a 0.4 application used `_cedro.jobs.*` for its own data, export or move those
records to another collection with the old version before upgrading. Existing
logs still replay and can be inspected; legacy application records are not
automatically converted into valid jobs. Keep that namespace out of application writes.
Queue payloads have a smaller, portable numeric range; existing collection
documents keep their previous range. Run expired recovery in your application
and deduplicate external effects; existing outbox consumers may need to filter
out internal queue operations from the changefeed.

## Upgrade from 0.5 to 0.6

Existing APIs and database/backup formats remain compatible. The new APIs are
`db.agent(namespace)` for revision-fenced checkpoints and `collection.search()`
for bounded text/vector/hybrid retrieval. `_cedro.agents.*` is now reserved,
in addition to `_cedro.jobs.*`. If an older application wrote its own records
there, export or move them with the old SDK before upgrading; they are not
automatically converted into checkpoint envelopes. Internal checkpoint writes
appear in the changefeed, so outbox consumers should select their intended
collections. State/metadata use portable numbers; ordinary documents retain
their previous numeric range.

## Build and test

```sh
python -m venv .venv
# Activate the environment with your platform's activation command.
python -m pip install 'maturin>=1.15,<2'
python scripts/prepare_core.py
maturin build --release --out dist
maturin sdist --out dist
python -m pip install ./dist/<your-platform-wheel>.whl
python -m unittest discover -s tests -v
python scripts/verify_sdist.py dist/cedrodb-0.8.0.tar.gz
python examples/offline_forms.py
python examples/inventory_outbox.py
python examples/local_worker.py
python examples/agent_memory.py
```

`prepare_core.py` copies only the core manifest, license and Rust source into
ignored `core/`. Regenerate that copy after engine changes. The sdist includes
those files and has no dependency on `../..` at installation. `verify_sdist.py`
rebuilds an extracted archive, installs the resulting wheel in an isolated target
and runs its tests. Keep `Cargo.lock` for reproducible dependency versions.

[Source](https://github.com/uppy19d0/cedrodb) ·
[Demo](https://www.ltavarez.me/labs/cedrodb) ·
[Issues](https://github.com/uppy19d0/cedrodb/issues)

## Command-line interface (0.8.0)

The current package registers `cedrodb` and keeps `python -m cedrodb` as an equivalent entry point. Run `cedrodb --help` for document, query, retrieval, maintenance and backup commands. See [the CLI guide](../../docs/cli.md). Earlier published 0.7.0 wheels only contain `doctor` and `benchmark`; build/install the current wheel to use the expanded CLI.
