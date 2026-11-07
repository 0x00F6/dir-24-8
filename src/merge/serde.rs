//! Serde-based deep merge (feature `serde`).
//!
//! Both payloads are serialized to [`serde_json::Value`], merged with
//! [`merge_json`] and the result is deserialized back into `T`. This happens
//! once, at insertion: lookups keep borrowing the materialized `T` and never
//! touch serde.
//!
//! | Data                   | Behavior                                  |
//! |------------------------|-------------------------------------------|
//! | objects / maps         | recursive merge per key                   |
//! | key absent in incoming | existing value kept                       |
//! | arrays / vectors       | concatenation, order and duplicates kept  |
//! | scalars                | incoming value wins                       |
//! | incompatible types     | incoming value wins                       |
//!
//! A field omitted with `#[serde(skip_serializing_if = "...")]` is absent
//! from the incoming object and therefore preserves the existing value; an
//! explicit JSON `null` is a scalar and replaces it.
//!
//! Allocations: serializing the existing payload copies its strings into the
//! `Value` tree; the incoming payload is consumed by `to_value` and the final
//! `from_value` moves strings, arrays and maps out of the merged tree.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::DeepMerge;

/// Merges `incoming` into `existing` with the rules of the module docs.
///
/// ```
/// use serde_json::json;
/// use dir_24_8::merge::serde::merge_json;
///
/// let mut existing = json!({"tags": ["a"], "geo": {"cc": "FR"}, "asn": 1});
/// merge_json(&mut existing, json!({"tags": ["b"], "geo": {"city": "Paris"}, "asn": 2}));
/// assert_eq!(
///     existing,
///     json!({"tags": ["a", "b"], "geo": {"cc": "FR", "city": "Paris"}, "asn": 2})
/// );
/// ```
pub fn merge_json(existing: &mut Value, incoming: Value) {
    match (existing, incoming) {
        (Value::Object(cur), Value::Object(inc)) => {
            for (key, value) in inc {
                match cur.get_mut(&key) {
                    Some(slot) => merge_json(slot, value),
                    None => {
                        cur.insert(key, value);
                    }
                }
            }
        }
        (Value::Array(cur), Value::Array(mut inc)) => cur.append(&mut inc),
        (slot, value) => *slot = value,
    }
}

impl DeepMerge for Value {
    fn deep_merge(&mut self, incoming: Self) {
        merge_json(self, incoming);
    }
}

/// Deep merges two serializable values through `serde_json::Value`.
///
/// On error (for example when the merged JSON no longer matches `T`, such as
/// a fixed-size array that grew by concatenation), `existing` is left
/// unchanged.
///
/// ```
/// use serde::{Deserialize, Serialize};
/// use dir_24_8::merge::serde::serde_deep_merge;
///
/// #[derive(Serialize, Deserialize)]
/// struct Info {
///     tags: Vec<String>,
///     #[serde(skip_serializing_if = "Option::is_none")]
///     city: Option<String>,
/// }
///
/// let mut info = Info { tags: vec!["a".into()], city: Some("Paris".into()) };
/// serde_deep_merge(&mut info, Info { tags: vec!["b".into()], city: None })?;
/// assert_eq!(info.tags, ["a", "b"]);
/// assert_eq!(info.city.as_deref(), Some("Paris")); // absent key: existing value kept
/// # Ok::<(), serde_json::Error>(())
/// ```
pub fn serde_deep_merge<T>(existing: &mut T, incoming: T) -> Result<(), serde_json::Error>
where
    T: Serialize + DeserializeOwned,
{
    let mut merged = serde_json::to_value(&*existing)?;
    let incoming = serde_json::to_value(incoming)?;
    merge_json(&mut merged, incoming);
    *existing = serde_json::from_value(merged)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_rules() {
        let mut a = json!({"m": {"x": 1, "keep": true}, "v": [1, 2], "s": "a", "t": 1});
        merge_json(
            &mut a,
            json!({"m": {"x": 2, "new": 3}, "v": [2], "s": null, "t": {"o": 1}}),
        );
        assert_eq!(
            a,
            json!({"m": {"x": 2, "keep": true, "new": 3}, "v": [1, 2, 2], "s": null, "t": {"o": 1}})
        );
    }
}
