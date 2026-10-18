//! CedroDB is an experimental embedded JSON database with an append-only log.
//!
//! A transaction becomes visible after its entire frame has been written and
//! `sync_all` succeeds. One handle owns the directory's database file at a time.
//! Reads observe committed state; there is no snapshot/MVCC API. All documents
//! are held in memory. Limits bound encoded content, not total process memory.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error as StdError;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

mod advanced;
mod agent;
mod conditional;
mod maintenance;
mod performance;
mod query;
mod queue;
mod search;
pub use advanced::{ChangeOperation, ChangePage, ChangeTransaction, Stats};
pub use agent::{AgentCheckpoint, AgentStatus, CheckpointOptions, CheckpointResult};
pub use conditional::{Condition, ConditionalResult};
pub use maintenance::IntegrityReport;
pub use performance::{
    CollectionDiagnostics, Diagnostics, OperationMetrics, ProfilePlan, QueryProfile, SearchProfile,
    SessionMetrics,
};
pub use query::{Filter, FilterOp, Query, QueryPage, Record};
pub use queue::{EnqueueOptions, EnqueueResult, Job, JobStatus, QueueStats, RecoveryResult};
pub use search::{SearchHit, SearchMode, SearchPage, SearchQuery};

const HEADER: &[u8; 16] = b"CEDRODB\0\x01\0\0\0\0\0\0\0";
const FRAME_HEADER_BYTES: usize = 12;
const EMPTY_BATCH_BYTES: usize = 10; // {"ops":[]}
const MAX_COLLECTION_BYTES: usize = 1_024;
const MAX_KEY_BYTES: usize = 4_096;
const MAX_DOCUMENT_DEPTH: usize = 64;

pub type Result<T> = std::result::Result<T, Error>;

/// Database errors. An I/O error during commit poisons the open handle.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Serialization(serde_json::Error),
    Corruption(String),
    LimitExceeded(String),
    InvalidInput(String),
    DatabaseLocked,
    WriterBusy,
    Poisoned,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Serialization(error) => write!(f, "JSON error: {error}"),
            Self::Corruption(message) => write!(f, "corrupt database: {message}"),
            Self::LimitExceeded(message) => write!(f, "limit exceeded: {message}"),
            Self::InvalidInput(message) => write!(f, "invalid input: {message}"),
            Self::DatabaseLocked => {
                write!(f, "database is already open by another handle or process")
            }
            Self::WriterBusy => write!(f, "another write transaction is active"),
            Self::Poisoned => write!(f, "database handle is poisoned; close and reopen it"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Serialization(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialization(value)
    }
}

/// Encoded-content limits. Map/allocator overhead and working buffers are extra.
///
/// State bytes count JSON value bytes plus UTF-8 collection/key bytes per record.
/// The log is not compacted and can grow without a disk-size limit.
/// Optional indexes and an eight-byte offset per transaction are additional RAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub max_document_bytes: usize,
    pub max_frame_bytes: usize,
    pub max_state_bytes: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_document_bytes: 1_048_576,
            max_frame_bytes: 16_777_216,
            max_state_bytes: 67_108_864,
        }
    }
}

impl Options {
    fn validate(self) -> Result<()> {
        if self.max_document_bytes == 0
            || self.max_state_bytes == 0
            || self.max_frame_bytes < EMPTY_BATCH_BYTES
            || self.max_frame_bytes > u32::MAX as usize
        {
            return Err(Error::InvalidInput(
                "document/state limits must be positive and frame limit must be 10..=u32::MAX"
                    .into(),
            ));
        }
        Ok(())
    }
}

/// Floating-point aggregate over numeric values in one top-level object field.
/// Missing, null, and nonnumeric fields are ignored. Integer values are converted
/// to f64 here, so values above 2^53 may lose precision. Stored JSON stays intact.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Aggregate {
    pub count: u64,
    pub sum: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone)]
struct Document {
    value: Value,
    content_bytes: usize,
}

type Collections = BTreeMap<String, BTreeMap<String, Document>>;

#[derive(Debug, Default)]
struct State {
    collections: Collections,
    content_bytes: usize,
    indexes: BTreeMap<(String, String), BTreeMap<String, BTreeSet<String>>>,
}

#[derive(Debug)]
struct Inner {
    file: File,
    state: State,
    writer_active: bool,
    poisoned: bool,
    // One offset per committed frame, never a second in-memory copy of history.
    frame_offsets: Vec<u64>,
    metrics: Arc<performance::SessionCounters>,
}

/// One exclusively opened database directory, safe to share across threads.
#[derive(Debug)]
pub struct Database {
    inner: Mutex<Inner>,
    options: Options,
    metrics: Arc<performance::SessionCounters>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Put {
        collection: String,
        key: String,
        value: Value,
    },
    Delete {
        collection: String,
        key: String,
    },
}

impl Operation {
    fn location(&self) -> (&str, &str) {
        match self {
            Self::Put {
                collection, key, ..
            }
            | Self::Delete { collection, key } => (collection, key),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    ops: Vec<Operation>,
}

#[derive(Debug)]
struct Change {
    collection: String,
    key: String,
    document: Option<Document>,
}

impl Database {
    /// Open or create a directory containing `cedro.log`.
    ///
    /// An advisory OS file lock rejects another CedroDB handle/process. Do not
    /// replace, delete, or edit the log while the database is open.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, Options::default())
    }

    pub fn open_with_options(path: impl AsRef<Path>, options: Options) -> Result<Self> {
        options.validate()?;
        let directory = path.as_ref();
        fs::create_dir_all(directory)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("cedro.log"))?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {}
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.raw_os_error().is_some_and(|code| {
                        Some(code) == fs2::lock_contended_error().raw_os_error()
                    }) =>
            {
                return Err(Error::DatabaseLocked);
            }
            Err(error) => return Err(Error::Io(error)),
        }

        let (state, frame_offsets) = if file.metadata()?.len() == 0 {
            file.write_all(HEADER)?;
            file.sync_all()?;
            sync_directory(directory)?;
            if let Some(parent) = directory.parent().filter(|p| !p.as_os_str().is_empty()) {
                sync_directory(parent)?;
            }
            (State::default(), Vec::new())
        } else {
            recover(&mut file, options)?
        };
        file.seek(SeekFrom::End(0))?;
        let metrics = Arc::new(performance::SessionCounters::default());
        Ok(Self {
            inner: Mutex::new(Inner {
                file,
                state,
                writer_active: false,
                poisoned: false,
                frame_offsets,
                metrics: Arc::clone(&metrics),
            }),
            options,
            metrics,
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Inner>> {
        let guard = self.inner.lock().map_err(|_| Error::Poisoned)?;
        if guard.poisoned {
            return Err(Error::Poisoned);
        }
        Ok(guard)
    }

    pub fn begin_write(&self) -> Result<WriteTransaction<'_>> {
        let mut inner = self.lock()?;
        if inner.writer_active {
            return Err(Error::WriterBusy);
        }
        inner.writer_active = true;
        Ok(WriteTransaction {
            database: self,
            operations: Vec::new(),
            encoded_bytes: EMPTY_BATCH_BYTES,
        })
    }

    pub fn get(&self, collection: &str, key: &str) -> Result<Option<Value>> {
        validate_location(collection, key)?;
        let inner = self.lock()?;
        Ok(inner
            .state
            .collections
            .get(collection)
            .and_then(|documents| documents.get(key))
            .map(|document| document.value.clone()))
    }

    /// Return committed records ordered by key. This clones the entire result.
    pub fn scan(&self, collection: &str) -> Result<Vec<(String, Value)>> {
        validate_collection(collection)?;
        let inner = self.lock()?;
        Ok(inner
            .state
            .collections
            .get(collection)
            .map(|documents| {
                documents
                    .iter()
                    .map(|(key, document)| (key.clone(), document.value.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn aggregate(&self, collection: &str, field: &str) -> Result<Aggregate> {
        validate_collection(collection)?;
        if field.is_empty() {
            return Err(Error::InvalidInput(
                "aggregate field must not be empty".into(),
            ));
        }
        let inner = self.lock()?;
        let mut aggregate = Aggregate {
            count: 0,
            sum: 0.0,
            min: None,
            max: None,
        };
        if let Some(documents) = inner.state.collections.get(collection) {
            for document in documents.values() {
                let Some(number) = document
                    .value
                    .as_object()
                    .and_then(|object| object.get(field))
                    .and_then(Value::as_f64)
                else {
                    continue;
                };
                let sum = aggregate.sum + number;
                if !sum.is_finite() {
                    return Err(Error::LimitExceeded(
                        "aggregate sum exceeds finite f64 range".into(),
                    ));
                }
                aggregate.count += 1;
                aggregate.sum = sum;
                aggregate.min = Some(aggregate.min.map_or(number, |value| value.min(number)));
                aggregate.max = Some(aggregate.max.map_or(number, |value| value.max(number)));
            }
        }
        Ok(aggregate)
    }
}

/// Staged writes. Dropping a transaction rolls it back without touching disk.
/// Reads through Database continue to see the previously committed state.
#[derive(Debug)]
pub struct WriteTransaction<'database> {
    database: &'database Database,
    operations: Vec<Operation>,
    encoded_bytes: usize,
}

impl WriteTransaction<'_> {
    pub fn put(&mut self, collection: &str, key: &str, value: Value) -> Result<()> {
        validate_user_location(collection, key)?;
        validate_document(&value, self.database.options)?;
        self.stage(Operation::Put {
            collection: collection.into(),
            key: key.into(),
            value,
        })
    }

    pub fn delete(&mut self, collection: &str, key: &str) -> Result<()> {
        validate_user_location(collection, key)?;
        self.stage(Operation::Delete {
            collection: collection.into(),
            key: key.into(),
        })
    }

    fn stage(&mut self, operation: Operation) -> Result<()> {
        let bytes = serde_json::to_vec(&operation)?.len();
        let encoded_bytes = self
            .encoded_bytes
            .checked_add(bytes)
            .and_then(|bytes| bytes.checked_add(usize::from(!self.operations.is_empty())))
            .ok_or_else(|| Error::LimitExceeded("transaction size overflow".into()))?;
        if encoded_bytes > self.database.options.max_frame_bytes {
            return Err(Error::LimitExceeded(
                "transaction frame is too large".into(),
            ));
        }
        self.operations.push(operation);
        self.encoded_bytes = encoded_bytes;
        Ok(())
    }

    /// Persist one complete frame and sync it before exposing any changes.
    /// After an I/O failure, reopen the database to resolve the commit outcome.
    pub fn commit(self) -> Result<()> {
        let mut inner = self.database.lock()?;
        commit_operations(&mut inner, &self.operations, self.database.options)
    }
}

fn commit_operations(inner: &mut Inner, operations: &[Operation], options: Options) -> Result<()> {
    if operations.is_empty() {
        return Ok(());
    }
    let metrics = Arc::clone(&inner.metrics);
    let timer = metrics.commit.start();
    let result = persist_operations(inner, operations, options);
    timer.finish(result.is_ok());
    result
}

fn persist_operations(inner: &mut Inner, operations: &[Operation], options: Options) -> Result<()> {
    let (changes, state_bytes) = prepare_changes(&inner.state, operations, options)?;
    // Serialize by reference so staging and compare-and-set share one commit path.
    #[derive(Serialize)]
    struct BorrowedBatch<'a> {
        ops: &'a [Operation],
    }
    let payload = serde_json::to_vec(&BorrowedBatch { ops: operations })?;
    if payload.len() > options.max_frame_bytes {
        return Err(Error::LimitExceeded(
            "transaction frame is too large".into(),
        ));
    }
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&(!(payload.len() as u32)).to_le_bytes());
    frame.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
    frame.extend_from_slice(&payload);
    // Every append seeks explicitly: changefeed reads also use this descriptor.
    let offset = match inner.file.seek(SeekFrom::End(0)) {
        Ok(offset) => offset,
        Err(error) => {
            inner.poisoned = true;
            return Err(Error::Io(error));
        }
    };
    if let Err(error) = inner
        .file
        .write_all(&frame)
        .and_then(|()| inner.file.sync_all())
    {
        inner.poisoned = true;
        return Err(Error::Io(error));
    }
    apply_changes(&mut inner.state, changes, state_bytes);
    inner.frame_offsets.push(offset);
    Ok(())
}

impl Drop for WriteTransaction<'_> {
    fn drop(&mut self) {
        // Avoid unwrap: a panic while the mutex was held already poisons it.
        if let Ok(mut inner) = self.database.inner.lock() {
            inner.writer_active = false;
        }
    }
}

fn validate_collection(collection: &str) -> Result<()> {
    if collection.is_empty() || collection.len() > MAX_COLLECTION_BYTES {
        return Err(Error::InvalidInput(format!(
            "collection must contain 1..={MAX_COLLECTION_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}

fn validate_location(collection: &str, key: &str) -> Result<()> {
    validate_collection(collection)?;
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(Error::InvalidInput(format!(
            "key must contain 1..={MAX_KEY_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}

/// Public mutation APIs reserve internal storage while log replay and typed methods
/// retain access through validate_location and the shared internal commit path.
fn validate_user_location(collection: &str, key: &str) -> Result<()> {
    validate_location(collection, key)?;
    if collection.starts_with(queue::QUEUE_PREFIX) {
        return Err(Error::InvalidInput(
            "collections beginning with _cedro.jobs. are reserved for queue APIs".into(),
        ));
    }
    if collection.starts_with(agent::AGENT_PREFIX) {
        return Err(Error::InvalidInput(
            "collections beginning with _cedro.agents. are reserved for agent APIs".into(),
        ));
    }
    Ok(())
}

fn validate_document(value: &Value, options: Options) -> Result<usize> {
    let mut pending = vec![(value, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        if depth > MAX_DOCUMENT_DEPTH {
            return Err(Error::LimitExceeded(format!(
                "JSON document nesting exceeds {MAX_DOCUMENT_DEPTH} levels"
            )));
        }
        match value {
            Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            _ => {}
        }
    }
    let mut counter = EncodedCounter {
        bytes: 0,
        limit: options.max_document_bytes,
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut counter, value);
    if counter.exceeded {
        return Err(Error::LimitExceeded("JSON document is too large".into()));
    }
    result?;
    Ok(counter.bytes)
}

struct EncodedCounter {
    bytes: usize,
    limit: usize,
    exceeded: bool,
}

impl Write for EncodedCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes) {
            self.exceeded = true;
            return Err(io::Error::other("document limit exceeded"));
        }
        self.bytes += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Decide which operation wins for each key and measure every document exactly once.
///
/// Returns `(operation index, encoded value bytes)` for the final operation of each key, ordered by
/// location. Every `Put` is validated, including ones superseded inside the same transaction.
fn plan_changes(operations: &[Operation], options: Options) -> Result<Vec<(usize, usize)>> {
    let mut final_operations = BTreeMap::new();
    for (index, operation) in operations.iter().enumerate() {
        let (collection, key) = operation.location();
        validate_location(collection, key)?;
        let value_bytes = match operation {
            Operation::Put { value, .. } => validate_document(value, options)?,
            Operation::Delete { .. } => 0,
        };
        final_operations.insert((collection, key), (index, value_bytes));
    }
    Ok(final_operations.into_values().collect())
}

/// Apply a plan to the current state. `take_value` supplies the document for a `Put` by operation
/// index (a clone when the operations are still needed, a move during replay).
fn build_changes(
    state: &State,
    operations: &[Operation],
    plan: &[(usize, usize)],
    options: Options,
    mut take_value: impl FnMut(usize) -> Option<Value>,
) -> Result<(Vec<Change>, usize)> {
    let mut state_bytes = state.content_bytes;
    let mut changes = Vec::with_capacity(plan.len());
    for &(index, value_bytes) in plan {
        let (collection, key) = operations[index].location();
        if let Some(old) = state
            .collections
            .get(collection)
            .and_then(|documents| documents.get(key))
        {
            state_bytes -= old.content_bytes;
        }
        let document = match &operations[index] {
            Operation::Put { .. } => {
                let content_bytes = collection
                    .len()
                    .checked_add(key.len())
                    .and_then(|bytes| bytes.checked_add(value_bytes))
                    .ok_or_else(|| Error::LimitExceeded("state size overflow".into()))?;
                state_bytes = state_bytes
                    .checked_add(content_bytes)
                    .ok_or_else(|| Error::LimitExceeded("state size overflow".into()))?;
                Some(Document {
                    value: take_value(index).expect("a put always carries a value"),
                    content_bytes,
                })
            }
            Operation::Delete { .. } => None,
        };
        changes.push(Change {
            collection: collection.into(),
            key: key.into(),
            document,
        });
    }
    if state_bytes > options.max_state_bytes {
        return Err(Error::LimitExceeded("committed state is too large".into()));
    }
    Ok((changes, state_bytes))
}

fn prepare_changes(
    state: &State,
    operations: &[Operation],
    options: Options,
) -> Result<(Vec<Change>, usize)> {
    let plan = plan_changes(operations, options)?;
    build_changes(
        state,
        operations,
        &plan,
        options,
        |index| match &operations[index] {
            Operation::Put { value, .. } => Some(value.clone()),
            Operation::Delete { .. } => None,
        },
    )
}

/// Same result as [`prepare_changes`] for operations the caller no longer needs: documents are
/// moved into the state instead of cloned (log replay and backup verification).
fn prepare_changes_owned(
    state: &State,
    mut operations: Vec<Operation>,
    options: Options,
) -> Result<(Vec<Change>, usize)> {
    let plan = plan_changes(&operations, options)?;
    // Detach the documents first; locations stay in `operations`.
    let mut values: Vec<Option<Value>> = operations
        .iter_mut()
        .map(|operation| match operation {
            Operation::Put { value, .. } => Some(std::mem::take(value)),
            Operation::Delete { .. } => None,
        })
        .collect();
    build_changes(state, &operations, &plan, options, |index| {
        values[index].take()
    })
}

fn apply_changes(state: &mut State, changes: Vec<Change>, state_bytes: usize) {
    for change in changes {
        for ((collection, field), index) in &mut state.indexes {
            if collection != &change.collection {
                continue;
            }
            let old_value = state
                .collections
                .get(collection)
                .and_then(|documents| documents.get(&change.key))
                .and_then(|document| query::field_value(&document.value, field));
            if let Some(value) = old_value {
                let token = query::index_token(value);
                if let Some(keys) = index.get_mut(&token) {
                    keys.remove(&change.key);
                    if keys.is_empty() {
                        index.remove(&token);
                    }
                }
            }
            if let Some(value) = change
                .document
                .as_ref()
                .and_then(|document| query::field_value(&document.value, field))
            {
                index
                    .entry(query::index_token(value))
                    .or_default()
                    .insert(change.key.clone());
            }
        }
        match change.document {
            Some(document) => {
                state
                    .collections
                    .entry(change.collection)
                    .or_default()
                    .insert(change.key, document);
            }
            None => {
                if let Some(documents) = state.collections.get_mut(&change.collection) {
                    documents.remove(&change.key);
                    if documents.is_empty() {
                        state.collections.remove(&change.collection);
                    }
                }
            }
        }
    }
    state.content_bytes = state_bytes;
}

fn recover(file: &mut File, options: Options) -> Result<(State, Vec<u64>)> {
    let file_bytes = file.metadata()?.len();
    if file_bytes < HEADER.len() as u64 {
        return Err(Error::Corruption("incomplete database header".into()));
    }
    let mut header = [0; HEADER.len()];
    file.read_exact(&mut header)?;
    if &header != HEADER {
        return Err(Error::Corruption(
            "unsupported or invalid database header".into(),
        ));
    }
    let mut state = State::default();
    let mut frame_offsets = Vec::new();
    let mut offset = HEADER.len() as u64;
    while offset < file_bytes {
        if file_bytes - offset < FRAME_HEADER_BYTES as u64 {
            truncate_tail(file, offset)?;
            break;
        }
        let mut frame_header = [0u8; FRAME_HEADER_BYTES];
        file.read_exact(&mut frame_header)?;
        let payload_length = u32::from_le_bytes(frame_header[..4].try_into().unwrap());
        let inverted_length = u32::from_le_bytes(frame_header[4..8].try_into().unwrap());
        if inverted_length != !payload_length {
            return Err(Error::Corruption(format!(
                "frame length integrity check failed at byte {offset}"
            )));
        }
        let payload_bytes = payload_length as usize;
        let checksum = u32::from_le_bytes(frame_header[8..12].try_into().unwrap());
        if payload_bytes < EMPTY_BATCH_BYTES || payload_bytes > options.max_frame_bytes {
            return Err(Error::Corruption(format!(
                "invalid frame length {payload_bytes} at byte {offset}"
            )));
        }
        let frame_bytes = FRAME_HEADER_BYTES as u64 + payload_bytes as u64;
        if file_bytes - offset < frame_bytes {
            truncate_tail(file, offset)?;
            break;
        }
        let mut payload = vec![0; payload_bytes];
        file.read_exact(&mut payload)?;
        if crc32fast::hash(&payload) != checksum {
            return Err(Error::Corruption(format!(
                "checksum mismatch at byte {offset}"
            )));
        }
        let batch: Batch = serde_json::from_slice(&payload).map_err(|error| {
            Error::Corruption(format!("invalid transaction at byte {offset}: {error}"))
        })?;
        let (changes, state_bytes) =
            prepare_changes_owned(&state, batch.ops, options).map_err(|error| {
                Error::Corruption(format!("invalid transaction at byte {offset}: {error}"))
            })?;
        apply_changes(&mut state, changes, state_bytes);
        frame_offsets.push(offset);
        offset += frame_bytes;
    }
    Ok((state, frame_offsets))
}

fn truncate_tail(file: &mut File, offset: u64) -> Result<()> {
    file.set_len(offset)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    // Rust's standard File API does not portably open directory handles.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn failed_commit_poisons_handle_until_reopen() {
        let path = std::env::temp_dir().join(format!(
            "cedrodb-poison-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let database = Database::open(&path).unwrap();
        // Replace the writer with a read-only descriptor to force write failure.
        database.inner.lock().unwrap().file = File::open(path.join("cedro.log")).unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction
            .put("orders", "one", json!({"amount": 12}))
            .unwrap();
        assert!(matches!(transaction.commit(), Err(Error::Io(_))));
        let diagnostics = database.diagnostics().unwrap();
        assert!(diagnostics.poisoned);
        assert_eq!(diagnostics.session.commit.attempts, 1);
        assert_eq!(diagnostics.session.commit.successes, 0);
        assert_eq!(diagnostics.session.commit.failures, 1);
        assert!(matches!(database.begin_write(), Err(Error::Poisoned)));
        assert!(matches!(
            database.get("orders", "one"),
            Err(Error::Poisoned)
        ));
        drop(database);
        let reopened = Database::open(&path).unwrap();
        assert_eq!(reopened.get("orders", "one").unwrap(), None);
        drop(reopened);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn reject_excessive_nesting_before_serialization() {
        let mut value = json!(0);
        for _ in 0..=MAX_DOCUMENT_DEPTH {
            value = Value::Array(vec![value]);
        }
        assert!(matches!(
            validate_document(&value, Options::default()),
            Err(Error::LimitExceeded(_))
        ));
    }
}
