use cedrodb::{
    ChangeOperation, Condition, Database, EnqueueOptions, Error, Job, JobStatus, Options, Query,
    QueueStats,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

const MAX_SAFE: u64 = 9_007_199_254_740_991;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cedrodb-queue-{}-{}",
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

fn options(now_ms: u64) -> EnqueueOptions {
    EnqueueOptions {
        now_ms,
        ..EnqueueOptions::default()
    }
}
fn token(job: &Job) -> &str {
    job.lease_token.as_deref().unwrap()
}
fn put(collection: &str, key: &str, value: Value) -> ChangeOperation {
    ChangeOperation::Put {
        collection: collection.into(),
        key: key.into(),
        value,
    }
}

#[test]
fn enqueue_deduplicates_ids_including_done_and_dead_and_validates_duplicate_inputs() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    let original = database
        .queue_enqueue("mail", "welcome", json!({"user":"Ana"}), options(10))
        .unwrap();
    assert!(original.inserted);
    let before = database.stats().unwrap();
    let duplicate = database
        .queue_enqueue(
            "mail",
            "welcome",
            json!({"changed":true}),
            EnqueueOptions {
                now_ms: 50,
                delay_ms: 10,
                max_attempts: 1,
                priority: 1000,
            },
        )
        .unwrap();
    assert!(!duplicate.inserted);
    assert_eq!(duplicate.job, original.job);
    assert_eq!(database.stats().unwrap(), before);
    assert!(database
        .queue_enqueue("mail", "welcome", json!(u64::MAX), options(0))
        .is_err());
    assert!(database
        .queue_enqueue(
            "mail",
            "welcome",
            json!(1),
            EnqueueOptions {
                max_attempts: 0,
                ..options(0)
            }
        )
        .is_err());
    assert_eq!(database.stats().unwrap(), before);
    let claimed = database
        .queue_claim("mail", "worker", 10, 100)
        .unwrap()
        .unwrap();
    assert!(database
        .queue_ack("mail", "welcome", token(&claimed), 20, &[])
        .unwrap());
    let completed = database.queue_get("mail", "welcome").unwrap().unwrap();
    let done_stats = database.stats().unwrap();
    assert_eq!(
        database
            .queue_enqueue("mail", "welcome", Value::Null, options(99))
            .unwrap()
            .job,
        completed
    );
    assert_eq!(completed.status, JobStatus::Done);
    assert_eq!(database.stats().unwrap(), done_stats);
    database
        .queue_enqueue(
            "mail",
            "dead",
            json!(1),
            EnqueueOptions {
                max_attempts: 1,
                ..options(0)
            },
        )
        .unwrap();
    let claimed = database
        .queue_claim("mail", "worker", 20, 100)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, "dead");
    assert!(database
        .queue_fail("mail", "dead", token(&claimed), 21, "failed", 0)
        .unwrap());
    let dead = database.queue_get("mail", "dead").unwrap().unwrap();
    let dead_stats = database.stats().unwrap();
    assert_eq!(dead.status, JobStatus::Dead);
    assert_eq!(
        database
            .queue_enqueue("mail", "dead", json!("replacement"), options(50))
            .unwrap()
            .job,
        dead
    );
    assert_eq!(database.stats().unwrap(), dead_stats);
}

#[test]
fn concurrent_workers_never_claim_the_same_unexpired_job_twice() {
    let directory = TestDirectory::new();
    let database = Arc::new(Database::open(&directory.0).unwrap());
    for id in 0..12 {
        database
            .queue_enqueue("work", &format!("{id:02}"), json!({"id":id}), options(100))
            .unwrap();
    }
    let barrier = Arc::new(Barrier::new(24));
    let threads: Vec<_> = (0..24)
        .map(|worker| {
            let database = Arc::clone(&database);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                database
                    .queue_claim("work", &format!("worker:{worker}"), 100, 1000)
                    .unwrap()
            })
        })
        .collect();
    let claimed: Vec<_> = threads
        .into_iter()
        .filter_map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(claimed.len(), 12);
    assert_eq!(
        claimed
            .iter()
            .map(|job| &job.id)
            .collect::<BTreeSet<_>>()
            .len(),
        12
    );
    assert_eq!(claimed.iter().map(token).collect::<BTreeSet<_>>().len(), 12);
    assert!(claimed
        .iter()
        .all(|job| job.attempts == 1 && job.status == JobStatus::Leased));
    assert_eq!(database.stats().unwrap().latest_cursor, 24);
    assert_eq!(
        database.queue_stats("work", 100).unwrap(),
        QueueStats {
            queued: 0,
            leased: 12,
            done: 0,
            dead: 0,
            ready: 0,
            expired: 0
        }
    );
    drop(database);
    let reopened = Database::open(&directory.0).unwrap();
    assert!(reopened
        .queue_claim("work", "late", 100, 1000)
        .unwrap()
        .is_none());
}

#[test]
fn claim_order_respects_priority_effective_availability_delays_and_id_ties() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    for (id, priority, delay_ms) in [
        ("a", 5, 5),
        ("b", 10, 50),
        ("c", 5, 0),
        ("d", 5, 0),
        ("e", 0, 0),
    ] {
        database
            .queue_enqueue(
                "order",
                id,
                Value::Null,
                EnqueueOptions {
                    priority,
                    delay_ms,
                    ..options(100)
                },
            )
            .unwrap();
    }
    for expected in ["c", "d", "e"] {
        assert_eq!(
            database
                .queue_claim("order", "worker", 104, 1000)
                .unwrap()
                .unwrap()
                .id,
            expected
        );
    }
    assert_eq!(
        database
            .queue_claim("order", "worker", 105, 1000)
            .unwrap()
            .unwrap()
            .id,
        "a"
    );
    assert!(database
        .queue_claim("order", "worker", 149, 1000)
        .unwrap()
        .is_none());
    assert_eq!(
        database
            .queue_claim("order", "worker", 150, 1000)
            .unwrap()
            .unwrap()
            .id,
        "b"
    );
    database
        .queue_enqueue(
            "effective",
            "expired",
            Value::Null,
            EnqueueOptions {
                priority: 7,
                ..options(0)
            },
        )
        .unwrap();
    database
        .queue_claim("effective", "first", 0, 10)
        .unwrap()
        .unwrap();
    database
        .queue_enqueue(
            "effective",
            "queued",
            Value::Null,
            EnqueueOptions {
                priority: 7,
                delay_ms: 9,
                ..options(0)
            },
        )
        .unwrap();
    assert_eq!(
        database
            .queue_claim("effective", "next", 10, 100)
            .unwrap()
            .unwrap()
            .id,
        "queued"
    );
    assert_eq!(
        database
            .queue_claim("effective", "next", 10, 100)
            .unwrap()
            .unwrap()
            .id,
        "expired"
    );
}

#[test]
fn expired_reclaim_fences_stale_workers_and_rejects_ack_at_the_expiration_boundary() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .queue_enqueue("work", "job", json!({"n":1}), options(100))
        .unwrap();
    let first = database
        .queue_claim("work", "worker:1", 100, 10)
        .unwrap()
        .unwrap();
    let before = database.stats().unwrap();
    assert!(!database
        .queue_ack("work", "job", token(&first), 110, &[])
        .unwrap());
    assert!(!database
        .queue_fail("work", "job", token(&first), 110, "late", 0)
        .unwrap());
    assert!(!database
        .queue_extend("work", "job", token(&first), 110, 10)
        .unwrap());
    assert_eq!(database.stats().unwrap(), before);
    assert_eq!(
        database.queue_stats("work", 110).unwrap(),
        QueueStats {
            queued: 0,
            leased: 1,
            done: 0,
            dead: 0,
            ready: 1,
            expired: 1
        }
    );
    let second = database
        .queue_claim("work", "worker:2", 110, 20)
        .unwrap()
        .unwrap();
    assert_ne!(token(&first), token(&second));
    assert_eq!(second.attempts, 2);
    assert_eq!(second.lease_until, Some(130));
    let before = database.stats().unwrap();
    assert!(!database
        .queue_ack("work", "job", token(&first), 111, &[])
        .unwrap());
    assert!(!database
        .queue_fail("work", "job", token(&first), 111, "stale", 0)
        .unwrap());
    assert!(!database
        .queue_extend("work", "job", token(&first), 111, 100)
        .unwrap());
    assert_eq!(database.stats().unwrap(), before);
    assert!(database
        .queue_ack("work", "job", token(&second), 129, &[])
        .unwrap());
    assert_eq!(
        database.queue_stats("work", 130).unwrap(),
        QueueStats {
            queued: 0,
            leased: 0,
            done: 1,
            dead: 0,
            ready: 0,
            expired: 0
        }
    );
}

#[test]
fn failed_jobs_wait_for_their_retry_delay_and_success_clears_the_prior_error() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .queue_enqueue("work", "retry", Value::Null, options(0))
        .unwrap();
    let first = database
        .queue_claim("work", "worker", 0, 100)
        .unwrap()
        .unwrap();
    assert!(database
        .queue_fail("work", "retry", token(&first), 1, "temporary", 9)
        .unwrap());
    let waiting = database.queue_get("work", "retry").unwrap().unwrap();
    assert_eq!(waiting.status, JobStatus::Queued);
    assert_eq!(waiting.available_at, 10);
    assert_eq!(waiting.attempts, 1);
    assert_eq!(waiting.last_error.as_deref(), Some("temporary"));
    assert!(
        waiting.lease_token.is_none() && waiting.worker.is_none() && waiting.lease_until.is_none()
    );
    assert!(database
        .queue_claim("work", "worker", 9, 100)
        .unwrap()
        .is_none());
    let second = database
        .queue_claim("work", "worker", 10, 100)
        .unwrap()
        .unwrap();
    assert_eq!(second.attempts, 2);
    assert_ne!(token(&first), token(&second));
    assert!(database
        .queue_ack("work", "retry", token(&second), 11, &[])
        .unwrap());
    let done = database.queue_get("work", "retry").unwrap().unwrap();
    assert_eq!(done.status, JobStatus::Done);
    assert!(done.last_error.is_none());
    database
        .queue_enqueue(
            "errors",
            "maximum",
            Value::Null,
            EnqueueOptions {
                max_attempts: 1,
                ..options(0)
            },
        )
        .unwrap();
    let job = database
        .queue_claim("errors", "worker", 0, 100)
        .unwrap()
        .unwrap();
    let maximum_error = "é".repeat(2048);
    assert!(database
        .queue_fail("errors", "maximum", token(&job), 1, &maximum_error, 0)
        .unwrap());
    assert_eq!(
        database
            .queue_get("errors", "maximum")
            .unwrap()
            .unwrap()
            .last_error
            .as_deref(),
        Some(maximum_error.as_str())
    );
}

#[test]
fn exhausted_leases_need_recovery_and_retry_dead_starts_a_new_fenced_attempt_cycle() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    let original = database
        .queue_enqueue(
            "work",
            "job",
            json!({"message":"preserve"}),
            EnqueueOptions {
                max_attempts: 1,
                priority: 7,
                ..options(0)
            },
        )
        .unwrap()
        .job;
    let first = database
        .queue_claim("work", "worker", 0, 10)
        .unwrap()
        .unwrap();
    assert!(database
        .queue_claim("work", "next", 10, 10)
        .unwrap()
        .is_none());
    assert_eq!(
        database.queue_stats("work", 10).unwrap(),
        QueueStats {
            queued: 0,
            leased: 1,
            done: 0,
            dead: 0,
            ready: 0,
            expired: 1
        }
    );
    let recovery = database.queue_recover_expired("work", 10, 1).unwrap();
    assert_eq!(recovery.dead, 1);
    assert_eq!(recovery.requeued, 0);
    assert!(!recovery.has_more);
    let dead = database.queue_get("work", "job").unwrap().unwrap();
    assert_eq!(dead.status, JobStatus::Dead);
    assert_eq!(dead.last_error.as_deref(), Some("lease expired"));
    assert!(dead.lease_until.is_none() && dead.lease_token.is_none() && dead.worker.is_none());
    assert!(database.queue_retry_dead("work", "job", 20).unwrap());
    let queued = database.queue_get("work", "job").unwrap().unwrap();
    assert_eq!(queued.attempts, 0);
    assert_eq!(queued.payload, original.payload);
    assert_eq!(queued.created_at, original.created_at);
    assert_eq!(queued.priority, original.priority);
    assert_eq!(queued.max_attempts, 1);
    assert_eq!(queued.available_at, 20);
    assert!(queued.last_error.is_none());
    let second = database
        .queue_claim("work", "worker", 20, 10)
        .unwrap()
        .unwrap();
    assert_eq!(second.attempts, 1);
    assert_ne!(token(&second), token(&first));
    assert!(!database
        .queue_ack("work", "job", token(&first), 21, &[])
        .unwrap());
    assert!(database
        .queue_fail("work", "job", token(&second), 21, "permanent", 0)
        .unwrap());
    assert_eq!(
        database.queue_get("work", "job").unwrap().unwrap().status,
        JobStatus::Dead
    );
    assert!(!database.queue_retry_dead("work", "missing", 30).unwrap());
    assert!(database.queue_retry_dead("work", "job", 30).unwrap());
    assert!(!database.queue_retry_dead("work", "job", 30).unwrap());
    let third = database
        .queue_claim("work", "worker", 30, 10)
        .unwrap()
        .unwrap();
    assert_ne!(token(&third), token(&second));
}

#[test]
fn recovery_is_bounded_deterministic_and_atomic_when_the_selected_batch_exceeds_a_frame() {
    let directory = TestDirectory::new();
    let database = Database::open_with_options(
        &directory.0,
        Options {
            max_frame_bytes: 600,
            ..Options::default()
        },
    )
    .unwrap();
    for (id, max_attempts) in [("a", 2), ("b", 2), ("c", 1)] {
        database
            .queue_enqueue(
                "work",
                id,
                Value::Null,
                EnqueueOptions {
                    max_attempts,
                    ..options(0)
                },
            )
            .unwrap();
    }
    for lease in [10, 20, 30] {
        database
            .queue_claim("work", "worker", 0, lease)
            .unwrap()
            .unwrap();
    }
    let before = database.stats().unwrap();
    assert!(matches!(
        database.queue_recover_expired("work", 30, 3),
        Err(Error::LimitExceeded(_))
    ));
    assert_eq!(database.stats().unwrap(), before);
    assert_eq!(database.queue_stats("work", 30).unwrap().leased, 3);
    let first = database.queue_recover_expired("work", 30, 1).unwrap();
    assert_eq!((first.requeued, first.dead, first.has_more), (1, 0, true));
    assert_eq!(
        database.queue_get("work", "a").unwrap().unwrap().status,
        JobStatus::Queued
    );
    let second = database.queue_recover_expired("work", 30, 1).unwrap();
    assert_eq!(
        (second.requeued, second.dead, second.has_more),
        (1, 0, true)
    );
    let third = database.queue_recover_expired("work", 30, 1).unwrap();
    assert_eq!((third.requeued, third.dead, third.has_more), (0, 1, false));
    assert_eq!(
        database.queue_stats("work", 30).unwrap(),
        QueueStats {
            queued: 2,
            leased: 0,
            done: 0,
            dead: 1,
            ready: 2,
            expired: 0
        }
    );
    let cursor = database.stats().unwrap().latest_cursor;
    assert_eq!(
        database
            .queue_recover_expired("work", 30, 10)
            .unwrap()
            .requeued,
        0
    );
    assert_eq!(database.stats().unwrap().latest_cursor, cursor);
}

#[test]
fn extending_a_lease_preserves_its_token_never_reduces_it_and_does_not_log_a_noop() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .queue_enqueue("work", "job", Value::Null, options(0))
        .unwrap();
    let job = database
        .queue_claim("work", "worker", 0, 10)
        .unwrap()
        .unwrap();
    let before = database.stats().unwrap();
    assert!(database
        .queue_extend("work", "job", token(&job), 1, 2)
        .unwrap());
    assert_eq!(database.stats().unwrap(), before);
    assert!(database
        .queue_extend("work", "job", token(&job), 5, 20)
        .unwrap());
    let extended = database.queue_get("work", "job").unwrap().unwrap();
    assert_eq!(extended.lease_until, Some(25));
    assert_eq!(extended.lease_token, job.lease_token);
    assert_eq!(extended.attempts, job.attempts);
    assert_eq!(
        database.stats().unwrap().latest_cursor,
        before.latest_cursor + 1
    );
    let before = database.stats().unwrap();
    assert!(!database
        .queue_extend("work", "job", token(&job), 25, 1)
        .unwrap());
    assert_eq!(database.stats().unwrap(), before);
    assert!(database
        .queue_ack("work", "job", token(&job), 24, &[])
        .unwrap());
}

#[test]
fn ack_commits_business_and_outbox_writes_with_done_state_in_one_frame_or_rolls_everything_back() {
    let directory = TestDirectory::new();
    let database = Database::open_with_options(
        &directory.0,
        Options {
            max_frame_bytes: 1000,
            max_document_bytes: 2048,
            ..Options::default()
        },
    )
    .unwrap();
    database
        .queue_enqueue("work", "job", json!({"order":"001"}), options(0))
        .unwrap();
    let job = database
        .queue_claim("work", "worker", 0, 100)
        .unwrap()
        .unwrap();
    let before = database.stats().unwrap();
    for reserved in ["_cedro.jobs.work", "_cedro.jobs.other"] {
        assert!(matches!(
            database.queue_ack(
                "work",
                "job",
                token(&job),
                1,
                &[put(reserved, "job", Value::Null)]
            ),
            Err(Error::InvalidInput(_))
        ));
    }
    assert!(database
        .queue_ack("work", "job", "stale", 1, &[put("orders", "", json!(1))])
        .is_err());
    assert!(matches!(
        database.queue_ack(
            "work",
            "job",
            token(&job),
            1,
            &[put("orders", "too-large", json!({"text":"x".repeat(650)}))]
        ),
        Err(Error::LimitExceeded(_))
    ));
    assert_eq!(database.stats().unwrap(), before);
    assert_eq!(
        database.queue_get("work", "job").unwrap().unwrap().status,
        JobStatus::Leased
    );
    assert!(database.get("orders", "too-large").unwrap().is_none());
    let operations = [
        put(
            "orders",
            "001",
            json!({"status":"processed","full_range_allowed":u64::MAX}),
        ),
        put("outbox", "order:001", json!({"kind":"order.processed"})),
    ];
    assert!(database
        .queue_ack("work", "job", token(&job), 1, &operations)
        .unwrap());
    let feed = database.changes(before.latest_cursor, 100).unwrap();
    assert_eq!(feed.transactions.len(), 1);
    assert_eq!(feed.transactions[0].operations.len(), 3);
    assert_eq!(feed.transactions[0].operations[1..], operations);
    assert_eq!(
        database.queue_get("work", "job").unwrap().unwrap().status,
        JobStatus::Done
    );
    assert!(database.get("orders", "001").unwrap().is_some());
    assert!(database.get("outbox", "order:001").unwrap().is_some());
    let committed = database.stats().unwrap();
    assert!(!database
        .queue_ack("work", "job", token(&job), 2, &operations)
        .unwrap());
    assert_eq!(database.stats().unwrap(), committed);
    let limited_directory = TestDirectory::new();
    let limited = Database::open_with_options(
        &limited_directory.0,
        Options {
            max_state_bytes: 600,
            ..Options::default()
        },
    )
    .unwrap();
    limited
        .queue_enqueue("work", "job", Value::Null, options(0))
        .unwrap();
    let job = limited
        .queue_claim("work", "worker", 0, 100)
        .unwrap()
        .unwrap();
    let before = limited.stats().unwrap();
    assert!(matches!(
        limited.queue_ack(
            "work",
            "job",
            token(&job),
            1,
            &[put("business", "huge", json!({"text":"x".repeat(500)}))]
        ),
        Err(Error::LimitExceeded(_))
    ));
    assert_eq!(limited.stats().unwrap(), before);
    assert_eq!(
        limited.queue_get("work", "job").unwrap().unwrap().status,
        JobStatus::Leased
    );
    assert!(limited.get("business", "huge").unwrap().is_none());
}

#[test]
fn queue_inputs_and_arithmetic_reject_unsafe_numbers_nuls_invalid_limits_and_overflow_before_writes(
) {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    let before = database.stats().unwrap();
    for name in ["", "bad/name", "á", "bad\0name"] {
        assert!(database
            .queue_enqueue(name, "id", Value::Null, options(0))
            .is_err());
    }
    assert!(database
        .queue_enqueue(&"q".repeat(129), "id", Value::Null, options(0))
        .is_err());
    for id in ["", "nul\0id"] {
        assert!(database
            .queue_enqueue("work", id, Value::Null, options(0))
            .is_err());
    }
    for invalid in [
        EnqueueOptions {
            max_attempts: 0,
            ..options(0)
        },
        EnqueueOptions {
            max_attempts: 1001,
            ..options(0)
        },
        EnqueueOptions {
            priority: 1001,
            ..options(0)
        },
        EnqueueOptions {
            delay_ms: 1,
            ..options(MAX_SAFE)
        },
        options(MAX_SAFE + 1),
    ] {
        assert!(database
            .queue_enqueue("work", "id", Value::Null, invalid)
            .is_err());
    }
    for payload in [
        json!({"nested":[MAX_SAFE+1]}),
        json!({"nested":{"n":-9_007_199_254_740_992i64}}),
        json!(u64::MAX),
        json!(1e100),
    ] {
        assert!(database
            .queue_enqueue("work", "id", payload, options(0))
            .is_err());
    }
    for worker in ["", "nul\0worker"] {
        assert!(database.queue_claim("work", worker, 0, 1).is_err());
    }
    assert!(database
        .queue_claim("work", &"🌲".repeat(33), 0, 1)
        .is_err());
    for (now, lease) in [(0, 0), (MAX_SAFE, 1), (0, MAX_SAFE + 1), (u64::MAX, 1)] {
        assert!(database.queue_claim("work", "worker", now, lease).is_err());
    }
    assert!(database
        .queue_extend("work", "missing", "token", 0, 0)
        .is_err());
    assert!(database
        .queue_fail("work", "missing", "token", MAX_SAFE, "error", 1)
        .is_err());
    assert!(database
        .queue_fail("work", "missing", "token", 0, &"é".repeat(2049), 0)
        .is_err());
    for error in ["", "nul\0error"] {
        assert!(database
            .queue_fail("work", "missing", "token", 0, error, 0)
            .is_err());
    }
    for token in ["", "nul\0token"] {
        assert!(database
            .queue_ack("work", "missing", token, 0, &[])
            .is_err());
        assert!(database
            .queue_fail("work", "missing", token, 0, "error", 0)
            .is_err());
        assert!(database
            .queue_extend("work", "missing", token, 0, 1)
            .is_err());
    }
    assert!(database
        .queue_ack("work", "missing", "token", MAX_SAFE + 1, &[])
        .is_err());
    assert!(database
        .queue_retry_dead("work", "missing", MAX_SAFE + 1)
        .is_err());
    assert!(database.queue_stats("work", MAX_SAFE + 1).is_err());
    for limit in [0, 1001, usize::MAX] {
        assert!(database.queue_recover_expired("work", 0, limit).is_err());
    }
    assert_eq!(database.stats().unwrap(), before);
    let accepted = database
        .queue_enqueue(
            "valid.name-1_2",
            "unicode:🌲",
            json!({"range":[MAX_SAFE,-9_007_199_254_740_991i64,0.5]}),
            options(MAX_SAFE),
        )
        .unwrap();
    assert!(accepted.inserted);
    assert_eq!(accepted.job.available_at, MAX_SAFE);
}

#[test]
fn malformed_internal_documents_are_errors_even_when_an_earlier_job_is_eligible() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    let valid = database
        .queue_enqueue("work", "a", Value::Null, options(0))
        .unwrap()
        .job;
    let claimed = database
        .queue_claim("work", "worker", 0, 10)
        .unwrap()
        .unwrap();
    let mut invalid_cases = Vec::new();
    let original = serde_json::to_value(&valid).unwrap();
    for (field, value) in [
        ("schema_version", json!(2)),
        ("id", json!("wrong")),
        ("attempts", json!(1001)),
        ("created_at", json!(MAX_SAFE + 1)),
        ("lease_token", json!("unexpected")),
        ("payload", json!({"unsafe":u64::MAX})),
        ("last_error", json!("x".repeat(4097))),
        ("unknown", json!(1)),
    ] {
        let mut bad = original.clone();
        bad[field] = value;
        invalid_cases.push(bad);
    }
    invalid_cases.push(json!({}));
    let leased = serde_json::to_value(&claimed).unwrap();
    for (field, value) in [
        ("lease_token", Value::Null),
        ("worker", json!("nul\0worker")),
        ("worker", json!("")),
        ("lease_until", json!(0)),
        ("status", json!("done")),
        ("last_error", json!("")),
    ] {
        let mut bad = leased.clone();
        bad[field] = value;
        invalid_cases.push(bad);
    }
    // Build a legacy-valid generic transaction in a CLOSED log: the reserved
    // namespace is read-only through public CRUD starting with version 0.5.
    drop(database);
    let log_path = directory.0.join("cedro.log");
    let baseline = fs::read(&log_path).unwrap();
    for mut invalid in invalid_cases {
        // Most fixtures keep all other fields valid, including id == stored key.
        if invalid.get("id").is_some_and(|id| id == "a") {
            invalid["id"] = json!("z");
        }
        let payload = serde_json::to_vec(&json!({"ops":[{
            "kind":"put","collection":"_cedro.jobs.work","key":"z","value":invalid
        }]}))
        .unwrap();
        let length = payload.len() as u32;
        let mut legacy_log = baseline.clone();
        legacy_log.extend_from_slice(&length.to_le_bytes());
        legacy_log.extend_from_slice(&(!length).to_le_bytes());
        legacy_log.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        legacy_log.extend_from_slice(&payload);
        fs::write(&log_path, legacy_log).unwrap();
        let database = Database::open(&directory.0).unwrap();
        let before = database.stats().unwrap();
        assert!(matches!(
            database.queue_get("work", "z"),
            Err(Error::Corruption(_))
        ));
        assert!(matches!(
            database.queue_claim("work", "next", 10, 10),
            Err(Error::Corruption(_))
        ));
        assert!(matches!(
            database.queue_stats("work", 10),
            Err(Error::Corruption(_))
        ));
        assert!(matches!(
            database.queue_recover_expired("work", 10, 1),
            Err(Error::Corruption(_))
        ));
        assert!(matches!(
            database.queue_enqueue("work", "z", Value::Null, options(0)),
            Err(Error::Corruption(_))
        ));
        assert_eq!(database.stats().unwrap(), before);
        assert_eq!(database.queue_get("work", "a").unwrap().unwrap(), claimed);
    }
}

#[test]
fn all_mutations_reject_an_active_writer_while_reads_keep_committed_state() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .queue_enqueue("work", "job", Value::Null, options(0))
        .unwrap();
    let job = database
        .queue_claim("work", "worker", 0, 100)
        .unwrap()
        .unwrap();
    let before = database.stats().unwrap();
    let transaction = database.begin_write().unwrap();
    assert!(matches!(
        database.queue_enqueue("work", "new", Value::Null, options(0)),
        Err(Error::WriterBusy)
    ));
    assert!(matches!(
        database.queue_claim("work", "worker", 1, 100),
        Err(Error::WriterBusy)
    ));
    assert!(matches!(
        database.queue_ack("work", "job", token(&job), 1, &[]),
        Err(Error::WriterBusy)
    ));
    assert!(matches!(
        database.queue_fail("work", "job", token(&job), 1, "error", 0),
        Err(Error::WriterBusy)
    ));
    assert!(matches!(
        database.queue_extend("work", "job", token(&job), 1, 100),
        Err(Error::WriterBusy)
    ));
    assert!(matches!(
        database.queue_recover_expired("work", 100, 10),
        Err(Error::WriterBusy)
    ));
    assert!(matches!(
        database.queue_retry_dead("work", "job", 1),
        Err(Error::WriterBusy)
    ));
    assert_eq!(database.queue_get("work", "job").unwrap().unwrap(), job);
    assert_eq!(database.queue_stats("work", 1).unwrap().leased, 1);
    assert_eq!(database.stats().unwrap(), before);
    drop(transaction);
}

#[test]
fn reopen_and_backup_restore_preserve_jobs_tokens_and_history_cursors() {
    let directory = TestDirectory::new();
    let source_path = directory.0.join("source");
    let database = Database::open(&source_path).unwrap();
    database
        .queue_enqueue("work", "a", json!({"unicode":"José 🌲"}), options(0))
        .unwrap();
    database
        .queue_enqueue("work", "b", Value::Null, options(0))
        .unwrap();
    let job = database
        .queue_claim("work", "worker", 0, 10)
        .unwrap()
        .unwrap();
    let backup = directory.0.join("snapshot.cedrobak");
    let report = database.backup(&backup).unwrap();
    let history = database.changes(0, 1000).unwrap();
    assert_eq!(report.transactions, 3);
    drop(database);
    let reopened = Database::open(&source_path).unwrap();
    assert_eq!(reopened.queue_get("work", "a").unwrap().unwrap(), job);
    assert_eq!(reopened.changes(0, 1000).unwrap(), history);
    assert!(reopened
        .queue_ack("work", "a", token(&job), 1, &[])
        .unwrap());
    let restored_path = directory.0.join("restored");
    assert_eq!(
        Database::restore_backup(&backup, &restored_path).unwrap(),
        report
    );
    let restored = Database::open(&restored_path).unwrap();
    assert_eq!(restored.queue_get("work", "a").unwrap().unwrap(), job);
    assert_eq!(restored.stats().unwrap().latest_cursor, 3);
    assert_eq!(restored.changes(0, 1000).unwrap(), history);
    // The pending b job has earlier effective availability than expired a.
    let next = restored
        .queue_claim("work", "next", 10, 10)
        .unwrap()
        .unwrap();
    assert_eq!(next.id, "b");
    let reclaimed = restored
        .queue_claim("work", "next", 10, 10)
        .unwrap()
        .unwrap();
    assert_eq!(reclaimed.id, "a");
    assert_ne!(reclaimed.lease_token, job.lease_token);
    assert_eq!(restored.stats().unwrap().latest_cursor, 5);
    assert_eq!(restored.changes(3, 100).unwrap().transactions.len(), 2);
}

#[test]
fn enqueue_options_wire_defaults_and_unknown_field_rejection_are_explicit() {
    let defaults: EnqueueOptions = serde_json::from_value(json!({"now_ms":100})).unwrap();
    assert_eq!(
        (
            defaults.now_ms,
            defaults.delay_ms,
            defaults.max_attempts,
            defaults.priority
        ),
        (100, 0, 3, 0)
    );
    assert!(serde_json::from_value::<EnqueueOptions>(json!({})).is_err());
    assert!(serde_json::from_value::<EnqueueOptions>(json!({"now_ms":0,"unknown":1})).is_err());
    assert!(
        serde_json::from_value::<EnqueueOptions>(json!({"now_ms":0,"max_attempts":1.5})).is_err()
    );
}

#[test]
fn public_mutations_cannot_override_queue_collections_and_reads_remain_available() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .queue_enqueue("work", "job", json!({"n":1}), options(0))
        .unwrap();
    let internal = "_cedro.jobs.work";
    let original = database.get(internal, "job").unwrap().unwrap();
    assert_eq!(database.scan(internal).unwrap().len(), 1);
    assert_eq!(
        database.query(internal, Query::default()).unwrap().records[0].value,
        original
    );
    database.create_index(internal, "status").unwrap();
    let before = database.stats().unwrap();
    for collection in [internal, "_cedro.jobs.new", "_cedro.jobs."] {
        assert!(matches!(
            database.put(collection, "job", json!({"malformed":true})),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            database.delete(collection, "job"),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            database.compare_exchange(collection, "job", None, Some(Value::Null)),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            database.compare_exchange(collection, "job", Some(original.clone()), None),
            Err(Error::InvalidInput(_))
        ));
        for reserved in [
            put(collection, "job", Value::Null),
            ChangeOperation::Delete {
                collection: collection.into(),
                key: "job".into(),
            },
        ] {
            assert!(matches!(
                database
                    .conditional_batch(&[], &[put("business", "uncommitted", json!(1)), reserved]),
                Err(Error::InvalidInput(_))
            ));
        }
        let mut transaction = database.begin_write().unwrap();
        transaction
            .put("business", "uncommitted", json!(1))
            .unwrap();
        assert!(matches!(
            transaction.put(collection, "job", Value::Null),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            transaction.delete(collection, "job"),
            Err(Error::InvalidInput(_))
        ));
        drop(transaction);
        assert_eq!(database.stats().unwrap(), before);
        assert_eq!(
            database.get(internal, "job").unwrap(),
            Some(original.clone())
        );
        assert!(database.get("business", "uncommitted").unwrap().is_none());
    }
    let condition = Condition::Equals {
        collection: internal.into(),
        key: "job".into(),
        value: original.clone(),
    };
    let result = database
        .conditional_batch(&[condition], &[put("business", "observed", json!(true))])
        .unwrap();
    assert!(result.applied);
    assert_eq!(result.cursor, before.latest_cursor + 1);
    assert_eq!(database.get(internal, "job").unwrap(), Some(original));
    // A neighboring name without the trailing dot is outside the reserved prefix.
    database
        .put("_cedro.jobs", "ordinary", json!(true))
        .unwrap();
    assert_eq!(
        database.get("_cedro.jobs", "ordinary").unwrap(),
        Some(json!(true))
    );
    assert!(database
        .queue_claim("work", "worker", 0, 100)
        .unwrap()
        .is_some());
}

#[test]
fn lease_metadata_needs_document_frame_and_state_headroom_and_failed_claims_remain_recoverable() {
    let probe_directory = TestDirectory::new();
    let probe = Database::open(&probe_directory.0).unwrap();
    let payload = json!({"text":"x".repeat(64)});
    probe
        .queue_enqueue("work", "job", payload.clone(), options(0))
        .unwrap();
    let probe_stats = probe.stats().unwrap();
    let document_bytes =
        serde_json::to_vec(&probe.get("_cedro.jobs.work", "job").unwrap().unwrap())
            .unwrap()
            .len();
    let frame_payload_bytes = probe_stats.log_bytes as usize - 16 - 12;
    let content_bytes = probe_stats.content_bytes as usize;
    for limits in [
        Options {
            max_document_bytes: document_bytes,
            ..Options::default()
        },
        Options {
            max_frame_bytes: frame_payload_bytes,
            ..Options::default()
        },
        Options {
            max_state_bytes: content_bytes,
            ..Options::default()
        },
    ] {
        let directory = TestDirectory::new();
        let database = Database::open_with_options(&directory.0, limits).unwrap();
        let queued = database
            .queue_enqueue("work", "job", payload.clone(), options(0))
            .unwrap()
            .job;
        let before = database.stats().unwrap();
        assert!(matches!(
            database.queue_claim("work", &"w".repeat(128), 0, 1000),
            Err(Error::LimitExceeded(_))
        ));
        assert_eq!(database.stats().unwrap(), before);
        assert_eq!(
            database.queue_get("work", "job").unwrap(),
            Some(queued.clone())
        );
        assert!(database
            .changes(before.latest_cursor, 100)
            .unwrap()
            .transactions
            .is_empty());
        drop(database);
        let reopened = Database::open_with_options(&directory.0, limits).unwrap();
        assert_eq!(reopened.queue_get("work", "job").unwrap(), Some(queued));
        drop(reopened);
        let larger = Database::open(&directory.0).unwrap();
        let claimed = larger
            .queue_claim("work", &"w".repeat(128), 0, 1000)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.attempts, 1);
        assert_eq!(claimed.status, JobStatus::Leased);
        assert_eq!(
            larger.stats().unwrap().latest_cursor,
            before.latest_cursor + 1
        );
    }
}
