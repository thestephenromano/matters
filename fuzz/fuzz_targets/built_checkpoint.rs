#![no_main]
//! See `matters_sync::fuzzing::built_checkpoint`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_sync::fuzzing::built_checkpoint(data));
