//! Boundary tests for every limit in spec section *Limits*: exactly at the
//! limit, and one byte over.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use bellwether::limits::*;
use bellwether::{
    Change, Event, FieldError, Id, MAX_BODY, ParseError, RecordStore, RemoveReason, Report,
    Scanner, State, parse,
};

fn p(s: &str) -> Result<Report, ParseError> {
    parse(s.as_bytes())
}

/// A body of exactly `len` bytes, padded with an unknown key.
fn body_of_len(len: usize) -> String {
    let mut s = String::from("state=idle:x=");
    s.extend(std::iter::repeat_n('a', len - s.len()));
    assert_eq!(s.len(), len);
    s
}

fn scan(input: &[u8]) -> (usize, usize) {
    let (mut seqs, mut over) = (0, 0);
    let mut s = Scanner::new();
    let mut on = |e: Event<'_>| match e {
        Event::Sequence { .. } => seqs += 1,
        Event::Oversized => over += 1,
        Event::Bytes(_) => {}
    };
    s.feed(input, &mut on);
    s.flush(&mut on);
    (seqs, over)
}

#[test]
fn whole_sequence_4096_st() {
    // 7 (prefix) + body + 2 (ESC \) = 4096
    let at = [b"\x1b]7501;", body_of_len(4087).as_bytes(), b"\x1b\\"].concat();
    assert_eq!(at.len(), MAX_SEQUENCE);
    assert_eq!(scan(&at), (1, 0));
    let over = [b"\x1b]7501;", body_of_len(4088).as_bytes(), b"\x1b\\"].concat();
    assert_eq!(over.len(), MAX_SEQUENCE + 1);
    assert_eq!(scan(&over), (0, 1));
}

#[test]
fn whole_sequence_4096_bel() {
    let at = [b"\x1b]7501;", body_of_len(4088).as_bytes(), b"\x07"].concat();
    assert_eq!(at.len(), MAX_SEQUENCE);
    assert_eq!(scan(&at), (1, 0));
    let over = [b"\x1b]7501;", body_of_len(4089).as_bytes(), b"\x07"].concat();
    assert_eq!(scan(&over), (0, 1));
}

#[test]
fn parse_body_limit() {
    assert_eq!(MAX_BODY, 4088);
    assert!(p(&body_of_len(MAX_BODY)).is_ok());
    assert_eq!(
        p(&body_of_len(MAX_BODY + 1)),
        Err(ParseError::TooLong { len: MAX_BODY + 1 })
    );
}

#[test]
fn key_length() {
    let k16 = "a".repeat(MAX_KEY);
    assert!(p(&format!("state=idle:{k16}=1")).is_ok());
    let k17 = "a".repeat(MAX_KEY + 1);
    assert_eq!(
        p(&format!("state=idle:{k17}=1")),
        Err(ParseError::KeyTooLong { key: k17 })
    );
}

#[test]
fn msg_encoded_and_decoded() {
    // 2048 bytes encodes to exactly 2732 characters (with one '=').
    let at = STANDARD.encode("a".repeat(MAX_MSG_DECODED));
    assert_eq!(at.len(), MAX_MSG_ENCODED);
    assert_eq!(
        p(&format!("state=done:msg={at}"))
            .unwrap()
            .msg()
            .unwrap()
            .len(),
        MAX_MSG_DECODED
    );
    // 2049 bytes also fits in 2732 characters (no padding), so this hits the
    // decoded limit.
    let dec_over = STANDARD_NO_PAD.encode("a".repeat(MAX_MSG_DECODED + 1));
    assert_eq!(dec_over.len(), MAX_MSG_ENCODED);
    assert_eq!(
        p(&format!("state=done:msg={dec_over}")),
        Err(ParseError::ValueTooLong {
            key: "msg",
            encoded: false,
            len: MAX_MSG_DECODED + 1,
            max: MAX_MSG_DECODED
        })
    );
    // One encoded byte over is caught before decoding, even when the base64
    // is invalid.
    let enc_over = format!("{at}=");
    assert_eq!(
        p(&format!("state=done:msg={enc_over}")),
        Err(ParseError::ValueTooLong {
            key: "msg",
            encoded: true,
            len: MAX_MSG_ENCODED + 1,
            max: MAX_MSG_ENCODED
        })
    );
}

#[test]
fn title_encoded() {
    // 192 bytes is exactly 256 characters. 256 characters can never decode to
    // more than 192 bytes, so on the wire only the encoded limit can be hit.
    let at = STANDARD.encode("a".repeat(MAX_TITLE_DECODED));
    assert_eq!(at.len(), MAX_TITLE_ENCODED);
    assert!(p(&format!("state=done:title={at}")).is_ok());
    assert!(matches!(
        p(&format!("state=done:title={at}YQ")),
        Err(ParseError::ValueTooLong {
            key: "title",
            encoded: true,
            ..
        })
    ));
}

#[test]
fn app_length() {
    let at = "a".repeat(MAX_APP);
    assert_eq!(
        p(&format!("state=done:app={at}"))
            .unwrap()
            .app()
            .unwrap()
            .as_str(),
        at
    );
    let over = "a".repeat(MAX_APP + 1);
    assert_eq!(
        p(&format!("state=done:app={over}")),
        Err(ParseError::ValueTooLong {
            key: "app",
            encoded: true,
            len: MAX_APP + 1,
            max: MAX_APP
        })
    );
}

#[test]
fn id_total_length() {
    let seg = "a".repeat(MAX_ID_SEGMENT);
    let at = format!("{seg}/{seg}/{seg}/{}", "b".repeat(29));
    assert_eq!(at.len(), MAX_ID);
    assert!(p(&format!("state=done:id={at}")).is_ok());
    let over = format!("{seg}/{seg}/{seg}/{}", "b".repeat(30));
    assert_eq!(over.len(), MAX_ID + 1);
    assert_eq!(
        p(&format!("state=done:id={over}")),
        Err(ParseError::InvalidId(FieldError::TooLong {
            len: MAX_ID + 1,
            max: MAX_ID
        }))
    );
}

#[test]
fn id_segment_length() {
    assert!(Id::new(&"a".repeat(MAX_ID_SEGMENT)).is_ok());
    assert_eq!(
        p(&format!(
            "state=done:id=x/{}",
            "a".repeat(MAX_ID_SEGMENT + 1)
        )),
        Err(ParseError::InvalidId(FieldError::SegmentTooLong {
            len: MAX_ID_SEGMENT + 1,
            max: MAX_ID_SEGMENT
        }))
    );
}

#[test]
fn id_depth() {
    let at = ["a"; MAX_ID_DEPTH].join("/");
    assert!(p(&format!("state=done:id={at}")).is_ok());
    let over = ["a"; MAX_ID_DEPTH + 1].join("/");
    assert_eq!(
        p(&format!("state=done:id={over}")),
        Err(ParseError::InvalidId(FieldError::TooDeep {
            depth: MAX_ID_DEPTH + 1,
            max: MAX_ID_DEPTH
        }))
    );
}

#[test]
fn records_per_terminal() {
    let mut store = RecordStore::new();
    let report = |i: usize| {
        Report::builder(State::Working)
            .id(Id::new(&format!("r{i}")).unwrap())
            .try_build()
            .unwrap()
    };
    for i in 0..MAX_RECORDS {
        let ch = store.apply(report(i));
        assert_eq!(ch.len(), 1, "no eviction while at or under the cap");
    }
    assert_eq!(store.len(), MAX_RECORDS);
    let ch = store.apply(report(MAX_RECORDS));
    assert_eq!(store.len(), MAX_RECORDS);
    assert_eq!(
        ch[0],
        Change::Removed {
            id: Some(Id::new("r0").unwrap()),
            reason: RemoveReason::Evicted
        }
    );
}
