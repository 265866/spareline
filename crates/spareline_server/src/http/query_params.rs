//! A request's query string, and the checks its parameters get.

use serde_json::Value;

use crate::api_error::ApiError;
use crate::http::js_text;

/// One `name=value` pair of a query string, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
struct QueryPair {
    name: String,
    value: String,
}

/// The parameters of a query string, in order, repeated names included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct QueryParams {
    pairs: Vec<QueryPair>,
}

impl QueryParams {
    /// Decodes `raw`, the part of the URI after `?`, as a form would: `+` is a space and
    /// `%XX` a byte.
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        let pairs: Vec<QueryPair> = form_urlencoded::parse(raw.unwrap_or("").as_bytes())
            .map(|(name, value)| QueryPair {
                name: name.into_owned(),
                value: value.into_owned(),
            })
            .collect();
        Self { pairs }
    }

    /// Every value of `name`, in order.
    pub(crate) fn all(&self, name: &str) -> Vec<String> {
        self.pairs
            .iter()
            .filter(|pair| pair.name == name)
            .map(|pair| pair.value.clone())
            .collect()
    }

    /// The first value of `name`; later ones are ignored, as the live build's query extractor
    /// ignored them.
    pub(crate) fn first(&self, name: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|pair| pair.name == name)
            .map(|pair| pair.value.as_str())
    }

    /// The whole number the first `name` gives, from `min` to `max`, read as JavaScript's
    /// `Number` reads it, as spareline-old's `z.coerce.number()` did.
    ///
    /// # Errors
    ///
    /// A 422 such as `limit: expected an integer from 1 to 500, got "0"`.
    pub(crate) fn integer(&self, name: &str, range: IntRange) -> Result<Option<i64>, ApiError> {
        self.first(name)
            .map(|raw| range.check(name, raw))
            .transpose()
    }

    /// The text the first `name` gives, of at most `max` characters as JavaScript counts them.
    ///
    /// # Errors
    ///
    /// A 422 such as `q: expected at most 200 characters, got 201`.
    pub(crate) fn text(&self, name: &str, max: usize) -> Result<Option<&str>, ApiError> {
        let Some(raw) = self.first(name) else {
            return Ok(None);
        };
        let length = js_text::length(raw);
        if length > max {
            return Err(ApiError::invalid(format!(
                "{name}: expected at most {max} characters, got {length}"
            )));
        }
        Ok(Some(raw))
    }
}

/// The whole numbers a parameter accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IntRange {
    /// The smallest, or `None` for no bound.
    pub(crate) min: Option<i64>,
    /// The largest, or `None` for no bound.
    pub(crate) max: Option<i64>,
}

impl IntRange {
    /// From `min` to `max`.
    pub(crate) const fn between(min: i64, max: i64) -> Self {
        Self {
            min: Some(min),
            max: Some(max),
        }
    }

    /// At least `min`.
    pub(crate) const fn at_least(min: i64) -> Self {
        Self {
            min: Some(min),
            max: None,
        }
    }

    /// Any whole number.
    pub(crate) const fn any() -> Self {
        Self {
            min: None,
            max: None,
        }
    }

    /// `raw` read as a whole number in the range, for the parameter `name`.
    ///
    /// # Errors
    ///
    /// A 422 naming the parameter, the range and the raw text.
    pub(crate) fn check(self, name: &str, raw: &str) -> Result<i64, ApiError> {
        let fits = |value: i64| {
            self.min.is_none_or(|min| value >= min) && self.max.is_none_or(|max| value <= max)
        };
        match js_text::whole_number(raw) {
            Some(value) if fits(value) => Ok(value),
            Some(_) | None => Err(ApiError::invalid(format!(
                "{name}: expected {}, got {}",
                self.describe(),
                quoted(raw)
            ))),
        }
    }

    /// What the range accepts, such as `an integer from 1 to 500`.
    fn describe(self) -> String {
        match (self.min, self.max) {
            (Some(min), Some(max)) => format!("an integer from {min} to {max}"),
            (Some(min), None) => format!("an integer of at least {min}"),
            (None, Some(max)) => format!("an integer of at most {max}"),
            (None, None) => "an integer".to_owned(),
        }
    }
}

/// `text` as a JSON string, so quotes and control characters in it read unambiguously.
pub(crate) fn quoted(text: &str) -> String {
    Value::from(text).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_pairs_in_order() {
        let query = QueryParams::parse(Some("types=device&types=message&q=a+b%2Cc&empty="));
        assert_eq!(query.all("types"), ["device", "message"]);
        assert_eq!(query.first("types"), Some("device"));
        assert_eq!(query.first("q"), Some("a b,c"));
        assert_eq!(query.first("empty"), Some(""));
        assert_eq!(query.first("missing"), None);
        assert_eq!(QueryParams::parse(None).all("types"), Vec::<String>::new());
    }

    #[test]
    fn integers_are_checked_against_their_range() {
        let query = QueryParams::parse(Some(
            "limit=0&after=7&wait=31&before=x&latest=&n=1&n=x&m=x&m=1",
        ));
        let limit = query
            .integer("limit", IntRange::between(1, 500))
            .unwrap_err();
        assert_eq!(
            limit.message(),
            r#"limit: expected an integer from 1 to 500, got "0""#
        );
        assert_eq!(limit.status().as_u16(), 422);
        assert_eq!(
            query.integer("after", IntRange::at_least(0)).unwrap(),
            Some(7)
        );
        assert_eq!(
            query
                .integer("before", IntRange::at_least(0))
                .unwrap_err()
                .message(),
            r#"before: expected an integer of at least 0, got "x""#
        );
        assert_eq!(
            query.integer("latest", IntRange::between(0, 500)).unwrap(),
            Some(0),
            "an empty value is 0, as Number(\"\") is"
        );
        assert_eq!(
            query.integer("absent", IntRange::any()).unwrap(),
            None,
            "a missing parameter is no value"
        );
        assert_eq!(
            query.integer("n", IntRange::any()).unwrap(),
            Some(1),
            "a repeated parameter takes its first value"
        );
        assert_eq!(
            query.integer("m", IntRange::any()).unwrap_err().message(),
            r#"m: expected an integer, got "x""#,
            "only the first value is checked"
        );
    }

    #[test]
    fn text_is_counted_in_utf16_code_units() {
        let long = "\u{1f600}".repeat(101);
        let query = QueryParams::parse(Some(&format!("q={long}")));
        assert_eq!(
            query.text("q", 200).unwrap_err().message(),
            "q: expected at most 200 characters, got 202"
        );
        assert_eq!(query.text("q", 202).unwrap(), Some(long.as_str()));
    }
}
