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

/// Identity of an object-like value for `==` (normal compare). Official TJS
/// compares only the underlying object for `tvtObject` operands, ignoring the
/// bound `this` (ObjThis); `null` is an object with NULL pointers, and
/// `CodeObject` has no official counterpart and only ever compares equal to
/// itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectIdentity {
    Null,
    Object(ObjectHandle),
    CodeObject(usize),
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
            // Official `operator bool` converts strings through `AsInteger`,
            // so `"true"` is true and `"0.5"` is false (it truncates to 0).
            Self::String(value) => string_to_integer(value) != 0,
            Self::Octet(value) => !value.is_empty(),
            Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => true,
        }
    }

    pub fn to_integer(&self) -> Result<i64> {
        match self {
            Self::Void => Ok(0),
            Self::Integer(value) => Ok(*value),
            Self::Real(value) => Ok(*value as i64),
            Self::String(value) => Ok(string_to_integer(value)),
            Self::Octet(_) => Err(TjsError::runtime("cannot convert octet to integer")),
            // Official `null` is an object variant (both pointers NULL), so
            // `AsInteger` throws `TJSConvertError` for it like any object.
            Self::Null | Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => {
                Err(TjsError::runtime("cannot convert object to integer"))
            }
        }
    }

    pub fn to_real(&self) -> Result<f64> {
        match self {
            Self::Void => Ok(0.0),
            Self::Integer(value) => Ok(*value as f64),
            Self::Real(value) => Ok(*value),
            Self::String(value) => Ok(string_to_real(value)),
            Self::Octet(_) => Err(TjsError::runtime("cannot convert octet to real")),
            Self::Null | Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => {
                Err(TjsError::runtime("cannot convert object to real"))
            }
        }
    }

    pub fn to_number_variant(&self) -> Result<Self> {
        match self {
            Self::Integer(_) | Self::Real(_) => Ok(self.clone()),
            Self::Void => Ok(Self::Integer(0)),
            // Official `AsNumber` keeps whatever `TJSParseNumber` produced
            // (integer or real) and falls back to integer 0.
            Self::String(value) => Ok(parse_number(value).unwrap_or(Self::Integer(0))),
            Self::Octet(_) => Err(TjsError::runtime("cannot convert octet to number")),
            Self::Null | Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => {
                Err(TjsError::runtime("cannot convert object to number"))
            }
        }
    }

    pub fn to_tjs_string(&self) -> Result<String> {
        match self {
            Self::Void => Ok(String::new()),
            // Official `null` is a `tvtObject` with NULL pointers; string
            // conversion renders it via TJSObjectToString like any object.
            Self::Null => Ok("(object 0x0:0x0)".to_string()),
            Self::Integer(value) => Ok(value.to_string()),
            Self::Real(value) => Ok(real_to_string(*value)),
            Self::String(value) => Ok(value.clone()),
            // Official `AsString` throws on octets (the hex-list form exists
            // only as a separate debug helper, never for `+`/comparisons).
            Self::Octet(_) => Err(TjsError::runtime("cannot convert octet to string")),
            // krkrz TJSObjectToString renders objects by identity
            // ("(object 0x<obj>:0x<objthis>)"), so distinct objects must never
            // collapse to one shared string. The handle index plays the role of
            // the official object pointer.
            Self::Object(handle) => Ok(format!("(object 0x{:x}:0x0)", handle.0)),
            Self::Closure(closure) => Ok(format!(
                "(object 0x{:x}:0x{:x})",
                closure.object.0,
                closure.this_obj.map(|this| this.0).unwrap_or(0)
            )),
            Self::CodeObject(index) => Ok(format!("(object 0x{:x}:0x0)", index)),
        }
    }

    pub fn to_octet(&self) -> Result<Self> {
        match self {
            Self::Octet(value) => Ok(Self::Octet(value.clone())),
            Self::String(value) => Ok(Self::Octet(value.as_bytes().to_vec())),
            Self::Void => Ok(Self::Octet(Vec::new())),
            Self::Integer(_) | Self::Real(_) => Ok(Self::Octet(self.to_tjs_string()?.into_bytes())),
            Self::Null | Self::Object(_) | Self::Closure(_) | Self::CodeObject(_) => {
                Err(TjsError::runtime("cannot convert object to octet"))
            }
        }
    }

    /// `==` (normal compare), following official `tTJSVariant::NormalCompare`:
    /// object-like operands (including bound methods) compare only their
    /// underlying object — the official compare deliberately ignores ObjThis —
    /// and variant-conversion errors are swallowed as `false` (the official
    /// implementation wraps the whole compare in `catch(eTJSVariantError)`,
    /// returning false for e.g. `0 == null` or `"x" == octet`).
    pub fn normal_eq(&self, rhs: &Self) -> bool {
        if let (Some(lhs), Some(rhs)) = (self.object_identity(), rhs.object_identity()) {
            return lhs == rhs;
        }
        if std::mem::discriminant(self) == std::mem::discriminant(rhs) {
            return self.discern_eq(rhs);
        }

        if matches!(self, Self::String(_)) || matches!(rhs, Self::String(_)) {
            return match (self.to_tjs_string(), rhs.to_tjs_string()) {
                (Ok(lhs), Ok(rhs)) => lhs == rhs,
                _ => false,
            };
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
            // Official catches the conversion error and returns false.
            _ => false,
        }
    }

    /// The identity official `NormalCompare` uses for object-like values:
    /// just the underlying object, ignoring any bound `this`. Official
    /// `null` is a `tvtObject` with NULL pointers, so `object == null`
    /// compares identities (false) instead of converting to numbers.
    fn object_identity(&self) -> Option<ObjectIdentity> {
        match self {
            Self::Null => Some(ObjectIdentity::Null),
            Self::Object(handle) => Some(ObjectIdentity::Object(*handle)),
            Self::Closure(closure) => Some(ObjectIdentity::Object(closure.object)),
            Self::CodeObject(index) => Some(ObjectIdentity::CodeObject(*index)),
            _ => None,
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

/// Advances past whitespace like official `TJSSkipSpace`.
fn skip_space(chars: &[char], pos: &mut usize) {
    while chars.get(*pos).is_some_and(|ch| ch.is_whitespace()) {
        *pos += 1;
    }
}

/// Official `TJSStringMatch` in word mode: an exact, case-sensitive match that
/// additionally refuses to match when an identifier character follows, so
/// `"true"` is 1 while `"trueish"` and `"TRUE"` are not numbers at all.
fn match_word(chars: &[char], pos: &mut usize, word: &str) -> bool {
    let mut cursor = *pos;
    for expected in word.chars() {
        if chars.get(cursor) != Some(&expected) {
            return false;
        }
        cursor += 1;
    }
    if chars
        .get(cursor)
        .is_some_and(|ch| ch.is_alphabetic() || *ch == '_')
    {
        return false;
    }
    *pos = cursor;
    true
}

/// Official `TJSExtractNumber`: collects the numeric prefix, allowing one
/// decimal point and one exponent marker (with an optional sign and spaces
/// around it), and reports whether what it collected is a real.
fn extract_number(
    chars: &[char],
    pos: &mut usize,
    radix: u32,
    exponent_marks: [char; 2],
) -> (String, bool) {
    let mut text = String::new();
    let mut point_found = false;
    let mut exponent_found = false;
    while let Some(&ch) = chars.get(*pos) {
        if !exponent_found && ch.is_digit(radix) {
            text.push(ch);
            *pos += 1;
        } else if ch == '.' && !point_found && !exponent_found {
            point_found = true;
            text.push(ch);
            *pos += 1;
        } else if !exponent_found && exponent_marks.contains(&ch) {
            exponent_found = true;
            text.push(ch);
            *pos += 1;
            skip_space(chars, pos);
            if let Some(&sign) = chars.get(*pos).filter(|ch| **ch == '+' || **ch == '-') {
                text.push(sign);
                *pos += 1;
                skip_space(chars, pos);
            }
        } else if exponent_found && ch.is_ascii_digit() {
            text.push(ch);
            *pos += 1;
        } else {
            break;
        }
    }
    (text, point_found || exponent_found)
}

/// C `strtod` prefix semantics, which official uses for decimal reals: the
/// longest parsable prefix wins and an unparsable string is `0.0`. Rust's
/// parser rejects the dangling exponents `TJSExtractNumber` can hand over
/// (`"1e"`, `"1e+"`), so shrink the tail until it parses.
fn parse_real_prefix(text: &str) -> f64 {
    let mut end = text.len();
    while end > 0 {
        if let Ok(value) = text[..end].parse::<f64>() {
            return value;
        }
        end -= 1;
    }
    0.0
}

/// Official `TJSParseNonDecimalReal`, computed arithmetically instead of by
/// IEEE bit assembly: digits are weighted by `radix` around the point and the
/// `p` exponent is a power of two (`0x1.8p3` is 12).
fn parse_radix_real(text: &str, radix: u32) -> f64 {
    let (mantissa, exponent) = text.split_once(['p', 'P']).unwrap_or((text, ""));
    let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let base = f64::from(radix);
    let mut value = 0.0;
    for ch in integer.chars() {
        value = value * base + f64::from(ch.to_digit(radix).unwrap_or(0));
    }
    let mut weight = 1.0 / base;
    for ch in fraction.chars() {
        value += f64::from(ch.to_digit(radix).unwrap_or(0)) * weight;
        weight /= base;
    }
    let exponent = exponent.strip_prefix('+').unwrap_or(exponent);
    value * 2f64.powi(exponent.parse().unwrap_or(0))
}

/// Official `TJSParseNonDecimalNumber`. `bits` is the width of one digit, so
/// the integer form shifts rather than multiplies and wraps on overflow
/// exactly like the C original.
fn parse_radix_number(chars: &[char], pos: &mut usize, radix: u32, bits: u32) -> Option<Variant> {
    let (text, is_real) = extract_number(chars, pos, radix, ['P', 'p']);
    if text.is_empty() {
        return None;
    }
    if is_real {
        return Some(Variant::Real(parse_radix_real(&text, radix)));
    }
    let mut value: i64 = 0;
    for ch in text.chars() {
        value = value
            .wrapping_shl(bits)
            .wrapping_add(i64::from(ch.to_digit(radix).unwrap_or(0)));
    }
    Some(Variant::Integer(value))
}

/// Official `TJSParseNumber2`: the literal words, then the `0`-prefixed radix
/// forms, then decimal.
fn parse_number_body(chars: &[char], pos: &mut usize) -> Option<Variant> {
    for (word, value) in [
        ("true", Variant::Integer(1)),
        ("false", Variant::Integer(0)),
        ("NaN", Variant::Real(f64::NAN)),
        ("Infinity", Variant::Real(f64::INFINITY)),
    ] {
        if match_word(chars, pos, word) {
            return Some(value);
        }
    }
    if chars.get(*pos) == Some(&'0') {
        let Some(mark) = chars.get(*pos + 1).copied() else {
            *pos += 1;
            return Some(Variant::Integer(0));
        };
        match mark {
            'x' | 'X' => {
                *pos += 2;
                return parse_radix_number(chars, pos, 16, 4);
            }
            'b' | 'B' => {
                *pos += 2;
                return parse_radix_number(chars, pos, 2, 1);
            }
            // A 2^n exponent with no radix mantissa is not a number.
            'p' | 'P' => return None,
            // `0.5` / `0e3` are ordinary decimals; keep the leading zero.
            '.' | 'e' | 'E' => {}
            // Any other digit after a leading zero makes it octal, and the
            // leading zero counts as one of the octal digits.
            _ => return parse_radix_number(chars, pos, 8, 3),
        }
    }
    let (text, is_real) = extract_number(chars, pos, 10, ['E', 'e']);
    if text.is_empty() {
        return None;
    }
    if is_real {
        return Some(Variant::Real(parse_real_prefix(&text)));
    }
    let mut value: i64 = 0;
    for ch in text.chars() {
        value = value
            .wrapping_mul(10)
            .wrapping_add(i64::from(ch.to_digit(10).unwrap_or(0)));
    }
    Some(Variant::Integer(value))
}

/// Ports official `TJSParseNumber` (`tjsLex.cpp`), the one parser behind every
/// string-to-number conversion (`AsInteger`/`AsReal`/`AsNumber`, and therefore
/// unary `+` and string truthiness). It accepts an optional sign, the literal
/// words `true`/`false`/`NaN`/`Infinity`, `0x`/`0b`/octal integers, and a
/// longest-prefix decimal (`"12abc"` is 12); note that leading whitespace is
/// *not* skipped, so `" 1"` is not a number. `None` means nothing numeric
/// could be read, which every caller reports as 0.
///
/// KAG depends on this: official KAGParser keeps every tag attribute a string,
/// so `[history enabled=true]` only works because `+"true"` is 1 here.
fn parse_number(value: &str) -> Option<Variant> {
    let chars: Vec<char> = value.chars().collect();
    let mut pos = 0;
    let negative = match chars.first() {
        Some('+') | Some('-') => {
            pos = 1;
            skip_space(&chars, &mut pos);
            chars.first() == Some(&'-')
        }
        _ => false,
    };
    let value = parse_number_body(&chars, &mut pos)?;
    if !negative {
        return Some(value);
    }
    Some(match value {
        Variant::Integer(value) => Variant::Integer(value.wrapping_neg()),
        Variant::Real(value) => Variant::Real(-value),
        other => other,
    })
}

/// String to integer the way official `tTJSVariantString::ToInteger` does:
/// parse, then take the parsed variant as an integer.
fn string_to_integer(value: &str) -> i64 {
    match parse_number(value) {
        Some(Variant::Integer(value)) => value,
        Some(Variant::Real(value)) => value as i64,
        _ => 0,
    }
}

/// String to real the way official `tTJSVariantString::ToReal` does.
fn string_to_real(value: &str) -> f64 {
    match parse_number(value) {
        Some(Variant::Integer(value)) => value as f64,
        Some(Variant::Real(value)) => value,
        _ => 0.0,
    }
}

/// Renders a real the way official TJS2 does (`tjsVariant.cpp`
/// `TJSSpecialRealToString` + `%.15lg`): special forms `NaN`, `+Infinity`,
/// `-Infinity`, `+0.0`, `-0.0`, and otherwise C `%g` rules with 15
/// significant digits — exponent form when the decimal exponent is < -4 or
/// >= 15, trailing zeros stripped in both forms.
fn real_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value == f64::INFINITY {
        return "+Infinity".to_string();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_string();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "+0.0".to_string()
        };
    }

    let rendered = format!("{value:.14e}");
    let (mantissa, exponent) = rendered.split_once('e').expect("scientific notation");
    let exponent: i32 = exponent.parse().expect("valid exponent");
    let negative = mantissa.starts_with('-');
    let mantissa = mantissa.trim_start_matches('-');
    // 15 significant digits; strip trailing zeros (C `%g`).
    let mut digits: String = mantissa.chars().filter(|digit| *digit != '.').collect();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }

    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if !(-4..15).contains(&exponent) {
        out.push_str(&digits);
        if digits.len() > 1 {
            out.insert(1 + usize::from(negative), '.');
        }
        out.push_str(if exponent >= 0 { "e+" } else { "e-" });
        out.push_str(&format!("{:02}", exponent.unsigned_abs()));
    } else {
        let point_position = exponent + 1; // digits before the decimal point
        if point_position >= 1 {
            let before = point_position as usize;
            if before >= digits.len() {
                out.push_str(&digits);
                out.push_str(&"0".repeat(before - digits.len()));
            } else {
                out.push_str(&digits[..before]);
                out.push('.');
                out.push_str(&digits[before..]);
            }
        } else {
            out.push_str("0.");
            out.push_str(&"0".repeat((-point_position) as usize));
            out.push_str(&digits);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_to_string_matches_official_tjs2() {
        for (value, expected) in [
            (f64::NAN, "NaN"),
            (f64::INFINITY, "+Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (0.0, "+0.0"),
            (-0.0, "-0.0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (1.5, "1.5"),
            (0.1, "0.1"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e14, "100000000000000"),
            (1e15, "1e+15"),
            (123456.789, "123456.789"),
            (std::f64::consts::PI, "3.14159265358979"),
            (1.0 / 3.0, "0.333333333333333"),
            (2.5e-5, "2.5e-05"),
            (-12345.678, "-12345.678"),
        ] {
            assert_eq!(real_to_string(value), expected, "value={value:?}");
        }
    }

    #[test]
    fn null_follows_official_object_semantics() {
        // null is a tvtObject with NULL pointers in official TJS2.
        assert_eq!(Variant::Null.to_tjs_string().unwrap(), "(object 0x0:0x0)");
        assert!(Variant::Null.to_real().is_err());
        assert!(Variant::Null.to_integer().is_err());
        assert!(Variant::Null.to_number_variant().is_err());
        assert!(Variant::Null.to_octet().is_err());
        assert!(!Variant::Null.is_truthy());
        assert_eq!(Variant::Null.typeof_name(), "Object");
        assert!(Variant::Null.normal_eq(&Variant::Null));
        assert!(!Variant::Null.normal_eq(&Variant::String("null".to_string())));
        assert!(!Variant::Null.normal_eq(&Variant::Void));
        // official NormalCompare catches the null->number conversion error
        // and returns false for `null == 0` / `0 == null`
        assert!(!Variant::Null.normal_eq(&Variant::Integer(0)));
        assert!(!Variant::Integer(0).normal_eq(&Variant::Null));
        // `object == null` compares object pointers in official (false)
        let handle = ObjectHandle(3);
        assert!(!Variant::Null.normal_eq(&Variant::Object(handle)));
        assert!(!Variant::Object(handle).normal_eq(&Variant::Null));
        assert!(!Variant::Closure(Closure::new(handle, None)).normal_eq(&Variant::Null));
    }

    #[test]
    fn octet_string_conversion_throws_like_official() {
        assert!(Variant::Octet(vec![1, 2]).to_tjs_string().is_err());
        // official NormalCompare catches the AsString error -> false
        assert!(!Variant::String("x".to_string()).normal_eq(&Variant::Octet(vec![1])));
    }

    #[test]
    fn object_equality_ignores_bound_this() {
        let function = ObjectHandle(1);
        let other = ObjectHandle(2);
        let with_this_a = Closure::new(function, Some(ObjectHandle(9)));
        let with_this_b = Closure::new(function, Some(ObjectHandle(10)));
        // == compares only the underlying object (official NormalCompare)
        assert!(Variant::Closure(with_this_a).normal_eq(&Variant::Closure(with_this_b)));
        assert!(Variant::Closure(with_this_a).normal_eq(&Variant::Object(function)));
        assert!(!Variant::Closure(with_this_a).normal_eq(&Variant::Object(other)));
        // === still distinguishes the bound this
        assert!(!Variant::Closure(with_this_a).discern_eq(&Variant::Closure(with_this_b)));
        assert!(!Variant::Closure(with_this_a).discern_eq(&Variant::Object(function)));
    }

    #[test]
    fn string_to_number_matches_official_parse_number() {
        for (text, expected) in [
            // The literal words official recognises, word-matched and
            // case-sensitive.
            ("true", Some(Variant::Integer(1))),
            ("false", Some(Variant::Integer(0))),
            ("TRUE", None),
            ("trueish", None),
            ("Infinity", Some(Variant::Real(f64::INFINITY))),
            ("-Infinity", Some(Variant::Real(f64::NEG_INFINITY))),
            // Radix forms introduced by a leading zero.
            ("0x10", Some(Variant::Integer(16))),
            ("0XfF", Some(Variant::Integer(255))),
            ("0b101", Some(Variant::Integer(5))),
            ("010", Some(Variant::Integer(8))),
            ("08", Some(Variant::Integer(0))),
            ("0", Some(Variant::Integer(0))),
            ("0x1.8p3", Some(Variant::Real(12.0))),
            // Decimal, longest prefix.
            ("10", Some(Variant::Integer(10))),
            ("-42", Some(Variant::Integer(-42))),
            ("- 42", Some(Variant::Integer(-42))),
            ("12abc", Some(Variant::Integer(12))),
            ("1.5", Some(Variant::Real(1.5))),
            ("1e3", Some(Variant::Real(1000.0))),
            ("1e", Some(Variant::Real(1.0))),
            (".5", Some(Variant::Real(0.5))),
            // Nothing numeric at all; official skips no leading space.
            ("", None),
            ("abc", None),
            (" 1", None),
        ] {
            assert_eq!(parse_number(text), expected, "text={text:?}");
        }
        assert!(
            parse_number("NaN")
                .expect("NaN parses")
                .to_real()
                .expect("real")
                .is_nan()
        );
    }

    #[test]
    fn kag_boolean_attributes_coerce_like_official() {
        // Official KAGParser stores `[history enabled=true]` as the *string*
        // "true", and KAG3 reads it with `+elm.enabled`. That only works
        // because official string-to-number maps "true" to 1, so these two
        // behaviours have to ship together.
        let enabled = Variant::String("true".to_string());
        assert_eq!(enabled.to_integer().expect("integer"), 1);
        assert_eq!(enabled.to_real().expect("real"), 1.0);
        assert_eq!(
            enabled.to_number_variant().expect("number"),
            Variant::Integer(1)
        );
        assert!(enabled.is_truthy());
        // `!(+elm.store)` must stay an inversion of the attribute.
        assert!(!Variant::String("false".to_string()).is_truthy());
        // Official truthiness goes through AsInteger, so a fraction truncates.
        assert!(!Variant::String("0.5".to_string()).is_truthy());
        assert!(Variant::String("1.5".to_string()).is_truthy());
    }
}
