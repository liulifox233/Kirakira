use std::cmp::Ordering;

use crate::error::{RuntimeError, TjsResult};

use super::{NumberValue, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComparisonOp {
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

pub fn add_values(lhs: &Value, rhs: &Value) -> TjsResult<Value> {
    match (lhs, rhs) {
        (Value::String(_), _) | (_, Value::String(_)) => Ok(Value::String(format!(
            "{}{}",
            lhs.coerce_to_string()?,
            rhs.coerce_to_string()?
        ))),
        (Value::Octet(lhs), Value::Octet(rhs)) => {
            let mut octets = lhs.clone();
            octets.extend_from_slice(rhs);
            Ok(Value::Octet(octets))
        }
        _ => {
            let lhs_number = numeric_operand(lhs, lhs, rhs, "+")?;
            let rhs_number = numeric_operand(rhs, lhs, rhs, "+")?;
            match (lhs_number, rhs_number) {
                (NumberValue::Integer(lhs), NumberValue::Integer(rhs)) => lhs
                    .checked_add(rhs)
                    .map(Value::Integer)
                    .ok_or_else(|| RuntimeError::type_error("integer addition overflowed")),
                (lhs, rhs) => Ok(Value::Real(lhs.to_real() + rhs.to_real())),
            }
        }
    }
}

pub fn sub_values(lhs: &Value, rhs: &Value) -> TjsResult<Value> {
    let lhs_number = numeric_operand(lhs, lhs, rhs, "-")?;
    let rhs_number = numeric_operand(rhs, lhs, rhs, "-")?;
    match (lhs_number, rhs_number) {
        (NumberValue::Integer(lhs), NumberValue::Integer(rhs)) => lhs
            .checked_sub(rhs)
            .map(Value::Integer)
            .ok_or_else(|| RuntimeError::type_error("integer subtraction overflowed")),
        (lhs, rhs) => Ok(Value::Real(lhs.to_real() - rhs.to_real())),
    }
}

pub fn mul_values(lhs: &Value, rhs: &Value) -> TjsResult<Value> {
    let lhs_number = numeric_operand(lhs, lhs, rhs, "*")?;
    let rhs_number = numeric_operand(rhs, lhs, rhs, "*")?;
    match (lhs_number, rhs_number) {
        (NumberValue::Integer(lhs), NumberValue::Integer(rhs)) => lhs
            .checked_mul(rhs)
            .map(Value::Integer)
            .ok_or_else(|| RuntimeError::type_error("integer multiplication overflowed")),
        (lhs, rhs) => Ok(Value::Real(lhs.to_real() * rhs.to_real())),
    }
}

pub fn div_values(lhs: &Value, rhs: &Value) -> TjsResult<Value> {
    Ok(Value::Real(
        numeric_operand(lhs, lhs, rhs, "/")?.to_real()
            / numeric_operand(rhs, lhs, rhs, "/")?.to_real(),
    ))
}

pub fn rem_values(lhs: &Value, rhs: &Value) -> TjsResult<Value> {
    let rhs_integer = numeric_operand(rhs, lhs, rhs, "%")?.to_integer();
    if rhs_integer == 0 {
        return Err(RuntimeError::type_error("integer remainder by zero"));
    }

    numeric_operand(lhs, lhs, rhs, "%")?
        .to_integer()
        .checked_rem(rhs_integer)
        .map(Value::Integer)
        .ok_or_else(|| RuntimeError::type_error("integer remainder overflowed"))
}

pub fn compare_values(lhs: &Value, rhs: &Value, op: ComparisonOp) -> TjsResult<bool> {
    let ordering = compare_order(lhs, rhs)?;
    Ok(matches!(
        (ordering, op),
        (
            Some(Ordering::Less),
            ComparisonOp::Less | ComparisonOp::LessOrEqual
        ) | (
            Some(Ordering::Equal),
            ComparisonOp::LessOrEqual | ComparisonOp::GreaterOrEqual
        ) | (
            Some(Ordering::Greater),
            ComparisonOp::Greater | ComparisonOp::GreaterOrEqual
        )
    ))
}

pub fn less_than_values(lhs: &Value, rhs: &Value) -> TjsResult<bool> {
    compare_values(lhs, rhs, ComparisonOp::Less)
}

pub fn less_or_equal_values(lhs: &Value, rhs: &Value) -> TjsResult<bool> {
    compare_values(lhs, rhs, ComparisonOp::LessOrEqual)
}

pub fn greater_than_values(lhs: &Value, rhs: &Value) -> TjsResult<bool> {
    compare_values(lhs, rhs, ComparisonOp::Greater)
}

pub fn greater_or_equal_values(lhs: &Value, rhs: &Value) -> TjsResult<bool> {
    compare_values(lhs, rhs, ComparisonOp::GreaterOrEqual)
}

pub fn strict_equal_values(lhs: &Value, rhs: &Value) -> bool {
    match (lhs, rhs) {
        (Value::Void, Value::Void) | (Value::Null, Value::Null) => true,
        (Value::Boolean(lhs), Value::Boolean(rhs)) => lhs == rhs,
        (Value::Integer(lhs), Value::Integer(rhs)) => lhs == rhs,
        (Value::Real(lhs), Value::Real(rhs)) => !lhs.is_nan() && !rhs.is_nan() && lhs == rhs,
        (Value::String(lhs), Value::String(rhs)) => lhs == rhs,
        (Value::Octet(lhs), Value::Octet(rhs)) => lhs == rhs,
        (Value::Object(lhs), Value::Object(rhs)) => lhs == rhs,
        _ => false,
    }
}

pub fn equal_values(lhs: &Value, rhs: &Value) -> bool {
    if same_runtime_type(lhs, rhs) {
        return strict_equal_values(lhs, rhs);
    }

    if matches!(lhs, Value::String(_)) || matches!(rhs, Value::String(_)) {
        return lhs
            .coerce_to_string()
            .and_then(|lhs| rhs.coerce_to_string().map(|rhs| lhs == rhs))
            .unwrap_or(false);
    }

    if is_voidish(lhs) {
        return equal_voidish_to(rhs);
    }
    if is_voidish(rhs) {
        return equal_voidish_to(lhs);
    }

    let Ok(lhs) = lhs.to_real() else {
        return false;
    };
    let Ok(rhs) = rhs.to_real() else {
        return false;
    };

    !lhs.is_nan() && !rhs.is_nan() && lhs == rhs
}

pub fn not_equal_values(lhs: &Value, rhs: &Value) -> bool {
    !equal_values(lhs, rhs)
}

fn compare_order(lhs: &Value, rhs: &Value) -> TjsResult<Option<Ordering>> {
    if let (Value::String(lhs), Value::String(rhs)) = (lhs, rhs) {
        return Ok(Some(lhs.cmp(rhs)));
    }

    let lhs_number = lhs.to_number()?;
    let rhs_number = rhs.to_number()?;

    match (lhs_number, rhs_number) {
        (NumberValue::Integer(lhs), NumberValue::Integer(rhs)) => Ok(Some(lhs.cmp(&rhs))),
        _ => Ok(lhs_number.to_real().partial_cmp(&rhs_number.to_real())),
    }
}

fn same_runtime_type(lhs: &Value, rhs: &Value) -> bool {
    lhs.value_type() == rhs.value_type()
}

fn is_voidish(value: &Value) -> bool {
    matches!(value, Value::Void | Value::Null)
}

fn equal_voidish_to(value: &Value) -> bool {
    match value {
        Value::Void | Value::Null => true,
        Value::Boolean(value) => !*value,
        Value::Integer(value) => *value == 0,
        Value::Real(value) => *value == 0.0,
        Value::String(value) => value.is_empty(),
        Value::Octet(_) | Value::Object(_) => false,
    }
}

fn numeric_operand(
    value: &Value,
    lhs: &Value,
    rhs: &Value,
    operator: &'static str,
) -> TjsResult<NumberValue> {
    value
        .to_number()
        .map_err(|_| unsupported_binary_operation(operator, lhs, rhs))
}

fn unsupported_binary_operation(operator: &'static str, lhs: &Value, rhs: &Value) -> RuntimeError {
    RuntimeError::type_error(format!(
        "operator {operator} is not supported for {} and {}",
        lhs.type_name(),
        rhs.type_name()
    ))
}
