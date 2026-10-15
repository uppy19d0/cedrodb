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

## Next recipe

Durable local jobs are planned for 0.5. See [the release roadmap](roadmap.md).
