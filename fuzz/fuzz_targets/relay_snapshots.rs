#![no_main]
//! See `matters_sync_client::fuzzing::snapshots`.
libfuzzer_sys::fuzz_target!(|data: &[u8]| matters_sync_client::fuzzing::snapshots(data));
