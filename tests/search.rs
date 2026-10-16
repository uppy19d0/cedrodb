use cedrodb::{Database, Error, Filter, FilterOp, SearchMode, SearchQuery};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cedrodb-search-{}-{}",
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

fn vector(values: &[f64]) -> SearchQuery {
    SearchQuery {
        vector: Some(values.to_vec()),
        ..Default::default()
    }
}
fn text(value: &str) -> SearchQuery {
    SearchQuery {
        text: Some(value.into()),
        ..Default::default()
    }
}
fn filter(field: &str, value: Value) -> Filter {
    Filter {
        field: field.into(),
        op: FilterOp::Eq,
        value,
    }
}

#[test]
fn cosine_ranks_all_candidates_with_deterministic_ties_and_negative_scores() {
    let directory = Directory::new();
    let db = Database::open(&directory.0).unwrap();
    for (key, embedding) in [
        ("negative", json!([-1, 0])),
        ("orthogonal", json!([0, 1])),
        ("b", json!([2, 0])),
        ("a", json!([1, 0])),
    ] {
        db.put("memory", key, json!({"embedding":embedding}))
            .unwrap();
    }
    db.put("memory", "missing", json!({"text":"not embedded"}))
        .unwrap();
    let result = db.search("memory", vector(&[1.0, 0.0])).unwrap();
    assert_eq!(result.mode, SearchMode::Vector);
    assert_eq!(result.scanned, 5);
    assert_eq!(result.matched, 4);
    assert_eq!(
        result
            .hits
            .iter()
            .map(|hit| hit.key.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "orthogonal", "negative"]
    );
    assert_eq!(
        result.hits.iter().map(|hit| hit.score).collect::<Vec<_>>(),
        vec![1.0, 1.0, 0.0, -1.0]
    );
    assert!(result.hits.iter().all(|hit| hit.text_score.is_none()));
    let threshold = db
        .search(
            "memory",
            SearchQuery {
                min_vector_score: Some(0.5),
                ..vector(&[1.0, 0.0])
            },
        )
        .unwrap();
    assert_eq!(threshold.matched, 2);
}

#[test]
fn normalization_handles_extreme_finite_magnitudes_without_overflow() {
    let directory = Directory::new();
    let db = Database::open(&directory.0).unwrap();
    db.put("memory", "large", json!({"embedding":[1e308,1e308]}))
        .unwrap();
    db.put("memory", "small", json!({"embedding":[1e-308,1e-308]}))
        .unwrap();
    let result = db.search("memory", vector(&[1e308, 1e308])).unwrap();
    assert_eq!(result.hits.len(), 2);
    assert!(result
        .hits
        .iter()
        .all(|hit| hit.score.is_finite() && (hit.score - 1.0).abs() < 1e-14));
}

#[test]
fn text_ranking_matches_bm25_reference_and_ignores_duplicate_query_terms() {
    let directory = Directory::new();
    let db = Database::open(&directory.0).unwrap();
    db.put("memory", "a", json!({"text":"apple apple banana"}))
        .unwrap();
    db.put("memory", "b", json!({"text":"apple banana carrot"}))
        .unwrap();
    db.put("memory", "c", json!({"text":"banana carrot carrot"}))
        .unwrap();
    let result = db.search("memory", text("APPLE apple")).unwrap();
    assert_eq!(result.mode, SearchMode::Text);
    assert_eq!(result.matched, 2);
    assert_eq!(result.hits[0].key, "a");
    let idf = (1.0f64 + (3.0 - 2.0 + 0.5) / (2.0 + 0.5)).ln();
    assert!((result.hits[0].score - idf * 2.0 * 2.2 / 3.2).abs() < 1e-14);
    assert!((result.hits[1].score - idf).abs() < 1e-14);
    assert_eq!(result, db.search("memory", text("apple")).unwrap());
}

#[test]
fn unicode_fields_and_json_pointers_are_searchable_without_accent_folding() {
    let directory = Directory::new();
    let db = Database::open(&directory.0).unwrap();
    db.put(
        "memory",
        "a",
        json!({"content":{"title":"CAFÉ seguro"},"summary":"café local"}),
    )
    .unwrap();
    db.put("memory", "b", json!({"content":{"title":"cafe seguro"}}))
        .unwrap();
    let result = db
        .search(
            "memory",
            SearchQuery {
                text_fields: vec!["/content/title".into(), "summary".into()],
                ..text("café")
            },
        )
        .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].key, "a");
}

#[test]
fn hybrid_combines_channels_using_full_rankings_before_top_k() {
    let directory = Directory::new();
    let db = Database::open(&directory.0).unwrap();
    db.put(
        "memory",
        "a",
        json!({"text":"refund refund", "embedding":[0,1]}),
    )
    .unwrap();
    db.put(
        "memory",
        "b",
        json!({"text":"shipping delays", "embedding":[1,0]}),
    )
    .unwrap();
    db.put(
        "memory",
        "c",
        json!({"text":"refund policy", "embedding":[1,1]}),
    )
    .unwrap();
    let query = SearchQuery {
        text: Some("refund".into()),
        ..vector(&[1.0, 0.0])
    };
    let all = db.search("memory", query.clone()).unwrap();
    assert_eq!(all.mode, SearchMode::Hybrid);
    assert_eq!(all.matched, 3);
    assert_eq!(
        all.hits
            .iter()
            .map(|hit| hit.key.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "c", "b"]
    );
    assert!((all.hits[0].score - (1.0 / 61.0 + 1.0 / 63.0)).abs() < 1e-14);
    assert!((all.hits[1].score - 2.0 / 62.0).abs() < 1e-14);
    let one = db
        .search("memory", SearchQuery { limit: 1, ..query })
        .unwrap();
    assert_eq!(one.hits, all.hits[..1]);
    assert_eq!(one.matched, all.matched);
}

#[test]
fn hybrid_can_return_text_only_documents_and_vector_only_documents() {
    let directory = Directory::new();
    let db = Database::open(&directory.0).unwrap();
    db.put("memory", "text", json!({"text":"invoice"})).unwrap();
    db.put("memory", "vector", json!({"embedding":[1,0]}))
        .unwrap();
    let result = db
        .search(
            "memory",
            SearchQuery {
                text: Some("invoice".into()),
                ..vector(&[1.0, 0.0])
            },
        )
        .unwrap();
    assert_eq!(result.matched, 2);
    assert_eq!(result.hits[0].key, "text");
    assert_eq!(result.hits[1].key, "vector");
    assert_eq!(result.hits[0].score, result.hits[1].score);
}

#[test]
fn metadata_prefilters_exclude_malformed_other_models_before_vector_validation() {
    let directory = Directory::new();
    let db = Database::open(&directory.0).unwrap();
    db.put(
        "memory",
        "a",
        json!({"model":"v1","embedding":[1,0],"text":"invoice"}),
    )
    .unwrap();
    db.put("memory", "b", json!({"model":"other","embedding":[0,0,0]}))
        .unwrap();
    db.put(
        "memory",
        "c",
        json!({"model":"other","embedding":"malformed"}),
    )
    .unwrap();
    let query = SearchQuery {
        filters: vec![filter("model", json!("v1"))],
        ..vector(&[1.0, 0.0])
    };
    let plain = db.search("memory", query.clone()).unwrap();
    db.create_index("memory", "model").unwrap();
    let indexed = db.search("memory", query).unwrap();
    assert_eq!(plain.hits, indexed.hits);
    assert_eq!(indexed.scanned, 1);
    assert_eq!(indexed.used_index.as_deref(), Some("model"));
    assert!(db.search("memory", vector(&[1.0, 0.0])).is_err());
}

#[test]
fn scan_budget_errors_instead_of_silently_returning_a_partial_top_k() {
    let directory = Directory::new();
    let db = Database::open(&directory.0).unwrap();
    for (key, tenant) in [("a", "alice"), ("b", "alice"), ("c", "bob")] {
        db.put("memory", key, json!({"tenant":tenant,"embedding":[1,0]}))
            .unwrap();
    }
    let query = SearchQuery {
        max_scanned: 2,
        limit: 1,
        filters: vec![filter("tenant", json!("alice"))],
        ..vector(&[1.0, 0.0])
    };
    assert!(matches!(
        db.search("memory", query.clone()),
        Err(Error::LimitExceeded(_))
    ));
    db.create_index("memory", "tenant").unwrap();
    let result = db.search("memory", query).unwrap();
    assert_eq!(result.matched, 2);
    assert_eq!(result.scanned, 2);
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].key, "a");
}

#[test]
fn malformed_eligible_vectors_fail_even_when_limit_is_already_filled() {
    let directory = Directory::new();
    let db = Database::open(&directory.0).unwrap();
    db.put("memory", "a", json!({"embedding":[1,0]})).unwrap();
    for invalid in [
        json!([0, 0]),
        json!([1]),
        json!([true, 0]),
        json!(["1", 0]),
        Value::Null,
    ] {
        db.put("memory", "z", json!({"embedding":invalid})).unwrap();
        assert!(matches!(
            db.search(
                "memory",
                SearchQuery {
                    limit: 1,
                    ..vector(&[1.0, 0.0])
                }
            ),
            Err(Error::InvalidInput(_))
        ));
    }
}

#[test]
fn invalid_search_inputs_are_rejected_even_for_missing_collections() {
    let directory = Directory::new();
    let db = Database::open(&directory.0).unwrap();
    let mut invalid = vec![
        SearchQuery::default(),
        text(""),
        text("---"),
        text(&"a".repeat(4097)),
        SearchQuery {
            limit: 0,
            ..text("ok")
        },
        SearchQuery {
            limit: 1001,
            ..text("ok")
        },
        SearchQuery {
            max_scanned: 0,
            ..text("ok")
        },
        SearchQuery {
            max_scanned: 100001,
            ..text("ok")
        },
        SearchQuery {
            text_fields: vec![],
            ..text("ok")
        },
        SearchQuery {
            text_fields: vec!["text".into(), "text".into()],
            ..text("ok")
        },
        SearchQuery {
            text_fields: vec!["/bad~pointer".into()],
            ..text("ok")
        },
        SearchQuery {
            min_vector_score: Some(0.5),
            ..text("ok")
        },
        SearchQuery {
            min_vector_score: Some(1.1),
            ..vector(&[1.0])
        },
    ];
    for values in [
        vec![],
        vec![0.0, 0.0],
        vec![f64::NAN],
        vec![f64::INFINITY],
        vec![1.0; 4097],
    ] {
        invalid.push(vector(&values));
    }
    for query in invalid {
        assert!(db.search("missing", query.clone()).is_err(), "{query:?}");
    }
}

#[test]
fn retrieval_is_read_only_and_ignores_uncommitted_writes() {
    let directory = Directory::new();
    let db = Database::open(&directory.0).unwrap();
    db.put("memory", "old", json!({"text":"invoice"})).unwrap();
    let before = db.stats().unwrap();
    let mut writer = db.begin_write().unwrap();
    writer
        .put("memory", "new", json!({"text":"invoice"}))
        .unwrap();
    let result = db.search("memory", text("invoice")).unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].key, "old");
    drop(writer);
    assert_eq!(db.stats().unwrap(), before);
}

#[test]
fn retrieval_survives_reopen_and_backup_restoration_without_extra_index_files() {
    let directory = Directory::new();
    let path = directory.0.join("database");
    let db = Database::open(&path).unwrap();
    db.put(
        "memory",
        "note",
        json!({"text":"invoice policy","embedding":[1,0]}),
    )
    .unwrap();
    let query = SearchQuery {
        text: Some("invoice".into()),
        ..vector(&[1.0, 0.0])
    };
    let expected = db.search("memory", query.clone()).unwrap();
    let backup = directory.0.join("memory.cedrobak");
    db.backup(&backup).unwrap();
    drop(db);
    let reopened = Database::open(&path).unwrap();
    assert_eq!(reopened.search("memory", query.clone()).unwrap(), expected);
    let restored = directory.0.join("restored");
    Database::restore_backup(&backup, &restored).unwrap();
    let restored = Database::open(restored).unwrap();
    assert_eq!(restored.search("memory", query).unwrap(), expected);
}
