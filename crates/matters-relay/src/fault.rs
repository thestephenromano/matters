//! Crash points for the conformance runner (#38): with the
//! `fault-injection` feature, a relay whose `MATTERS_RELAY_CRASH_AT` names
//! the point exits on the spot, as a crash would, without unwinding,
//! committing or answering. Without the feature these are no-ops.

pub(crate) fn crash_point(point: &str) {
    #[cfg(feature = "fault-injection")]
    if std::env::var("MATTERS_RELAY_CRASH_AT").as_deref() == Ok(point) {
        std::process::exit(86);
    }
    #[cfg(not(feature = "fault-injection"))]
    let _ = point;
}
