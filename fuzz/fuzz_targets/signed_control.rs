#![no_main]
//! See `matters_sync::fuzzing::signed_control`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_sync::fuzzing::signed_control(data));
