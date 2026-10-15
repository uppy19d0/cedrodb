use cedrodb::{ChangeOperation, Database, Error, Filter, FilterOp, Options, Query};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cedrodb-advanced-{}-{}",
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

fn filter(field: &str, op: FilterOp, value: Value) -> Filter {
    Filter {
        field: field.into(),
        op,
        value,
    }
}

#[test]
fn indexed_pages_match_scan_pages_and_reduce_candidates() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    let mut transaction = database.begin_write().unwrap();
    for index in 0..100 {
        transaction
            .put(
                "events",
                &format!("{index:03}"),
                json!({"profile":{"city": if index % 10 == 0 {"SDQ"} else {"STI"}},"amount":index}),
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    let mut query = Query {
        filters: vec![
            filter("/profile/city", FilterOp::Eq, json!("SDQ")),
            filter("amount", FilterOp::Gte, json!(20)),
        ],
        limit: 3,
        after: None,
    };
    let mut expected = Vec::new();
    let mut scanned_without_index = 0;
    loop {
        let page = database.query("events", query.clone()).unwrap();
        scanned_without_index += page.scanned;
        expected.extend(page.records);
        query.after = page.next_after;
        if query.after.is_none() {
            break;
        }
    }
    database.create_index("events", "/profile/city").unwrap();
    query.after = None;
    let mut indexed = Vec::new();
    let mut scanned_with_index = 0;
    loop {
        let page = database.query("events", query.clone()).unwrap();
        assert_eq!(page.used_index.as_deref(), Some("/profile/city"));
        scanned_with_index += page.scanned;
        indexed.extend(page.records);
        query.after = page.next_after;
        if query.after.is_none() {
            break;
        }
    }
    assert_eq!(indexed, expected);
    assert_eq!(expected.len(), 8);
    assert!(scanned_with_index < scanned_without_index);
    assert!(indexed.windows(2).all(|pair| pair[0].key < pair[1].key));
}

#[test]
fn indexes_track_only_final_committed_values_and_survive_collection_recreation() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database.create_index("jobs", "state").unwrap();
    database
        .put("jobs", "a", json!({"state":"pending"}))
        .unwrap();
    let query = |state| Query {
        filters: vec![filter("state", FilterOp::Eq, json!(state))],
        ..Query::default()
    };
    let mut transaction = database.begin_write().unwrap();
    transaction
        .put("jobs", "a", json!({"state":"running"}))
        .unwrap();
    transaction
        .put("jobs", "a", json!({"state":"done"}))
        .unwrap();
    assert_eq!(
        database
            .query("jobs", query("pending"))
            .unwrap()
            .records
            .len(),
        1
    );
    assert!(database
        .query("jobs", query("done"))
        .unwrap()
        .records
        .is_empty());
    transaction.commit().unwrap();
    assert!(database
        .query("jobs", query("pending"))
        .unwrap()
        .records
        .is_empty());
    assert!(database
        .query("jobs", query("running"))
        .unwrap()
        .records
        .is_empty());
    assert_eq!(
        database.query("jobs", query("done")).unwrap().records.len(),
        1
    );
    let mut rollback = database.begin_write().unwrap();
    rollback.delete("jobs", "a").unwrap();
    drop(rollback);
    assert_eq!(
        database.query("jobs", query("done")).unwrap().records.len(),
        1
    );
    database.delete("jobs", "a").unwrap();
    assert_eq!(database.stats().unwrap().collections, 0);
    database
        .put("jobs", "b", json!({"state":"pending"}))
        .unwrap();
    assert_eq!(
        database.query("jobs", query("pending")).unwrap().records[0].key,
        "b"
    );
    drop(database);
    let reopened = Database::open(&directory.0).unwrap();
    assert_eq!(reopened.stats().unwrap().indexes, 0);
    assert_eq!(
        reopened.query("jobs", query("pending")).unwrap().records[0].key,
        "b"
    );
}

#[test]
fn index_equality_preserves_structures_and_signed_float_zero() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .put("values", "negative-zero", json!({"value":{"a":[-0.0]}}))
        .unwrap();
    database
        .put("values", "positive-zero", json!({"value":{"a":[0.0]}}))
        .unwrap();
    database
        .put("values", "integer-zero", json!({"value":{"a":[0]}}))
        .unwrap();
    let query = Query {
        filters: vec![filter("value", FilterOp::Eq, json!({"a":[0.0]}))],
        ..Query::default()
    };
    let baseline = database.query("values", query.clone()).unwrap().records;
    assert_eq!(baseline.len(), 3);
    database.create_index("values", "value").unwrap();
    assert_eq!(database.query("values", query).unwrap().records, baseline);
    database.delete("values", "negative-zero").unwrap();
    assert_eq!(
        database
            .query(
                "values",
                Query {
                    filters: vec![filter("value", FilterOp::Eq, json!({"a":[-0.0]}))],
                    ..Query::default()
                }
            )
            .unwrap()
            .records
            .len(),
        2
    );
}

#[test]
fn pointer_escapes_missing_and_null_have_explicit_semantics() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .put(
            "docs",
            "a",
            json!({"a/b":{"~":[null,"yes"]},"nullable":null}),
        )
        .unwrap();
    database.put("docs", "b", json!({})).unwrap();
    let query = |field, op, value| Query {
        filters: vec![filter(field, op, value)],
        ..Query::default()
    };
    assert_eq!(
        database
            .query("docs", query("/a~1b/~0/1", FilterOp::Eq, json!("yes")))
            .unwrap()
            .records[0]
            .key,
        "a"
    );
    assert_eq!(
        database
            .query("docs", query("nullable", FilterOp::Exists, json!(true)))
            .unwrap()
            .records[0]
            .key,
        "a"
    );
    assert_eq!(
        database
            .query("docs", query("nullable", FilterOp::Exists, json!(false)))
            .unwrap()
            .records[0]
            .key,
        "b"
    );
    assert_eq!(
        database
            .query("docs", query("nullable", FilterOp::Eq, Value::Null))
            .unwrap()
            .records
            .len(),
        1
    );
    assert!(database
        .query("docs", query("nullable", FilterOp::Ne, Value::Null))
        .unwrap()
        .records
        .is_empty());
    assert!(database
        .query("docs", query("/bad~2escape", FilterOp::Eq, json!(1)))
        .is_err());
    assert!(database
        .query("docs", query("nullable", FilterOp::Exists, json!(1)))
        .is_err());
}

#[test]
fn ordering_never_rounds_large_integers_into_equal_values() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .put("numbers", "a", json!({"n":9_007_199_254_740_992u64}))
        .unwrap();
    database
        .put("numbers", "b", json!({"n":9_007_199_254_740_993u64}))
        .unwrap();
    database.put("numbers", "c", json!({"n":u64::MAX})).unwrap();
    database.put("numbers", "d", json!({"n":i64::MIN})).unwrap();
    let query = |op, value| Query {
        filters: vec![filter("n", op, value)],
        ..Query::default()
    };
    assert_eq!(
        database
            .query(
                "numbers",
                query(FilterOp::Gt, json!(9_007_199_254_740_992u64))
            )
            .unwrap()
            .records
            .iter()
            .map(|record| record.key.as_str())
            .collect::<Vec<_>>(),
        ["b", "c"]
    );
    assert_eq!(
        database
            .query("numbers", query(FilterOp::Lt, json!(0)))
            .unwrap()
            .records[0]
            .key,
        "d"
    );
    assert!(matches!(
        database.query("numbers", query(FilterOp::Gte, json!(0.5))),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn compare_exchange_has_one_winner_and_mismatch_never_appends() {
    let directory = TestDirectory::new();
    let database = Arc::new(Database::open(&directory.0).unwrap());
    database.put("state", "version", json!(0)).unwrap();
    let barrier = Arc::new(Barrier::new(16));
    let threads: Vec<_> = (1..=16)
        .map(|version| {
            let database = Arc::clone(&database);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                database
                    .compare_exchange("state", "version", Some(json!(0)), Some(json!(version)))
                    .unwrap()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
    assert_eq!(database.stats().unwrap().latest_cursor, 2);
    let bytes = database.stats().unwrap().log_bytes;
    assert!(!database
        .compare_exchange("state", "version", Some(json!(0)), None)
        .unwrap());
    assert_eq!(database.stats().unwrap().log_bytes, bytes);
    let final_value = database.get("state", "version").unwrap();
    drop(database);
    let reopened = Database::open(&directory.0).unwrap();
    assert_eq!(reopened.get("state", "version").unwrap(), final_value);
}

#[test]
fn compare_exchange_distinguishes_absence_and_json_null_and_blocks_staged_writer() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    assert!(database
        .compare_exchange("state", "a", None, Some(Value::Null))
        .unwrap());
    assert!(!database
        .compare_exchange("state", "a", None, Some(json!(1)))
        .unwrap());
    assert!(database
        .compare_exchange("state", "a", Some(Value::Null), None)
        .unwrap());
    assert!(database.compare_exchange("state", "a", None, None).unwrap());
    assert_eq!(database.stats().unwrap().latest_cursor, 2);
    let transaction = database.begin_write().unwrap();
    assert!(matches!(
        database.compare_exchange("state", "a", None, Some(json!(1))),
        Err(Error::WriterBusy)
    ));
    drop(transaction);
    assert!(database
        .compare_exchange("state", "a", None, Some(json!(1)))
        .unwrap());
}

#[test]
fn changefeed_preserves_atomic_outbox_batches_and_resumes_after_reopening() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    let mut transaction = database.begin_write().unwrap();
    transaction
        .put("orders", "1", json!({"status":"created"}))
        .unwrap();
    transaction
        .put("outbox", "order:1", json!({"event":"order.created"}))
        .unwrap();
    transaction.commit().unwrap();
    database
        .put("orders", "2", json!({"status":"created"}))
        .unwrap();
    let first = database.changes(0, 1).unwrap();
    assert_eq!(first.transactions.len(), 1);
    assert_eq!(first.transactions[0].operations.len(), 2);
    assert_eq!(first.next_cursor, 1);
    assert!(first.has_more);
    // A read must not leave the append cursor pointing into previous history.
    database.delete("orders", "2").unwrap();
    drop(database);
    let reopened = Database::open(&directory.0).unwrap();
    assert!(reopened.get("outbox", "order:1").unwrap().is_some());
    assert!(reopened.get("orders", "2").unwrap().is_none());
    let rest = reopened.changes(first.next_cursor, 100).unwrap();
    assert_eq!(
        rest.transactions
            .iter()
            .map(|transaction| transaction.cursor)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    assert!(matches!(
        rest.transactions[1].operations[0],
        ChangeOperation::Delete { .. }
    ));
    assert_eq!(rest.next_cursor, 3);
    assert!(!rest.has_more);
    let exhausted = reopened.changes(3, 10).unwrap();
    assert!(exhausted.transactions.is_empty());
    assert_eq!(exhausted.next_cursor, 3);
    assert!(reopened.changes(4, 10).is_err());
    assert!(reopened.changes(u64::MAX, 10).is_err());
}

#[test]
fn torn_tail_never_produces_a_changefeed_transaction_or_cursor_gap() {
    let original = TestDirectory::new();
    let database = Database::open(&original.0).unwrap();
    database.put("docs", "committed", json!(1)).unwrap();
    let committed_length = database.stats().unwrap().log_bytes as usize;
    database
        .put("docs", "torn", json!({"padding":"x".repeat(80)}))
        .unwrap();
    drop(database);
    let bytes = fs::read(original.0.join("cedro.log")).unwrap();
    for cut in [
        committed_length + 1,
        committed_length + 11,
        committed_length + 12,
        bytes.len() - 1,
    ] {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("cedro.log"), &bytes[..cut]).unwrap();
        let database = Database::open(&directory.0).unwrap();
        assert_eq!(database.stats().unwrap().latest_cursor, 1);
        assert_eq!(database.changes(0, 100).unwrap().transactions.len(), 1);
        assert!(database.get("docs", "torn").unwrap().is_none());
        database.put("docs", "replacement", json!(2)).unwrap();
        assert_eq!(database.changes(1, 100).unwrap().transactions[0].cursor, 2);
    }
}

#[test]
fn invalid_queries_limits_and_failed_commits_leave_history_unchanged() {
    let directory = TestDirectory::new();
    let database = Database::open_with_options(
        &directory.0,
        Options {
            max_state_bytes: 64,
            ..Options::default()
        },
    )
    .unwrap();
    database.create_index("docs", "value").unwrap();
    database.put("docs", "a", json!({"value":1})).unwrap();
    let stats = database.stats().unwrap();
    assert!(database
        .compare_exchange(
            "docs",
            "a",
            Some(json!({"value":1})),
            Some(json!({"value":"x".repeat(100)}))
        )
        .is_err());
    assert_eq!(database.stats().unwrap(), stats);
    assert_eq!(
        database
            .query(
                "docs",
                Query {
                    filters: vec![filter("value", FilterOp::Eq, json!(1))],
                    ..Query::default()
                }
            )
            .unwrap()
            .records
            .len(),
        1
    );
    for limit in [0, 1001, usize::MAX] {
        assert!(database
            .query(
                "docs",
                Query {
                    limit,
                    ..Query::default()
                }
            )
            .is_err());
        assert!(database.changes(0, limit).is_err());
    }
    assert!(database
        .query(
            "docs",
            serde_json::from_str::<Query>(r#"{"limit":0}"#).unwrap()
        )
        .is_err());
    let defaults: Query = serde_json::from_str("{}").unwrap();
    assert_eq!(defaults.limit, 100);
    assert_eq!(database.stats().unwrap().latest_cursor, 1);
}

#[test]
fn incomplete_garbage_recovery_keeps_the_last_cursor_and_safe_append_position() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database.put("docs", "a", json!(1)).unwrap();
    drop(database);
    // Also cover incomplete garbage recovery for a feed resumed at the last cursor.
    OpenOptions::new()
        .append(true)
        .open(directory.0.join("cedro.log"))
        .unwrap()
        .write_all(&[1, 2, 3])
        .unwrap();
    let reopened = Database::open(&directory.0).unwrap();
    assert_eq!(reopened.stats().unwrap().latest_cursor, 1);
    assert!(reopened.changes(1, 1).unwrap().transactions.is_empty());
    reopened.put("docs", "b", json!(2)).unwrap();
    assert_eq!(reopened.changes(0, 1).unwrap().next_cursor, 1);
    assert_eq!(reopened.changes(1, 1).unwrap().next_cursor, 2);
}

#[test]
fn safe_numeric_equality_supports_nested_python_to_javascript_roundtrips() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    let python: Value =
        serde_json::from_str(r#"{"revision":1.0,"nested":[{"zero":-0.0,"n":2.0}],"flag":true}"#)
            .unwrap();
    let javascript: Value =
        serde_json::from_str(r#"{"revision":1,"nested":[{"zero":0,"n":2}],"flag":true}"#).unwrap();
    database
        .put("interop", "document", json!({"payload":python.clone()}))
        .unwrap();
    let equal_query = Query {
        filters: vec![filter("payload", FilterOp::Eq, javascript.clone())],
        ..Query::default()
    };
    let baseline = database
        .query("interop", equal_query.clone())
        .unwrap()
        .records;
    assert_eq!(baseline.len(), 1);
    database.create_index("interop", "payload").unwrap();
    assert_eq!(
        database.query("interop", equal_query).unwrap().records,
        baseline
    );
    let not_equal = Query {
        filters: vec![filter("payload", FilterOp::Ne, javascript.clone())],
        ..Query::default()
    };
    assert!(database
        .query("interop", not_equal)
        .unwrap()
        .records
        .is_empty());
    let boolean_as_integer = json!({"payload":{"revision":1,"nested":[{"zero":0,"n":2}],"flag":1}});
    assert!(!database
        .compare_exchange("interop", "document", Some(boolean_as_integer), None)
        .unwrap());
    let integer_document = json!({"payload":javascript});
    assert!(database
        .compare_exchange(
            "interop",
            "document",
            Some(integer_document.clone()),
            Some(integer_document.clone())
        )
        .unwrap());
    assert_eq!(
        database
            .query(
                "interop",
                Query {
                    filters: vec![filter("payload", FilterOp::Eq, python)],
                    ..Query::default()
                }
            )
            .unwrap()
            .records
            .len(),
        1
    );
    drop(database);
    let reopened = Database::open(&directory.0).unwrap();
    assert!(reopened
        .compare_exchange("interop", "document", Some(integer_document), None)
        .unwrap());
}

#[test]
fn mixed_numeric_equality_has_exact_boundaries_and_never_rounds_large_integers() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    let safe_bound = 9_007_199_254_740_992i64;
    let values = [
        ("negative-safe", json!(-safe_bound)),
        ("positive-safe", json!(safe_bound)),
        ("positive-unsafe", json!(safe_bound as u64 + 1)),
        ("positive-unsafe-even", json!(safe_bound as u64 + 2)),
        ("negative-unsafe", json!(-safe_bound - 1)),
        ("maximum", json!(u64::MAX)),
        ("minimum", json!(i64::MIN)),
        ("bool", json!(true)),
    ];
    for (key, value) in &values {
        database.put("numbers", key, json!({"n":value})).unwrap();
    }
    let cases = [
        (json!(safe_bound as f64), vec!["positive-safe"]),
        (json!(-safe_bound as f64), vec!["negative-safe"]),
        (json!((safe_bound + 2) as f64), vec![]),
        (json!(u64::MAX as f64), vec![]),
        (json!(i64::MIN as f64), vec![]),
        (json!(true), vec!["bool"]),
        (json!(safe_bound as u64 + 1), vec!["positive-unsafe"]),
    ];
    for indexed in [false, true] {
        if indexed {
            database.create_index("numbers", "n").unwrap();
        }
        for (value, expected) in &cases {
            let page = database
                .query(
                    "numbers",
                    Query {
                        filters: vec![filter("n", FilterOp::Eq, value.clone())],
                        ..Query::default()
                    },
                )
                .unwrap();
            assert_eq!(
                page.records
                    .iter()
                    .map(|record| record.key.as_str())
                    .collect::<Vec<_>>(),
                *expected
            );
        }
    }
    let stats = database.stats().unwrap();
    assert!(!database
        .compare_exchange(
            "numbers",
            "positive-unsafe-even",
            Some(json!({"n":(safe_bound + 2) as f64})),
            None
        )
        .unwrap());
    assert!(!database
        .compare_exchange(
            "numbers",
            "maximum",
            Some(json!({"n":u64::MAX as f64})),
            None
        )
        .unwrap());
    assert_eq!(database.stats().unwrap(), stats);
}

#[test]
fn changefeed_pages_are_bounded_by_encoded_frame_bytes_and_resume_without_gaps() {
    let directory = TestDirectory::new();
    let options = Options {
        max_frame_bytes: 256,
        ..Options::default()
    };
    let database = Database::open_with_options(&directory.0, options).unwrap();
    let mut frame_bytes = Vec::new();
    let mut previous_bytes = database.stats().unwrap().log_bytes;
    for (index, padding) in [0, 0, 120, 0, 0, 120, 0].into_iter().enumerate() {
        database
            .put(
                "events",
                &index.to_string(),
                json!({"text":"x".repeat(padding)}),
            )
            .unwrap();
        let bytes = database.stats().unwrap().log_bytes;
        frame_bytes.push(bytes - previous_bytes);
        previous_bytes = bytes;
    }
    let budget = options.max_frame_bytes as u64 + 12;
    assert!(frame_bytes.iter().sum::<u64>() > budget * 2);
    let read_pages = |database: &Database| {
        let mut cursor = 0u64;
        let mut pages = Vec::new();
        while cursor < frame_bytes.len() as u64 {
            let page = database.changes(cursor, 1000).unwrap();
            assert!(!page.transactions.is_empty());
            let start = cursor as usize;
            let end = page.next_cursor as usize;
            let used_bytes: u64 = frame_bytes[start..end].iter().sum();
            assert!(used_bytes <= budget);
            if end < frame_bytes.len() {
                assert!(used_bytes + frame_bytes[end] > budget);
            }
            assert_eq!(
                page.transactions
                    .iter()
                    .map(|transaction| transaction.cursor)
                    .collect::<Vec<_>>(),
                (cursor + 1..=page.next_cursor).collect::<Vec<_>>()
            );
            assert_eq!(page.has_more, end < frame_bytes.len());
            cursor = page.next_cursor;
            pages.push(page);
        }
        pages
    };
    let before = read_pages(&database);
    assert!(before.len() > 2);
    assert_eq!(database.changes(0, 1).unwrap().transactions.len(), 1);
    drop(database);
    let reopened = Database::open_with_options(&directory.0, options).unwrap();
    assert_eq!(read_pages(&reopened), before);
    assert!(reopened
        .changes(frame_bytes.len() as u64, 1000)
        .unwrap()
        .transactions
        .is_empty());
}
