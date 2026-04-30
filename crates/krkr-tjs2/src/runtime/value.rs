use std::fmt;

use crate::error::{Result, TjsError};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ObjectHandle(pub usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Closure {
    pub object: ObjectHandle,
    pub this_obj: Option<ObjectHandle>,
}

impl Closure {
    pub const fn new(object: ObjectHandle, this_obj: Option<ObjectHandle>) -> Self {
        Self { object, this_obj }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Variant {
    #[default]
    Void,
    Null,
    Integer(i64),
    Real(f64),
    String(String),
    Octet(Vec<u8>),
    Object(ObjectHandle),
    Closure(Closure),
    CodeObject(usize),
}

impl Variant {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::Null => "object",
            Self::Integer(_) => "integer",
            Self::Real(_) => "real",
            Self::String(_) => "string",
            Self::Octet(_) => "octet",
            Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => "object",
        }
    }

    pub fn typeof_name(&self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::Null | Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => "Object",
            Self::String(_) => "String",
            Self::Integer(_) => "Integer",
            Self::Real(_) => "Real",
            Self::Octet(_) => "Octet",
        }
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Void | Self::Null => false,
            Self::Integer(value) => *value != 0,
            Self::Real(value) => *value != 0.0,
            Self::String(value) => Self::parse_numeric_string(value)
                .map(|value| value != 0.0)
                .unwrap_or(false),
            Self::Octet(value) => !value.is_empty(),
            Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => true,
        }
    }

    pub fn to_integer(&self) -> Result<i64> {
        match self {
            Self::Void | Self::Null => Ok(0),
            Self::Integer(value) => Ok(*value),
            Self::Real(value) => Ok(*value as i64),
            Self::String(value) => Ok(Self::parse_numeric_string(value).unwrap_or(0.0) as i64),
            Self::Octet(_) => Err(TjsError::runtime("cannot convert octet to integer")),
            Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => {
                Err(TjsError::runtime("cannot convert object to integer"))
            }
        }
    }

    pub fn to_real(&self) -> Result<f64> {
        match self {
            Self::Void | Self::Null => Ok(0.0),
            Self::Integer(value) => Ok(*value as f64),
            Self::Real(value) => Ok(*value),
            Self::String(value) => Ok(Self::parse_numeric_string(value).unwrap_or(0.0)),
            Self::Octet(_) => Err(TjsError::runtime("cannot convert octet to real")),
            Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => {
                Err(TjsError::runtime("cannot convert object to real"))
            }
        }
    }

    pub fn to_number_variant(&self) -> Result<Self> {
        match self {
            Self::Integer(_) | Self::Real(_) => Ok(self.clone()),
            Self::Void | Self::Null => Ok(Self::Integer(0)),
            Self::String(value) => {
                if let Some(integer) = Self::parse_integer_string(value) {
                    Ok(Self::Integer(integer))
                } else {
                    Ok(Self::Real(Self::parse_numeric_string(value).unwrap_or(0.0)))
                }
            }
            Self::Octet(_) => Err(TjsError::runtime("cannot convert octet to number")),
            Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => {
                Err(TjsError::runtime("cannot convert object to number"))
            }
        }
    }

    pub fn to_tjs_string(&self) -> Result<String> {
        Ok(match self {
            Self::Void => String::new(),
            Self::Null => "null".to_string(),
            Self::Integer(value) => value.to_string(),
            Self::Real(value) => real_to_string(*value),
            Self::String(value) => value.clone(),
            Self::Octet(value) => value.iter().map(|byte| format!("{byte:02x}")).collect(),
            Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => "[object]".to_string(),
        })
    }

    pub fn to_octet(&self) -> Result<Self> {
        match self {
            Self::Octet(value) => Ok(Self::Octet(value.clone())),
            Self::String(value) => Ok(Self::Octet(value.as_bytes().to_vec())),
            Self::Void | Self::Null => Ok(Self::Octet(Vec::new())),
            Self::Integer(_) | Self::Real(_) => Ok(Self::Octet(self.to_tjs_string()?.into_bytes())),
            Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => {
                Err(TjsError::runtime("cannot convert object to octet"))
            }
        }
    }

    pub fn normal_eq(&self, rhs: &Self) -> bool {
        if std::mem::discriminant(self) == std::mem::discriminant(rhs) {
            return self.discern_eq(rhs);
        }

        if matches!(self, Self::String(_)) || matches!(rhs, Self::String(_)) {
            return self.to_tjs_string().ok() == rhs.to_tjs_string().ok();
        }

        if matches!(self, Self::Void) {
            return matches!(rhs, Self::Integer(0))
                || matches!(rhs, Self::Real(value) if *value == 0.0)
                || matches!(rhs, Self::String(value) if value.is_empty());
        }
        if matches!(rhs, Self::Void) {
            return rhs.normal_eq(self);
        }

        match (self.to_real(), rhs.to_real()) {
            (Ok(lhs), Ok(rhs)) => !lhs.is_nan() && !rhs.is_nan() && lhs == rhs,
            _ => false,
        }
    }

    pub fn discern_eq(&self, rhs: &Self) -> bool {
        match (self, rhs) {
            (Self::Void, Self::Void) => true,
            (Self::Null, Self::Null) => true,
            (Self::Integer(lhs), Self::Integer(rhs)) => lhs == rhs,
            (Self::Real(lhs), Self::Real(rhs)) => !lhs.is_nan() && !rhs.is_nan() && lhs == rhs,
            (Self::String(lhs), Self::String(rhs)) => lhs == rhs,
            (Self::Octet(lhs), Self::Octet(rhs)) => lhs == rhs,
            (Self::Object(lhs), Self::Object(rhs)) => lhs == rhs,
            (Self::Closure(lhs), Self::Closure(rhs)) => lhs == rhs,
            (Self::CodeObject(lhs), Self::CodeObject(rhs)) => lhs == rhs,
            _ => false,
        }
    }

    pub fn less_than(&self, rhs: &Self) -> Result<bool> {
        if matches!(self, Self::String(_)) && matches!(rhs, Self::String(_)) {
            return Ok(self.to_tjs_string()? < rhs.to_tjs_string()?);
        }
        if matches!((self, rhs), (Self::Integer(_), Self::Integer(_))) {
            return Ok(self.to_integer()? < rhs.to_integer()?);
        }
        Ok(self.to_real()? < rhs.to_real()?)
    }

    pub fn greater_than(&self, rhs: &Self) -> Result<bool> {
        if matches!(self, Self::String(_)) && matches!(rhs, Self::String(_)) {
            return Ok(self.to_tjs_string()? > rhs.to_tjs_string()?);
        }
        if matches!((self, rhs), (Self::Integer(_), Self::Integer(_))) {
            return Ok(self.to_integer()? > rhs.to_integer()?);
        }
        Ok(self.to_real()? > rhs.to_real()?)
    }

    pub fn increment(&self) -> Result<Self> {
        self.add(&Self::Integer(1))
    }

    pub fn decrement(&self) -> Result<Self> {
        self.sub(&Self::Integer(1))
    }

    pub fn logical_not(&self) -> Self {
        Self::Integer(i64::from(!self.is_truthy()))
    }

    pub fn bit_not(&self) -> Result<Self> {
        Ok(Self::Integer(!self.to_integer()?))
    }

    pub fn negate(&self) -> Result<Self> {
        match self.to_number_variant()? {
            Self::Integer(value) => Ok(Self::Integer(-value)),
            Self::Real(value) => Ok(Self::Real(-value)),
            _ => unreachable!("to_number_variant returns numeric variants"),
        }
    }

    pub fn char_code_of(&self) -> Result<Self> {
        let text = self.to_tjs_string()?;
        Ok(Self::Integer(
            text.encode_utf16().next().map(i64::from).unwrap_or(0),
        ))
    }

    pub fn char_from_code(&self) -> Result<Self> {
        let unit = self.to_integer()? as u16;
        Ok(Self::String(String::from_utf16_lossy(&[unit])))
    }

    pub fn add(&self, rhs: &Self) -> Result<Self> {
        if matches!(self, Self::String(_)) || matches!(rhs, Self::String(_)) {
            return Ok(Self::String(self.to_tjs_string()? + &rhs.to_tjs_string()?));
        }

        if let (Self::Octet(lhs), Self::Octet(rhs)) = (self, rhs) {
            let mut bytes = lhs.clone();
            bytes.extend_from_slice(rhs);
            return Ok(Self::Octet(bytes));
        }

        if matches!((self, rhs), (Self::Integer(_), Self::Integer(_))) {
            return Ok(Self::Integer(self.to_integer()? + rhs.to_integer()?));
        }

        if matches!(self, Self::Void) {
            return match rhs {
                Self::Integer(value) => Ok(Self::Integer(*value)),
                Self::Real(value) => Ok(Self::Real(*value)),
                _ => Ok(Self::Real(self.to_real()? + rhs.to_real()?)),
            };
        }
        if matches!(rhs, Self::Void) && matches!(self, Self::Integer(_) | Self::Real(_)) {
            return Ok(self.clone());
        }

        Ok(Self::Real(self.to_real()? + rhs.to_real()?))
    }

    pub fn binary_int(&self, rhs: &Self, op: impl FnOnce(i64, i64) -> i64) -> Result<Self> {
        Ok(Self::Integer(op(self.to_integer()?, rhs.to_integer()?)))
    }

    pub fn sub(&self, rhs: &Self) -> Result<Self> {
        match (self.to_number_variant()?, rhs.to_number_variant()?) {
            (Self::Integer(lhs), Self::Integer(rhs)) => Ok(Self::Integer(lhs - rhs)),
            (lhs, rhs) => Ok(Self::Real(lhs.to_real()? - rhs.to_real()?)),
        }
    }

    pub fn mul(&self, rhs: &Self) -> Result<Self> {
        match (self.to_number_variant()?, rhs.to_number_variant()?) {
            (Self::Integer(lhs), Self::Integer(rhs)) => Ok(Self::Integer(lhs * rhs)),
            (lhs, rhs) => Ok(Self::Real(lhs.to_real()? * rhs.to_real()?)),
        }
    }

    pub fn div(&self, rhs: &Self) -> Result<Self> {
        let divisor = rhs.to_real()?;
        if divisor == 0.0 {
            return Err(TjsError::runtime("division by zero"));
        }
        Ok(Self::Real(self.to_real()? / divisor))
    }

    pub fn idiv(&self, rhs: &Self) -> Result<Self> {
        let divisor = rhs.to_integer()?;
        if divisor == 0 {
            return Err(TjsError::runtime("integer division by zero"));
        }
        Ok(Self::Integer(self.to_integer()? / divisor))
    }

    pub fn modulo(&self, rhs: &Self) -> Result<Self> {
        let divisor = rhs.to_integer()?;
        if divisor == 0 {
            return Err(TjsError::runtime("modulo by zero"));
        }
        Ok(Self::Integer(self.to_integer()? % divisor))
    }

    fn parse_numeric_string(value: &str) -> Option<f64> {
        value.trim().parse::<f64>().ok()
    }

    fn parse_integer_string(value: &str) -> Option<i64> {
        let value = value.trim();
        if value.is_empty() {
            return Some(0);
        }
        value.parse::<i64>().ok()
    }
}

impl fmt::Display for Variant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Void => write!(f, "void"),
            Self::Null => write!(f, "null"),
            Self::Integer(value) => write!(f, "{value}"),
            Self::Real(value) => write!(f, "{value}"),
            Self::String(value) => write!(f, "{value:?}"),
            Self::Octet(value) => write!(f, "<octet:{} bytes>", value.len()),
            Self::Object(handle) => write!(f, "<object #{}>", handle.0),
            Self::Closure(closure) => write!(f, "<closure #{}>", closure.object.0),
            Self::CodeObject(index) => write!(f, "<inter_object #{index}>"),
        }
    }
}

fn real_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value == f64::INFINITY {
        return "Infinity".to_string();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_string();
    }
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}
