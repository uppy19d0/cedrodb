# CedroDB

**Embedded JSON storage for applications that resume local work after a restart.**

CedroDB is a Rust engine with native Node.js/TypeScript and Python libraries. Documents, atomic batches and a durable transaction changefeed share one storage format. Save a document and its outbox event together, process complete transactions, and resume from a saved cursor.

**0.5.0 is experimental.** We are approaching 1.0 through incremental, tested releases. The engine is not production validated; there are no superiority claims against SQLite, MDBX, UnQLite or DuckDB.

[Node API](bindings/node/README.md) · [Python API](bindings/python/README.md) · [Backups](docs/backups.md) · [Recipes](docs/use-cases.md) · [Local jobs](docs/jobs.md) · [Changelog](CHANGELOG.md) · [Road to 1.0](docs/roadmap.md) · [Live examples](https://www.ltavarez.me/labs/cedrodb)

## Application fit

| Application | CedroDB provides | The application still owns |
| --- | --- | --- |
| Offline forms and desktop tools | JSON documents, consistent backup files and checked restoration | Validation, encryption, off-device copies and conflict resolution |
| Local integrations | A document and outbox event in one atomic batch; durable resume cursors | Delivery, checkpoints and destination deduplication |
| Background reports and deferred imports | Persistent jobs, leased ownership, retries and atomic result writes | Worker lifecycle, capacity, recovery scheduling and external idempotency |
| Small catalogs and operational tools | JSON Pointer filters, equality indexes and keyset pages | Schema, index recreation after opening and a memory budget |
| Maintenance before upgrades | Full-history backups and integrity checks without silent repair | Scheduling, restore drills and retention |

One open handle owns a database directory. Operations within it serialize through a mutex and all live documents remain in RAM. Choose an established database when you need mature production guarantees, SQL, multi-process access or data larger than the available memory.

## Install

Node.js 0.5.0 is available as a release download while npm account verification is pending:

```sh
npm install https://www.ltavarez.me/downloads/cedrodb-0.5.0.tgz
```

Once npm publication is verified, install by name:

```sh
npm install cedrodb@0.5.0
```

Install Python from [PyPI](https://pypi.org/project/cedrodb/0.5.0/). Pip selects a compatible wheel when available; direct release downloads remain available from [the platform selector](https://www.ltavarez.me/labs/cedrodb).

```sh
python -m pip install cedrodb==0.5.0
```

Python supports 3.9+; its source distribution needs Rust 1.85+ and a platform linker. Node supports 18+, includes binaries for macOS ARM64/x64, Linux x64 with compatible glibc and Windows x64, and needs Rust 1.88+ for its source fallback. [Distribution details](docs/native-libraries.md).

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

## Features in 0.5

- JSON documents, explicit collection/key names and atomic cross-collection batches.
- Versioned append-only frames with length checks, CRC32 and `sync_all` before changes become visible.
- Incomplete-tail recovery; complete corrupted frames produce an error.
- AND filters (`eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `exists`), nested JSON Pointers, equality indexes and ordered keyset pages.
- Atomic compare-and-exchange, numeric aggregates and operational statistics.
- [Conditional cross-document batches](docs/conditional-batches.md) for guarded stock reservations and idempotent request keys.
- Durable changefeed with complete transactions and bounded encoded pages.
- Read-only integrity checks, consistent full-history backup files and validated restoration without overwriting existing destinations.
- Persistent [local job queues](docs/jobs.md): priorities, scheduling, leases, retry limits, stale-worker fencing and atomic result acknowledgements.
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

The 0.5 log remains compatible with 0.2. Backup containers are separate. See [architecture](docs/architecture.md), [migration notes](docs/migrations.md) and [benchmark protocol](docs/benchmarks.md).

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
