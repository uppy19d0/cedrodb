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

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

/// Reads one operation straight from the JSON map. An internally tagged derive would first copy the whole
/// document into serde's intermediate buffer; replaying a large log spent most of its time doing that.
/// Accepted shape and errors match the derived version: `kind` is `put` or `delete`, `put` needs `value`,
/// `delete` rejects it, unknown or repeated fields are errors.
impl<'de> Deserialize<'de> for Operation {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_map(OperationVisitor)
    }
}

struct OperationVisitor;

/// Field names are matched without allocating a `String` per field (four per operation otherwise).
enum Field {
    Kind,
    Collection,
    Key,
    Value,
    Unknown(String),
}

impl<'de> Deserialize<'de> for Field {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct FieldVisitor;
        impl serde::de::Visitor<'_> for FieldVisitor {
            type Value = Field;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an operation field name")
            }
            fn visit_str<E: serde::de::Error>(self, name: &str) -> std::result::Result<Field, E> {
                Ok(match name {
                    "kind" => Field::Kind,
                    "collection" => Field::Collection,
                    "key" => Field::Key,
                    "value" => Field::Value,
                    other => Field::Unknown(other.to_owned()),
                })
            }
        }
        deserializer.deserialize_identifier(FieldVisitor)
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Put,
    Delete,
}

impl<'de> Deserialize<'de> for Kind {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct KindVisitor;
        impl serde::de::Visitor<'_> for KindVisitor {
            type Value = Kind;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("`put` or `delete`")
            }
            fn visit_str<E: serde::de::Error>(self, name: &str) -> std::result::Result<Kind, E> {
                match name {
                    "put" => Ok(Kind::Put),
                    "delete" => Ok(Kind::Delete),
                    other => Err(E::unknown_variant(other, &["put", "delete"])),
                }
            }
        }
        deserializer.deserialize_str(KindVisitor)
    }
}

impl<'de> serde::de::Visitor<'de> for OperationVisitor {
    type Value = Operation;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a put or delete operation")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Operation, A::Error> {
        use serde::de::Error as _;
        let (mut kind, mut collection, mut key): (Option<Kind>, Option<String>, Option<String>) =
            (None, None, None);
        let mut value: Option<Value> = None;
        while let Some(field) = map.next_key::<Field>()? {
            match field {
                Field::Kind => set_once(&mut kind, map.next_value()?, "kind")?,
                Field::Collection => set_once(&mut collection, map.next_value()?, "collection")?,
                Field::Key => set_once(&mut key, map.next_value()?, "key")?,
                Field::Value => set_once(&mut value, map.next_value()?, "value")?,
                Field::Unknown(other) => {
                    return Err(A::Error::unknown_field(
                        &other,
                        &["kind", "collection", "key", "value"],
                    ))
                }
            }
        }
        let collection = collection.ok_or_else(|| A::Error::missing_field("collection"))?;
        let key = key.ok_or_else(|| A::Error::missing_field("key"))?;
        match kind {
            Some(Kind::Put) => Ok(Operation::Put {
                collection,
                key,
                value: value.ok_or_else(|| A::Error::missing_field("value"))?,
            }),
            Some(Kind::Delete) => {
                if value.is_some() {
                    return Err(A::Error::unknown_field(
                        "value",
                        &["kind", "collection", "key"],
                    ));
                }
                Ok(Operation::Delete { collection, key })
            }
            None => Err(A::Error::missing_field("kind")),
        }
    }
}

fn set_once<T, E: serde::de::Error>(
    slot: &mut Option<T>,
    value: T,
    name: &'static str,
) -> std::result::Result<(), E> {
    if slot.replace(value).is_some() {
        return Err(E::duplicate_field(name));
    }
    Ok(())
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
    operations: Vec<Operation>,
    options: Options,
) -> Result<(Vec<Change>, usize)> {
    let planned = plan_changes_owned(operations, options)?;
    finish_changes_owned(state, planned, options)
}

/// State-independent half of [`prepare_changes_owned`]: validation and winner selection. Safe to
/// run on any thread, in any order.
struct PlannedChanges {
    operations: Vec<Operation>,
    values: Vec<Option<Value>>,
    plan: Vec<(usize, usize)>,
}

fn plan_changes_owned(mut operations: Vec<Operation>, options: Options) -> Result<PlannedChanges> {
    let plan = plan_changes(&operations, options)?;
    // Detach the documents first; locations stay in `operations`.
    let values: Vec<Option<Value>> = operations
        .iter_mut()
        .map(|operation| match operation {
            Operation::Put { value, .. } => Some(std::mem::take(value)),
            Operation::Delete { .. } => None,
        })
        .collect();
    Ok(PlannedChanges {
        operations,
        values,
        plan,
    })
}

/// State-dependent half: size accounting against the current state. Must run in log order.
fn finish_changes_owned(
    state: &State,
    mut planned: PlannedChanges,
    options: Options,
) -> Result<(Vec<Change>, usize)> {
    build_changes(
        state,
        &planned.operations,
        &planned.plan,
        options,
        |index| planned.values[index].take(),
    )
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

/// Reference implementation: strictly one frame at a time. Kept as the specification the parallel
/// path must match (same state, same first error, same message, same truncation).
fn recover_sequential(file: &mut File, options: Options) -> Result<(State, Vec<u64>)> {
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
        if payload_bytes < EMPTY_BATCH_BYTES {
            return Err(Error::Corruption(format!(
                "invalid frame length {payload_bytes} at byte {offset}"
            )));
        }
        if payload_bytes > options.max_frame_bytes {
            return Err(Error::LimitExceeded(format!(
                "frame of {payload_bytes} bytes at byte {offset} exceeds max_frame_bytes {}; \
                 reopen with the limits the log was written with (limits must not shrink)",
                options.max_frame_bytes
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
            prepare_changes_owned(&state, batch.ops, options).map_err(|error| match error {
                Error::LimitExceeded(message) => Error::LimitExceeded(format!(
                    "transaction at byte {offset}: {message}; reopen with the limits the log \
                     was written with (limits must not shrink)"
                )),
                error => {
                    Error::Corruption(format!("invalid transaction at byte {offset}: {error}"))
                }
            })?;
        apply_changes(&mut state, changes, state_bytes);
        frame_offsets.push(offset);
        offset += frame_bytes;
    }
    Ok((state, frame_offsets))
}

/// Frames decoded per parallel window: bounded by count and by payload bytes so reopen memory
/// stays small. A window always holds at least one frame.
const RECOVER_WINDOW_FRAMES: usize = 256;
const RECOVER_WINDOW_BYTES: usize = 4 * 1024 * 1024;
/// Below this many frames in a window, threads cost more than they save.
const RECOVER_MIN_PARALLEL_FRAMES: usize = 4;
const RECOVER_MAX_WORKERS: usize = 8;

fn recover(file: &mut File, options: Options) -> Result<(State, Vec<u64>)> {
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(RECOVER_MAX_WORKERS);
    if workers < 2 {
        recover_sequential(file, options)
    } else {
        recover_parallel(file, options, workers)
    }
}

struct RawFrame {
    offset: u64,
    checksum: u32,
    payload: Vec<u8>,
}

/// What ended a window scan, to be acted on only after the frames before it were applied
/// successfully (an earlier frame's error always wins, as in the sequential path).
enum ScanEnd {
    More,
    Truncate(u64),
    Fail(Error),
}

fn map_prepare_error(offset: u64, error: Error) -> Error {
    match error {
        Error::LimitExceeded(message) => Error::LimitExceeded(format!(
            "transaction at byte {offset}: {message}; reopen with the limits the log \
             was written with (limits must not shrink)"
        )),
        error => Error::Corruption(format!("invalid transaction at byte {offset}: {error}")),
    }
}

/// Header checks and payload read for the next frames; mirrors the checks of
/// `recover_sequential` in the same order, but defers checksum and decoding.
fn scan_window(
    file: &mut File,
    file_bytes: u64,
    offset: &mut u64,
    options: Options,
    frames: &mut Vec<RawFrame>,
) -> ScanEnd {
    let mut window_bytes = 0usize;
    while *offset < file_bytes {
        if frames.len() >= RECOVER_WINDOW_FRAMES || (window_bytes >= RECOVER_WINDOW_BYTES) {
            return ScanEnd::More;
        }
        let at = *offset;
        if file_bytes - at < FRAME_HEADER_BYTES as u64 {
            return ScanEnd::Truncate(at);
        }
        let mut frame_header = [0u8; FRAME_HEADER_BYTES];
        if let Err(error) = file.read_exact(&mut frame_header) {
            return ScanEnd::Fail(error.into());
        }
        let payload_length = u32::from_le_bytes(frame_header[..4].try_into().unwrap());
        let inverted_length = u32::from_le_bytes(frame_header[4..8].try_into().unwrap());
        if inverted_length != !payload_length {
            return ScanEnd::Fail(Error::Corruption(format!(
                "frame length integrity check failed at byte {at}"
            )));
        }
        let payload_bytes = payload_length as usize;
        let checksum = u32::from_le_bytes(frame_header[8..12].try_into().unwrap());
        if payload_bytes < EMPTY_BATCH_BYTES {
            return ScanEnd::Fail(Error::Corruption(format!(
                "invalid frame length {payload_bytes} at byte {at}"
            )));
        }
        if payload_bytes > options.max_frame_bytes {
            return ScanEnd::Fail(Error::LimitExceeded(format!(
                "frame of {payload_bytes} bytes at byte {at} exceeds max_frame_bytes {}; \
                 reopen with the limits the log was written with (limits must not shrink)",
                options.max_frame_bytes
            )));
        }
        let frame_bytes = FRAME_HEADER_BYTES as u64 + payload_bytes as u64;
        if file_bytes - at < frame_bytes {
            return ScanEnd::Truncate(at);
        }
        // Do not let one window exceed the byte budget (but always take one frame).
        if !frames.is_empty() && window_bytes + payload_bytes > RECOVER_WINDOW_BYTES {
            // Rewind the header just read; it is re-read in the next window.
            if let Err(error) = file.seek(SeekFrom::Start(at)) {
                return ScanEnd::Fail(error.into());
            }
            return ScanEnd::More;
        }
        let mut payload = vec![0; payload_bytes];
        if let Err(error) = file.read_exact(&mut payload) {
            return ScanEnd::Fail(error.into());
        }
        window_bytes += payload_bytes;
        frames.push(RawFrame {
            offset: at,
            checksum,
            payload,
        });
        *offset = at + frame_bytes;
    }
    ScanEnd::More
}

/// Checksum, parse and plan one frame; the part of recovery that does not depend on state.
fn decode_frame(frame: RawFrame, options: Options) -> Result<PlannedChanges> {
    let offset = frame.offset;
    if crc32fast::hash(&frame.payload) != frame.checksum {
        return Err(Error::Corruption(format!(
            "checksum mismatch at byte {offset}"
        )));
    }
    let batch: Batch = serde_json::from_slice(&frame.payload).map_err(|error| {
        Error::Corruption(format!("invalid transaction at byte {offset}: {error}"))
    })?;
    drop(frame);
    plan_changes_owned(batch.ops, options).map_err(|error| map_prepare_error(offset, error))
}

fn decode_frames(
    frames: Vec<RawFrame>,
    options: Options,
    workers: usize,
) -> Vec<Result<PlannedChanges>> {
    let count = frames.len();
    let workers = workers.min(count);
    if count < RECOVER_MIN_PARALLEL_FRAMES || workers < 2 {
        return frames
            .into_iter()
            .map(|frame| decode_frame(frame, options))
            .collect();
    }
    let chunk_size = count.div_ceil(workers);
    let mut chunks: Vec<Vec<RawFrame>> = Vec::with_capacity(workers);
    let mut remaining = frames;
    while remaining.len() > chunk_size {
        let rest = remaining.split_off(chunk_size);
        chunks.push(std::mem::replace(&mut remaining, rest));
    }
    chunks.push(remaining);
    std::thread::scope(|scope| {
        let mut chunks = chunks.into_iter();
        let first = chunks.next().expect("at least one chunk");
        // `None` marks a chunk run inline because its thread could not be spawned.
        let mut handles = Vec::new();
        for chunk in chunks {
            // On spawn failure the chunk is lost with the closure, so keep it reachable.
            let slot = std::sync::Arc::new(Mutex::new(Some(chunk)));
            let worker_slot = slot.clone();
            let spawned = std::thread::Builder::new()
                .name("cedrodb-recover".into())
                .spawn_scoped(scope, move || {
                    let chunk = worker_slot.lock().ok().and_then(|mut guard| guard.take());
                    chunk
                        .unwrap_or_default()
                        .into_iter()
                        .map(|frame| decode_frame(frame, options))
                        .collect::<Vec<_>>()
                });
            handles.push((slot, spawned.ok()));
        }
        let mut results: Vec<Result<PlannedChanges>> = first
            .into_iter()
            .map(|frame| decode_frame(frame, options))
            .collect();
        for (slot, handle) in handles {
            match handle {
                Some(handle) => match handle.join() {
                    Ok(part) => results.extend(part),
                    Err(panic) => std::panic::resume_unwind(panic),
                },
                None => {
                    let chunk = slot.lock().ok().and_then(|mut guard| guard.take());
                    results.extend(
                        chunk
                            .unwrap_or_default()
                            .into_iter()
                            .map(|frame| decode_frame(frame, options)),
                    );
                }
            }
        }
        results
    })
}

/// Same observable result as [`recover_sequential`]: frames are scanned in bounded windows,
/// decoded on up to `workers` threads, then applied strictly in log order. Any error (including
/// one that ended the scan) is reported only after every earlier frame applied cleanly.
fn recover_parallel(
    file: &mut File,
    options: Options,
    workers: usize,
) -> Result<(State, Vec<u64>)> {
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
    loop {
        let mut frames = Vec::new();
        let end = scan_window(file, file_bytes, &mut offset, options, &mut frames);
        let offsets: Vec<u64> = frames.iter().map(|frame| frame.offset).collect();
        for (frame_offset, planned) in offsets
            .into_iter()
            .zip(decode_frames(frames, options, workers))
        {
            let (changes, state_bytes) = finish_changes_owned(&state, planned?, options)
                .map_err(|error| map_prepare_error(frame_offset, error))?;
            apply_changes(&mut state, changes, state_bytes);
            frame_offsets.push(frame_offset);
        }
        match end {
            ScanEnd::More if offset < file_bytes => {}
            ScanEnd::More => break,
            ScanEnd::Truncate(at) => {
                truncate_tail(file, at)?;
                break;
            }
            ScanEnd::Fail(error) => return Err(error),
        }
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

    fn parse(text: &str) -> std::result::Result<Operation, serde_json::Error> {
        serde_json::from_str(text)
    }

    #[test]
    fn operations_round_trip_through_their_log_encoding() {
        let put = Operation::Put {
            collection: "orders".into(),
            key: "1".into(),
            value: json!({"total": 12.5, "items": [1, 2, {"deep": null}]}),
        };
        let encoded = serde_json::to_string(&put).unwrap();
        assert!(encoded.starts_with(r#"{"kind":"put","collection":"orders","key":"1","value":"#));
        match parse(&encoded).unwrap() {
            Operation::Put {
                collection,
                key,
                value,
            } => {
                assert_eq!((collection.as_str(), key.as_str()), ("orders", "1"));
                assert_eq!(
                    value,
                    json!({"total": 12.5, "items": [1, 2, {"deep": null}]})
                );
            }
            other => panic!("expected a put, got {other:?}"),
        }
        let delete = serde_json::to_string(&Operation::Delete {
            collection: "c".into(),
            key: "k".into(),
        })
        .unwrap();
        assert!(matches!(parse(&delete).unwrap(), Operation::Delete { .. }));
    }

    #[test]
    fn operation_fields_may_appear_in_any_order_and_nulls_are_values() {
        let operation = parse(r#"{"value":null,"key":"k","collection":"c","kind":"put"}"#).unwrap();
        assert!(matches!(
            operation,
            Operation::Put {
                value: Value::Null,
                ..
            }
        ));
    }

    #[test]
    fn malformed_operations_are_rejected() {
        for (text, expected) in [
            (
                r#"{"collection":"c","key":"k","value":1}"#,
                "missing field `kind`",
            ),
            (
                r#"{"kind":"put","key":"k","value":1}"#,
                "missing field `collection`",
            ),
            (
                r#"{"kind":"put","collection":"c","value":1}"#,
                "missing field `key`",
            ),
            (
                r#"{"kind":"put","collection":"c","key":"k"}"#,
                "missing field `value`",
            ),
            (
                r#"{"kind":"update","collection":"c","key":"k"}"#,
                "unknown variant `update`",
            ),
            (
                r#"{"kind":"delete","collection":"c","key":"k","value":1}"#,
                "unknown field `value`",
            ),
            (
                r#"{"kind":"put","collection":"c","key":"k","value":1,"extra":true}"#,
                "unknown field `extra`",
            ),
            (
                r#"{"kind":"put","kind":"delete","collection":"c","key":"k"}"#,
                "duplicate field `kind`",
            ),
            (
                r#"{"kind":"put","collection":"c","key":"k","value":1,"value":2}"#,
                "duplicate field `value`",
            ),
            (
                r#"{"kind":"put","collection":7,"key":"k","value":1}"#,
                "invalid type",
            ),
            (r#"["put","c","k"]"#, "invalid type"),
        ] {
            let error = parse(text).expect_err(text).to_string();
            assert!(
                error.contains(expected),
                "{text}: expected `{expected}` in `{error}`"
            );
        }
    }
}

#[cfg(test)]
mod recover_equivalence {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn frame(ops: &[Value]) -> Vec<u8> {
        let payload = serde_json::to_vec(&json!({ "ops": ops })).unwrap();
        let length = payload.len() as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&(!length).to_le_bytes());
        bytes.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes
    }

    fn put(collection: &str, key: &str, value: Value) -> Value {
        json!({"kind": "put", "collection": collection, "key": key, "value": value})
    }

    fn delete(collection: &str, key: &str) -> Value {
        json!({"kind": "delete", "collection": collection, "key": key})
    }

    /// A valid log of `frames` frames with overwrites and deletes across frames.
    fn sample_log(frames: usize) -> (Vec<u8>, Vec<usize>) {
        let mut bytes = HEADER.to_vec();
        let mut starts = Vec::new();
        for index in 0..frames {
            starts.push(bytes.len());
            let mut ops = vec![
                put(
                    "c",
                    &format!("k{}", index % 7),
                    json!({"n": index, "s": "x".repeat(index % 5)}),
                ),
                put("d", &format!("k{index}"), json!([index, {"a": null}])),
            ];
            if index % 3 == 0 {
                ops.push(delete("c", &format!("k{}", (index + 2) % 7)));
            }
            bytes.extend(frame(&ops));
        }
        (bytes, starts)
    }

    /// Outcome of one recovery on a private copy: state, offsets, final file length or error text.
    fn outcome(bytes: &[u8], options: Options, parallel: bool, workers: usize) -> String {
        let path = std::env::temp_dir().join(format!(
            "cedrodb-recover-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, bytes).unwrap();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let result = if parallel {
            recover_parallel(&mut file, options, workers)
        } else {
            recover_sequential(&mut file, options)
        };
        let length = file.metadata().unwrap().len();
        drop(file);
        fs::remove_file(&path).unwrap();
        match result {
            Ok((state, offsets)) => format!("ok {state:?} {offsets:?} len={length}"),
            Err(error) => format!(
                "err {} {error} len={length}",
                format!("{error:?}").split('(').next().unwrap()
            ),
        }
    }

    fn assert_same(bytes: &[u8], options: Options) -> String {
        let reference = outcome(bytes, options, false, 1);
        for workers in [2, 4, 8] {
            assert_eq!(
                outcome(bytes, options, true, workers),
                reference,
                "workers={workers}"
            );
        }
        reference
    }

    #[test]
    fn valid_logs_match_across_frame_counts() {
        for frames in [0, 1, 3, 4, 5, 17, 300, 600] {
            let (bytes, _) = sample_log(frames);
            assert!(assert_same(&bytes, Options::default()).starts_with("ok "));
        }
    }

    #[test]
    fn corrupt_middle_frame_matches_and_first_error_wins() {
        let (bytes, starts) = sample_log(40);
        // Bad CRC in frame 10 and a length mismatch later: the earlier frame's error must win.
        let mut mutated = bytes.clone();
        mutated[starts[10] + FRAME_HEADER_BYTES + 3] ^= 0x55;
        mutated[starts[30] + 4] ^= 0x01;
        let result = assert_same(&mutated, Options::default());
        assert!(
            result.contains(&format!("checksum mismatch at byte {}", starts[10])),
            "{result}"
        );
        // Only the later length-complement error.
        let mut mutated = bytes.clone();
        mutated[starts[30] + 4] ^= 0x01;
        let result = assert_same(&mutated, Options::default());
        assert!(result.contains("length integrity"), "{result}");
        // A frame with a valid CRC but invalid JSON contents, before a bad CRC.
        let mut mutated = bytes[..starts[5]].to_vec();
        let junk = b"{\"ops\":[{\"kind\":\"nope\"}]}";
        let length = junk.len() as u32;
        mutated.extend_from_slice(&length.to_le_bytes());
        mutated.extend_from_slice(&(!length).to_le_bytes());
        mutated.extend_from_slice(&crc32fast::hash(junk).to_le_bytes());
        mutated.extend_from_slice(junk);
        let mut tail = bytes[starts[6]..].to_vec();
        tail[FRAME_HEADER_BYTES + 2] ^= 0x10;
        mutated.extend(tail);
        let result = assert_same(&mutated, Options::default());
        assert!(result.contains("invalid transaction at byte"), "{result}");
    }

    #[test]
    fn truncated_and_crash_tails_match_and_reopen_cleanly() {
        let (bytes, starts) = sample_log(40);
        for cut in [
            bytes.len() - 1,
            starts[39] + 5,
            starts[39] + FRAME_HEADER_BYTES,
            starts[20] + 1,
            HEADER.len(),
        ] {
            let result = assert_same(&bytes[..cut], Options::default());
            assert!(result.starts_with("ok "), "{result}");
        }
        // Truncated tail after a corrupt earlier frame: the corruption error wins, no truncation.
        let mut mutated = bytes[..bytes.len() - 3].to_vec();
        mutated[starts[2] + FRAME_HEADER_BYTES + 1] ^= 0x20;
        let result = assert_same(&mutated, Options::default());
        assert!(result.starts_with("err Corruption"), "{result}");
        assert!(
            result.ends_with(&format!("len={}", mutated.len())),
            "{result}"
        );
        // Real crash tail through the public API: reopen twice, same recovered documents.
        let path = std::env::temp_dir().join(format!("cedrodb-crash-tail-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        {
            let database = Database::open(&path).unwrap();
            for index in 0..30 {
                let mut transaction = database.begin_write().unwrap();
                transaction
                    .put("c", &format!("k{index}"), json!(index))
                    .unwrap();
                transaction.commit().unwrap();
            }
        }
        let log = path.join("cedro.log");
        let mut data = fs::read(&log).unwrap();
        data.extend_from_slice(&[0x40, 0, 0, 0, 0xbf, 0xff]);
        fs::write(&log, &data).unwrap();
        let database = Database::open(&path).unwrap();
        assert_eq!(database.scan("c").unwrap().len(), 30);
        drop(database);
        assert_eq!(fs::metadata(&log).unwrap().len() as usize, data.len() - 6);
        assert_eq!(Database::open(&path).unwrap().scan("c").unwrap().len(), 30);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn limit_errors_match() {
        let (bytes, starts) = sample_log(40);
        // Frame too large for the options: detected at scan time, after earlier frames applied.
        let mut big = bytes[..starts[20]].to_vec();
        big.extend(frame(&[put("c", "big", json!("y".repeat(5000)))]));
        big.extend_from_slice(&bytes[starts[20]..]);
        let options = Options {
            max_frame_bytes: 1000,
            ..Options::default()
        };
        let result = assert_same(&big, options);
        assert!(result.starts_with("err LimitExceeded"), "{result}");
        // Document too large (frame fits), and state too large (state-dependent, finish step).
        let options = Options {
            max_document_bytes: 4000,
            ..Options::default()
        };
        let result = assert_same(&big, options);
        assert!(result.starts_with("err LimitExceeded"), "{result}");
        let options = Options {
            max_state_bytes: 600,
            ..Options::default()
        };
        let result = assert_same(&bytes, options);
        assert!(result.contains("committed state is too large"), "{result}");
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }
    }

    #[test]
    fn seeded_mutations_agree_with_the_sequential_reference() {
        let (bytes, _) = sample_log(24);
        let tight = Options {
            max_frame_bytes: 200,
            max_document_bytes: 64,
            max_state_bytes: 700,
        };
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let (mut oks, mut errs) = (0, 0);
        for round in 0..600 {
            let mut mutated = bytes.clone();
            for _ in 0..1 + rng.below(3) {
                match rng.below(4) {
                    0 => {
                        let at = rng.below(mutated.len());
                        mutated[at] ^= 1 << rng.below(8);
                    }
                    1 => {
                        let keep = HEADER.len() + rng.below(mutated.len() - HEADER.len());
                        mutated.truncate(keep);
                    }
                    2 => {
                        let at = HEADER.len() + rng.below(mutated.len() - HEADER.len());
                        mutated[at] = rng.next() as u8;
                    }
                    _ => {
                        for _ in 0..rng.below(20) {
                            mutated.push(rng.next() as u8);
                        }
                    }
                }
                if mutated.len() <= HEADER.len() {
                    break;
                }
            }
            let options = if round % 5 == 0 {
                tight
            } else {
                Options::default()
            };
            let result = outcome(&mutated, options, false, 1);
            if result.starts_with("ok ") {
                oks += 1
            } else {
                errs += 1
            }
            for workers in [2, 8] {
                assert_eq!(
                    outcome(&mutated, options, true, workers),
                    result,
                    "round {round}"
                );
            }
        }
        assert!(oks > 20 && errs > 20, "ok={oks} err={errs}");
    }

    #[test]
    fn window_byte_budget_splits_without_changing_the_result() {
        // Frames large enough that the 16 MiB byte budget forces several windows.
        let mut bytes = HEADER.to_vec();
        for index in 0..10 {
            let text = "z".repeat(900_000);
            let ops: Vec<Value> = (0..4)
                .map(|part| put("big", &format!("k{index}-{part}"), json!(text)))
                .collect();
            bytes.extend(frame(&ops));
        }
        let options = Options {
            max_state_bytes: usize::MAX / 2,
            ..Options::default()
        };
        let result = assert_same(&bytes, options);
        assert!(result.starts_with("ok "), "{result}");
    }
}
