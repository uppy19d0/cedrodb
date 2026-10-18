use super::*;

pub(super) const QUEUE_PREFIX: &str = "_cedro.jobs.";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_QUEUE_NAMES_BYTES: usize = 128;
const MAX_WORKER_BYTES: usize = 128;
const MAX_ERROR_BYTES: usize = 4_096;
const MAX_TOKEN_BYTES: usize = MAX_KEY_BYTES + 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Leased,
    Done,
    Dead,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub schema_version: u32,
    pub id: String,
    pub payload: Value,
    pub status: JobStatus,
    pub attempts: u32,
    pub max_attempts: u32,
    pub priority: u32,
    pub created_at: u64,
    pub available_at: u64,
    pub lease_until: Option<u64>,
    pub lease_token: Option<String>,
    pub worker: Option<String>,
    pub last_error: Option<String>,
}

fn default_max_attempts() -> u32 {
    3
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnqueueOptions {
    pub now_ms: u64,
    #[serde(default)]
    pub delay_ms: u64,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default)]
    pub priority: u32,
}

impl Default for EnqueueOptions {
    fn default() -> Self {
        Self {
            now_ms: 0,
            delay_ms: 0,
            max_attempts: 3,
            priority: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnqueueResult {
    pub inserted: bool,
    pub job: Job,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueStats {
    pub queued: u64,
    pub leased: u64,
    pub done: u64,
    pub dead: u64,
    pub ready: u64,
    pub expired: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryResult {
    pub requeued: u64,
    pub dead: u64,
    pub has_more: bool,
}

fn queue_collection(name: &str) -> Result<String> {
    if name.is_empty()
        || name.len() > MAX_QUEUE_NAMES_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(Error::InvalidInput(
            "queue name must contain 1..=128 ASCII letters, digits, underscores, dashes, or dots"
                .into(),
        ));
    }
    Ok(format!("{QUEUE_PREFIX}{name}"))
}

fn safe_time(value: u64, label: &str) -> Result<()> {
    if value > MAX_SAFE_INTEGER {
        return Err(Error::InvalidInput(format!(
            "{label} must be at most 2^53-1"
        )));
    }
    Ok(())
}

fn add_time(now_ms: u64, duration_ms: u64, label: &str, positive: bool) -> Result<u64> {
    safe_time(now_ms, "now_ms")?;
    safe_time(duration_ms, label)?;
    if positive && duration_ms == 0 {
        return Err(Error::InvalidInput(format!("{label} must be positive")));
    }
    now_ms
        .checked_add(duration_ms)
        .filter(|value| *value <= MAX_SAFE_INTEGER)
        .ok_or_else(|| Error::InvalidInput("timestamp plus duration exceeds 2^53-1".into()))
}

fn validate_worker(worker: &str) -> Result<()> {
    if worker.is_empty() || worker.len() > MAX_WORKER_BYTES || worker.contains('\0') {
        return Err(Error::InvalidInput(
            "worker must contain 1..=128 UTF-8 bytes without NUL".into(),
        ));
    }
    Ok(())
}

fn corruption(id: &str, reason: impl fmt::Display) -> Error {
    Error::Corruption(format!("invalid queue job {id:?}: {reason}"))
}

fn validate_queue_key(collection: &str, id: &str) -> Result<()> {
    validate_location(collection, id)?;
    if id.contains('\0') {
        return Err(Error::InvalidInput("job id must not contain NUL".into()));
    }
    Ok(())
}

fn validate_token(token: &str) -> Result<()> {
    if token.is_empty() || token.contains('\0') {
        return Err(Error::InvalidInput(
            "lease token must be nonempty and must not contain NUL".into(),
        ));
    }
    Ok(())
}

fn validate_payload_numbers(payload: &Value) -> Result<()> {
    let mut pending = vec![payload];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(number) => {
                let safe = if let Some(value) = number.as_i64() {
                    (-(MAX_SAFE_INTEGER as i64)..=MAX_SAFE_INTEGER as i64).contains(&value)
                } else if let Some(value) = number.as_u64() {
                    value <= MAX_SAFE_INTEGER
                } else {
                    number.as_f64().is_some_and(|value| {
                        value.is_finite() && value.abs() <= MAX_SAFE_INTEGER as f64
                    })
                };
                if !safe {
                    return Err(Error::InvalidInput("queue payload numbers must be within +/-(2^53-1) for cross-SDK JSON interoperability".into()));
                }
            }
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            _ => {}
        }
    }
    Ok(())
}

/// Job fields without the payload, read in place from the stored document.
/// The payload is ignored here and validated by reference in `read_meta`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobMeta {
    schema_version: u32,
    id: String,
    #[allow(dead_code)]
    payload: serde::de::IgnoredAny,
    status: JobStatus,
    attempts: u32,
    max_attempts: u32,
    priority: u32,
    created_at: u64,
    available_at: u64,
    lease_until: Option<u64>,
    lease_token: Option<String>,
    worker: Option<String>,
    last_error: Option<String>,
}

fn read_meta(key: &str, document: &Document) -> Result<JobMeta> {
    let job: JobMeta =
        JobMeta::deserialize(&document.value).map_err(|error| corruption(key, error))?;
    if job.schema_version != 1
        || job.id != key
        || job.id.is_empty()
        || job.id.len() > MAX_KEY_BYTES
        || job.id.contains('\0')
    {
        return Err(corruption(
            key,
            "schema version or id does not match its key",
        ));
    }
    validate_payload_numbers(document.value.get("payload").unwrap_or(&Value::Null))
        .map_err(|error| corruption(key, error))?;
    if !(1..=1000).contains(&job.max_attempts)
        || job.attempts > job.max_attempts
        || job.priority > 1000
    {
        return Err(corruption(
            key,
            "invalid attempts, max_attempts, or priority",
        ));
    }
    for value in [
        Some(job.created_at),
        Some(job.available_at),
        job.lease_until,
    ]
    .into_iter()
    .flatten()
    {
        if value > MAX_SAFE_INTEGER {
            return Err(corruption(key, "timestamp exceeds 2^53-1"));
        }
    }
    if job.last_error.as_ref().is_some_and(|error| {
        error.is_empty() || error.len() > MAX_ERROR_BYTES || error.contains('\0')
    }) {
        return Err(corruption(
            key,
            "last_error must contain 1..=4096 UTF-8 bytes without NUL",
        ));
    }
    match job.status {
        JobStatus::Leased => {
            if job.attempts == 0
                || !job
                    .lease_until
                    .is_some_and(|until| until > job.available_at)
                || !job.lease_token.as_ref().is_some_and(|token| {
                    !token.is_empty() && token.len() <= MAX_TOKEN_BYTES && !token.contains('\0')
                })
                || !job.worker.as_ref().is_some_and(|worker| {
                    !worker.is_empty() && worker.len() <= MAX_WORKER_BYTES && !worker.contains('\0')
                })
            {
                return Err(corruption(key, "leased jobs need attempts, a valid token/worker, and a lease after availability"));
            }
        }
        status => {
            if job.lease_until.is_some() || job.lease_token.is_some() || job.worker.is_some() {
                return Err(corruption(
                    key,
                    "non-leased jobs must not contain lease fields",
                ));
            }
            if (status == JobStatus::Queued && job.attempts >= job.max_attempts)
                || (status == JobStatus::Done && job.attempts == 0)
                || (status == JobStatus::Dead && job.attempts != job.max_attempts)
            {
                return Err(corruption(key, "attempt count does not match job status"));
            }
        }
    }
    Ok(job)
}

/// Validate a stored job in place and clone it (payload included) once.
fn read_job(key: &str, document: &Document) -> Result<Job> {
    let meta = read_meta(key, document)?;
    Ok(Job {
        schema_version: meta.schema_version,
        id: meta.id,
        payload: document
            .value
            .get("payload")
            .cloned()
            .unwrap_or(Value::Null),
        status: meta.status,
        attempts: meta.attempts,
        max_attempts: meta.max_attempts,
        priority: meta.priority,
        created_at: meta.created_at,
        available_at: meta.available_at,
        lease_until: meta.lease_until,
        lease_token: meta.lease_token,
        worker: meta.worker,
        last_error: meta.last_error,
    })
}

fn find_job(inner: &Inner, collection: &str, id: &str) -> Result<Option<Job>> {
    inner
        .state
        .collections
        .get(collection)
        .and_then(|jobs| jobs.get(id))
        .map(|document| read_job(id, document))
        .transpose()
}

fn job_operation(collection: &str, job: &Job, options: Options) -> Result<Operation> {
    let value = serde_json::to_value(job)?;
    validate_document(&value, options)?;
    Ok(Operation::Put {
        collection: collection.into(),
        key: job.id.clone(),
        value,
    })
}

fn encoded_operation_bytes(
    operation: &impl Serialize,
    encoded_bytes: usize,
    has_previous: bool,
    options: Options,
) -> Result<usize> {
    let comma = usize::from(has_previous);
    let remaining = options
        .max_frame_bytes
        .checked_sub(encoded_bytes)
        .and_then(|bytes| bytes.checked_sub(comma))
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
    Ok(encoded_bytes + comma + counter.bytes)
}

fn push_checked(
    operations: &mut Vec<Operation>,
    encoded_bytes: &mut usize,
    operation: Operation,
    options: Options,
) -> Result<()> {
    *encoded_bytes =
        encoded_operation_bytes(&operation, *encoded_bytes, !operations.is_empty(), options)?;
    operations.push(operation);
    Ok(())
}

fn external_operations(operations: &[ChangeOperation], options: Options) -> Result<Vec<Operation>> {
    let mut prepared = Vec::new();
    let mut encoded_bytes = EMPTY_BATCH_BYTES;
    for operation in operations {
        let (collection, key) = match operation {
            ChangeOperation::Put {
                collection, key, ..
            }
            | ChangeOperation::Delete { collection, key } => (collection, key),
        };
        validate_user_location(collection, key)?;
        if collection.starts_with(QUEUE_PREFIX) {
            return Err(Error::InvalidInput(
                "ack operations must not modify internal queue collections".into(),
            ));
        }
        if let ChangeOperation::Put { value, .. } = operation {
            validate_document(value, options)?;
        }
        encoded_bytes =
            encoded_operation_bytes(operation, encoded_bytes, !prepared.is_empty(), options)?;
        // Count the public wire representation before duplicating any payload.
        // ChangeOperation and Operation have the same tagged JSON format.
        let operation = match operation {
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
        };
        prepared.push(operation);
    }
    Ok(prepared)
}

fn commit_job(inner: &mut Inner, collection: &str, job: &Job, options: Options) -> Result<()> {
    commit_operations(inner, &[job_operation(collection, job, options)?], options)
}

fn clear_lease(job: &mut Job) {
    job.lease_until = None;
    job.lease_token = None;
    job.worker = None;
}

fn owns_lease(job: &Job, token: &str, now_ms: u64) -> bool {
    job.status == JobStatus::Leased
        && job.lease_token.as_deref() == Some(token)
        && job.lease_until.is_some_and(|until| now_ms < until)
}

fn effective_availability(job: &JobMeta, now_ms: u64) -> Option<u64> {
    match job.status {
        JobStatus::Queued if job.available_at <= now_ms => Some(job.available_at),
        JobStatus::Leased if job.attempts < job.max_attempts => {
            job.lease_until.filter(|until| *until <= now_ms)
        }
        _ => None,
    }
}

type JobMap = BTreeMap<String, Document>;

/// Key of the job `queue_claim` would lease: highest priority, then earliest
/// availability, then smallest id. Only metadata is read; nothing is cloned.
fn select_claim(documents: &JobMap, now_ms: u64) -> Result<Option<&str>> {
    let mut selected: Option<(u32, u64, &str)> = None;
    for (key, document) in documents {
        let job = read_meta(key, document)?;
        if let Some(availability) = effective_availability(&job, now_ms) {
            let preferred = selected.map_or(true, |(priority, current_availability, current)| {
                job.priority > priority
                    || (job.priority == priority
                        && (availability, key.as_str()) < (current_availability, current))
            });
            if preferred {
                selected = Some((job.priority, availability, key.as_str()));
            }
        }
    }
    Ok(selected.map(|(_, _, key)| key))
}

fn scan_stats(documents: &JobMap, now_ms: u64) -> Result<QueueStats> {
    let mut stats = QueueStats {
        queued: 0,
        leased: 0,
        done: 0,
        dead: 0,
        ready: 0,
        expired: 0,
    };
    for (key, document) in documents {
        let job = read_meta(key, document)?;
        match job.status {
            JobStatus::Queued => stats.queued += 1,
            JobStatus::Leased => stats.leased += 1,
            JobStatus::Done => stats.done += 1,
            JobStatus::Dead => stats.dead += 1,
        }
        if effective_availability(&job, now_ms).is_some() {
            stats.ready += 1;
        }
        if job.status == JobStatus::Leased && job.lease_until.is_some_and(|until| until <= now_ms) {
            stats.expired += 1;
        }
    }
    Ok(stats)
}

/// Unsorted (lease_until, id) pairs of expired leases.
fn scan_expired(documents: &JobMap, now_ms: u64) -> Result<Vec<(u64, String)>> {
    let mut expired = Vec::new();
    for (key, document) in documents {
        let job = read_meta(key, document)?;
        if job.status == JobStatus::Leased {
            if let Some(until) = job.lease_until.filter(|until| *until <= now_ms) {
                expired.push((until, key.clone()));
            }
        }
    }
    Ok(expired)
}

impl Database {
    /// Insert a durable job, or return the original job for a duplicate id.
    /// Done/dead jobs keep their ids. Inputs are validated even for duplicates.
    /// Queue data lives in `_cedro.jobs.<name>`; do not edit it through CRUD.
    pub fn queue_enqueue(
        &self,
        name: &str,
        id: &str,
        payload: Value,
        options: EnqueueOptions,
    ) -> Result<EnqueueResult> {
        let collection = queue_collection(name)?;
        validate_queue_key(&collection, id)?;
        if !(1..=1000).contains(&options.max_attempts) || options.priority > 1000 {
            return Err(Error::InvalidInput(
                "max_attempts must be 1..=1000 and priority must be 0..=1000".into(),
            ));
        }
        let available_at = add_time(options.now_ms, options.delay_ms, "delay_ms", false)?;
        validate_document(&payload, self.options)?;
        validate_payload_numbers(&payload)?;
        let job = Job {
            schema_version: 1,
            id: id.into(),
            payload,
            status: JobStatus::Queued,
            attempts: 0,
            max_attempts: options.max_attempts,
            priority: options.priority,
            created_at: options.now_ms,
            available_at,
            lease_until: None,
            lease_token: None,
            worker: None,
            last_error: None,
        };
        let mut operations = Vec::new();
        let mut encoded_bytes = EMPTY_BATCH_BYTES;
        push_checked(
            &mut operations,
            &mut encoded_bytes,
            job_operation(&collection, &job, self.options)?,
            self.options,
        )?;
        let mut inner = self.lock()?;
        if inner.writer_active {
            return Err(Error::WriterBusy);
        }
        if let Some(job) = find_job(&inner, &collection, id)? {
            return Ok(EnqueueResult {
                inserted: false,
                job,
            });
        }
        commit_operations(&mut inner, &operations, self.options)?;
        Ok(EnqueueResult {
            inserted: true,
            job,
        })
    }

    /// Claim the highest priority eligible job, then earliest effective
    /// availability, then smallest id. Expired nonfinal attempts may be reclaimed.
    /// Final expired attempts need queue_recover_expired to become dead jobs.
    /// Scans are O(n); there is no background worker or priority index.
    pub fn queue_claim(
        &self,
        name: &str,
        worker: &str,
        now_ms: u64,
        lease_ms: u64,
    ) -> Result<Option<Job>> {
        let collection = queue_collection(name)?;
        validate_worker(worker)?;
        let lease_until = add_time(now_ms, lease_ms, "lease_ms", true)?;
        let mut inner = self.lock()?;
        if inner.writer_active {
            return Err(Error::WriterBusy);
        }
        let selected = match inner.state.collections.get(&collection) {
            Some(documents) => select_claim(documents, now_ms)?
                .map(|key| read_job(key, &documents[key]))
                .transpose()?,
            None => None,
        };
        let Some(mut job) = selected else {
            return Ok(None);
        };
        job.attempts += 1;
        let next_cursor = (inner.frame_offsets.len() as u64)
            .checked_add(1)
            .ok_or_else(|| Error::LimitExceeded("transaction cursor overflow".into()))?;
        job.status = JobStatus::Leased;
        job.lease_until = Some(lease_until);
        job.worker = Some(worker.into());
        // Unique within a database history. Restoring/forking a backup creates
        // a separate history and is not a globally unique fencing authority.
        job.lease_token = Some(format!("q1:{next_cursor}:{}:{}", job.attempts, job.id));
        commit_job(&mut inner, &collection, &job, self.options)?;
        Ok(Some(job))
    }

    /// Atomically finish a valid lease and commit optional business/outbox writes.
    /// External effects still need idempotency. Queue collections are forbidden
    /// in these extra operations. Expired and stale tokens return false.
    pub fn queue_ack(
        &self,
        name: &str,
        id: &str,
        token: &str,
        now_ms: u64,
        operations: &[ChangeOperation],
    ) -> Result<bool> {
        let collection = queue_collection(name)?;
        validate_queue_key(&collection, id)?;
        validate_token(token)?;
        safe_time(now_ms, "now_ms")?;
        let external = external_operations(operations, self.options)?;
        let mut inner = self.lock()?;
        if inner.writer_active {
            return Err(Error::WriterBusy);
        }
        let Some(mut job) = find_job(&inner, &collection, id)? else {
            return Ok(false);
        };
        if !owns_lease(&job, token, now_ms) {
            return Ok(false);
        }
        job.status = JobStatus::Done;
        job.last_error = None;
        clear_lease(&mut job);
        let mut operations = Vec::new();
        let mut encoded_bytes = EMPTY_BATCH_BYTES;
        push_checked(
            &mut operations,
            &mut encoded_bytes,
            job_operation(&collection, &job, self.options)?,
            self.options,
        )?;
        for operation in external {
            push_checked(&mut operations, &mut encoded_bytes, operation, self.options)?;
        }
        commit_operations(&mut inner, &operations, self.options)?;
        Ok(true)
    }

    /// Fail a live lease, scheduling another attempt or marking it dead when
    /// its attempts are exhausted. Retry delays use the caller's clock.
    pub fn queue_fail(
        &self,
        name: &str,
        id: &str,
        token: &str,
        now_ms: u64,
        error: &str,
        retry_delay_ms: u64,
    ) -> Result<bool> {
        let collection = queue_collection(name)?;
        validate_queue_key(&collection, id)?;
        validate_token(token)?;
        let available_at = add_time(now_ms, retry_delay_ms, "retry_delay_ms", false)?;
        if error.is_empty() || error.len() > MAX_ERROR_BYTES || error.contains('\0') {
            return Err(Error::InvalidInput(
                "error must contain 1..=4096 UTF-8 bytes without NUL".into(),
            ));
        }
        let mut inner = self.lock()?;
        if inner.writer_active {
            return Err(Error::WriterBusy);
        }
        let Some(mut job) = find_job(&inner, &collection, id)? else {
            return Ok(false);
        };
        if !owns_lease(&job, token, now_ms) {
            return Ok(false);
        }
        clear_lease(&mut job);
        job.last_error = Some(error.into());
        job.status = if job.attempts < job.max_attempts {
            job.available_at = available_at;
            JobStatus::Queued
        } else {
            JobStatus::Dead
        };
        commit_job(&mut inner, &collection, &job, self.options)?;
        Ok(true)
    }

    /// Extend a current live lease without reducing its expiration. A successful
    /// request that leaves the expiration unchanged writes no additional frame.
    pub fn queue_extend(
        &self,
        name: &str,
        id: &str,
        token: &str,
        now_ms: u64,
        lease_ms: u64,
    ) -> Result<bool> {
        let collection = queue_collection(name)?;
        validate_queue_key(&collection, id)?;
        validate_token(token)?;
        let requested_until = add_time(now_ms, lease_ms, "lease_ms", true)?;
        let mut inner = self.lock()?;
        if inner.writer_active {
            return Err(Error::WriterBusy);
        }
        let Some(mut job) = find_job(&inner, &collection, id)? else {
            return Ok(false);
        };
        if !owns_lease(&job, token, now_ms) {
            return Ok(false);
        }
        let existing_until = job.lease_until.unwrap();
        if requested_until <= existing_until {
            return Ok(true);
        }
        job.lease_until = Some(requested_until);
        commit_job(&mut inner, &collection, &job, self.options)?;
        Ok(true)
    }

    pub fn queue_get(&self, name: &str, id: &str) -> Result<Option<Job>> {
        let collection = queue_collection(name)?;
        validate_queue_key(&collection, id)?;
        let inner = self.lock()?;
        find_job(&inner, &collection, id)
    }

    /// Raw status counts, plus jobs eligible now and expired leases. Leased jobs
    /// include expired leases; ready also includes expired nonfinal attempts.
    pub fn queue_stats(&self, name: &str, now_ms: u64) -> Result<QueueStats> {
        let collection = queue_collection(name)?;
        safe_time(now_ms, "now_ms")?;
        let inner = self.lock()?;
        let stats = match inner.state.collections.get(&collection) {
            Some(documents) => scan_stats(documents, now_ms)?,
            None => scan_stats(&JobMap::new(), now_ms)?,
        };
        for count in [
            stats.queued,
            stats.leased,
            stats.done,
            stats.dead,
            stats.ready,
            stats.expired,
        ] {
            safe_time(count, "queue count")?;
        }
        Ok(stats)
    }

    /// Recover oldest expired leases first, at most limit jobs in one atomic
    /// frame. A frame-size error changes nothing; retry with a smaller limit.
    pub fn queue_recover_expired(
        &self,
        name: &str,
        now_ms: u64,
        limit: usize,
    ) -> Result<RecoveryResult> {
        let collection = queue_collection(name)?;
        safe_time(now_ms, "now_ms")?;
        if !(1..=1000).contains(&limit) {
            return Err(Error::InvalidInput(
                "recovery limit must be 1..=1000".into(),
            ));
        }
        let mut inner = self.lock()?;
        if inner.writer_active {
            return Err(Error::WriterBusy);
        }
        let mut expired = match inner.state.collections.get(&collection) {
            Some(documents) => scan_expired(documents, now_ms)?,
            None => Vec::new(),
        };
        expired.sort_unstable();
        let mut result = RecoveryResult {
            requeued: 0,
            dead: 0,
            has_more: expired.len() > limit,
        };
        let mut operations = Vec::new();
        let mut encoded_bytes = EMPTY_BATCH_BYTES;
        for (_, id) in expired.into_iter().take(limit) {
            let mut job = find_job(&inner, &collection, &id)?
                .ok_or_else(|| corruption(&id, "job vanished while mutex held"))?;
            clear_lease(&mut job);
            job.last_error = Some("lease expired".into());
            if job.attempts < job.max_attempts {
                job.status = JobStatus::Queued;
                job.available_at = now_ms;
                result.requeued += 1;
            } else {
                job.status = JobStatus::Dead;
                result.dead += 1;
            }
            push_checked(
                &mut operations,
                &mut encoded_bytes,
                job_operation(&collection, &job, self.options)?,
                self.options,
            )?;
        }
        commit_operations(&mut inner, &operations, self.options)?;
        Ok(result)
    }

    /// Start another attempt cycle for a dead job. Its identity, payload,
    /// creation timestamp, priority, and max_attempts remain unchanged.
    pub fn queue_retry_dead(&self, name: &str, id: &str, now_ms: u64) -> Result<bool> {
        let collection = queue_collection(name)?;
        validate_queue_key(&collection, id)?;
        safe_time(now_ms, "now_ms")?;
        let mut inner = self.lock()?;
        if inner.writer_active {
            return Err(Error::WriterBusy);
        }
        let Some(mut job) = find_job(&inner, &collection, id)? else {
            return Ok(false);
        };
        if job.status != JobStatus::Dead {
            return Ok(false);
        }
        job.status = JobStatus::Queued;
        job.attempts = 0;
        job.available_at = now_ms;
        job.last_error = None;
        clear_lease(&mut job);
        commit_job(&mut inner, &collection, &job, self.options)?;
        Ok(true)
    }
}

#[cfg(test)]
mod scan_equivalence {
    use super::*;

    // Old implementation, kept verbatim as a test oracle.
    fn read_job_old(key: &str, document: &Document) -> Result<Job> {
        let job: Job = serde_json::from_value(document.value.clone())
            .map_err(|error| corruption(key, error))?;
        if job.schema_version != 1
            || job.id != key
            || job.id.is_empty()
            || job.id.len() > MAX_KEY_BYTES
            || job.id.contains('\0')
        {
            return Err(corruption(
                key,
                "schema version or id does not match its key",
            ));
        }
        validate_payload_numbers(&job.payload).map_err(|error| corruption(key, error))?;
        if !(1..=1000).contains(&job.max_attempts)
            || job.attempts > job.max_attempts
            || job.priority > 1000
        {
            return Err(corruption(
                key,
                "invalid attempts, max_attempts, or priority",
            ));
        }
        for value in [
            Some(job.created_at),
            Some(job.available_at),
            job.lease_until,
        ]
        .into_iter()
        .flatten()
        {
            if value > MAX_SAFE_INTEGER {
                return Err(corruption(key, "timestamp exceeds 2^53-1"));
            }
        }
        if job.last_error.as_ref().is_some_and(|error| {
            error.is_empty() || error.len() > MAX_ERROR_BYTES || error.contains('\0')
        }) {
            return Err(corruption(
                key,
                "last_error must contain 1..=4096 UTF-8 bytes without NUL",
            ));
        }
        match job.status {
            JobStatus::Leased => {
                if job.attempts == 0
                    || !job
                        .lease_until
                        .is_some_and(|until| until > job.available_at)
                    || !job.lease_token.as_ref().is_some_and(|token| {
                        !token.is_empty() && token.len() <= MAX_TOKEN_BYTES && !token.contains('\0')
                    })
                    || !job.worker.as_ref().is_some_and(|worker| {
                        !worker.is_empty()
                            && worker.len() <= MAX_WORKER_BYTES
                            && !worker.contains('\0')
                    })
                {
                    return Err(corruption(key, "leased jobs need attempts, a valid token/worker, and a lease after availability"));
                }
            }
            status => {
                if job.lease_until.is_some() || job.lease_token.is_some() || job.worker.is_some() {
                    return Err(corruption(
                        key,
                        "non-leased jobs must not contain lease fields",
                    ));
                }
                if (status == JobStatus::Queued && job.attempts >= job.max_attempts)
                    || (status == JobStatus::Done && job.attempts == 0)
                    || (status == JobStatus::Dead && job.attempts != job.max_attempts)
                {
                    return Err(corruption(key, "attempt count does not match job status"));
                }
            }
        }
        Ok(job)
    }

    fn effective_availability_old(job: &Job, now_ms: u64) -> Option<u64> {
        match job.status {
            JobStatus::Queued if job.available_at <= now_ms => Some(job.available_at),
            JobStatus::Leased if job.attempts < job.max_attempts => {
                job.lease_until.filter(|until| *until <= now_ms)
            }
            _ => None,
        }
    }

    fn old_claim(documents: &JobMap, now_ms: u64) -> Result<Option<Job>> {
        let mut selected: Option<(Job, u64)> = None;
        for (key, document) in documents {
            let job = read_job_old(key, document)?;
            if let Some(availability) = effective_availability_old(&job, now_ms) {
                let preferred =
                    selected
                        .as_ref()
                        .map_or(true, |(current, current_availability)| {
                            job.priority > current.priority
                                || (job.priority == current.priority
                                    && (availability, &job.id)
                                        < (*current_availability, &current.id))
                        });
                if preferred {
                    selected = Some((job, availability));
                }
            }
        }
        Ok(selected.map(|(job, _)| job))
    }

    fn old_stats(documents: &JobMap, now_ms: u64) -> Result<QueueStats> {
        let mut stats = QueueStats {
            queued: 0,
            leased: 0,
            done: 0,
            dead: 0,
            ready: 0,
            expired: 0,
        };
        for (key, document) in documents {
            let job = read_job_old(key, document)?;
            match job.status {
                JobStatus::Queued => stats.queued += 1,
                JobStatus::Leased => stats.leased += 1,
                JobStatus::Done => stats.done += 1,
                JobStatus::Dead => stats.dead += 1,
            }
            if effective_availability_old(&job, now_ms).is_some() {
                stats.ready += 1;
            }
            if job.status == JobStatus::Leased
                && job.lease_until.is_some_and(|until| until <= now_ms)
            {
                stats.expired += 1;
            }
        }
        Ok(stats)
    }

    fn old_expired(documents: &JobMap, now_ms: u64) -> Result<Vec<(u64, String)>> {
        let mut expired = Vec::new();
        for (key, document) in documents {
            let job = read_job_old(key, document)?;
            if job.status == JobStatus::Leased {
                if let Some(until) = job.lease_until.filter(|until| *until <= now_ms) {
                    expired.push((until, key.clone()));
                }
            }
        }
        Ok(expired)
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self, bound: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % bound
        }
    }

    fn check(db: &Database, name: &str, now: u64) {
        let collection = queue_collection(name).unwrap();
        let inner = db.lock().unwrap();
        let empty = JobMap::new();
        let documents = inner.state.collections.get(&collection).unwrap_or(&empty);
        let new = select_claim(documents, now)
            .unwrap()
            .map(|key| read_job(key, &documents[key]).unwrap());
        assert_eq!(new, old_claim(documents, now).unwrap());
        assert_eq!(
            scan_stats(documents, now).unwrap(),
            old_stats(documents, now).unwrap()
        );
        assert_eq!(
            scan_expired(documents, now).unwrap(),
            old_expired(documents, now).unwrap()
        );
        for (key, document) in documents {
            assert_eq!(
                read_job(key, document).unwrap(),
                read_job_old(key, document).unwrap()
            );
        }
    }

    #[test]
    fn seeded_workload_matches_old_scans() {
        let dir = std::env::temp_dir().join(format!("cedrodb-scan-eq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let db = Database::open(&dir).unwrap();
        for seed in [1u64, 7, 42] {
            let name = format!("q{seed}");
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut now = 1_000u64;
            let mut leases: Vec<(String, String)> = Vec::new();
            for step in 0..400u64 {
                now += rng.next(50);
                match rng.next(8) {
                    0..=2 => {
                        let id = format!("job-{}", rng.next(150));
                        let payload = serde_json::json!({"n": step, "s": "x".repeat(rng.next(40) as usize), "a": [1, 2.5, null]});
                        let options = EnqueueOptions {
                            now_ms: now,
                            delay_ms: rng.next(100),
                            max_attempts: 1 + rng.next(3) as u32,
                            priority: rng.next(4) as u32,
                        };
                        db.queue_enqueue(&name, &id, payload, options).unwrap();
                    }
                    3 | 4 => {
                        if let Some(job) =
                            db.queue_claim(&name, "w", now, 20 + rng.next(80)).unwrap()
                        {
                            leases.push((job.id, job.lease_token.unwrap()));
                        }
                    }
                    5 => {
                        if !leases.is_empty() {
                            let (id, token) =
                                leases.swap_remove(rng.next(leases.len() as u64) as usize);
                            let _ = db.queue_ack(&name, &id, &token, now, &[]);
                        }
                    }
                    6 => {
                        if !leases.is_empty() {
                            let (id, token) =
                                leases.swap_remove(rng.next(leases.len() as u64) as usize);
                            let _ = db.queue_fail(&name, &id, &token, now, "boom", rng.next(30));
                        }
                    }
                    _ => {
                        db.queue_recover_expired(&name, now, 1 + rng.next(5) as usize)
                            .unwrap();
                    }
                }
                if step % 5 == 0 {
                    check(&db, &name, now);
                    check(&db, &name, now + 500);
                }
            }
            let stats = db.queue_stats(&name, now).unwrap();
            assert!(stats.dead + stats.done + stats.queued + stats.leased > 0);
        }
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
