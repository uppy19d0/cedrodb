import assert from 'node:assert/strict';
import { Database } from '../bindings/node/index.mjs';

const [directory, mode] = process.argv.slice(2);
const database = new Database(directory);
try {
  if (mode === 'exchange') {
    const records = database.collection('records');
    const previous = records.get('python');
    assert.deepEqual(previous, { ratio: 1, source: 'python' });
    assert.equal(records.compareExchange('python', previous, { ratio: 1, source: 'node' }), true);
    records.createIndex('ratio');
    const page = records.findPage({ filters: [{ field: 'ratio', op: 'eq', value: 1 }], limit: 10 });
    assert.equal(page.records[0].key, 'python');
    assert.equal(page.usedIndex, 'ratio');
    records.put('node', { source: 'node', count: 2 });
    assert.equal(database.changes({ since: 1, limit: 10 }).nextCursor, 3);
  } else if (mode === 'precision') {
    assert.throws(() => database.collection('large').get('integer'));
    assert.throws(() => database.collection('large').findPage());
    assert.throws(() => database.changes({ since: 3, limit: 1 }));
  } else if (mode === 'agent') {
    const exact = 0.19450526354013234;
    const agents = database.agent('interop');
    const previous = agents.get('thread:1');
    assert.deepEqual(previous.state, { ratio: exact, source: 'python' });
    assert.deepEqual(previous.metadata, { ratio: exact });
    assert.deepEqual(database.collection('outbox').get('python-agent'), { ready: true });
    const saved = agents.checkpoint('thread:1', { ratio: exact, source: 'node' }, {
      expectedRevision: previous.revision, metadata: previous.metadata, nowMs: 1001,
      operations: [{ kind: 'put', collection: 'results', key: 'node-agent', value: { ratio: exact } }]
    });
    assert.equal(saved.applied, true);
    assert.ok(saved.checkpoint.revision > previous.revision);
    const memory = database.collection('memory');
    memory.createIndex('model');
    const hits = memory.search({ text: 'refund', vector: [exact, 1], minVectorScore: 0.99,
      filters: [{ field: 'model', op: 'eq', value: 'demo' }] });
    assert.equal(hits.scanned, 1);
    assert.deepEqual(hits.hits.map(hit => hit.key), ['policy']);
    const cursor = database.stats().latestCursor;
    const profile = memory.profileSearch({ text: 'refund', vector: [exact, 1], minVectorScore: 0.99,
      filters: [{ field: 'model', op: 'eq', value: 'demo' }] });
    assert.deepEqual(profile.page, hits);
    assert.equal(profile.plan.candidateRecords, 1);
    assert.equal(profile.plan.usedIndex, 'model');
    assert.equal(database.diagnostics().session.search.successes, 2);
    assert.equal(database.stats().latestCursor, cursor);
  } else {
    throw new Error('Unknown interoperability check mode');
  }
} finally {
  database.close();
}
