use cedrodb::{Database, Error, Options};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cedrodb-maintenance-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn join(&self, name: impl AsRef<Path>) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn streaming_backup_restores_history_cursors_values_and_future_writes() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.join("source")).unwrap();
    let mut transaction = database.begin_write().unwrap();
    transaction
        .put(
            "documents",
            "unicode",
            json!({"name":"José 🌲","maximum":u64::MAX,"nested":[true,null,1.0]}),
        )
        .unwrap();
    transaction
        .put("documents", "large", json!({"text":"x".repeat(180_000)}))
        .unwrap();
    transaction.put("documents", "temporary", json!(1)).unwrap();
    transaction.commit().unwrap();
    database.delete("documents", "temporary").unwrap();
    database
        .put(
            "documents",
            "unicode",
            json!({"name":"María 🌲","maximum":u64::MAX,"nested":[true,null,1.0]}),
        )
        .unwrap();
    database.create_index("documents", "name").unwrap();
    let verified = database.verify().unwrap();
    let original_records = database.scan("documents").unwrap();
    let original_history = database.changes(0, 1000).unwrap();
    let backup = directory.join("database.cedrobak");
    assert_eq!(database.backup(&backup).unwrap(), verified);
    let backup_bytes = fs::read(&backup).unwrap();
    assert_eq!(backup_bytes.len() as u64, 32 + verified.log_bytes);
    assert_eq!(verified.transactions, 3);
    assert_eq!(verified.records, 2);
    assert!(verified.log_bytes > 65_536);
    database
        .put("documents", "after-backup", json!({"ok":true}))
        .unwrap();
    assert_eq!(database.verify().unwrap().transactions, 4);
    assert_eq!(fs::read(&backup).unwrap(), backup_bytes);
    let restored_path = directory.join("restored");
    assert_eq!(
        Database::restore_backup(&backup, &restored_path).unwrap(),
        verified
    );
    assert_eq!(fs::read(&backup).unwrap(), backup_bytes);
    let restored = Database::open(&restored_path).unwrap();
    assert_eq!(restored.verify().unwrap(), verified);
    assert_eq!(restored.scan("documents").unwrap(), original_records);
    assert_eq!(restored.changes(0, 1000).unwrap(), original_history);
    assert_eq!(restored.stats().unwrap().indexes, 0);
    assert!(restored.get("documents", "after-backup").unwrap().is_none());
    restored
        .put("documents", "after-restore", json!(true))
        .unwrap();
    assert_eq!(
        restored
            .changes(verified.transactions, 100)
            .unwrap()
            .transactions[0]
            .cursor,
        verified.transactions + 1
    );
    drop(restored);
    let reopened = Database::open(&restored_path).unwrap();
    assert_eq!(
        reopened.get("documents", "after-restore").unwrap(),
        Some(json!(true))
    );
}

#[test]
fn empty_database_backup_has_a_complete_log_header_and_zero_transactions() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.join("source")).unwrap();
    let backup = directory.join("empty.cedrobak");
    let report = database.backup(&backup).unwrap();
    assert_eq!(report.log_bytes, 16);
    assert_eq!(report.transactions, 0);
    assert_eq!(report.records, 0);
    assert_eq!(report.content_bytes, 0);
    assert_eq!(fs::metadata(&backup).unwrap().len(), 48);
    let restored_path = directory.join("restored");
    assert_eq!(
        Database::restore_backup(&backup, &restored_path).unwrap(),
        report
    );
    let restored = Database::open(restored_path).unwrap();
    assert!(restored.changes(0, 100).unwrap().transactions.is_empty());
}

#[test]
fn backup_and_restore_never_overwrite_existing_paths_or_create_missing_parents() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.join("source")).unwrap();
    database.put("docs", "a", json!({"n":1})).unwrap();
    let existing = directory.join("existing");
    fs::write(&existing, b"keep this file").unwrap();
    assert!(database.backup(&existing).is_err());
    assert_eq!(fs::read(&existing).unwrap(), b"keep this file");
    assert!(database
        .backup(directory.join("missing/backup.cedrobak"))
        .is_err());
    assert!(!directory.join("missing").exists());
    let backup = directory.join("valid.cedrobak");
    database.backup(&backup).unwrap();
    let backup_before = fs::read(&backup).unwrap();
    assert!(Database::restore_backup(&backup, &existing).is_err());
    assert_eq!(fs::read(&existing).unwrap(), b"keep this file");
    let empty = directory.join("empty");
    fs::create_dir(&empty).unwrap();
    assert!(Database::restore_backup(&backup, &empty).is_err());
    assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);
    let populated = directory.join("populated");
    fs::create_dir(&populated).unwrap();
    fs::write(populated.join("unrelated"), b"keep").unwrap();
    assert!(Database::restore_backup(&backup, &populated).is_err());
    assert_eq!(fs::read(populated.join("unrelated")).unwrap(), b"keep");
    assert!(Database::restore_backup(&backup, directory.join("another-missing/restore")).is_err());
    assert!(!directory.join("another-missing").exists());
    assert_eq!(fs::read(&backup).unwrap(), backup_before);
}

#[test]
fn every_truncated_backup_is_rejected_before_destination_creation() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.join("source")).unwrap();
    database.put("docs", "first", json!(1)).unwrap();
    database
        .put("docs", "second", json!({"text":"last frame"}))
        .unwrap();
    let backup = directory.join("valid.cedrobak");
    database.backup(&backup).unwrap();
    let bytes = fs::read(backup).unwrap();
    let candidate = directory.join("truncated.cedrobak");
    let destination = directory.join("must-not-be-created");
    // Includes truncation exactly after a valid earlier transaction, which an
    // unwrapped cedro.log recovery would otherwise accept as a shorter history.
    for cut in 0..bytes.len() {
        fs::write(&candidate, &bytes[..cut]).unwrap();
        assert!(
            matches!(
                Database::restore_backup(&candidate, &destination),
                Err(Error::Corruption(_))
            ),
            "accepted cut {cut}"
        );
        assert!(!destination.exists(), "created destination at cut {cut}");
        assert_eq!(fs::read(&candidate).unwrap(), &bytes[..cut]);
    }
}

#[test]
fn corrupt_container_headers_payloads_and_trailing_bytes_are_rejected_without_writes() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.join("source")).unwrap();
    database.put("docs", "a", json!({"n":1})).unwrap();
    let backup = directory.join("valid.cedrobak");
    database.backup(&backup).unwrap();
    let bytes = fs::read(backup).unwrap();
    let mut candidates = Vec::new();
    for position in [0, 9, 16, 24, 28, 32, bytes.len() - 1] {
        let mut corrupted = bytes.clone();
        corrupted[position] ^= 0x80;
        candidates.push(corrupted);
    }
    let mut overflowing_length = bytes.clone();
    overflowing_length[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    candidates.push(overflowing_length);
    let mut trailing = bytes.clone();
    trailing.push(0);
    candidates.push(trailing);
    for (index, corrupted) in candidates.into_iter().enumerate() {
        let candidate = directory.join(format!("corrupt-{index}.cedrobak"));
        let destination = directory.join(format!("destination-{index}"));
        fs::write(&candidate, &corrupted).unwrap();
        assert!(matches!(
            Database::restore_backup(&candidate, &destination),
            Err(Error::Corruption(_))
        ));
        assert!(!destination.exists());
        assert_eq!(fs::read(candidate).unwrap(), corrupted);
    }
}

fn replace_container_log(original: &[u8], log: &[u8]) -> Vec<u8> {
    let mut container = original[..32].to_vec();
    container[16..24].copy_from_slice(&(log.len() as u64).to_le_bytes());
    container[24..28].copy_from_slice(&crc32fast::hash(log).to_le_bytes());
    container.extend_from_slice(log);
    container
}

fn append_frame(log: &mut Vec<u8>, payload: &[u8]) {
    let length = payload.len() as u32;
    log.extend_from_slice(&length.to_le_bytes());
    log.extend_from_slice(&(!length).to_le_bytes());
    log.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    log.extend_from_slice(payload);
}

#[test]
fn valid_outer_crc_cannot_hide_invalid_log_frames_operations_or_content_limits() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.join("source")).unwrap();
    let backup = directory.join("valid.cedrobak");
    database.backup(&backup).unwrap();
    let original = fs::read(&backup).unwrap();
    let valid_header = &original[32..48];
    let mut invalid_operation = valid_header.to_vec();
    append_frame(
        &mut invalid_operation,
        br#"{"ops":[{"kind":"put","collection":"docs","key":"","value":1}]}"#,
    );
    let mut invalid_json = valid_header.to_vec();
    append_frame(&mut invalid_json, br#"{"ops":[invalid-json]}"#);
    let mut wrong_length_complement = valid_header.to_vec();
    append_frame(&mut wrong_length_complement, br#"{"ops":[]}"#);
    wrong_length_complement[20] ^= 1;
    let mut wrong_frame_checksum = valid_header.to_vec();
    append_frame(&mut wrong_frame_checksum, br#"{"ops":[]}"#);
    wrong_frame_checksum[24] ^= 1;
    let mut invalid_header = valid_header.to_vec();
    invalid_header[0] ^= 1;
    let mut torn_frame = valid_header.to_vec();
    torn_frame.extend_from_slice(&[1, 2, 3]);
    for (index, log) in [
        invalid_operation,
        invalid_json,
        wrong_length_complement,
        wrong_frame_checksum,
        invalid_header,
        torn_frame,
    ]
    .into_iter()
    .enumerate()
    {
        let candidate = directory.join(format!("invalid-log-{index}.cedrobak"));
        let destination = directory.join(format!("destination-{index}"));
        let container = replace_container_log(&original, &log);
        fs::write(&candidate, &container).unwrap();
        assert!(matches!(
            Database::restore_backup(&candidate, &destination),
            Err(Error::Corruption(_))
        ));
        assert!(!destination.exists());
        assert_eq!(fs::read(candidate).unwrap(), container);
    }
    database
        .put("docs", "larger", json!({"text":"x".repeat(100)}))
        .unwrap();
    let larger = directory.join("larger.cedrobak");
    database.backup(&larger).unwrap();
    let limited = directory.join("limited");
    let options = Options {
        max_document_bytes: 16,
        ..Options::default()
    };
    assert!(matches!(
        Database::restore_backup_with_options(&larger, &limited, options),
        Err(Error::Corruption(_))
    ));
    assert!(!limited.exists());
    let state_limited = directory.join("state-limited");
    assert!(matches!(
        Database::restore_backup_with_options(
            &larger,
            &state_limited,
            Options {
                max_state_bytes: 16,
                ..Options::default()
            }
        ),
        Err(Error::Corruption(_))
    ));
    assert!(!state_limited.exists());
    let restored = directory.join("restored");
    assert_eq!(
        Database::restore_backup_with_options(&larger, &restored, Options::default())
            .unwrap()
            .records,
        1
    );
}

#[test]
fn maintenance_rejects_active_transactions_without_publishing_backups() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.join("source")).unwrap();
    database.put("docs", "committed", json!(1)).unwrap();
    let mut transaction = database.begin_write().unwrap();
    transaction.put("docs", "staged", json!(2)).unwrap();
    let backup = directory.join("busy.cedrobak");
    assert!(matches!(database.verify(), Err(Error::WriterBusy)));
    assert!(matches!(database.backup(&backup), Err(Error::WriterBusy)));
    assert!(!backup.exists());
    drop(transaction);
    assert_eq!(database.verify().unwrap().records, 1);
    assert_eq!(database.backup(&backup).unwrap().records, 1);
    assert!(database.get("docs", "staged").unwrap().is_none());
}
