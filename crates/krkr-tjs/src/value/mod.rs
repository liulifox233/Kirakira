mod object;
mod ops;

pub use object::ObjectValue;
pub use ops::{
    ComparisonOp, add_values, compare_values, div_values, equal_values, greater_or_equal_values,
    greater_than_values, less_or_equal_values, less_than_values, mul_values, not_equal_values,
    rem_values, strict_equal_values, sub_values,
};

use crate::error::{RuntimeError, TjsResult};

pub type Octet = Vec<u8>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueType {
    Void,
    Null,
    Boolean,
    Integer,
    Real,
    String,
    Octet,
    Object,
}

impl ValueType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::Null => "null",
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Real => "real",
            Self::String => "string",
            Self::Octet => "octet",
            Self::Object => "object",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NumberValue {
    Integer(i64),
    Real(f64),
}

impl NumberValue {
    pub const fn is_integer(self) -> bool {
        matches!(self, Self::Integer(_))
    }

    pub fn to_integer(self) -> i64 {
        match self {
            Self::Integer(value) => value,
            Self::Real(value) => real_to_integer(value),
        }
    }

    pub fn to_real(self) -> f64 {
        match self {
            Self::Integer(value) => value as f64,
            Self::Real(value) => value,
        }
    }

    pub fn into_value(self) -> Value {
        match self {
            Self::Integer(value) => Value::Integer(value),
            Self::Real(value) => Value::Real(value),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Void,
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    String(String),
    Octet(Octet),
    Object(Box<ObjectValue>),
}

impl Value {
    pub const fn value_type(&self) -> ValueType {
        match self {
            Self::Void => ValueType::Void,
            Self::Null => ValueType::Null,
            Self::Boolean(_) => ValueType::Boolean,
            Self::Integer(_) => ValueType::Integer,
            Self::Real(_) => ValueType::Real,
            Self::String(_) => ValueType::String,
            Self::Octet(_) => ValueType::Octet,
            Self::Object(_) => ValueType::Object,
        }
    }

    pub const fn type_name(&self) -> &'static str {
        self.value_type().as_str()
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Void | Self::Null => false,
            Self::Boolean(value) => *value,
            Self::Integer(value) => *value != 0,
            Self::Real(value) => *value != 0.0 && !value.is_nan(),
            Self::String(_) => self.to_integer().is_ok_and(|value| value != 0),
            Self::Octet(value) => !value.is_empty(),
            Self::Object(_) => true,
        }
    }

    pub fn to_number(&self) -> TjsResult<NumberValue> {
        match self {
            Self::Void | Self::Null => Ok(NumberValue::Integer(0)),
            Self::Boolean(value) => Ok(NumberValue::Integer(i64::from(*value))),
            Self::Integer(value) => Ok(NumberValue::Integer(*value)),
            Self::Real(value) => Ok(NumberValue::Real(*value)),
            Self::String(value) => Ok(parse_string_number(value)),
            Self::Octet(_) | Self::Object(_) => Err(self.conversion_error("integer/real")),
        }
    }

    pub fn to_integer(&self) -> TjsResult<i64> {
        Ok(self.to_number()?.to_integer())
    }

    pub fn to_real(&self) -> TjsResult<f64> {
        Ok(self.to_number()?.to_real())
    }

    pub fn coerce_to_string(&self) -> TjsResult<String> {
        match self {
            Self::Void => Ok(String::new()),
            Self::Null => Ok("null".to_owned()),
            Self::Boolean(value) => Ok(if *value { "1" } else { "0" }.to_owned()),
            Self::Octet(_) => Err(self.conversion_error("string")),
            _ => Ok(self.to_tjs_string()),
        }
    }

    pub fn to_tjs_string(&self) -> String {
        match self {
            Self::Void => "void".to_owned(),
            Self::Null => "null".to_owned(),
            Self::Boolean(value) => value.to_string(),
            Self::Integer(value) => value.to_string(),
            Self::Real(value) => format_real(*value),
            Self::String(value) => value.clone(),
            Self::Octet(value) => format!("<% {} %>", format_octet(value)),
            Self::Object(_) => "[object Object]".to_owned(),
        }
    }

    fn conversion_error(&self, target: &'static str) -> RuntimeError {
        RuntimeError::type_error(format!(
            "cannot convert {} value to {target}",
            self.type_name()
        ))
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Real(value)
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<Vec<u8>> for Value {
    fn from(value: Vec<u8>) -> Self {
        Self::Octet(value)
    }
}

impl From<&[u8]> for Value {
    fn from(value: &[u8]) -> Self {
        Self::Octet(value.to_owned())
    }
}

impl From<ObjectValue> for Value {
    fn from(value: ObjectValue) -> Self {
        Self::Object(Box::new(value))
    }
}

fn parse_string_number(value: &str) -> NumberValue {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return NumberValue::Integer(0);
    }

    match trimmed {
        "true" => return NumberValue::Integer(1),
        "false" => return NumberValue::Integer(0),
        "NaN" => return NumberValue::Real(f64::NAN),
        "Infinity" | "+Infinity" => return NumberValue::Real(f64::INFINITY),
        "-Infinity" => return NumberValue::Real(f64::NEG_INFINITY),
        _ => {}
    }

    if let Some(value) = parse_prefixed_integer(trimmed) {
        return NumberValue::Integer(value);
    }

    if trimmed.contains(['.', 'e', 'E']) {
        return trimmed
            .parse::<f64>()
            .map(NumberValue::Real)
            .unwrap_or(NumberValue::Integer(0));
    }

    trimmed
        .parse::<i64>()
        .map(NumberValue::Integer)
        .or_else(|_| trimmed.parse::<f64>().map(NumberValue::Real))
        .unwrap_or(NumberValue::Integer(0))
}

fn parse_prefixed_integer(value: &str) -> Option<i64> {
    let (sign, digits) = if let Some(rest) = value.strip_prefix('-') {
        (-1_i64, rest)
    } else if let Some(rest) = value.strip_prefix('+') {
        (1_i64, rest)
    } else {
        (1_i64, value)
    };

    let (digits, radix) = if let Some(digits) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (digits, 16)
    } else if let Some(digits) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        (digits, 2)
    } else if let Some(digits) = digits
        .strip_prefix("0o")
        .or_else(|| digits.strip_prefix("0O"))
    {
        (digits, 8)
    } else if digits.len() > 1
        && digits.starts_with('0')
        && digits.chars().all(|c| matches!(c, '0'..='7'))
    {
        (&digits[1..], 8)
    } else {
        return None;
    };

    i64::from_str_radix(digits, radix)
        .ok()
        .and_then(|number| number.checked_mul(sign))
}

fn real_to_integer(value: f64) -> i64 {
    if value.is_nan() {
        0
    } else if value >= i64::MAX as f64 {
        i64::MAX
    } else if value <= i64::MIN as f64 {
        i64::MIN
    } else {
        value.trunc() as i64
    }
}

fn format_real(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "+Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        value.to_string()
    }
}

fn format_octet(value: &[u8]) -> String {
    value
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
