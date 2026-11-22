#![no_main]
//! See `matters_core::fuzzing::engine_import`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_core::fuzzing::engine_import(data));
