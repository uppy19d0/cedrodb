# CedroDB for Node.js

Experimental embedded document database with a **real native Rust engine**, loaded through N-API. Documents, atomic batches, compare-and-exchange, equality indexes and durable change cursors all use the same CedroDB engine as the Python binding. There is no database server or subprocess.

## Install

Until a registry release is available, install the downloadable package:

```sh
npm install https://www.ltavarez.me/downloads/cedrodb-0.2.0.tgz
```

Matching native binaries bundled under `prebuilds/<platform>/cedrodb.node` load directly. When no matching binary exists, the install script builds the included Rust sources using **Rust >= 1.88, Cargo and a platform C linker**. The package has no npm runtime dependencies. `npm install --ignore-scripts` works only if a matching prebuilt binary is included; otherwise run `npm rebuild cedrodb` with the toolchain installed.

## Quick start

```js
import { Database } from 'cedrodb';
// CommonJS: const { Database } = require('cedrodb');

const db = new Database('./my-data');
try {
  const jobs = db.collection('jobs');
  jobs.putMany([
    { key: '001', value: { state: 'pending', priority: 4 } },
    { key: '002', value: { state: 'done', priority: 2 } },
  ]);
  jobs.createIndex('state');

  const page = jobs.findPage({
    filters: [{ field: 'state', op: 'eq', value: 'pending' }],
    limit: 50,
  });
  console.log(page.records, page.usedIndex, page.scanned);

  const claimed = jobs.compareExchange(
    '001', { state: 'pending', priority: 4 }, { state: 'running', priority: 4 },
  );
  console.log({ claimed });
  console.log(db.changes({ since: 0, limit: 20 }));
} finally {
  db.close();
}
```

**All methods are synchronous and block the calling Node.js thread**, including disk synchronization and scans. Use a Node worker thread if your application must keep its event loop responsive. A database directory can be opened by one handle/process at a time. `close()` releases its file lock immediately and is idempotent; retained collections throw after closing.

## API

- `new Database(path, { maxDocumentBytes?, maxFrameBytes?, maxStateBytes? })`: defaults are 1 MiB per document, 16 MiB per transaction frame and 64 MiB encoded live content. These limits do not cap total process memory. Indexes, allocators and result buffers consume additional memory.
- `db.collection(name)`: a named collection, created when documents are written.
- `collection.put(key, value)`, `get(key)`, `delete(key)`: JSON values with explicit string keys. A missing document returns `undefined`; a stored JSON null returns `null`.
- `collection.putMany([{ key, value }, ...])`: one atomic transaction.
- `db.batch([{ kind: 'put', collection, key, value }, { kind: 'delete', collection, key }])`: one atomic transaction across collections. Validation failures commit none of it. After an I/O failure, close/reopen to resolve the commit outcome.
- `collection.find(query)` / `findPage(query)`: filters are ANDed; operators are `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `exists`. Fields are top-level names or RFC 6901 JSON Pointers (`/profile/city`). `exists` takes a boolean; ordered comparisons take numbers or strings.
- `findPage({ filters?, limit?, after? })`: returns `{ records, nextAfter?, scanned, usedIndex? }`. Key ordering, default 100 and maximum 1000 results. Pass `nextAfter` back as `after`. Concurrent writes between pages can change results; the cursor is not a snapshot.
- `collection.createIndex(field)`: an in-memory equality index, updated by commits. **Indexes are session-local: recreate them after reopening.** Only equality filters use indexes in 0.2.0; range filters scan.
- `collection.compareExchange(key, expected, replacement)`: compare the complete stored JSON value and atomically replace it; returns a boolean. `undefined` means absent for `expected`, or delete for `replacement`. A mismatch writes no log transaction. `null` always means stored JSON null. The absent-to-absent case succeeds without a write.
- `db.changes({ since = 0, limit = 100 })`: `{ transactions: [{ cursor, operations }], nextCursor, hasMore }`. `since` is an exclusive durable transaction cursor; `limit` counts complete transactions, from 1 to 1000. Pages also stop at an encoded log-frame budget of `maxFrameBytes + 12`, without splitting transactions; serialized response and allocation overhead are additional. Persist `nextCursor` to resume. No delivery acknowledgments or automatic replication are implemented. Cursors belong to one database history.
- `collection.aggregate(field)`: `{ count, sum, min, max }` over a top-level numeric field, using floating-point arithmetic. It does not accept nested JSON Pointers.
- `db.stats()`: version, records, collections, contentBytes, logBytes, latestCursor and indexes.
- `db.close()`: deterministic native cleanup.

## Data compatibility and limits

Only plain JSON objects, dense arrays, strings, booleans, null and finite numbers within `±Number.MAX_SAFE_INTEGER` are accepted. `BigInt`, unsafe numbers, `undefined` inside documents, accessors, custom class instances, cycles and unpaired Unicode surrogates throw before committing. Use strings for large identifiers. Reads from a database written by Rust/Python also reject numbers outside the safe numeric range, so those integers cannot be silently rounded by JavaScript. Decimal arithmetic still has the normal IEEE 754 floating-point limitations.

Equality filters and compare-and-exchange compare JSON values structurally. Exactly representable integral floats compare equal to integers within the core's safe numeric range, so a Python float `1.0` matches a JavaScript integer `1`, including inside objects and arrays. Booleans remain distinct from numbers. Object property order does not affect equality. Stored JSON is nested at most 64 levels.

This is an experimental engine. Active documents remain in RAM; its append-only log has no compaction or retention yet. SQL, MVCC and multi-process access to the same directory are not implemented. Do not modify the log while open. There are no benchmark claims against SQLite, MDBX or DuckDB.

## Build and verify

From the CedroDB repository:

```sh
cd bindings/node
npm run build
npm test
npm pack
```

The vendoring script copies only the core `Cargo.toml`, `Cargo.lock`, `src/` and `LICENSE` into `core/`. The packed npm package contains those sources and its own locked Rust dependencies; installation never needs a path outside the package. Source code: [CedroDB](https://github.com/uppy19d0/cedrodb).
