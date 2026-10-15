# Guarded inventory and multi-document decisions

A read followed by an unconditional write does not protect a business decision against another writer. `conditionalBatch` checks the complete expected documents and writes all changes under one engine mutex. Use it to reserve stock, create an order once and record an outbox event together.

```js
const inventory = db.collection('inventory');
const expected = inventory.get('sku:1');
if (expected && expected.available > 0) {
  const result = db.conditionalBatch({
    conditions: [
      { kind: 'absent', collection: 'orders', key: 'request:123' },
      { kind: 'equals', collection: 'inventory', key: 'sku:1', value: expected },
    ],
    operations: [
      { kind: 'put', collection: 'inventory', key: 'sku:1',
        value: { ...expected, available: expected.available - 1, revision: expected.revision + 1 } },
      { kind: 'put', collection: 'orders', key: 'request:123', value: { sku: 'sku:1', quantity: 1 } },
      { kind: 'put', collection: 'outbox', key: 'event:request:123', value: { type: 'stock.reserved' } },
    ],
  });
  if (!result.applied) console.log('Re-read before deciding whether to retry:', result.failedCondition);
}
```

Python exposes `db.conditional_batch(conditions, operations)` and returns `applied`, `failed_condition` and `cursor`. Rust accepts slices of `Condition` and `ChangeOperation`.

## Contract

- Conditions inspect committed state before any operation in the batch. All must match; a mismatch writes nothing and does not advance the log cursor.
- `absent` requires a missing key. `equals` compares a whole JSON value; `value: null` requires a stored null. Missing and null are distinct.
- `failedCondition` / `failed_condition` is the zero-based first mismatch. `cursor` is the latest committed cursor after the call, including the batch on success.
- At most 64 conditions are accepted. Locations, expected JSON values and operations are validated even when a condition would not match. Size/depth/frame/state limits still apply.
- Empty operations can check conditions without creating a frame. Empty conditions make the write batch unconditional.
- An active staged write transaction is rejected. I/O failure follows the normal poisoned-handle/uncertain-commit contract.

This is optimistic validation of the documents you name, not MVCC or a snapshot of earlier reads. Include every dependency that matters to the decision. Equality alone cannot detect a value changed and changed back; include an application-managed increasing revision if that distinction matters.

The engine does not enforce stock rules, uniqueness of arbitrary JSON fields or a schema. Applications validate nonnegative stock, choose stable request/event keys and decide how to handle a conflicting or already-completed request. External delivery of the outbox event still needs deduplication.

The transaction log stores the resulting operations, not failed attempts or preconditions. Its existing format and changefeed behavior remain unchanged.
