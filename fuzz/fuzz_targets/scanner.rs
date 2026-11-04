#![no_main]
//! The scanner must never panic, must stay within its buffer bound, and must
//! reproduce its input exactly, however the input is chunked. The first byte
//! picks the chunk size.

use bellwether::{Event, Scanner, limits::MAX_SEQUENCE};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&first, rest)) = data.split_first() else { return };
    let chunk = usize::from(first).max(1);
    let mut scanner = Scanner::new();
    let mut rebuilt = Vec::with_capacity(rest.len());
    let mut on = |ev: Event<'_>| match ev {
        Event::Bytes(b) | Event::Sequence { raw: b, .. } => rebuilt.extend_from_slice(b),
        Event::Oversized => {}
    };
    for c in rest.chunks(chunk) {
        scanner.feed(c, &mut on);
        assert!(scanner.buffered() <= MAX_SEQUENCE + 1);
    }
    scanner.flush(&mut on);
    assert_eq!(rebuilt, rest);
});
