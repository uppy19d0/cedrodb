# CedroDB for Python

An embedded Rust document database with a native Python API. Store JSON locally,
commit changes across collections atomically, query indexed fields, and replay
durable transactions to drive local automation. No server or subprocess runs.

**Experimental v0.2.0.** This package is prepared for publication; do not assume
the `cedrodb` name/version is already available on PyPI. Install a release wheel
matching your platform or the self-contained source archive. Building from
source requires Rust 1.85+, a C linker and Python 3.9+; a compatible wheel needs
no Rust toolchain at runtime. Wheels use CPython's stable ABI for Python 3.9+.

```sh
python -m pip install ./cedrodb-0.2.0.tar.gz
```

## Documents and indexed queries

```python
from cedrodb import Database

with Database("./app.cedro") as db:
    orders = db.collection("orders")
    orders.put_many({
        "order:1": {"status": "paid", "total": 120, "customer": {"city": "Santo Domingo"}},
        "order:2": {"status": "pending", "total": 35},
    })
    orders.create_index("status")
    page = orders.find_page(
        [{"field": "status", "op": "eq", "value": "paid"}], limit=20
    )
    print(page["records"])       # [{"key": "order:1", "value": {...}}]
    print(page["used_index"])    # status
    print(orders.get("order:1"))
```

Use `find()` for a single list of `{key, value}` records, `find_page()` for
metadata (`next_after`, `scanned`, `used_index`), or `iter_records(page_size=100)`
to iterate bounded result pages. `limit`/`page_size` must be 1..1000. Pass the
previous page's `next_after` back as `after` to continue. Pages are ordered by key
and do not share a snapshot; writes between requests may change later results.

Filters are ANDed. Supported operations: `eq`, `ne`, `gt`, `gte`, `lt`, `lte`,
and `exists` (boolean value). A field is a top-level name or an RFC 6901 JSON
Pointer, for example `/customer/city`. Missing fields differ from JSON null;
use `exists` to test absence. Indexes serve equality filters, stay consistent
with writes, and must be recreated after reopening.

## Atomic outbox and durable changefeed

Write the application document and the event in **one durable transaction**.
This prevents committing an order without its corresponding event.

```python
with Database("./app.cedro") as db:
    with db.transaction() as tx:
        tx.put("orders", "order:3", {"status": "paid", "total": 90})
        tx.put("outbox", "order:3:paid", {
            "event_id": "order:3:paid", "type": "order.paid", "order_id": "order:3"
        })

    page = db.changes(since=0, limit=100)
    for transaction in page["transactions"]:
        for operation in transaction["operations"]:
            if operation["collection"] == "outbox" and operation["kind"] == "put":
                print(operation["value"])
    # Save page["next_cursor"] in your application only after processing succeeds.
```

The feed survives close/reopen and includes complete put/delete operations in
transaction order. `since` is an exclusive transaction cursor, starting at zero;
it belongs to this database history. `limit` counts transactions (1..1000), not
individual operations. `has_more` indicates that another page is available.
The feed also caps the encoded frame bytes per page at `max_frame_bytes + 12`,
always returning a whole transaction. Decoded Python objects use extra memory.
Persist checkpoints and deduplicate external effects in your application:
crashing after an effect but before its checkpoint can cause delivery again.
The library does not promise exactly-once delivery or provide a background worker.

`db.batch([...])` is the direct alternative to a context manager:

```python
db.batch([
    {"kind": "put", "collection": "orders", "key": "order:4", "value": {"total": 10}},
    {"kind": "delete", "collection": "drafts", "key": "order:4"},
])
```

`db.transaction()` buffers writes and snapshots each staged value. Exiting
normally commits one batch; an exception rolls it back. Reads observe committed
data, without read-your-writes, snapshot isolation or automatic conflict checks.

## Conditional updates without confusing null and missing

```python
from cedrodb import DELETE, MISSING

jobs = db.collection("jobs")
created = jobs.compare_exchange("job:1", expected=MISSING, replacement={"state": "ready"})
claimed = jobs.compare_exchange(
    "job:1", expected={"state": "ready"}, replacement={"state": "running"}
)
removed = jobs.compare_exchange("nullable", expected=None, replacement=DELETE)
missing = jobs.get("unknown", MISSING) is MISSING
```

`replacement` is required. `None` stores JSON null; `DELETE` removes a record.
`MISSING` as expected means the key must not exist. A mismatch returns `False`
without appending a transaction. Conditional updates compare the full JSON value.

## API and limits

- `Database(path, *, max_document_bytes=1_048_576, max_frame_bytes=16_777_216,
  max_state_bytes=67_108_864)`: open/create a directory.
- `db.collection(name)`: obtain a collection.
- Collection: `put`, `get`, `delete`, `put_many`, `find`, `find_page`,
  `iter_records`, `create_index`, `compare_exchange`, `aggregate`.
- Database: `batch`, `transaction`, `changes`, `stats`, `close`, `closed`.
- `close()` is idempotent, releases the native file lock immediately, and makes
  existing collection handles fail with `DatabaseClosedError`.
- Core failures raise `CedroError`. Unsupported JSON raises `TypeError` or
  `ValueError`; integers outside `[-2**63, 2**64 - 1]` raise `OverflowError`.

Integer values in that range round-trip exactly, including values larger than
JavaScript's safe integer range. NaN, infinity, cycles, non-string object keys
and unsupported objects are rejected before mutation. JSON nesting is limited
by the core to 64 levels. Python JSON transport always uses `allow_nan=False`.
`aggregate(field)` uses floating-point arithmetic and may lose integer precision
above 2**53; stored documents are preserved.

Only one open database handle/process owns a directory. All active documents,
indexes and a transaction-offset table are in memory. Encoded-content limits
exclude allocator overhead and index memory. The append-only log has no
compaction or retention yet. Index definitions are handle-local. This is an
experimental engine without SQL, MVCC, replication or encryption.

## Build and verify a checkout

```sh
python -m venv .venv
# Activate the virtual environment using your platform's activation command.
python -m pip install 'maturin>=1.15,<2'
python scripts/prepare_core.py
maturin build --release --out dist
maturin sdist --out dist
python -m pip install ./dist/<your-platform-wheel>.whl
python -m unittest discover -s tests -v
python scripts/verify_sdist.py dist/cedrodb-0.2.0.tar.gz
python examples/atomic_outbox.py
```

`prepare_core.py` copies only the core's Cargo manifest, license and Rust source
into ignored `core/`. The source distribution includes these files and builds
without the repository or any `../..` dependency. Regenerate this copy after
changing the Rust engine. Keep `Cargo.lock` for reproducible dependency versions.
Build wheels separately for each operating system and CPU architecture.

Project: <https://github.com/uppy19d0/cedrodb> ·
Demo: <https://www.ltavarez.me/labs/cedrodb>
