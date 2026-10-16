use super::*;
use std::cmp::Ordering;
use std::ops::Bound::{Excluded, Unbounded};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub key: String,
    pub value: Value,
}

/// All filters are ANDed. `after` is an exclusive key, not a snapshot token.
/// Concurrent changes between pages can change the set of returned documents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Query {
    pub filters: Vec<Filter>,
    pub limit: usize,
    pub after: Option<String>,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            filters: Vec::new(),
            limit: 100,
            after: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    /// A top-level property or RFC 6901 JSON Pointer (e.g. `/profile/city`).
    pub field: String,
    pub op: FilterOp,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    Exists,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryPage {
    pub records: Vec<Record>,
    pub next_after: Option<String>,
    /// Includes the lookahead record used to detect another matching page.
    pub scanned: u64,
    /// Field of the selected equality index, if any.
    pub used_index: Option<String>,
}

pub(crate) fn field_value<'a>(value: &'a Value, field: &str) -> Option<&'a Value> {
    if field.starts_with('/') {
        value.pointer(field)
    } else {
        value.as_object()?.get(field)
    }
}

const EXACT_INTEGER_BOUND: i128 = 1i128 << 53;

fn integer(number: &serde_json::Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

fn safe_integral_float(number: &serde_json::Number) -> Option<i128> {
    if !number.is_f64() {
        return None;
    }
    number
        .as_f64()
        .filter(|value| value.fract() == 0.0 && value.abs() <= EXACT_INTEGER_BOUND as f64)
        .map(|value| value as i128)
}

/// Structural JSON equality with exact safe integer/float interoperability.
/// Booleans never compare equal to numbers. Mixed integer/float equality is
/// allowed only within +/-2^53, avoiding any rounded conversion of large integers.
pub(crate) fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => match (integer(left), integer(right)) {
            (Some(left), Some(right)) => left == right,
            (Some(left), None) => safe_integral_float(right) == Some(left),
            (None, Some(right)) => safe_integral_float(left) == Some(right),
            (None, None) => left == right,
        },
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| values_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left)| {
                    right
                        .get(key)
                        .is_some_and(|right| values_equal(left, right))
                })
        }
        _ => left == right,
    }
}

/// Canonical tokens agree with values_equal, including object key order and
/// exact safe integral floats. Large integers retain their full decimal token.
pub(crate) fn index_token(value: &Value) -> String {
    fn encode(value: &Value, output: &mut String) {
        match value {
            Value::Object(object) => {
                output.push('{');
                let mut entries: Vec<_> = object.iter().collect();
                entries.sort_unstable_by_key(|(key, _)| *key);
                for (position, (key, value)) in entries.into_iter().enumerate() {
                    if position != 0 {
                        output.push(',');
                    }
                    output.push_str(&Value::String(key.clone()).to_string());
                    output.push(':');
                    encode(value, output);
                }
                output.push('}');
            }
            Value::Array(array) => {
                output.push('[');
                for (position, value) in array.iter().enumerate() {
                    if position != 0 {
                        output.push(',');
                    }
                    encode(value, output);
                }
                output.push(']');
            }
            Value::Number(number) => {
                if let Some(integer) = safe_integral_float(number) {
                    output.push_str(&integer.to_string());
                } else {
                    output.push_str(&value.to_string());
                }
            }
            _ => output.push_str(&value.to_string()),
        }
    }
    let mut output = String::new();
    encode(value, &mut output);
    output
}

pub(crate) fn validate_field(field: &str) -> Result<()> {
    if field.is_empty() || field.len() > MAX_KEY_BYTES {
        return Err(Error::InvalidInput(
            "field must contain 1..=4096 UTF-8 bytes".into(),
        ));
    }
    if field.starts_with('/') {
        let mut characters = field.chars();
        while let Some(character) = characters.next() {
            if character == '~' && !matches!(characters.next(), Some('0' | '1')) {
                return Err(Error::InvalidInput(
                    "JSON Pointer escape must be ~0 or ~1".into(),
                ));
            }
        }
    }
    Ok(())
}

// Compare all i64/u64 values exactly. A float mixed with an integer outside the
// exact f64 integer range is rejected rather than silently rounding that integer.
fn numeric_order(left: &serde_json::Number, right: &serde_json::Number) -> Result<Ordering> {
    match (integer(left), integer(right)) {
        (Some(left), Some(right)) => Ok(left.cmp(&right)),
        (left_integer, right_integer) => {
            if left_integer
                .into_iter()
                .chain(right_integer)
                .any(|value| !(-EXACT_INTEGER_BOUND..=EXACT_INTEGER_BOUND).contains(&value))
            {
                return Err(Error::InvalidInput("cannot order an integer outside +/-2^53 against a float; use an integer filter".into()));
            }
            left.as_f64()
                .and_then(|left| right.as_f64().and_then(|right| left.partial_cmp(&right)))
                .ok_or_else(|| Error::InvalidInput("cannot order non-finite JSON numbers".into()))
        }
    }
}

pub(crate) fn matches_filter(document: &Value, filter: &Filter) -> Result<bool> {
    let candidate = field_value(document, &filter.field);
    match filter.op {
        FilterOp::Exists => Ok(candidate.is_some() == filter.value.as_bool().unwrap_or(false)),
        // Missing is distinct from JSON null, and doesn't match eq/ne.
        FilterOp::Eq => Ok(candidate.is_some_and(|value| values_equal(value, &filter.value))),
        FilterOp::Ne => Ok(candidate.is_some_and(|value| !values_equal(value, &filter.value))),
        op => {
            let order = match (candidate, &filter.value) {
                (Some(Value::Number(left)), Value::Number(right)) => numeric_order(left, right)?,
                (Some(Value::String(left)), Value::String(right)) => left.cmp(right),
                _ => return Ok(false),
            };
            Ok(match op {
                FilterOp::Gt => order.is_gt(),
                FilterOp::Gte => !order.is_lt(),
                FilterOp::Lt => order.is_lt(),
                FilterOp::Lte => !order.is_gt(),
                _ => unreachable!(),
            })
        }
    }
}

pub(crate) fn validate_filters(filters: &[Filter], options: Options) -> Result<()> {
    if filters.len() > 64 {
        return Err(Error::LimitExceeded(
            "query supports at most 64 filters".into(),
        ));
    }
    for filter in filters {
        validate_field(&filter.field)?;
        validate_document(&filter.value, options)?;
        if filter.op == FilterOp::Exists && !filter.value.is_boolean() {
            return Err(Error::InvalidInput(
                "exists filter value must be a boolean".into(),
            ));
        }
        if matches!(
            filter.op,
            FilterOp::Gt | FilterOp::Gte | FilterOp::Lt | FilterOp::Lte
        ) && !filter.value.is_number()
            && !filter.value.is_string()
        {
            return Err(Error::InvalidInput(
                "ordered filter value must be a number or string".into(),
            ));
        }
    }
    Ok(())
}

impl Database {
    /// Build a handle-local equality index. Recreate indexes after reopening.
    /// Index allocations are additional to Options::max_state_bytes.
    pub fn create_index(&self, collection: &str, field: &str) -> Result<()> {
        validate_collection(collection)?;
        validate_field(field)?;
        let mut inner = self.lock()?;
        let index_key = (collection.to_owned(), field.to_owned());
        if inner.state.indexes.contains_key(&index_key) {
            return Ok(());
        }
        if inner.state.indexes.len() >= 64 {
            return Err(Error::LimitExceeded(
                "at most 64 handle-local indexes are supported".into(),
            ));
        }
        let mut index = BTreeMap::<String, BTreeSet<String>>::new();
        if let Some(documents) = inner.state.collections.get(collection) {
            for (key, document) in documents {
                if let Some(value) = field_value(&document.value, field) {
                    index
                        .entry(index_token(value))
                        .or_default()
                        .insert(key.clone());
                }
            }
        }
        inner.state.indexes.insert(index_key, index);
        Ok(())
    }

    /// Stream matching records in key order into one bounded result page.
    /// Equality is recursive JSON equality; integer/float values interoperate
    /// exactly within +/-2^53, including 1 == 1.0 and -0.0 == 0.
    pub fn query(&self, collection: &str, query: Query) -> Result<QueryPage> {
        validate_collection(collection)?;
        if !(1..=1000).contains(&query.limit) {
            return Err(Error::InvalidInput("query limit must be 1..=1000".into()));
        }
        validate_filters(&query.filters, self.options)?;
        if let Some(after) = &query.after {
            validate_location(collection, after)?;
        }
        let inner = self.lock()?;
        let mut page = QueryPage {
            records: Vec::new(),
            next_after: None,
            scanned: 0,
            used_index: None,
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
                    .map(|index| (filter, index.get(&index_token(&filter.value))))
            })
            .min_by_key(|(_, keys)| keys.map_or(0, BTreeSet::len));
        let candidates: Box<dyn Iterator<Item = (&String, &Document)> + '_> =
            if let Some((filter, keys)) = selected_index {
                page.used_index = Some(filter.field.clone());
                match keys {
                    Some(keys) => {
                        let keys: Box<dyn Iterator<Item = &String>> = if let Some(after) =
                            &query.after
                        {
                            Box::new(keys.range::<str, _>((Excluded(after.as_str()), Unbounded)))
                        } else {
                            Box::new(keys.iter())
                        };
                        Box::new(
                            keys.filter_map(|key| {
                                documents.get(key).map(|document| (key, document))
                            }),
                        )
                    }
                    None => Box::new(std::iter::empty()),
                }
            } else if let Some(after) = &query.after {
                Box::new(documents.range::<str, _>((Excluded(after.as_str()), Unbounded)))
            } else {
                Box::new(documents.iter())
            };
        // An equality index excludes nonmatching documents before evaluation.
        // Give plain scans the same behavior: eliminate documents using all
        // non-ordering predicates before evaluating potentially unsupported
        // mixed numeric ordering. Index selection must not change query errors.
        let evaluation_filters: Vec<_> = query
            .filters
            .iter()
            .filter(|filter| matches!(filter.op, FilterOp::Eq | FilterOp::Ne | FilterOp::Exists))
            .chain(query.filters.iter().filter(|filter| {
                !matches!(filter.op, FilterOp::Eq | FilterOp::Ne | FilterOp::Exists)
            }))
            .collect();
        for (key, document) in candidates {
            page.scanned += 1;
            let mut matches = true;
            for filter in &evaluation_filters {
                if !matches_filter(&document.value, filter)? {
                    matches = false;
                    break;
                }
            }
            if !matches {
                continue;
            }
            if page.records.len() == query.limit {
                page.next_after = page.records.last().map(|record| record.key.clone());
                break;
            }
            page.records.push(Record {
                key: key.clone(),
                value: document.value.clone(),
            });
        }
        Ok(page)
    }
}
