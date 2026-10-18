//! Deterministic workloads from docs/benchmarks.md: insert (1/100/1,000 per commit), reads by key,
//! aggregate, update 10%, delete 10%, reopen/recovery. Results are verified against an in-memory
//! model before any timing is reported.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::engines::{AggregateResult, Cedro, Engine, Sqlite};
use crate::Config;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn key(i: u64) -> String {
    format!("customer-{i:08}")
}

/// Scores are multiples of 0.25, so sums are exact in f64 regardless of summation order.
fn score(i: u64, revision: u64) -> f64 {
    ((i * 37 + revision * 11) % 1000) as f64 / 4.0
}

fn document(i: u64, revision: u64) -> Value {
    json!({
        "id": i,
        "name": format!("Customer {i}"),
        "group": i % 100,
        "score": score(i, revision),
        "active": i % 3 != 0,
        "notes": format!("generated record {i} revision {revision} with deterministic padding text for realistic size"),
    })
}

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e6
}

/// Nearest-rank percentiles over the recorded samples.
fn stats(mut samples: Vec<Duration>) -> Value {
    if samples.is_empty() {
        return json!({ "count": 0 });
    }
    samples.sort();
    let at = |q: f64| micros(samples[(((samples.len() as f64) * q).ceil() as usize).clamp(1, samples.len()) - 1]);
    let mean = samples.iter().map(|d| micros(*d)).sum::<f64>() / samples.len() as f64;
    json!({ "count": samples.len(), "p50_us": at(0.50), "p95_us": at(0.95), "p99_us": at(0.99), "max_us": micros(*samples.last().unwrap()), "mean_us": mean })
}

pub fn peak_rss_kib() -> u64 {
    // ru_maxrss is bytes on macOS and KiB on Linux.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let raw = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") { raw / 1024 } else { raw }
}

fn make_engine(name: &str) -> Box<dyn Engine> {
    match name {
        "cedrodb" => Box::new(Cedro::new()),
        "sqlite-wal-full" => Box::new(Sqlite::new("sqlite-wal-full", "WAL", "FULL")),
        "sqlite-delete-full" => Box::new(Sqlite::new("sqlite-delete-full", "DELETE", "FULL")),
        "sqlite-wal-normal" => Box::new(Sqlite::new("sqlite-wal-normal", "WAL", "NORMAL")),
        other => panic!("unknown engine {other}"),
    }
}

/// Live-document model used to verify every engine independently of the other engines.
struct Model {
    revisions: Vec<Option<u64>>,
}

impl Model {
    fn expected(&self) -> AggregateResult {
        let (mut count, mut sum, mut min, mut max) = (0u64, 0.0f64, f64::INFINITY, f64::NEG_INFINITY);
        for (i, revision) in self.revisions.iter().enumerate() {
            if let Some(revision) = revision {
                let value = score(i as u64, *revision);
                count += 1;
                sum += value;
                min = min.min(value);
                max = max.max(value);
            }
        }
        AggregateResult { count, sum, min, max }
    }
}

fn aggregate_matches(actual: &AggregateResult, expected: &AggregateResult) -> bool {
    actual.count == expected.count && actual.sum == expected.sum && actual.min == expected.min && actual.max == expected.max
}

/// Generates each batch just before its commit (outside the timed region) and drops it afterwards, so the
/// harness never holds the whole dataset. Both engines go through this same path.
fn commit_in_batches(engine: &mut dyn Engine, documents: u64, batch: usize) -> (Duration, Vec<Duration>) {
    let mut latencies = Vec::new();
    let mut total = Duration::ZERO;
    let mut start = 0u64;
    while start < documents {
        let end = (start + batch as u64).min(documents);
        let items: Vec<(String, Value)> = (start..end).map(|i| (key(i), document(i, 0))).collect();
        let before = Instant::now();
        engine.commit_puts(items);
        let elapsed = before.elapsed();
        total += elapsed;
        latencies.push(elapsed);
        start = end;
    }
    (total, latencies)
}

pub fn run(name: &str, config: &Config) -> Value {
    let n = config.records;
    let mut rng = Rng(config.seed);
    let mut engine = make_engine(name);
    let root = config.directory.join(name);
    let _ = std::fs::remove_dir_all(&root);

    // --- Insert workloads. Batch 1 is capped because every commit waits for the disk. ---
    let mut inserts = Vec::new();
    let mut verified = true;
    let mut notes: Vec<String> = Vec::new();
    for batch in [1usize, 100, 1000] {
        let directory = root.join(format!("insert-{batch}"));
        engine.open(&directory);
        let documents = if batch == 1 { n.min(config.single_commit_limit) } else { n };
        let (total, latencies) = commit_in_batches(engine.as_mut(), documents, batch);
        inserts.push(json!({
            "documents_per_commit": batch, "documents": documents, "commits": latencies.len(),
            "total_ms": total.as_secs_f64() * 1e3, "documents_per_second": documents as f64 / total.as_secs_f64(),
            "commit_latency": stats(latencies),
        }));
        if batch != 1000 {
            engine.close();
        }
    }
    // The 1,000-per-commit dataset stays open for the remaining workloads.
    let mut model = Model { revisions: (0..n).map(|_| Some(0)).collect() };
    let loaded = engine.aggregate();
    if !aggregate_matches(&loaded, &model.expected()) {
        verified = false;
        notes.push("aggregate after load does not match the model".into());
    }
    for _ in 0..200 {
        let i = rng.below(n);
        if engine.get(&key(i)) != Some(document(i, 0)) {
            verified = false;
            notes.push(format!("stored document {i} differs from the generated one"));
            break;
        }
    }
    let disk_after_load = engine.disk_bytes();

    // --- Reads by key (warm, in-process; the OS page cache is not controlled). ---
    let reads = 5_000usize.min(n as usize * 2).max(1);
    let hit_keys: Vec<String> = (0..reads).map(|_| key(rng.below(n))).collect();
    let miss_keys: Vec<String> = (0..reads).map(|i| format!("absent-{i:08}")).collect();
    let mut hit_samples = Vec::with_capacity(reads);
    let mut hit_values = Vec::with_capacity(reads);
    for key in &hit_keys {
        let before = Instant::now();
        let value = engine.get(key);
        hit_samples.push(before.elapsed());
        hit_values.push(value);
    }
    let mut miss_samples = Vec::with_capacity(reads);
    let mut misses_found = 0;
    for key in &miss_keys {
        let before = Instant::now();
        if engine.get(key).is_some() {
            misses_found += 1;
        }
        miss_samples.push(before.elapsed());
    }
    if hit_values.iter().any(Option::is_none) || misses_found > 0 {
        verified = false;
        notes.push("read workload returned a wrong presence result".into());
    }

    // --- Aggregate over the whole collection. ---
    let mut aggregate_samples = Vec::new();
    for _ in 0..config.iterations {
        let before = Instant::now();
        let result = engine.aggregate();
        aggregate_samples.push(before.elapsed());
        if !aggregate_matches(&result, &model.expected()) {
            verified = false;
            notes.push("aggregate result differs from the model".into());
        }
    }

    // --- Update 10% then delete another 10%, 100 documents per commit. ---
    let mut order: Vec<u64> = (0..n).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, rng.below(i as u64 + 1) as usize);
    }
    let tenth = (n / 10) as usize;
    let (update_ids, rest) = order.split_at(tenth);
    let delete_ids = &rest[..tenth.min(rest.len())];
    let mut update_latencies = Vec::new();
    let update_started = Instant::now();
    for chunk in update_ids.chunks(100) {
        let items: Vec<(String, Value)> = chunk.iter().map(|&i| (key(i), document(i, 1))).collect();
        let before = Instant::now();
        engine.commit_puts(items);
        update_latencies.push(before.elapsed());
    }
    let update_total = update_started.elapsed();
    for &i in update_ids {
        model.revisions[i as usize] = Some(1);
    }
    let mut delete_latencies = Vec::new();
    let delete_started = Instant::now();
    for chunk in delete_ids.chunks(100) {
        let keys: Vec<String> = chunk.iter().map(|&i| key(i)).collect();
        let before = Instant::now();
        engine.commit_deletes(&keys);
        delete_latencies.push(before.elapsed());
    }
    let delete_total = delete_started.elapsed();
    for &i in delete_ids {
        model.revisions[i as usize] = None;
    }
    if !aggregate_matches(&engine.aggregate(), &model.expected()) {
        verified = false;
        notes.push("aggregate after update/delete does not match the model".into());
    }
    let disk_after_changes = engine.disk_bytes();

    // --- Reopen: CedroDB replays the whole log; SQLite opens the file (and recovers its WAL if needed). ---
    let directory = root.join("insert-1000");
    engine.close();
    let before = Instant::now();
    engine.open(&directory);
    let reopen = before.elapsed();
    let first_aggregate_started = Instant::now();
    let after_reopen = engine.aggregate();
    let first_aggregate = first_aggregate_started.elapsed();
    if !aggregate_matches(&after_reopen, &model.expected()) {
        verified = false;
        notes.push("state after reopening does not match the model".into());
    }
    engine.close();

    json!({
        "engine": engine.label(),
        "durability": engine.durability(),
        "verified": verified,
        "verification_notes": notes,
        "inserts": inserts,
        "get_hit": stats(hit_samples),
        "get_miss": stats(miss_samples),
        "aggregate": stats(aggregate_samples),
        "update_10_percent": { "documents": update_ids.len(), "documents_per_commit": 100, "total_ms": update_total.as_secs_f64() * 1e3, "commit_latency": stats(update_latencies) },
        "delete_10_percent": { "documents": delete_ids.len(), "documents_per_commit": 100, "total_ms": delete_total.as_secs_f64() * 1e3, "commit_latency": stats(delete_latencies) },
        "reopen_ms": reopen.as_secs_f64() * 1e3,
        "first_aggregate_after_reopen_ms": first_aggregate.as_secs_f64() * 1e3,
        "disk_bytes_after_load": disk_after_load,
        "disk_bytes_after_changes": disk_after_changes,
        "live_documents_at_end": model.expected().count,
        "peak_rss_kib": peak_rss_kib(),
    })
}
