//! `fftgraph.dll` placeholder.
//!
//! Real plugin: FFT spectrum analysis of playing audio for visualisers.
//! Upstream: https://github.com/krkrz/fftgraph
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(FftGraphPlugin, "fftgraph.dll");
