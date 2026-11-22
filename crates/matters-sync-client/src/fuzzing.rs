//! Fuzz entry points for the client's parsers of relay responses (#45):
//! the bodies of the `relay_page` and `relay_snapshots` cargo-fuzz targets
//! in `fuzz/`, shared with the stable corpus replay test
//! (`tests/fuzz_replay.rs`), as `matters_sync::fuzzing` does for the sync
//! parsers.
//!
//! Each takes arbitrary bytes and must never panic. Beyond that, a page the
//! client's checks accept must be one the contract allows, whatever cursor
//! it is judged against.

use minicbor::Encoder;

use crate::{check_page, decode_cursor, parse_page, parse_snapshots, refusal, PageFault};
use crate::{MAX_RETRY_AFTER, PAGE};

fn cursor(generation: &[u8; 16], last: u64) -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.array(2)
        .and_then(|e| e.bytes(generation))
        .and_then(|e| e.u64(last))
        .expect("writing to a Vec cannot fail");
    e.into_writer()
}

/// A page of a record or authority log, and an error body.
pub fn page(data: &[u8]) {
    // The same bytes as an error body: its wait is always capped.
    let refused = refusal(429, data);
    assert!(refused.after.is_none_or(|after| after <= MAX_RETRY_AFTER));
    let Some(page) = parse_page(data) else {
        return;
    };
    let next = decode_cursor(&page.next);
    let mut afters = vec![None, Some(page.next.clone())];
    if let Some((generation, last)) = next {
        afters.push(Some(cursor(&generation, last.saturating_sub(1))));
        afters.push(Some(cursor(&generation, page.high_water.saturating_add(1))));
        afters.push(Some(cursor(&[0xee; 16], last)));
    }
    for after in afters {
        let judged = check_page(&page, after.as_deref());
        let last = after.as_deref().and_then(decode_cursor);
        if let Some((generation, at)) = last {
            if generation != page.generation || at > page.high_water {
                assert_eq!(judged, Err(PageFault::GenerationChanged));
                continue;
            }
        }
        if judged.is_err() {
            continue;
        }
        let from = last.map_or(0, |(_, at)| at);
        let (generation, next) = next.expect("an accepted page's cursor decodes");
        assert_eq!(generation, page.generation);
        assert!(page.entries.len() <= PAGE as usize);
        let mut previous = from;
        for (sequence, _) in &page.entries {
            assert!(*sequence > previous, "entries ascend past the cursor");
            previous = *sequence;
        }
        assert!(previous <= next && next <= page.high_water);
        assert_eq!(page.complete, next == page.high_water);
        assert!(page.complete || next > from, "an incomplete page advances");
    }
}

/// Whether the client accepts `data` as a first page (no cursor yet): for
/// the replay test to check that the seeds reach the checks' far side.
pub fn page_accepted(data: &[u8]) -> bool {
    parse_page(data).is_some_and(|page| check_page(&page, None).is_ok())
}

/// The relay's list of its newest snapshot records.
pub fn snapshots(data: &[u8]) {
    if let Some((_, records)) = parse_snapshots(data) {
        // Each entry takes at least three bytes of the body.
        assert!(records.len() <= data.len() / 3);
    }
}
