import { mkdirSync } from 'node:fs';
import path from 'node:path';
import { Database } from '../index.mjs';

// Use a fresh root each time: backups and restore targets never overwrite files.
const root = path.resolve(process.argv[2] ?? `./offline-demo-${Date.now()}`);
mkdirSync(root, { recursive: true });
const backupPath = path.join(root, 'before-sync.cedro-backup');
const restoredPath = path.join(root, 'restored-data');
const db = new Database(path.join(root, 'device-data'));
try {
  const inspections = db.collection('inspections');
  db.batch([
    { kind: 'put', collection: 'inspections', key: 'inspection:001', value: { site: 'Santo Domingo', status: 'pending', temperature: 24.5 } },
    { kind: 'put', collection: 'sync', key: 'checkpoint', value: { cursor: 0 } },
  ]);
  inspections.createIndex('status');
  console.log('Ready to sync:', inspections.find({ filters: [{ field: 'status', op: 'eq', value: 'pending' }] }));
  console.log('Verified:', db.verify());
  console.log('Backup:', db.backup(backupPath));

  // This later write belongs only to the original database.
  inspections.put('inspection:002', { site: 'Santiago', status: 'pending', temperature: 26 });
  console.log('Restored:', Database.restoreBackup(backupPath, restoredPath));
  const restored = new Database(restoredPath);
  try {
    restored.collection('inspections').createIndex('status');
    console.log('Recovered inspections:', restored.collection('inspections').find());
    console.log('Original durable history:', restored.changes({ since: 0 }));
  } finally {
    restored.close();
  }
} finally {
  db.close();
}

