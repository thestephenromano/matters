#![no_main]
//! See `matters_relay::fuzzing::request`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_relay::fuzzing::request(data));
