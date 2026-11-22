#![no_main]
//! See `matters_relay::fuzzing::export`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_relay::fuzzing::export(data));
