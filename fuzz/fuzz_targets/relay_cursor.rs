#![no_main]
//! See `matters_relay::fuzzing::cursor`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_relay::fuzzing::cursor(data));
