//! JSON as JavaScript's `JSON.parse` reads it: numbers as doubles and object keys in the order a
//! JavaScript object holds them, which the profile's error text depends on.

use std::fmt::{self, Formatter};

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

/// One JSON value, with objects keeping JavaScript's key order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum JsonValue {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, as the nearest double.
    Number(f64),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Self>),
    /// An object, in JavaScript's key order: keys that are array indices first, ascending, then
    /// the others in the order they first appeared. A repeated key keeps its first place and
    /// takes its last value, as `JSON.parse` does.
    Object(Vec<Member>),
}

/// One key and value of a JSON object.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Member {
    /// The key.
    pub(crate) key: String,
    /// The value.
    pub(crate) value: JsonValue,
}

impl JsonValue {
    /// Parses JSON text.
    pub(crate) fn parse(text: &str) -> serde_json::Result<Self> {
        serde_json::from_str(text)
    }

    /// The type name zod's messages give a value, such as `null` or `array`.
    pub(crate) const fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }

    /// The same value as `serde_json` holds it, for a typed deserialization. An integral number
    /// becomes an integer, so it can fill an integer field.
    pub(crate) fn to_serde(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(*value),
            Self::Number(value) => integral(*value).map_or_else(
                || Number::from_f64(*value).map_or(Value::Null, Value::Number),
                |integer| Value::Number(integer.into()),
            ),
            Self::String(value) => Value::String(value.clone()),
            Self::Array(items) => Value::Array(items.iter().map(Self::to_serde).collect()),
            Self::Object(members) => {
                let mut object = Map::new();
                for member in members {
                    object.insert(member.key.clone(), member.value.to_serde());
                }
                Value::Object(object)
            }
        }
    }
}

/// The integer a double holds exactly, within the range JavaScript counts as safe.
fn integral(value: f64) -> Option<i64> {
    const MAX_SAFE: f64 = 9_007_199_254_740_991.0;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the value is integral and within the safe range, so the cast is exact"
    )]
    (value.fract() == 0.0 && value.abs() <= MAX_SAFE).then_some(value as i64)
}

/// Whether JavaScript treats `key` as an array index, which objects list before other keys.
fn is_array_index(key: &str) -> bool {
    key.parse::<u32>()
        .is_ok_and(|index| index != u32::MAX && index.to_string() == key)
}

impl<'de> Deserialize<'de> for JsonValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = JsonValue;

    fn expecting(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<JsonValue, E> {
        Ok(JsonValue::Bool(value))
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "JavaScript reads every number as the nearest double"
    )]
    fn visit_i64<E>(self, value: i64) -> Result<JsonValue, E> {
        Ok(JsonValue::Number(value as f64))
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "JavaScript reads every number as the nearest double"
    )]
    fn visit_u64<E>(self, value: u64) -> Result<JsonValue, E> {
        Ok(JsonValue::Number(value as f64))
    }

    fn visit_f64<E>(self, value: f64) -> Result<JsonValue, E> {
        Ok(JsonValue::Number(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<JsonValue, E> {
        Ok(JsonValue::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<JsonValue, E> {
        Ok(JsonValue::String(value))
    }

    fn visit_unit<E>(self) -> Result<JsonValue, E> {
        Ok(JsonValue::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<JsonValue, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(JsonValue::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<JsonValue, A::Error> {
        let mut members: Vec<Member> = Vec::new();
        while let Some((key, value)) = map.next_entry::<String, JsonValue>()? {
            match members.iter_mut().find(|member| member.key == key) {
                Some(member) => member.value = value,
                None => members.push(Member { key, value }),
            }
        }
        let mut indices: Vec<Member> = Vec::new();
        let mut others: Vec<Member> = Vec::new();
        for member in members {
            if is_array_index(&member.key) {
                indices.push(member);
            } else {
                others.push(member);
            }
        }
        indices.sort_by_key(|member| member.key.parse::<u32>().unwrap_or(u32::MAX));
        indices.extend(others);
        Ok(JsonValue::Object(indices))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Vec<String> {
        match JsonValue::parse(text).unwrap() {
            JsonValue::Object(members) => members.into_iter().map(|member| member.key).collect(),
            other => panic!("not an object: {other:?}"),
        }
    }

    #[test]
    fn objects_keep_javascripts_key_order() {
        assert_eq!(keys(r#"{"2":1,"1":1,"b":1,"a":1}"#), ["1", "2", "b", "a"]);
        assert_eq!(keys(r#"{"01":1,"1":1}"#), ["1", "01"]);
        assert_eq!(
            keys(r#"{"4294967295":1,"4294967294":1}"#),
            ["4294967294", "4294967295"]
        );
    }

    #[test]
    fn a_repeated_key_keeps_its_first_place_and_its_last_value() {
        assert_eq!(
            JsonValue::parse(r#"{"a":1,"b":2,"a":3}"#).unwrap(),
            JsonValue::Object(vec![
                Member {
                    key: "a".to_owned(),
                    value: JsonValue::Number(3.0)
                },
                Member {
                    key: "b".to_owned(),
                    value: JsonValue::Number(2.0)
                },
            ])
        );
    }

    #[test]
    fn integral_numbers_become_integers_for_serde() {
        assert_eq!(
            JsonValue::parse("[8.0, 1.5, -0]").unwrap().to_serde(),
            serde_json::json!([8, 1.5, 0])
        );
    }
}
