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
import re
import time
from typing import (
    Any, Dict, Iterable, Iterator, List, Literal, Optional, Tuple, TypedDict, Union,
)

from ._native import CedroError, DatabaseClosedError, NativeDatabase

__version__ = "0.9.0"
__all__ = [
    "Database", "Collection", "Transaction", "CedroError", "DatabaseClosedError",
    "MISSING", "DELETE", "JSONValue", "Filter", "Record", "QueryPage",
    "ChangePage", "Stats", "IntegrityReport", "Condition", "AbsentCondition",
    "EqualsCondition", "ConditionalResult", "__version__",
    "Queue", "Job", "JobStatus", "EnqueueResult", "QueueStats", "RecoveryResult",
    "MAX_SAFE_INTEGER",
    "AgentStore", "AgentCheckpoint", "CheckpointResult", "AgentStatus",
    "SearchHit", "SearchPage", "SearchMode",
    "OperationMetrics", "SessionMetrics", "CollectionDiagnostics", "Diagnostics",
    "ContentLimits", "ProfilePlan", "QueryProfile", "SearchProfile",
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


SearchMode = Literal["text", "vector", "hybrid"]


class SearchHit(TypedDict):
    key: str
    value: JSONValue
    score: float
    vector_score: Optional[float]
    text_score: Optional[float]


class SearchPage(TypedDict):
    hits: List[SearchHit]
    scanned: int
    matched: int
    used_index: Optional[str]
    mode: SearchMode


AgentStatus = Literal["running", "waiting", "completed"]


class AgentCheckpoint(TypedDict):
    schema_version: int
    thread_id: str
    revision: int
    status: AgentStatus
    state: JSONValue
    metadata: Dict[str, JSONValue]
    updated_at: int


class CheckpointResult(TypedDict):
    applied: bool
    cursor: int
    checkpoint: Optional[AgentCheckpoint]


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


class OperationMetrics(TypedDict):
    attempts: int
    successes: int
    failures: int
    total_elapsed_micros: int
    max_elapsed_micros: int


class SessionMetrics(TypedDict):
    query: OperationMetrics
    search: OperationMetrics
    commit: OperationMetrics


class ContentLimits(TypedDict):
    max_document_bytes: int
    max_frame_bytes: int
    max_state_bytes: int


class CollectionDiagnostics(TypedDict):
    collection: str
    records: int
    content_bytes: int
    index_fields: List[str]


class Diagnostics(TypedDict):
    stats: Stats
    limits: ContentLimits
    collections: List[CollectionDiagnostics]
    session: SessionMetrics
    poisoned: bool
    writer_active: bool


class ProfilePlan(TypedDict):
    collection_records: int
    candidate_records: int
    scanned: int
    returned: int
    used_index: Optional[str]


class QueryProfile(TypedDict):
    page: QueryPage
    elapsed_micros: int
    plan: ProfilePlan


class SearchProfile(TypedDict):
    page: SearchPage
    elapsed_micros: int
    plan: ProfilePlan


class IntegrityReport(TypedDict):
    log_bytes: int
    transactions: int
    records: int
    content_bytes: int


class AbsentCondition(TypedDict):
    kind: Literal["absent"]
    collection: str
    key: str


class EqualsCondition(TypedDict):
    kind: Literal["equals"]
    collection: str
    key: str
    value: JSONValue


Condition = Union[AbsentCondition, EqualsCondition]


class ConditionalResult(TypedDict):
    applied: bool
    failed_condition: Optional[int]
    cursor: int


MAX_SAFE_INTEGER = 9_007_199_254_740_991
JobStatus = Literal["queued", "leased", "done", "dead"]


class Job(TypedDict):
    schema_version: int
    id: str
    payload: JSONValue
    status: JobStatus
    attempts: int
    max_attempts: int
    priority: int
    created_at: int
    available_at: int
    lease_until: Optional[int]
    lease_token: Optional[str]
    worker: Optional[str]
    last_error: Optional[str]


class EnqueueResult(TypedDict):
    inserted: bool
    job: Job


class QueueStats(TypedDict):
    queued: int
    leased: int
    done: int
    dead: int
    ready: int
    expired: int


class RecoveryResult(TypedDict):
    requeued: int
    dead: int
    has_more: bool


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


def _queue_integer(
    value: Any, label: str, minimum: int = 0, maximum: int = MAX_SAFE_INTEGER
) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError("{} must be an integer".format(label))
    if not minimum <= value <= maximum:
        raise ValueError("{} must be in [{}, {}]".format(label, minimum, maximum))
    return value


def _queue_now(now_ms: Optional[int]) -> int:
    return _queue_integer(time.time_ns() // 1_000_000 if now_ms is None else now_ms, "now_ms")


def _queue_name(value: Any) -> str:
    name = _string(value, "queue name")
    if re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", name) is None:
        raise ValueError("queue name must contain 1..128 ASCII letters, digits, _, . or -")
    return name


def _queue_text(value: Any, label: str, maximum_bytes: Optional[int] = None) -> str:
    text = _string(value, label)
    if not text or "\x00" in text:
        raise ValueError("{} must be nonempty and must not contain NUL".format(label))
    if maximum_bytes is not None and len(text.encode("utf-8")) > maximum_bytes:
        raise ValueError("{} exceeds {} UTF-8 bytes".format(label, maximum_bytes))
    return text


def _validate_queue_payload(value: Any) -> None:
    _validate_json(value)

    def portable(item: Any) -> None:
        if isinstance(item, bool):
            return
        if isinstance(item, (int, float)):
            if abs(item) > MAX_SAFE_INTEGER:
                raise OverflowError("queue payload numbers must be within +/-MAX_SAFE_INTEGER")
        elif isinstance(item, dict):
            for nested in item.values():
                portable(nested)
        elif isinstance(item, (list, tuple)):
            for nested in item:
                portable(nested)

    portable(value)


def _validate_agent_json(value: Any) -> None:
    """Agent envelopes can be exchanged with JavaScript without numeric loss."""
    _validate_json(value)

    def portable(item: Any) -> None:
        if isinstance(item, bool):
            return
        if isinstance(item, (int, float)) and abs(item) > MAX_SAFE_INTEGER:
            raise OverflowError("agent state and metadata numbers must be within +/-MAX_SAFE_INTEGER")
        if isinstance(item, dict):
            for nested in item.values():
                portable(nested)
        elif isinstance(item, (list, tuple)):
            for nested in item:
                portable(nested)

    portable(value)


def _finite_number(value: Any, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise TypeError("{} must be a number".format(label))
    try:
        number = float(value)
    except OverflowError:
        raise ValueError("{} must be finite".format(label)) from None
    if not math.isfinite(number):
        raise ValueError("{} must be finite".format(label))
    return number


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

    def queue(self, name: str) -> Queue:
        """Access a durable local job queue. Use an application-managed worker loop."""
        self._check_open()
        return Queue(self, _queue_name(name))

    def agent(self, namespace: str) -> AgentStore:
        """Access revision-fenced JSON checkpoints in an application namespace."""
        self._check_open()
        return AgentStore(self, namespace)

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

    def conditional_batch(
        self, conditions: Iterable[Condition], operations: Iterable[Mapping]
    ) -> ConditionalResult:
        """Atomically commit a batch only if all pre-state conditions match.

        Up to 64 absent/equals conditions are checked against the same committed
        state, before any write. A mismatch returns the zero-based failing
        condition index and unchanged cursor, without appending to the log.
        Missing records and equals(value=None) differ. Invalid requests raise
        before mutation, including when a condition would fail.
        """
        self._check_open()
        request = {"conditions": list(conditions), "operations": list(operations)}
        return json.loads(self._native.conditional_batch_json(_dumps(request)))

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

    def diagnostics(self) -> Diagnostics:
        """Inspect content limits, collection/index inventory and session metrics.

        No document values are returned. Timers measure native work, excluding
        Python serialization. Counters reset on reopen; only query, search and
        nonempty shared commit attempts are tracked. Conflicts and write errors
        rejected before the commit path do not increment commit failures.
        """
        self._check_open()
        return json.loads(self._native.diagnostics_json())

    def verify(self) -> IntegrityReport:
        """Verify the committed log and return its integrity report.

        This performs a synchronous read of the log and does not repair it.
        """
        self._check_open()
        return json.loads(self._native.verify_json())

    def backup(self, path: Union[str, os.PathLike]) -> IntegrityReport:
        """Write a verified backup file without overwriting an existing path.

        The backup preserves committed transaction history and changefeed
        cursors. Handle-local index definitions must be recreated after restore.
        """
        self._check_open()
        location = _string(os.fspath(path), "backup path")
        return json.loads(self._native.backup_json(location))

    @staticmethod
    def restore_backup(
        backup_path: Union[str, os.PathLike],
        destination_path: Union[str, os.PathLike],
        *,
        max_document_bytes: int = 1_048_576,
        max_frame_bytes: int = 16_777_216,
        max_state_bytes: int = 67_108_864,
    ) -> IntegrityReport:
        """Verify and restore a backup into a directory that does not yet exist.

        Returns a report, without opening or retaining a database handle. Use
        the same content limits when restoring and opening a custom-sized store.
        """
        backup = _string(os.fspath(backup_path), "backup path")
        destination = _string(os.fspath(destination_path), "destination path")
        options = (
            _positive_integer(max_document_bytes, "max_document_bytes"),
            _positive_integer(max_frame_bytes, "max_frame_bytes"),
            _positive_integer(max_state_bytes, "max_state_bytes"),
        )
        return json.loads(NativeDatabase.restore_backup_json(backup, destination, *options))


class AgentStore:
    """Durable JSON state with optimistic revision checks and atomic business writes.

    This is a framework-neutral store, not a LangGraph checkpointer. Namespaces
    organize records; the application remains responsible for authorization.
    """

    def __init__(self, database: Database, namespace: str) -> None:
        self._database = database
        self.namespace = _string(namespace, "agent namespace")
        if re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", self.namespace) is None:
            raise ValueError("agent namespace must contain 1..128 ASCII letters, digits, _, . or -")

    def _request(self, action: str, thread_id: str, **arguments: Any) -> Any:
        self._database._check_open()
        request = {
            "action": action, "namespace": self.namespace,
            "thread_id": _queue_text(thread_id, "thread_id", 4096), **arguments,
        }
        return json.loads(self._database._native.agent_json(_dumps(request)))

    def get(self, thread_id: str) -> Optional[AgentCheckpoint]:
        """Return the latest committed checkpoint, or None if the thread is absent."""
        return self._request("get", thread_id)

    def checkpoint(
        self,
        thread_id: str,
        state: JSONValue,
        *,
        expected_revision: Optional[int] = None,
        now_ms: Optional[int] = None,
        status: AgentStatus = "running",
        metadata: Optional[Dict[str, JSONValue]] = None,
        operations: Optional[Iterable[Mapping]] = None,
    ) -> CheckpointResult:
        """Commit state plus put/delete operations if the expected revision matches.

        expected_revision=None means the thread MUST be absent, not an
        unconditional update. On conflict no writes occur and checkpoint holds
        the current value. A success uses the durable transaction cursor as its
        revision. External effects still require application-level idempotency.
        """
        self._database._check_open()
        if expected_revision is not None:
            _queue_integer(expected_revision, "expected_revision", 1)
        _string(status, "status")
        if status not in ("running", "waiting", "completed"):
            raise ValueError("status must be running, waiting or completed")
        actual_metadata = {} if metadata is None else metadata
        if not isinstance(actual_metadata, dict):
            raise TypeError("metadata must be a JSON object")
        _validate_agent_json(state)
        _validate_agent_json(actual_metadata)
        return self._request(
            "checkpoint", thread_id, state=state, expected_revision=expected_revision,
            now_ms=_queue_now(now_ms), status=status, metadata=actual_metadata,
            operations=[] if operations is None else list(operations),
        )


class Queue:
    """Durable local jobs with delayed availability, leases and bounded retries.

    Queue payload numbers use +/-MAX_SAFE_INTEGER for lossless Node/Python
    interoperability. Ordinary collection documents retain their full integer
    range. Queue storage is reserved: do not mutate _cedro.jobs.* collections.
    Calls are synchronous and scans are O(n), without a background worker.
    """

    def __init__(self, database: Database, name: str) -> None:
        self._database = database
        self.name = _queue_name(name)

    def _request(self, action: str, **arguments: Any) -> Any:
        self._database._check_open()
        request = {"action": action, "name": self.name, **arguments}
        return json.loads(self._database._native.queue_json(_dumps(request)))

    def enqueue(
        self,
        id: str,
        payload: JSONValue,
        *,
        now_ms: Optional[int] = None,
        delay_ms: int = 0,
        max_attempts: int = 3,
        priority: int = 0,
    ) -> EnqueueResult:
        """Insert once by id; an existing queued/leased/done/dead job is returned."""
        self._database._check_open()
        _validate_queue_payload(payload)
        return self._request(
            "enqueue", id=_queue_text(id, "job id", 4096), payload=payload,
            now_ms=_queue_now(now_ms), delay_ms=_queue_integer(delay_ms, "delay_ms"),
            max_attempts=_queue_integer(max_attempts, "max_attempts", 1, 1000),
            priority=_queue_integer(priority, "priority", 0, 1000),
        )

    def claim(
        self, worker: str, *, now_ms: Optional[int] = None, lease_ms: int = 30_000
    ) -> Optional[Job]:
        """Lease ready work in priority order, or reclaim eligible expired work."""
        return self._request(
            "claim", worker=_queue_text(worker, "worker", 128), now_ms=_queue_now(now_ms),
            lease_ms=_queue_integer(lease_ms, "lease_ms", 1),
        )

    def ack(
        self,
        id: str,
        token: str,
        *,
        now_ms: Optional[int] = None,
        operations: Iterable[Mapping] = (),
    ) -> bool:
        """Complete an active lease plus optional business writes in one transaction.

        The token must match and the lease must still be active. Operations use
        put/delete dictionaries and cannot write to any _cedro.jobs.* collection.
        """
        return self._request(
            "ack", id=_queue_text(id, "job id", 4096), token=_queue_text(token, "lease token"),
            now_ms=_queue_now(now_ms), operations=list(operations),
        )

    def fail(
        self,
        id: str,
        token: str,
        error: str,
        *,
        now_ms: Optional[int] = None,
        retry_delay_ms: int = 0,
    ) -> bool:
        """Requeue an active lease after a delay, or mark exhausted attempts dead."""
        message = _queue_text(error, "error", 4096)
        return self._request(
            "fail", id=_queue_text(id, "job id", 4096), token=_queue_text(token, "lease token"),
            error=message, now_ms=_queue_now(now_ms),
            retry_delay_ms=_queue_integer(retry_delay_ms, "retry_delay_ms"),
        )

    def extend(
        self, id: str, token: str, *, now_ms: Optional[int] = None, lease_ms: int = 30_000
    ) -> bool:
        """Renew an active matching lease; stale or expired tokens return False."""
        return self._request(
            "extend", id=_queue_text(id, "job id", 4096), token=_queue_text(token, "lease token"),
            now_ms=_queue_now(now_ms), lease_ms=_queue_integer(lease_ms, "lease_ms", 1),
        )

    def get(self, id: str) -> Optional[Job]:
        return self._request("get", id=_queue_text(id, "job id", 4096))

    def stats(self, *, now_ms: Optional[int] = None) -> QueueStats:
        return self._request("stats", now_ms=_queue_now(now_ms))

    def recover_expired(
        self, *, now_ms: Optional[int] = None, limit: int = 10
    ) -> RecoveryResult:
        """Requeue expired leases or mark exhausted jobs dead in bounded batches."""
        return self._request(
            "recover_expired", now_ms=_queue_now(now_ms),
            limit=_queue_integer(limit, "limit", 1, 1000),
        )

    def retry_dead(self, id: str, *, now_ms: Optional[int] = None) -> bool:
        """Reset a dead job's attempts and make it ready again, preserving its id."""
        return self._request("retry_dead", id=_queue_text(id, "job id", 4096), now_ms=_queue_now(now_ms))


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

    def profile_query(
        self,
        filters: Optional[Iterable[Filter]] = None,
        *,
        limit: int = 100,
        after: Optional[str] = None,
    ) -> QueryProfile:
        """Execute a query once and return its page, plan and native microseconds.

        Counts as one ordinary query. Reading does not append a transaction.
        The plan describes this execution, including its keyset cursor.
        """
        self._database._check_open()
        _positive_integer(limit, "limit")
        if after is not None:
            _string(after, "after")
        request = {"filters": [] if filters is None else list(filters), "limit": limit, "after": after}
        return json.loads(self._database._native.profile_query_json(self.name, _dumps(request)))

    def _search_request(
        self,
        *,
        text: Optional[str] = None,
        vector: Optional[Iterable[float]] = None,
        vector_field: str = "embedding",
        text_fields: Optional[Iterable[str]] = None,
        filters: Optional[Iterable[Filter]] = None,
        limit: int = 10,
        max_scanned: int = 10_000,
        min_vector_score: Optional[float] = None,
    ) -> str:
        self._database._check_open()
        if text is not None:
            _string(text, "text")
        if vector is None:
            coordinates = None
        else:
            if isinstance(vector, (str, bytes, Mapping)):
                raise TypeError("vector must be an iterable of numbers")
            coordinates = [_finite_number(item, "vector coordinate") for item in vector]
        if isinstance(text_fields, (str, bytes, Mapping)):
            raise TypeError("text_fields must be an iterable of field strings")
        fields = ["text"] if text_fields is None else [
            _string(field, "text field") for field in text_fields
        ]
        request = {
            "text": text, "vector": coordinates,
            "vector_field": _string(vector_field, "vector_field"), "text_fields": fields,
            "filters": [] if filters is None else list(filters),
            "limit": _queue_integer(limit, "limit", 1, 1000),
            "max_scanned": _queue_integer(max_scanned, "max_scanned", 1, 100_000),
            "min_vector_score": None if min_vector_score is None else _finite_number(
                min_vector_score, "min_vector_score"
            ),
        }
        return _dumps(request)

    def search(
        self,
        *,
        text: Optional[str] = None,
        vector: Optional[Iterable[float]] = None,
        vector_field: str = "embedding",
        text_fields: Optional[Iterable[str]] = None,
        filters: Optional[Iterable[Filter]] = None,
        limit: int = 10,
        max_scanned: int = 10_000,
        min_vector_score: Optional[float] = None,
    ) -> SearchPage:
        """Rank prefiltered documents by BM25, exact cosine, or hybrid RRF.

        Supply your own embeddings. Exceeding max_scanned raises instead of
        returning partial rankings. Index definitions are local to this handle.
        """
        request = self._search_request(
            text=text, vector=vector, vector_field=vector_field, text_fields=text_fields,
            filters=filters, limit=limit, max_scanned=max_scanned,
            min_vector_score=min_vector_score,
        )
        return json.loads(self._database._native.search_json(self.name, request))

    def profile_search(
        self,
        *,
        text: Optional[str] = None,
        vector: Optional[Iterable[float]] = None,
        vector_field: str = "embedding",
        text_fields: Optional[Iterable[str]] = None,
        filters: Optional[Iterable[Filter]] = None,
        limit: int = 10,
        max_scanned: int = 10_000,
        min_vector_score: Optional[float] = None,
    ) -> SearchProfile:
        """Execute retrieval once and report its page, plan and native timing.

        The result includes document values, like search(). A failed retrieval
        raises the same error; no partial page or successful profile is returned.
        """
        request = self._search_request(
            text=text, vector=vector, vector_field=vector_field, text_fields=text_fields,
            filters=filters, limit=limit, max_scanned=max_scanned,
            min_vector_score=min_vector_score,
        )
        return json.loads(self._database._native.profile_search_json(self.name, request))

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
