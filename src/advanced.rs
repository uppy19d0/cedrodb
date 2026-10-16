use super::*;

/// A committed operation in its original transaction order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChangeOperation {
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

impl From<Operation> for ChangeOperation {
    fn from(operation: Operation) -> Self {
        match operation {
            Operation::Put {
                collection,
                key,
                value,
            } => Self::Put {
                collection,
                key,
                value,
            },
            Operation::Delete { collection, key } => Self::Delete { collection, key },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeTransaction {
    /// Sequence number of a complete durable frame, starting at one.
    pub cursor: u64,
    pub operations: Vec<ChangeOperation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangePage {
    pub transactions: Vec<ChangeTransaction>,
    pub next_cursor: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub version: String,
    pub records: u64,
    pub collections: u64,
    /// Encoded live document + collection/key bytes; excludes indexes/allocator.
    pub content_bytes: u64,
    pub log_bytes: u64,
    pub latest_cursor: u64,
    pub indexes: u64,
}

impl Database {
    pub fn put(&self, collection: &str, key: &str, value: Value) -> Result<()> {
        let mut transaction = self.begin_write()?;
        transaction.put(collection, key, value)?;
        transaction.commit()
    }

    pub fn delete(&self, collection: &str, key: &str) -> Result<()> {
        let mut transaction = self.begin_write()?;
        transaction.delete(collection, key)?;
        transaction.commit()
    }

    /// Compare a complete document and persist its replacement atomically.
    /// `None` means absent/deletion; `Some(Value::Null)` is a stored JSON null.
    /// Numeric integer/float equality is recursive and exact within +/-2^53.
    /// A mismatch writes nothing. An active staged writer returns WriterBusy.
    pub fn compare_exchange(
        &self,
        collection: &str,
        key: &str,
        expected: Option<Value>,
        replacement: Option<Value>,
    ) -> Result<bool> {
        validate_user_location(collection, key)?;
        if let Some(value) = &replacement {
            validate_document(value, self.options)?;
        }
        let mut inner = self.lock()?;
        if inner.writer_active {
            return Err(Error::WriterBusy);
        }
        let current = inner
            .state
            .collections
            .get(collection)
            .and_then(|documents| documents.get(key))
            .map(|document| &document.value);
        let matches = match (current, expected.as_ref()) {
            (None, None) => true,
            (Some(current), Some(expected)) => query::values_equal(current, expected),
            _ => false,
        };
        if !matches {
            return Ok(false);
        }
        // An absent -> absent conditional delete is a successful no-op.
        if current.is_none() && replacement.is_none() {
            return Ok(true);
        }
        let operation = match replacement {
            Some(value) => Operation::Put {
                collection: collection.into(),
                key: key.into(),
                value,
            },
            None => Operation::Delete {
                collection: collection.into(),
                key: key.into(),
            },
        };
        commit_operations(&mut inner, &[operation], self.options)?;
        Ok(true)
    }

    /// Replay complete transactions after an exclusive durable cursor.
    /// Reads only requested frames using an eight-byte in-memory offset per
    /// transaction. Limit is transaction count (1..=1000), not operation count.
    /// One page contains at most max_frame_bytes + 12 encoded frame bytes;
    /// complete frames are returned and at least one available frame fits.
    /// Decoded JSON/allocator overhead is additional. No retention/compaction.
    /// A cursor belongs to this database history and must be <= latest_cursor.
    pub fn changes(&self, since: u64, limit: usize) -> Result<ChangePage> {
        if !(1..=1000).contains(&limit) {
            return Err(Error::InvalidInput(
                "changefeed limit must be 1..=1000".into(),
            ));
        }
        let mut inner = self.lock()?;
        let total = inner.frame_offsets.len();
        if since > total as u64 {
            return Err(Error::InvalidInput(
                "changefeed cursor exceeds latest committed cursor".into(),
            ));
        }
        let start = since as usize;
        let end = start.saturating_add(limit).min(total);
        let mut transactions = Vec::with_capacity(end - start);
        let log_bytes = inner.file.metadata()?.len();
        let byte_budget = self.options.max_frame_bytes as u64 + FRAME_HEADER_BYTES as u64;
        let mut page_bytes = 0u64;
        for index in start..end {
            let offset = inner.frame_offsets[index];
            let next_offset = inner
                .frame_offsets
                .get(index + 1)
                .copied()
                .unwrap_or(log_bytes);
            let frame_bytes = next_offset.checked_sub(offset).ok_or_else(|| {
                Error::Corruption("changefeed frame offsets are out of order".into())
            })?;
            if !transactions.is_empty() && frame_bytes > byte_budget.saturating_sub(page_bytes) {
                break;
            }
            let batch = read_batch(&mut inner.file, offset, self.options)?;
            page_bytes += frame_bytes;
            transactions.push(ChangeTransaction {
                cursor: index as u64 + 1,
                operations: batch.ops.into_iter().map(ChangeOperation::from).collect(),
            });
        }
        let next_cursor = since + transactions.len() as u64;
        Ok(ChangePage {
            transactions,
            next_cursor,
            has_more: next_cursor < total as u64,
        })
    }

    pub fn stats(&self) -> Result<Stats> {
        let inner = self.lock()?;
        stats_from_inner(&inner)
    }
}

pub(super) fn stats_from_inner(inner: &Inner) -> Result<Stats> {
    Ok(Stats {
        version: env!("CARGO_PKG_VERSION").into(),
        records: inner
            .state
            .collections
            .values()
            .map(|documents| documents.len() as u64)
            .sum(),
        collections: inner.state.collections.len() as u64,
        content_bytes: inner.state.content_bytes as u64,
        log_bytes: inner.file.metadata()?.len(),
        latest_cursor: inner.frame_offsets.len() as u64,
        indexes: inner.state.indexes.len() as u64,
    })
}

fn read_batch(file: &mut File, offset: u64, options: Options) -> Result<Batch> {
    file.seek(SeekFrom::Start(offset))?;
    let mut header = [0u8; FRAME_HEADER_BYTES];
    file.read_exact(&mut header)?;
    let length = u32::from_le_bytes(header[..4].try_into().unwrap());
    let inverted = u32::from_le_bytes(header[4..8].try_into().unwrap());
    if inverted != !length
        || (length as usize) < EMPTY_BATCH_BYTES
        || length as usize > options.max_frame_bytes
    {
        return Err(Error::Corruption(format!(
            "invalid changefeed frame at byte {offset}"
        )));
    }
    let checksum = u32::from_le_bytes(header[8..12].try_into().unwrap());
    let mut payload = vec![0; length as usize];
    file.read_exact(&mut payload)?;
    if crc32fast::hash(&payload) != checksum {
        return Err(Error::Corruption(format!(
            "changefeed checksum mismatch at byte {offset}"
        )));
    }
    serde_json::from_slice(&payload).map_err(|error| {
        Error::Corruption(format!(
            "invalid changefeed transaction at byte {offset}: {error}"
        ))
    })
}
