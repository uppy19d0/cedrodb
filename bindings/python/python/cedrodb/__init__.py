"""A native, embedded CedroDB database. All persistence runs in Rust.

Documents are JSON values. Integers in [-2**63, 2**64 - 1] round-trip exactly.
Use MISSING to distinguish an absent record from a stored JSON null, and DELETE
as a compare-exchange replacement to remove a record.
"""

from __future__ import annotations

from collections.abc import Mapping
import json
import math
import os
from typing import (
    Any, Dict, Iterable, Iterator, List, Literal, Optional, Tuple, TypedDict, Union,
)

from ._native import CedroError, DatabaseClosedError, NativeDatabase

__version__ = "0.2.0"
__all__ = [
    "Database", "Collection", "Transaction", "CedroError", "DatabaseClosedError",
    "MISSING", "DELETE", "JSONValue", "Filter", "Record", "QueryPage",
    "ChangePage", "Stats", "__version__",
]

JSONValue = Union[None, bool, int, float, str, List["JSONValue"], Dict[str, "JSONValue"]]


class Filter(TypedDict):
    field: str
    op: Literal["eq", "ne", "gt", "gte", "lt", "lte", "exists"]
    value: JSONValue


class Record(TypedDict):
    key: str
    value: JSONValue


class QueryPage(TypedDict):
    records: List[Record]
    next_after: Optional[str]
    scanned: int
    used_index: Optional[str]


class ChangeOperation(TypedDict, total=False):
    kind: Literal["put", "delete"]
    collection: str
    key: str
    value: JSONValue


class ChangeTransaction(TypedDict):
    cursor: int
    operations: List[ChangeOperation]


class ChangePage(TypedDict):
    transactions: List[ChangeTransaction]
    next_cursor: int
    has_more: bool


class Stats(TypedDict):
    version: str
    records: int
    collections: int
    content_bytes: int
    log_bytes: int
    latest_cursor: int
    indexes: int


class Aggregate(TypedDict):
    count: int
    sum: float
    min: Optional[float]
    max: Optional[float]


class _Sentinel:
    __slots__ = ("_name",)

    def __init__(self, name: str) -> None:
        self._name = name

    def __repr__(self) -> str:
        return self._name


MISSING = _Sentinel("MISSING")
DELETE = _Sentinel("DELETE")


def _validate_json(value: Any, active: Optional[set] = None, depth: int = 0) -> None:
    """Prevent JSON coercions that would silently change user data."""
    if depth > 128:
        raise ValueError("JSON nesting exceeds the transport limit of 128")
    if value is None or isinstance(value, (str, bool)):
        return
    if isinstance(value, int):
        if not -(2**63) <= value <= 2**64 - 1:
            raise OverflowError("JSON integer must be in [-2**63, 2**64 - 1]")
        return
    if isinstance(value, float):
        if not math.isfinite(value):
            raise ValueError("JSON numbers must be finite; NaN and Infinity are unsupported")
        return
    if not isinstance(value, (dict, list, tuple)):
        raise TypeError("unsupported JSON value of type {}".format(type(value).__name__))
    if active is None:
        active = set()
    identity = id(value)
    if identity in active:
        raise ValueError("circular references are not JSON values")
    active.add(identity)
    try:
        if isinstance(value, dict):
            for key, item in value.items():
                if not isinstance(key, str):
                    raise TypeError("JSON object keys must be strings")
                _validate_json(item, active, depth + 1)
        else:
            for item in value:
                _validate_json(item, active, depth + 1)
    finally:
        active.remove(identity)


def _dumps(value: Any) -> str:
    _validate_json(value)
    return json.dumps(value, allow_nan=False, ensure_ascii=True, separators=(",", ":"))


def _string(value: Any, label: str) -> str:
    if not isinstance(value, str):
        raise TypeError("{} must be a string".format(label))
    return value


def _positive_integer(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError("{} must be an integer".format(label))
    if value <= 0:
        raise ValueError("{} must be positive".format(label))
    return value


class Database:
    """Exclusively open a database directory, creating it if necessary.

    Use a context manager or close() to release its process-level file lock.
    Limits cover encoded content, not Python/Rust allocator overhead. Active
    documents and indexes reside in memory; the log currently has no compaction.
    """

    def __init__(
        self,
        path: Union[str, os.PathLike],
        *,
        max_document_bytes: int = 1_048_576,
        max_frame_bytes: int = 16_777_216,
        max_state_bytes: int = 67_108_864,
    ) -> None:
        location = _string(os.fspath(path), "path")
        self._native = NativeDatabase(
            location,
            _positive_integer(max_document_bytes, "max_document_bytes"),
            _positive_integer(max_frame_bytes, "max_frame_bytes"),
            _positive_integer(max_state_bytes, "max_state_bytes"),
        )

    @property
    def closed(self) -> bool:
        return self._native.closed

    def _check_open(self) -> None:
        if self.closed:
            raise DatabaseClosedError("database is closed")

    def close(self) -> None:
        """Release the native database and its file lock. Repeated calls are safe."""
        self._native.close()

    def __enter__(self) -> Database:
        self._check_open()
        return self

    def __exit__(self, exc_type: Any, exc: Any, traceback: Any) -> bool:
        self.close()
        return False

    def collection(self, name: str) -> Collection:
        self._check_open()
        return Collection(self, _string(name, "collection name"))

    def batch(self, operations: Iterable[Mapping]) -> None:
        """Commit put/delete operations together, or leave all records unchanged.

        Example: [{"kind": "put", "collection": "orders", "key": "1",
                   "value": {"total": 20}}]. Delete operations omit value.
        The entire iterable and its JSON values are validated before staging.
        """
        self._check_open()
        encoded = _dumps(list(operations))
        self._native.batch_json(encoded)

    def transaction(self) -> Transaction:
        """Create a buffered write batch that commits on a successful with exit."""
        self._check_open()
        return Transaction(self)

    def changes(self, since: int = 0, limit: int = 100) -> ChangePage:
        """Read durable transactions after an exclusive cursor, including reopened history.

        Persist next_cursor only after handling the corresponding side effects.
        Delivery retries can repeat effects; consumers need their own deduplication.
        """
        self._check_open()
        if isinstance(since, bool) or not isinstance(since, int):
            raise TypeError("since must be an integer")
        if not 0 <= since <= 2**64 - 1:
            raise ValueError("since must be in [0, 2**64 - 1]")
        _positive_integer(limit, "limit")
        return json.loads(self._native.changes_json(since, limit))

    def stats(self) -> Stats:
        self._check_open()
        return json.loads(self._native.stats_json())


class Collection:
    """Named view over a database. Documents are any supported JSON value."""

    def __init__(self, database: Database, name: str) -> None:
        self._database = database
        self.name = _string(name, "collection name")

    def get(self, key: str, default: Any = None) -> Any:
        """Return a document or default. Pass MISSING to distinguish absence from null."""
        self._database._check_open()
        encoded = self._database._native.get_json(self.name, _string(key, "key"))
        return default if encoded is None else json.loads(encoded)

    def put(self, key: str, value: JSONValue) -> None:
        self._database._check_open()
        self._database._native.put_json(self.name, _string(key, "key"), _dumps(value))

    def delete(self, key: str) -> None:
        self._database._check_open()
        self._database._native.delete(self.name, _string(key, "key"))

    def put_many(
        self, items: Union[Mapping[str, JSONValue], Iterable[Tuple[str, JSONValue]]]
    ) -> None:
        """Write a mapping or iterable of (key, value) pairs as one atomic batch."""
        pairs = items.items() if isinstance(items, Mapping) else items
        self._database.batch([
            {"kind": "put", "collection": self.name, "key": _string(key, "key"), "value": value}
            for key, value in pairs
        ])

    def compare_exchange(
        self, key: str, expected: Any = MISSING, *, replacement: Any
    ) -> bool:
        """Atomically replace only if current JSON matches expected.

        expected=MISSING means absent; expected=None means stored null.
        replacement=DELETE removes the record; replacement=None stores null.
        False means a mismatch and does not append a transaction to the log.
        """
        self._database._check_open()
        if expected is DELETE:
            raise TypeError("use MISSING to expect an absent record")
        if replacement is MISSING:
            raise TypeError("use DELETE to delete a compare-exchange record")
        expected_json = None if expected is MISSING else _dumps(expected)
        replacement_json = None if replacement is DELETE else _dumps(replacement)
        return self._database._native.compare_exchange_json(
            self.name, _string(key, "key"), expected_json, replacement_json
        )

    def find_page(
        self,
        filters: Optional[Iterable[Filter]] = None,
        *,
        limit: int = 100,
        after: Optional[str] = None,
    ) -> QueryPage:
        """Return records ordered by key, plus paging and index-use metadata.

        Filters are combined with AND. Fields are top-level names or RFC 6901
        JSON Pointers such as /profile/city. Pages do not share
        a snapshot: writes between requests may affect subsequent results.
        """
        self._database._check_open()
        _positive_integer(limit, "limit")
        if after is not None:
            _string(after, "after")
        query = {"filters": list(filters) if filters is not None else [], "limit": limit, "after": after}
        return json.loads(self._database._native.query_json(self.name, _dumps(query)))

    def find(
        self,
        filters: Optional[Iterable[Filter]] = None,
        *,
        limit: int = 100,
        after: Optional[str] = None,
    ) -> List[Record]:
        """Return one page of {key, value} records. Use find_page for continuation."""
        return self.find_page(filters, limit=limit, after=after)["records"]

    def iter_records(
        self, filters: Optional[Iterable[Filter]] = None, *, page_size: int = 100
    ) -> Iterator[Record]:
        """Read bounded result pages until exhausted, without a snapshot guarantee."""
        predicates = list(filters) if filters is not None else []
        after = None
        while True:
            page = self.find_page(predicates, limit=page_size, after=after)
            yield from page["records"]
            after = page["next_after"]
            if after is None:
                return

    def create_index(self, field: str) -> None:
        """Create an in-memory equality index. Recreate it after reopening."""
        self._database._check_open()
        self._database._native.create_index(self.name, _string(field, "field"))

    def aggregate(self, field: str) -> Aggregate:
        """Count/sum/min/max top-level numeric fields using f64 arithmetic.

        Aggregates can lose integer precision above 2**53; stored JSON stays exact.
        """
        self._database._check_open()
        return json.loads(self._database._native.aggregate_json(self.name, _string(field, "field")))


class Transaction:
    """Buffered writes committed atomically, with rollback on a with-block exception.

    Reads continue to see committed data. This is a write batch, without read
    isolation, read-your-writes or conflict detection between read and commit.
    Document values are copied at staging time, so later mutations do not alter
    staged operations. Native errors during commit leave their outcome subject
    to the core's I/O recovery rules.
    """

    def __init__(self, database: Database) -> None:
        self._database = database
        self._operations: List[Dict[str, Any]] = []
        self._finished = False

    def _check_active(self) -> None:
        self._database._check_open()
        if self._finished:
            raise CedroError("transaction has already finished")

    def put(self, collection: str, key: str, value: JSONValue) -> Transaction:
        self._check_active()
        operation = {
            "kind": "put", "collection": _string(collection, "collection name"),
            "key": _string(key, "key"), "value": value,
        }
        self._operations.append(json.loads(_dumps(operation)))
        return self

    def delete(self, collection: str, key: str) -> Transaction:
        self._check_active()
        self._operations.append({
            "kind": "delete", "collection": _string(collection, "collection name"),
            "key": _string(key, "key"),
        })
        return self

    def commit(self) -> None:
        self._check_active()
        self._finished = True
        operations, self._operations = self._operations, []
        self._database.batch(operations)

    def rollback(self) -> None:
        self._finished = True
        self._operations.clear()

    def __enter__(self) -> Transaction:
        self._check_active()
        return self

    def __exit__(self, exc_type: Any, exc: Any, traceback: Any) -> bool:
        if exc_type is not None:
            self.rollback()
        elif not self._finished:
            self.commit()
        return False
