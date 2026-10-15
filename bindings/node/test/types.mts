import cedrodb, { Database, Queue, type Query, type JsonValue, type QueueStats, type Job } from 'cedrodb';
const db = new Database('./unused-typecheck');
const jobs = db.collection<{ state: string; score: number }>('jobs');
const query: Query = { filters: [{ field: 'state', op: 'eq', value: 'pending' }] };
const values: JsonValue[] = jobs.find(query).map(({ value }) => value);
const page = jobs.findPage({ after: 'a', limit: 10 });
const next: string | undefined = page.nextAfter;
const cursor: number = db.changes().nextCursor;
const same: typeof Database = cedrodb.Database;
const verified: number = db.verify().transactions;
const backedUp: number = db.backup('./new-backup.cedro').logBytes;
const restored: number = Database.restoreBackup('./new-backup.cedro', './new-data', { maxStateBytes: 1024 }).records;
const conditional = db.conditionalBatch({
  conditions: [{ kind: 'absent', collection: 'orders', key: 'order:1' }],
  operations: [{ kind: 'put', collection: 'orders', key: 'order:1', value: { state: 'paid' } }],
});
const failed: number | null = conditional.failedCondition;
const queue = db.queue<{ task: string }>('reports');
const queued: Job<{ task: string }> = queue.enqueue('job:1', { task: 'report' }, { nowMs: 1000, priority: 2 }).job;
const queueConstructor: typeof Queue = cedrodb.Queue;
const queueStats: QueueStats = queue.stats({ nowMs: 1000 });
const lease = queue.claim('worker:1', { nowMs: 1000, leaseMs: 100 });
if (lease) {
  const payload: string = lease.payload.task;
  queue.extend(lease.id, lease.leaseToken, { nowMs: 1010, leaseMs: 100 });
  queue.ack(lease.id, lease.leaseToken, { nowMs: 1020, operations: [{ kind: 'put', collection: 'results', key: lease.id, value: payload }] });
}
void [values, next, cursor, same, verified, backedUp, restored, failed, queued, queueConstructor, queueStats];
// @ts-expect-error BigInt is not a JSON document.
jobs.put('a', 1n);
// @ts-expect-error Query operator is a closed set.
jobs.find({ filters: [{ field: 'score', op: 'contains', value: 1 }] });
// @ts-expect-error absent is a distinct condition without a value field.
db.conditionalBatch({ conditions: [{ kind: 'absent', collection: 'c', key: 'a', value: null }], operations: [] });
// @ts-expect-error Queue payloads use the declared generic type.
queue.enqueue('job:2', { task: 42 });
// @ts-expect-error Timestamps and durations use safe integer numbers, not BigInt.
queue.claim('worker:2', { leaseMs: 1n });
