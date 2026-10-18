//! Engine adapters. Both store the same JSON documents under string keys and return `serde_json::Value`,
//! so decoding cost is part of every read for both engines.

use std::fs;
use std::path::{Path, PathBuf};

use cedrodb::Database;
use rusqlite::{params, Connection};
use serde_json::Value;

pub struct AggregateResult {
    pub count: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
}

pub trait Engine {
    fn label(&self) -> String;
    /// Human-readable durability contract, copied into the report.
    fn durability(&self) -> String;
    fn open(&mut self, directory: &Path);
    fn close(&mut self);
    fn commit_puts(&mut self, items: Vec<(String, Value)>);
    fn commit_deletes(&mut self, keys: &[String]);
    fn get(&self, key: &str) -> Option<Value>;
    fn aggregate(&self) -> AggregateResult;
    fn disk_bytes(&self) -> u64;
}

pub fn directory_bytes(path: &Path) -> u64 {
    fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| {
                    let meta = entry.metadata().ok();
                    match meta {
                        Some(meta) if meta.is_dir() => directory_bytes(&entry.path()),
                        Some(meta) => meta.len(),
                        None => 0,
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

pub struct Cedro {
    database: Option<Database>,
    directory: PathBuf,
}

impl Cedro {
    pub fn new() -> Self {
        Self { database: None, directory: PathBuf::new() }
    }
    fn db(&self) -> &Database {
        self.database.as_ref().expect("database is open")
    }
}

impl Engine for Cedro {
    fn label(&self) -> String {
        "cedrodb".into()
    }
    fn durability(&self) -> String {
        "append-only log; every commit calls File::sync_all (fsync, F_FULLFSYNC on macOS); cannot be disabled".into()
    }
    fn open(&mut self, directory: &Path) {
        self.directory = directory.to_path_buf();
        self.database = Some(Database::open(directory).expect("open CedroDB"));
    }
    fn close(&mut self) {
        self.database = None;
    }
    fn commit_puts(&mut self, items: Vec<(String, Value)>) {
        let mut tx = self.db().begin_write().expect("begin");
        for (key, value) in items {
            tx.put("docs", &key, value).expect("put");
        }
        tx.commit().expect("commit");
    }
    fn commit_deletes(&mut self, keys: &[String]) {
        let mut tx = self.db().begin_write().expect("begin");
        for key in keys {
            tx.delete("docs", key).expect("delete");
        }
        tx.commit().expect("commit");
    }
    fn get(&self, key: &str) -> Option<Value> {
        self.db().get("docs", key).expect("get")
    }
    fn aggregate(&self) -> AggregateResult {
        let a = self.db().aggregate("docs", "score").expect("aggregate");
        AggregateResult { count: a.count, sum: a.sum, min: a.min.unwrap_or(f64::NAN), max: a.max.unwrap_or(f64::NAN) }
    }
    fn disk_bytes(&self) -> u64 {
        directory_bytes(&self.directory)
    }
}

/// SQLite configuration. `fullfsync` is always ON so macOS flushes the device cache like CedroDB does.
pub struct Sqlite {
    name: &'static str,
    journal_mode: &'static str,
    synchronous: &'static str,
    connection: Option<Connection>,
    directory: PathBuf,
}

impl Sqlite {
    pub fn new(name: &'static str, journal_mode: &'static str, synchronous: &'static str) -> Self {
        Self { name, journal_mode, synchronous, connection: None, directory: PathBuf::new() }
    }
    fn conn(&self) -> &Connection {
        self.connection.as_ref().expect("connection is open")
    }
}

impl Engine for Sqlite {
    fn label(&self) -> String {
        self.name.into()
    }
    fn durability(&self) -> String {
        format!(
            "journal_mode={}, synchronous={}, fullfsync=ON (device-cache flush on macOS){}",
            self.journal_mode,
            self.synchronous,
            if self.synchronous == "NORMAL" { "; NOT equivalent to CedroDB: a WAL commit is not synced until checkpoint" } else { "" }
        )
    }
    fn open(&mut self, directory: &Path) {
        fs::create_dir_all(directory).expect("create directory");
        self.directory = directory.to_path_buf();
        let connection = Connection::open(directory.join("bench.sqlite3")).expect("open SQLite");
        connection
            .execute_batch(&format!(
                "PRAGMA journal_mode={}; PRAGMA synchronous={}; PRAGMA fullfsync=ON; PRAGMA checkpoint_fullfsync=ON;
                 CREATE TABLE IF NOT EXISTS docs(key TEXT PRIMARY KEY, body TEXT NOT NULL);",
                self.journal_mode, self.synchronous
            ))
            .expect("configure SQLite");
        self.connection = Some(connection);
    }
    fn close(&mut self) {
        self.connection = None;
    }
    fn commit_puts(&mut self, items: Vec<(String, Value)>) {
        let connection = self.connection.as_mut().expect("open");
        let tx = connection.transaction().expect("begin");
        {
            let mut statement = tx.prepare_cached("INSERT OR REPLACE INTO docs(key, body) VALUES (?1, ?2)").expect("prepare");
            for (key, value) in &items {
                statement.execute(params![key, value.to_string()]).expect("insert");
            }
        }
        tx.commit().expect("commit");
    }
    fn commit_deletes(&mut self, keys: &[String]) {
        let connection = self.connection.as_mut().expect("open");
        let tx = connection.transaction().expect("begin");
        {
            let mut statement = tx.prepare_cached("DELETE FROM docs WHERE key = ?1").expect("prepare");
            for key in keys {
                statement.execute(params![key]).expect("delete");
            }
        }
        tx.commit().expect("commit");
    }
    fn get(&self, key: &str) -> Option<Value> {
        let mut statement = self.conn().prepare_cached("SELECT body FROM docs WHERE key = ?1").expect("prepare");
        let body: Option<String> = statement.query_row(params![key], |row| row.get(0)).ok();
        body.map(|text| serde_json::from_str(&text).expect("stored JSON"))
    }
    fn aggregate(&self) -> AggregateResult {
        self.conn()
            .query_row(
                "SELECT count(json_extract(body,'$.score')), sum(json_extract(body,'$.score')), min(json_extract(body,'$.score')), max(json_extract(body,'$.score')) FROM docs",
                [],
                |row| Ok(AggregateResult { count: row.get::<_, i64>(0)? as u64, sum: row.get(1)?, min: row.get(2)?, max: row.get(3)? }),
            )
            .expect("aggregate")
    }
    fn disk_bytes(&self) -> u64 {
        directory_bytes(&self.directory)
    }
}
