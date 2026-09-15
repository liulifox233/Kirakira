//! Shared test scaffolding: the synthetic PSB writer used by the adapter's
//! own tests and by `crates/krkr-plugins`' motionplayer tests.
//!
//! A test binary that declares `mod support;` gets every helper here, so the
//! ones it does not call would read as dead code; the `allow` keeps each
//! binary's warnings to the helpers it actually has.
#![allow(dead_code)]

pub mod player_fixture;
pub mod psb_write;
