//! Exact, bounded retrieval over committed JSON documents. Embeddings are
//! supplied by the application; this module never calls a model or network.
use super::*;

const MAX_DIMENSIONS: usize = 4096;
const MAX_TEXT_FIELDS: usize = 8;
const MAX_QUERY_TERMS: usize = 64;
const RRF_K: f64 = 60.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchQuery {
    pub text: Option<String>,
    pub vector: Option<Vec<f64>>,
    pub vector_field: String,
    pub text_fields: Vec<String>,
    pub filters: Vec<Filter>,
    pub limit: usize,
    /// Maximum candidates inspected, before filters. An equality index can
    /// reduce that set. Exceeding the budget is an error, never a partial top-k.
    pub max_scanned: usize,
    pub min_vector_score: Option<f64>,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: None,
            vector: None,
            vector_field: "embedding".into(),
            text_fields: vec!["text".into()],
            filters: Vec::new(),
            limit: 10,
            max_scanned: 10_000,
            min_vector_score: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Text,
    Vector,
    Hybrid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub key: String,
    pub value: Value,
    /// BM25 for text, cosine for vector, or reciprocal rank fusion for hybrid.
    /// Scores from different modes are not directly comparable.
    pub score: f64,
    pub vector_score: Option<f64>,
    pub text_score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    pub scanned: u64,
    /// Documents matching at least one retrieval channel after prefiltering.
    pub matched: u64,
    pub used_index: Option<String>,
    pub mode: SearchMode,
}

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.chars().flat_map(char::to_lowercase).collect())
}

/// Scale before computing a norm: very large/small finite magnitudes must not
/// overflow/underflow their squares into an invalid cosine score.
fn normalize(vector: &[f64]) -> Result<Vec<f64>> {
    if !(1..=MAX_DIMENSIONS).contains(&vector.len()) {
        return Err(Error::InvalidInput(format!(
            "search vector dimensions must be 1..={MAX_DIMENSIONS}"
        )));
    }
    let mut scale = 0.0f64;
    for value in vector {
        if !value.is_finite() {
            return Err(Error::InvalidInput(
                "search vectors must contain finite numbers".into(),
            ));
        }
        scale = scale.max(value.abs());
    }
    if scale == 0.0 {
        return Err(Error::InvalidInput(
            "search vectors must have nonzero magnitude".into(),
        ));
    }
    let norm = vector
        .iter()
        .map(|value| (value / scale).powi(2))
        .sum::<f64>()
        .sqrt();
    Ok(vector.iter().map(|value| (value / scale) / norm).collect())
}

struct Candidate<'a> {
    key: &'a String,
    value: &'a Value,
    vector_score: Option<f64>,
    text_score: Option<f64>,
    frequencies: Vec<usize>,
    length: usize,
    score: f64,
}

impl Database {
    /// Text/BM25, exact cosine or hybrid retrieval under one read lock. All
    /// metadata predicates run before scoring. No approximate index, model,
    /// asynchronous execution, pagination snapshot or authorization is implied.
    pub fn search(&self, collection: &str, query: SearchQuery) -> Result<SearchPage> {
        validate_collection(collection)?;
        if !(1..=1000).contains(&query.limit) {
            return Err(Error::InvalidInput("search limit must be 1..=1000".into()));
        }
        if !(1..=100_000).contains(&query.max_scanned) {
            return Err(Error::InvalidInput(
                "search max_scanned must be 1..=100000".into(),
            ));
        }
        query::validate_filters(&query.filters, self.options)?;
        query::validate_field(&query.vector_field)?;
        if !(1..=MAX_TEXT_FIELDS).contains(&query.text_fields.len()) {
            return Err(Error::InvalidInput(
                "search text_fields must contain 1..=8 unique fields".into(),
            ));
        }
        let mut fields = BTreeSet::new();
        for field in &query.text_fields {
            query::validate_field(field)?;
            if !fields.insert(field) {
                return Err(Error::InvalidInput(
                    "search text_fields must be unique".into(),
                ));
            }
        }
        let terms: Vec<String> = if let Some(text) = &query.text {
            if text.is_empty() || text.len() > 4096 {
                return Err(Error::InvalidInput(
                    "search text must contain 1..=4096 UTF-8 bytes".into(),
                ));
            }
            let terms: BTreeSet<_> = tokens(text).collect();
            if terms.is_empty() || terms.len() > MAX_QUERY_TERMS {
                return Err(Error::InvalidInput(
                    "search text must contain 1..=64 distinct alphanumeric terms".into(),
                ));
            }
            terms.into_iter().collect()
        } else {
            Vec::new()
        };
        let vector = query.vector.as_deref().map(normalize).transpose()?;
        let mode = match (query.text.is_some(), vector.is_some()) {
            (true, false) => SearchMode::Text,
            (false, true) => SearchMode::Vector,
            (true, true) => SearchMode::Hybrid,
            (false, false) => {
                return Err(Error::InvalidInput(
                    "search requires text, vector, or both".into(),
                ));
            }
        };
        if let Some(minimum) = query.min_vector_score {
            if vector.is_none() || !minimum.is_finite() || !(-1.0..=1.0).contains(&minimum) {
                return Err(Error::InvalidInput(
                    "min_vector_score requires a vector and a finite value in -1..=1".into(),
                ));
            }
        }
        let inner = self.lock()?;
        let mut page = SearchPage {
            hits: Vec::new(),
            scanned: 0,
            matched: 0,
            used_index: None,
            mode,
        };
        let Some(documents) = inner.state.collections.get(collection) else {
            return Ok(page);
        };
        let selected_index = query
            .filters
            .iter()
            .filter(|filter| filter.op == FilterOp::Eq)
            .filter_map(|filter| {
                inner
                    .state
                    .indexes
                    .get(&(collection.to_owned(), filter.field.clone()))
                    .map(|index| (filter, index.get(&query::index_token(&filter.value))))
            })
            .min_by_key(|(_, keys)| keys.map_or(0, BTreeSet::len));
        let entries: Box<dyn Iterator<Item = (&String, &Document)> + '_> =
            if let Some((filter, keys)) = selected_index {
                page.used_index = Some(filter.field.clone());
                match keys {
                    Some(keys) => Box::new(
                        keys.iter()
                            .filter_map(|key| documents.get(key).map(|document| (key, document))),
                    ),
                    None => Box::new(std::iter::empty()),
                }
            } else {
                Box::new(documents.iter())
            };
        // The predicate order matches ordinary queries, so an equality index
        // cannot change mixed-number comparison errors in excluded documents.
        let filters: Vec<_> = query
            .filters
            .iter()
            .filter(|filter| matches!(filter.op, FilterOp::Eq | FilterOp::Ne | FilterOp::Exists))
            .chain(query.filters.iter().filter(|filter| {
                !matches!(filter.op, FilterOp::Eq | FilterOp::Ne | FilterOp::Exists)
            }))
            .collect();
        let mut candidates = Vec::new();
        let mut document_frequencies = vec![0usize; terms.len()];
        let mut corpus_size = 0usize;
        let mut corpus_length = 0usize;
        for (key, document) in entries {
            if page.scanned == query.max_scanned as u64 {
                return Err(Error::LimitExceeded(
                    "search scan budget exceeded; narrow filters, create an equality index, or raise max_scanned".into(),
                ));
            }
            page.scanned += 1;
            let mut eligible = true;
            for filter in &filters {
                if !query::matches_filter(&document.value, filter)? {
                    eligible = false;
                    break;
                }
            }
            if !eligible {
                continue;
            }
            let mut candidate = Candidate {
                key,
                value: &document.value,
                vector_score: None,
                text_score: None,
                frequencies: vec![0usize; terms.len()],
                length: 0,
                score: 0.0,
            };
            if let Some(query_vector) = &vector {
                if let Some(value) = query::field_value(&document.value, &query.vector_field) {
                    let array = value.as_array().ok_or_else(|| {
                        Error::InvalidInput(format!(
                            "document {key:?}: vector field must be a numeric array"
                        ))
                    })?;
                    if array.len() != query_vector.len() {
                        return Err(Error::InvalidInput(format!(
                            "document {key:?}: vector dimensions differ from query; filter by embedding model"
                        )));
                    }
                    let values: Vec<f64> = array
                        .iter()
                        .map(|value| {
                            value.as_f64().ok_or_else(|| {
                                Error::InvalidInput(format!(
                                    "document {key:?}: vector must contain finite numbers"
                                ))
                            })
                        })
                        .collect::<Result<_>>()?;
                    let normalized = normalize(&values).map_err(|error| {
                        Error::InvalidInput(format!("document {key:?}: {error}"))
                    })?;
                    let score = query_vector
                        .iter()
                        .zip(normalized)
                        .map(|(left, right)| left * right)
                        .sum::<f64>()
                        .clamp(-1.0, 1.0);
                    if query
                        .min_vector_score
                        .map_or(true, |minimum| score >= minimum)
                    {
                        candidate.vector_score = Some(score);
                    }
                }
            }
            if !terms.is_empty() {
                for field in &query.text_fields {
                    if let Some(text) =
                        query::field_value(&document.value, field).and_then(Value::as_str)
                    {
                        for token in tokens(text) {
                            candidate.length += 1;
                            if let Ok(index) = terms.binary_search(&token) {
                                candidate.frequencies[index] += 1;
                            }
                        }
                    }
                }
                if candidate.length > 0 {
                    corpus_size += 1;
                    corpus_length += candidate.length;
                    for (frequency, documents) in
                        candidate.frequencies.iter().zip(&mut document_frequencies)
                    {
                        if *frequency > 0 {
                            *documents += 1;
                        }
                    }
                }
            }
            candidates.push(candidate);
        }
        if corpus_size > 0 {
            let average_length = corpus_length as f64 / corpus_size as f64;
            for candidate in &mut candidates {
                if candidate.length == 0 {
                    continue;
                }
                let mut score = 0.0;
                for (frequency, document_frequency) in
                    candidate.frequencies.iter().zip(&document_frequencies)
                {
                    if *frequency == 0 {
                        continue;
                    }
                    let idf = (1.0
                        + (corpus_size as f64 - *document_frequency as f64 + 0.5)
                            / (*document_frequency as f64 + 0.5))
                        .ln();
                    let frequency = *frequency as f64;
                    let denominator =
                        frequency + 1.2 * (0.25 + 0.75 * candidate.length as f64 / average_length);
                    score += idf * frequency * 2.2 / denominator;
                }
                candidate.text_score = Some(score);
            }
        }
        if mode == SearchMode::Hybrid {
            for channel in [SearchMode::Text, SearchMode::Vector] {
                let mut ranks: Vec<usize> = candidates
                    .iter()
                    .enumerate()
                    .filter_map(|(index, candidate)| {
                        let score = if channel == SearchMode::Text {
                            candidate.text_score.filter(|score| *score > 0.0)
                        } else {
                            candidate.vector_score
                        };
                        score.map(|_| index)
                    })
                    .collect();
                ranks.sort_unstable_by(|left, right| {
                    let score = |index: usize| {
                        if channel == SearchMode::Text {
                            candidates[index].text_score.unwrap()
                        } else {
                            candidates[index].vector_score.unwrap()
                        }
                    };
                    score(*right)
                        .total_cmp(&score(*left))
                        .then_with(|| candidates[*left].key.cmp(candidates[*right].key))
                });
                for (rank, index) in ranks.into_iter().enumerate() {
                    candidates[index].score += 1.0 / (RRF_K + rank as f64 + 1.0);
                }
            }
        } else {
            for candidate in &mut candidates {
                candidate.score = if mode == SearchMode::Vector {
                    candidate.vector_score.unwrap_or(0.0)
                } else {
                    candidate.text_score.unwrap_or(0.0)
                };
            }
        }
        candidates.retain(|candidate| match mode {
            SearchMode::Vector => candidate.vector_score.is_some(),
            SearchMode::Text => candidate.text_score.is_some_and(|score| score > 0.0),
            SearchMode::Hybrid => candidate.score > 0.0,
        });
        page.matched = candidates.len() as u64;
        candidates.sort_unstable_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.key.cmp(right.key))
        });
        page.hits = candidates
            .into_iter()
            .take(query.limit)
            .map(|candidate| SearchHit {
                key: candidate.key.clone(),
                value: candidate.value.clone(),
                score: candidate.score,
                vector_score: candidate.vector_score,
                text_score: candidate.text_score,
            })
            .collect();
        Ok(page)
    }
}
