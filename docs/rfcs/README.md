# CedroDB RFCs

An RFC is a short design document written before code, for changes that are hard to undo once released.

## When an RFC is needed

Write one when a change does any of the following:

- changes the on-disk format (log, backup, any new file) or the recovery rule;
- weakens or redefines a documented guarantee (durability, ordering, cursors, fencing tokens);
- adds a public API whose shape would be a compatibility commitment;
- touches the commit path in `src/lib.rs` (`persist_operations`, `recover`);
- needs a measurement plan before its benefit can be claimed.

Bug fixes, internal refactors with identical behavior, and docs do not need one. The list of known RFC-worthy items is section 12 of [production-readiness](../architecture/production-readiness.md).

## Process

1. Copy the template below to `NNNN-short-name.md` (next free number) and open a PR with status `Draft`.
2. Discuss in the PR. Unresolved points go under "Open questions", not into silence.
3. Status moves to `Accepted` only when the maintainer says so in the PR. Acceptance is not a promise of a release date.
4. Implementation PRs link the RFC. If the implementation diverges, update the RFC in the same PR.
5. Claims about performance in an RFC are hypotheses until a measurement is linked.

## Status values

| Status | Meaning |
| --- | --- |
| Draft | Under discussion; nothing is decided |
| Accepted | Design agreed; implementation may start (usually behind an experimental flag) |
| Implemented | Shipped; the RFC records the final behavior and the version |
| Rejected | Decided against; kept so the reasoning is not lost |
| Superseded | Replaced by a later RFC (link it) |

## Template

```
# RFC NNNN: Title
Status: Draft | Date | Author
## Summary
## Motivation (what problem, for whom, evidence)
## Non-goals
## Design (format, semantics, recovery, API)
## Compatibility (old readers, old data, bindings)
## Failure and crash matrix
## Test and measurement plan
## Alternatives considered
## Open questions
## Rollout
```

## Index

| RFC | Title | Status |
| --- | --- | --- |
| [0001](0001-relaxed-durability.md) | Relaxed durability mode | Draft |
| [0002](0002-snapshot-checkpoint.md) | Snapshot / checkpoint file for faster reopen | Draft |
