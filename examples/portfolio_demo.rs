use cedrodb::{Database, Filter, FilterOp, Query};
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
        "capabilities": [
            "Rust engine with its own append-only transaction log",
            "JSON documents, committed reads and ordered scans",
            "Atomic frame commits followed by sync_all",
            "CRC32 verification and incomplete-tail recovery",
            "Numeric count, sum, min and max",
            "Resumable durable changefeed with complete transaction boundaries",
            "Atomic compare-and-exchange and transactional outbox building blocks",
            "JSON Pointer filters, ordered cursor pagination and handle-local equality indexes",
            "Exclusive database file lock"
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
            "Durability depends on operating system, filesystem and storage sync semantics"
        ]
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    drop(database);
    Ok(())
}
