//! Native calls only: Python handles its public JSON ergonomics, while every
//! database operation, transaction and recovery runs in the shared Rust core.

use cedrodb::{
    AgentStatus, ChangeOperation, CheckpointOptions, Condition, Database, EnqueueOptions, Options,
    Query, SearchQuery,
};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Mutex;

pyo3::create_exception!(_native, CedroError, PyRuntimeError);
pyo3::create_exception!(_native, DatabaseClosedError, CedroError);

fn engine_error(error: cedrodb::Error) -> PyErr {
    CedroError::new_err(error.to_string())
}

fn parse<T: for<'de> Deserialize<'de>>(encoded: &str) -> PyResult<T> {
    serde_json::from_str(encoded).map_err(|error| PyValueError::new_err(error.to_string()))
}

fn encode(value: &impl serde::Serialize) -> PyResult<String> {
    serde_json::to_string(value).map_err(|error| PyValueError::new_err(error.to_string()))
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
        error: String,
        now_ms: u64,
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

#[pyclass(frozen, module = "cedrodb._native")]
struct NativeDatabase {
    // Closing takes ownership and drops the actual core, releasing its OS lock.
    // All calls hold this mutex, so close cannot invalidate an in-flight call.
    inner: Mutex<Option<Database>>,
}

impl NativeDatabase {
    fn with_database<T>(&self, call: impl FnOnce(&Database) -> PyResult<T>) -> PyResult<T> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| CedroError::new_err("native database mutex is poisoned"))?;
        let database = guard
            .as_ref()
            .ok_or_else(|| DatabaseClosedError::new_err("database is closed"))?;
        call(database)
    }
}

#[pymethods]
impl NativeDatabase {
    #[new]
    #[pyo3(signature = (path, max_document_bytes=1_048_576, max_frame_bytes=16_777_216, max_state_bytes=67_108_864))]
    fn new(
        py: Python<'_>,
        path: &str,
        max_document_bytes: usize,
        max_frame_bytes: usize,
        max_state_bytes: usize,
    ) -> PyResult<Self> {
        let options = Options {
            max_document_bytes,
            max_frame_bytes,
            max_state_bytes,
        };
        let database =
            py.detach(|| Database::open_with_options(path, options).map_err(engine_error))?;
        Ok(Self {
            inner: Mutex::new(Some(database)),
        })
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            // Cleanup must remain possible after an unexpected Rust panic.
            let mut guard = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            drop(guard.take());
            Ok(())
        })
    }

    #[getter]
    fn closed(&self) -> PyResult<bool> {
        self.inner
            .lock()
            .map(|guard| guard.is_none())
            .map_err(|_| CedroError::new_err("native database mutex is poisoned"))
    }

    fn get_json(&self, py: Python<'_>, collection: &str, key: &str) -> PyResult<Option<String>> {
        py.detach(|| {
            self.with_database(|database| {
                database
                    .get(collection, key)
                    .map_err(engine_error)?
                    .as_ref()
                    .map(encode)
                    .transpose()
            })
        })
    }

    fn put_json(&self, py: Python<'_>, collection: &str, key: &str, value: &str) -> PyResult<()> {
        py.detach(|| {
            let value = parse(value)?;
            self.with_database(|database| {
                database.put(collection, key, value).map_err(engine_error)
            })
        })
    }

    fn delete(&self, py: Python<'_>, collection: &str, key: &str) -> PyResult<()> {
        py.detach(|| {
            self.with_database(|database| database.delete(collection, key).map_err(engine_error))
        })
    }

    fn batch_json(&self, py: Python<'_>, operations: &str) -> PyResult<()> {
        py.detach(|| {
            // Parse the whole payload before opening a transaction. Staging or
            // validation errors drop the transaction and leave no partial writes.
            let operations: Vec<BatchOperation> = parse(operations)?;
            self.with_database(|database| {
                let mut transaction = database.begin_write().map_err(engine_error)?;
                for operation in operations {
                    match operation {
                        BatchOperation::Put {
                            collection,
                            key,
                            value,
                        } => transaction.put(&collection, &key, value),
                        BatchOperation::Delete { collection, key } => {
                            transaction.delete(&collection, &key)
                        }
                    }
                    .map_err(engine_error)?;
                }
                transaction.commit().map_err(engine_error)
            })
        })
    }

    fn conditional_batch_json(&self, py: Python<'_>, request: &str) -> PyResult<String> {
        py.detach(|| {
            let request: ConditionalRequest = parse(request)?;
            self.with_database(|database| {
                encode(
                    &database
                        .conditional_batch(&request.conditions, &request.operations)
                        .map_err(engine_error)?,
                )
            })
        })
    }

    fn queue_json(&self, py: Python<'_>, request: &str) -> PyResult<String> {
        py.detach(|| {
            let request: QueueRequest = parse(request)?;
            self.with_database(|database| match request {
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
                    error,
                    now_ms,
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
            })
        })
    }

    #[pyo3(signature = (collection, key, expected, replacement))]
    fn compare_exchange_json(
        &self,
        py: Python<'_>,
        collection: &str,
        key: &str,
        expected: Option<&str>,
        replacement: Option<&str>,
    ) -> PyResult<bool> {
        py.detach(|| {
            let expected = expected.map(parse).transpose()?;
            let replacement = replacement.map(parse).transpose()?;
            self.with_database(|database| {
                database
                    .compare_exchange(collection, key, expected, replacement)
                    .map_err(engine_error)
            })
        })
    }

    fn query_json(&self, py: Python<'_>, collection: &str, query: &str) -> PyResult<String> {
        py.detach(|| {
            let query: Query = parse(query)?;
            self.with_database(|database| {
                encode(&database.query(collection, query).map_err(engine_error)?)
            })
        })
    }

    fn search_json(&self, py: Python<'_>, collection: &str, query: &str) -> PyResult<String> {
        py.detach(|| {
            let query: SearchQuery = parse(query)?;
            self.with_database(|database| {
                encode(&database.search(collection, query).map_err(engine_error)?)
            })
        })
    }

    fn diagnostics_json(&self, py: Python<'_>) -> PyResult<String> {
        py.detach(|| {
            self.with_database(|database| encode(&database.diagnostics().map_err(engine_error)?))
        })
    }

    fn profile_query_json(
        &self,
        py: Python<'_>,
        collection: &str,
        query: &str,
    ) -> PyResult<String> {
        py.detach(|| {
            let query: Query = parse(query)?;
            self.with_database(|database| {
                encode(
                    &database
                        .profile_query(collection, query)
                        .map_err(engine_error)?,
                )
            })
        })
    }

    fn profile_search_json(
        &self,
        py: Python<'_>,
        collection: &str,
        query: &str,
    ) -> PyResult<String> {
        py.detach(|| {
            let query: SearchQuery = parse(query)?;
            self.with_database(|database| {
                encode(
                    &database
                        .profile_search(collection, query)
                        .map_err(engine_error)?,
                )
            })
        })
    }

    fn agent_json(&self, py: Python<'_>, request: &str) -> PyResult<String> {
        py.detach(|| {
            let request: AgentRequest = parse(request)?;
            self.with_database(|database| match request {
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
                } => encode(
                    &database
                        .agent_checkpoint(
                            &namespace,
                            &thread_id,
                            state,
                            CheckpointOptions {
                                expected_revision,
                                now_ms,
                                status,
                                metadata,
                                operations,
                            },
                        )
                        .map_err(engine_error)?,
                ),
            })
        })
    }

    fn create_index(&self, py: Python<'_>, collection: &str, field: &str) -> PyResult<()> {
        py.detach(|| {
            self.with_database(|database| {
                database
                    .create_index(collection, field)
                    .map_err(engine_error)
            })
        })
    }

    fn aggregate_json(&self, py: Python<'_>, collection: &str, field: &str) -> PyResult<String> {
        py.detach(|| {
            self.with_database(|database| {
                encode(
                    &database
                        .aggregate(collection, field)
                        .map_err(engine_error)?,
                )
            })
        })
    }

    fn changes_json(&self, py: Python<'_>, since: u64, limit: usize) -> PyResult<String> {
        py.detach(|| {
            self.with_database(|database| {
                encode(&database.changes(since, limit).map_err(engine_error)?)
            })
        })
    }

    fn stats_json(&self, py: Python<'_>) -> PyResult<String> {
        py.detach(|| {
            self.with_database(|database| encode(&database.stats().map_err(engine_error)?))
        })
    }

    fn verify_json(&self, py: Python<'_>) -> PyResult<String> {
        py.detach(|| {
            self.with_database(|database| encode(&database.verify().map_err(engine_error)?))
        })
    }

    fn backup_json(&self, py: Python<'_>, path: &str) -> PyResult<String> {
        py.detach(|| {
            self.with_database(|database| encode(&database.backup(path).map_err(engine_error)?))
        })
    }

    #[staticmethod]
    #[pyo3(signature = (backup_path, destination_path, max_document_bytes=1_048_576, max_frame_bytes=16_777_216, max_state_bytes=67_108_864))]
    fn restore_backup_json(
        py: Python<'_>,
        backup_path: &str,
        destination_path: &str,
        max_document_bytes: usize,
        max_frame_bytes: usize,
        max_state_bytes: usize,
    ) -> PyResult<String> {
        py.detach(|| {
            let options = Options {
                max_document_bytes,
                max_frame_bytes,
                max_state_bytes,
            };
            encode(
                &Database::restore_backup_with_options(backup_path, destination_path, options)
                    .map_err(engine_error)?,
            )
        })
    }
}

#[pymodule]
mod _native {
    #[pymodule_export]
    use super::NativeDatabase;

    #[pymodule_init]
    fn init(module: &pyo3::Bound<'_, pyo3::types::PyModule>) -> pyo3::PyResult<()> {
        use pyo3::types::PyModuleMethods;
        module.add("CedroError", module.py().get_type::<super::CedroError>())?;
        module.add(
            "DatabaseClosedError",
            module.py().get_type::<super::DatabaseClosedError>(),
        )?;
        Ok(())
    }
}
