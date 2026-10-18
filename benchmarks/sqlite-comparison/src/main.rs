//! Reproducible CedroDB vs SQLite comparison. Read `README.md` before quoting numbers.
//!
//! Each engine runs in its own child process so peak RSS is not contaminated by another
//! engine. Data come from a seeded generator and results are verified before timing is reported.

mod engines;
mod report;
mod workload;

use std::process::{Command, Stdio};

use serde_json::{json, Value};

const ENGINES: [&str; 4] = ["cedrodb", "sqlite-wal-full", "sqlite-delete-full", "sqlite-wal-normal"];

pub struct Config {
    pub records: u64,
    pub iterations: usize,
    pub seed: u64,
    pub single_commit_limit: u64,
    pub directory: std::path::PathBuf,
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = Config {
        records: arg(&args, "--records").map_or(10_000, |v| v.parse().expect("--records must be a number")),
        iterations: arg(&args, "--iterations").map_or(20, |v| v.parse().expect("--iterations must be a number")),
        seed: arg(&args, "--seed").map_or(42, |v| v.parse().expect("--seed must be a number")),
        single_commit_limit: arg(&args, "--single-commit-limit").map_or(1_000, |v| v.parse().expect("number")),
        directory: arg(&args, "--dir").map_or_else(|| std::env::temp_dir().join("cedrodb-sqlite-comparison"), Into::into),
    };

    if let Some(directory) = arg(&args, "--reopen") {
        // A/B helper: open an existing CedroDB directory once in a fresh process and report time and peak RSS.
        let started = std::time::Instant::now();
        let database = cedrodb::Database::open(&directory).expect("open existing database");
        let reopen_ms = started.elapsed().as_secs_f64() * 1e3;
        let documents = database.aggregate("docs", "score").map_or(0, |a| a.count);
        println!("{}", json!({ "reopen_ms": reopen_ms, "documents": documents, "peak_rss_kib": workload::peak_rss_kib() }));
        return;
    }

    if let Some(engine) = arg(&args, "--engine") {
        // Child mode: run one engine and print its result as JSON.
        let result = workload::run(&engine, &config);
        println!("{}", serde_json::to_string(&result).unwrap());
        return;
    }

    // Driver mode: run every engine in a fresh process, then merge the reports.
    let exe = std::env::current_exe().expect("current executable");
    let mut results = Vec::new();
    for engine in ENGINES {
        eprintln!("running {engine} …");
        let output = Command::new(&exe)
            .args(["--engine", engine, "--records", &config.records.to_string(), "--iterations", &config.iterations.to_string()])
            .args(["--seed", &config.seed.to_string(), "--single-commit-limit", &config.single_commit_limit.to_string()])
            .args(["--dir", config.directory.to_str().expect("utf-8 path")])
            .stderr(Stdio::inherit())
            .output()
            .expect("run child");
        assert!(output.status.success(), "{engine} failed");
        results.push(serde_json::from_slice::<Value>(&output.stdout).expect("child JSON"));
    }
    let document = json!({
        "environment": report::environment(),
        "parameters": { "records": config.records, "iterations": config.iterations, "seed": config.seed, "single_commit_limit": config.single_commit_limit },
        "engines": results,
    });
    println!("{}", serde_json::to_string_pretty(&document).unwrap());
    eprintln!("\n{}", report::markdown(&document));
}
