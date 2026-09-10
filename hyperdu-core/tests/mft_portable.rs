//! Exercise the on-disk parsers on Linux too; no volume or Windows APIs used.
#![cfg(not(windows))]

pub use hyperdu_core::{Stat, StatMap};

#[path = "../src/platform/windows_impl/mft.rs"]
mod mft;
#[path = "../src/platform/windows_impl/mft_reader.rs"]
mod mft_reader;

#[path = "../src/simd.rs"]
mod simd;
