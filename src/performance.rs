//! Fixed-size, handle-local instrumentation. No metrics are written to the log.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationMetrics {
    pub attempts: u64,
    pub successes: u64,
    pub failures: u64,
    pub total_elapsed_micros: u64,
    pub max_elapsed_micros: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetrics {
    pub query: OperationMetrics,
    pub search: OperationMetrics,
    pub commit: OperationMetrics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionDiagnostics {
    pub collection: String,
    pub records: u64,
    pub content_bytes: u64,
    pub index_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostics {
    pub stats: Stats,
    pub limits: Options,
    pub collections: Vec<CollectionDiagnostics>,
    pub session: SessionMetrics,
    pub poisoned: bool,
    pub writer_active: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfilePlan {
    pub collection_records: u64,
    /// Candidates remaining after the query's exclusive key cursor, before
    /// metadata filters. An equality index may restrict this set.
    pub candidate_records: u64,
    pub scanned: u64,
    pub returned: u64,
    pub used_index: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryProfile {
    pub page: QueryPage,
    pub elapsed_micros: u64,
    pub plan: ProfilePlan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchProfile {
    pub page: SearchPage,
    pub elapsed_micros: u64,
    pub plan: ProfilePlan,
}

#[derive(Debug, Default)]
pub(super) struct SessionCounters {
    pub query: OperationCounter,
    pub search: OperationCounter,
    pub commit: OperationCounter,
}

#[derive(Debug, Default)]
pub(super) struct OperationCounter {
    attempts: AtomicU64,
    successes: AtomicU64,
    failures: AtomicU64,
    total_elapsed_micros: AtomicU64,
    max_elapsed_micros: AtomicU64,
}

fn saturating_add(counter: &AtomicU64, value: u64) {
    atomic_update(counter, |current| current.saturating_add(value));
}

fn atomic_update(counter: &AtomicU64, update: impl Fn(u64) -> u64) {
    let mut current = counter.load(Ordering::SeqCst);
    loop {
        let next = update(current);
        if next == current {
            return;
        }
        match counter.compare_exchange_weak(current, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

impl SessionCounters {
    fn snapshot(&self) -> SessionMetrics {
        SessionMetrics {
            query: self.query.snapshot(),
            search: self.search.snapshot(),
            commit: self.commit.snapshot(),
        }
    }
}

impl OperationCounter {
    pub fn start(&self) -> OperationTimer<'_> {
        let started = Instant::now();
        saturating_add(&self.attempts, 1);
        OperationTimer {
            counter: self,
            started,
            finished: false,
        }
    }

    fn snapshot(&self) -> OperationMetrics {
        // Load attempts last: a completed operation incremented it before its
        // outcome. Different fields may still observe different instants.
        let successes = self.successes.load(Ordering::SeqCst);
        let failures = self.failures.load(Ordering::SeqCst);
        let total_elapsed_micros = self.total_elapsed_micros.load(Ordering::SeqCst);
        let max_elapsed_micros = self.max_elapsed_micros.load(Ordering::SeqCst);
        OperationMetrics {
            attempts: self.attempts.load(Ordering::SeqCst),
            successes,
            failures,
            total_elapsed_micros,
            max_elapsed_micros,
        }
    }

    fn complete(&self, success: bool, elapsed_micros: u64) {
        saturating_add(
            if success {
                &self.successes
            } else {
                &self.failures
            },
            1,
        );
        saturating_add(&self.total_elapsed_micros, elapsed_micros);
        atomic_update(&self.max_elapsed_micros, |current| {
            current.max(elapsed_micros)
        });
    }
}

pub(super) struct OperationTimer<'a> {
    counter: &'a OperationCounter,
    started: Instant,
    finished: bool,
}

impl OperationTimer<'_> {
    pub fn finish(mut self, success: bool) -> u64 {
        let elapsed = self.started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        self.counter.complete(success, elapsed);
        self.finished = true;
        elapsed
    }
}

impl Drop for OperationTimer<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let elapsed = self.started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
            self.counter.complete(false, elapsed);
        }
    }
}

impl Database {
    /// Inspect committed counts and handle-local metrics, never document values.
    /// I/O poisoning is reported; a panicked/poisoned mutex still returns error.
    /// Metrics can race concurrent completions and are not a global time snapshot.
    pub fn diagnostics(&self) -> Result<Diagnostics> {
        let inner = self.inner.lock().map_err(|_| Error::Poisoned)?;
        let mut collections: BTreeMap<String, CollectionDiagnostics> = inner
            .state
            .collections
            .iter()
            .map(|(collection, documents)| {
                (
                    collection.clone(),
                    CollectionDiagnostics {
                        collection: collection.clone(),
                        records: documents.len() as u64,
                        content_bytes: documents
                            .values()
                            .map(|document| document.content_bytes as u64)
                            .sum(),
                        index_fields: Vec::new(),
                    },
                )
            })
            .collect();
        for (collection, field) in inner.state.indexes.keys() {
            collections
                .entry(collection.clone())
                .or_insert_with(|| CollectionDiagnostics {
                    collection: collection.clone(),
                    records: 0,
                    content_bytes: 0,
                    index_fields: Vec::new(),
                })
                .index_fields
                .push(field.clone());
        }
        Ok(Diagnostics {
            stats: advanced::stats_from_inner(&inner)?,
            limits: self.options,
            collections: collections.into_values().collect(),
            session: self.metrics.snapshot(),
            poisoned: inner.poisoned,
            writer_active: inner.writer_active,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_and_durations_saturate_instead_of_wrapping() {
        let counter = OperationCounter::default();
        counter.attempts.store(u64::MAX, Ordering::SeqCst);
        counter.successes.store(u64::MAX, Ordering::SeqCst);
        counter
            .total_elapsed_micros
            .store(u64::MAX - 1, Ordering::SeqCst);
        let timer = counter.start();
        timer.finish(true);
        counter.complete(true, 10);
        let snapshot = counter.snapshot();
        assert_eq!(snapshot.attempts, u64::MAX);
        assert_eq!(snapshot.successes, u64::MAX);
        assert_eq!(snapshot.total_elapsed_micros, u64::MAX);
        assert!(snapshot.max_elapsed_micros >= 10);
    }
}
