use cedrodb::{Database, Options};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("cedrodb-{label}-{}-{sequence}", std::process::id()));
        fs::create_dir(&path).expect("create an isolated test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn log(&self) -> PathBuf {
        self.0.join("cedro.log")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn populate_baseline(directory: &Path) {
    let database = Database::open(directory).unwrap();
    let mut transaction = database.begin_write().unwrap();
    transaction
        .put("records", "original", json!({"value": "durable"}))
        .unwrap();
    transaction.commit().unwrap();
}

#[test]
fn committed_changes_and_deletions_survive_reopening() {
    let directory = TestDirectory::new("reopen");
    {
        let database = Database::open(directory.path()).unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction
            .put("users", "ana", json!({"name": "Ana", "age": 30}))
            .unwrap();
        transaction
            .put("users", "temporary", json!({"delete": true}))
            .unwrap();
        transaction.commit().unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction.delete("users", "temporary").unwrap();
        transaction
            .put("users", "ana", json!({"name": "Ana", "age": 31}))
            .unwrap();
        transaction.commit().unwrap();
    }

    let database = Database::open(directory.path()).unwrap();
    assert_eq!(
        database.get("users", "ana").unwrap(),
        Some(json!({"name": "Ana", "age": 31}))
    );
    assert_eq!(database.get("users", "temporary").unwrap(), None);
}

#[test]
fn dropping_a_transaction_rolls_back_its_entire_batch() {
    let directory = TestDirectory::new("rollback");
    populate_baseline(directory.path());
    let bytes_before = fs::read(directory.log()).unwrap();
    {
        let database = Database::open(directory.path()).unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction.delete("records", "original").unwrap();
        transaction
            .put("records", "uncommitted", json!({"value": 2}))
            .unwrap();
        drop(transaction);
        assert_eq!(
            database.get("records", "original").unwrap(),
            Some(json!({"value": "durable"}))
        );
        assert_eq!(database.get("records", "uncommitted").unwrap(), None);
    }
    assert_eq!(fs::read(directory.log()).unwrap(), bytes_before);
    let database = Database::open(directory.path()).unwrap();
    assert_eq!(database.get("records", "uncommitted").unwrap(), None);
}

#[test]
fn readers_only_observe_a_batch_after_commit() {
    let directory = TestDirectory::new("visibility");
    let database = Database::open(directory.path()).unwrap();
    let mut transaction = database.begin_write().unwrap();
    transaction.put("batch", "one", json!({"n": 1})).unwrap();
    transaction.put("batch", "two", json!({"n": 2})).unwrap();
    assert!(database.scan("batch").unwrap().is_empty());
    transaction.commit().unwrap();
    assert_eq!(database.scan("batch").unwrap().len(), 2);
}

#[test]
fn a_second_database_handle_cannot_open_the_same_storage() {
    let directory = TestDirectory::new("exclusive");
    let first = Database::open(directory.path()).unwrap();
    assert!(Database::open(directory.path()).is_err());
    drop(first);
    assert!(Database::open(directory.path()).is_ok());
}

#[test]
fn every_torn_transaction_boundary_recovers_only_preceding_commits() {
    let source = TestDirectory::new("torn-source");
    populate_baseline(source.path());
    let baseline = fs::read(source.log()).unwrap();
    {
        let database = Database::open(source.path()).unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction.delete("records", "original").unwrap();
        transaction
            .put("records", "replacement", json!({"value": "new", "n": 42}))
            .unwrap();
        transaction.commit().unwrap();
    }
    let complete = fs::read(source.log()).unwrap();
    assert!(complete.len() > baseline.len());

    for cut in baseline.len()..complete.len() {
        let candidate = TestDirectory::new("torn-candidate");
        fs::write(candidate.log(), &complete[..cut]).unwrap();
        {
            let database = Database::open(candidate.path())
                .unwrap_or_else(|error| panic!("recovery failed at byte {cut}: {error}"));
            assert_eq!(
                database.get("records", "original").unwrap(),
                Some(json!({"value": "durable"})),
                "lost committed document at byte {cut}"
            );
            assert_eq!(
                database.get("records", "replacement").unwrap(),
                None,
                "partially applied transaction at byte {cut}"
            );
            let mut transaction = database.begin_write().unwrap();
            transaction
                .put("after", "recovery", json!({"ok": true}))
                .unwrap();
            transaction.commit().unwrap();
        }
        let database = Database::open(candidate.path()).unwrap();
        assert_eq!(
            database.get("after", "recovery").unwrap(),
            Some(json!({"ok": true}))
        );
        assert_eq!(database.get("records", "replacement").unwrap(), None);
    }
}

#[test]
fn an_incomplete_tail_is_truncated_to_the_previous_transaction() {
    let directory = TestDirectory::new("truncate");
    populate_baseline(directory.path());
    let baseline = fs::read(directory.log()).unwrap();
    let mut damaged = baseline.clone();
    damaged.extend_from_slice(&[0x01, 0x02, 0x03]);
    fs::write(directory.log(), damaged).unwrap();
    let database = Database::open(directory.path()).unwrap();
    assert_eq!(fs::read(directory.log()).unwrap(), baseline);
    assert!(database.get("records", "original").unwrap().is_some());
}

#[test]
fn complete_checksum_corruption_fails_without_modifying_storage() {
    let directory = TestDirectory::new("checksum");
    populate_baseline(directory.path());
    let mut corrupted = fs::read(directory.log()).unwrap();
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0x80;
    fs::write(directory.log(), &corrupted).unwrap();
    assert!(Database::open(directory.path()).is_err());
    assert_eq!(fs::read(directory.log()).unwrap(), corrupted);
}

#[test]
fn an_oversized_declared_frame_is_rejected_without_repair() {
    let directory = TestDirectory::new("oversized");
    populate_baseline(directory.path());
    let mut corrupted = fs::read(directory.log()).unwrap();
    corrupted[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    // Keep the length's complement valid to exercise the size bound itself.
    corrupted[20..24].copy_from_slice(&0u32.to_le_bytes());
    fs::write(directory.log(), &corrupted).unwrap();
    assert!(Database::open(directory.path()).is_err());
    assert_eq!(fs::read(directory.log()).unwrap(), corrupted);
}

#[test]
fn corruption_in_any_length_byte_cannot_discard_a_committed_transaction() {
    let source = TestDirectory::new("length-source");
    populate_baseline(source.path());
    let complete = fs::read(source.log()).unwrap();
    for byte in 0..4 {
        let directory = TestDirectory::new("length-corruption");
        let mut corrupted = complete.clone();
        corrupted[16 + byte] ^= 1;
        fs::write(directory.log(), &corrupted).unwrap();
        // Even a large plausible declared length must fail its integrity check
        // before recovery considers it an interrupted write or allocates it.
        let options = Options {
            max_frame_bytes: u32::MAX as usize,
            ..Options::default()
        };
        assert!(
            Database::open_with_options(directory.path(), options).is_err(),
            "length byte {byte} was accepted"
        );
        assert_eq!(
            fs::read(directory.log()).unwrap(),
            corrupted,
            "length byte {byte} caused destructive recovery"
        );
    }
}

#[test]
fn partial_or_unrecognized_file_headers_are_rejected_without_repair() {
    let source = TestDirectory::new("header-source");
    {
        let _database = Database::open(source.path()).unwrap();
    }
    let valid = fs::read(source.log()).unwrap();
    assert_eq!(valid.len(), 16);

    for length in 1..16 {
        let directory = TestDirectory::new("short-header");
        fs::write(directory.log(), &valid[..length]).unwrap();
        assert!(Database::open(directory.path()).is_err(), "length {length}");
        assert_eq!(fs::read(directory.log()).unwrap(), &valid[..length]);
    }

    for position in [0, 8, 12] {
        let directory = TestDirectory::new("bad-header");
        let mut invalid = valid.clone();
        invalid[position] ^= 0xff;
        fs::write(directory.log(), &invalid).unwrap();
        assert!(Database::open(directory.path()).is_err(), "byte {position}");
        assert_eq!(fs::read(directory.log()).unwrap(), invalid);
    }
}

#[test]
fn commit_survives_process_exit_without_running_destructors() {
    const CHILD_DIRECTORY: &str = "CEDRODB_DURABILITY_CHILD_DIRECTORY";
    if let Some(directory) = std::env::var_os(CHILD_DIRECTORY) {
        let database = Database::open(PathBuf::from(directory)).unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction
            .put("crash", "committed", json!({"survived": true}))
            .unwrap();
        transaction.commit().unwrap();
        // Exits without dropping either the database or the transaction owner.
        std::process::exit(0);
    }

    let directory = TestDirectory::new("process-exit");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "commit_survives_process_exit_without_running_destructors",
            "--nocapture",
        ])
        .env(CHILD_DIRECTORY, directory.path())
        .status()
        .unwrap();
    assert!(status.success());
    // Reopening also verifies that the operating system releases the file lock.
    let database = Database::open(directory.path()).unwrap();
    assert_eq!(
        database.get("crash", "committed").unwrap(),
        Some(json!({"survived": true}))
    );
}
