import { statSync } from 'node:fs';
import path from 'node:path';
import { Database } from '../index.mjs';

const directory = process.argv[2];
if (!directory) {
  console.error('Usage: node examples/diagnostics.mjs /path/to/existing-database');
  console.error('The path must be a CedroDB directory containing cedro.log.');
  process.exit(1);
}
try {
  if (!statSync(path.join(directory, 'cedro.log')).isFile()) throw new Error('not a file');
} catch {
  console.error('No existing cedro.log found. This inspection example does not create a database.');
  process.exit(1);
}

// No document writes, index creation, or profiling operations are performed.
// Opening the directory still follows CedroDB's ordinary recovery rules.
const db = new Database(directory);
try {
  const report = db.diagnostics();
  console.log('Database:', path.resolve(directory));
  console.table([{
    version: report.stats.version,
    records: report.stats.records,
    collections: report.stats.collections,
    encodedContentBytes: report.stats.contentBytes,
    logBytes: report.stats.logBytes,
    latestCursor: report.stats.latestCursor,
    poisoned: report.poisoned,
    writerActive: report.writerActive,
  }]);
  console.log('Configured encoded-content limits (not a process RAM cap):');
  console.table([report.limits]);
  console.log('Collections and session indexes:');
  console.table(report.collections.map((collection) => ({
    collection: collection.collection,
    records: collection.records,
    contentBytes: collection.contentBytes,
    indexFields: collection.indexFields.join(', ') || '(none)',
  })));
  console.log('Native operation counters for this newly opened handle:');
  console.table(Object.entries(report.session).map(([operation, metrics]) => ({
    operation,
    ...metrics,
  })));
  console.log('Session counters reset on open. Call db.diagnostics() inside your running application to inspect its workload.');
} finally {
  db.close();
}
