use cedrodb::{Database, Filter, FilterOp, Query};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cedrodb-audit-{}-{}",
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

fn equality_query(value: Value) -> Query {
    Query {
        filters: vec![Filter {
            field: "/payload/n".into(),
            op: FilterOp::Eq,
            value,
        }],
        limit: 3,
        after: None,
    }
}

#[test]
fn index_planning_cannot_change_whether_a_query_succeeds() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.0).unwrap();
    database
        .put("records", "a", json!({"n": u64::MAX, "group": "other"}))
        .unwrap();
    database
        .put("records", "b", json!({"n": 1, "group": "wanted"}))
        .unwrap();
    let query = Query {
        filters: vec![
            Filter {
                field: "n".into(),
                op: FilterOp::Gt,
                value: json!(0.5),
            },
            Filter {
                field: "group".into(),
                op: FilterOp::Eq,
                value: json!("wanted"),
            },
        ],
        ..Query::default()
    };
    let without_index = database.query("records", query.clone());
    database.create_index("records", "group").unwrap();
    let with_index = database.query("records", query);
    assert_eq!(
        without_index.is_ok(),
        with_index.is_ok(),
        "an optimization must preserve query success/error semantics: scan={without_index:?}, indexed={with_index:?}"
    );
    if let (Ok(without_index), Ok(with_index)) = (without_index, with_index) {
        assert_eq!(without_index.records, with_index.records);
    }
}

#[test]
fn indexes_and_plain_scans_agree_through_repeated_mutations_and_pagination() {
    let plain_directory = TestDirectory::new();
    let indexed_directory = TestDirectory::new();
    let plain = Database::open(&plain_directory.0).unwrap();
    let indexed = Database::open(&indexed_directory.0).unwrap();
    indexed.create_index("records", "/payload/n").unwrap();
    let numbers = [
        json!(0),
        json!(-0.0),
        json!(1),
        json!(1.0),
        json!(1.5),
        json!(-2),
        json!(9_007_199_254_740_992u64),
        json!(9_007_199_254_740_993u64),
        json!(u64::MAX),
        Value::Null,
        json!([1.0, {"a": 0}]),
        json!([1, {"a": -0.0}]),
    ];
    for step in 0..48 {
        let key = format!("{:02}", (step * 7) % 13);
        if step % 7 == 0 {
            plain.delete("records", &key).unwrap();
            indexed.delete("records", &key).unwrap();
        } else {
            let document = if step % 5 == 0 {
                json!({"missing": true})
            } else {
                json!({"payload": {"n": numbers[step % numbers.len()].clone()}})
            };
            plain.put("records", &key, document.clone()).unwrap();
            indexed.put("records", &key, document).unwrap();
        }
        for number in &numbers {
            let mut query = equality_query(number.clone());
            loop {
                let expected = plain.query("records", query.clone()).unwrap();
                let actual = indexed.query("records", query.clone()).unwrap();
                assert_eq!(
                    actual.records, expected.records,
                    "step {step}, filter {number}"
                );
                assert_eq!(actual.next_after, expected.next_after);
                query.after = expected.next_after;
                if query.after.is_none() {
                    break;
                }
            }
        }
    }
}
