# CedroDB for Node.js

A small embedded document database powered by the CedroDB Rust engine through N-API. Store JSON locally, commit related changes together, query indexed fields, and replay durable transactions without running a database server.

**Experimental 0.7 release.** CedroDB keeps active documents in memory and persists a checksummed transaction log. Capabilities include query diagnostics/profiles, exact local text/vector retrieval, revision-protected agent checkpoints, durable local job queues, guarded transactions and verified backups. It is intended for evaluation, local tools and applications whose storage limits fit the documented memory model.

[Project](https://github.com/uppy19d0/cedrodb) · [Architecture](https://github.com/uppy19d0/cedrodb/blob/main/docs/architecture.md) · [Backup and recovery](https://github.com/uppy19d0/cedrodb/blob/main/docs/backups.md) · [Python API](https://github.com/uppy19d0/cedrodb/tree/main/bindings/python) · [Interactive demo](https://www.ltavarez.me/labs/cedrodb)

## Install

```sh
npm install cedrodb@0.7.0
```

Requires **Node.js 18 or newer**. ESM, CommonJS and TypeScript are supported; the package has no npm runtime dependencies.

Matching release binaries avoid a local Rust build:

| Operating system | Architecture / runtime | Binary directory |
| --- | --- | --- |
| macOS | Apple Silicon ARM64 | `prebuilds/darwin-arm64` |
| macOS | Intel x64 | `prebuilds/darwin-x64` |
| Linux | x64, glibc 2.34 or newer | `prebuilds/linux-x64-gnu` |
| Windows | x64, MSVC | `prebuilds/win32-x64-msvc` |

Installation loads a compatible bundled binary when one is present. If none loads, the install script builds the included Rust sources. Source builds require **Rust 1.88+, Cargo and a platform C linker**: Xcode Command Line Tools on macOS, a C development toolchain on Linux, or MSVC Build Tools on Windows. Rust is not required at runtime when a matching prebuilt binary loads.

Linux musl is detected separately and never loads a glibc binary. Other operating systems, CPU architectures and libc combinations have no prebuilt compatibility guarantee; a source build is an application-side evaluation, not a tested release target. This native Node package does not support browsers, Android or iOS. It does not download another executable during installation.

If installation scripts are disabled, a matching prebuilt binary must already be included. After installing a missing build toolchain, retry compilation with:

```sh
npm rebuild cedrodb
```

## Quick start

```js
import { Database } from 'cedrodb';
// CommonJS: const { Database } = require('cedrodb');

const db = new Database('./app-data');
try {
  const inspections = db.collection('inspections');
  inspections.putMany([
    { key: '001', value: { state: 'pending', site: 'Santo Domingo', score: 9 } },
    { key: '002', value: { state: 'sent', site: 'Santiago', score: 7 } },
  ]);
  inspections.createIndex('state');

  const page = inspections.findPage({
    filters: [{ field: 'state', op: 'eq', value: 'pending' }],
    limit: 50,
  });
  console.log(page.records); // [{ key: '001', value: { ... } }]
  console.log(page.usedIndex, page.scanned);
  console.log(db.verify());
} finally {
  db.close();
}
```

**All methods are synchronous and block the calling Node.js thread**, including disk synchronization, verification and backups. Use a Node worker thread when the application's event loop must remain responsive. One native handle/process owns a database directory at a time. Always close it explicitly; garbage collection is only a cleanup fallback.

## Where it fits

| Use case | Useful primitives | Application responsibility |
| --- | --- | --- |
| Offline desktop or field application | JSON collections, equality indexes, verified backups | UI, synchronization protocol and conflict policy |
| Local outbox or audit trail | Atomic batches and durable change cursors | Delivery, checkpoints and deduplication of external effects |
| Local workers and automation | Durable jobs, leases, retries and atomic acknowledgment with results | Run the worker loop, provide a suitable clock and make external effects idempotent |
| Local AI assistants and retrieval tools | Exact search, revision-protected checkpoints and atomic checkpoint/result writes | Generate embeddings, select the model/tenant, validate messages and control execution |
| Device state and worker checkpoints | Compare-and-exchange, persisted history | Scheduling, retries and business rules |
| Diagnose a bounded local workload | Session metrics, collection diagnostics and query/search profiles | Measure representative workloads and choose useful indexes |
| Node.js and Python utilities sharing local data | The same Rust engine and on-disk format | Open the directory sequentially; use portable JSON numbers |

## Diagnostics and query profiles

Inspect configured limits, encoded content, session indexes and native operation counters without writing records:

```js
const report = db.diagnostics();
console.table(report.collections);
console.log(report.limits, report.poisoned, report.writerActive);
console.table(report.session);

const inspections = db.collection('inspections');
const query = {
  filters: [{ field: 'state', op: 'eq', value: 'pending' }],
  limit: 50,
};
const before = inspections.profileQuery(query);
inspections.createIndex('state');
const after = inspections.profileQuery(query);
console.log({ before: before.plan, after: after.plan });
console.log(after.elapsedMicros, after.page.records);

const searchProfile = db.collection('knowledge').profileSearch({
  text: 'backups',
  filters: [{ field: 'model', op: 'eq', value: 'synthetic-2d' }],
  maxScanned: 1000,
});
console.log(searchProfile.elapsedMicros, searchProfile.plan, searchProfile.page.hits);
```

`diagnostics()` returns `{ stats, limits, collections, session, poisoned, writerActive }`. Each collection reports its name, record count, encoded `contentBytes` and session `indexFields`. Limits are `maxDocumentBytes`, `maxFrameBytes` and `maxStateBytes`; they do not describe total process RAM. `session` contains `query`, `search` and `commit` counters with `attempts`, `successes`, `failures`, `totalElapsedMicros` and `maxElapsedMicros`. Counters and indexes reset when the database is reopened. Counters observed during concurrent calls are not one atomic snapshot across all operations.

`profileQuery()` and `profileSearch()` execute the operation once and return `{ page, elapsedMicros, plan }`. `page` has exactly the ordinary query/search result shape. `plan` contains `collectionRecords`, `candidateRecords`, `scanned`, `returned` and optional `usedIndex`. Candidates reflect the selected equality index; query candidates also exclude keys at or before `after`. Query scan counts include pagination lookahead, so `scanned` can exceed `returned` even when an index is used. Search retains its full ranking and scan-budget rules; a failed operation throws instead of returning a partial profile.

Elapsed microseconds are measured inside the Rust engine, including query/search lock wait, planning, scanning, ranking and cloning the result. They exclude SDK JSON conversion and application work, and may be zero for short calls. Profiles count one query/search attempt, just like ordinary queries/searches. Core validation failures count; inputs rejected by JavaScript or its native JSON bridge before engine invocation do not. Commit counters cover nonempty commit attempts, including failures inside that commit path; they exclude earlier input/staging failures, condition/revision conflicts and empty batches. Commit timings exclude lock wait. These counters are diagnostics, not a complete API-call audit trail.

Run [diagnostics.mjs](https://github.com/uppy19d0/cedrodb/blob/main/bindings/node/examples/diagnostics.mjs) against an existing directory to print a report. It performs no record writes; ordinary database opening can still recover an incomplete final append. A newly opened inspection handle starts with zero session counters, so inspect a running application's handle for live workload metrics. [profile-indexes.mjs](https://github.com/uppy19d0/cedrodb/blob/main/bindings/node/examples/profile-indexes.mjs) creates a fresh synthetic dataset and prints actual timings and candidate counts before/after an equality index. It checks identical results and makes no benchmark or speedup guarantee.

## Local retrieval and agent checkpoints

Use `Collection.search()` to retrieve local documents, then `AgentStore.checkpoint()` to save application-defined agent state and its related results/outbox writes in one transaction. The engine does not call an embedding model, execute tools or run a workflow framework.

### Exact text, vector and hybrid retrieval

```js
const knowledge = db.collection('knowledge');
knowledge.putMany([
  { key: 'backups', value: { tenant: 'alice', model: 'synthetic-2d', text: 'Backups preserve transaction history.', embedding: [1, 0] } },
  { key: 'jobs', value: { tenant: 'alice', model: 'synthetic-2d', text: 'Jobs use leases and retries.', embedding: [0.2, 0.9] } },
]);
knowledge.createIndex('model');

const found = knowledge.search({
  text: 'backups history',
  vector: [1, 0],
  vectorField: 'embedding',
  textFields: ['text'],
  filters: [
    { field: 'tenant', op: 'eq', value: 'alice' },
    { field: 'model', op: 'eq', value: 'synthetic-2d' },
  ],
  limit: 5,
  maxScanned: 1000,
});
console.log(found.hits, found.mode, found.scanned, found.usedIndex);
```

The vectors above are synthetic API examples, not useful model embeddings or benchmarks. Applications generate embeddings separately and should store the model/version with each document. Filter by model and tenant before retrieval; equal dimensions do not make embeddings from different models compatible. Filters select data and do not provide authentication or authorization.

`search({ text })` uses BM25 with `k1 = 1.2` and `b = 0.75`. Text is split on non-alphanumeric Unicode characters and lowercased; there is no stemming, accent folding, language detection or typo tolerance. Corpus statistics are calculated over the filtered documents with nonempty text in the selected fields. Fields can be top-level names or RFC 6901 JSON Pointers.

`search({ vector })` uses exact cosine similarity, including non-unit vectors. Query vectors must have 1–4096 finite numeric components and nonzero magnitude. A filtered document with no vector field is skipped by that channel; a present malformed, zero or dimension-mismatched vector produces an error. Metadata filters run before document vector validation, so unrelated models can be excluded explicitly. `minVectorScore` accepts a cosine threshold from -1 to 1 and requires a vector query.

Providing both `text` and `vector` uses reciprocal rank fusion: each channel contributes `1 / (60 + rank)` with ranks starting at one. The engine ranks each complete channel before applying `limit`. Text matches and vector matches form a union; `minVectorScore` filters the vector channel, so a document can still appear through its text score. Final ties are ordered by document key. BM25, cosine and fused scores have different meanings and should not be compared across modes.

The result is `{ hits, scanned, matched, usedIndex?, mode }`. Each hit contains `{ key, value, score, vectorScore, textScore }`, with null when a channel has no computed score. A computed nonmatching text score can be zero when the vector channel supplies the hit. `matched` counts documents matching at least one channel before the result limit. `mode` is `text`, `vector` or `hybrid`.

| Search option | Default / limit |
| --- | --- |
| `text` | Optional; 1–4096 UTF-8 bytes and 1–64 distinct alphanumeric query terms |
| `vector` | Optional number array; 1–4096 dimensions |
| `vectorField` | `embedding` |
| `textFields` | `['text']`; 1–8 unique field names / JSON Pointers |
| `filters` | `[]`; same AND predicates as `find()` |
| `limit` | 10; allowed 1–1000 |
| `maxScanned` | 10000; allowed 1–100000 |
| `minVectorScore` | Omitted; otherwise a finite number in -1..1 |

At least one retrieval channel is required. `maxScanned` counts candidates visited **before** metadata filtering. The narrowest available equality index can reduce that candidate set. Exceeding the budget throws instead of returning a silently truncated top-k. Search is an exact synchronous scan/ranking operation over committed state, with additional result/working memory. It has no approximate vector index, persistent full-text index or automatic embedding provider; use it for bounded local workloads.

### Revision-protected agent state

```js
const agents = db.agent('support');
const created = agents.checkpoint('alice:thread:42', {
  messages: [{ id: 'u1', role: 'user', parts: [{ type: 'text', text: 'Explain backups' }] }],
  step: 'retrieve',
}, { nowMs: 1000 });

if (created.applied) {
  const saved = agents.checkpoint('alice:thread:42', {
    step: 'review',
    retrievedKeys: found.hits.map(hit => hit.key),
  }, {
    expectedRevision: created.checkpoint.revision,
    nowMs: 1001,
    status: 'waiting',
    metadata: { approvalRequired: true },
    operations: [
      { kind: 'put', collection: 'tool_results', key: 'retrieval:42', value: { keys: found.hits.map(hit => hit.key) } },
      { kind: 'put', collection: 'outbox', key: 'review:42', value: { type: 'agent.review_requested' } },
    ],
  });
  console.log(saved.applied, saved.checkpoint);
}
```

`db.agent(namespace)` returns an `AgentStore` using the same native database handle. `get(threadId)` returns the latest `AgentCheckpoint` or `undefined`. Checkpoints contain `schemaVersion`, `threadId`, `revision`, `status`, `state`, `metadata` and `updatedAt`. State is any supported JSON value; metadata must be a JSON object. Both use portable safe-range numbers across Node, Python and Rust. Namespace names use 1–128 ASCII letters, digits, underscores, dots or hyphens; thread IDs use 1–4096 UTF-8 bytes without NUL.

`checkpoint(threadId, state, options?)` returns `{ applied, cursor, checkpoint }`. **An omitted, undefined or null `expectedRevision` requires the thread to be absent.** It never means an unconditional update. To update an existing thread, pass the positive revision returned by `get()` or a successful checkpoint. Revisions are opaque durable transaction cursors and can skip numbers when unrelated commits occur; do not calculate the next revision yourself.

The engine checks the revision and commits the checkpoint and optional ordinary put/delete operations under one lock. A stale continuation changes nothing and returns `applied: false` with the actual checkpoint, or null when the thread is absent. Validation errors throw even when the revision would conflict. All ordinary document, state and frame limits apply to the combined checkpoint envelope and operations. Leave headroom for metadata and results; failures commit none of the transaction.

Options default to `expectedRevision: null`, `nowMs: Date.now()`, `status: 'running'`, `metadata: {}` and `operations: []`. Status accepts `running`, `waiting` or `completed` and is application-managed data: it does not authorize a tool, enforce an approval or resume execution. The latest checkpoint survives reopening and verified backup restoration. Older log history remains retained, but this API exposes the current checkpoint rather than a historical branching workflow.

The `_cedro.agents.<namespace>` collection is reserved. Direct generic mutations and application writes from checkpoint/queue completion operations cannot target reserved agent or queue collections. Namespaces separate records; applications still own access control, message/tool schema validation, execution and external-effect idempotency. Revision checks prevent stale local state updates, not duplicate external tool calls.

This is a CedroDB JSON checkpoint API, **not an implementation of LangGraph's checkpoint saver/store contracts**. Applications using AI SDK should preserve their full UI message representation and validate restored messages against their current tool and metadata schemas. See [AI SDK persistence guidance](https://ai-sdk.dev/docs/ai-sdk-ui/chatbot-message-persistence) and the complete runnable [agent-memory.mjs example](https://github.com/uppy19d0/cedrodb/blob/main/bindings/node/examples/agent-memory.mjs). The example makes no model calls and uses clearly labeled synthetic embeddings.

## Durable local jobs

A queue belongs to the same database as your documents. Its practical advantage is **atomic completion with application writes**: acknowledge a job, save its result and create an outbox event in one durable transaction.

```js
const queue = db.queue('reports');
queue.enqueue('report:42', { customer: 'cedro', amount: 25 }, {
  maxAttempts: 3,
  priority: 10,
});

const job = queue.claim('desktop-worker', { leaseMs: 30000 });
if (job) {
  const completed = queue.ack(job.id, job.leaseToken, {
    operations: [
      { kind: 'put', collection: 'reports', key: job.id, value: { total: job.payload.amount } },
      { kind: 'put', collection: 'outbox', key: `${job.id}:ready`, value: { type: 'report.ready', reportId: job.id } },
    ],
  });
  console.log({ completed });
}
```

Stable job IDs deduplicate enqueue attempts, including jobs already `done` or `dead`; duplicates return `{ inserted: false, job }` and keep the original payload. A new job starts `queued`. `claim()` leases a ready job and increments its attempt count. Selection uses descending priority, then availability time and ID. Delayed jobs are skipped until their availability time.

The claim returns an opaque lease token. `ack()`, `fail()` and `extend()` require that token and a lease that has not expired. At the exact expiration timestamp the old lease is invalid. Reclaiming a job creates a new token; stale tokens return `false` without writing. Tokens fence ownership only inside the same database history and must not be used as globally unique external identifiers.

```js
const job = queue.claim('desktop-worker', { leaseMs: 30000 });
if (job) {
  queue.extend(job.id, job.leaseToken, { leaseMs: 60000 });
  // If work fails instead of completing:
  queue.fail(job.id, job.leaseToken, 'Renderer temporarily unavailable', {
    retryDelayMs: 5000,
  });
}
```

`fail()` requeues a job while attempts remain, otherwise moves it to `dead`. An expired lease with attempts remaining can be reclaimed directly. An exhausted expired lease stays `leased` until `recoverExpired()` moves it to `dead`. Recovery is explicit and bounded; call it again while `hasMore` is true. `retryDead()` requeues a dead job with its attempt count reset, preserving the stable ID and payload.

```js
console.log(queue.stats());
console.log(queue.recoverExpired({ limit: 10 }));
// { requeued, dead, hasMore }
queue.retryDead('report:42');
```

| Queue API | Result and defaults |
| --- | --- |
| `db.queue(name)` | Queue handle using the database's existing native connection |
| `queue.enqueue(id, payload, options?)` | `{ inserted, job }`; delay 0 ms, maxAttempts 3, priority 0 |
| `queue.claim(worker, options?)` | Leased job or `undefined`; lease 30000 ms |
| `queue.ack(id, token, { operations? }?)` | Boolean; completion and optional put/delete operations commit together |
| `queue.fail(id, token, error, options?)` | Boolean; retryDelayMs defaults to 0 |
| `queue.extend(id, token, options?)` | Boolean; leaseMs defaults to 30000; never shortens an existing lease |
| `queue.get(id)` | Job or `undefined` |
| `queue.stats(options?)` | `{ queued, leased, done, dead, ready, expired }` |
| `queue.recoverExpired(options?)` | `{ requeued, dead, hasMore }`; limit 10, allowed 1–1000 |
| `queue.retryDead(id, options?)` | Boolean |

All timed calls accept `nowMs`, defaulting to `Date.now()`. `enqueue()` additionally accepts `delayMs`, `maxAttempts` (1–1000) and `priority` (0–1000); `claim()` and `extend()` accept a positive `leaseMs`. Durations and timestamps must be nonnegative safe integers, and their sums must remain in the safe range. An injected clock makes tests deterministic; clock jumps can change lease behavior, so applications should use a consistent time source.

Queue names contain 1–128 ASCII letters, digits, underscores, dots or hyphens. Job IDs contain 1–4096 UTF-8 bytes, worker names 1–128 and errors 1–4096; none may contain NUL characters. Job payloads use portable JSON with every numeric magnitude within `Number.MAX_SAFE_INTEGER`, including jobs written through Python or Rust. Use strings for larger identifiers.

Jobs expose `schemaVersion`, `id`, `payload`, `status`, `attempts`, `maxAttempts`, `priority`, `createdAt`, `availableAt`, `leaseUntil`, `leaseToken`, `worker` and `lastError`. Lease fields, worker and last error may be null. TypeScript's `LeasedJob` return type makes the token non-null after a successful claim.

Queue metadata is stored in the reserved `_cedro.jobs.<name>` collection. Use queue methods to mutate it; acknowledgment operations cannot target any reserved queue collection. The full job record, including metadata, counts toward the configured document and state limits. Leave headroom: a payload can fit when enqueued but a later lease, worker name or error can make the job exceed a document, state or frame limit. Such transitions fail atomically and leave the prior job state unchanged. Close and reopen with higher limits if additional capacity is needed; the queue never prunes jobs automatically. Queue changes appear in the durable changefeed and are included in verification and backups.

Queue operations are synchronous; `claim()`, `stats()` and `recoverExpired()` scan the queue's current jobs and are intended for bounded local workloads. Done/dead jobs and their history remain stored. The library provides no scheduler, background worker, server, automatic pruning or exactly-once execution of external effects. One database owner coordinates the workload; independent processes cannot concurrently open the same directory. If a worker performs an external action and crashes before acknowledgment, another attempt may perform that action again. Use application idempotency keys and an outbox delivery policy.

See [local-worker.mjs](https://github.com/uppy19d0/cedrodb/blob/main/bindings/node/examples/local-worker.mjs) for a complete deterministic retry and atomic-result example without sleeps.

## Atomic writes and conditional updates

`putMany()` commits one collection's records together. `batch()` commits operations across collections in a single transaction:

```js
db.batch([
  { kind: 'put', collection: 'orders', key: 'order:42', value: { state: 'paid', total: 25 } },
  { kind: 'put', collection: 'outbox', key: 'order:42:paid', value: { type: 'order.paid', orderId: 'order:42' } },
]);

const jobs = db.collection('jobs');
const created = jobs.compareExchange('job:1', undefined, { state: 'ready' });
const claimed = jobs.compareExchange('job:1', { state: 'ready' }, { state: 'running' });
const removed = jobs.compareExchange('job:1', { state: 'running' }, undefined);
```

For conditional updates, `undefined` means absent when used as the expected value and deletion when used as the replacement. `null` always means stored JSON null. A mismatch returns `false` without appending a transaction; absent-to-absent succeeds without writing. Comparison covers the complete document. Validation failures commit none of a batch. After a commit I/O error, close and reopen the database to resolve its outcome.

### Guarded transactions across collections

Use `conditionalBatch()` when several related changes depend on previously read data. The engine checks every guard against committed state, then commits all operations while holding the same database mutex:

```js
const inventory = db.collection('inventory');
inventory.put('item:cedar', { available: 3, revision: 1 });
const before = inventory.get('item:cedar');

const result = db.conditionalBatch({
  conditions: [
    { kind: 'equals', collection: 'inventory', key: 'item:cedar', value: before },
    { kind: 'absent', collection: 'orders', key: 'order:42' },
  ],
  operations: [
    { kind: 'put', collection: 'inventory', key: 'item:cedar', value: { available: before.available - 1, revision: before.revision + 1 } },
    { kind: 'put', collection: 'orders', key: 'order:42', value: { state: 'reserved' } },
    { kind: 'put', collection: 'outbox', key: 'order:42:reserved', value: { type: 'order.reserved', orderId: 'order:42' } },
  ],
});
console.log(result); // { applied: true, failedCondition: null, cursor: ... }
```

A mismatch returns `applied: false` and the zero-based `failedCondition` index, leaves every collection unchanged, and does not advance the durable cursor. Include a guard for every document your decision depends on. An `absent` guard forbids a value field; an `equals` guard requires a JSON value, with null distinct from absence. Conditions are evaluated before any operation, including operations on the guarded keys. The limit is 64 conditions, with normal document and transaction-frame limits. Invalid input throws even when another guard would mismatch. Matching empty batches succeed without writing; an empty condition list makes the batch unconditional.

After a mismatch, applications can read fresh data and decide whether to retry. This API does not add snapshots or automatic retries, and the outbox example still requires deduplication when producing external effects. See the runnable [guarded inventory example](https://github.com/uppy19d0/cedrodb/blob/main/bindings/node/examples/guarded-inventory.mjs).

## Queries and durable changes

```js
const page = db.collection('orders').findPage({
  filters: [
    { field: 'state', op: 'eq', value: 'paid' },
    { field: '/customer/city', op: 'exists', value: true },
  ],
  limit: 100,
});
// Continue with { ...query, after: page.nextAfter } when nextAfter is defined.

const changes = db.changes({ since: 0, limit: 100 });
for (const transaction of changes.transactions) {
  console.log(transaction.cursor, transaction.operations);
}
// Save changes.nextCursor only after processing succeeds.
```

Filters are ANDed. Operators are `eq`, `ne`, `gt`, `gte`, `lt`, `lte` and `exists`; fields are top-level names or RFC 6901 JSON Pointers. `exists` takes a boolean. Ordered comparisons take numbers or strings. Missing fields differ from null.

Query pages are ordered by key, default to 100 results, and accept at most 1000. `find()` returns one bounded page of records; `findPage()` also returns `nextAfter`, `scanned` and `usedIndex`. Pass the exclusive `nextAfter` key into the next query. Pages are not snapshots, so intervening writes can change results. Equality filters can use session-local indexes; range filters scan. Recreate indexes after reopening or restoring a database.

The changefeed returns complete committed transactions with an exclusive `since` cursor. The transaction limit is 1–1000; pages also stop at an encoded log-frame budget of `maxFrameBytes + 12`, without splitting transactions. The decoded result and JSON response require additional memory. Cursors survive reopening and verified backups, belong to one database history, and must not exceed its latest cursor. The library has no background delivery worker or exactly-once guarantee; applications must checkpoint and deduplicate external effects.

## Verification, backup and restore

```js
const id = Date.now();
const backupPath = `./snapshot-${id}.cedro-backup`;
const restoredPath = `./restored-${id}`;

const integrity = db.verify();
const snapshot = db.backup(backupPath);
const restored = Database.restoreBackup(backupPath, restoredPath, {
  maxStateBytes: 64 * 1024 * 1024,
});
console.log(integrity, snapshot, restored);
// { logBytes, transactions, records, contentBytes }

const recovered = new Database(restoredPath);
try {
  console.log(recovered.changes({ since: 0 }));
} finally {
  recovered.close();
}
```

`verify()` strictly validates every persisted frame and replays the log without repairing or truncating it. `backup(path)` verifies the log and writes a versioned container with its exact length and CRC32. `Database.restoreBackup(path, destination, options?)` validates the container and log before creating the destination directory. The returned `IntegrityReport` always describes persisted data, with camelCase counters.

Backup files and restore directories **must not already exist**, including empty destination directories. Their parent directories must exist. A backup contains full history and preserves change cursors; it is not compaction. The backup container is distinct from `cedro.log` and cannot be opened directly as a database. Restored indexes must be recreated. CRC32 detects accidental corruption; backups are not encrypted or cryptographically authenticated.

Verification and backup hold the database lock and replay documents into additional memory. Transfers use a bounded copy buffer, but the replayed state, one frame and allocator overhead remain outside the encoded-content limits. Keep backups unchanged while restoring. See the runnable [offline application example](https://github.com/uppy19d0/cedrodb/blob/main/bindings/node/examples/offline-backup.mjs).

## API reference

| API | Result / behavior |
| --- | --- |
| `new Database(path, options?)` | Open or create a database directory |
| `db.collection(name)` | Obtain a collection handle |
| `db.queue(name)` | Obtain a durable local queue handle |
| `db.agent(namespace)` | Obtain a revision-protected JSON agent state handle |
| `collection.put(key, value)` / `delete(key)` | Commit one mutation |
| `collection.get(key)` | JSON value, or `undefined` when absent |
| `collection.putMany([{ key, value }, ...])` | Commit one atomic batch |
| `db.batch(operations)` | Commit puts/deletes across collections |
| `db.conditionalBatch({ conditions, operations })` | `{ applied, failedCondition, cursor }`; commit only if every guard matches |
| `collection.find(query?)` | One bounded array of `{ key, value }` records |
| `collection.findPage(query?)` | `{ records, nextAfter?, scanned, usedIndex? }` |
| `collection.search(query)` | Exact text/vector/hybrid `SearchPage` |
| `collection.createIndex(field)` | Build an equality index for this session |
| `collection.compareExchange(key, expected, replacement)` | Boolean conditional update result |
| `collection.aggregate(field)` | `{ count, sum, min, max }` over a top-level numeric field |
| `db.changes({ since?, limit? })` | `{ transactions, nextCursor, hasMore }` |
| `db.stats()` | Version, records, collections, contentBytes, logBytes, latestCursor, indexes |
| `db.diagnostics()` | Stats, configured limits, collection/index details, session counters and status flags |
| `collection.profileQuery(query?)` | `{ page, elapsedMicros, plan }` for one executed query |
| `collection.profileSearch(query)` | `{ page, elapsedMicros, plan }` for one executed exact search |
| `db.verify()` / `db.backup(path)` | `IntegrityReport` |
| `Database.restoreBackup(file, newDirectory, options?)` | `IntegrityReport` |
| `agent.get(threadId)` | Latest `AgentCheckpoint` or `undefined` |
| `agent.checkpoint(threadId, state, options?)` | `{ applied, cursor, checkpoint }`; checkpoint and application writes commit together |
| `db.close()` | Release the native handle and exclusive file lock; idempotent |

Complete TypeScript declarations are included for both ESM and CommonJS, including typed `Queue`, `Job`, `LeasedJob`, `AgentStore`, `AgentCheckpoint`, `SearchPage`, `Diagnostics`, `QueryProfile` and `SearchProfile` exports. Profiles preserve the collection's declared document type. Calls on retained collections, queues and agent stores fail after their database closes.

## Python interoperability and JSON rules

Node.js and Python use the same storage engine and log format. Close the Node handle before opening the same directory in Python:

```python
from cedrodb import Database

with Database("./app-data") as db:
    print(db.collection("inspections").get("001"))
```

Only plain JSON objects, dense standard arrays, strings, booleans, null and finite numbers within `±Number.MAX_SAFE_INTEGER` are accepted by the Node API. `BigInt`, unsafe numbers, document `undefined`, accessors, custom class instances, cycles, serialization hooks and unpaired Unicode surrogates are rejected. Use strings for larger identifiers. Reads from a database written by Rust/Python also reject out-of-range numbers before conversion to JavaScript, preventing silent integer rounding. Python can store a wider integer range, so applications sharing data should follow the Node range.

Structural equality ignores object property order. Exactly representable integral floats equal integers within the core's safe numeric range, including inside nested values; Python `1.0` can match JavaScript `1`. Booleans remain distinct from numbers. Decimal calculations retain normal IEEE 754 limitations. `aggregate()` uses floating-point arithmetic and supports a top-level field, not a nested JSON Pointer.

## Limits and migration from 0.2–0.6

The default limits are 1 MiB per document, 16 MiB per transaction frame and 64 MiB of encoded live content. Override them with `maxDocumentBytes`, `maxFrameBytes` and `maxStateBytes`. These are encoded-content limits, not a cap on total process RAM: all active documents, indexes, transaction offsets, results and verification replay consume memory. JSON nesting is limited to 64 levels.

The append-only log has no compaction or retention yet. SQL, MVCC, automatic replication, encryption and concurrent multi-process ownership of a directory are not implemented. Do not edit, replace or remove an open database's log. There are no comparative benchmark claims against SQLite, MDBX or DuckDB.

Version 0.3 added `verify()`, `backup()` and `Database.restoreBackup()`. Version 0.4 added `conditionalBatch()` for guarded changes across collections. Version 0.5 added the `Queue` API. Version 0.6 adds exact retrieval and revision-protected agent checkpoints with schema-versioned records stored through the same engine. These additions preserve the earlier document/query API and log format, so existing directories open directly. Ordinary opening still recovers an incomplete final append according to the engine's recovery rules; `verify()` is strict and never repairs a log. A backup container must be restored through the backup API rather than copied over a live log. Recreate session indexes after opening the restored directory, and resume the changefeed from a cursor belonging to that restored history.

Version 0.7 adds `diagnostics()`, `profileQuery()` and `profileSearch()` without changing the on-disk format or existing query/search results. No data migration is required. Metrics describe only the current open session and are not saved in backups. Existing workloads do not acquire new persisted metadata.

The `_cedro.jobs.` collection prefix became reserved in 0.5; `_cedro.agents.` becomes reserved in 0.6. Direct application writes to either prefix are rejected. If an earlier application used that prefix for its own collections, migrate queue-prefix records with version 0.4, or agent-prefix records with version 0.5, before enabling the corresponding feature under those names. Ordinary application collections retain their existing behavior.

## Build and test

From the repository checkout:

```sh
cd bindings/node
npm run build
npm test
npm pack
node examples/offline-backup.mjs
node examples/guarded-inventory.mjs
node examples/local-worker.mjs
node examples/agent-memory.mjs
node examples/profile-indexes.mjs
# Inspect the existing directory printed by an example:
node examples/diagnostics.mjs /path/to/existing-database
```

The build vendors only the core `Cargo.toml`, `Cargo.lock`, `src/` and license into `core/`. Packed packages include those sources plus locked Rust dependencies and build without a path outside the package. Build native artifacts separately for each platform; a successful source build on one platform does not establish compatibility with another.
