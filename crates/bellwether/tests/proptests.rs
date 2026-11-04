//! Property tests: encode/parse round trip, and Scanner chunking invariance.

use bellwether::{
    App, Event, Id, Kind, Message, Progress, Report, Scanner, State, Terminator, parse,
};
use proptest::prelude::*;

fn state() -> impl Strategy<Value = State> {
    prop_oneof![
        Just(State::Idle),
        Just(State::Working),
        Just(State::Done),
        Just(State::Blocked),
        Just(State::Error),
        Just(State::Clear),
    ]
}

fn kind() -> impl Strategy<Value = Kind> {
    prop_oneof![
        Just(Kind::Permission),
        Just(Kind::Question),
        Just(Kind::Auth)
    ]
}

fn id() -> impl Strategy<Value = Id> {
    prop::collection::vec("[A-Za-z0-9_.+-]{1,32}", 1..=8)
        .prop_filter_map("id too long", |segs| Id::new(&segs.join("/")).ok())
}

/// Free text with no control characters. Mostly short, sometimes near the
/// limit.
fn text(max_bytes: usize) -> impl Strategy<Value = String> {
    prop_oneof![
        "[^\\p{Cc}]{0,40}",
        Just("x".repeat(max_bytes)),
        "[^\\p{Cc}]{0,600}".prop_map(move |s| {
            let mut out = String::new();
            for c in s.chars() {
                if out.len() + c.len_utf8() > max_bytes {
                    break;
                }
                out.push(c);
            }
            out
        }),
    ]
}

prop_compose! {
    fn report()(
        state in state(),
        kind in proptest::option::of(kind()),
        progress in proptest::option::of(0u8..=100),
        app in proptest::option::of("[A-Za-z0-9_.+-]{1,32}"),
        id in proptest::option::of(id()),
        title in proptest::option::of(text(192)),
        msg in proptest::option::of(text(2048)),
    ) -> Report {
        let mut b = Report::builder(state).maybe_id(id);
        if let Some(k) = kind.filter(|_| state.allows_kind()) { b = b.kind(k); }
        if let Some(p) = progress.filter(|_| state.allows_progress()) {
            b = b.progress(Progress::new(p).unwrap());
        }
        if let Some(a) = app { b = b.app(App::new(&a).unwrap()); }
        if let Some(t) = title { b = b.title(t); }
        if let Some(m) = msg { b = b.msg(m); }
        b.try_build().unwrap()
    }
}

fn body(encoded: &[u8], t: Terminator) -> &[u8] {
    &encoded[7..encoded.len() - t.as_bytes().len()]
}

/// Filler that contains no ESC, so it can't interact with sequences, plus
/// some whole non-7501 escape sequences.
fn filler() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        prop::collection::vec(any::<u8>().prop_filter("no ESC", |b| *b != 0x1b), 0..64),
        Just(b"\x1b[31m".to_vec()),
        Just(b"\x1b]0;title\x07".to_vec()),
        Just(b"\x1b]7502;nope\x1b\\".to_vec()),
        Just(b"\x1b]750x".to_vec()), // diverges from the prefix late
        Just(b"\x1b".to_vec()),      // lone ESC right before the next item
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn encode_then_parse_round_trips(r in report(), bel in any::<bool>()) {
        let t = if bel { Terminator::Bel } else { Terminator::St };
        let bytes = r.encode(t);
        prop_assert!(bytes.len() <= bellwether::limits::MAX_SEQUENCE);
        prop_assert_eq!(parse(body(&bytes, t)), Ok(r));
    }

    #[test]
    fn scanner_is_chunking_invariant(
        items in prop::collection::vec((filler(), report(), any::<bool>()), 0..8),
        tail in filler(),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..12),
    ) {
        let mut stream = Vec::new();
        let mut expected = Vec::new();
        for (fill, r, bel) in items {
            stream.extend_from_slice(&fill);
            stream.extend(r.encode(if bel { Terminator::Bel } else { Terminator::St }));
            expected.push(r);
        }
        stream.extend_from_slice(&tail);

        // Split the stream at arbitrary points.
        let mut points: Vec<usize> = cuts.iter().map(|i| i.index(stream.len() + 1)).collect();
        points.sort_unstable();
        points.dedup();

        let mut scanner = Scanner::new();
        let mut found = Vec::new();
        let mut rebuilt = Vec::new();
        let mut on = |ev: Event<'_>| match ev {
            Event::Bytes(b) => rebuilt.extend_from_slice(b),
            Event::Sequence { raw, message, .. } => {
                rebuilt.extend_from_slice(raw);
                match message {
                    Message::Report(r) => found.push(r.unwrap()),
                    other => panic!("unexpected {other:?}"),
                }
            }
            Event::Oversized => panic!("oversized"),
        };
        let mut start = 0;
        for &p in &points {
            scanner.feed(&stream[start..p], &mut on);
            start = p;
        }
        scanner.feed(&stream[start..], &mut on);
        scanner.flush(&mut on);

        prop_assert_eq!(found, expected);
        prop_assert_eq!(rebuilt, stream);
    }

    /// Arbitrary bytes never panic or leak memory, and the stream is always
    /// reproduced exactly.
    #[test]
    fn scanner_reproduces_arbitrary_input(
        data in prop::collection::vec(
            prop_oneof![any::<u8>(), Just(0x1b), Just(b']'), Just(0x07)], 0..2000),
        chunk in 1usize..64,
    ) {
        let mut seeded = b"\x1b]7501;".to_vec();
        seeded.extend_from_slice(&data);
        let mut scanner = Scanner::new();
        let mut rebuilt = Vec::new();
        for c in seeded.chunks(chunk) {
            scanner.feed(c, |ev| match ev {
                Event::Bytes(b) | Event::Sequence { raw: b, .. } => rebuilt.extend_from_slice(b),
                Event::Oversized => {}
            });
            prop_assert!(scanner.buffered() <= bellwether::limits::MAX_SEQUENCE + 1);
        }
        scanner.flush(|ev| if let Event::Bytes(b) = ev { rebuilt.extend_from_slice(b) });
        prop_assert_eq!(rebuilt, seeded);
    }
}
