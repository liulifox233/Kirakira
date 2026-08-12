#![forbid(unsafe_code)]

//! TJS2 compatibility crate.
//!
//! The crate is intentionally split around an explicit compatibility pipeline:
//!
//! ```text
//! source -> frontend -> HIR -> MIR -> bytecode -> verifier -> VM
//! binary .tjs2 ------------------------^
//! ```
//!
//! The bytecode parser, verifier, disassembler, and the frontend-to-MIR
//! lowering path are wired and tested. Bytecode emission from MIR is still a
//! later compatibility layer.

pub mod bytecode;
pub mod compiler;
pub mod debug;
pub mod decompile;
pub mod error;
pub mod frontend;
pub mod runtime;
pub mod vm;

pub use compiler::{compile_source_to_bytecode, compile_source_to_mir, execute_source};
pub use error::{
    Result, Span, TjsError, TjsErrorContext, TjsErrorKind, TjsMemberAccess, TjsMemberOperation,
    TjsSourceLocation, TjsStackFrame,
};
pub use frontend::{
    Diagnostic, DiagnosticSeverity, FrontendOptions, FrontendOutput, SourceLocation,
    analyze_script, parse_expression, parse_script,
};
