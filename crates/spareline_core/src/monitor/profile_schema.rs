//! The profile schema as spareline-old's zod schema checked it, issue for issue, so an invalid
//! profile is refused with the same text.

use std::fmt::Write as _;
use std::sync::LazyLock;

use regex::Regex;

use crate::monitor::json_value::{JsonValue, Member};
use crate::text;

/// The largest integer zod's `.int()` accepts, JavaScript's `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

static PACKAGE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)+$").expect("a valid pattern")
});

/// One step of the path to a value: an object key or an array index.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Key(&'static str),
    Index(usize),
}

/// One problem with a profile, as zod reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Issue {
    path: Vec<Segment>,
    message: String,
}

/// Collects the issues of one validation, with the path of the value being checked.
#[derive(Debug, Default)]
struct Checker {
    path: Vec<Segment>,
    issues: Vec<Issue>,
}

/// Checks a parsed profile against the schema. On success, the value with zod's transforms
/// applied: strings trimmed and defaults filled in.
///
/// # Errors
///
/// The issues formatted as zod's `prettifyError` writes them: `✖ <message>`, then
/// `  → at <path>` unless the issue is about the whole profile, one per line, sorted by path
/// length and otherwise in the order zod found them.
pub(crate) fn validate(value: &JsonValue) -> Result<JsonValue, String> {
    let mut checker = Checker::default();
    let profile = checker.profile(value);
    if checker.issues.is_empty() {
        return Ok(profile.unwrap_or(JsonValue::Null));
    }
    let mut issues = checker.issues;
    issues.sort_by_key(|issue| issue.path.len());
    let mut text = String::new();
    for issue in issues {
        if !text.is_empty() {
            text.push('\n');
        }
        write!(text, "✖ {}", issue.message).expect("writing to a String never fails");
        if !issue.path.is_empty() {
            write!(text, "\n  → at {}", dot_path(&issue.path))
                .expect("writing to a String never fails");
        }
    }
    Err(text)
}

/// The path as zod's `toDotPath` writes paths of schema keys and indices: `screen.brightness`,
/// `removeApps[0]`.
fn dot_path(path: &[Segment]) -> String {
    let mut text = String::new();
    for segment in path {
        match segment {
            Segment::Key(key) => {
                if !text.is_empty() {
                    text.push('.');
                }
                text.push_str(key);
            }
            Segment::Index(index) => {
                write!(text, "[{index}]").expect("writing to a String never fails");
            }
        }
    }
    text
}

impl Checker {
    fn issue(&mut self, message: String) {
        self.issues.push(Issue {
            path: self.path.clone(),
            message,
        });
    }

    fn at<T>(&mut self, segment: Segment, check: impl FnOnce(&mut Self) -> T) -> T {
        self.path.push(segment);
        let result = check(self);
        self.path.pop();
        result
    }

    fn invalid_type(&mut self, expected: &str, value: &JsonValue) {
        self.issue(format!(
            "Invalid input: expected {expected}, received {}",
            value.type_name()
        ));
    }

    /// A strict object: each known field in schema order, then one issue naming every
    /// unrecognized key. Returns the object with its fields' outputs, absent ones left out.
    fn object(&mut self, value: &JsonValue, fields: &[NamedField]) -> Option<JsonValue> {
        let JsonValue::Object(members) = value else {
            self.invalid_type("object", value);
            return None;
        };
        let mut output = Vec::new();
        for &NamedField { name, field } in fields {
            let input = members
                .iter()
                .find(|member| member.key == name)
                .map(|member| &member.value);
            if let Some(result) = self.at(Segment::Key(name), |checker| checker.field(field, input))
            {
                output.push(Member {
                    key: name.to_owned(),
                    value: result,
                });
            }
        }
        let unknown: Vec<&str> = members
            .iter()
            .map(|member| member.key.as_str())
            .filter(|key| fields.iter().all(|field| field.name != *key))
            .collect();
        if !unknown.is_empty() {
            let plural = if unknown.len() > 1 { "s" } else { "" };
            let names: Vec<String> = unknown.iter().map(|key| format!("\"{key}\"")).collect();
            self.issue(format!("Unrecognized key{plural}: {}", names.join(", ")));
        }
        Some(JsonValue::Object(output))
    }

    /// A field the profile may leave out: zod fills in its default, if any, without checking
    /// it, and the typed profile's serde defaults do the same.
    fn field(&mut self, field: Field, input: Option<&JsonValue>) -> Option<JsonValue> {
        input.and_then(|value| self.check(field, value))
    }

    fn check(&mut self, field: Field, value: &JsonValue) -> Option<JsonValue> {
        match field {
            Field::Bool => {
                if matches!(value, JsonValue::Bool(_)) {
                    Some(value.clone())
                } else {
                    self.invalid_type("boolean", value);
                    None
                }
            }
            Field::Int { min, max } => self.int(value, min, max),
            Field::String(rules) => self.string(value, &rules),
            Field::Strings { rules, max } => {
                self.array(value, max, |checker, item| checker.string(item, &rules))
            }
            Field::RadiosOff => self.object(
                value,
                &[NamedField::new(
                    "dataThresholdBytes",
                    Field::Int {
                        min: Some(0.0),
                        max: None,
                    },
                )],
            ),
            Field::Screen => self.object(
                value,
                &[
                    NamedField::new("autoBrightness", Field::Bool),
                    NamedField::new(
                        "brightness",
                        Field::Int {
                            min: Some(0.0),
                            max: Some(255.0),
                        },
                    ),
                    NamedField::new(
                        "timeoutSeconds",
                        Field::Int {
                            min: Some(1.0),
                            max: Some(86_400.0),
                        },
                    ),
                    NamedField::new("stayOnWhileCharging", Field::Bool),
                ],
            ),
            Field::LockScreen => self.object(
                value,
                &[
                    NamedField::new("title", Field::String(notice_line(40))),
                    NamedField::new(
                        "lines",
                        Field::Strings {
                            rules: notice_line(100),
                            max: Some(3),
                        },
                    ),
                ],
            ),
        }
    }

    /// `z.number().int()` with optional bounds: a non-integer stops the checks, while a value
    /// outside the safe range is reported and the bounds still checked.
    fn int(&mut self, value: &JsonValue, min: Option<f64>, max: Option<f64>) -> Option<JsonValue> {
        let JsonValue::Number(number) = value else {
            self.invalid_type("number", value);
            return None;
        };
        let number = *number;
        if number.fract() != 0.0 {
            self.issue("Invalid input: expected int, received number".to_owned());
            return None;
        }
        let before = self.issues.len();
        if number > MAX_SAFE_INTEGER {
            self.issue("Too big: expected int to be <=9007199254740991".to_owned());
        } else if number < -MAX_SAFE_INTEGER {
            self.issue("Too small: expected int to be >=-9007199254740991".to_owned());
        }
        if let Some(min) = min
            && number < min
        {
            self.issue(format!("Too small: expected number to be >={min}"));
        }
        if let Some(max) = max
            && number > max
        {
            self.issue(format!("Too big: expected number to be <={max}"));
        }
        (self.issues.len() == before).then(|| value.clone())
    }

    /// `z.string()` with its rules: trimmed first when the schema trims, then the length
    /// bounds, counted in code points as zod 4 counts them, then the pattern.
    fn string(&mut self, value: &JsonValue, rules: &StringRules) -> Option<JsonValue> {
        let JsonValue::String(raw) = value else {
            self.invalid_type("string", value);
            return None;
        };
        let text = match rules.trim {
            Trim::Yes => text::trim(raw),
            Trim::No => raw.as_str(),
        };
        let before = self.issues.len();
        let length = text.chars().count();
        let StringRules {
            min, max, pattern, ..
        } = *rules;
        if let Some(min) = min
            && length < min
        {
            self.issue(format!(
                "Too small: expected string to have >={min} characters"
            ));
        }
        if let Some(max) = max
            && length > max
        {
            self.issue(format!(
                "Too big: expected string to have <={max} characters"
            ));
        }
        if !(pattern.matches)(text) {
            self.issue(pattern.message.to_owned());
        }
        (self.issues.len() == before).then(|| JsonValue::String(text.to_owned()))
    }

    /// `z.array(item)` with an optional `max` length, checked after the items.
    fn array(
        &mut self,
        value: &JsonValue,
        max: Option<usize>,
        mut item: impl FnMut(&mut Self, &JsonValue) -> Option<JsonValue>,
    ) -> Option<JsonValue> {
        let JsonValue::Array(items) = value else {
            self.invalid_type("array", value);
            return None;
        };
        let before = self.issues.len();
        let mut output = Vec::with_capacity(items.len());
        for (index, value) in items.iter().enumerate() {
            if let Some(result) = self.at(Segment::Index(index), |checker| item(checker, value)) {
                output.push(result);
            }
        }
        if let Some(max) = max
            && items.len() > max
        {
            self.issue(format!("Too big: expected array to have <={max} items"));
        }
        (self.issues.len() == before).then_some(JsonValue::Array(output))
    }

    fn profile(&mut self, value: &JsonValue) -> Option<JsonValue> {
        self.object(
            value,
            &[
                NamedField::new("radiosOff", Field::RadiosOff),
                NamedField::new("dataSaver", Field::Bool),
                NamedField::new("smsApp", Field::String(PACKAGE_NAME_RULES)),
                NamedField::new(
                    "removeApps",
                    Field::Strings {
                        rules: PACKAGE_NAME_RULES,
                        max: None,
                    },
                ),
                NamedField::new("screen", Field::Screen),
                NamedField::new("disableSystemUpdates", Field::Bool),
                NamedField::new(
                    "radioLogBufferMiB",
                    Field::Int {
                        min: Some(1.0),
                        max: Some(64.0),
                    },
                ),
                NamedField::new("mmsWithDataOff", Field::Bool),
                NamedField::new("lockScreen", Field::LockScreen),
            ],
        )
    }
}

/// Whether a string schema trims its input before the other checks.
#[derive(Clone, Copy, Debug)]
enum Trim {
    Yes,
    No,
}

/// A pattern check with the message zod gives when it fails.
#[derive(Clone, Copy, Debug)]
struct PatternCheck {
    matches: fn(&str) -> bool,
    message: &'static str,
}

/// The checks of one string schema, in the order zod runs them.
#[derive(Clone, Copy, Debug)]
struct StringRules {
    trim: Trim,
    min: Option<usize>,
    max: Option<usize>,
    pattern: PatternCheck,
}

/// An Android package name: `/^[A-Za-z][\w]*(\.[A-Za-z_][\w]*)+$/`, untrimmed.
const PACKAGE_NAME_RULES: StringRules = StringRules {
    trim: Trim::No,
    min: None,
    max: None,
    pattern: PatternCheck {
        matches: is_package_name,
        message: "an Android package name",
    },
};

/// One line of the lock screen notice: trimmed, 1 to `max` code points, no CR or LF.
const fn notice_line(max: usize) -> StringRules {
    StringRules {
        trim: Trim::Yes,
        min: Some(1),
        max: Some(max),
        pattern: PatternCheck {
            matches: is_one_line,
            message: "one line",
        },
    }
}

fn is_package_name(text: &str) -> bool {
    PACKAGE_NAME.is_match(text)
}

/// `/^[^\r\n]+$/`.
fn is_one_line(text: &str) -> bool {
    !text.is_empty() && !text.contains(['\r', '\n'])
}

/// A field of an object schema, by its key.
#[derive(Clone, Copy, Debug)]
struct NamedField {
    name: &'static str,
    field: Field,
}

impl NamedField {
    const fn new(name: &'static str, field: Field) -> Self {
        Self { name, field }
    }
}

/// The schema of one field.
#[derive(Clone, Copy, Debug)]
enum Field {
    Bool,
    Int {
        min: Option<f64>,
        max: Option<f64>,
    },
    String(StringRules),
    Strings {
        rules: StringRules,
        max: Option<usize>,
    },
    RadiosOff,
    Screen,
    LockScreen,
}
