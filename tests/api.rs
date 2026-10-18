use cedrodb::{Database, Options};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("cedrodb-api-{}-{sequence}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn values_preserve_unicode_integer_ranges_and_null() {
    let directory = TestDirectory::new();
    let expected = json!({
        "nombre": "José 🌲",
        "minimum": i64::MIN,
        "maximum": u64::MAX,
        "nested": [true, false, null, {"dato": "áéíóú"}]
    });
    {
        let database = Database::open(directory.path()).unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction
            .put("colección", "árbol", expected.clone())
            .unwrap();
        transaction.put("colección", "null", Value::Null).unwrap();
        transaction.commit().unwrap();
    }
    let database = Database::open(directory.path()).unwrap();
    assert_eq!(database.get("colección", "árbol").unwrap(), Some(expected));
    assert_eq!(
        database.get("colección", "null").unwrap(),
        Some(Value::Null)
    );
    assert_eq!(database.get("colección", "missing").unwrap(), None);
}

#[test]
fn scans_are_sorted_and_collections_are_isolated() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.path()).unwrap();
    let mut transaction = database.begin_write().unwrap();
    for key in ["z", "a", "m"] {
        transaction.put("first", key, json!({"key": key})).unwrap();
    }
    transaction
        .put("second", "a", json!({"different": true}))
        .unwrap();
    transaction.commit().unwrap();
    let records = database.scan("first").unwrap();
    assert_eq!(
        records
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["a", "m", "z"]
    );
    assert_eq!(database.scan("second").unwrap().len(), 1);
    assert!(database.scan("missing").unwrap().is_empty());
    assert_ne!(
        database.get("first", "a").unwrap(),
        database.get("second", "a").unwrap()
    );
}

#[test]
fn operations_in_a_transaction_are_applied_in_order() {
    let directory = TestDirectory::new();
    {
        let database = Database::open(directory.path()).unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction
            .put("records", "key", json!({"version": 1}))
            .unwrap();
        transaction.delete("records", "key").unwrap();
        transaction
            .put("records", "key", json!({"version": 2}))
            .unwrap();
        transaction.delete("records", "absent").unwrap();
        transaction.commit().unwrap();
    }
    let database = Database::open(directory.path()).unwrap();
    assert_eq!(
        database.scan("records").unwrap(),
        vec![("key".into(), json!({"version": 2}))]
    );
}

#[test]
fn empty_batches_and_deleting_absent_records_are_valid() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.path()).unwrap();
    database.begin_write().unwrap().commit().unwrap();
    let mut transaction = database.begin_write().unwrap();
    transaction.delete("missing", "absent").unwrap();
    transaction.commit().unwrap();
    assert!(database.scan("missing").unwrap().is_empty());
}

#[test]
fn empty_identifiers_are_rejected() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.path()).unwrap();
    let mut transaction = database.begin_write().unwrap();
    assert!(transaction.put("", "key", json!({})).is_err());
    assert!(transaction.put("records", "", json!({})).is_err());
    assert!(transaction.delete("", "key").is_err());
    assert!(transaction.delete("records", "").is_err());
}

#[test]
fn aggregate_ignores_missing_null_text_and_nested_fields() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.path()).unwrap();
    let mut transaction = database.begin_write().unwrap();
    let records = [
        json!({"amount": 10}),
        json!({"amount": -4}),
        json!({"amount": 2.5}),
        json!({"amount": null}),
        json!({"amount": "100"}),
        json!({"other": 100}),
        json!({"nested": {"amount": 100}}),
        json!({"amount": true}),
        Value::Null,
    ];
    for (index, record) in records.into_iter().enumerate() {
        transaction
            .put("payments", &index.to_string(), record)
            .unwrap();
    }
    transaction.commit().unwrap();
    let aggregate = database.aggregate("payments", "amount").unwrap();
    assert_eq!(aggregate.count, 3);
    assert_eq!(aggregate.sum, 8.5);
    assert_eq!(aggregate.min, Some(-4.0));
    assert_eq!(aggregate.max, Some(10.0));

    let empty = database.aggregate("missing", "amount").unwrap();
    assert_eq!(empty.count, 0);
    assert_eq!(empty.sum, 0.0);
    assert_eq!(empty.min, None);
    assert_eq!(empty.max, None);
}

#[test]
fn aggregate_rejects_a_nonfinite_sum() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.path()).unwrap();
    let mut transaction = database.begin_write().unwrap();
    transaction
        .put("large", "one", json!({"amount": 1.0e308}))
        .unwrap();
    transaction
        .put("large", "two", json!({"amount": 1.0e308}))
        .unwrap();
    transaction.commit().unwrap();
    assert!(database.aggregate("large", "amount").is_err());
    assert!(database.get("large", "one").unwrap().is_some());
}

#[test]
fn document_limit_rejects_a_write_without_changing_committed_state() {
    let directory = TestDirectory::new();
    let options = Options {
        max_document_bytes: 64,
        ..Options::default()
    };
    {
        let database = Database::open_with_options(directory.path(), options).unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction
            .put("records", "existing", json!({"ok": true}))
            .unwrap();
        transaction.commit().unwrap();
        let mut transaction = database.begin_write().unwrap();
        assert!(transaction
            .put("records", "huge", json!({"value": "x".repeat(1024)}))
            .is_err());
    }
    let database = Database::open(directory.path()).unwrap();
    assert_eq!(
        database.get("records", "existing").unwrap(),
        Some(json!({"ok": true}))
    );
    assert_eq!(database.get("records", "huge").unwrap(), None);
}

#[test]
fn transaction_limit_prevents_partial_persistence() {
    let directory = TestDirectory::new();
    let options = Options {
        max_frame_bytes: 512,
        ..Options::default()
    };
    {
        let database = Database::open_with_options(directory.path(), options).unwrap();
        let mut transaction = database.begin_write().unwrap();
        let mut rejected = false;
        for index in 0..30 {
            if transaction
                .put(
                    "records",
                    &index.to_string(),
                    json!({"payload": "x".repeat(50)}),
                )
                .is_err()
            {
                rejected = true;
                break;
            }
        }
        if rejected {
            drop(transaction);
        } else {
            assert!(transaction.commit().is_err());
        }
        assert!(database.scan("records").unwrap().is_empty());
    }
    let database = Database::open(directory.path()).unwrap();
    assert!(database.scan("records").unwrap().is_empty());
}

#[test]
fn state_limit_rejects_growth_and_preserves_previous_commits() {
    let directory = TestDirectory::new();
    let options = Options {
        max_state_bytes: 512,
        ..Options::default()
    };
    {
        let database = Database::open_with_options(directory.path(), options).unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction
            .put("records", "small", json!({"ok": true}))
            .unwrap();
        transaction.commit().unwrap();
        let mut transaction = database.begin_write().unwrap();
        if transaction
            .put("records", "huge", json!({"payload": "x".repeat(1024)}))
            .is_ok()
        {
            assert!(transaction.commit().is_err());
        }
        assert_eq!(
            database.get("records", "small").unwrap(),
            Some(json!({"ok": true}))
        );
        assert_eq!(database.get("records", "huge").unwrap(), None);
    }
    let database = Database::open(directory.path()).unwrap();
    assert_eq!(
        database.get("records", "small").unwrap(),
        Some(json!({"ok": true}))
    );
    assert_eq!(database.get("records", "huge").unwrap(), None);
}

#[test]
fn a_deep_document_cannot_create_an_unreadable_committed_log() {
    let directory = TestDirectory::new();
    let mut deeply_nested = json!("leaf");
    for _ in 0..150 {
        deeply_nested = Value::Array(vec![deeply_nested]);
    }
    let accepted;
    {
        let database = Database::open(directory.path()).unwrap();
        let mut transaction = database.begin_write().unwrap();
        accepted = transaction
            .put("records", "deep", deeply_nested.clone())
            .is_ok();
        if accepted {
            transaction.commit().unwrap();
        }
    }
    let database = Database::open(directory.path())
        .expect("every successfully committed document must be readable after reopening");
    assert_eq!(
        database.get("records", "deep").unwrap(),
        accepted.then_some(deeply_nested)
    );
}

#[test]
fn concurrent_one_shot_puts_serialize_instead_of_writer_busy() {
    let dir = TestDirectory::new();
    let db = std::sync::Arc::new(Database::open(dir.path()).unwrap());
    let handles: Vec<_> = (0..8)
        .map(|t| {
            let db = db.clone();
            std::thread::spawn(move || {
                for i in 0..50 {
                    db.put(
                        "c",
                        &format!("k{t}-{i}"),
                        serde_json::json!({"t": t, "i": i}),
                    )
                    .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(db.get("c", "k7-49").unwrap().unwrap()["i"], 49);
}
