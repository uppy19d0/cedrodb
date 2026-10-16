# Application recipes

These patterns do not promise distributed synchronization, exactly-once delivery or production readiness.

## Offline forms and desktop upgrades

Save forms by explicit application keys and group related metadata into one batch. Keep large attachment bytes outside the document store. Before changing document schemas, verify and back up the database, restore into a fresh directory for rehearsal, and validate application invariants. See [recovery instructions](backups.md), [Node example](../bindings/node/examples/offline-backup.mjs) and [Python example](../bindings/python/examples/offline_forms.py).

Schema migration, attachment storage, encryption and synchronization belong to the application. Live documents stay in RAM; the default encoded-state limit is 64 MiB.

## Local transactional outbox

Write business data and an event in one batch. Read `changes` after a saved cursor, deliver outbox operations using a stable event ID, then save the checkpoint after the complete transaction. A crash between delivery and checkpoint persistence may cause another delivery; deduplicate at the destination.

See [Node quickstart](../bindings/node/examples/quickstart.mjs) and [Python outbox](../bindings/python/examples/atomic_outbox.py). The engine does not supply HTTP delivery, external acknowledgements or a background service.

## Small operational catalogs

Use JSON Pointer filters, create equality indexes for selective lookups and paginate by key instead of cloning entire collections. Recreate indexes after reopening. Concurrent writes may change separate pages; they are not a common snapshot.

The application owns validation, user permissions and memory sizing. Do not load data exceeding configured limits or available RAM.

## Inventory reservations and request idempotency

Use a conditional batch to require an unchanged stock document and an absent order key, then update stock, create the order and record an outbox event in one frame. Contenders using the same expected stock cannot both reserve the last item. A stable order key rejects replayed requests. See [the full contract and example](conditional-batches.md). The application still owns stock rules and conflict handling.

## Background reports and deferred processing

Enqueue a stable request ID, claim it with a lease and acknowledge the current token together with the generated document and an outbox event. Restarted workers can reclaim expired attempts; retry limits move persistent failures to dead state for inspection and explicit redrive. Use a consistent clock and periodically recover expired final attempts. [Complete queue lifecycle and limits](jobs.md).

Queue scans are O(n), jobs remain in RAM and there is no background service or automatic retention. External calls still need idempotency. See [the roadmap](roadmap.md) for work remaining before 1.0.

## Local assistants and resumable agent state

Keep application-managed conversation/checkpoint state in `db.agent(namespace)`. Save a checkpoint and ordinary result/outbox writes under an expected revision, then reopen by the same thread ID after restart. A stale continuation receives a conflict and writes nothing. Store model/version metadata on memory documents and use text, supplied embeddings or hybrid retrieval with prefilters and an explicit scan budget.

See the [agent contracts](agents.md), [Node example](../bindings/node/examples/agent-memory.mjs), [Python example](../bindings/python/examples/agent_memory.py) and [ecosystem research](ai-research.md). The application owns embeddings, authenticated tool approvals, external idempotency and framework serialization. This is checkpoint storage, not automatic workflow/model resumption.
