use cedrodb::{
    AgentStatus, ChangeOperation, CheckpointOptions, Condition, Database, EnqueueOptions, Filter,
    FilterOp, JobStatus, Query, SearchQuery,
};
use serde_json::json;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

struct DemoDirectory(PathBuf);

impl Drop for DemoDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let directory = DemoDirectory(
        std::env::temp_dir().join(format!("cedrodb-portfolio-{}-{unique}", std::process::id())),
    );
    std::fs::create_dir(&directory.0)?;
    {
        let database = Database::open(&directory.0)?;
        let mut transaction = database.begin_write()?;
        transaction.put(
            "orders",
            "order:001",
            json!({"customer": "Ana", "amount": 125.5, "status": "paid"}),
        )?;
        transaction.put(
            "orders",
            "order:002",
            json!({"customer": "Luis", "amount": 74.5, "status": "paid"}),
        )?;
        transaction.put(
            "orders",
            "order:003",
            json!({"customer": "María", "amount": 250.0, "status": "pending"}),
        )?;
        // An order batch and its delivery intent share the same durable frame.
        transaction.put(
            "outbox",
            "event:001",
            json!({"kind":"orders.created","count":3,"processed":false}),
        )?;
        transaction.commit()?;
        let mut rollback = database.begin_write()?;
        rollback.put("orders", "order:discarded", json!({"amount": 99999}))?;
        // Dropping rollback deliberately leaves no record on disk.
    }
    let database = Database::open(&directory.0)?;
    let records: Vec<_> = database
        .scan("orders")?
        .into_iter()
        .map(|(key, value)| json!({"key": key, "value": value}))
        .collect();
    let aggregate = database.aggregate("orders", "amount")?;
    database.create_index("orders", "status")?;
    let query = database.query(
        "orders",
        Query {
            filters: vec![
                Filter {
                    field: "status".into(),
                    op: FilterOp::Eq,
                    value: json!("paid"),
                },
                Filter {
                    field: "amount".into(),
                    op: FilterOp::Gte,
                    value: json!(100),
                },
            ],
            limit: 10,
            after: None,
        },
    )?;
    let expected = json!({"kind":"orders.created","count":3,"processed":false});
    let processed = json!({"kind":"orders.created","count":3,"processed":true});
    let matched = database.compare_exchange(
        "outbox",
        "event:001",
        Some(expected.clone()),
        Some(processed.clone()),
    )?;
    let retry =
        database.compare_exchange("outbox", "event:001", Some(expected), Some(processed))?;
    let changes = database.changes(0, 10)?;
    let stats = database.stats()?;
    let verified = database.verify()?;
    let backup_path = directory.0.join("orders.cedrobak");
    database.backup(&backup_path)?;
    let restored_path = directory.0.join("restored");
    let restored_report = Database::restore_backup(&backup_path, &restored_path)?;
    let restored = Database::open(&restored_path)?;
    let cursors_preserved = restored.changes(0, 10)? == changes;
    drop(restored);
    let stock = Database::open(directory.0.join("stock-demo"))?;
    let expected_stock = json!({"available":1,"revision":1});
    stock.put("inventory", "sku:1", expected_stock.clone())?;
    let conditions = vec![
        Condition::Absent {
            collection: "orders".into(),
            key: "order:1".into(),
        },
        Condition::Equals {
            collection: "inventory".into(),
            key: "sku:1".into(),
            value: expected_stock.clone(),
        },
    ];
    let operations = vec![
        ChangeOperation::Put {
            collection: "inventory".into(),
            key: "sku:1".into(),
            value: json!({"available":0,"revision":2}),
        },
        ChangeOperation::Put {
            collection: "orders".into(),
            key: "order:1".into(),
            value: json!({"sku":"sku:1","quantity":1}),
        },
        ChangeOperation::Put {
            collection: "outbox".into(),
            key: "event:1".into(),
            value: json!({"type":"stock.reserved","order":"order:1"}),
        },
    ];
    let reserved = stock.conditional_batch(&conditions, &operations)?;
    let duplicate = stock.conditional_batch(&conditions, &operations)?;
    let stale = stock.conditional_batch(
        &[
            Condition::Absent {
                collection: "orders".into(),
                key: "order:2".into(),
            },
            Condition::Equals {
                collection: "inventory".into(),
                key: "sku:1".into(),
                value: expected_stock,
            },
        ],
        &operations,
    )?;
    let remaining = stock.get("inventory", "sku:1")?.unwrap()["available"].clone();
    let jobs_path = directory.0.join("jobs-demo");
    let jobs = Database::open(&jobs_path)?;
    jobs.queue_enqueue(
        "reports",
        "job:report",
        json!({"type":"report.generate","site":"SDQ"}),
        EnqueueOptions {
            now_ms: 1000,
            priority: 5,
            ..EnqueueOptions::default()
        },
    )?;
    let duplicate_job = jobs.queue_enqueue(
        "reports",
        "job:report",
        json!({"type":"ignored duplicate"}),
        EnqueueOptions {
            now_ms: 1000,
            ..EnqueueOptions::default()
        },
    )?;
    let first_lease = jobs.queue_claim("reports", "worker-A", 1000, 100)?.unwrap();
    drop(jobs); // Simulate an application restart with an unfinished lease.
    let jobs = Database::open(&jobs_path)?;
    let history_resumed = jobs.queue_get("reports", "job:report")?.unwrap().status
        == JobStatus::Leased
        && jobs.changes(1, 10)?.transactions.len() == 1;
    let new_lease = jobs.queue_claim("reports", "worker-B", 1100, 100)?.unwrap();
    let stale_ack = jobs.queue_ack(
        "reports",
        "job:report",
        first_lease.lease_token.as_ref().unwrap(),
        1101,
        &[],
    )?;
    let completed = jobs.queue_ack(
        "reports",
        "job:report",
        new_lease.lease_token.as_ref().unwrap(),
        1101,
        &[
            ChangeOperation::Put {
                collection: "reports".into(),
                key: "report:1".into(),
                value: json!({"state":"generated"}),
            },
            ChangeOperation::Put {
                collection: "outbox".into(),
                key: "event:report:1".into(),
                value: json!({"type":"report.generated"}),
            },
        ],
    )?;
    jobs.queue_enqueue(
        "reports",
        "job:failure",
        json!({"type":"unavailable endpoint"}),
        EnqueueOptions {
            now_ms: 1200,
            max_attempts: 1,
            ..EnqueueOptions::default()
        },
    )?;
    let doomed = jobs.queue_claim("reports", "worker-C", 1200, 100)?.unwrap();
    jobs.queue_fail(
        "reports",
        &doomed.id,
        doomed.lease_token.as_ref().unwrap(),
        1201,
        "external endpoint unavailable",
        0,
    )?;
    let dead_lettered =
        jobs.queue_get("reports", "job:failure")?.unwrap().status == JobStatus::Dead;
    let agent_path = directory.0.join("agent-demo");
    let agent_db = Database::open(&agent_path)?;
    // Synthetic vectors make the example deterministic. No embedding model,
    // model endpoint, or retrieval-quality benchmark is involved.
    let checkpoint = agent_db.agent_checkpoint(
        "support",
        "conversation:42",
        json!({"messages":[{"role":"user","content":"What is the refund policy?"}]}),
        CheckpointOptions {
            now_ms: 1000,
            status: AgentStatus::Waiting,
            metadata: json!({"schema":"support-v1"}),
            operations: vec![
                ChangeOperation::Put {
                    collection: "memory".into(), key: "refund-policy".into(),
                    value: json!({"text":"Refund refund policy: 30 days.","embedding":[1.0,0.0],"model":"demo-v1","tenant":"shop-1"}),
                },
                ChangeOperation::Put {
                    collection: "memory".into(), key: "shipping-policy".into(),
                    value: json!({"text":"Shipping deliveries: 3 days.","embedding":[0.8,0.2],"model":"demo-v1","tenant":"shop-1"}),
                },
                ChangeOperation::Put {
                    collection: "memory".into(), key: "another-model".into(),
                    value: json!({"text":"Different embedding model.","embedding":[0,0,0],"model":"other","tenant":"shop-2"}),
                },
            ],
            ..Default::default()
        },
    )?;
    let initial_revision = checkpoint.checkpoint.as_ref().unwrap().revision;
    drop(agent_db);
    let agent_db = Database::open(agent_path)?;
    let resumed_checkpoint = agent_db.agent_get("support", "conversation:42")?.unwrap();
    let resumed = resumed_checkpoint.revision == initial_revision
        && resumed_checkpoint.status == AgentStatus::Waiting;
    agent_db.create_index("memory", "model")?;
    let memory = agent_db.search(
        "memory",
        SearchQuery {
            text: Some("refund policy".into()),
            vector: Some(vec![1.0, 0.0]),
            filters: vec![
                Filter {
                    field: "model".into(),
                    op: FilterOp::Eq,
                    value: json!("demo-v1"),
                },
                Filter {
                    field: "tenant".into(),
                    op: FilterOp::Eq,
                    value: json!("shop-1"),
                },
            ],
            ..Default::default()
        },
    )?;
    let agent_completed = agent_db.agent_checkpoint(
        "support",
        "conversation:42",
        json!({"answer":"Refunds are available within 30 days.","memory_keys":["refund-policy"]}),
        CheckpointOptions {
            expected_revision: Some(initial_revision),
            now_ms: 1001,
            status: AgentStatus::Completed,
            operations: vec![
                ChangeOperation::Put {
                    collection: "answers".into(),
                    key: "conversation:42".into(),
                    value: json!({"source":"refund-policy"}),
                },
                ChangeOperation::Put {
                    collection: "outbox".into(),
                    key: "conversation:42:answered".into(),
                    value: json!({"event_id":"conversation:42:answered","type":"answer.ready"}),
                },
            ],
            ..Default::default()
        },
    )?;
    let agent_stale = agent_db.agent_checkpoint(
        "support",
        "conversation:42",
        json!({"answer":"obsolete"}),
        CheckpointOptions {
            expected_revision: Some(initial_revision),
            now_ms: 1002,
            ..Default::default()
        },
    )?;
    let agent_changes = agent_db.changes(initial_revision, 10)?;
    let atomic_outbox = agent_completed.applied
        && agent_changes.transactions.len() == 1
        && agent_changes.transactions[0].operations.len() == 3
        && agent_changes.transactions[0].cursor == agent_completed.cursor
        && agent_db
            .get("outbox", "conversation:42:answered")?
            .is_some();
    let output = json!({
        "engine": {"name": "CedroDB", "version": env!("CARGO_PKG_VERSION")},
        "generatedBy": "cargo run --example portfolio_demo",
        "records": records,
        "aggregate": {
            "field": "amount", "count": aggregate.count, "sum": aggregate.sum,
            "min": aggregate.min, "max": aggregate.max
        },
        "advanced": {
            "query": query,
            "changes": changes,
            "compare_exchange": {"matched": matched, "mismatch_rejected": !retry},
            "stats": stats
        },
        "use_cases": {
            "maintenance": {"verified":verified,"restored":restored_report,"cursors_preserved":cursors_preserved},
            "inventory": {"reserved":reserved.applied,"duplicate_rejected":!duplicate.applied,
                "stale_rejected":!stale.applied,"failed_condition":stale.failed_condition,
                "remaining":remaining,"orders":stock.scan("orders")?.len(),
                "outbox_events":stock.scan("outbox")?.len(),"cursor":stock.stats()?.latest_cursor},
            "jobs": {"deduplicated":!duplicate_job.inserted,"expired_reclaimed":new_lease.attempts == 2,
                "stale_ack_rejected":!stale_ack,"completed":completed,"dead_lettered":dead_lettered,
                "attempts":new_lease.attempts,"outputs":jobs.scan("reports")?.len(),
                "history_resumed":history_resumed,"stats":jobs.queue_stats("reports",1201)?},
            "agent_memory": {"checkpoint_saved":checkpoint.applied,"stale_rejected":!agent_stale.applied,
                "resumed":resumed,"atomic_outbox":atomic_outbox,"search_mode":memory.mode,
                "search_hits":memory.hits.len(),"scanned":memory.scanned,
                "top_key":memory.hits.first().map(|hit|hit.key.as_str()),"revision":agent_completed.cursor}
        },
        "capabilities": [
            "Rust engine with its own append-only transaction log",
            "JSON documents, committed reads and ordered scans",
            "Atomic frame commits followed by sync_all",
            "CRC32 verification and incomplete-tail recovery",
            "Numeric count, sum, min and max",
            "Resumable durable changefeed with complete transaction boundaries",
            "Atomic compare-and-exchange and transactional outbox building blocks",
            "JSON Pointer filters, ordered cursor pagination and handle-local equality indexes",
            "Exclusive database file lock",
            "Verified full-history backups and validated no-overwrite restoration",
            "Atomic conditional batches spanning inventory, orders and outbox",
            "Durable local jobs with priorities, leases, retries and fenced atomic acknowledgments",
            "Revision-fenced agent checkpoints with atomic document and outbox writes",
            "Bounded text, exact vector and hybrid memory retrieval with metadata prefilters"
        ],
        "limitations": [
            format!("Experimental v{}: not production validated", env!("CARGO_PKG_VERSION")),
            "One open database handle/process and one active write transaction",
            "Committed data is held in memory; no log compaction or retention",
            "Equality indexes are recreated per handle and use additional memory",
            "Changefeed keeps an eight-byte offset per committed transaction in memory",
            "Keyset pagination is not a snapshot across separate calls",
            "Outbox consumers need idempotency; no exactly-once external side effects",
            "No SQL, MVCC, columnar storage, replication or C ABI yet",
            "Aggregates use f64 and can lose integer precision above 2^53",
            "Exact retrieval scans and sorts candidates; no ANN index or model inference",
            "Agent checkpoints store application state; no workflow execution or authorization",
            "Durability depends on operating system, filesystem and storage sync semantics"
        ]
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    drop(database);
    Ok(())
}
