'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { Database } = require('..');

function fixture(t, options) {
  const directory = fs.mkdtempSync(path.join(tmpdir(), 'cedrodb-node-conditional-'));
  const database = new Database(directory, options);
  t.after(() => { database.close(); fs.rmSync(directory, { recursive: true, force: true }); });
  return { database, directory };
}

test('conditional inventory and outbox commit together, maintain indexes and persist on reopen', (t) => {
  const { database, directory } = fixture(t);
  database.batch([
    { kind: 'put', collection: 'inventory', key: 'item:1', value: { quantity: 3, version: 1 } },
    { kind: 'put', collection: 'drafts', key: 'order:42', value: true },
  ]);
  database.collection('inventory').createIndex('quantity');
  const request = {
    conditions: [
      { kind: 'equals', collection: 'inventory', key: 'item:1', value: { version: 1, quantity: 3 } },
      { kind: 'absent', collection: 'orders', key: 'order:42' },
    ],
    operations: [
      { kind: 'put', collection: 'inventory', key: 'item:1', value: { quantity: 2, version: 2 } },
      { kind: 'put', collection: 'orders', key: 'order:42', value: { state: 'reserved' } },
      { kind: 'put', collection: 'outbox', key: 'order:42:reserved', value: { type: 'order.reserved' } },
      { kind: 'delete', collection: 'drafts', key: 'order:42' },
    ],
  };
  assert.deepEqual(database.conditionalBatch(request), { applied: true, failedCondition: null, cursor: 2 });
  assert.equal(database.collection('drafts').get('order:42'), undefined);
  const indexed = database.collection('inventory').findPage({ filters: [{ field: 'quantity', op: 'eq', value: 2 }] });
  assert.equal(indexed.usedIndex, 'quantity');
  assert.equal(indexed.records.length, 1);
  assert.equal(database.changes({ since: 1 }).transactions[0].operations.length, 4);
  database.close();
  const reopened = new Database(directory);
  try {
    assert.deepEqual(reopened.collection('inventory').get('item:1'), { quantity: 2, version: 2 });
    assert.deepEqual(reopened.collection('orders').get('order:42'), { state: 'reserved' });
    assert.deepEqual(reopened.collection('outbox').get('order:42:reserved'), { type: 'order.reserved' });
    assert.deepEqual(reopened.conditionalBatch(request), { applied: false, failedCondition: 0, cursor: 2 });
    assert.equal(reopened.verify().transactions, 2);
  } finally { reopened.close(); }
});

test('a mismatching guard reports its zero-based index and never writes a partial batch', (t) => {
  const { database } = fixture(t);
  database.collection('inventory').put('item:1', { quantity: 2 });
  const before = database.stats();
  const result = database.conditionalBatch({
    conditions: [
      { kind: 'absent', collection: 'orders', key: 'order:1' },
      { kind: 'equals', collection: 'inventory', key: 'item:1', value: { quantity: 3 } },
    ],
    operations: [
      { kind: 'put', collection: 'orders', key: 'order:1', value: { total: 10 } },
      { kind: 'delete', collection: 'inventory', key: 'item:1' },
    ],
  });
  assert.deepEqual(result, { applied: false, failedCondition: 1, cursor: before.latestCursor });
  assert.equal(database.collection('orders').get('order:1'), undefined);
  assert.deepEqual(database.collection('inventory').get('item:1'), { quantity: 2 });
  assert.equal(database.stats().logBytes, before.logBytes);
  assert.deepEqual(database.changes({ since: before.latestCursor }).transactions, []);
});

test('all guards inspect committed state before any operation is applied', (t) => {
  const { database } = fixture(t);
  database.collection('state').put('version', 1);
  const success = database.conditionalBatch({
    conditions: [
      { kind: 'equals', collection: 'state', key: 'version', value: 1 },
      { kind: 'absent', collection: 'state', key: 'new' },
    ],
    operations: [
      { kind: 'put', collection: 'state', key: 'version', value: 2 },
      { kind: 'put', collection: 'state', key: 'new', value: true },
    ],
  });
  assert.equal(success.applied, true);
  const cursor = success.cursor;
  const impossible = database.conditionalBatch({
    conditions: [
      { kind: 'absent', collection: 'state', key: 'future' },
      { kind: 'equals', collection: 'state', key: 'future', value: true },
    ],
    operations: [{ kind: 'put', collection: 'state', key: 'future', value: true }],
  });
  assert.deepEqual(impossible, { applied: false, failedCondition: 1, cursor });
  assert.equal(database.collection('state').get('future'), undefined);
});

test('equals null and absent guards remain distinct', (t) => {
  const { database } = fixture(t);
  database.collection('c').put('nullable', null);
  const check = (condition) => database.conditionalBatch({ conditions: [condition], operations: [] });
  assert.equal(check({ kind: 'absent', collection: 'c', key: 'nullable' }).applied, false);
  assert.equal(check({ kind: 'equals', collection: 'c', key: 'missing', value: null }).applied, false);
  assert.equal(check({ kind: 'equals', collection: 'c', key: 'nullable', value: null }).applied, true);
  assert.equal(check({ kind: 'absent', collection: 'c', key: 'missing' }).applied, true);
  assert.equal(database.stats().latestCursor, 1); // Matching empty batches are no-ops.
});

test('validation failures roll back every operation and do not advance the cursor', (t) => {
  const { database } = fixture(t, { maxDocumentBytes: 40 });
  database.collection('c').put('guard', 1);
  assert.throws(() => database.conditionalBatch({
    conditions: [{ kind: 'equals', collection: 'c', key: 'guard', value: 1 }],
    operations: [
      { kind: 'put', collection: 'c', key: 'first', value: true },
      { kind: 'put', collection: 'c', key: 'oversized', value: { text: 'x'.repeat(100) } },
    ],
  }), /too large/);
  assert.equal(database.collection('c').get('first'), undefined);
  assert.equal(database.stats().latestCursor, 1);
  assert.throws(() => database.conditionalBatch({
    conditions: [{ kind: 'equals', collection: 'c', key: 'guard', value: 99 }],
    operations: [{ kind: 'put', collection: 'c', key: 'oversized', value: { text: 'x'.repeat(100) } }],
  }), /too large/); // Invalid input is rejected even if a guard would mismatch.
  assert.equal(database.stats().latestCursor, 1);
});

test('conditions use core numeric equality consistently with other language representations', (t) => {
  const { database, directory } = fixture(t);
  database.close();
  const { NativeDatabase } = require('../native.cjs');
  const native = new NativeDatabase(directory, '{}');
  try {
    native.batch('[{"kind":"put","collection":"c","key":"n","value":{"nested":[1.0,-0.0],"flag":true}}]');
  } finally { native.close(); }
  const reopened = new Database(directory);
  try {
    const result = reopened.conditionalBatch({
      conditions: [{ kind: 'equals', collection: 'c', key: 'n', value: { flag: true, nested: [1, 0] } }],
      operations: [{ kind: 'put', collection: 'c', key: 'matched', value: true }],
    });
    assert.equal(result.applied, true);
    const booleanMismatch = reopened.conditionalBatch({
      conditions: [{ kind: 'equals', collection: 'c', key: 'matched', value: 1 }], operations: [],
    });
    assert.equal(booleanMismatch.applied, false);
  } finally { reopened.close(); }
});

test('conditionalBatch enforces strict JSON, request shapes and a 64-condition limit', (t) => {
  const { database } = fixture(t);
  const absent = { kind: 'absent', collection: 'c', key: 'a' };
  const invalid = [
    undefined,
    { conditions: [] },
    { conditions: [], operations: [], unknown: true },
    { conditions: [{ ...absent, value: undefined }], operations: [] },
    { conditions: [{ kind: 'equals', collection: 'c', key: 'a' }], operations: [] },
    { conditions: [{ kind: 'equals', collection: 'c', key: 'a', value: 1n }], operations: [] },
    { conditions: [{ kind: 'equals', collection: 'c', key: 'a', value: Number.MAX_SAFE_INTEGER + 1 }], operations: [] },
    { conditions: [null], operations: [] },
    { conditions: new Array(1), operations: [] },
    { conditions: [], operations: [{ kind: 'put', collection: 'c', key: 'a', value: NaN }] },
    { conditions: Array.from({ length: 65 }, () => absent), operations: [] },
  ];
  for (const request of invalid) assert.throws(() => database.conditionalBatch(request));
  assert.deepEqual(database.conditionalBatch({ conditions: Array.from({ length: 64 }, () => absent), operations: [] }), {
    applied: true, failedCondition: null, cursor: 0,
  });
  assert.equal(database.stats().latestCursor, 0);
  database.close();
  assert.throws(() => database.conditionalBatch({ conditions: [], operations: [] }), /closed/);
});
