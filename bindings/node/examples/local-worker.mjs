import { Database } from '../index.mjs';

const db = new Database(process.argv[2] ?? `./local-worker-${Date.now()}`);
try {
  const queue = db.queue('reports');
  const now = 1000; // Injected clock: this example needs no timers or sleeps.
  console.log('Enqueued:', queue.enqueue('report:42', { customer: 'cedro', amount: 25 }, {
    nowMs: now, maxAttempts: 3, priority: 10,
  }).inserted);
  console.log('Duplicate:', queue.enqueue('report:42', { ignored: true }, { nowMs: now }).inserted);

  const first = queue.claim('worker:offline', { nowMs: now, leaseMs: 100 });
  if (!first) throw new Error('Expected a ready job');
  queue.fail(first.id, first.leaseToken, 'Temporary report renderer failure', {
    nowMs: now + 10, retryDelayMs: 50,
  });

  console.log('Before retry is due:', queue.claim('worker:offline', { nowMs: now + 59, leaseMs: 100 }));
  const retry = queue.claim('worker:offline', { nowMs: now + 60, leaseMs: 100 });
  if (!retry) throw new Error('Expected a retryable job');
  console.log('Stale acknowledgment:', queue.ack(first.id, first.leaseToken, { nowMs: now + 61 }));
  console.log('Acknowledged with results:', queue.ack(retry.id, retry.leaseToken, {
    nowMs: now + 61,
    operations: [
      { kind: 'put', collection: 'reports', key: retry.id, value: { customer: retry.payload.customer, total: retry.payload.amount } },
      { kind: 'put', collection: 'outbox', key: `${retry.id}:ready`, value: { type: 'report.ready', reportId: retry.id } },
    ],
  }));
  console.log('Queue:', queue.stats({ nowMs: now + 61 }));
  console.log('Persisted report:', db.collection('reports').get('report:42'));
  console.log('Verified:', db.verify());
} finally {
  db.close();
}
