# Compatibility and upgrades

## 0.2 to 0.3

The transaction log format is unchanged. Existing Node/Python methods remain available; 0.3 adds integrity and backup/restore APIs. There is no automatic document-schema transformation.

Close a database before opening its directory from another process or language. Preserve a tested copy before upgrades. A backup container is not a raw `cedro.log`; restore through the dedicated API into a new directory.

Recreate indexes after reopening or restoration. Complete backups retain transaction cursors, but a cursor after an older backup's final transaction is invalid in that copy. Numeric ranges, null/missing distinctions and floating-point aggregate behavior are unchanged.

## 0.3 to 0.4

The log remains compatible. Conditional batches are additive; conditions are evaluated before writes and are not stored as new log operations. Existing backups/restore and cursors keep their format. Guard all read dependencies explicitly and handle a mismatch before retrying.

## 0.4 to 0.5

The log and backup formats remain unchanged; queue methods are additive. The prefix `_cedro.jobs.` is now reserved for generic mutation APIs. If an existing application used it for ordinary collections, migrate those documents to another collection with 0.4 before enabling queue use. Existing documents remain readable; opening a log does not silently rewrite them.

Queue payloads use the shared JavaScript-safe numeric range in all runtimes; ordinary document numeric ranges are unchanged. Queue internal schema version is 1. Restoring/forking a history requires stopping old workers and reconciling external effects; tokens are not globally unique across forks.

## Pre-1.0 policy

Incompatible format/API changes must have release and migration notes. A stable backward/forward compatibility contract is a gate for 1.0, not a guarantee already established by the project.
## 0.5 to 0.6

The log/backup formats remain unchanged. Generic mutation APIs, including queue
ACK writes, now reserve `_cedro.agents.*` for typed checkpoint storage. If your
older application used that prefix for ordinary data, move it with 0.5 before
upgrading; existing records are not silently rewritten. Revisions are global
durable cursors, not per-thread counters. Agent statuses remain application data.

Floating-point parsing now preserves exact f64 round trips during replay. This
fixes possible one-ULP changes after reopening in earlier versions and keeps
vector-threshold decisions stable for the same stored values. Existing log
bytes do not require conversion. Numeric model/version compatibility still
belongs to the application.

Search is read-only and uses existing ordinary collections; no persistent search
index or embedding model is created. Recreate equality indexes after reopening.
See [agent/search contracts](agents.md) before choosing budgets and data scope.

## 0.6 to 0.7

The transaction log and backup containers are unchanged. New diagnostics and
profiling APIs are additive; ordinary query/search return shapes remain intact.
Session metrics start at zero on every open and are absent from backups. Existing
indexes remain handle-local and must still be recreated after reopening.

Profiling executes the query/search and increments its session counter once.
Do not wrap a profile call with an additional ordinary query unless two
executions are intended. The application owns metric export and retention.
