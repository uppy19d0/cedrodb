use cedrodb::{
    ChangeOperation, CheckpointOptions, Condition, Database, EnqueueOptions, Filter, FilterOp,
    Options, Query, SearchQuery, SessionMetrics,
};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cedrodb-performance-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn eq(field: &str, value: Value) -> Filter {
    Filter {
        field: field.into(),
        op: FilterOp::Eq,
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
fn populate(database: &Database) {
    let mut transaction = database.begin_write().unwrap();
    for index in 0..20 {
        transaction.put("memory", &format!("{index:02}"), json!({"group":if index%2==0 {"even"}else{"odd"},"n":index,"text":"local memory","embedding":[1,0]})).unwrap();
    }
    transaction.commit().unwrap();
}

#[test]
fn diagnostics_report_encoded_counts_limits_indexes_and_no_document_values() {
    let directory = Directory::new();
    let options = Options {
        max_document_bytes: 500,
        ..Default::default()
    };
    let database = Database::open_with_options(&directory.0, options).unwrap();
    database
        .put("z", "key", json!({"private":"DO-NOT-EXPOSE"}))
        .unwrap();
    database.put("a", "key", json!([1, 2, 3])).unwrap();
    database.create_index("z", "private").unwrap();
    database.create_index("empty", "field").unwrap();
    let before = database.verify().unwrap();
    let report = database.diagnostics().unwrap();
    assert_eq!(report.stats, database.stats().unwrap());
    assert_eq!(report.limits, options);
    assert!(!report.poisoned);
    assert!(!report.writer_active);
    assert_eq!(
        report
            .collections
            .iter()
            .map(|entry| entry.collection.as_str())
            .collect::<Vec<_>>(),
        ["a", "empty", "z"]
    );
    assert_eq!(
        report
            .collections
            .iter()
            .map(|entry| entry.records)
            .sum::<u64>(),
        report.stats.records
    );
    assert_eq!(
        report
            .collections
            .iter()
            .map(|entry| entry.content_bytes)
            .sum::<u64>(),
        report.stats.content_bytes
    );
    assert_eq!(report.collections[1].records, 0);
    assert_eq!(report.collections[1].index_fields, ["field"]);
    assert_eq!(report.collections[2].index_fields, ["private"]);
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("DO-NOT-EXPOSE"));
    assert_eq!(report.session.commit.successes, 2);
    let writer = database.begin_write().unwrap();
    assert!(database.diagnostics().unwrap().writer_active);
    drop(writer);
    assert_eq!(database.verify().unwrap(), before);
}

#[test]
fn profiling_executes_once_and_returns_the_actual_page_without_writing() {
    let directory = Directory::new();
    let database = Database::open(&directory.0).unwrap();
    populate(&database);
    let before = database.verify().unwrap();
    let query = Query {
        filters: vec![eq("group", json!("even"))],
        limit: 3,
        after: None,
    };
    let normal = database.query("memory", query.clone()).unwrap();
    let profiled = database.profile_query("memory", query).unwrap();
    assert_eq!(profiled.page, normal);
    assert_eq!(profiled.plan.collection_records, 20);
    assert_eq!(profiled.plan.candidate_records, 20);
    assert_eq!(profiled.plan.scanned, profiled.page.scanned);
    assert_eq!(profiled.plan.returned, 3);
    assert_eq!(profiled.plan.used_index, None);
    let search = SearchQuery {
        text: Some("memory".into()),
        vector: Some(vec![1.0, 0.0]),
        filters: vec![eq("group", json!("even"))],
        limit: 3,
        ..Default::default()
    };
    let normal = database.search("memory", search.clone()).unwrap();
    let profiled = database.profile_search("memory", search).unwrap();
    assert_eq!(profiled.page, normal);
    assert_eq!(profiled.plan.scanned, 20);
    assert_eq!(profiled.plan.returned, 3);
    assert_eq!(profiled.plan.candidate_records, 20);
    let counters = database.diagnostics().unwrap().session;
    assert_eq!(counters.query.attempts, 2);
    assert_eq!(counters.query.successes, 2);
    assert_eq!(counters.search.attempts, 2);
    assert_eq!(counters.search.successes, 2);
    assert_eq!(counters.commit.successes, 1);
    assert_eq!(database.verify().unwrap(), before);
}

#[test]
fn shared_plans_count_candidates_after_cursor_and_only_select_equality_indexes() {
    let directory = Directory::new();
    let database = Database::open(&directory.0).unwrap();
    populate(&database);
    let query = Query {
        filters: vec![eq("group", json!("even"))],
        limit: 2,
        after: Some("09".into()),
    };
    let plain = database.profile_query("memory", query.clone()).unwrap();
    assert_eq!(plain.plan.collection_records, 20);
    assert_eq!(plain.plan.candidate_records, 10);
    database.create_index("memory", "group").unwrap();
    let indexed = database.profile_query("memory", query).unwrap();
    assert_eq!(indexed.plan.candidate_records, 5);
    assert_eq!(indexed.plan.used_index.as_deref(), Some("group"));
    assert_eq!(plain.page.records, indexed.page.records);
    assert_eq!(plain.page.next_after, indexed.page.next_after);
    assert!(indexed.plan.scanned < plain.plan.scanned);
    assert_eq!(indexed.plan.scanned, 3); // two results plus one lookahead.
    let search = database
        .profile_search(
            "memory",
            SearchQuery {
                text: Some("memory".into()),
                filters: vec![eq("group", json!("even"))],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(search.plan.candidate_records, 10);
    assert_eq!(search.plan.scanned, 10);
    assert_eq!(search.plan.used_index.as_deref(), Some("group"));
    let ordered = Filter {
        field: "group".into(),
        op: FilterOp::Gte,
        value: json!("even"),
    };
    let query = database
        .profile_query(
            "memory",
            Query {
                filters: vec![ordered.clone()],
                ..Default::default()
            },
        )
        .unwrap();
    let search = database
        .profile_search(
            "memory",
            SearchQuery {
                text: Some("memory".into()),
                filters: vec![ordered],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(query.plan.used_index, None);
    assert_eq!(search.plan.used_index, None);
    assert_eq!(query.plan.candidate_records, 20);
    assert_eq!(search.plan.candidate_records, 20);
    let absent = database.profile_query("missing", Query::default()).unwrap();
    assert_eq!(absent.plan.collection_records, 0);
    assert_eq!(absent.plan.candidate_records, 0);
}

#[test]
fn read_counters_include_validation_and_budget_errors_but_no_hidden_retries() {
    let directory = Directory::new();
    let database = Database::open(&directory.0).unwrap();
    populate(&database);
    assert!(database.query("", Query::default()).is_err());
    assert!(database
        .profile_query(
            "memory",
            Query {
                limit: 0,
                ..Default::default()
            }
        )
        .is_err());
    database.query("missing", Query::default()).unwrap();
    assert!(database.search("memory", SearchQuery::default()).is_err());
    assert!(database
        .profile_search(
            "memory",
            SearchQuery {
                text: Some("memory".into()),
                max_scanned: 1,
                ..Default::default()
            }
        )
        .is_err());
    database
        .profile_search(
            "missing",
            SearchQuery {
                text: Some("memory".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let counters = database.diagnostics().unwrap().session;
    assert_eq!(
        (
            counters.query.attempts,
            counters.query.successes,
            counters.query.failures
        ),
        (3, 1, 2)
    );
    assert_eq!(
        (
            counters.search.attempts,
            counters.search.successes,
            counters.search.failures
        ),
        (3, 1, 2)
    );
    assert_eq!(counters.commit.successes, 1);
    assert!(counters.query.max_elapsed_micros <= counters.query.total_elapsed_micros);
    assert!(counters.search.max_elapsed_micros <= counters.search.total_elapsed_micros);
}

#[test]
fn commit_counters_cover_all_shared_durable_paths_and_exclude_noops_and_early_errors() {
    let directory = Directory::new();
    let database = Database::open(&directory.0).unwrap();
    database.put("records", "a", json!(1)).unwrap();
    assert!(database
        .compare_exchange("records", "a", Some(json!(1)), Some(json!(2)))
        .unwrap());
    database
        .conditional_batch(&[], &[put("records", "b", json!(3))])
        .unwrap();
    database
        .agent_checkpoint("agent", "thread", Value::Null, CheckpointOptions::default())
        .unwrap();
    database
        .queue_enqueue("q", "job", Value::Null, EnqueueOptions::default())
        .unwrap();
    let job = database
        .queue_claim("q", "worker", 0, 100)
        .unwrap()
        .unwrap();
    assert!(database
        .queue_ack("q", "job", job.lease_token.as_deref().unwrap(), 1, &[])
        .unwrap());
    let before = database.diagnostics().unwrap().session.commit;
    assert_eq!(
        (before.attempts, before.successes, before.failures),
        (7, 7, 0)
    );
    database.begin_write().unwrap().commit().unwrap();
    assert!(!database
        .compare_exchange("records", "a", Some(json!(0)), None)
        .unwrap());
    assert!(
        !database
            .conditional_batch(
                &[Condition::Absent {
                    collection: "records".into(),
                    key: "a".into()
                }],
                &[put("records", "c", json!(4))]
            )
            .unwrap()
            .applied
    );
    assert!(
        !database
            .queue_enqueue("q", "job", Value::Null, EnqueueOptions::default())
            .unwrap()
            .inserted
    );
    assert!(!database.queue_ack("q", "job", "stale", 1, &[]).unwrap());
    assert!(database.put("records", "", Value::Null).is_err());
    assert!(database
        .agent_checkpoint(
            "agent",
            "thread",
            json!(u64::MAX),
            CheckpointOptions::default()
        )
        .is_err());
    let after = database.diagnostics().unwrap().session.commit;
    assert_eq!(after, before);
    assert_eq!(database.stats().unwrap().latest_cursor, 7);
}

#[test]
fn failed_shared_commit_is_recorded_without_counting_staging_validation_as_a_commit() {
    let directory = Directory::new();
    let database = Database::open_with_options(
        &directory.0,
        Options {
            max_state_bytes: 100,
            ..Default::default()
        },
    )
    .unwrap();
    database.put("records", "a", json!(1)).unwrap();
    assert!(database.put("records", "", Value::Null).is_err());
    assert!(database
        .put("records", "huge", json!("x".repeat(200)))
        .is_err());
    let report = database.diagnostics().unwrap();
    assert_eq!(
        (
            report.session.commit.attempts,
            report.session.commit.successes,
            report.session.commit.failures
        ),
        (2, 1, 1)
    );
    assert_eq!(report.stats.latest_cursor, 1);
    assert!(!report.poisoned);
    assert_eq!(database.get("records", "huge").unwrap(), None);
}

#[test]
fn reopening_and_restoring_reset_metrics_while_durable_counts_remain() {
    let directory = Directory::new();
    let source = directory.0.join("source");
    let database = Database::open(&source).unwrap();
    populate(&database);
    database.query("memory", Query::default()).unwrap();
    let backup = directory.0.join("backup.cedrobak");
    database.backup(&backup).unwrap();
    let stats = database.stats().unwrap();
    drop(database);
    let reopened = Database::open(source).unwrap();
    let report = reopened.diagnostics().unwrap();
    assert_eq!(report.stats, stats);
    assert_eq!(report.session, SessionMetrics::default());
    let target = directory.0.join("restored");
    Database::restore_backup(backup, &target).unwrap();
    let restored = Database::open(target).unwrap();
    assert_eq!(
        restored.diagnostics().unwrap().session,
        SessionMetrics::default()
    );
    assert_eq!(restored.stats().unwrap(), stats);
}

#[test]
fn concurrent_profiles_and_diagnostics_finish_without_recursive_locks_or_lost_counts() {
    let directory = Directory::new();
    let database = Arc::new(Database::open(&directory.0).unwrap());
    populate(&database);
    let readers: Vec<_> = (0..8)
        .map(|_| {
            let database = Arc::clone(&database);
            std::thread::spawn(move || {
                for _ in 0..10 {
                    assert_eq!(
                        database
                            .profile_query("memory", Query::default())
                            .unwrap()
                            .page
                            .records
                            .len(),
                        20
                    );
                    assert_eq!(
                        database
                            .profile_search(
                                "memory",
                                SearchQuery {
                                    text: Some("memory".into()),
                                    ..Default::default()
                                }
                            )
                            .unwrap()
                            .page
                            .matched,
                        20
                    );
                    assert_eq!(database.diagnostics().unwrap().stats.records, 20);
                }
            })
        })
        .collect();
    for reader in readers {
        reader.join().unwrap();
    }
    let counters = database.diagnostics().unwrap().session;
    assert_eq!(
        (
            counters.query.attempts,
            counters.query.successes,
            counters.query.failures
        ),
        (80, 80, 0)
    );
    assert_eq!(
        (
            counters.search.attempts,
            counters.search.successes,
            counters.search.failures
        ),
        (80, 80, 0)
    );
    assert_eq!(counters.commit.successes, 1);
}
