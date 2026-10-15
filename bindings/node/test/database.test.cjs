'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { mkdtempSync, rmSync } = require('node:fs');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { Database } = require('..');

function fixture(t, options) {
  const directory = mkdtempSync(path.join(tmpdir(), 'cedrodb-node-'));
  const database = new Database(directory, options);
  t.after(() => { database.close(); rmSync(directory, { recursive: true, force: true }); });
  return { database, directory };
}

test('writes persist across close/reopen; absent and JSON null stay distinct', (t) => {
  const { database, directory } = fixture(t);
  const documents = database.collection('documents');
  assert.equal(documents.get('missing'), undefined);
  documents.put('null', null);
  documents.put('unicode', { title: 'Cedro 🌲', enabled: true, items: [1, 1.5, null] });
  database.close();
  const reopened = new Database(directory);
  try {
    assert.equal(reopened.collection('documents').get('null'), null);
    assert.deepEqual(reopened.collection('documents').get('unicode'), { title: 'Cedro 🌲', enabled: true, items: [1, 1.5, null] });
  } finally { reopened.close(); }
});

test('atomic cross-collection batches produce complete durable change transactions', (t) => {
  const { database, directory } = fixture(t);
  database.batch([
    { kind: 'put', collection: 'orders', key: 'o1', value: { total: 25 } },
    { kind: 'put', collection: 'events', key: 'e1', value: { type: 'order-created' } },
  ]);
  database.collection('orders').delete('o1');
  const first = database.changes({ limit: 1 });
  assert.equal(first.transactions.length, 1);
  assert.equal(first.transactions[0].operations.length, 2);
  assert.equal(first.nextCursor, 1);
  assert.equal(first.hasMore, true);
  database.close();
  const reopened = new Database(directory);
  try {
    const second = reopened.changes({ since: first.nextCursor, limit: 1 });
    assert.deepEqual(second.transactions[0].operations, [{ kind: 'delete', collection: 'orders', key: 'o1' }]);
    assert.equal(second.nextCursor, 2);
    assert.equal(second.hasMore, false);
    assert.throws(() => reopened.changes({ since: 3 }), /cursor/);
  } finally { reopened.close(); }
});

test('a core validation failure rolls back every staged batch operation', (t) => {
  const { database } = fixture(t, { maxDocumentBytes: 30 });
  assert.throws(() => database.batch([
    { kind: 'put', collection: 'c', key: 'small', value: { n: 1 } },
    { kind: 'put', collection: 'c', key: 'large', value: { text: 'x'.repeat(100) } },
  ]), /too large/);
  assert.equal(database.collection('c').get('small'), undefined);
  assert.equal(database.stats().latestCursor, 0);
  database.collection('c').put('after', true); // Writer lock released on rollback.
});

test('change pages respect a frame budget while preserving complete transactions', (t) => {
  const { database } = fixture(t, { maxFrameBytes: 300 });
  const rows = database.collection('rows');
  rows.put('a', { text: 'x'.repeat(150) });
  rows.put('b', { text: 'x'.repeat(150) });
  const first = database.changes({ limit: 1000 });
  assert.equal(first.transactions.length, 1);
  assert.equal(first.transactions[0].operations[0].value.text.length, 150);
  assert.equal(first.hasMore, true);
  const second = database.changes({ since: first.nextCursor });
  assert.equal(second.transactions.length, 1);
  assert.equal(second.hasMore, false);
});

test('compareExchange handles absent/null/delete atomically without logging mismatches', (t) => {
  const { database } = fixture(t);
  const locks = database.collection('locks');
  assert.equal(locks.compareExchange('job', undefined, null), true);
  assert.equal(locks.compareExchange('job', undefined, { owner: 'B' }), false);
  assert.equal(database.stats().latestCursor, 1);
  assert.equal(locks.compareExchange('job', null, { owner: 'A' }), true);
  assert.equal(locks.compareExchange('job', { owner: 'A' }, undefined), true);
  assert.equal(locks.get('job'), undefined);
  const cursor = database.stats().latestCursor;
  assert.equal(locks.compareExchange('job', undefined, undefined), true);
  assert.equal(database.stats().latestCursor, cursor);
});

test('indexes accelerate equality queries and follow committed mutations', (t) => {
  const { database } = fixture(t);
  const users = database.collection('users');
  users.putMany([
    { key: 'a', value: { profile: { city: 'Santo Domingo' }, score: 9 } },
    { key: 'b', value: { profile: { city: 'Santiago' }, score: 4 } },
    { key: 'c', value: { profile: { city: 'Santo Domingo' }, score: 7 } },
  ]);
  users.createIndex('/profile/city');
  const query = { filters: [{ field: '/profile/city', op: 'eq', value: 'Santo Domingo' }] };
  const page = users.findPage(query);
  assert.equal(page.usedIndex, '/profile/city');
  assert.deepEqual(page.records.map(({ key }) => key), ['a', 'c']);
  assert.equal(page.scanned, 2);
  users.put('a', { profile: { city: 'Santiago' }, score: 10 });
  users.delete('c');
  assert.deepEqual(users.find(query), []);
  assert.deepEqual(users.aggregate('score'), { count: 2, sum: 14, min: 4, max: 10 });
});

test('bounded pages use exclusive keys; missing fields differ from null', (t) => {
  const { database } = fixture(t);
  const rows = database.collection('rows');
  rows.putMany([{ key: 'a', value: { n: 1 } }, { key: 'b', value: { n: 2, tag: null } }, { key: 'c', value: { n: 3 } }]);
  const first = rows.findPage({ limit: 2 });
  assert.deepEqual(first.records.map(({ key }) => key), ['a', 'b']);
  assert.equal(first.nextAfter, 'b');
  const last = rows.findPage({ after: first.nextAfter, limit: 2 });
  assert.deepEqual(last.records.map(({ key }) => key), ['c']);
  assert.equal(last.nextAfter, undefined);
  assert.deepEqual(rows.find({ filters: [{ field: 'tag', op: 'exists', value: true }] }).map(({ key }) => key), ['b']);
  assert.deepEqual(rows.find({ filters: [{ field: 'n', op: 'gte', value: 2 }] }).map(({ key }) => key), ['b', 'c']);
  assert.throws(() => rows.find({ limit: 1001 }), /1000/);
});

test('close releases the OS lock deterministically and retained collections reject operations', (t) => {
  const { database, directory } = fixture(t);
  const collection = database.collection('c');
  assert.throws(() => new Database(directory), /already open/);
  database.close();
  database.close();
  assert.throws(() => collection.get('a'), /closed/);
  const reopened = new Database(directory);
  reopened.close();
});

test('session indexes must be recreated after reopening', (t) => {
  const { database, directory } = fixture(t);
  database.collection('c').put('a', { n: 1 });
  database.collection('c').createIndex('n');
  assert.equal(database.stats().indexes, 1);
  database.close();
  const reopened = new Database(directory);
  try { assert.equal(reopened.stats().indexes, 0); } finally { reopened.close(); }
});

test('invalid JSON cannot silently change meaning before persistence', (t) => {
  const { database } = fixture(t);
  const rows = database.collection('rows');
  const cycle = {}; cycle.self = cycle;
  const getter = {}; Object.defineProperty(getter, 'value', { enumerable: true, get() { return 1; } });
  for (const value of [undefined, NaN, Infinity, 9007199254740992, 1n, { value: undefined }, new Date(), [, 1], cycle, getter, '\ud800']) {
    assert.throws(() => rows.put('invalid', value));
  }
  rows.put('safe', { high: Number.MAX_SAFE_INTEGER, low: -Number.MAX_SAFE_INTEGER });
  assert.deepEqual(rows.get('safe'), { high: Number.MAX_SAFE_INTEGER, low: -Number.MAX_SAFE_INTEGER });
  assert.equal(database.stats().latestCursor, 1);
});

test('array serialization hooks are rejected before execution or persistence', (t) => {
  const { database } = fixture(t);
  const rows = database.collection('rows');
  const before = database.stats();
  let hookCalls = 0;
  class RewritingArray extends Array {
    toJSON() { hookCalls += 1; return ['rewritten']; }
  }
  assert.throws(() => rows.put('subclass', new RewritingArray(1, 2, 3)), TypeError);
  const ownHook = [1, 2, 3];
  ownHook.toJSON = () => { hookCalls += 1; return ['rewritten']; };
  assert.throws(() => rows.put('own-hook', ownHook), TypeError);

  const previousHook = Object.getOwnPropertyDescriptor(Array.prototype, 'toJSON');
  try {
    Object.defineProperty(Array.prototype, 'toJSON', {
      configurable: true,
      value() { hookCalls += 1; return ['rewritten']; },
    });
    assert.throws(() => rows.put('inherited-hook', [1, 2, 3]), TypeError);
  } finally {
    if (previousHook) Object.defineProperty(Array.prototype, 'toJSON', previousHook);
    else delete Array.prototype.toJSON;
  }
  assert.equal(hookCalls, 0);
  assert.deepEqual(database.stats(), before);
  assert.deepEqual(rows.find(), []);
});

test('ESM and CommonJS expose the same native engine', async (t) => {
  const esm = await import('../index.mjs');
  assert.equal(esm.Database, Database);
  const { database } = fixture(t);
  assert.equal(esm.version, database.stats().version);
});
