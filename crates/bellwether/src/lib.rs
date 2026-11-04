//! OSC 7501, the **Program Status Protocol** (rev 0.4).
//!
//! A program uses this terminal escape sequence to report what it is doing:
//! idle, working, blocked on the user, done, or failed, and why.
//! Spec: <https://www.superlogical.com/rex/docs/build/program-status>
//!
//! This crate covers both sides of the protocol:
//!
//! * **Emitters** build a [`Report`] and [`Report::encode`] it.
//! * **Receivers** (terminals, multiplexers, wrappers) find sequences in a byte
//!   stream with [`Scanner`], [`parse`] bodies, and keep state in a
//!   [`RecordStore`].
//! * **Feature detection** is handled by [`detect`].
//!
//! The core does no I/O and has no platform code. The `tty` feature adds
//! [`tty`], which writes to and probes `/dev/tty`. The `serde` feature derives
//! `Serialize`/`Deserialize` for the value types.
//!
//! ```
//! use bellwether::{parse, App, Report, State, Terminator};
//!
//! let report = Report::builder(State::Working)
//!     .app(App::new("brew").unwrap())
//!     .msg("Installing updates")
//!     .try_build()
//!     .unwrap();
//! let bytes = report.encode(Terminator::St);
//! assert_eq!(bytes, b"\x1b]7501;state=working:app=brew:msg=SW5zdGFsbGluZyB1cGRhdGVz\x1b\\");
//!
//! // Receivers parse the body between `ESC ] 7501 ;` and ST.
//! let body = &bytes[7..bytes.len() - 2];
//! assert_eq!(parse(body).unwrap(), report);
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod detect;
mod error;
pub mod limits;
mod parse;
mod report;
mod scan;
mod store;
#[cfg(feature = "tty")]
pub mod tty;
mod types;

pub use detect::{Detection, Detector, Support};
pub use error::{BuildError, DetectError, FieldError, ParseError, UnknownValue};
pub use parse::{MAX_BODY, parse};
pub use report::{Report, ReportBuilder, Terminator};
pub use scan::{Event, Message, Scanner};
pub use store::{Change, Osc94, Record, RecordStore, RemoveReason, StoreConfig};
pub use types::{App, Id, Kind, Progress, State};

/// The bytes that start every OSC 7501 sequence: `ESC ] 7501 ;`
/// (spec: *Syntax*).
pub const PREFIX: &[u8] = b"\x1b]7501;";

/// Value bytes allowed by the grammar: `[A-Za-z0-9_.,+/=-]` (spec: *Syntax*).
pub(crate) fn is_value_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-')
}

/// Bytes allowed in `app` and `id` segments: `[A-Za-z0-9_.+-]` (spec: *Keys*,
/// *Records and ids*).
pub(crate) fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-')
}

/// Strips the whitespace the spec says to remove around keys and values
/// (ASCII space and HT; see NOTES.md).
pub(crate) fn trim_ws(mut s: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = s {
        s = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = s {
        s = rest;
    }
    s
}

/// Splits a body into `(key, value)` pairs and applies the spec's rules for
/// malformed pairs: a pair with no `=`, an empty key, a key outside `[a-z]+`,
/// or a value byte outside the value set is skipped. Keys and values are
/// trimmed. Key length is *not* checked here.
pub(crate) fn pairs(body: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    body.split(|&b| b == b':').filter_map(|pair| {
        let eq = pair.iter().position(|&b| b == b'=')?;
        let key = trim_ws(&pair[..eq]);
        let value = trim_ws(&pair[eq + 1..]);
        let key_ok = !key.is_empty() && key.iter().all(u8::is_ascii_lowercase);
        let value_ok = value.iter().copied().all(is_value_byte);
        (key_ok && value_ok).then_some((key, value))
    })
}
