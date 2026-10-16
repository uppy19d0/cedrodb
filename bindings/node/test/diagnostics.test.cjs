'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { Database } = require('..');

function fixture(t, options) {
  const directory = fs.mkdtempSync(path.join(tmpdir(), 'cedrodb-node-diagnostics-'));
  const database = new Database(directory, options);
  t.after(() => { database.close(); fs.rmSync(directory, { recursive: true, force: true }); });
  return { directory, database, documents: database.collection('docs') };
}
function assertMetrics(metrics, attempts, successes, failures) {
  assert.equal(metrics.attempts, attempts);
  assert.equal(metrics.successes, successes);
  assert.equal(metrics.failures, failures);
  assert.ok(Number.isSafeInteger(metrics.totalElapsedMicros) && metrics.totalElapsedMicros >= 0);
  assert.ok(Number.isSafeInteger(metrics.maxElapsedMicros) && metrics.maxElapsedMicros >= 0);
  assert.ok(metrics.totalElapsedMicros >= metrics.maxElapsedMicros);
}

test('diagnostics reports configured limits, collections and indexes without writing to the log', (t) => {
  const { database, documents } = fixture(t, {
    maxDocumentBytes: 4096, maxFrameBytes: 65536, maxStateBytes: 1048576,
  });
  documents.putMany([
    { key: 'a', value: { kind: 'target', nested: { flag: true } } },
    { key: 'b', value: { kind: 'other', nested: { flag: false } } },
  ]);
  documents.createIndex('kind');
  documents.createIndex('/nested/flag');
  database.collection('other').put('x', true);
  // Inspect through the owning handle: Windows rejects a second file descriptor
  // while the database holds its exclusive log lock.
  const before = database.verify();
  const cursor = database.stats().latestCursor;
  const report = database.diagnostics();
  assert.deepEqual(report.stats, database.stats());
  assert.deepEqual(report.limits, { maxDocumentBytes: 4096, maxFrameBytes: 65536, maxStateBytes: 1048576 });
  assert.equal(report.poisoned, false);
  assert.equal(report.writerActive, false);
  assert.equal(report.collections.length, 2);
  const collection = report.collections.find((entry) => entry.collection === 'docs');
  assert.equal(collection.records, 2);
  assert.deepEqual(collection.indexFields.slice().sort(), ['/nested/flag', 'kind']);
  assert.equal(report.collections.reduce((sum, entry) => sum + entry.contentBytes, 0), report.stats.contentBytes);
  assert.deepEqual(database.diagnostics(), report);
  assert.deepEqual(database.verify(), before);
  assert.equal(database.stats().logBytes, before.logBytes);
  assert.equal(database.stats().latestCursor, cursor);
});

test('profileQuery reports actual scan/lookahead counts and reuses normal query results', (t) => {
  const { database, documents } = fixture(t);
  documents.putMany(Array.from({ length: 5 }, (_, index) => ({
    key: 'k' + index,
    value: { kind: index % 2 === 0 ? 'target' : 'other', created_at: 'user data', used_index: { nested_field: true } },
  })));
  const query = { after: 'k1', limit: 1 };
  const profile = documents.profileQuery(query);
  assert.deepEqual(profile.page, documents.findPage(query));
  assert.deepEqual(profile.plan, {
    collectionRecords: 5, candidateRecords: 3, scanned: 2, returned: 1, usedIndex: undefined,
  });
  assert.ok(Number.isSafeInteger(profile.elapsedMicros) && profile.elapsedMicros >= 0);
  assert.equal(profile.page.nextAfter, 'k2');
  assert.equal(profile.page.records[0].value.created_at, 'user data');
  assert.deepEqual(profile.page.records[0].value.used_index, { nested_field: true });
  assertMetrics(database.diagnostics().session.query, 2, 2, 0);
});

test('profiled equality indexes reduce candidates without changing query or ranked search results', (t) => {
  const { database, documents } = fixture(t);
  documents.putMany(Array.from({ length: 100 }, (_, index) => ({
    key: String(index).padStart(3, '0'),
    value: { group: index % 20 === 0 ? 'target' : 'other', text: 'cedro backups', embedding: [1, 0], user_field: 'untouched' },
  })));
  const filters = [{ field: 'group', op: 'eq', value: 'target' }];
  const query = { filters, limit: 2 };
  const search = { text: 'cedro', vector: [1, 0], filters, limit: 2, maxScanned: 100 };
  const queryBefore = documents.profileQuery(query);
  const searchBefore = documents.profileSearch(search);
  documents.createIndex('group');
  const queryAfter = documents.profileQuery(query);
  const searchAfter = documents.profileSearch(search);
  assert.deepEqual(queryBefore.page.records, queryAfter.page.records);
  assert.deepEqual(searchBefore.page.hits, searchAfter.page.hits);
  assert.equal(queryBefore.plan.candidateRecords, 100);
  assert.equal(queryAfter.plan.candidateRecords, 5);
  assert.equal(queryAfter.plan.scanned, 3);
  assert.equal(queryAfter.plan.usedIndex, 'group');
  assert.equal(searchBefore.plan.scanned, 100);
  assert.deepEqual(searchAfter.plan, {
    collectionRecords: 100, candidateRecords: 5, scanned: 5, returned: 2, usedIndex: 'group',
  });
  assert.equal(searchAfter.page.hits[0].value.user_field, 'untouched');
  assertMetrics(database.diagnostics().session.query, 2, 2, 0);
  assertMetrics(database.diagnostics().session.search, 2, 2, 0);
});

test('profiled and ordinary core validation failures count once; wrapper validation never invokes the engine', (t) => {
  const { database, documents } = fixture(t);
  documents.put('a', { text: 'cedro', embedding: [1, 0] });
  assert.throws(() => documents.profileQuery({ filters: [{ field: 'n', op: 'gt', value: true }] }), /ordered/);
  assert.throws(() => documents.profileSearch({ vector: [0, 0] }), /nonzero/);
  assert.throws(() => documents.search({ vector: [0, 0] }), /nonzero/);
  assertMetrics(database.diagnostics().session.query, 1, 0, 1);
  assertMetrics(database.diagnostics().session.search, 2, 0, 2);
  const before = database.diagnostics().session;
  assert.throws(() => documents.profileQuery({ unknown: true }), TypeError);
  assert.throws(() => documents.profileSearch({ text: 'cedro', unknown: true }), TypeError);
  assert.throws(() => documents.profileSearch({ vector: [1n, 0] }), TypeError);
  assert.deepEqual(database.diagnostics().session, before);
});

test('a failed profiled search preserves the scan-budget contract and does not write data', (t) => {
  const { database, documents } = fixture(t);
  documents.putMany([{ key: 'a', value: { text: 'cedro' } }, { key: 'b', value: { text: 'cedro' } }]);
  const cursor = database.stats().latestCursor;
  assert.throws(() => documents.profileSearch({ text: 'cedro', maxScanned: 1 }), /budget/);
  assertMetrics(database.diagnostics().session.search, 1, 0, 1);
  assert.equal(database.stats().latestCursor, cursor);
  assert.equal(database.diagnostics().poisoned, false);
});

test('commit metrics count nonempty attempts, including commit validation errors but excluding staged errors and conflicts', (t) => {
  const { database, documents } = fixture(t, { maxStateBytes: 64 });
  database.batch([]);
  assertMetrics(database.diagnostics().session.commit, 0, 0, 0);
  documents.put('a', 'small');
  assertMetrics(database.diagnostics().session.commit, 1, 1, 0);
  assert.equal(documents.compareExchange('a', 'wrong', true), false);
  assert.equal(documents.compareExchange('missing', undefined, undefined), true);
  assert.equal(database.conditionalBatch({
    conditions: [{ kind: 'absent', collection: 'docs', key: 'a' }], operations: [],
  }).applied, false);
  assertMetrics(database.diagnostics().session.commit, 1, 1, 0);
  assert.throws(() => documents.put('b', 'x'.repeat(100)), /limit/i);
  assertMetrics(database.diagnostics().session.commit, 2, 1, 1);
  assert.equal(documents.get('b'), undefined);
  assert.equal(database.diagnostics().poisoned, false);
});

test('session metrics and indexes reset after reopen while durable state remains', (t) => {
  const { database, directory, documents } = fixture(t);
  documents.put('a', { text: 'cedro', group: 'target' });
  documents.createIndex('group');
  documents.profileQuery();
  documents.profileSearch({ text: 'cedro' });
  assertMetrics(database.diagnostics().session.commit, 1, 1, 0);
  const cursor = database.stats().latestCursor;
  database.close();
  const reopened = new Database(directory);
  try {
    const report = reopened.diagnostics();
    assert.equal(report.stats.latestCursor, cursor);
    assert.equal(report.stats.records, 1);
    assert.deepEqual(report.collections[0].indexFields, []);
    assertMetrics(report.session.query, 0, 0, 0);
    assertMetrics(report.session.search, 0, 0, 0);
    assertMetrics(report.session.commit, 0, 0, 0);
  } finally { reopened.close(); }
});

test('empty profiles return empty plans and closed database handles reject diagnostics/profiling', (t) => {
  const { database, documents } = fixture(t);
  const empty = { collectionRecords: 0, candidateRecords: 0, scanned: 0, returned: 0, usedIndex: undefined };
  assert.deepEqual(documents.profileQuery().plan, empty);
  assert.deepEqual(documents.profileSearch({ text: 'cedro' }).plan, empty);
  database.close();
  assert.throws(() => database.diagnostics(), /closed/);
  assert.throws(() => documents.profileQuery(), /closed/);
  assert.throws(() => documents.profileSearch({ text: 'cedro' }), /closed/);
});
