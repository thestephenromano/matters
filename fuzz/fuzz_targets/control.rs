#![no_main]
//! See `matters_sync::fuzzing::control`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_sync::fuzzing::control(data));
