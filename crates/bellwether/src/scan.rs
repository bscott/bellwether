use crate::detect::{Support, parse_reply};
use crate::error::{DetectError, ParseError};
use crate::limits::MAX_SEQUENCE;
use crate::parse::parse;
use crate::report::Report;
use crate::{PREFIX, trim_ws};

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// What a complete OSC 7501 sequence turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// A report, or the reason it was discarded (spec: *Syntax*).
    Report(Result<Report, ParseError>),
    /// A body of exactly `?`. Sent by a program, it is a feature detection
    /// query. Sent by a terminal, it is a minimal reply meaning the default
    /// states and kinds are supported (spec: *Feature detection*).
    Query,
    /// A feature detection reply with pairs, such as `? : states=…`.
    Reply(Result<Support, DetectError>),
}

/// One item produced by [`Scanner::feed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    /// Bytes that are not part of a complete, in-limit OSC 7501 sequence. Pass
    /// them through unchanged.
    Bytes(&'a [u8]),
    /// A complete OSC 7501 sequence.
    Sequence {
        /// The whole sequence, `ESC ] 7501 ;` through ST.
        raw: &'a [u8],
        /// The body between `ESC ] 7501 ;` and ST.
        body: &'a [u8],
        /// What the body contains.
        message: Message,
    },
    /// An OSC 7501 sequence grew past 4096 bytes and was discarded (spec:
    /// *Limits*). Its bytes are still emitted as [`Event::Bytes`], so the
    /// stream stays unchanged.
    Oversized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum St {
    /// Plain bytes.
    Ground,
    /// `buf` holds the first `n` bytes of `PREFIX`.
    Prefix(usize),
    /// `buf` holds `PREFIX` and part of a body.
    Body,
    /// `buf` ends with an ESC inside the body: ST or abort.
    BodyEsc,
    /// Inside a sequence that went over the limit. Bytes pass straight through
    /// until it ends.
    Overflow,
    /// `buf` holds one ESC seen in `Overflow`.
    OverflowEsc,
}

/// Incremental scanner for OSC 7501 sequences in a byte stream (spec:
/// *Syntax*, *Limits*).
///
/// * Sequences can be split across any number of [`feed`](Self::feed) calls.
/// * The stream is never altered. Concatenating every `Bytes` and `Sequence`
///   `raw` slice, in order, gives back the input exactly.
/// * At most 4097 bytes are buffered (the 4096 limit plus one look-ahead byte),
///   however long a sequence runs.
/// * A sequence ends at ST, which is `ESC \` or BEL. CAN, SUB, or ESC followed
///   by anything other than `\` aborts it, as in the VT500 parser model. An
///   aborted sequence is passed through as plain bytes.
///
/// ```
/// use bellwether::{Event, Message, Scanner};
///
/// let mut scanner = Scanner::new();
/// let mut reports = Vec::new();
/// let mut passthrough = Vec::new();
/// for chunk in [&b"hi \x1b]75"[..], b"01;state=working\x1b", b"\\ there"] {
///     scanner.feed(chunk, |ev| match ev {
///         Event::Bytes(b) => passthrough.extend_from_slice(b),
///         Event::Sequence { message: Message::Report(r), .. } => reports.push(r),
///         _ => {}
///     });
/// }
/// assert_eq!(passthrough, b"hi  there");
/// assert_eq!(reports.len(), 1);
/// ```
#[derive(Debug, Clone)]
pub struct Scanner {
    state: St,
    buf: Vec<u8>,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    /// A scanner in the ground state.
    pub fn new() -> Self {
        Scanner {
            state: St::Ground,
            buf: Vec::new(),
        }
    }

    /// Bytes currently held back while a possible sequence is in progress.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Scans `input` and reports what it finds to `on`, in stream order.
    pub fn feed(&mut self, input: &[u8], mut on: impl FnMut(Event<'_>)) {
        // Start of the current pass-through run in `input`. Only meaningful in
        // `Ground` and `Overflow`.
        let mut run = 0;
        let mut i = 0;
        while i < input.len() {
            let b = input[i];
            match self.state {
                St::Ground => {
                    if b == ESC {
                        emit(&mut on, &input[run..i]);
                        self.start_escape();
                        self.state = St::Prefix(1);
                    }
                    i += 1;
                }
                St::Prefix(n) => {
                    if b == PREFIX[n] {
                        self.buf.push(b);
                        self.state = if n + 1 == PREFIX.len() {
                            St::Body
                        } else {
                            St::Prefix(n + 1)
                        };
                        i += 1;
                    } else {
                        // Not ours. Release the held bytes and look at `b` again in
                        // the ground state (it may be a new ESC).
                        emit(&mut on, &self.buf);
                        self.buf.clear();
                        self.state = St::Ground;
                        run = i;
                    }
                }
                St::Body => match b {
                    BEL => {
                        self.buf.push(b);
                        self.complete(&mut on);
                        i += 1;
                        run = i;
                    }
                    ESC => {
                        self.buf.push(b);
                        self.state = St::BodyEsc;
                        i += 1;
                    }
                    CAN | SUB => {
                        emit(&mut on, &self.buf);
                        self.buf.clear();
                        self.state = St::Ground;
                        run = i;
                    }
                    _ if self.buf.len() + 2 > MAX_SEQUENCE => {
                        // This byte plus the shortest ST (BEL) would exceed the limit.
                        emit(&mut on, &self.buf);
                        on(Event::Oversized);
                        self.buf.clear();
                        self.state = St::Overflow;
                        run = i;
                    }
                    _ => {
                        self.buf.push(b);
                        i += 1;
                    }
                },
                St::BodyEsc => {
                    if b == b'\\' {
                        self.buf.push(b);
                        self.complete(&mut on);
                        i += 1;
                        run = i;
                    } else {
                        // Aborted. The ESC may start a new sequence.
                        self.buf.pop();
                        emit(&mut on, &self.buf);
                        self.start_escape();
                        self.state = St::Prefix(1);
                    }
                }
                St::Overflow => {
                    match b {
                        BEL | CAN | SUB => self.state = St::Ground,
                        ESC => {
                            emit(&mut on, &input[run..i]);
                            self.start_escape();
                            self.state = St::OverflowEsc;
                        }
                        _ => {}
                    }
                    i += 1;
                }
                St::OverflowEsc => {
                    if b == b'\\' {
                        // End of the oversized sequence.
                        emit(&mut on, &self.buf);
                        self.buf.clear();
                        self.state = St::Ground;
                        run = i;
                        i += 1;
                    } else {
                        self.state = St::Prefix(1);
                    }
                }
            }
        }
        if matches!(self.state, St::Ground | St::Overflow) {
            emit(&mut on, &input[run..]);
        }
    }

    /// End of stream: releases any held bytes as [`Event::Bytes`] and resets to
    /// the ground state.
    pub fn flush(&mut self, mut on: impl FnMut(Event<'_>)) {
        emit(&mut on, &self.buf);
        self.buf.clear();
        self.state = St::Ground;
    }

    fn start_escape(&mut self) {
        self.buf.clear();
        self.buf.push(ESC);
    }

    fn complete(&mut self, on: &mut impl FnMut(Event<'_>)) {
        let raw = &self.buf[..];
        if raw.len() > MAX_SEQUENCE {
            on(Event::Bytes(raw));
            on(Event::Oversized);
        } else {
            let term = if raw.last() == Some(&BEL) { 1 } else { 2 };
            let body = &raw[PREFIX.len()..raw.len() - term];
            let message = classify(body);
            on(Event::Sequence { raw, body, message });
        }
        self.buf.clear();
        self.state = St::Ground;
    }
}

fn emit(on: &mut impl FnMut(Event<'_>), bytes: &[u8]) {
    if !bytes.is_empty() {
        on(Event::Bytes(bytes));
    }
}

fn classify(body: &[u8]) -> Message {
    let t = trim_ws(body);
    if t == b"?" {
        Message::Query
    } else if t.first() == Some(&b'?') {
        Message::Reply(parse_reply(body))
    } else {
        Message::Report(parse(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::State;

    #[derive(Debug, PartialEq)]
    enum Ev {
        B(Vec<u8>),
        S(Vec<u8>, Message),
        Over,
    }

    fn scan_chunks(chunks: &[&[u8]]) -> Vec<Ev> {
        let mut s = Scanner::new();
        let mut out: Vec<Ev> = Vec::new();
        let mut push = |e: Event<'_>| match e {
            Event::Bytes(b) => {
                if let Some(Ev::B(prev)) = out.last_mut() {
                    prev.extend_from_slice(b);
                } else {
                    out.push(Ev::B(b.to_vec()));
                }
            }
            Event::Sequence { raw, message, .. } => out.push(Ev::S(raw.to_vec(), message)),
            Event::Oversized => out.push(Ev::Over),
        };
        for c in chunks {
            s.feed(c, &mut push);
        }
        s.flush(&mut push);
        out
    }

    fn state_of(m: &Message) -> State {
        match m {
            Message::Report(Ok(r)) => r.state(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn passes_other_sequences_through() {
        let input = b"\x1b[31mred\x1b]0;title\x07\x1b]75;x\x07\x1b]7502;x\x07\x1b]7501x\x07";
        assert_eq!(scan_chunks(&[input]), vec![Ev::B(input.to_vec())]);
    }

    #[test]
    fn both_terminators_and_byte_by_byte() {
        let input = b"a\x1b]7501;state=idle\x07b\x1b]7501;state=done\x1b\\c";
        let chunks: Vec<&[u8]> = input.chunks(1).collect();
        let ev = scan_chunks(&chunks);
        assert_eq!(ev.len(), 5);
        assert_eq!(ev[0], Ev::B(b"a".to_vec()));
        match &ev[1] {
            Ev::S(raw, m) => {
                assert_eq!(raw, b"\x1b]7501;state=idle\x07");
                assert_eq!(state_of(m), State::Idle);
            }
            e => panic!("{e:?}"),
        }
        assert_eq!(ev[2], Ev::B(b"b".to_vec()));
        match &ev[3] {
            Ev::S(_, m) => assert_eq!(state_of(m), State::Done),
            e => panic!("{e:?}"),
        }
        assert_eq!(ev[4], Ev::B(b"c".to_vec()));
    }

    #[test]
    fn esc_abort_restarts() {
        // A 7501 sequence aborted by ESC that starts another 7501 sequence.
        let input = b"\x1b]7501;state=idle\x1b]7501;state=done\x07";
        let ev = scan_chunks(&[input]);
        assert_eq!(ev[0], Ev::B(b"\x1b]7501;state=idle".to_vec()));
        match &ev[1] {
            Ev::S(_, m) => assert_eq!(state_of(m), State::Done),
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn can_aborts() {
        let input = b"\x1b]7501;state=idle\x18rest";
        assert_eq!(scan_chunks(&[input]), vec![Ev::B(input.to_vec())]);
    }

    #[test]
    fn query_and_reply() {
        let ev = scan_chunks(&[b"\x1b]7501;?\x1b\\\x1b]7501;?:kinds=auth\x07"]);
        assert!(matches!(&ev[0], Ev::S(_, Message::Query)));
        match &ev[1] {
            Ev::S(_, Message::Reply(Ok(s))) => assert_eq!(s.kinds, ["auth"]),
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn partial_prefix_at_eof_is_flushed() {
        assert_eq!(
            scan_chunks(&[b"x\x1b]75"]),
            vec![Ev::B(b"x\x1b]75".to_vec())]
        );
    }

    #[test]
    fn oversized_passes_through_and_resyncs() {
        let mut input = b"\x1b]7501;state=idle:x=".to_vec();
        input.resize(6000, b'a');
        input.extend_from_slice(b"\x1b\\after\x1b]7501;state=done\x07");
        for chunk in [7, 4096, 10_000] {
            let chunks: Vec<&[u8]> = input.chunks(chunk).collect();
            let ev = scan_chunks(&chunks);
            let overs = ev.iter().filter(|e| matches!(e, Ev::Over)).count();
            assert_eq!(overs, 1);
            let mut rebuilt = Vec::new();
            for e in &ev {
                match e {
                    Ev::B(b) | Ev::S(b, _) => rebuilt.extend_from_slice(b),
                    Ev::Over => {}
                }
            }
            assert_eq!(rebuilt, input);
            match ev.last().unwrap() {
                Ev::S(_, m) => assert_eq!(state_of(m), State::Done),
                e => panic!("{e:?}"),
            }
        }
    }
}
