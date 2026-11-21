#![no_main]
//! See `matters_sync::fuzzing::typed`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_sync::fuzzing::typed(data));
