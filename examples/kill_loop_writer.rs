//! Helper process for `tests/kill_loop.rs`. Commits one document per
//! transaction forever and prints `ack N` only after `commit` returned Ok.
//! The test SIGKILLs it. Not intended for general use.
use cedrodb::Database;
use serde_json::json;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args()
        .nth(1)
        .ok_or("usage: kill_loop_writer DIR")?;
    let database = Database::open(directory)?;
    let stdout = std::io::stdout();
    for sequence in 0u64.. {
        let mut transaction = database.begin_write()?;
        transaction.put("kill", &format!("{sequence:08}"), json!({"n": sequence}))?;
        transaction.commit()?;
        let mut out = stdout.lock();
        writeln!(out, "ack {sequence}")?;
        out.flush()?;
    }
    Ok(())
}
