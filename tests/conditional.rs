use cedrodb::{ChangeOperation, Condition, Database, Error, Filter, FilterOp, Options, Query};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cedrodb-conditional-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn absent(collection: &str, key: &str) -> Condition {
    Condition::Absent {
        collection: collection.into(),
        key: key.into(),
    }
}

fn equals(collection: &str, key: &str, value: Value) -> Condition {
    Condition::Equals {
        collection: collection.into(),
        key: key.into(),
        value,
    }
}

fn put(collection: &str, key: &str, value: Value) -> ChangeOperation {
    ChangeOperation::Put {
        collection: collection.into(),
        key: key.into(),
        value,
    }
}

fn delete(collection: &str, key: &str) -> ChangeOperation {
    ChangeOperation::Delete {
        collection: collection.into(),
        key: key.into(),
    }
}

#[test]
fn competing_stock_reservations_commit_one_order_and_one_outbox_event_in_one_frame() {
    let directory = TestDirectory::new();
    let database = Arc::new(Database::open(&directory.0).unwrap());
    database
        .put("stock", "last-item", json!({"available":1}))
        .unwrap();
    let barrier = Arc::new(Barrier::new(24));
    let attempts: Vec<_> = (0..24)
        .map(|buyer| {
            let database = Arc::clone(&database);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let key = format!("buyer:{buyer:02}");
                let conditions = [
                    equals("stock", "last-item", json!({"available":1})),
                    absent("orders", &key),
                    absent("outbox", &key),
                ];
                let operations = [
                    put("stock", "last-item", json!({"available":0})),
                    put("orders", &key, json!({"item":"last-item"})),
                    put("outbox", &key, json!({"kind":"order.created"})),
                ];
                barrier.wait();
                database
                    .conditional_batch(&conditions, &operations)
                    .unwrap()
            })
        })
        .collect();
    let results: Vec<_> = attempts
        .into_iter()
        .map(|attempt| attempt.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.applied).count(), 1);
    assert!(results.iter().all(|result| result.cursor == 2));
    assert!(results
        .iter()
        .filter(|result| !result.applied)
        .all(|result| result.failed_condition == Some(0)));
    assert_eq!(database.scan("orders").unwrap().len(), 1);
    assert_eq!(database.scan("outbox").unwrap().len(), 1);
    assert_eq!(
        database.get("stock", "last-item").unwrap(),
        Some(json!({"available":0}))
    );
    let changes = database.changes(1, 100).unwrap();
    assert_eq!(changes.transactions.len(), 1);
    assert_eq!(changes.transactions[0].operations.len(), 3);
    assert_eq!(changes.transactions[0].cursor, 2);
    drop(database);
    let reopened = Database::open(&directory.0).unwrap();
    assert_eq!(reopened.stats().unwrap().latest_cursor, 2);
    assert_eq!(reopened.changes(1, 100).unwrap(), changes);
    assert_eq!(reopened.scan("orders").unwrap().len(), 1);
    assert_eq!(reopened.scan("outbox").unwrap().len(), 1);
}

#[test]
fn first_failed_condition_distinguishes_absence_from_null_without_advancing_history() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database.put("docs", "null", Value::Null).unwrap();
    let before = database.stats().unwrap();
    let conditions = [
        equals("docs", "null", Value::Null),
        equals("docs", "missing", Value::Null),
        absent("docs", "null"),
    ];
    let result = database
        .conditional_batch(&conditions, &[put("docs", "uncommitted", json!(1))])
        .unwrap();
    assert!(!result.applied);
    assert_eq!(result.failed_condition, Some(1));
    assert_eq!(result.cursor, before.latest_cursor);
    assert_eq!(database.stats().unwrap(), before);
    assert!(database.get("docs", "uncommitted").unwrap().is_none());
    assert_eq!(
        database
            .conditional_batch(&[absent("docs", "null")], &[])
            .unwrap()
            .failed_condition,
        Some(0)
    );
    let success = database
        .conditional_batch(
            &[
                absent("docs", "missing"),
                equals("docs", "null", Value::Null),
            ],
            &[],
        )
        .unwrap();
    assert!(success.applied);
    assert_eq!(success.failed_condition, None);
    assert_eq!(success.cursor, before.latest_cursor);
    assert_eq!(database.stats().unwrap(), before);
}

#[test]
fn conditional_numeric_equality_is_recursive_safe_and_never_confuses_bool_or_large_integers() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .put(
            "docs",
            "nested",
            json!({"numbers":[1.0,{"zero":-0.0}],"enabled":true}),
        )
        .unwrap();
    let matched = database
        .conditional_batch(
            &[equals(
                "docs",
                "nested",
                json!({"numbers":[1,{"zero":0}],"enabled":true}),
            )],
            &[put("results", "safe-numeric", json!(true))],
        )
        .unwrap();
    assert!(matched.applied);
    let before = database.stats().unwrap();
    let boolean_mismatch = database
        .conditional_batch(
            &[equals(
                "docs",
                "nested",
                json!({"numbers":[1,{"zero":0}],"enabled":1}),
            )],
            &[delete("docs", "nested")],
        )
        .unwrap();
    assert!(!boolean_mismatch.applied);
    assert_eq!(database.stats().unwrap(), before);
    database
        .put("docs", "large", json!(9_007_199_254_740_993u64))
        .unwrap();
    database.put("docs", "maximum", json!(u64::MAX)).unwrap();
    let large_before = database.stats().unwrap();
    for (key, value) in [
        ("large", json!(9_007_199_254_740_993u64 as f64)),
        ("maximum", json!(u64::MAX as f64)),
    ] {
        assert!(
            !database
                .conditional_batch(&[equals("docs", key, value)], &[delete("docs", key)])
                .unwrap()
                .applied
        );
        assert_eq!(database.stats().unwrap(), large_before);
    }
    assert!(
        database
            .conditional_batch(&[equals("docs", "maximum", json!(u64::MAX))], &[])
            .unwrap()
            .applied
    );
}

#[test]
fn all_conditions_observe_committed_prestate_and_repeated_operations_keep_their_order() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .put("docs", "existing", json!({"version":0}))
        .unwrap();
    let operations = [
        put("docs", "new", json!({"version":1})),
        put("docs", "existing", json!({"version":1})),
        put("docs", "temporary", json!(1)),
        delete("docs", "temporary"),
    ];
    let impossible = database
        .conditional_batch(
            &[
                absent("docs", "new"),
                equals("docs", "existing", json!({"version":1})),
            ],
            &operations,
        )
        .unwrap();
    assert!(!impossible.applied);
    assert_eq!(impossible.failed_condition, Some(1));
    assert_eq!(impossible.cursor, 1);
    let applied = database
        .conditional_batch(
            &[
                absent("docs", "new"),
                equals("docs", "existing", json!({"version":0})),
                absent("docs", "temporary"),
            ],
            &operations,
        )
        .unwrap();
    assert!(applied.applied);
    assert_eq!(applied.cursor, 2);
    assert!(database.get("docs", "temporary").unwrap().is_none());
    assert_eq!(
        database.get("docs", "existing").unwrap(),
        Some(json!({"version":1}))
    );
    let change = database.changes(1, 100).unwrap();
    assert_eq!(change.transactions.len(), 1);
    assert_eq!(change.transactions[0].operations, operations);
}

#[test]
fn invalid_identifiers_condition_counts_and_document_limits_do_not_mutate_anything() {
    let directory = TestDirectory::new();
    let database = Database::open_with_options(
        &directory.0,
        Options {
            max_document_bytes: 32,
            ..Options::default()
        },
    )
    .unwrap();
    database.put("docs", "known", json!(true)).unwrap();
    let before = database.stats().unwrap();
    let valid = [put("docs", "result", json!(1))];
    assert!(matches!(
        database.conditional_batch(&[absent("", "a")], &valid),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        database.conditional_batch(&[absent("docs", "")], &valid),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        database.conditional_batch(&vec![absent("docs", "missing"); 65], &valid),
        Err(Error::InvalidInput(_))
    ));
    assert!(
        database
            .conditional_batch(&vec![absent("docs", "missing"); 64], &[])
            .unwrap()
            .applied
    );
    let oversized = json!({"text":"x".repeat(100)});
    assert!(matches!(
        database.conditional_batch(&[equals("docs", "known", oversized.clone())], &valid),
        Err(Error::LimitExceeded(_))
    ));
    for operation in [
        put("", "a", json!(1)),
        delete("docs", ""),
        put("docs", "a", oversized),
    ] {
        assert!(database
            .conditional_batch(&[absent("docs", "known")], &[operation])
            .is_err());
    }
    assert_eq!(database.stats().unwrap(), before);
    assert!(database.get("docs", "result").unwrap().is_none());
    let oversized_collection = "c".repeat(1025);
    assert!(database
        .conditional_batch(&[absent(&oversized_collection, "key")], &valid)
        .is_err());
    let oversized_key = "k".repeat(4097);
    assert!(database
        .conditional_batch(&[], &[delete("docs", &oversized_key)])
        .is_err());
    assert_eq!(database.stats().unwrap(), before);
}

#[test]
fn exact_encoded_frame_limit_includes_array_commas_and_rejects_one_extra_byte() {
    let operations = [
        put("docs", "a", json!({"text":"x".repeat(25)})),
        put("docs", "b", json!({"text":"y".repeat(30)})),
    ];
    let exact_bytes = serde_json::to_vec(&json!({"ops":operations}))
        .unwrap()
        .len();
    let too_small = TestDirectory::new();
    let small = Database::open_with_options(
        &too_small.0,
        Options {
            max_frame_bytes: exact_bytes - 1,
            ..Options::default()
        },
    )
    .unwrap();
    let before = small.stats().unwrap();
    assert!(matches!(
        small.conditional_batch(&[], &operations),
        Err(Error::LimitExceeded(_))
    ));
    assert_eq!(small.stats().unwrap(), before);
    assert!(small.scan("docs").unwrap().is_empty());
    let directory = TestDirectory::new();
    let options = Options {
        max_frame_bytes: exact_bytes,
        ..Options::default()
    };
    let database = Database::open_with_options(&directory.0, options).unwrap();
    let applied = database.conditional_batch(&[], &operations).unwrap();
    assert!(applied.applied);
    assert_eq!(applied.cursor, 1);
    assert_eq!(
        database.stats().unwrap().log_bytes,
        (16 + 12 + exact_bytes) as u64
    );
    let before = database.stats().unwrap();
    let one_more_byte = [
        put("docs", "a", json!({"text":"x".repeat(26)})),
        operations[1].clone(),
    ];
    assert!(matches!(
        database.conditional_batch(&[], &one_more_byte),
        Err(Error::LimitExceeded(_))
    ));
    assert_eq!(database.stats().unwrap(), before);
    drop(database);
    let reopened = Database::open_with_options(&directory.0, options).unwrap();
    assert_eq!(
        reopened.changes(0, 100).unwrap().transactions[0].operations,
        operations
    );
}

#[test]
fn active_writer_rejects_conditional_batch_and_empty_success_does_not_advance_cursor() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database.put("docs", "committed", json!(true)).unwrap();
    let before = database.stats().unwrap();
    let mut transaction = database.begin_write().unwrap();
    transaction.put("docs", "staged", json!(1)).unwrap();
    assert!(matches!(
        database.conditional_batch(&[], &[]),
        Err(Error::WriterBusy)
    ));
    assert!(matches!(
        database.conditional_batch(
            &[absent("docs", "result")],
            &[put("docs", "result", json!(1))]
        ),
        Err(Error::WriterBusy)
    ));
    assert_eq!(database.stats().unwrap(), before);
    drop(transaction);
    let empty = database.conditional_batch(&[], &[]).unwrap();
    assert!(empty.applied);
    assert_eq!(empty.cursor, 1);
    assert_eq!(database.stats().unwrap(), before);
    assert!(database.get("docs", "staged").unwrap().is_none());
}

fn indexed_state(database: &Database, state: &str) -> Vec<String> {
    let page = database
        .query(
            "jobs",
            Query {
                filters: vec![Filter {
                    field: "state".into(),
                    op: FilterOp::Eq,
                    value: json!(state),
                }],
                ..Query::default()
            },
        )
        .unwrap();
    assert_eq!(page.used_index.as_deref(), Some("state"));
    page.records.into_iter().map(|record| record.key).collect()
}

#[test]
fn conditional_batches_update_indexes_only_for_the_final_committed_values() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database.create_index("jobs", "state").unwrap();
    database
        .put("jobs", "a", json!({"state":"pending"}))
        .unwrap();
    database
        .put("jobs", "b", json!({"state":"pending"}))
        .unwrap();
    let conditions = [
        equals("jobs", "a", json!({"state":"pending"})),
        equals("jobs", "b", json!({"state":"pending"})),
    ];
    let operations = [
        put("jobs", "a", json!({"state":"running"})),
        put("jobs", "a", json!({"state":"done"})),
        delete("jobs", "b"),
        put("jobs", "c", json!({"state":"done"})),
    ];
    let before = database.stats().unwrap();
    let mismatch = database
        .conditional_batch(&[absent("jobs", "a")], &operations)
        .unwrap();
    assert!(!mismatch.applied);
    assert_eq!(database.stats().unwrap(), before);
    assert_eq!(indexed_state(&database, "pending"), ["a", "b"]);
    assert!(indexed_state(&database, "done").is_empty());
    let result = database
        .conditional_batch(&conditions, &operations)
        .unwrap();
    assert!(result.applied);
    assert_eq!(result.cursor, before.latest_cursor + 1);
    assert!(indexed_state(&database, "pending").is_empty());
    assert!(indexed_state(&database, "running").is_empty());
    assert_eq!(indexed_state(&database, "done"), ["a", "c"]);
    assert_eq!(
        database
            .changes(before.latest_cursor, 100)
            .unwrap()
            .transactions[0]
            .operations,
        operations
    );
}

#[test]
fn failed_state_growth_does_not_publish_partial_operations_or_index_updates() {
    let directory = TestDirectory::new();
    let database = Database::open_with_options(
        &directory.0,
        Options {
            max_state_bytes: 96,
            ..Options::default()
        },
    )
    .unwrap();
    database.create_index("jobs", "state").unwrap();
    database
        .put("jobs", "a", json!({"state":"pending"}))
        .unwrap();
    let before = database.stats().unwrap();
    let operations = [
        put("jobs", "a", json!({"state":"done"})),
        put("jobs", "huge", json!({"padding":"x".repeat(100)})),
    ];
    assert!(matches!(
        database.conditional_batch(
            &[equals("jobs", "a", json!({"state":"pending"}))],
            &operations
        ),
        Err(Error::LimitExceeded(_))
    ));
    assert_eq!(database.stats().unwrap(), before);
    assert_eq!(indexed_state(&database, "pending"), ["a"]);
    assert!(indexed_state(&database, "done").is_empty());
    assert!(database.get("jobs", "huge").unwrap().is_none());
}

#[test]
fn condition_wire_format_rejects_ambiguous_unknown_or_incomplete_predicates() {
    assert!(matches!(
        serde_json::from_value::<Condition>(json!({"kind":"absent","collection":"docs","key":"a"}))
            .unwrap(),
        Condition::Absent { .. }
    ));
    assert!(matches!(
        serde_json::from_value::<Condition>(
            json!({"kind":"equals","collection":"docs","key":"a","value":null})
        )
        .unwrap(),
        Condition::Equals {
            value: Value::Null,
            ..
        }
    ));
    for invalid in [
        json!({"kind":"absent","collection":"docs","key":"a","value":null}),
        json!({"kind":"equals","collection":"docs","key":"a"}),
        json!({"kind":"equal","collection":"docs","key":"a","value":1}),
    ] {
        assert!(serde_json::from_value::<Condition>(invalid).is_err());
    }
}
