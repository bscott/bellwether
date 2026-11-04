use crate::types::State;

/// A typed field value ([`App`](crate::App), [`Id`](crate::Id)) breaks the
/// grammar or limits in spec sections *Keys* and *Records and ids*.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FieldError {
    /// The value is empty.
    #[error("value is empty")]
    Empty,
    /// The value contains a byte outside the allowed set.
    #[error("invalid character {0:?}")]
    InvalidChar(char),
    /// The value is longer than allowed.
    #[error("too long: {len} bytes, max {max}")]
    TooLong {
        /// Actual length in bytes.
        len: usize,
        /// Limit in bytes.
        max: usize,
    },
    /// An id contains an empty segment (`a//b`, a leading or trailing `/`).
    #[error("empty id segment")]
    EmptySegment,
    /// An id segment is longer than 32 bytes.
    #[error("id segment too long: {len} bytes, max {max}")]
    SegmentTooLong {
        /// Actual length in bytes.
        len: usize,
        /// Limit in bytes.
        max: usize,
    },
    /// An id has more than 8 segments.
    #[error("id too deep: {depth} levels, max {max}")]
    TooDeep {
        /// Actual number of segments.
        depth: usize,
        /// Limit.
        max: usize,
    },
}

/// A string is not a recognised `state` or `kind` value (spec: *Keys*).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown value {0:?}")]
pub struct UnknownValue(pub String);

/// [`ReportBuilder::try_build`](crate::ReportBuilder::try_build) rejected the
/// report (spec: *Syntax*, *Keys*, *Limits*).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildError {
    /// `kind` was set on a state other than `blocked`.
    #[error("kind applies only to the blocked state, not {0}")]
    KindNotAllowed(State),
    /// `progress` was set on a state other than `working` or `blocked`.
    #[error("progress applies only to working and blocked, not {0}")]
    ProgressNotAllowed(State),
    /// `msg` or `title` contains a control character.
    #[error("{key} contains a control character")]
    ControlChar {
        /// `"msg"` or `"title"`.
        key: &'static str,
    },
    /// `msg` or `title` is longer than its decoded limit.
    #[error("{key} too long: {len} bytes, max {max}")]
    TooLong {
        /// `"msg"` or `"title"`.
        key: &'static str,
        /// UTF-8 length in bytes.
        len: usize,
        /// Limit in bytes.
        max: usize,
    },
}

/// Why [`parse`](crate::parse) discarded a report. Every variant means the
/// whole report is dropped (spec: *Syntax*, *Records and ids*, *States*,
/// *Limits*).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The body is longer than the whole-sequence limit allows.
    #[error("sequence too long: body is {len} bytes")]
    TooLong {
        /// Body length in bytes.
        len: usize,
    },
    /// A key is longer than 16 bytes.
    #[error("key {key:?} longer than 16 bytes")]
    KeyTooLong {
        /// The offending key.
        key: String,
    },
    /// The report has no `state` key.
    #[error("missing state")]
    MissingState,
    /// The `state` value is not recognised.
    #[error("unknown state {0:?}")]
    UnknownState(String),
    /// A `msg` or `title` value is not valid base64, or doesn't decode to UTF-8.
    #[error("{key}: invalid base64 or UTF-8")]
    InvalidBase64 {
        /// `"msg"` or `"title"`.
        key: &'static str,
    },
    /// Decoded `msg` or `title` text contains a control character.
    #[error("{key}: decoded text contains a control character")]
    ControlChar {
        /// `"msg"` or `"title"`.
        key: &'static str,
    },
    /// A `msg`, `title` or `app` value breaks its length limit.
    #[error("{key} too long ({}): {len} bytes, max {max}", if *.encoded { "encoded" } else { "decoded" })]
    ValueTooLong {
        /// `"msg"`, `"title"` or `"app"`.
        key: &'static str,
        /// Whether the limit was on the encoded form (`app` is never encoded,
        /// and reports `true`).
        encoded: bool,
        /// Actual length in bytes.
        len: usize,
        /// Limit in bytes.
        max: usize,
    },
    /// The `id` does not match the grammar or breaks a limit.
    #[error("invalid id: {0}")]
    InvalidId(FieldError),
    /// The body starts with `?`: it is a feature detection query or reply, not
    /// a report.
    #[error("not a report (feature detection query or reply)")]
    NotAReport,
}

/// A feature detection reply could not be parsed (spec: *Feature detection*).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DetectError {
    /// The body doesn't start with `?`.
    #[error("not a feature detection reply")]
    NotAReply,
}
