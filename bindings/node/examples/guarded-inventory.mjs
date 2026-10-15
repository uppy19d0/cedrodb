import { Database } from '../index.mjs';

const db = new Database(process.argv[2] ?? `./guarded-inventory-${Date.now()}`);
try {
  const inventory = db.collection('inventory');
  inventory.put('item:cedar', { available: 3, revision: 1 });
  const before = inventory.get('item:cedar');
  const request = {
    conditions: [
      { kind: 'equals', collection: 'inventory', key: 'item:cedar', value: before },
      { kind: 'absent', collection: 'orders', key: 'order:42' },
    ],
    operations: [
      { kind: 'put', collection: 'inventory', key: 'item:cedar', value: { available: before.available - 1, revision: before.revision + 1 } },
      { kind: 'put', collection: 'orders', key: 'order:42', value: { item: 'item:cedar', quantity: 1, state: 'reserved' } },
      { kind: 'put', collection: 'outbox', key: 'order:42:reserved', value: { type: 'order.reserved', orderId: 'order:42' } },
    ],
  };
  console.log('Reservation:', db.conditionalBatch(request));
  console.log('Retry with stale state:', db.conditionalBatch(request));
  console.log('Remaining inventory:', inventory.get('item:cedar'));
  console.log('Durable transaction:', db.changes({ since: 1 }));
} finally {
  db.close();
}
