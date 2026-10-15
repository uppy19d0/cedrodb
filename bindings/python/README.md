# CedroDB for Python

**Keep application documents, events and recovery history in one local database.**
CedroDB is an embedded document engine written in Rust, with a native Python API.
A form submission and its outbox event can commit together, indexed queries find
pending work, and a durable changefeed lets your application resume processing
after restarting. Conditional batches reserve resources and append events only
when all expected documents still match. Integrity verification and portable
backups preserve the recovery history.

Use it for offline forms and inspections, desktop application state, local
automation, or small services that need JSON documents without running a database
server. The Python package calls the Rust engine directly; it does not spawn a
process or maintain a second Python storage implementation.

**Experimental software.** Active documents and indexes live in RAM, a directory
has one owning handle/process, and the log currently has no compaction. See the
limits below before choosing it for important data. There are no performance
claims or production-readiness guarantees.

## Installation

For the 0.4.0 release on PyPI:

```sh
python -m pip install cedrodb==0.4.0
```

PyPI publication requires the project's publisher configuration. If that version
is not available there yet, obtain its wheel or self-contained source archive
from [GitHub Releases](https://github.com/uppy19d0/cedrodb/releases), then install
the downloaded file:

```sh
python -m pip install ./cedrodb-0.4.0.tar.gz
```

A compatible wheel needs no Rust toolchain at runtime. Source installation
requires Python 3.9+, Rust 1.85+ and a working C linker. The source archive bundles
the Rust core; it builds independently of the repository.

The release wheel build matrix covers four platform/architecture combinations:

| Platform | Architecture | Python ABI |
| --- | --- | --- |
| Linux | x86_64 | CPython 3.9+ (`cp39-abi3`) |
| macOS | Apple Silicon / ARM64 | CPython 3.9+ (`cp39-abi3`) |
| macOS | Intel / x86_64 | CPython 3.9+ (`cp39-abi3`) |
| Windows | x86_64 | CPython 3.9+ (`cp39-abi3`) |

Check a release's assets for its available builds and platform tags. Other
architectures may require a source build; a Linux wheel does not imply support
for every Linux distribution or musl-based system.

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

## API reference

| Object | Methods |
| --- | --- |
| `Database` | `collection`, `batch`, `conditional_batch`, `transaction`, `changes`, `stats`, `verify`, `backup`, `close` |
| `Database.restore_backup` | Static restore with optional content-limit keywords |
| `Collection` | `put`, `get`, `delete`, `put_many`, `find`, `find_page`, `iter_records`, `create_index`, `compare_exchange`, `aggregate` |
| `Transaction` | `put(collection, key, value)`, `delete(collection, key)`, `commit`, `rollback`, context manager |

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
python scripts/verify_sdist.py dist/cedrodb-0.4.0.tar.gz
python examples/offline_forms.py
python examples/inventory_outbox.py
```

`prepare_core.py` copies only the core manifest, license and Rust source into
ignored `core/`. Regenerate that copy after engine changes. The sdist includes
those files and has no dependency on `../..` at installation. `verify_sdist.py`
rebuilds an extracted archive, installs the resulting wheel in an isolated target
and runs its tests. Keep `Cargo.lock` for reproducible dependency versions.

[Source](https://github.com/uppy19d0/cedrodb) ·
[Demo](https://www.ltavarez.me/labs/cedrodb) ·
[Issues](https://github.com/uppy19d0/cedrodb/issues)
