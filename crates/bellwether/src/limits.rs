//! Hard limits from spec section *Limits*.
//!
//! A report that breaks any of these is discarded whole.

/// Whole sequence, from OSC through ST.
pub const MAX_SEQUENCE: usize = 4096;
/// Key length in bytes.
pub const MAX_KEY: usize = 16;
/// `msg`: base64-encoded length. Checked before decoding.
pub const MAX_MSG_ENCODED: usize = 2732;
/// `msg`: decoded UTF-8 length in bytes.
pub const MAX_MSG_DECODED: usize = 2048;
/// `title`: base64-encoded length. Checked before decoding.
pub const MAX_TITLE_ENCODED: usize = 256;
/// `title`: decoded UTF-8 length in bytes.
pub const MAX_TITLE_DECODED: usize = 192;
/// `app` length in bytes.
pub const MAX_APP: usize = 32;
/// `id`: total length in bytes.
pub const MAX_ID: usize = 128;
/// `id`: length of one segment in bytes.
pub const MAX_ID_SEGMENT: usize = 32;
/// `id`: number of segments (depth).
pub const MAX_ID_DEPTH: usize = 8;
/// Records per terminal. When a new record would exceed this, the
/// least recently updated record is evicted.
pub const MAX_RECORDS: usize = 256;
/// The fewest records a terminal MUST support.
pub const MIN_RECORDS: usize = 64;
