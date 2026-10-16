# CedroDB

**Local storage for agent checkpoints, searchable memory and recoverable work.**

CedroDB is a Rust engine with native Node.js/TypeScript and Python libraries. Save an agent checkpoint and its document/outbox writes in one transaction, reject stale checkpoint revisions, and retrieve local memory using text, supplied embeddings or both. Documents, jobs, backups and a durable changefeed share one storage format.

**0.8.0 is experimental.** We are approaching 1.0 through incremental, tested releases. The engine is not production validated; there are no superiority claims against SQLite, MDBX, UnQLite or DuckDB.

[CLI commands](docs/cli.md) · [Performance tools](docs/performance.md) · [Installation guide](docs/installation.md) · [Agents and retrieval](docs/agents.md) · [Research](docs/ai-research.md) · [Node API](bindings/node/README.md) · [Python API](bindings/python/README.md) · [Backups](docs/backups.md) · [Recipes](docs/use-cases.md) · [Local jobs](docs/jobs.md) · [Changelog](CHANGELOG.md) · [Road to 1.0](docs/roadmap.md) · [Live examples](https://www.ltavarez.me/labs/cedrodb)

## Application fit

| Application | CedroDB provides | The application still owns |
| --- | --- | --- |
| Local assistants and agent tools | Revision-fenced checkpoints, atomic state/results/outbox writes, text/vector/hybrid retrieval | Models, embeddings, tool authorization, context selection and execution orchestration |
| Offline forms and desktop tools | JSON documents, consistent backup files and checked restoration | Validation, encryption, off-device copies and conflict resolution |
| Local integrations | A document and outbox event in one atomic batch; durable resume cursors | Delivery, checkpoints and destination deduplication |
| Background reports and deferred imports | Persistent jobs, leased ownership, retries and atomic result writes | Worker lifecycle, capacity, recovery scheduling and external idempotency |
| Small catalogs and operational tools | JSON Pointer filters, equality indexes and keyset pages | Schema, index recreation after opening and a memory budget |
| Maintenance before upgrades | Full-history backups and integrity checks without silent repair | Scheduling, restore drills and retention |

One open handle owns a database directory. Operations within it serialize through a mutex and all live documents remain in RAM. Choose an established database when you need mature production guarantees, SQL, multi-process access or data larger than the available memory.

## Install

Install Node.js from [npm](https://www.npmjs.com/package/cedrodb/v/0.8.0):

```sh
npm install cedrodb@0.8.0
```

Install Python from [PyPI](https://pypi.org/project/cedrodb/0.8.0/). Pip selects a compatible wheel when available; direct release downloads remain available from [the platform selector](https://www.ltavarez.me/labs/cedrodb).

```sh
python -m pip install cedrodb==0.8.0
```

Python supports 3.9+; its source distribution needs Rust 1.85+ and a platform linker. Node supports 18+, includes binaries for macOS ARM64/x64, Linux x64 with compatible glibc and Windows x64, and needs Rust 1.88+ for its source fallback. [Distribution details](docs/native-libraries.md).

## Diagnose queries and memory retrieval

Inspect the live owner handle, then execute a query with an actual plan and native timing:

```python
report = db.diagnostics()
profile = db.collection('support-memory').profile_search(
    text='refund policy',
    filters=[{'field': 'tenant', 'op': 'eq', 'value': 'shop-1'}],
)
print(profile['elapsed_micros'], profile['plan'])
print(report['session'])
```

The Node equivalents are `db.diagnostics()`, `collection.profileQuery(options)` and
`collection.profileSearch(options)`. Profiles include the ordinary result page;
they execute once and do not append a log transaction. Session counters cover
queries, searches and nonempty shared commit attempts; they reset on reopening.

Run the included Python tools against an isolated temporary workload:

```sh
python -m cedrodb doctor
python -m cedrodb benchmark --records 1000 --iterations 30
```

The benchmark checks result equality before reporting timings, compares actual
scan work with/without an equality index, and records native/SDK latency samples,
batch writes, index construction and reopening. It opens no application database.
[Metric definitions, monitoring and methodology](docs/performance.md).

## Save a document and its event together

```js
import { Database } from 'cedrodb';

const db = new Database('./orders.cedro'); // A directory.
try {
  db.batch([
    { kind: 'put', collection: 'orders', key: 'order:1',
      value: { status: 'paid', amount: 120 } },
    { kind: 'put', collection: 'outbox', key: 'event:1',
      value: { type: 'order.paid', orderId: 'order:1' } },
  ]);
  db.collection('orders').createIndex('status');
  const paid = db.collection('orders').findPage({
    filters: [{ field: 'status', op: 'eq', value: 'paid' }],
  });
  const changes = db.changes({ since: 0, limit: 50 });
  console.log(paid.records, changes.nextCursor);
} finally {
  db.close();
}
```

Persist a consumer cursor after processing. A crash between delivery and checkpoint persistence can cause delivery again. Consumers must be idempotent; no exactly-once external effects are promised.

## Back up before upgrading an offline application

```python
from cedrodb import Database

with Database('./field-data.cedro') as db:
    db.collection('forms').put('form:1', {'status': 'draft', 'site': 'SDQ'})
    report = db.verify()
    db.backup('./before-upgrade.cedrobak')  # Must not exist.

Database.restore_backup('./before-upgrade.cedrobak', './restored.cedro')
with Database('./restored.cedro') as restored:
    assert restored.collection('forms').get('form:1')['site'] == 'SDQ'
```

Backups preserve the complete log and transaction cursors. The versioned container checks its expected length and CRC32; restoration validates it before creating a new destination. CRC32 detects accidental corruption, not malicious modification. Backup and verification hold the engine mutex. [Recovery procedures and limits](docs/backups.md).

## Search memory and save agent progress

```python
from cedrodb import Database

with Database('./assistant.cedro') as db:
    memory = db.collection('support-memory')
    memory.put('refund-policy', {
        'text': 'Refunds are available within 30 days.',
        'embedding': [1.0, 0.0],  # Synthetic demo; supply real embeddings.
        'model': 'demo-v1', 'tenant': 'shop-1',
    })
    page = memory.search(
        text='refund', vector=[1.0, 0.0],
        filters=[{'field': 'tenant', 'op': 'eq', 'value': 'shop-1'},
                 {'field': 'model', 'op': 'eq', 'value': 'demo-v1'}],
    )
    agents = db.agent('support')
    previous = agents.get('conversation:42')
    saved = agents.checkpoint(
        'conversation:42', {'memory_keys': [hit['key'] for hit in page['hits']]},
        expected_revision=previous['revision'] if previous else None,
        operations=[{'kind': 'put', 'collection': 'outbox',
                     'key': 'conversation:42:answer',
                     'value': {'type': 'answer.ready'}}],
    )
    assert saved['applied']
```

A stale revision changes neither the checkpoint nor its outbox writes. Text search uses BM25, vector search uses exact cosine similarity, and hybrid search combines both rankings using reciprocal rank fusion. Filters run before scoring; equality indexes can narrow the scan. Exceeding the scan budget returns an error rather than a partial top-k. Your application supplies embeddings; CedroDB makes no model or network calls. [Full contracts and boundaries](docs/agents.md).

## Features in 0.7

- JSON documents, explicit collection/key names and atomic cross-collection batches.
- Versioned append-only frames with length checks, CRC32 and `sync_all` before changes become visible.
- Incomplete-tail recovery; complete corrupted frames produce an error.
- AND filters (`eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `exists`), nested JSON Pointers, equality indexes and ordered keyset pages.
- Atomic compare-and-exchange, numeric aggregates and operational statistics.
- [Conditional cross-document batches](docs/conditional-batches.md) for guarded stock reservations and idempotent request keys.
- Durable changefeed with complete transactions and bounded encoded pages.
- Read-only integrity checks, consistent full-history backup files and validated restoration without overwriting existing destinations.
- Persistent [local job queues](docs/jobs.md): priorities, scheduling, leases, retry limits, stale-worker fencing and atomic result acknowledgements.
- Scoped agent checkpoints with revision checks and atomic document/outbox operations.
- Text/BM25, exact vector/cosine and hybrid/RRF retrieval with metadata prefilters, deterministic ties and explicit scan budgets.
- Exact floating-point parsing across replay, backups and SDK JSON boundaries.
- Collection/index inventory, content limits, session counters and query/search profiles with actual candidate/scan work.
- Isolated Python benchmark and runtime diagnostic commands.
- Native SDKs, TypeScript ESM/CommonJS types, Python type hints and shared Node/Python storage.

Indexes are session-local: recreate them after opening. Separate calls/pages do not share a snapshot. APIs are synchronous; use a worker for disk-heavy work when the application's main thread must stay responsive.

## Resource and compatibility boundaries

| Default encoded-content limit | Value |
| --- | --- |
| Document | 1 MiB |
| Transaction payload | 16 MiB |
| Live state | 64 MiB |
| JSON depth | 64 levels |
| Query page | 1–1,000 records |
| Equality indexes per handle | 64 |

These are not RSS limits: maps, indexes, decoded JSON, result copies and temporary verification state consume additional RAM. Changefeed offsets use eight bytes per transaction. The log/backups retain history; compaction and automatic retention are not implemented.

JavaScript rejects unsafe integers instead of rounding Rust/Python data. Python supports integers in [-2^63, 2^64−1]. Structural equality treats exactly equivalent integer/float values within ±2^53 alike, with booleans distinct. Aggregates use `f64` and can lose integer precision above 2^53; use an appropriate exact representation for accounting.

The 0.6 log remains compatible with 0.2. Backup containers are separate. See [architecture](docs/architecture.md), [migration notes](docs/migrations.md) and [benchmark protocol](docs/benchmarks.md).

SQL, MVCC, replication, encryption, multi-process database access and a C ABI are not implemented. Durability depends on OS/filesystem/device synchronization. A commit I/O failure poisons the handle: close/reopen to resolve the uncertain outcome before retrying. Process-kill tests do not certify physical power-loss behavior.

## Build and contribute

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked --example portfolio_demo
```

The core currently targets Rust 1.74+; SDK source builds require the higher versions above. Rust is not published on crates.io; use a local path dependency. The [roadmap](docs/roadmap.md) defines the evidence gates for 1.0. Crash recovery tests, malformed-input cases and interoperability reproductions are especially useful contributions.

## License

MIT. Dependencies retain their respective licenses.
