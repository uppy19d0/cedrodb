use cedrodb::{AgentStatus, CheckpointOptions, Database, SearchQuery};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cedrodb-float-roundtrip-{}-{}",
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

fn assert_float_bits(values: &Value, expected: &[f64]) {
    let actual: Vec<_> = values
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_f64().unwrap().to_bits())
        .collect();
    let expected: Vec<_> = expected.iter().map(|value| value.to_bits()).collect();
    assert_eq!(actual, expected, "persisted IEEE-754 bits changed");
}

#[test]
fn ordinary_values_and_portable_checkpoint_floats_keep_exact_bits_after_recovery_and_restore() {
    let directory = TestDirectory::new();
    let database_path = directory.0.join("source");
    let database = Database::open(&database_path).unwrap();
    // The first two values previously lost one ULP with serde_json's default
    // parser. Include signed zero and the smallest subnormal as separate cases.
    let portable = [
        0.19450526354013234,
        -1.1270445130675529e-20,
        1.2345678901234567,
        f64::from_bits(1),
        -0.0,
    ];
    let ordinary = [
        -5.256595936985015e64,
        1.0e308,
        -1.0e308,
        f64::from_bits(1),
        0.19450526354013234,
        -0.0,
    ];
    database.put("values", "numbers", json!(ordinary)).unwrap();
    let original = database
        .agent_checkpoint(
            "roundtrip",
            "thread",
            json!({"numbers":portable}),
            CheckpointOptions {
                status: AgentStatus::Waiting,
                metadata: json!({"fingerprint":portable[0]}),
                ..CheckpointOptions::default()
            },
        )
        .unwrap()
        .checkpoint
        .unwrap();
    let check = |database: &Database| {
        assert_float_bits(
            &database.get("values", "numbers").unwrap().unwrap(),
            &ordinary,
        );
        let checkpoint = database.agent_get("roundtrip", "thread").unwrap().unwrap();
        assert_eq!(checkpoint.revision, original.revision);
        assert_eq!(checkpoint.status, AgentStatus::Waiting);
        assert_float_bits(&checkpoint.state["numbers"], &portable);
        assert_eq!(
            checkpoint.metadata["fingerprint"]
                .as_f64()
                .unwrap()
                .to_bits(),
            portable[0].to_bits()
        );
    };
    check(&database);
    let backup = directory.0.join("float.cedrobak");
    database.backup(&backup).unwrap();
    drop(database);
    let reopened = Database::open(database_path).unwrap();
    check(&reopened);
    let restored_path = directory.0.join("restored");
    Database::restore_backup(&backup, &restored_path).unwrap();
    let restored = Database::open(restored_path).unwrap();
    check(&restored);
}

#[test]
fn vector_threshold_membership_and_score_bits_survive_recovery_and_backup_restore() {
    let directory = TestDirectory::new();
    let source = directory.0.join("source");
    let database = Database::open(&source).unwrap();
    let coordinate = 0.19450526354013234;
    database
        .put("memory", "match", json!({"embedding":[coordinate,1.0]}))
        .unwrap();
    let query = SearchQuery {
        vector: Some(vec![coordinate, 1.0]),
        min_vector_score: Some(0.9999999999999998),
        ..SearchQuery::default()
    };
    let original = database.search("memory", query.clone()).unwrap();
    assert_eq!(original.matched, 1);
    assert_eq!(original.hits.len(), 1);
    let check = |database: &Database| {
        let recovered = database.search("memory", query.clone()).unwrap();
        assert_eq!(recovered.matched, original.matched);
        assert_eq!(recovered.hits.len(), 1);
        assert_eq!(recovered.hits[0].key, "match");
        assert_eq!(
            recovered.hits[0].score.to_bits(),
            original.hits[0].score.to_bits()
        );
        assert_eq!(
            recovered.hits[0].vector_score.unwrap().to_bits(),
            original.hits[0].vector_score.unwrap().to_bits()
        );
        assert_float_bits(&recovered.hits[0].value["embedding"], &[coordinate, 1.0]);
    };
    let backup = directory.0.join("vector.cedrobak");
    database.backup(&backup).unwrap();
    drop(database);
    let reopened = Database::open(source).unwrap();
    check(&reopened);
    let restored_path = directory.0.join("restored");
    Database::restore_backup(&backup, &restored_path).unwrap();
    let restored = Database::open(restored_path).unwrap();
    check(&restored);
}
