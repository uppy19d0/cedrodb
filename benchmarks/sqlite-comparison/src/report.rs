//! Environment capture and a Markdown summary of the merged report.

use std::process::Command;

use serde_json::{json, Value};

fn command(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unavailable".into())
}

fn cpu() -> String {
    if cfg!(target_os = "macos") {
        command("sysctl", &["-n", "machdep.cpu.brand_string"])
    } else {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|text| text.lines().find(|l| l.starts_with("model name")).map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string()))
            .unwrap_or_else(|| "unavailable".into())
    }
}

fn memory_bytes() -> String {
    if cfg!(target_os = "macos") { command("sysctl", &["-n", "hw.memsize"]) } else { command("sh", &["-c", "grep MemTotal /proc/meminfo | tr -s ' ' | cut -d' ' -f2"]) }
}

fn cedrodb_version() -> &'static str {
    include_str!("../../../Cargo.toml")
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or("unknown")
}

pub fn environment() -> Value {
    json!({
        "cedrodb_version": cedrodb_version(),
        "sqlite_version": rusqlite::version(),
        "rustc": command("rustc", &["--version"]),
        "build": if cfg!(debug_assertions) { "debug (NOT valid for comparisons)" } else { "release" },
        "os": command("uname", &["-srm"]),
        "cpu": cpu(),
        "memory_raw": memory_bytes(),
        "note": "Record the disk model and file system separately; cache state is not controlled.",
    })
}

fn number(value: &Value, path: &[&str]) -> Option<f64> {
    path.iter().try_fold(value, |v, key| v.get(*key)).and_then(Value::as_f64)
}

fn cell(value: Option<f64>, scale: f64, decimals: usize) -> String {
    value.map_or("–".into(), |v| format!("{:.*}", decimals, v / scale))
}

pub fn markdown(document: &Value) -> String {
    let engines = document["engines"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    let env = &document["environment"];
    out.push_str(&format!(
        "CedroDB {} vs SQLite {} · {} · {} · {}\nrecords={} iterations={} seed={}\n\n",
        env["cedrodb_version"].as_str().unwrap_or("?"), env["sqlite_version"].as_str().unwrap_or("?"),
        env["build"].as_str().unwrap_or("?"), env["os"].as_str().unwrap_or("?"), env["cpu"].as_str().unwrap_or("?"),
        document["parameters"]["records"], document["parameters"]["iterations"], document["parameters"]["seed"]
    ));
    let rows: Vec<(&str, Box<dyn Fn(&Value) -> String>)> = vec![
        ("verified", Box::new(|e| e["verified"].to_string())),
        ("insert docs/s · 1 per commit", Box::new(|e| cell(number(&e["inserts"][0], &["documents_per_second"]), 1.0, 0))),
        ("insert docs/s · 100 per commit", Box::new(|e| cell(number(&e["inserts"][1], &["documents_per_second"]), 1.0, 0))),
        ("insert docs/s · 1000 per commit", Box::new(|e| cell(number(&e["inserts"][2], &["documents_per_second"]), 1.0, 0))),
        ("commit p50 ms · 1 per commit", Box::new(|e| cell(number(&e["inserts"][0], &["commit_latency", "p50_us"]), 1000.0, 2))),
        ("commit p99 ms · 1 per commit", Box::new(|e| cell(number(&e["inserts"][0], &["commit_latency", "p99_us"]), 1000.0, 2))),
        ("get hit p50 µs", Box::new(|e| cell(number(e, &["get_hit", "p50_us"]), 1.0, 1))),
        ("get hit p99 µs", Box::new(|e| cell(number(e, &["get_hit", "p99_us"]), 1.0, 1))),
        ("get miss p50 µs", Box::new(|e| cell(number(e, &["get_miss", "p50_us"]), 1.0, 1))),
        ("aggregate p50 ms", Box::new(|e| cell(number(e, &["aggregate", "p50_us"]), 1000.0, 2))),
        ("update commit p50 ms (100 docs)", Box::new(|e| cell(number(e, &["update_10_percent", "commit_latency", "p50_us"]), 1000.0, 2))),
        ("delete commit p50 ms (100 docs)", Box::new(|e| cell(number(e, &["delete_10_percent", "commit_latency", "p50_us"]), 1000.0, 2))),
        ("reopen ms", Box::new(|e| cell(number(e, &["reopen_ms"]), 1.0, 1))),
        ("disk MiB after load", Box::new(|e| cell(number(e, &["disk_bytes_after_load"]), 1_048_576.0, 2))),
        ("disk MiB after changes", Box::new(|e| cell(number(e, &["disk_bytes_after_changes"]), 1_048_576.0, 2))),
        ("peak RSS MiB", Box::new(|e| cell(number(e, &["peak_rss_kib"]), 1024.0, 1))),
    ];
    out.push_str("| metric |");
    for engine in &engines {
        out.push_str(&format!(" {} |", engine["engine"].as_str().unwrap_or("?")));
    }
    out.push_str("\n| --- |");
    for _ in &engines {
        out.push_str(" ---: |");
    }
    out.push('\n');
    for (label, render) in rows {
        out.push_str(&format!("| {label} |"));
        for engine in &engines {
            out.push_str(&format!(" {} |", render(engine)));
        }
        out.push('\n');
    }
    out.push_str("\nDurability contracts:\n");
    for engine in &engines {
        out.push_str(&format!("- {}: {}\n", engine["engine"].as_str().unwrap_or("?"), engine["durability"].as_str().unwrap_or("?")));
    }
    out.push_str("\nsqlite-wal-normal is NOT durability-equivalent to CedroDB; compare CedroDB with the two *-full rows.\n");
    out
}
