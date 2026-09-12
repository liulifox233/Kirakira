use std::fmt;

use crate::runtime::{ObjectHandle, Variant};

pub type Result<T> = std::result::Result<T, TjsError>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub const fn empty(offset: usize) -> Self {
        Self {
            start: offset,
            end: offset,
        }
    }

    pub fn join(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TjsErrorKind {
    Lex,
    Parse,
    Mir,
    Bytecode,
    Verify,
    Runtime,
    /// A member read that found nothing. The reference reports
    /// `TJS_E_MEMBERNOTFOUND` here, and several callers branch on exactly that
    /// code instead of on the message -- `tTJSObjectProxy` falls through to
    /// its second object, the TYPEOFD opcode answers "undefined"
    /// (`tjsInterCodeExec.cpp:2134`).  It carries the same script-facing text
    /// as any other runtime error.
    MemberNotFound,
    /// A host operation cannot complete yet (for example a Web resource
    /// fetch). The VM preserves its call stack and retries the instruction
    /// after the host supplies the resource.
    ResourcePending,
    Codegen,
    DebugQuit,
    /// `TJS_E_NOTIMPL` (-1002). `TJS_DENY_NATIVE_PROP_SETTER` and the
    /// `PropSetByVS` fallback protocol treat this as "this object does not
    /// implement that operation": the reference retries the write through
    /// `PropSet` (`tjsInterCodeExec.cpp:1650`) and `TJSIsObjectValid` accepts
    /// it as a valid object (`tjsErrorDefs.h:57`).
    NotImpl,
    /// `TJS_E_INVALIDPARAM` (-1003). Natives return it when an argument is
    /// present but unusable.
    InvalidParam,
    /// `TJS_E_BADPARAMCOUNT` (-1004). The reference checks the argument count
    /// before any argument type, so this is the code a method reports for a
    /// wrong number of arguments no matter what was passed.
    BadParamCount,
    /// `TJS_E_INVALIDTYPE` (-1005). Calling something that is not a function
    /// (`tjsObject.cpp:1322`), and the code `tjsObject.cpp:1530` swallows when
    /// a property write falls through.
    InvalidType,
    /// `TJS_E_INVALIDOBJECT` (-1006). Reported for an object whose native
    /// instance has been invalidated (a closed layer, a released window).
    InvalidObject,
    /// `TJS_E_ACCESSDENYED` (-1007). Reading a property that has no getter,
    /// writing one that has no setter, and writing an official read-only
    /// property (`TJS_DENY_NATIVE_PROP_SETTER`).
    AccessDenied,
    /// `TJS_E_NATIVECLASSCRASH` (-1008). A native instance the class cannot
    /// use, for example a native method that has no instance behind `this`.
    NativeClassCrash,
}

impl TjsErrorKind {
    /// The numeric `tjs_error` value the reference associates with this kind.
    ///
    /// Natives return these codes and the VM converts them into exceptions
    /// (`TJSThrowFrom_tjs_error`, `tjsError.cpp:231-273`), which is why some
    /// call sites branch on the code rather than on the message
    /// (`TJS_E_MEMBERNOTFOUND` in the class-chain walks, `TJS_E_NOTIMPL` in
    /// the `PropSetByVS` fallback). The number is internal control flow:
    /// scripts only ever see `Exception.message` and `Exception.trace`
    /// (`tjsException.cpp:35-52`), never the code.
    ///
    /// Kinds that describe a compiler or host stage rather than a TJS
    /// operation have no code, and neither have the message-only exceptions
    /// the reference raises directly (`TJSNullAccess`,
    /// `TJSThrowVariantConvertError`).
    pub const fn tjs_error_code(self) -> Option<i32> {
        match self {
            Self::MemberNotFound => Some(-1001),
            Self::NotImpl => Some(-1002),
            Self::InvalidParam => Some(-1003),
            Self::BadParamCount => Some(-1004),
            Self::InvalidType => Some(-1005),
            Self::InvalidObject => Some(-1006),
            Self::AccessDenied => Some(-1007),
            Self::NativeClassCrash => Some(-1008),
            Self::Lex
            | Self::Parse
            | Self::Mir
            | Self::Bytecode
            | Self::Verify
            | Self::Runtime
            | Self::ResourcePending
            | Self::Codegen
            | Self::DebugQuit => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TjsError {
    pub kind: TjsErrorKind,
    pub span: Option<Span>,
    pub message: String,
    pub contexts: Vec<TjsErrorContext>,
    /// Handle of the original thrown object while it is still alive.  Event
    /// boundaries can pass that object directly to `System.exceptionHandler`
    /// instead of approximating it with a host error wrapper.
    pub exception_object: Option<ObjectHandle>,
    /// Name of the TJS class of an exception that escaped a `throw`.
    ///
    /// The native KRKR event loop passes the original exception object to
    /// `System.exceptionHandler`, so handlers can distinguish framework
    /// exceptions such as `ConductorException`.  Keep that bit of identity
    /// alongside the host error while the VM unwinds its call stack.
    pub exception_class: Option<String>,
    /// Message carried by the original thrown object, when it differs from
    /// the host-facing diagnostic assembled during VM unwinding.
    pub exception_message: Option<String>,
}

impl TjsError {
    pub fn new(kind: TjsErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            span: None,
            message: message.into(),
            contexts: Vec::new(),
            exception_object: None,
            exception_class: None,
            exception_message: None,
        }
    }

    pub fn at(kind: TjsErrorKind, span: Span, message: impl Into<String>) -> Self {
        Self {
            kind,
            span: Some(span),
            message: message.into(),
            contexts: Vec::new(),
            exception_object: None,
            exception_class: None,
            exception_message: None,
        }
    }

    pub fn lex(span: Span, message: impl Into<String>) -> Self {
        Self::at(TjsErrorKind::Lex, span, message)
    }

    pub fn parse(span: Span, message: impl Into<String>) -> Self {
        Self::at(TjsErrorKind::Parse, span, message)
    }

    pub fn mir(message: impl Into<String>) -> Self {
        Self::new(TjsErrorKind::Mir, message)
    }

    pub fn bytecode(message: impl Into<String>) -> Self {
        Self::new(TjsErrorKind::Bytecode, message)
    }

    pub fn verify(message: impl Into<String>) -> Self {
        Self::new(TjsErrorKind::Verify, message)
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(TjsErrorKind::Runtime, message)
    }

    pub fn resource_pending(name: impl Into<String>) -> Self {
        Self::new(TjsErrorKind::ResourcePending, name)
    }

    pub fn codegen(message: impl Into<String>) -> Self {
        Self::new(TjsErrorKind::Codegen, message)
    }

    /// `TJS_E_MEMBERNOTFOUND` (-1001) carrying the official script-visible
    /// text `Member "%1" does not exist` (`string_table_en.rc:15`,
    /// `IDS_TJS_MEMBER_NOT_FOUND`), the text `TJSThrowFrom_tjs_error` builds
    /// for a named miss (`tjsError.cpp:240-244`).
    ///
    /// Callers that branch on the code keep working: the kind, not the text,
    /// is the identity ([`TjsError::is_member_not_found`]).
    pub fn member_not_found(name: &str) -> Self {
        Self::new(
            TjsErrorKind::MemberNotFound,
            format!("Member \"{name}\" does not exist"),
        )
    }

    /// `TJS_E_MEMBERNOTFOUND` (-1001) for a lookup with no member name, the
    /// shape `TJSThrowFrom_tjs_error(hr, NULL)` reports
    /// (`string_table_en.rc:16`, `IDS_TJS_MEMBER_NOT_FOUND_NO_NAME_GIVEN`).
    pub fn member_not_found_without_name() -> Self {
        Self::new(TjsErrorKind::MemberNotFound, "Member does not exist")
    }

    /// `TJS_E_NOTIMPL` (-1002): `Called method is not implemented`
    /// (`string_table_en.rc:17`).
    pub fn not_implemented() -> Self {
        Self::new(TjsErrorKind::NotImpl, "Called method is not implemented")
    }

    /// `TJS_E_INVALIDPARAM` (-1003): `Invalid argument`
    /// (`string_table_en.rc:18`).
    pub fn invalid_param() -> Self {
        Self::new(TjsErrorKind::InvalidParam, "Invalid argument")
    }

    /// `TJS_E_BADPARAMCOUNT` (-1004): `Invalid argument count`
    /// (`string_table_en.rc:19`). The reference always checks the argument
    /// count before looking at the arguments themselves.
    pub fn bad_param_count() -> Self {
        Self::new(TjsErrorKind::BadParamCount, "Invalid argument count")
    }

    /// `TJS_E_INVALIDTYPE` (-1005): `Not a function or invalid
    /// method/property type` (`string_table_en.rc:20`). Calling a value that
    /// is not callable reports this (`tjsObject.cpp:1322-1327`).
    pub fn invalid_type() -> Self {
        Self::new(
            TjsErrorKind::InvalidType,
            "Not a function or invalid method/property type",
        )
    }

    /// `TJS_E_INVALIDOBJECT` (-1006): `The object is already invalidated`
    /// (`string_table_en.rc:40`).
    pub fn invalid_object() -> Self {
        Self::new(
            TjsErrorKind::InvalidObject,
            "The object is already invalidated",
        )
    }

    /// `TJS_E_ACCESSDENYED` (-1007): `Invalid operation for Read-only or
    /// Write-only property` (`string_table_en.rc:38`). The reference reports
    /// it for a read without a getter, a write without a setter, and a write
    /// to a property declared read-only.
    pub fn access_denied() -> Self {
        Self::new(
            TjsErrorKind::AccessDenied,
            "Invalid operation for Read-only or Write-only property",
        )
    }

    /// `TJS_E_NATIVECLASSCRASH` (-1008): `Invalid object context`
    /// (`string_table_en.rc:39`).
    pub fn native_class_crash() -> Self {
        Self::new(TjsErrorKind::NativeClassCrash, "Invalid object context")
    }

    /// `TJSRangeError`: `The value is out of the range`
    /// (`string_table_en.rc:37`, `IDS_TJS_RANGE_ERROR`; no `%1`).
    ///
    /// The reference raises it with `TJS_eTJSError(TJSRangeError)`
    /// (`tjsErrorInc.h:38` declares the message, `tjsInterCodeExec.cpp:75`
    /// throws it for a string/octet index outside the value), which is a
    /// plain script error without a `tjs_error` code, so the kind stays
    /// `Runtime` -- the same modelling as [`TjsError::null_access`].
    pub fn range_error() -> Self {
        Self::new(TjsErrorKind::Runtime, "The value is out of the range")
    }

    /// The reference's null-object failure, `Accessing to null object`
    /// (`string_table_en.rc:14`, `IDS_TJS_NULL_ACCESS`).
    ///
    /// `TJSThrowNullAccess` (`tjsVariant.cpp:177-180`) raises it as a plain
    /// script exception without a `tjs_error` code, so the kind stays
    /// `Runtime`.
    pub fn null_access() -> Self {
        Self::new(TjsErrorKind::Runtime, "Accessing to null object")
    }

    /// The reference's conversion failure, `IDS_TJS_VARIANT_CONVERT_ERROR`:
    /// `Cannot convert the variable type (%1 to %2)` (`string_table_en.rc:7`),
    /// with `%1` rendered by `TJSVariantToReadableString` (`tjsUtils.cpp:50`)
    /// and `%2` by `TJSVariantTypeToTypeString` (`tjsUtils.cpp:36-48`, whose
    /// names are lower-case: `string`, `int`, `void`, ...).
    ///
    /// `TJSThrowVariantConvertError` (`tjsVariant.cpp:142-151`) raises it as a
    /// plain script exception, so the kind stays `Runtime`.
    pub fn variant_convert(value: &Variant, target_type: &str) -> Self {
        Self::new(
            TjsErrorKind::Runtime,
            format!(
                "Cannot convert the variable type ({} to {target_type})",
                readable_value(value)
            ),
        )
    }

    /// The reference's object-conversion failure: `Cannot convert the
    /// variable type (%1 to Object)` (`string_table_en.rc:8`,
    /// `IDS_TJS_VARIANT_CONVERT_ERROR_TO_OBJECT`), with `%1` rendered by
    /// `TJSVariantToReadableString` (`tjsUtils.cpp:50`).
    ///
    /// `TJSThrowVariantConvertError` (`tjsVariant.cpp:142-151`) raises it as a
    /// plain script exception, so the kind stays `Runtime`.
    pub fn variant_convert_to_object(value: &Variant) -> Self {
        Self::variant_convert(value, "Object")
    }

    /// Raised when the user quits an interactive debug session. It is never
    /// converted into a catchable TJS exception.
    pub fn debug_quit() -> Self {
        Self::new(TjsErrorKind::DebugQuit, "debug session terminated")
    }

    pub fn is_debug_quit(&self) -> bool {
        self.kind == TjsErrorKind::DebugQuit
    }

    /// True when the read found no member at all, the shape KRKR reports as
    /// `TJS_E_MEMBERNOTFOUND`.
    pub fn is_member_not_found(&self) -> bool {
        self.kind == TjsErrorKind::MemberNotFound
    }

    /// The official `tjs_error` code behind this failure, when it has one.
    /// See [`TjsErrorKind::tjs_error_code`].
    pub fn tjs_error_code(&self) -> Option<i32> {
        self.kind.tjs_error_code()
    }

    pub fn with_context(mut self, context: TjsErrorContext) -> Self {
        self.contexts.push(context);
        self
    }

    pub fn with_exception_class(mut self, class: impl Into<String>) -> Self {
        let class = class.into();
        if self.exception_class.is_none() {
            self.exception_class = Some(class);
        }
        self
    }

    pub fn with_exception_message(mut self, message: impl Into<String>) -> Self {
        self.exception_message = Some(message.into());
        self
    }

    pub fn with_stack_frame(self, frame: TjsStackFrame) -> Self {
        self.with_context(TjsErrorContext::StackFrame(frame))
    }

    pub fn with_member_access(self, access: TjsMemberAccess) -> Self {
        self.with_context(TjsErrorContext::MemberAccess(access))
    }
}

impl fmt::Display for TjsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MAX_CONTEXTS: usize = 24;
        match self.span {
            Some(span) => write!(
                f,
                "{:?} error at {}..{}: {}",
                self.kind, span.start, span.end, self.message
            ),
            None => write!(f, "{:?} error: {}", self.kind, self.message),
        }?;
        for context in self.contexts.iter().take(MAX_CONTEXTS) {
            write!(f, "\n  {context}")?;
        }
        if self.contexts.len() > MAX_CONTEXTS {
            write!(
                f,
                "\n  ... {} more context entries omitted",
                self.contexts.len() - MAX_CONTEXTS
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for TjsError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TjsErrorContext {
    StackFrame(TjsStackFrame),
    MemberAccess(TjsMemberAccess),
}

impl fmt::Display for TjsErrorContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StackFrame(frame) => write!(f, "at {frame}"),
            Self::MemberAccess(access) => write!(f, "while {access}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TjsStackFrame {
    pub storage: Option<String>,
    pub object_name: String,
    pub context: String,
    pub bytecode_offset: usize,
    pub source: Option<TjsSourceLocation>,
}

impl fmt::Display for TjsStackFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(storage) = &self.storage {
            write!(f, "{storage}:")?;
        }
        write!(
            f,
            "{} [{}] bytecode {}",
            self.object_name, self.context, self.bytecode_offset
        )?;
        if let Some(source) = &self.source {
            write!(f, " ({source})")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TjsSourceLocation {
    pub storage: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub utf16_offset: Option<usize>,
}

impl fmt::Display for TjsSourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(storage) = &self.storage {
            write!(f, "{storage}")?;
        } else {
            write!(f, "source")?;
        }
        match (self.line, self.column) {
            (Some(line), Some(column)) => write!(f, ":{line}:{column}"),
            _ => match self.utf16_offset {
                Some(offset) => write!(f, " utf16-offset {offset}"),
                None => Ok(()),
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TjsMemberAccess {
    pub operation: TjsMemberOperation,
    /// The object the access ran on. A call through a *value* rather than a
    /// named member has no receiver to name -- the call opcodes take their
    /// callee from a register -- so this field holds the value being called
    /// there (see [`TjsMemberAccess::member_name`]).
    pub receiver_type: String,
    /// The member name, or `None` for a call through a value: `VM_CALL` and
    /// `VM_NEW` carry no member name (the reference compiles `new a.b()` as a
    /// property read followed by `VM_NEW`, `tjsInterCodeGen.cpp:1819`), so the
    /// call site is identified by the callee's kind instead.
    pub member_name: Option<String>,
    /// Kind of the value a member read resolved to (`object<Scripts>#133`,
    /// `NativeFunction#134`, ...), for the call sites that reached one.
    pub callee_type: Option<String>,
}

impl fmt::Display for TjsMemberAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.member_name {
            Some(member_name) => write!(
                f,
                "{} member `{}` on {}",
                self.operation, member_name, self.receiver_type
            )?,
            None => write!(f, "{} {}", self.operation, self.receiver_type)?,
        }
        if let Some(callee_type) = &self.callee_type {
            write!(f, " with callee {callee_type}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TjsMemberOperation {
    Getting,
    Setting,
    Calling,
    Deleting,
    /// The `new` opcode's call: `VM_NEW` runs `CreateNew` rather than
    /// `FuncCall` (`tjsInterCodeExec.cpp:2357-2400`), and a value whose
    /// default `CreateNew` cannot construct reports the same
    /// `TJS_E_INVALIDTYPE` as the `FuncCall` miss (`tjsObject.cpp:1800-1804`).
    Constructing,
}

impl fmt::Display for TjsMemberOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Getting => write!(f, "getting"),
            Self::Setting => write!(f, "setting"),
            Self::Calling => write!(f, "calling"),
            Self::Deleting => write!(f, "deleting"),
            Self::Constructing => write!(f, "constructing"),
        }
    }
}

/// Renders a value the way `TJSVariantToReadableString` (`tjsUtils.cpp:50`)
/// does, for the shapes an object conversion can observe: `(void)`,
/// `(int)5`, `(real)1.5`, `(string)"a\nb"`, `(octet)<% 00 01 %>`. A value that
/// *is* an object never needs the rendering (it converts), so the object
/// forms are not reachable from here.
fn readable_value(value: &Variant) -> String {
    // `TJSVariantToReadableString`'s default `maxlen` (`tjsUtils.h:114`).
    const MAX_LEN: usize = 512;
    let rendered = match value {
        Variant::Void => "(void)".to_string(),
        Variant::Null => "(object)".to_string(),
        Variant::Integer(value) => format!("(int){value}"),
        Variant::Real(value) => {
            format!("(real){}", crate::runtime::value::real_to_string(*value))
        }
        Variant::String(value) => format!("(string)\"{}\"", escape_c(value)),
        Variant::Octet(value) => {
            let mut rendered = String::from("(octet)<% ");
            for (index, byte) in value.iter().enumerate() {
                if index > 0 {
                    rendered.push(' ');
                }
                rendered.push_str(&format!("{byte:02X}"));
            }
            rendered.push_str(" %>");
            rendered
        }
        Variant::Object(_) | Variant::Closure(_) | Variant::CodeObject(_) => "(object)".to_string(),
    };
    trim_length(rendered, MAX_LEN)
}

/// `TJSTrimStringLength` (`tjsUtils.cpp:20`): cap the rendering at `len`
/// characters, the last three of which become an ellipsis.
fn trim_length(value: String, len: usize) -> String {
    if value.chars().count() <= len {
        return value;
    }
    let mut kept: Vec<char> = value.chars().take(len).collect();
    if len >= 3 {
        kept[len - 3..].copy_from_slice(&['.', '.', '.']);
    }
    kept.into_iter().collect()
}

/// `tTJSString::EscapeC` (`tjsString.cpp:199`), including its rule that a
/// hex digit right after a `\xNN` escape is escaped as well so the two
/// escapes cannot merge when the text is read back.
fn escape_c(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    let mut after_hex_escape = false;
    for ch in value.chars() {
        match ch {
            '\u{07}' => {
                escaped.push_str("\\a");
                after_hex_escape = false;
            }
            '\u{08}' => {
                escaped.push_str("\\b");
                after_hex_escape = false;
            }
            '\u{0c}' => {
                escaped.push_str("\\f");
                after_hex_escape = false;
            }
            '\n' => {
                escaped.push_str("\\n");
                after_hex_escape = false;
            }
            '\r' => {
                escaped.push_str("\\r");
                after_hex_escape = false;
            }
            '\t' => {
                escaped.push_str("\\t");
                after_hex_escape = false;
            }
            '\u{0b}' => {
                escaped.push_str("\\v");
                after_hex_escape = false;
            }
            '\\' => {
                escaped.push_str("\\\\");
                after_hex_escape = false;
            }
            '\'' => {
                escaped.push_str("\\'");
                after_hex_escape = false;
            }
            '"' => {
                escaped.push_str("\\\"");
                after_hex_escape = false;
            }
            ch => {
                if (after_hex_escape && ch.is_ascii_hexdigit()) || (ch as u32) < 0x20 {
                    escaped.push_str(&format!("\\x{:02x}", ch as u32));
                    after_hex_escape = true;
                } else {
                    escaped.push(ch);
                    after_hex_escape = false;
                }
            }
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_kinds_carry_the_official_numeric_codes() {
        // `tjsErrorDefs.h:26-33`.
        for (kind, code) in [
            (TjsErrorKind::MemberNotFound, -1001),
            (TjsErrorKind::NotImpl, -1002),
            (TjsErrorKind::InvalidParam, -1003),
            (TjsErrorKind::BadParamCount, -1004),
            (TjsErrorKind::InvalidType, -1005),
            (TjsErrorKind::InvalidObject, -1006),
            (TjsErrorKind::AccessDenied, -1007),
            (TjsErrorKind::NativeClassCrash, -1008),
        ] {
            assert_eq!(kind.tjs_error_code(), Some(code), "{kind:?}");
            assert_eq!(TjsError::new(kind, "").tjs_error_code(), Some(code));
        }
        // Compiler and host stages have no `tjs_error` value.
        for kind in [
            TjsErrorKind::Lex,
            TjsErrorKind::Parse,
            TjsErrorKind::Mir,
            TjsErrorKind::Bytecode,
            TjsErrorKind::Verify,
            TjsErrorKind::Runtime,
            TjsErrorKind::ResourcePending,
            TjsErrorKind::Codegen,
            TjsErrorKind::DebugQuit,
        ] {
            assert_eq!(kind.tjs_error_code(), None, "{kind:?}");
        }
    }

    #[test]
    fn error_constructors_use_the_official_english_texts() {
        // `string_table_en.rc:7-40`, the texts `TJSThrowFrom_tjs_error`
        // selects by code (`tjsError.cpp:231-273`).
        assert_eq!(
            TjsError::member_not_found("children").message,
            "Member \"children\" does not exist"
        );
        assert_eq!(
            TjsError::member_not_found("children").kind,
            TjsErrorKind::MemberNotFound
        );
        assert_eq!(
            TjsError::member_not_found_without_name().message,
            "Member does not exist"
        );
        assert_eq!(
            TjsError::not_implemented().message,
            "Called method is not implemented"
        );
        assert_eq!(TjsError::invalid_param().message, "Invalid argument");
        assert_eq!(
            TjsError::bad_param_count().message,
            "Invalid argument count"
        );
        assert_eq!(
            TjsError::invalid_type().message,
            "Not a function or invalid method/property type"
        );
        assert_eq!(
            TjsError::access_denied().message,
            "Invalid operation for Read-only or Write-only property"
        );
        assert_eq!(
            TjsError::invalid_object().message,
            "The object is already invalidated"
        );
        assert_eq!(
            TjsError::native_class_crash().message,
            "Invalid object context"
        );
        assert_eq!(TjsError::null_access().message, "Accessing to null object");
    }

    #[test]
    fn member_not_found_identity_survives_the_text_change() {
        // Five call sites branch on the kind rather than the message.
        assert!(TjsError::member_not_found("x").is_member_not_found());
        assert!(TjsError::member_not_found_without_name().is_member_not_found());
        assert!(!TjsError::access_denied().is_member_not_found());
        assert!(!TjsError::invalid_type().is_member_not_found());
    }

    #[test]
    fn variant_conversion_renders_values_like_the_reference() {
        // `TJSVariantToReadableString` (`tjsUtils.cpp:50`) tags the type.
        for (value, expected) in [
            (Variant::Void, "(void)"),
            (Variant::Integer(5), "(int)5"),
            (Variant::Real(1.5), "(real)1.5"),
            (Variant::Real(1.0 / 3.0), "(real)0.333333333333333"),
            (Variant::String("a\nb".to_string()), "(string)\"a\\nb\""),
            (
                Variant::String("quote\"".to_string()),
                "(string)\"quote\\\"\"",
            ),
            (Variant::Octet(vec![0, 1, 255]), "(octet)<% 00 01 FF %>"),
        ] {
            assert_eq!(
                TjsError::variant_convert_to_object(&value).message,
                format!("Cannot convert the variable type ({expected} to Object)"),
                "value={value:?}"
            );
        }
    }

    #[test]
    fn hex_escapes_do_not_merge_with_a_following_hex_digit() {
        // `EscapeC` (`tjsString.cpp:199-245`) escapes a hex digit that comes
        // right after a `\xNN` escape so the pair cannot be read as one, and
        // the escape chain continues while the next character is still a hex
        // digit.
        assert_eq!(
            TjsError::variant_convert_to_object(&Variant::String("\u{1}ab".to_string())).message,
            "Cannot convert the variable type ((string)\"\\x01\\x61\\x62\" to Object)"
        );
    }

    #[test]
    fn long_values_are_trimmed_like_the_reference() {
        // `TJSTrimStringLength` keeps the first `maxlen` characters and writes
        // an ellipsis over the last three, on the whole rendering (so a long
        // string loses its closing quote).
        let value = Variant::String("x".repeat(600));
        let expected = format!(
            "Cannot convert the variable type ((string)\"{}... to Object)",
            "x".repeat(500)
        );
        assert_eq!(
            TjsError::variant_convert_to_object(&value).message,
            expected
        );
    }

    #[test]
    fn member_access_context_renders_named_and_valueless_accesses() {
        fn access(
            operation: TjsMemberOperation,
            member_name: Option<&str>,
            callee_type: Option<&str>,
        ) -> TjsMemberAccess {
            TjsMemberAccess {
                operation,
                receiver_type: "object<Scripts>#133".to_string(),
                member_name: member_name.map(str::to_string),
                callee_type: callee_type.map(str::to_string),
            }
        }

        // A named member access keeps its original sentence, both on its own
        // and through the context wrapper.
        let named = access(
            TjsMemberOperation::Calling,
            Some("newMenuStorage"),
            Some("NativeFunction#134"),
        );
        assert_eq!(
            named.to_string(),
            "calling member `newMenuStorage` on object<Scripts>#133 with callee NativeFunction#134"
        );
        assert_eq!(
            TjsError::runtime("").with_member_access(named).contexts[0].to_string(),
            "while calling member `newMenuStorage` on object<Scripts>#133 with callee NativeFunction#134"
        );
        // A call through a value has no member name to show: `VM_CALL` and
        // `VM_NEW` carry none, so the callee's kind identifies the call site
        // instead.
        assert_eq!(
            access(TjsMemberOperation::Constructing, None, None).to_string(),
            "constructing object<Scripts>#133"
        );
        assert_eq!(
            access(TjsMemberOperation::Calling, None, None).to_string(),
            "calling object<Scripts>#133"
        );
        assert_eq!(TjsMemberOperation::Constructing.to_string(), "constructing");
    }
}
