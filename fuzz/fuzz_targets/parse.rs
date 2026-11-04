#![no_main]
//! `parse` must never panic. Every report it accepts must re-encode within
//! the limits and parse back to itself.

use bellwether::{Terminator, limits::MAX_SEQUENCE, parse};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(report) = parse(data) {
        let bytes = report.encode(Terminator::Bel);
        assert!(bytes.len() <= MAX_SEQUENCE);
        let body = &bytes[7..bytes.len() - 1];
        assert_eq!(parse(body), Ok(report));
    }
});
