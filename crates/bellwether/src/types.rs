use std::fmt;
use std::str::FromStr;

use crate::error::{FieldError, UnknownValue};
use crate::is_name_byte;
use crate::limits::{MAX_APP, MAX_ID, MAX_ID_DEPTH, MAX_ID_SEGMENT};

/// The `state` key (spec: *States*, *Keys*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum State {
    /// At rest, waiting for the user's next instruction.
    Idle,
    /// Running. May carry progress.
    Working,
    /// Finished a piece of work the user has not looked at yet.
    Done,
    /// Can't continue until the user acts. `kind` says what kind of action.
    Blocked,
    /// Failed and stopped.
    Error,
    /// Not a state: removes the addressed record and every record beneath it,
    /// or every record when there is no `id`.
    Clear,
}

impl State {
    /// The states a terminal supports when its detection reply has no
    /// `states` pair (spec: *Feature detection*). This list never changes.
    pub const DEFAULT_SUPPORTED: [State; 5] = [
        State::Idle,
        State::Working,
        State::Done,
        State::Blocked,
        State::Error,
    ];

    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Working => "working",
            State::Done => "done",
            State::Blocked => "blocked",
            State::Error => "error",
            State::Clear => "clear",
        }
    }

    /// Whether `kind` applies to this state. Only `blocked` (spec: *Keys*).
    pub fn allows_kind(self) -> bool {
        self == State::Blocked
    }

    /// Whether `progress` applies to this state: `working` and `blocked`
    /// (spec: *Keys*).
    pub fn allows_progress(self) -> bool {
        matches!(self, State::Working | State::Blocked)
    }

    /// Whether a record in this state survives process exit and a new shell
    /// prompt: `done` and `error` (spec: *States*, Lifetime).
    pub fn survives_exit(self) -> bool {
        matches!(self, State::Done | State::Error)
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for State {
    type Err = UnknownValue;
    /// Exact, case-sensitive match on the wire spelling.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "idle" => State::Idle,
            "working" => State::Working,
            "done" => State::Done,
            "blocked" => State::Blocked,
            "error" => State::Error,
            "clear" => State::Clear,
            _ => return Err(UnknownValue(s.to_owned())),
        })
    }
}

/// The `kind` key: what a `blocked` program is waiting for (spec: *Keys*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Kind {
    /// Approval to do something.
    Permission,
    /// The user must type an answer.
    Question,
    /// A login, token, or credential.
    Auth,
}

impl Kind {
    /// The kinds a terminal supports when its detection reply has no `kinds`
    /// pair (spec: *Feature detection*). This list never changes.
    pub const DEFAULT_SUPPORTED: [Kind; 3] = [Kind::Permission, Kind::Question, Kind::Auth];

    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Permission => "permission",
            Kind::Question => "question",
            Kind::Auth => "auth",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Kind {
    type Err = UnknownValue;
    /// Exact, case-sensitive match on the wire spelling.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "permission" => Kind::Permission,
            "question" => Kind::Question,
            "auth" => Kind::Auth,
            _ => return Err(UnknownValue(s.to_owned())),
        })
    }
}

/// The `progress` key: an integer percentage, 0 to 100 (spec: *Keys*).
/// When a report has no progress, the program is busy but has no percentage
/// to report (indeterminate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "u8", into = "u8"))]
pub struct Progress(u8);

impl Progress {
    /// Returns `None` when `n > 100`.
    pub fn new(n: u8) -> Option<Self> {
        (n <= 100).then_some(Progress(n))
    }

    /// The percentage.
    pub fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Progress {
    type Error = FieldError;
    fn try_from(n: u8) -> Result<Self, Self::Error> {
        Progress::new(n).ok_or(FieldError::TooLong {
            len: n as usize,
            max: 100,
        })
    }
}

impl From<Progress> for u8 {
    fn from(p: Progress) -> u8 {
        p.0
    }
}

impl fmt::Display for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The `app` key: a stable program identifier matching
/// `[A-Za-z0-9_.+-]{1,32}` (spec: *Keys*). Used for grouping, filtering and
/// icons. It is not a display label; `title` is.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "String", into = "String"))]
pub struct App(String);

impl App {
    /// Validates `s` against `[A-Za-z0-9_.+-]{1,32}`.
    pub fn new(s: &str) -> Result<Self, FieldError> {
        if s.is_empty() {
            return Err(FieldError::Empty);
        }
        if s.len() > MAX_APP {
            return Err(FieldError::TooLong {
                len: s.len(),
                max: MAX_APP,
            });
        }
        check_name_chars(s)?;
        Ok(App(s.to_owned()))
    }

    /// The identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for App {
    type Error = FieldError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        App::new(&s)
    }
}

impl From<App> for String {
    fn from(a: App) -> String {
        a.0
    }
}

impl fmt::Display for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A record id: `segment ("/" segment)*` with `segment = [A-Za-z0-9_.+-]{1,32}`,
/// at most 128 bytes and 8 levels deep (spec: *Records and ids*, *Limits*).
///
/// The root record has no id, so it is represented as `Option<Id>::None`
/// throughout the crate. `/` makes `build/test` a child of `build`, and
/// `build` a child of the root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "String", into = "String"))]
pub struct Id(String);

impl Id {
    /// Validates `s` against the id grammar and limits.
    pub fn new(s: &str) -> Result<Self, FieldError> {
        if s.len() > MAX_ID {
            return Err(FieldError::TooLong {
                len: s.len(),
                max: MAX_ID,
            });
        }
        let mut depth = 0;
        for seg in s.split('/') {
            depth += 1;
            if depth > MAX_ID_DEPTH {
                return Err(FieldError::TooDeep {
                    depth: s.split('/').count(),
                    max: MAX_ID_DEPTH,
                });
            }
            if seg.is_empty() {
                return Err(FieldError::EmptySegment);
            }
            if seg.len() > MAX_ID_SEGMENT {
                return Err(FieldError::SegmentTooLong {
                    len: seg.len(),
                    max: MAX_ID_SEGMENT,
                });
            }
            check_name_chars(seg)?;
        }
        Ok(Id(s.to_owned()))
    }

    /// The id as written on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The `/`-separated segments.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// The parent id, or `None` when the parent is the root record.
    pub fn parent(&self) -> Option<Id> {
        self.0.rsplit_once('/').map(|(p, _)| Id(p.to_owned()))
    }

    /// Appends one segment, as nested `bellwether run` does with
    /// `BELLWETHER_PARENT_ID`. Fails if the result breaks the grammar or limits.
    pub fn child(&self, segment: &str) -> Result<Id, FieldError> {
        Id::new(&format!("{}/{}", self.0, segment))
    }

    /// Whether `self` lies strictly beneath `other` (`a/b` is a descendant of
    /// `a`, but `ab` is not).
    pub fn is_descendant_of(&self, other: &Id) -> bool {
        self.0.len() > other.0.len()
            && self.0.starts_with(&other.0)
            && self.0.as_bytes()[other.0.len()] == b'/'
    }
}

impl TryFrom<String> for Id {
    type Error = FieldError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Id::new(&s)
    }
}

impl From<Id> for String {
    fn from(i: Id) -> String {
        i.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Id {
    type Err = FieldError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Id::new(s)
    }
}

impl FromStr for App {
    type Err = FieldError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        App::new(s)
    }
}

fn check_name_chars(s: &str) -> Result<(), FieldError> {
    match s.chars().find(|&c| !c.is_ascii() || !is_name_byte(c as u8)) {
        Some(c) => Err(FieldError::InvalidChar(c)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_relationships() {
        let a = Id::new("build").unwrap();
        let ab = Id::new("build/test").unwrap();
        let abc = Id::new("build/test/unit").unwrap();
        let other = Id::new("buildx/test").unwrap();
        assert!(ab.is_descendant_of(&a));
        assert!(abc.is_descendant_of(&a));
        assert!(!a.is_descendant_of(&a));
        assert!(!other.is_descendant_of(&a));
        assert_eq!(abc.parent(), Some(ab.clone()));
        assert_eq!(a.parent(), None);
        assert_eq!(a.child("test").unwrap(), ab);
        assert_eq!(
            abc.segments().collect::<Vec<_>>(),
            ["build", "test", "unit"]
        );
    }

    #[test]
    fn id_grammar() {
        assert_eq!(Id::new(""), Err(FieldError::EmptySegment));
        assert_eq!(Id::new("a//b"), Err(FieldError::EmptySegment));
        assert_eq!(Id::new("/a"), Err(FieldError::EmptySegment));
        assert_eq!(Id::new("a/"), Err(FieldError::EmptySegment));
        assert_eq!(Id::new("a b"), Err(FieldError::InvalidChar(' ')));
        assert_eq!(Id::new("a,b"), Err(FieldError::InvalidChar(',')));
        assert!(Id::new("A-z_0.9+").is_ok());
    }

    #[test]
    fn app_grammar() {
        assert_eq!(App::new(""), Err(FieldError::Empty));
        assert_eq!(App::new("a/b"), Err(FieldError::InvalidChar('/')));
        assert_eq!(App::new("é"), Err(FieldError::InvalidChar('é')));
        assert!(App::new("claude-code").is_ok());
    }

    #[test]
    fn state_kind_round_trip() {
        for s in State::DEFAULT_SUPPORTED.iter().chain([&State::Clear]) {
            assert_eq!(s.as_str().parse::<State>().unwrap(), *s);
        }
        for k in Kind::DEFAULT_SUPPORTED {
            assert_eq!(k.as_str().parse::<Kind>().unwrap(), k);
        }
        assert!("Working".parse::<State>().is_err());
    }

    #[test]
    fn progress_range() {
        assert_eq!(Progress::new(100).map(Progress::get), Some(100));
        assert_eq!(Progress::new(101), None);
    }
}
