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
  } else {
    throw new Error('Unknown interoperability check mode');
  }
} finally {
  database.close();
}
