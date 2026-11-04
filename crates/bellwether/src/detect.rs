//! Feature detection (spec: *Feature detection*).
//!
//! The program sends [`QUERY`] followed immediately by [`DA1`]
//! ([`query_with_da1`] produces both in one buffer). A supporting terminal
//! answers the query before DA1. Every terminal answers DA1. So if the DA1
//! reply arrives first, the protocol is unsupported. [`Detector`] runs that
//! race over whatever bytes come back from the tty, with no I/O of its own.

use crate::error::DetectError;
use crate::scan::{Event, Message, Scanner};
use crate::types::{Kind, State};
use crate::{pairs, trim_ws};

/// `ESC ] 7501 ; ? ESC \`: the feature detection query.
pub const QUERY: &[u8] = b"\x1b]7501;?\x1b\\";

/// `CSI c`: primary device attributes (DA1). Every terminal answers it.
pub const DA1: &[u8] = b"\x1b[c";

/// [`QUERY`] followed by [`DA1`], ready to send in one `write()`.
pub fn query_with_da1() -> Vec<u8> {
    [QUERY, DA1].concat()
}

/// The states and kinds a terminal says it supports.
///
/// When the reply has no `states` pair, the five original states are filled
/// in. When it has no `kinds` pair, the three original kinds are filled in.
/// Values this crate doesn't know are kept as strings, so callers can check
/// for states from later spec revisions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Support {
    /// Supported `state` values, not counting `clear`.
    pub states: Vec<String>,
    /// Supported `kind` values.
    pub kinds: Vec<String>,
}

impl Default for Support {
    /// The meaning of a bare `?` reply: the five states and three kinds.
    fn default() -> Self {
        Support {
            states: State::DEFAULT_SUPPORTED
                .iter()
                .map(|s| s.as_str().to_owned())
                .collect(),
            kinds: Kind::DEFAULT_SUPPORTED
                .iter()
                .map(|k| k.as_str().to_owned())
                .collect(),
        }
    }
}

impl Support {
    /// Whether `state` is supported. `clear` is always supported.
    pub fn supports(&self, state: State) -> bool {
        state == State::Clear || self.states.iter().any(|s| s == state.as_str())
    }

    /// Whether `kind` is supported.
    pub fn supports_kind(&self, kind: Kind) -> bool {
        self.kinds.iter().any(|k| k == kind.as_str())
    }
}

/// Parses a reply body such as `? : states=idle,working : kinds=auth`.
///
/// Malformed pairs and pairs this crate doesn't understand are ignored. A
/// missing `states` or `kinds` gets the defaults.
pub fn parse_reply(body: &[u8]) -> Result<Support, DetectError> {
    let body = trim_ws(body);
    let rest = body.strip_prefix(b"?").ok_or(DetectError::NotAReply)?;
    let mut support = Support::default();
    for (key, value) in pairs(rest) {
        let list = || -> Vec<String> {
            value
                .split(|&b| b == b',')
                .map(trim_ws)
                .filter(|v| !v.is_empty())
                .map(|v| String::from_utf8_lossy(v).into_owned())
                .collect()
        };
        match key {
            b"states" => support.states = list(),
            b"kinds" => support.kinds = list(),
            _ => {}
        }
    }
    Ok(support)
}

/// Result of the detection race.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// Neither reply has arrived yet.
    Pending,
    /// The terminal answered the query before DA1.
    Supported(Support),
    /// The DA1 reply came first, or (as decided by the caller) nothing came in
    /// time.
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Csi {
    Idle,
    Esc,
    Bracket,
    Params,
}

/// A pure state machine for the DA1 race. Feed it the bytes read back from
/// the terminal, in any chunking. Unrelated input, such as keystrokes or
/// other replies, is ignored.
///
/// ```
/// use bellwether::detect::{Detection, Detector};
///
/// let mut d = Detector::new();
/// assert_eq!(d.feed(b"\x1b]7501;?:kinds=auth"), Detection::Pending);
/// assert!(matches!(d.feed(b"\x1b\\\x1b[?62;22c"), Detection::Supported(_)));
/// assert!(d.da1_seen());
///
/// let mut d = Detector::new();
/// assert_eq!(d.feed(b"\x1b[?1;2c"), Detection::Unsupported);
/// ```
#[derive(Debug, Clone)]
pub struct Detector {
    scanner: Scanner,
    csi: Csi,
    result: Detection,
    da1_seen: bool,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    /// A detector that hasn't seen any input.
    pub fn new() -> Self {
        Detector {
            scanner: Scanner::new(),
            csi: Csi::Idle,
            result: Detection::Pending,
            da1_seen: false,
        }
    }

    /// Whether the DA1 reply has arrived. Once it has, nothing more is coming,
    /// so a caller can stop reading without leaving reply bytes in the tty's
    /// input.
    pub fn da1_seen(&self) -> bool {
        self.da1_seen
    }

    /// The decision so far.
    pub fn result(&self) -> &Detection {
        &self.result
    }

    /// Consumes terminal input and returns the decision so far. Once the
    /// result leaves `Pending`, it doesn't change.
    pub fn feed(&mut self, input: &[u8]) -> Detection {
        let Detector {
            scanner,
            csi,
            result,
            da1_seen,
        } = self;
        scanner.feed(input, |ev| match ev {
            Event::Sequence { message, .. } => {
                let support = match message {
                    Message::Query => Some(Support::default()),
                    Message::Reply(Ok(s)) => Some(s),
                    _ => None,
                };
                if let (Some(s), Detection::Pending) = (support, &result) {
                    *result = Detection::Supported(s);
                }
            }
            Event::Bytes(bytes) => {
                for &b in bytes {
                    if step_da1(csi, b) {
                        *da1_seen = true;
                        if *result == Detection::Pending {
                            *result = Detection::Unsupported;
                        }
                    }
                }
            }
            Event::Oversized => {}
        });
        self.result.clone()
    }
}

/// Recognises a DA1 reply, `ESC [ ? Ps ; … c`. Returns true on the final `c`.
fn step_da1(csi: &mut Csi, b: u8) -> bool {
    let (next, hit) = match (*csi, b) {
        (_, 0x1b) => (Csi::Esc, false),
        (Csi::Esc, b'[') => (Csi::Bracket, false),
        (Csi::Bracket, b'?') => (Csi::Params, false),
        (Csi::Params, b'0'..=b'9' | b';') => (Csi::Params, false),
        (Csi::Params, b'c') => (Csi::Idle, true),
        _ => (Csi::Idle, false),
    };
    *csi = next;
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_defaults() {
        assert_eq!(parse_reply(b"?").unwrap(), Support::default());
        let s =
            parse_reply(b"? : states=idle,working,done,blocked,error,paused : kinds=auth").unwrap();
        assert!(s.states.contains(&"paused".to_owned()));
        assert!(s.supports(State::Blocked));
        assert!(s.supports(State::Clear));
        assert!(s.supports_kind(Kind::Auth));
        assert!(!s.supports_kind(Kind::Question));
        // Only kinds given, so states are the defaults.
        assert_eq!(s.states.len(), 6);
        let s = parse_reply(b"?:kinds=auth:future=x:Bad=1").unwrap();
        assert_eq!(s.states.len(), 5);
        assert_eq!(parse_reply(b"state=idle"), Err(DetectError::NotAReply));
    }

    #[test]
    fn race_supported_split_across_chunks() {
        let input = b"k\x1b]7501;?:states=idle,working\x1b\\x\x1b[?65;1;9c";
        for n in 1..input.len() {
            let mut d = Detector::new();
            for c in input.chunks(n) {
                d.feed(c);
            }
            match d.result() {
                Detection::Supported(s) => assert_eq!(s.states, ["idle", "working"]),
                r => panic!("{n}: {r:?}"),
            }
            assert!(d.da1_seen());
        }
    }

    #[test]
    fn race_unsupported_and_sticky() {
        let mut d = Detector::new();
        d.feed(b"\x1b[?6");
        assert_eq!(d.feed(b"2c"), Detection::Unsupported);
        assert_eq!(d.feed(b"\x1b]7501;?\x07"), Detection::Unsupported);
    }

    #[test]
    fn non_da1_csi_ignored() {
        let mut d = Detector::new();
        assert_eq!(d.feed(b"\x1b[1;5c\x1b[?1u\x1b[A"), Detection::Pending);
        assert!(!d.da1_seen());
    }
}
