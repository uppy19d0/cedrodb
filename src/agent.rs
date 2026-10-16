//! Revision-fenced checkpoint storage for application-managed agents.
//!
//! A checkpoint and its ordinary document/outbox writes share one durable frame.
//! This module does not execute workflows, resume queues, or authorize tools.

use super::*;

pub(crate) const AGENT_PREFIX: &str = "_cedro.agents.";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Running,
    Waiting,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCheckpoint {
    pub schema_version: u32,
    pub thread_id: String,
    pub revision: u64,
    pub status: AgentStatus,
    pub state: Value,
    pub metadata: Value,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CheckpointOptions {
    /// None creates only if absent; Some fences against the committed revision.
    pub expected_revision: Option<u64>,
    pub now_ms: u64,
    pub status: AgentStatus,
    pub metadata: Value,
    pub operations: Vec<ChangeOperation>,
}

impl Default for CheckpointOptions {
    fn default() -> Self {
        Self {
            expected_revision: None,
            now_ms: 0,
            status: AgentStatus::Running,
            metadata: Value::Object(Default::default()),
            operations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointResult {
    pub applied: bool,
    pub cursor: u64,
    /// The new checkpoint on success, or the committed checkpoint on conflict.
    pub checkpoint: Option<AgentCheckpoint>,
}

fn collection_name(namespace: &str) -> Result<String> {
    if namespace.is_empty()
        || namespace.len() > 128
        || !namespace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(Error::InvalidInput(
            "agent namespace must contain 1..=128 ASCII letters, digits, underscores, dashes, or dots"
                .into(),
        ));
    }
    Ok(format!("{AGENT_PREFIX}{namespace}"))
}

fn validate_thread(collection: &str, thread_id: &str) -> Result<()> {
    validate_location(collection, thread_id)?;
    if thread_id.contains('\0') {
        return Err(Error::InvalidInput(
            "agent thread id must not contain NUL".into(),
        ));
    }
    Ok(())
}

fn validate_safe_number(value: u64, label: &str) -> Result<()> {
    if value > MAX_SAFE_INTEGER {
        return Err(Error::InvalidInput(format!(
            "{label} must be at most 2^53-1"
        )));
    }
    Ok(())
}

fn validate_portable_json(value: &Value) -> Result<()> {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(number) => {
                let safe = if let Some(value) = number.as_i64() {
                    value.unsigned_abs() <= MAX_SAFE_INTEGER
                } else if let Some(value) = number.as_u64() {
                    value <= MAX_SAFE_INTEGER
                } else {
                    number.as_f64().is_some_and(|value| {
                        value.is_finite() && value.abs() <= MAX_SAFE_INTEGER as f64
                    })
                };
                if !safe {
                    return Err(Error::InvalidInput(
                        "checkpoint state and metadata numbers must be within +/-(2^53-1)".into(),
                    ));
                }
            }
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            _ => {}
        }
    }
    Ok(())
}

fn read_checkpoint(
    thread_id: &str,
    document: &Document,
    latest_cursor: u64,
) -> Result<AgentCheckpoint> {
    let invalid = |reason: &dyn fmt::Display| {
        Error::Corruption(format!("invalid agent checkpoint {thread_id:?}: {reason}"))
    };
    let checkpoint: AgentCheckpoint =
        serde_json::from_value(document.value.clone()).map_err(|error| invalid(&error))?;
    if checkpoint.schema_version != 1
        || checkpoint.thread_id != thread_id
        || checkpoint.revision == 0
        || checkpoint.revision > latest_cursor
        || !checkpoint.metadata.is_object()
    {
        return Err(invalid(&"schema, thread, revision, or metadata is invalid"));
    }
    validate_safe_number(checkpoint.revision, "checkpoint revision")
        .map_err(|error| invalid(&error))?;
    validate_safe_number(checkpoint.updated_at, "checkpoint updated_at")
        .map_err(|error| invalid(&error))?;
    validate_portable_json(&checkpoint.state).map_err(|error| invalid(&error))?;
    validate_portable_json(&checkpoint.metadata).map_err(|error| invalid(&error))?;
    Ok(checkpoint)
}

fn encoded_operation_size(
    operation: &impl Serialize,
    encoded_bytes: usize,
    has_previous: bool,
    options: Options,
) -> Result<usize> {
    let comma_bytes = usize::from(has_previous);
    let remaining = options
        .max_frame_bytes
        .checked_sub(encoded_bytes)
        .and_then(|bytes| bytes.checked_sub(comma_bytes))
        .ok_or_else(|| Error::LimitExceeded("transaction frame is too large".into()))?;
    let mut counter = EncodedCounter {
        bytes: 0,
        limit: remaining,
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut counter, operation);
    if counter.exceeded {
        return Err(Error::LimitExceeded(
            "transaction frame is too large".into(),
        ));
    }
    result?;
    Ok(encoded_bytes + comma_bytes + counter.bytes)
}

impl Database {
    pub fn agent_get(&self, namespace: &str, thread_id: &str) -> Result<Option<AgentCheckpoint>> {
        let collection = collection_name(namespace)?;
        validate_thread(&collection, thread_id)?;
        let inner = self.lock()?;
        inner
            .state
            .collections
            .get(&collection)
            .and_then(|checkpoints| checkpoints.get(thread_id))
            .map(|document| read_checkpoint(thread_id, document, inner.frame_offsets.len() as u64))
            .transpose()
    }

    /// Save state and ordinary document/outbox operations under one revision
    /// comparison and one commit. A conflict changes nothing. Status is stored
    /// application state; the caller owns execution, approvals, and transitions.
    pub fn agent_checkpoint(
        &self,
        namespace: &str,
        thread_id: &str,
        state: Value,
        options: CheckpointOptions,
    ) -> Result<CheckpointResult> {
        let collection = collection_name(namespace)?;
        validate_thread(&collection, thread_id)?;
        validate_safe_number(options.now_ms, "now_ms")?;
        if let Some(revision) = options.expected_revision {
            validate_safe_number(revision, "expected_revision")?;
            if revision == 0 {
                return Err(Error::InvalidInput(
                    "expected_revision must be positive".into(),
                ));
            }
        }
        if !options.metadata.is_object() {
            return Err(Error::InvalidInput(
                "checkpoint metadata must be an object".into(),
            ));
        }
        validate_document(&state, self.options)?;
        validate_document(&options.metadata, self.options)?;
        validate_portable_json(&state)?;
        validate_portable_json(&options.metadata)?;

        let mut operations = Vec::new();
        let mut encoded_bytes = EMPTY_BATCH_BYTES;
        for operation in &options.operations {
            match operation {
                ChangeOperation::Put {
                    collection,
                    key,
                    value,
                } => {
                    validate_user_location(collection, key)?;
                    validate_document(value, self.options)?;
                }
                ChangeOperation::Delete { collection, key } => {
                    validate_user_location(collection, key)?;
                }
            }
            encoded_bytes = encoded_operation_size(
                operation,
                encoded_bytes,
                !operations.is_empty(),
                self.options,
            )?;
            // Duplicate only values whose combined representation fits a frame.
            operations.push(match operation {
                ChangeOperation::Put {
                    collection,
                    key,
                    value,
                } => Operation::Put {
                    collection: collection.clone(),
                    key: key.clone(),
                    value: value.clone(),
                },
                ChangeOperation::Delete { collection, key } => Operation::Delete {
                    collection: collection.clone(),
                    key: key.clone(),
                },
            });
        }
        let mut inner = self.lock()?;
        if inner.writer_active {
            return Err(Error::WriterBusy);
        }
        let cursor = inner.frame_offsets.len() as u64;
        let current = inner
            .state
            .collections
            .get(&collection)
            .and_then(|checkpoints| checkpoints.get(thread_id))
            .map(|document| read_checkpoint(thread_id, document, cursor))
            .transpose()?;
        let revision = cursor
            .checked_add(1)
            .filter(|revision| *revision <= MAX_SAFE_INTEGER)
            .ok_or_else(|| Error::LimitExceeded("checkpoint revision exceeds 2^53-1".into()))?;
        let checkpoint = AgentCheckpoint {
            schema_version: 1,
            thread_id: thread_id.into(),
            revision,
            status: options.status,
            state,
            metadata: options.metadata,
            updated_at: options.now_ms,
        };
        // Count before constructing the additional JSON tree. This also bounds
        // the envelope's encoded allocation when state and metadata each fit.
        let mut counter = EncodedCounter {
            bytes: 0,
            limit: self.options.max_document_bytes,
            exceeded: false,
        };
        let result = serde_json::to_writer(&mut counter, &checkpoint);
        if counter.exceeded {
            return Err(Error::LimitExceeded("JSON document is too large".into()));
        }
        result?;
        let value = serde_json::to_value(&checkpoint)?;
        validate_document(&value, self.options)?;
        let checkpoint_operation = Operation::Put {
            collection,
            key: thread_id.into(),
            value,
        };
        encoded_operation_size(
            &checkpoint_operation,
            encoded_bytes,
            !operations.is_empty(),
            self.options,
        )?;
        operations.insert(0, checkpoint_operation);

        if current.as_ref().map(|checkpoint| checkpoint.revision) != options.expected_revision {
            return Ok(CheckpointResult {
                applied: false,
                cursor,
                checkpoint: current,
            });
        }
        commit_operations(&mut inner, &operations, self.options)?;
        Ok(CheckpointResult {
            applied: true,
            cursor: revision,
            checkpoint: Some(checkpoint),
        })
    }
}
