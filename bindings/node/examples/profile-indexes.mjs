import assert from 'node:assert/strict';
import { existsSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { Database } from '../index.mjs';

const directory = process.argv[2] ?? mkdtempSync(path.join(tmpdir(), 'cedrodb-profile-'));
if (existsSync(path.join(directory, 'cedro.log'))) {
  throw new Error('Use a new database directory: this example writes a synthetic dataset.');
}
const db = new Database(directory);
try {
  const documents = db.collection('knowledge');
  documents.putMany(Array.from({ length: 1000 }, (_, index) => ({
    key: String(index).padStart(6, '0'),
    value: {
      group: index % 100 === 0 ? 'target' : 'other',
      text: index % 100 === 0 ? 'cedro backups preserve history' : 'cedro jobs use leases',
      embedding: index % 100 === 0 ? [1, 0] : [0, 1],
    },
  })));
  const filters = [{ field: 'group', op: 'eq', value: 'target' }];
  const query = { filters, limit: 5 };
  const search = { text: 'backups', vector: [1, 0], filters, limit: 5, maxScanned: 1000 };

  const queryBefore = documents.profileQuery(query);
  const searchBefore = documents.profileSearch(search);
  documents.createIndex('group');
  const queryAfter = documents.profileQuery(query);
  const searchAfter = documents.profileSearch(search);

  assert.deepEqual(queryBefore.page.records, queryAfter.page.records);
  assert.deepEqual(searchBefore.page.hits, searchAfter.page.hits);
  const row = (operation, profile) => ({
    operation,
    coreElapsedMicros: profile.elapsedMicros,
    collectionRecords: profile.plan.collectionRecords,
    candidateRecords: profile.plan.candidateRecords,
    scanned: profile.plan.scanned,
    returned: profile.plan.returned,
    usedIndex: profile.plan.usedIndex ?? '(full scan)',
  });
  console.log('Synthetic 1000-record dataset; hand-written vectors are not model embeddings.');
  console.table([
    row('query before index', queryBefore),
    row('search before index', searchBefore),
    row('query after index', queryAfter),
    row('search after index', searchAfter),
  ]);
  console.log('These are measured single-run core timings, not a benchmark or a promised speedup.');
  console.log('They exclude SDK JSON conversion, startup, and index construction. Indexes are session-local.');
  console.log('Session counters:', db.diagnostics().session);
  console.log('Database retained for inspection:', path.resolve(directory));
} finally {
  db.close();
}
