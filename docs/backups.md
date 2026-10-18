# Backup, integrity and recovery

Use backups before application/schema upgrades, when transferring an offline dataset or while rehearsing recovery. They preserve complete committed history, not only current documents.

```js
const verified = db.verify();
const copied = db.backup('./before-upgrade.cedrobak');
const restored = Database.restoreBackup('./before-upgrade.cedrobak', './recovered.cedro');
// Reports: logBytes, transactions, records, contentBytes.
```

Python uses `db.verify()`, `db.backup(path)` and `Database.restore_backup(file, directory)` with snake_case report fields. Rust exposes the same operations and `restore_backup_with_options`.

## Guarantees and procedure

`verify` replays the complete log and checks its header, frame lengths/checksums, JSON operations and configured limits without repairing it. Normal database opening may already have recovered an incomplete final frame. Preserve a damaged original before investigating it.

`backup` creates a new FILE. Its parent must already exist and an existing path is never overwritten. It verifies and copies a consistent committed log while holding the database mutex. An active staged writer is rejected. Other operations on that handle wait while maintenance runs.

The versioned container stores the exact log length and CRC32. This detects truncation even at an otherwise valid transaction boundary. It is not compressed, encrypted or cryptographically authenticated.

Restoration validates the complete container and embedded log before creating a destination DIRECTORY. The destination must not exist; its parent must exist. Use the same or larger content limits when restoring data created with increased limits. The input backup is opened without modification.

After restoring, open the new directory, recreate session-local indexes, verify it and check application invariants. Switch the application through its normal configuration and retain the original until recovery has been reviewed.

Transaction order and cursors are preserved. A cursor newer than an older backup's final transaction is invalid in that restored copy. A restored copy and a separately advanced original are different histories once they diverge.

## Limits

Content limits (`max_document_bytes`, `max_frame_bytes`, `max_state_bytes`) are caller policy and are not stored in the log. Never lower them for an existing database: opening it with smaller limits fails with `LimitExceeded` (not `Corruption`) and leaves the file unchanged; reopen with the original or larger limits.

- Copying uses a 64 KiB buffer, but verification replays live data into additional RAM and decodes frames. Total memory is not limited to that buffer.
- Backups include the full growing log. There are no incremental backups, retention scheduler, remote uploads or automatic failover.
- Files are synchronized before success; final durability depends on filesystem/OS/device behavior. Directory sync support is platform-dependent.
- Failed-operation cleanup is best effort and removes only newly created files/directories. Restore builds in a hidden sibling directory (`.NAME.PID.N.restoring`) and renames it to the destination only when complete, so a crash never leaves a partial destination at the final name; it may leave that hidden directory, which is safe to delete. The existing-destination check precedes the rename, leaving a narrow race with a concurrent creator of an empty directory at the same name. Not tested under real process kills.
- Keep copies on separate storage to protect against device failure, and apply OS encryption/access controls where needed.

Executable examples: [Node offline backup](../bindings/node/examples/offline-backup.mjs), [Python offline forms](../bindings/python/examples/offline_forms.py).

## Format fixtures

`tests/fixtures/v1-log.bin` and `v1-backup.bin` pin the version-1 log and backup bytes; `tests/format_fixtures.rs` checks that they open, verify, restore and match the current writer. A deliberate format change must regenerate them (command in the test header) and bump the format version. Mutation tests there are seeded and deterministic; coverage-guided fuzzing is not yet set up.
