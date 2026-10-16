'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { Database } = require('..');

function fixture(t) {
  const directory = fs.mkdtempSync(path.join(tmpdir(), 'cedrodb-node-search-'));
  const database = new Database(directory);
  t.after(() => { database.close(); fs.rmSync(directory, { recursive: true, force: true }); });
  return { directory, database, documents: database.collection('knowledge') };
}
const keys = (page) => page.hits.map((hit) => hit.key);
const near = (actual, expected) => assert.ok(Math.abs(actual - expected) < 1e-12, actual + ' differs from ' + expected);

test('exact cosine ranks non-unit vectors with deterministic key ties and complete metadata', (t) => {
  const { documents } = fixture(t);
  documents.putMany([
    { key: 'b-same', value: { embedding: [2, 0] } },
    { key: 'a-same', value: { embedding: [4, 0] } },
    { key: 'near', value: { embedding: [1, 1] } },
    { key: 'orthogonal', value: { embedding: [0, 2] } },
    { key: 'opposite', value: { embedding: [-1, 0] } },
    { key: 'missing', value: { text: 'No embedding' } },
  ]);
  const page = documents.search({ vector: [3, 0], limit: 3 });
  assert.deepEqual(keys(page), ['a-same', 'b-same', 'near']);
  assert.equal(page.mode, 'vector');
  assert.equal(page.scanned, 6);
  assert.equal(page.matched, 5);
  assert.equal(page.usedIndex, undefined);
  assert.equal(page.hits[0].textScore, null);
  near(page.hits[0].score, 1);
  near(page.hits[2].vectorScore, Math.SQRT1_2);
  assert.deepEqual(keys(documents.search({ vector: [1, 0], minVectorScore: 0.5 })), ['a-same', 'b-same', 'near']);
});

test('BM25 uses Unicode alphanumeric tokens, case folding and explicit nested fields', (t) => {
  const { documents } = fixture(t);
  documents.putMany([
    { key: 'strong', value: { title: 'CEDRO cedro', body: { text: 'árbol' } } },
    { key: 'weak', value: { title: 'cedro', body: { text: 'muchas otras palabras sin relación' } } },
    { key: 'other', value: { title: 'roble', body: { text: 'bosque' } } },
  ]);
  const page = documents.search({ text: 'CeDrO', textFields: ['title', '/body/text'] });
  assert.deepEqual(keys(page), ['strong', 'weak']);
  assert.equal(page.mode, 'text');
  assert.equal(page.matched, 2);
  assert.equal(page.hits[0].vectorScore, null);
  assert.ok(page.hits[0].textScore > page.hits[1].textScore);
  assert.deepEqual(keys(documents.search({ text: 'ÁRBOL', textFields: ['/body/text'] })), ['strong']);
  assert.deepEqual(documents.search({ text: 'nonexistent', textFields: ['title'] }).hits, []);
});

test('hybrid RRF ranks every channel before applying the result limit', (t) => {
  const { documents } = fixture(t);
  documents.putMany([
    { key: 'a-text', value: { text: 'cedro cedro cedro', embedding: [0, 1] } },
    { key: 'b-joint', value: { text: 'cedro cedar filler filler', embedding: [1, 0] } },
    { key: 'c-vector', value: { text: 'other', embedding: [0.8, 0.6] } },
  ]);
  assert.deepEqual(keys(documents.search({ text: 'cedro' })), ['a-text', 'b-joint']);
  assert.deepEqual(keys(documents.search({ vector: [1, 0] })), ['b-joint', 'c-vector', 'a-text']);
  const page = documents.search({ text: 'cedro', vector: [1, 0], limit: 1 });
  assert.equal(page.mode, 'hybrid');
  assert.equal(page.matched, 3);
  assert.deepEqual(keys(page), ['b-joint']);
  near(page.hits[0].score, 1 / 61 + 1 / 62);
  near(page.hits[0].vectorScore, 1);
  assert.ok(page.hits[0].textScore > 0);
});

test('metadata predicates run before vector validation and equality indexes narrow scanning', (t) => {
  const { documents } = fixture(t);
  documents.putMany([
    { key: 'a-foreign', value: { tenant: 'bob', model: 'v1', embedding: 'invalid' } },
    { key: 'b-old-model', value: { tenant: 'alice', model: 'v0', embedding: [0, 0, 0] } },
    { key: 'c-eligible', value: { tenant: 'alice', model: 'v1', embedding: [1, 0] } },
  ]);
  const filters = [
    { field: 'tenant', op: 'eq', value: 'alice' },
    { field: 'model', op: 'eq', value: 'v1' },
  ];
  assert.deepEqual(keys(documents.search({ vector: [1, 0], filters })), ['c-eligible']);
  assert.throws(() => documents.search({ vector: [1, 0] }), /vector/);
  documents.createIndex('model');
  const page = documents.search({ vector: [1, 0], filters });
  assert.equal(page.usedIndex, 'model');
  assert.equal(page.scanned, 2);
  assert.deepEqual(keys(page), ['c-eligible']);
});

test('eligible malformed vectors and mismatched dimensions produce errors instead of partial results', (t) => {
  const { database } = fixture(t);
  for (const [index, embedding] of [[0, [0, 0]], [1, [1, 0, 0]], [2, [1, 'bad']], [3, 'bad'], [4, []]]) {
    const documents = database.collection('invalid' + index);
    documents.put('a-valid', { embedding: [1, 0] });
    documents.put('b-invalid', { embedding });
    assert.throws(() => documents.search({ vector: [1, 0], limit: 1 }), /vector|dimension/);
  }
});

test('scan budgets count candidates before filtering and never silently truncate exact top-k', (t) => {
  const { documents } = fixture(t);
  documents.putMany([
    { key: 'a', value: { tenant: 'bob', text: 'cedro' } },
    { key: 'b', value: { tenant: 'alice', text: 'cedro' } },
    { key: 'c', value: { tenant: 'bob', text: 'cedro' } },
  ]);
  const query = { text: 'cedro', filters: [{ field: 'tenant', op: 'eq', value: 'alice' }], maxScanned: 1 };
  assert.throws(() => documents.search(query), /budget/);
  documents.createIndex('tenant');
  const page = documents.search(query);
  assert.deepEqual(keys(page), ['b']);
  assert.equal(page.scanned, 1);
  assert.equal(page.usedIndex, 'tenant');
  documents.put('c', { tenant: 'alice', text: 'cedro' });
  assert.throws(() => documents.search(query), /budget/);
  const empty = documents.search({ ...query, filters: [{ field: 'tenant', op: 'eq', value: 'missing' }] });
  assert.equal(empty.scanned, 0);
  assert.deepEqual(empty.hits, []);
});

test('rankings survive reopening and tiny vectors normalize without underflow', (t) => {
  const { database, directory, documents } = fixture(t);
  documents.putMany([
    { key: 'a', value: { text: 'cedro cedro', embedding: [1e-300, 0], model: 'v1' } },
    { key: 'b', value: { text: 'cedro tree', embedding: [0, Number.MAX_SAFE_INTEGER], model: 'v1' } },
  ]);
  documents.createIndex('model');
  const query = { text: 'cedro', vector: [1e-300, 0], filters: [{ field: 'model', op: 'eq', value: 'v1' }] };
  const before = documents.search(query);
  database.close();
  const reopened = new Database(directory);
  try {
    const after = reopened.collection('knowledge').search(query);
    assert.deepEqual(after.hits, before.hits);
    assert.equal(after.usedIndex, undefined);
  } finally { reopened.close(); }
});

test('search options reject malformed JSON, unsupported fields and invalid limits', (t) => {
  const { database, documents } = fixture(t);
  for (const query of [
    undefined, {}, { text: '' }, { text: '!!!' }, { text: 'x'.repeat(4097) },
    { text: 'cedro', extra: true }, { text: 'cedro', limit: 0 }, { text: 'cedro', limit: 1001 },
    { text: 'cedro', maxScanned: 0 }, { text: 'cedro', maxScanned: 100001 },
    { text: 'cedro', textFields: [] }, { text: 'cedro', textFields: ['text', 'text'] },
    { text: 'cedro', textFields: new Array(1) }, { text: 'cedro', filters: null },
    { text: 'cedro', minVectorScore: 0.5 }, { vector: [] }, { vector: [0, 0] },
    { vector: [NaN, 0] }, { vector: [1n, 0] }, { vector: [Number.MAX_SAFE_INTEGER + 1] },
    { vector: new Float32Array([1, 0]) }, { vector: [[1], 0] },
    { vector: [1, 0], minVectorScore: 2 }, { vector: [1, 0], vectorField: '' },
    { text: Array.from({ length: 65 }, (_, index) => 'word' + index).join(' ') },
  ]) assert.throws(() => documents.search(query));
  assert.equal(database.stats().latestCursor, 0);
  database.close();
  assert.throws(() => documents.search({ text: 'cedro' }), /closed/);
});
