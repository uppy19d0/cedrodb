import { Database } from '../index.mjs';

const db = new Database(process.argv[2] ?? './cedro-data');
try {
  const jobs = db.collection('jobs');
  jobs.putMany([
    { key: 'job:001', value: { status: 'pending', payload: { task: 'send-report' }, cost: 12 } },
    { key: 'job:002', value: { status: 'done', payload: { task: 'import-csv' }, cost: 8 } },
  ]);
  jobs.createIndex('status');
  console.log('Pending jobs:', jobs.findPage({ filters: [{ field: 'status', op: 'eq', value: 'pending' }] }));
  console.log('Claimed:', jobs.compareExchange('job:001',
    { status: 'pending', payload: { task: 'send-report' }, cost: 12 },
    { status: 'running', payload: { task: 'send-report' }, cost: 12 }));
  console.log('Committed changes:', db.changes({ since: 0, limit: 10 }));
  console.log('Stats:', db.stats());
} finally {
  db.close();
}
