//! Format-compatibility fixtures and deterministic mutation robustness tests.
//!
//! Fixtures `tests/fixtures/v1-log.bin` (a `cedro.log`) and
//! `tests/fixtures/v1-backup.bin` were produced by `build_sources` below with
//! the format-version-1 writer. They carry a `.bin` suffix because `*.log` is
//! git-ignored. To regenerate (only for an intentional format change):
//! `CEDRODB_WRITE_FIXTURES=1 cargo test --test format_fixtures regenerate -- --ignored`
use cedrodb::{Database, IntegrityReport, Options};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "cedrodb-fixtures-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn join(&self, n: &str) -> PathBuf {
        self.0.join(n)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Builds the log and backup bytes with the current writer.
fn build_sources(dir: &Dir) -> (Vec<u8>, Vec<u8>) {
    let db = Database::open(dir.join("src")).unwrap();
    let mut tx = db.begin_write().unwrap();
    tx.put(
        "users",
        "ada",
        json!({"name":"Ada","age":36,"tags":["math","code"]}),
    )
    .unwrap();
    tx.put(
        "users",
        "uni",
        json!({"name":"José 🌲","ratio":1.5,"none":null}),
    )
    .unwrap();
    tx.put("users", "gone", json!(true)).unwrap();
    tx.commit().unwrap();
    db.put("notes", "n1", json!({"text":"hello","max":u64::MAX}))
        .unwrap();
    db.delete("users", "gone").unwrap();
    db.backup(dir.join("backup.bin")).unwrap();
    drop(db);
    (
        fs::read(dir.join("src").join("cedro.log")).unwrap(),
        fs::read(dir.join("backup.bin")).unwrap(),
    )
}

#[test]
#[ignore]
fn regenerate() {
    if std::env::var_os("CEDRODB_WRITE_FIXTURES").is_none() {
        return;
    }
    let dir = Dir::new();
    let (log, backup) = build_sources(&dir);
    fs::create_dir_all(fixture("")).unwrap();
    fs::write(fixture("v1-log.bin"), log).unwrap();
    fs::write(fixture("v1-backup.bin"), backup).unwrap();
}

fn check_contents(db: &Database) {
    assert_eq!(
        db.get("users", "ada").unwrap(),
        Some(json!({"name":"Ada","age":36,"tags":["math","code"]}))
    );
    assert_eq!(
        db.get("users", "uni").unwrap(),
        Some(json!({"name":"José 🌲","ratio":1.5,"none":null}))
    );
    assert_eq!(db.get("users", "gone").unwrap(), None);
    assert_eq!(
        db.get("notes", "n1").unwrap(),
        Some(json!({"text":"hello","max":u64::MAX}))
    );
}

fn check_report(r: &IntegrityReport, log_len: usize) {
    assert_eq!(r.log_bytes, log_len as u64);
    assert_eq!(r.transactions, 3);
}

#[test]
fn committed_fixture_log_opens_and_verifies() {
    let log = fs::read(fixture("v1-log.bin")).unwrap();
    let dir = Dir::new();
    fs::create_dir(dir.join("db")).unwrap();
    fs::write(dir.join("db").join("cedro.log"), &log).unwrap();
    let db = Database::open(dir.join("db")).unwrap();
    check_contents(&db);
    check_report(&db.verify().unwrap(), log.len());
    drop(db);
    assert_eq!(fs::read(dir.join("db").join("cedro.log")).unwrap(), log);
}

#[test]
fn committed_fixture_backup_restores_and_verifies() {
    let log = fs::read(fixture("v1-log.bin")).unwrap();
    let dir = Dir::new();
    let report = Database::restore_backup(fixture("v1-backup.bin"), dir.join("restored")).unwrap();
    check_report(&report, log.len());
    let db = Database::open(dir.join("restored")).unwrap();
    check_contents(&db);
    check_report(&db.verify().unwrap(), log.len());
    drop(db);
    assert_eq!(
        fs::read(dir.join("restored").join("cedro.log")).unwrap(),
        log
    );
}

#[test]
fn current_writer_reproduces_fixture_bytes() {
    let dir = Dir::new();
    let (log, backup) = build_sources(&dir);
    assert!(
        log == fs::read(fixture("v1-log.bin")).unwrap(),
        "log format changed: committed fixture no longer matches the writer"
    );
    assert!(
        backup == fs::read(fixture("v1-backup.bin")).unwrap(),
        "backup format changed: committed fixture no longer matches the writer"
    );
}

/// Small deterministic xorshift64* generator (no dependencies).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn mutate(rng: &mut Rng, original: &[u8]) -> Vec<u8> {
    let mut b = original.to_vec();
    match rng.below(4) {
        0 => {
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
            }
        }
        1 => b.truncate(rng.below(b.len() + 1)),
        2 => {
            for _ in 0..1 + rng.below(64) {
                b.push(rng.next() as u8);
            }
        }
        _ => {
            let i = rng.below(b.len());
            let n = (1 + rng.below(8)).min(b.len() - i);
            for x in &mut b[i..i + n] {
                *x = rng.next() as u8;
            }
        }
    }
    b
}

#[test]
fn mutated_log_and_backup_never_panic() {
    let log = fs::read(fixture("v1-log.bin")).unwrap();
    let backup = fs::read(fixture("v1-backup.bin")).unwrap();
    let dir = Dir::new();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut ok, mut err) = (0u32, 0u32);
    for round in 0..300 {
        let m = mutate(&mut rng, &log);
        let db_dir = dir.join(&format!("db{round}"));
        fs::create_dir(&db_dir).unwrap();
        fs::write(db_dir.join("cedro.log"), &m).unwrap();
        match Database::open_with_options(&db_dir, Options::default()) {
            Ok(db) => {
                let _ = db.verify();
                let _ = db.scan("users");
                let _ = db.changes(0, 100);
                ok += 1;
            }
            Err(_) => err += 1,
        }
        let m = mutate(&mut rng, &backup);
        let bpath = dir.join(&format!("b{round}.bin"));
        fs::write(&bpath, &m).unwrap();
        let dest = dir.join(&format!("r{round}"));
        match Database::restore_backup(&bpath, &dest) {
            Ok(_) => {
                let db = Database::open(&dest).unwrap();
                db.verify().unwrap();
                ok += 1;
            }
            Err(_) => {
                assert!(!dest.exists(), "failed restore left a destination");
                err += 1;
            }
        }
    }
    // Sanity: mutations must exercise both outcomes.
    assert!(ok > 0 && err > 0, "ok={ok} err={err}");
}
