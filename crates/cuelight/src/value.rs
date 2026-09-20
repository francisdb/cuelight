use serde::{Deserialize, Serialize};

/// A value a host can push into the engine, and the type show variables hold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Value {
    Bool(bool),
    Number(f64),
    Text(String),
}

impl Value {
    /// Numeric view of the value, used by property bindings.
    /// Booleans read as 0.0 / 1.0; text reads as 0.0.
    pub fn as_number(&self) -> f64 {
        match self {
            Value::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            Value::Number(n) => *n,
            Value::Text(_) => 0.0,
        }
    }
}

impl Value {
    /// Text view of the value: text as is, booleans as `true`/`false`,
    /// numbers in their shortest form (`2`, `2.5`).
    pub fn to_text(&self) -> String {
        match self {
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => crate::model::NumberFormat::Plain.format(*n),
            Value::Text(t) => t.clone(),
        }
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Number(v)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_owned())
    }
}
