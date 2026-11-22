#![no_main]
//! See `matters_core::fuzzing::receive`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_core::fuzzing::receive(data));
