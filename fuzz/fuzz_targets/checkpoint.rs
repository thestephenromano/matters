#![no_main]
//! See `matters_sync::fuzzing::checkpoint`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_sync::fuzzing::checkpoint(data));
