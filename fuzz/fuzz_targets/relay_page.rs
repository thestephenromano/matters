#![no_main]
//! See `matters_sync_client::fuzzing::page`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_sync_client::fuzzing::page(data));
