//! Process-kill durability test (unix only). Proves only: after SIGKILL of a
//! writer process at varying points, the log reopens and verifies, the
//! recovered documents are a contiguous prefix 0..n of the commit sequence,
//! every commit acknowledged before the kill is present, and at most one
//! unacknowledged in-flight commit is present. It does NOT prove power-loss
//! durability (SIGKILL leaves the OS page cache intact) or behavior under I/O
//! faults.
#![cfg(unix)]

use cedrodb::Database;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn writer_binary() -> PathBuf {
    // tests run from target/<profile>/deps; examples live in ../examples.
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.push("examples");
    path.push("kill_loop_writer");
    if !path.exists() {
        // `cargo test --test kill_loop` does not build examples; build on demand.
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "--locked", "--example", "kill_loop_writer"])
            .status()
            .unwrap();
        assert!(status.success());
    }
    assert!(path.exists(), "example binary missing: {}", path.display());
    path
}

#[test]
fn acknowledged_commits_survive_sigkill_at_varying_points() {
    let root = std::env::temp_dir().join(format!("cedrodb-killloop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir(&root).unwrap();
    // Deterministic, varied kill points (acks observed before killing).
    let targets = [0u64, 1, 2, 5, 13, 40, 7, 25];
    for (round, target) in targets.iter().enumerate() {
        // Fresh directory per round so the prefix check is exact.
        let dir = root.join(format!("round-{round}"));
        std::fs::create_dir(&dir).unwrap();
        let mut child = Command::new(writer_binary())
            .arg(&dir)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        // Highest acknowledged sequence in this round, if any.
        let mut last_ack: Option<u64> = None;
        if *target > 0 {
            for line in lines.by_ref() {
                let line = line.unwrap();
                let n: u64 = line.strip_prefix("ack ").unwrap().parse().unwrap();
                last_ack = Some(n);
                if n + 1 >= *target {
                    break;
                }
            }
        }
        child.kill().unwrap(); // SIGKILL
        child.wait().unwrap();
        // Acks printed before the kill but not yet read are still acknowledged.
        for line in lines.map_while(Result::ok) {
            if let Some(n) = line.strip_prefix("ack ").and_then(|s| s.parse().ok()) {
                last_ack = Some(n);
            }
        }

        let database = Database::open(&dir).expect("reopen after SIGKILL");
        database.verify().expect("log verifies after SIGKILL");
        let keys: Vec<String> = database
            .scan("kill")
            .unwrap()
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        // Keys are zero-padded, so scan order is numeric. Must be a prefix 0..n.
        for (index, key) in keys.iter().enumerate() {
            assert_eq!(key, &format!("{index:08}"), "round {round}: gap or reorder");
        }
        let present = keys.len() as u64;
        let acked = last_ack.map_or(0, |n| n + 1);
        assert!(
            present >= acked,
            "round {round}: {acked} acked but {present} present"
        );
        assert!(
            present <= acked + 1,
            "round {round}: {present} present, {acked} acked"
        );
        drop(database);
    }
    let _ = std::fs::remove_dir_all(&root);
}
