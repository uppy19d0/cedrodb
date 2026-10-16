use cedrodb::{
    AgentStatus, ChangeOperation, CheckpointOptions, Condition, Database, EnqueueOptions, Error,
    Options,
};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
const MAX_SAFE: u64 = 9_007_199_254_740_991;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cedrodb-agent-{}-{}",
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

fn put(collection: &str, key: &str, value: Value) -> ChangeOperation {
    ChangeOperation::Put {
        collection: collection.into(),
        key: key.into(),
        value,
    }
}

#[test]
fn checkpoint_and_business_outbox_writes_share_one_durable_revision() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database.put("other", "before", json!(true)).unwrap();
    let result = database
        .agent_checkpoint(
            "tenant-a",
            "conversation-1",
            json!({"step": 1}),
            CheckpointOptions {
                status: AgentStatus::Waiting,
                now_ms: 123,
                metadata: json!({"tool_call_id":"call-1"}),
                operations: vec![
                    put("business", "result", json!({"ok":true})),
                    put("outbox", "event-1", json!({"type":"waiting"})),
                ],
                ..CheckpointOptions::default()
            },
        )
        .unwrap();
    assert!(result.applied);
    assert_eq!(result.cursor, 2);
    let checkpoint = result.checkpoint.unwrap();
    assert_eq!(checkpoint.schema_version, 1);
    assert_eq!(checkpoint.thread_id, "conversation-1");
    assert_eq!(checkpoint.revision, result.cursor);
    assert_eq!(checkpoint.status, AgentStatus::Waiting);
    assert_eq!(checkpoint.updated_at, 123);
    assert_eq!(
        database.changes(1, 1).unwrap().transactions[0]
            .operations
            .len(),
        3
    );
    drop(database);
    let reopened = Database::open(&directory.0).unwrap();
    assert_eq!(
        reopened.agent_get("tenant-a", "conversation-1").unwrap(),
        Some(checkpoint)
    );
    assert_eq!(
        reopened.get("business", "result").unwrap(),
        Some(json!({"ok":true}))
    );
    assert!(reopened.get("outbox", "event-1").unwrap().is_some());
}

#[test]
fn conflicts_return_committed_state_without_any_writes() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    let initial = database
        .agent_checkpoint(
            "a",
            "thread",
            json!({"step":1}),
            CheckpointOptions::default(),
        )
        .unwrap();
    let before = database.stats().unwrap();
    for expected_revision in [None, Some(2), Some(MAX_SAFE)] {
        let result = database
            .agent_checkpoint(
                "a",
                "thread",
                json!({"step":2}),
                CheckpointOptions {
                    expected_revision,
                    operations: vec![put("business", "must-not-exist", json!(true))],
                    ..CheckpointOptions::default()
                },
            )
            .unwrap();
        assert!(!result.applied);
        assert_eq!(result.cursor, initial.cursor);
        assert_eq!(result.checkpoint, initial.checkpoint);
        assert_eq!(database.stats().unwrap(), before);
        assert_eq!(database.get("business", "must-not-exist").unwrap(), None);
    }
    let absent = database
        .agent_checkpoint(
            "a",
            "missing",
            Value::Null,
            CheckpointOptions {
                expected_revision: Some(1),
                ..CheckpointOptions::default()
            },
        )
        .unwrap();
    assert!(!absent.applied);
    assert_eq!(absent.checkpoint, None);
    assert_eq!(database.stats().unwrap(), before);
}

#[test]
fn concurrent_checkpoint_writers_have_one_winner_and_one_business_result() {
    let directory = TestDirectory::new();
    let database = Arc::new(Database::open(&directory.0).unwrap());
    let revision = database
        .agent_checkpoint("a", "thread", json!(0), CheckpointOptions::default())
        .unwrap()
        .cursor;
    let barrier = Arc::new(Barrier::new(12));
    let writers: Vec<_> = (0..12)
        .map(|writer| {
            let database = Arc::clone(&database);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                database
                    .agent_checkpoint(
                        "a",
                        "thread",
                        json!({"winner":writer}),
                        CheckpointOptions {
                            expected_revision: Some(revision),
                            operations: vec![put("winners", &writer.to_string(), json!(writer))],
                            ..CheckpointOptions::default()
                        },
                    )
                    .unwrap()
                    .applied
            })
        })
        .collect();
    let winners = writers
        .into_iter()
        .map(|writer| writer.join().unwrap())
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1);
    let records = database.scan("winners").unwrap();
    assert_eq!(records.len(), 1);
    let checkpoint = database.agent_get("a", "thread").unwrap().unwrap();
    assert_eq!(checkpoint.state["winner"], records[0].1);
    assert_eq!(database.stats().unwrap().latest_cursor, 2);
}

#[test]
fn revision_is_the_global_durable_cursor_and_staging_reads_stay_committed() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    let initial = database
        .agent_checkpoint("a", "thread", json!(1), CheckpointOptions::default())
        .unwrap();
    let writer = database.begin_write().unwrap();
    assert_eq!(
        database.agent_get("a", "thread").unwrap(),
        initial.checkpoint
    );
    assert!(matches!(
        database.agent_checkpoint("a", "thread", json!(2), CheckpointOptions::default()),
        Err(Error::WriterBusy)
    ));
    drop(writer);
    database.put("other", "interleaved", json!(true)).unwrap();
    let updated = database
        .agent_checkpoint(
            "a",
            "thread",
            json!(2),
            CheckpointOptions {
                expected_revision: Some(initial.cursor),
                status: AgentStatus::Completed,
                ..CheckpointOptions::default()
            },
        )
        .unwrap();
    assert!(updated.applied);
    assert_eq!(updated.cursor, 3);
    assert_eq!(updated.checkpoint.unwrap().revision, 3);
}

#[test]
fn namespace_and_thread_components_do_not_alias() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    for (namespace, thread, value) in [
        ("a.b", "c", 1),
        ("a", "b.c", 2),
        ("a", "c", 3),
        ("a-b_9", "á/🌲", 4),
    ] {
        database
            .agent_checkpoint(
                namespace,
                thread,
                json!(value),
                CheckpointOptions::default(),
            )
            .unwrap();
        assert_eq!(
            database
                .agent_get(namespace, thread)
                .unwrap()
                .unwrap()
                .state,
            json!(value)
        );
    }
    assert_eq!(
        database.agent_get("a.b", "c").unwrap().unwrap().state,
        json!(1)
    );
    assert_eq!(
        database.agent_get("a", "b.c").unwrap().unwrap().state,
        json!(2)
    );
    assert_eq!(database.agent_get("a.b.c", "missing").unwrap(), None);
}

#[test]
fn all_invalid_input_is_rejected_before_a_conflict_or_business_write() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .agent_checkpoint("a", "thread", Value::Null, CheckpointOptions::default())
        .unwrap();
    let before = database.stats().unwrap();
    let invalid_options = [
        CheckpointOptions {
            expected_revision: Some(0),
            ..CheckpointOptions::default()
        },
        CheckpointOptions {
            expected_revision: Some(MAX_SAFE + 1),
            ..CheckpointOptions::default()
        },
        CheckpointOptions {
            now_ms: MAX_SAFE + 1,
            ..CheckpointOptions::default()
        },
        CheckpointOptions {
            metadata: json!([]),
            ..CheckpointOptions::default()
        },
        CheckpointOptions {
            metadata: json!({"n":u64::MAX}),
            ..CheckpointOptions::default()
        },
        CheckpointOptions {
            operations: vec![put("business", "", json!(true))],
            ..CheckpointOptions::default()
        },
        CheckpointOptions {
            operations: vec![put("_cedro.agents.a", "thread", json!(true))],
            ..CheckpointOptions::default()
        },
        CheckpointOptions {
            operations: vec![put("_cedro.jobs.a", "job", json!(true))],
            ..CheckpointOptions::default()
        },
    ];
    for options in invalid_options {
        assert!(database
            .agent_checkpoint("a", "thread", json!({"step":2}), options)
            .is_err());
        assert_eq!(database.stats().unwrap(), before);
    }
    for value in [json!(u64::MAX), json!(i64::MIN), json!([{"n":1.0e100}])] {
        assert!(database
            .agent_checkpoint("a", "thread", value, CheckpointOptions::default())
            .is_err());
    }
    for namespace in ["", "a/b", "ñ", "a\0b", &"a".repeat(129)] {
        assert!(database.agent_get(namespace, "thread").is_err());
        assert!(database
            .agent_checkpoint(
                namespace,
                "thread",
                Value::Null,
                CheckpointOptions::default()
            )
            .is_err());
    }
    for thread in ["", "a\0b", &"a".repeat(4097)] {
        assert!(database.agent_get("a", thread).is_err());
        assert!(database
            .agent_checkpoint("a", thread, Value::Null, CheckpointOptions::default())
            .is_err());
    }
    assert_eq!(database.stats().unwrap(), before);
}

#[test]
fn portable_boundaries_round_trip_and_metadata_must_remain_an_object() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    let state =
        json!({"max":MAX_SAFE,"min":-(MAX_SAFE as i64),"float":1.5,"null":null,"unicode":"🌲"});
    let result = database
        .agent_checkpoint(
            "a",
            "thread",
            state.clone(),
            CheckpointOptions {
                now_ms: MAX_SAFE,
                metadata: json!({"nested":[MAX_SAFE, -(MAX_SAFE as i64), true]}),
                operations: vec![put("business", "large-integer", json!(u64::MAX))],
                ..CheckpointOptions::default()
            },
        )
        .unwrap();
    drop(database);
    let database = Database::open(&directory.0).unwrap();
    assert_eq!(
        database.agent_get("a", "thread").unwrap(),
        result.checkpoint
    );
    assert_eq!(
        database.agent_get("a", "thread").unwrap().unwrap().state,
        state
    );
    assert_eq!(
        database.get("business", "large-integer").unwrap(),
        Some(json!(u64::MAX))
    );
    let defaults: CheckpointOptions = serde_json::from_value(json!({})).unwrap();
    assert_eq!(defaults.status, AgentStatus::Running);
    assert_eq!(defaults.metadata, json!({}));
    assert_eq!(defaults.expected_revision, None);
    assert!(serde_json::from_value::<CheckpointOptions>(json!({"unknown":true})).is_err());
    assert!(serde_json::from_value::<CheckpointOptions>(json!({"status":"approved"})).is_err());
}

#[test]
fn generic_mutations_and_ack_cannot_bypass_checkpoint_revision_fencing() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .agent_checkpoint("a", "thread", json!(1), CheckpointOptions::default())
        .unwrap();
    database
        .queue_enqueue("q", "job", Value::Null, EnqueueOptions::default())
        .unwrap();
    let job = database
        .queue_claim("q", "worker", 0, 100)
        .unwrap()
        .unwrap();
    let before = database.stats().unwrap();
    let reserved = "_cedro.agents.a";
    assert!(database.put(reserved, "thread", Value::Null).is_err());
    assert!(database.delete(reserved, "thread").is_err());
    assert!(database
        .compare_exchange(reserved, "thread", None, Some(Value::Null))
        .is_err());
    assert!(database
        .conditional_batch(&[], &[put(reserved, "thread", Value::Null)])
        .is_err());
    let mut transaction = database.begin_write().unwrap();
    assert!(transaction.put(reserved, "thread", Value::Null).is_err());
    assert!(transaction.delete(reserved, "thread").is_err());
    drop(transaction);
    assert!(database
        .queue_ack(
            "q",
            "job",
            job.lease_token.as_deref().unwrap(),
            1,
            &[put(reserved, "thread", Value::Null)]
        )
        .is_err());
    assert_eq!(database.stats().unwrap(), before);
    assert_eq!(
        database.agent_get("a", "thread").unwrap().unwrap().state,
        json!(1)
    );
}

#[test]
fn document_frame_and_state_limits_preserve_the_entire_previous_checkpoint() {
    for options in [
        Options {
            max_document_bytes: 180,
            ..Options::default()
        },
        Options {
            max_frame_bytes: 400,
            ..Options::default()
        },
        Options {
            max_state_bytes: 400,
            ..Options::default()
        },
    ] {
        let directory = TestDirectory::new();
        let database = Database::open_with_options(&directory.0, options).unwrap();
        let initial = database
            .agent_checkpoint("a", "thread", json!(1), CheckpointOptions::default())
            .unwrap();
        let before = database.stats().unwrap();
        let update = CheckpointOptions {
            expected_revision: Some(initial.cursor),
            operations: vec![put("business", "huge", json!("x".repeat(800)))],
            ..CheckpointOptions::default()
        };
        assert!(database
            .agent_checkpoint("a", "thread", json!(2), update)
            .is_err());
        assert_eq!(
            database.agent_get("a", "thread").unwrap(),
            initial.checkpoint
        );
        assert_eq!(database.get("business", "huge").unwrap(), None);
        assert_eq!(database.stats().unwrap(), before);
    }
    let directory = TestDirectory::new();
    let database = Database::open_with_options(
        &directory.0,
        Options {
            max_document_bytes: 64,
            ..Options::default()
        },
    )
    .unwrap();
    assert!(database
        .agent_checkpoint("a", "thread", Value::Null, CheckpointOptions::default())
        .is_err());
    assert_eq!(database.stats().unwrap().latest_cursor, 0);
}

#[test]
fn the_checkpoint_envelope_cannot_make_a_committed_document_unreadable() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    let mut state = Value::Null;
    for _ in 0..64 {
        state = Value::Array(vec![state]);
    }
    assert!(database
        .agent_checkpoint("a", "deep", state, CheckpointOptions::default())
        .is_err());
    assert_eq!(database.stats().unwrap().latest_cursor, 0);
    drop(database);
    let database = Database::open(&directory.0).unwrap();
    assert_eq!(database.agent_get("a", "deep").unwrap(), None);
}

#[test]
fn individually_valid_writes_cannot_overflow_the_combined_checkpoint_frame() {
    let directory = TestDirectory::new();
    let database = Database::open_with_options(
        &directory.0,
        Options {
            max_frame_bytes: 400,
            ..Options::default()
        },
    )
    .unwrap();
    let business_value = json!("x".repeat(180));
    database
        .put("business", "fits-alone", business_value.clone())
        .unwrap();
    let original = database
        .agent_checkpoint("a", "thread", json!(1), CheckpointOptions::default())
        .unwrap();
    let before = database.stats().unwrap();
    assert!(database
        .agent_checkpoint(
            "a",
            "thread",
            json!(2),
            CheckpointOptions {
                expected_revision: Some(original.cursor),
                operations: vec![put("business", "combined", business_value)],
                ..CheckpointOptions::default()
            },
        )
        .is_err());
    assert_eq!(
        database.agent_get("a", "thread").unwrap(),
        original.checkpoint
    );
    assert_eq!(database.get("business", "combined").unwrap(), None);
    assert_eq!(database.stats().unwrap(), before);
}

#[test]
fn backup_and_restore_preserve_checkpoint_revisions_and_atomic_business_state() {
    let directory = TestDirectory::new();
    let database = Database::open(directory.0.join("source")).unwrap();
    let original = database
        .agent_checkpoint(
            "a",
            "thread",
            json!({"step":1}),
            CheckpointOptions::default(),
        )
        .unwrap();
    let backup = directory.0.join("backup.cedrobak");
    database.backup(&backup).unwrap();
    database
        .agent_checkpoint(
            "a",
            "thread",
            json!({"step":2}),
            CheckpointOptions {
                expected_revision: Some(original.cursor),
                ..CheckpointOptions::default()
            },
        )
        .unwrap();
    let restored_path = directory.0.join("restored");
    Database::restore_backup(&backup, &restored_path).unwrap();
    let restored = Database::open(restored_path).unwrap();
    assert_eq!(
        restored.agent_get("a", "thread").unwrap(),
        original.checkpoint
    );
    let update = restored
        .agent_checkpoint(
            "a",
            "thread",
            json!({"step":3}),
            CheckpointOptions {
                expected_revision: Some(original.cursor),
                operations: vec![put("business", "restored", json!(true))],
                ..CheckpointOptions::default()
            },
        )
        .unwrap();
    assert!(update.applied);
    assert_eq!(update.cursor, 2);
    assert_eq!(
        restored.get("business", "restored").unwrap(),
        Some(json!(true))
    );
}

#[test]
fn torn_checkpoint_transactions_never_publish_only_the_business_result() {
    let source = TestDirectory::new();
    let database = Database::open(&source.0).unwrap();
    let original = database
        .agent_checkpoint("a", "thread", json!(1), CheckpointOptions::default())
        .unwrap();
    let boundary = database.stats().unwrap().log_bytes as usize;
    database
        .agent_checkpoint(
            "a",
            "thread",
            json!(2),
            CheckpointOptions {
                expected_revision: Some(original.cursor),
                operations: vec![put("business", "result", json!(true))],
                ..CheckpointOptions::default()
            },
        )
        .unwrap();
    drop(database);
    let complete = fs::read(source.0.join("cedro.log")).unwrap();
    for cut in [
        boundary + 1,
        boundary + 11,
        boundary + 12,
        complete.len() - 1,
        complete.len(),
    ] {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("cedro.log"), &complete[..cut]).unwrap();
        let recovered = Database::open(&directory.0).unwrap();
        let published = cut == complete.len();
        assert_eq!(
            recovered.agent_get("a", "thread").unwrap().unwrap().state,
            json!(if published { 2 } else { 1 })
        );
        assert_eq!(
            recovered.get("business", "result").unwrap(),
            published.then_some(json!(true))
        );
        assert_eq!(
            recovered.stats().unwrap().latest_cursor,
            if published { 2 } else { 1 }
        );
    }
}

#[test]
fn malformed_legacy_checkpoint_records_fail_without_being_overwritten() {
    let source = TestDirectory::new();
    let database = Database::open(&source.0).unwrap();
    database
        .agent_checkpoint("a", "thread", Value::Null, CheckpointOptions::default())
        .unwrap();
    drop(database);
    let original = fs::read(source.0.join("cedro.log")).unwrap();
    let batch: Value = serde_json::from_slice(&original[28..]).unwrap();
    for (field, value) in [
        ("schema_version", json!(2)),
        ("thread_id", json!("another-thread")),
        ("revision", json!(0)),
        ("revision", json!(2)),
        ("metadata", json!([])),
        ("state", json!({"n":u64::MAX})),
        ("updated_at", json!(MAX_SAFE + 1)),
        ("status", json!("approved")),
    ] {
        let directory = TestDirectory::new();
        let mut modified = batch.clone();
        modified["ops"][0]["value"][field] = value;
        let payload = serde_json::to_vec(&modified).unwrap();
        let length = payload.len() as u32;
        let mut bytes = original[..16].to_vec();
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&(!length).to_le_bytes());
        bytes.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        bytes.extend_from_slice(&payload);
        let log = directory.0.join("cedro.log");
        fs::write(&log, &bytes).unwrap();
        let recovered = Database::open(&directory.0).unwrap();
        assert!(matches!(
            recovered.agent_get("a", "thread"),
            Err(Error::Corruption(_))
        ));
        assert!(matches!(
            recovered.agent_checkpoint(
                "a",
                "thread",
                json!("overwrite"),
                CheckpointOptions::default()
            ),
            Err(Error::Corruption(_))
        ));
        assert_eq!(fs::read(log).unwrap(), bytes);
    }
}

#[test]
fn checkpoint_conditions_can_observe_state_without_mutating_its_reserved_collection() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .agent_checkpoint("a", "thread", json!(1), CheckpointOptions::default())
        .unwrap();
    let stored = database.get("_cedro.agents.a", "thread").unwrap().unwrap();
    assert!(
        database
            .conditional_batch(
                &[Condition::Equals {
                    collection: "_cedro.agents.a".into(),
                    key: "thread".into(),
                    value: stored,
                }],
                &[put("business", "observed", json!(true))]
            )
            .unwrap()
            .applied
    );
}
