//! Thin N-API bridge. Persistence, queries, indexes and concurrency decisions
//! belong to the shared CedroDB Rust engine, never to a JavaScript copy.
use cedrodb::{
    AgentStatus, ChangeOperation, CheckpointOptions, Condition, Database, EnqueueOptions, Options,
    Query, SearchQuery,
};
use napi::{Error, Result, Status};
use napi_derive::napi;
use serde::Deserialize;
use serde_json::Value;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn invalid(message: impl Into<String>) -> Error {
    Error::new(Status::InvalidArg, message.into())
}

fn engine_error(error: cedrodb::Error) -> Error {
    Error::new(Status::GenericFailure, error.to_string())
}

fn decode<T: serde::de::DeserializeOwned>(json: &str) -> Result<T> {
    serde_json::from_str(json).map_err(|error| invalid(error.to_string()))
}

// Databases can also be written by Rust and Python. Reject unsafe numbers on
// reads rather than letting JSON.parse silently round integers from those APIs.
fn encode<T: serde::Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value).map_err(|error| invalid(error.to_string()))?;
    validate_portable_json(&value)?;
    serde_json::to_string(&value).map_err(|error| invalid(error.to_string()))
}

fn validate_portable_json(value: &Value) -> Result<()> {
    match value {
        Value::Number(number) => {
            let unsafe_number = if let Some(value) = number.as_i64() {
                value.unsigned_abs() > MAX_SAFE_INTEGER
            } else if let Some(value) = number.as_u64() {
                value > MAX_SAFE_INTEGER
            } else {
                number
                    .as_f64()
                    .is_none_or(|value| !value.is_finite() || value.abs() > MAX_SAFE_INTEGER as f64)
            };
            if unsafe_number {
                return Err(invalid(
                    "number exceeds JavaScript's safe numeric range; store large identifiers as strings",
                ));
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_portable_json(value)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_portable_json(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingOptions {
    max_document_bytes: Option<usize>,
    max_frame_bytes: Option<usize>,
    max_state_bytes: Option<usize>,
}

fn binding_options(options_json: &str) -> Result<Options> {
    let input: BindingOptions = decode(options_json)?;
    let defaults = Options::default();
    Ok(Options {
        max_document_bytes: input
            .max_document_bytes
            .unwrap_or(defaults.max_document_bytes),
        max_frame_bytes: input.max_frame_bytes.unwrap_or(defaults.max_frame_bytes),
        max_state_bytes: input.max_state_bytes.unwrap_or(defaults.max_state_bytes),
    })
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum BatchOperation {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConditionalRequest {
    conditions: Vec<Condition>,
    operations: Vec<ChangeOperation>,
}

// Shared JSON protocol with the Python binding. Defaults are resolved by the
// language wrapper; every field here is required and unknown fields are errors.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum QueueRequest {
    Enqueue {
        name: String,
        id: String,
        payload: Value,
        now_ms: u64,
        delay_ms: u64,
        max_attempts: u32,
        priority: u32,
    },
    Claim {
        name: String,
        worker: String,
        now_ms: u64,
        lease_ms: u64,
    },
    Ack {
        name: String,
        id: String,
        token: String,
        now_ms: u64,
        operations: Vec<ChangeOperation>,
    },
    Fail {
        name: String,
        id: String,
        token: String,
        now_ms: u64,
        error: String,
        retry_delay_ms: u64,
    },
    Extend {
        name: String,
        id: String,
        token: String,
        now_ms: u64,
        lease_ms: u64,
    },
    Get {
        name: String,
        id: String,
    },
    Stats {
        name: String,
        now_ms: u64,
    },
    RecoverExpired {
        name: String,
        now_ms: u64,
        limit: usize,
    },
    RetryDead {
        name: String,
        id: String,
        now_ms: u64,
    },
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum AgentRequest {
    Get {
        namespace: String,
        thread_id: String,
    },
    Checkpoint {
        namespace: String,
        thread_id: String,
        state: Value,
        expected_revision: Option<u64>,
        now_ms: u64,
        status: AgentStatus,
        metadata: Value,
        operations: Vec<ChangeOperation>,
    },
}

#[napi]
pub struct NativeDatabase {
    database: Option<Database>,
}

impl NativeDatabase {
    fn database(&self) -> Result<&Database> {
        self.database
            .as_ref()
            .ok_or_else(|| Error::new(Status::GenericFailure, "CedroDB database is closed"))
    }
}

#[napi]
impl NativeDatabase {
    #[napi(constructor)]
    pub fn new(path: String, options_json: String) -> Result<Self> {
        Ok(Self {
            database: Some(
                Database::open_with_options(path, binding_options(&options_json)?)
                    .map_err(engine_error)?,
            ),
        })
    }

    #[napi]
    pub fn restore_backup(
        backup_path: String,
        destination_path: String,
        options_json: String,
    ) -> Result<String> {
        encode(
            &Database::restore_backup_with_options(
                backup_path,
                destination_path,
                binding_options(&options_json)?,
            )
            .map_err(engine_error)?,
        )
    }

    #[napi]
    pub fn close(&mut self) {
        // Deterministic drop releases the OS file lock immediately. Garbage
        // collection remains a fallback, not a requirement for reopening.
        self.database.take();
    }

    #[napi]
    pub fn get(&self, collection: String, key: String) -> Result<Option<String>> {
        self.database()?
            .get(&collection, &key)
            .map_err(engine_error)?
            .map(|value| encode(&value))
            .transpose()
    }

    #[napi]
    pub fn batch(&self, operations_json: String) -> Result<()> {
        let operations: Vec<BatchOperation> = decode(&operations_json)?;
        let mut transaction = self.database()?.begin_write().map_err(engine_error)?;
        for operation in operations {
            match operation {
                BatchOperation::Put {
                    collection,
                    key,
                    value,
                } => {
                    validate_portable_json(&value)?;
                    transaction
                        .put(&collection, &key, value)
                        .map_err(engine_error)?;
                }
                BatchOperation::Delete { collection, key } => {
                    transaction
                        .delete(&collection, &key)
                        .map_err(engine_error)?;
                }
            }
        }
        transaction.commit().map_err(engine_error)
    }

    #[napi]
    pub fn conditional_batch_json(&self, request_json: String) -> Result<String> {
        let request: ConditionalRequest = decode(&request_json)?;
        for condition in &request.conditions {
            if let Condition::Equals { value, .. } = condition {
                validate_portable_json(value)?;
            }
        }
        for operation in &request.operations {
            if let ChangeOperation::Put { value, .. } = operation {
                validate_portable_json(value)?;
            }
        }
        encode(
            &self
                .database()?
                .conditional_batch(&request.conditions, &request.operations)
                .map_err(engine_error)?,
        )
    }

    #[napi]
    pub fn queue_json(&self, request_json: String) -> Result<String> {
        let value: Value = decode(&request_json)?;
        // Validate all numeric input before invoking any mutating engine API,
        // including callers of the raw bridge rather than the public wrapper.
        validate_portable_json(&value)?;
        let request: QueueRequest =
            serde_json::from_value(value).map_err(|error| invalid(error.to_string()))?;
        let database = self.database()?;
        match request {
            QueueRequest::Enqueue {
                name,
                id,
                payload,
                now_ms,
                delay_ms,
                max_attempts,
                priority,
            } => encode(
                &database
                    .queue_enqueue(
                        &name,
                        &id,
                        payload,
                        EnqueueOptions {
                            now_ms,
                            delay_ms,
                            max_attempts,
                            priority,
                        },
                    )
                    .map_err(engine_error)?,
            ),
            QueueRequest::Claim {
                name,
                worker,
                now_ms,
                lease_ms,
            } => encode(
                &database
                    .queue_claim(&name, &worker, now_ms, lease_ms)
                    .map_err(engine_error)?,
            ),
            QueueRequest::Ack {
                name,
                id,
                token,
                now_ms,
                operations,
            } => encode(
                &database
                    .queue_ack(&name, &id, &token, now_ms, &operations)
                    .map_err(engine_error)?,
            ),
            QueueRequest::Fail {
                name,
                id,
                token,
                now_ms,
                error,
                retry_delay_ms,
            } => encode(
                &database
                    .queue_fail(&name, &id, &token, now_ms, &error, retry_delay_ms)
                    .map_err(engine_error)?,
            ),
            QueueRequest::Extend {
                name,
                id,
                token,
                now_ms,
                lease_ms,
            } => encode(
                &database
                    .queue_extend(&name, &id, &token, now_ms, lease_ms)
                    .map_err(engine_error)?,
            ),
            QueueRequest::Get { name, id } => {
                encode(&database.queue_get(&name, &id).map_err(engine_error)?)
            }
            QueueRequest::Stats { name, now_ms } => {
                encode(&database.queue_stats(&name, now_ms).map_err(engine_error)?)
            }
            QueueRequest::RecoverExpired {
                name,
                now_ms,
                limit,
            } => encode(
                &database
                    .queue_recover_expired(&name, now_ms, limit)
                    .map_err(engine_error)?,
            ),
            QueueRequest::RetryDead { name, id, now_ms } => encode(
                &database
                    .queue_retry_dead(&name, &id, now_ms)
                    .map_err(engine_error)?,
            ),
        }
    }

    #[napi]
    pub fn find_page(&self, collection: String, query_json: String) -> Result<String> {
        let query: Query = decode(&query_json)?;
        for filter in &query.filters {
            validate_portable_json(&filter.value)?;
        }
        encode(
            &self
                .database()?
                .query(&collection, query)
                .map_err(engine_error)?,
        )
    }

    #[napi]
    pub fn search_json(&self, collection: String, query_json: String) -> Result<String> {
        let value: Value = decode(&query_json)?;
        validate_portable_json(&value)?;
        let query: SearchQuery =
            serde_json::from_value(value).map_err(|error| invalid(error.to_string()))?;
        encode(
            &self
                .database()?
                .search(&collection, query)
                .map_err(engine_error)?,
        )
    }

    #[napi]
    pub fn profile_query_json(&self, collection: String, query_json: String) -> Result<String> {
        let query: Query = decode(&query_json)?;
        for filter in &query.filters {
            validate_portable_json(&filter.value)?;
        }
        encode(
            &self
                .database()?
                .profile_query(&collection, query)
                .map_err(engine_error)?,
        )
    }

    #[napi]
    pub fn profile_search_json(&self, collection: String, query_json: String) -> Result<String> {
        let value: Value = decode(&query_json)?;
        validate_portable_json(&value)?;
        let query: SearchQuery =
            serde_json::from_value(value).map_err(|error| invalid(error.to_string()))?;
        encode(
            &self
                .database()?
                .profile_search(&collection, query)
                .map_err(engine_error)?,
        )
    }

    #[napi]
    pub fn agent_json(&self, request_json: String) -> Result<String> {
        let value: Value = decode(&request_json)?;
        validate_portable_json(&value)?;
        let request: AgentRequest =
            serde_json::from_value(value).map_err(|error| invalid(error.to_string()))?;
        let database = self.database()?;
        match request {
            AgentRequest::Get {
                namespace,
                thread_id,
            } => encode(
                &database
                    .agent_get(&namespace, &thread_id)
                    .map_err(engine_error)?,
            ),
            AgentRequest::Checkpoint {
                namespace,
                thread_id,
                state,
                expected_revision,
                now_ms,
                status,
                metadata,
                operations,
            } => {
                let options = CheckpointOptions {
                    expected_revision,
                    now_ms,
                    status,
                    metadata,
                    operations,
                };
                encode(
                    &database
                        .agent_checkpoint(&namespace, &thread_id, state, options)
                        .map_err(engine_error)?,
                )
            }
        }
    }

    #[napi]
    pub fn create_index(&self, collection: String, field: String) -> Result<()> {
        self.database()?
            .create_index(&collection, &field)
            .map_err(engine_error)
    }

    #[napi]
    pub fn compare_exchange(
        &self,
        collection: String,
        key: String,
        expected_json: Option<String>,
        replacement_json: Option<String>,
    ) -> Result<bool> {
        let expected: Option<Value> = expected_json.as_deref().map(decode).transpose()?;
        let replacement: Option<Value> = replacement_json.as_deref().map(decode).transpose()?;
        for value in expected.iter().chain(replacement.iter()) {
            validate_portable_json(value)?;
        }
        self.database()?
            .compare_exchange(&collection, &key, expected, replacement)
            .map_err(engine_error)
    }

    #[napi]
    pub fn changes(&self, since: String, limit: u32) -> Result<String> {
        let since: u64 = since
            .parse()
            .map_err(|_| invalid("invalid change cursor"))?;
        encode(
            &self
                .database()?
                .changes(since, limit as usize)
                .map_err(engine_error)?,
        )
    }

    #[napi]
    pub fn stats(&self) -> Result<String> {
        encode(&self.database()?.stats().map_err(engine_error)?)
    }

    #[napi]
    pub fn diagnostics(&self) -> Result<String> {
        encode(&self.database()?.diagnostics().map_err(engine_error)?)
    }

    #[napi]
    pub fn verify(&self) -> Result<String> {
        encode(&self.database()?.verify().map_err(engine_error)?)
    }

    #[napi]
    pub fn backup(&self, path: String) -> Result<String> {
        encode(&self.database()?.backup(path).map_err(engine_error)?)
    }

    #[napi]
    pub fn aggregate(&self, collection: String, field: String) -> Result<String> {
        encode(
            &self
                .database()?
                .aggregate(&collection, &field)
                .map_err(engine_error)?,
        )
    }
}
