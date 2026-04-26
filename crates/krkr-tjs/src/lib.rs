mod bytecode;
mod call_frame;
mod error;
mod kag3_boot;
mod runtime;
pub mod value;

pub use bytecode::{Bytecode, Instruction, Vm};
pub use call_frame::{CallFrame, CallFrameStack};
pub use error::{RuntimeError, RuntimeErrorKind, TjsResult};
pub use kag3_boot::{Kag3BootPlan, scan_kag3_boot_plan};
pub use runtime::{ExecutionMode, Runtime, RuntimeOptions};
pub use value::{
    ComparisonOp, NumberValue, ObjectValue, Octet, Value, ValueType, add_values, compare_values,
    div_values, equal_values, greater_or_equal_values, greater_than_values, less_or_equal_values,
    less_than_values, mul_values, not_equal_values, rem_values, strict_equal_values, sub_values,
};
