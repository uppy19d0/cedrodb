use super::*;

/// Preconditions compare committed documents before any operation in the batch.
/// Absent is distinct from Equals { value: Value::Null, .. }.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    Absent {
        collection: String,
        key: String,
    },
    Equals {
        collection: String,
        key: String,
        value: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionalResult {
    pub applied: bool,
    /// Zero-based index of the first unmatched precondition.
    pub failed_condition: Option<usize>,
    /// Latest durable cursor after success, or the unchanged cursor on mismatch.
    pub cursor: u64,
}

impl Database {
    /// Check every precondition and commit the whole batch under one mutex.
    /// A mismatch writes nothing; this does not provide a read snapshot or MVCC.
    /// Include every document your decision depends on in the preconditions.
    pub fn conditional_batch(
        &self,
        conditions: &[Condition],
        operations: &[ChangeOperation],
    ) -> Result<ConditionalResult> {
        if conditions.len() > 64 {
            return Err(Error::InvalidInput(
                "a conditional batch supports at most 64 conditions".into(),
            ));
        }
        for condition in conditions {
            match condition {
                Condition::Absent { collection, key } => validate_location(collection, key)?,
                Condition::Equals {
                    collection,
                    key,
                    value,
                } => {
                    validate_location(collection, key)?;
                    validate_document(value, self.options)?;
                }
            }
        }
        let mut prepared_operations = Vec::new();
        let mut encoded_bytes = EMPTY_BATCH_BYTES;
        for operation in operations {
            match operation {
                ChangeOperation::Put {
                    collection,
                    key,
                    value,
                } => {
                    validate_location(collection, key)?;
                    validate_document(value, self.options)?;
                }
                ChangeOperation::Delete { collection, key } => validate_location(collection, key)?,
            }
            let comma_bytes = usize::from(!prepared_operations.is_empty());
            let remaining = self
                .options
                .max_frame_bytes
                .checked_sub(encoded_bytes)
                .and_then(|remaining| remaining.checked_sub(comma_bytes))
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
            encoded_bytes += comma_bytes + counter.bytes;
            // Clone only after the operation is known to fit. At most one
            // configured frame's encoded content is duplicated for staging.
            prepared_operations.push(match operation {
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
        for (index, condition) in conditions.iter().enumerate() {
            let (collection, key) = match condition {
                Condition::Absent { collection, key }
                | Condition::Equals {
                    collection, key, ..
                } => (collection, key),
            };
            let current = inner
                .state
                .collections
                .get(collection)
                .and_then(|documents| documents.get(key));
            let matches = match (condition, current) {
                (Condition::Absent { .. }, None) => true,
                (Condition::Equals { value, .. }, Some(document)) => {
                    query::values_equal(&document.value, value)
                }
                _ => false,
            };
            if !matches {
                return Ok(ConditionalResult {
                    applied: false,
                    failed_condition: Some(index),
                    cursor: inner.frame_offsets.len() as u64,
                });
            }
        }
        commit_operations(&mut inner, &prepared_operations, self.options)?;
        Ok(ConditionalResult {
            applied: true,
            failed_condition: None,
            cursor: inner.frame_offsets.len() as u64,
        })
    }
}
