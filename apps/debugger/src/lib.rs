//! Shared pieces of the `krkr-debug` tooling.
//!
//! `krkr-debug` (headless, deterministic) and `krkr-desktop` (windowed,
//! real-time) both embed the same interactive console so an agent can inspect
//! either process over stdin or a FIFO.

pub mod cli;
pub mod console;
pub mod snapshot;
