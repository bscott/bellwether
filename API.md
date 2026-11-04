# bellwether library: public API (Phase 1)

Approved 2026-11-03. Small additions made during implementation:
`ReportBuilder::maybe_id`, `Terminator::as_bytes`, `Scanner::buffered`,
`Detector::{da1_seen, result}`, `RecordStore::is_empty`, `Tty::from_fd`,
`detect::parse_reply` re-exported at `bellwether::detect`, `MAX_BODY`, and the
`limits` constants module. Rustdoc (`cargo doc --open`) is the source of truth.

Crate `bellwether`. Default features: none. Optional features: `serde`
(Serialize/Deserialize on the value types) and `tty` (`bellwether::tty`, the only
module that does I/O or has `unsafe`).

```rust
// ── types.rs ─────────────────────────────── spec: Keys, Records and ids
pub enum State { Idle, Working, Done, Blocked, Error, Clear }
impl State {
    pub const DEFAULT_SUPPORTED: [State; 5];        // reply without `states`
    pub fn as_str(self) -> &'static str;
    pub fn allows_kind(self) -> bool;               // Blocked
    pub fn allows_progress(self) -> bool;           // Working | Blocked
    pub fn survives_exit(self) -> bool;             // Done | Error
}
impl FromStr for State { type Err = UnknownValue; }

pub enum Kind { Permission, Question, Auth }        // + as_str, FromStr, DEFAULT_SUPPORTED

pub struct Progress(u8);                            // 0..=100
impl Progress { pub fn new(n: u8) -> Option<Self>; pub fn get(self) -> u8; }

pub struct App(/* String */);                       // [A-Za-z0-9_.+-]{1,32}
impl App { pub fn new(s: &str) -> Result<Self, FieldError>; pub fn as_str(&self) -> &str; }

pub struct Id(/* String */);                        // segment("/"segment)*, ≤128 B, ≤32 B/seg, ≤8 deep
impl Id {
    pub fn new(s: &str) -> Result<Self, FieldError>;
    pub fn as_str(&self) -> &str;
    pub fn segments(&self) -> impl Iterator<Item = &str>;
    pub fn parent(&self) -> Option<Id>;             // None = root
    pub fn child(&self, segment: &str) -> Result<Id, FieldError>;  // used for BELLWETHER_PARENT_ID nesting
    pub fn is_descendant_of(&self, other: &Id) -> bool;
}

// ── report.rs ──────────────────────────────── spec: Syntax, Keys, Limits
pub struct Report { /* private; always wire-valid and normalised */ }
impl Report {
    pub fn builder(state: State) -> ReportBuilder;
    pub fn clear(id: Option<Id>) -> Report;
    // getters
    pub fn state(&self) -> State;
    pub fn kind(&self) -> Option<Kind>;
    pub fn progress(&self) -> Option<Progress>;     // None = indeterminate
    pub fn app(&self) -> Option<&App>;
    pub fn id(&self) -> Option<&Id>;                // None = root record
    pub fn title(&self) -> Option<&str>;
    pub fn msg(&self) -> Option<&str>;
    // wire
    pub fn encode(&self, t: Terminator) -> Vec<u8>; // one complete sequence
    pub fn encode_into(&self, out: &mut Vec<u8>, t: Terminator);
}
pub enum Terminator { St /* ESC \ (default) */, Bel }

pub struct ReportBuilder { .. }
impl ReportBuilder {
    pub fn kind(self, k: Kind) -> Self;
    pub fn progress(self, p: Progress) -> Self;
    pub fn app(self, a: App) -> Self;
    pub fn id(self, id: Id) -> Self;
    pub fn title(self, s: impl Into<String>) -> Self;
    pub fn msg(self, s: impl Into<String>) -> Self;
    /// Strict ("try_new"): Err on control chars, msg >2048 B / title >192 B,
    /// kind without Blocked, progress without Working|Blocked.
    pub fn try_build(self) -> Result<Report, BuildError>;
    /// Lossy: strips control chars, truncates msg/title on char boundaries,
    /// drops inapplicable kind/progress. Never fails.
    pub fn build_lossy(self) -> Report;
}
```

`App` and `Id` are typed in the builder on purpose. A lossy id would
quietly retarget a report, and the spec forbids falling back to root for a bad id.

```rust
// ── parse.rs ──────────────────────────────────── spec: Syntax, Limits
/// One body: the bytes after `ESC ] 7501 ;` and before ST.
pub fn parse(body: &[u8]) -> Result<Report, ParseError>;

pub enum ParseError {           // thiserror; every variant = "report discarded"
    TooLong { len: usize },
    KeyTooLong { key: String },
    MissingState,
    UnknownState(String),
    InvalidBase64 { key: &'static str },   // includes invalid UTF-8
    ControlChar { key: &'static str },
    ValueTooLong { key: &'static str, encoded: bool, len: usize, max: usize },  // msg/title/app
    InvalidId(FieldError),
    NotAReport,                             // body starts with `?` (query/reply)
}

// ── scan.rs ───────────────────────────────────── spec: Syntax, Limits
/// Incremental, allocation-bounded (≤4096 B buffered). It never alters
/// the stream: anything that isn't a complete, in-limit OSC 7501 sequence
/// comes out as Bytes, so a pass-through wrapper can forward everything
/// and only look at the Sequence events.
pub struct Scanner { .. }
impl Scanner {
    pub fn new() -> Self;
    pub fn feed(&mut self, input: &[u8], on: impl FnMut(Event<'_>));
    pub fn flush(&mut self, on: impl FnMut(Event<'_>));   // EOF: release a buffered partial prefix
}
pub enum Event<'a> {
    Bytes(&'a [u8]),                        // pass through untouched
    Sequence { raw: &'a [u8], body: &'a [u8], message: Message },
    Oversized,                              // >4096 B; its bytes still come out as Bytes
}
pub enum Message {
    Report(Result<Report, ParseError>),
    Query,                                  // `?`
    Reply(Result<Support, DetectError>),    // `? : states=…`
}

// ── store.rs ───────────── spec: Records and ids, States (Lifetime), Limits, Relationship to other sequences
pub struct RecordStore { .. }
pub struct StoreConfig { pub max_records: usize /* 256 */, pub drop_idle_on_exit: bool /* false */ }
pub struct Record { /* report minus id */ }   // state(), kind(), progress(), app(), title(), msg(), updated(): u64
pub enum Change { Set(Option<Id>), Removed { id: Option<Id>, reason: RemoveReason } }
pub enum RemoveReason { Cleared, Evicted, ProcessExit, Prompt, Reset }
pub enum Osc94 { Remove, Set(Progress), Error, Indeterminate, Paused(Progress) }

impl RecordStore {
    pub fn new() -> Self;
    pub fn with_config(c: StoreConfig) -> Self;
    pub fn apply(&mut self, r: Report) -> Vec<Change>;   // replace, subtree clear, LRU evict
    pub fn on_process_exit(&mut self) -> Vec<Change>;
    pub fn on_prompt(&mut self) -> Vec<Change>;          // OSC 133;A
    pub fn on_ris(&mut self) -> Vec<Change>;             // also re-enables OSC 9;4 mapping
    pub fn apply_osc_9_4(&mut self, p: Osc94) -> Vec<Change>;  // no-op after first 7501 report
    pub fn osc_9_4_mapping_active(&self) -> bool;
    pub fn get(&self, id: Option<&Id>) -> Option<&Record>;
    pub fn effective_app(&self, id: Option<&Id>) -> Option<&App>;  // nearest-ancestor inheritance
    pub fn iter(&self) -> impl Iterator<Item = (Option<&Id>, &Record)>;  // root first, then id order
    pub fn len(&self) -> usize;
}

// ── detect.rs ─────────────────────────────── spec: Feature detection
pub const QUERY: &[u8];                       // ESC ] 7501 ; ? ESC \
pub const DA1: &[u8];                         // ESC [ c
pub fn query_with_da1() -> Vec<u8>;           // QUERY ++ DA1, one write
pub struct Support { pub states: Vec<String>, pub kinds: Vec<String> }  // defaults filled in when absent
impl Support { pub fn supports(&self, s: State) -> bool; pub fn supports_kind(&self, k: Kind) -> bool; }
pub fn parse_reply(body: &[u8]) -> Result<Support, DetectError>;

/// Pure state machine for the DA1 race: feed it whatever the tty returns.
/// Other input (keystrokes, unrelated replies) is ignored.
pub struct Detector { .. }
pub enum Detection { Pending, Supported(Support), Unsupported }
impl Detector { pub fn new() -> Self; pub fn feed(&mut self, input: &[u8]) -> Detection; }

// ── tty.rs (feature = "tty") ─ the only module with unsafe ──────────
pub struct Tty { .. }                         // opens /dev/tty
impl Tty {
    pub fn open() -> io::Result<Option<Tty>>; // None = no controlling tty
    pub fn write_sequence(&self, bytes: &[u8]) -> io::Result<()>;  // one write(2)
    pub fn probe(&self, timeout: Duration) -> io::Result<Detection>; // raw mode + guaranteed termios restore
}
```

Encode key order is state, kind, app, id, title, progress, msg. That is the order
every spec example uses, so the golden tests can compare encoded bytes against
the spec text exactly. Base64 output is padded, as in the spec examples.
