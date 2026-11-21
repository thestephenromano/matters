#![no_main]
//! See `matters_sync::fuzzing::record`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_sync::fuzzing::record(data));
