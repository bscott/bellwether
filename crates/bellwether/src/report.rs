use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::PREFIX;
use crate::error::BuildError;
use crate::limits::{MAX_MSG_DECODED, MAX_TITLE_DECODED};
use crate::types::{App, Id, Kind, Progress, State};

/// The sequence terminator, ST (spec: *Conventions*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Terminator {
    /// `ESC \` (0x1B 0x5C), the standard string terminator.
    #[default]
    St,
    /// BEL (0x07). Shorter and widely accepted.
    Bel,
}

impl Terminator {
    /// The terminator bytes.
    pub fn as_bytes(self) -> &'static [u8] {
        match self {
            Terminator::St => b"\x1b\\",
            Terminator::Bel => b"\x07",
        }
    }
}

/// One OSC 7501 report (spec: *Syntax*, *Keys*).
///
/// A `Report` is always valid on the wire and normalised:
/// * every field is within its limit;
/// * `msg` and `title` contain no control characters;
/// * `kind` is present only with [`State::Blocked`];
/// * `progress` is present only with [`State::Working`] or [`State::Blocked`].
///
/// So `parse(body_of(r.encode(..))) == Ok(r)` for every report.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "ReportFields"))]
pub struct Report {
    pub(crate) state: State,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub(crate) kind: Option<Kind>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub(crate) progress: Option<Progress>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub(crate) app: Option<App>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub(crate) id: Option<Id>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub(crate) title: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub(crate) msg: Option<String>,
}

impl Report {
    /// Starts a report in `state`.
    pub fn builder(state: State) -> ReportBuilder {
        ReportBuilder {
            state,
            kind: None,
            progress: None,
            app: None,
            id: None,
            title: None,
            msg: None,
        }
    }

    /// A `clear` report. It removes the record `id` and everything beneath
    /// it, or every record when `id` is `None` (spec: *States*).
    pub fn clear(id: Option<Id>) -> Report {
        Report {
            id,
            ..Report::builder(State::Clear).build_lossy()
        }
    }

    /// The `state` key.
    pub fn state(&self) -> State {
        self.state
    }
    /// The `kind` key. Only ever present with [`State::Blocked`].
    pub fn kind(&self) -> Option<Kind> {
        self.kind
    }
    /// The `progress` key. `None` means indeterminate.
    pub fn progress(&self) -> Option<Progress> {
        self.progress
    }
    /// The `app` key.
    pub fn app(&self) -> Option<&App> {
        self.app.as_ref()
    }
    /// The `id` key. `None` addresses the root record.
    pub fn id(&self) -> Option<&Id> {
        self.id.as_ref()
    }
    /// The decoded `title`.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
    /// The decoded `msg`.
    pub fn msg(&self) -> Option<&str> {
        self.msg.as_deref()
    }

    /// Encodes one complete sequence, `ESC ] 7501 ; pairs ST`.
    ///
    /// Keys come out in this order: state, kind, app, id, title, progress, msg.
    /// That is the order used by every example in the spec. Base64 is
    /// standard and padded.
    pub fn encode(&self, terminator: Terminator) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        self.encode_into(&mut out, terminator);
        out
    }

    /// Like [`encode`](Self::encode), but appends to `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>, terminator: Terminator) {
        out.extend_from_slice(PREFIX);
        out.extend_from_slice(b"state=");
        out.extend_from_slice(self.state.as_str().as_bytes());
        if let Some(k) = self.kind {
            out.extend_from_slice(b":kind=");
            out.extend_from_slice(k.as_str().as_bytes());
        }
        if let Some(a) = &self.app {
            out.extend_from_slice(b":app=");
            out.extend_from_slice(a.as_str().as_bytes());
        }
        if let Some(i) = &self.id {
            out.extend_from_slice(b":id=");
            out.extend_from_slice(i.as_str().as_bytes());
        }
        if let Some(t) = &self.title {
            out.extend_from_slice(b":title=");
            push_base64(out, t);
        }
        if let Some(p) = self.progress {
            out.extend_from_slice(b":progress=");
            out.extend_from_slice(p.get().to_string().as_bytes());
        }
        if let Some(m) = &self.msg {
            out.extend_from_slice(b":msg=");
            push_base64(out, m);
        }
        out.extend_from_slice(terminator.as_bytes());
    }
}

fn push_base64(out: &mut Vec<u8>, text: &str) {
    let start = out.len();
    out.resize(
        start + base64::encoded_len(text.len(), true).expect("bounded length"),
        0,
    );
    let n = STANDARD
        .encode_slice(text, &mut out[start..])
        .expect("buffer sized by encoded_len");
    out.truncate(start + n);
}

/// Builds a [`Report`]. `app`, `id`, `kind` and `progress` take typed values
/// that are valid by construction. `msg` and `title` take free text, which is
/// checked by [`try_build`](Self::try_build) or cleaned up by
/// [`build_lossy`](Self::build_lossy).
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct ReportBuilder {
    state: State,
    kind: Option<Kind>,
    progress: Option<Progress>,
    app: Option<App>,
    id: Option<Id>,
    title: Option<String>,
    msg: Option<String>,
}

impl ReportBuilder {
    /// Sets `kind`. Valid only with `blocked`.
    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = Some(kind);
        self
    }
    /// Sets `progress`. Valid only with `working` or `blocked`.
    pub fn progress(mut self, progress: Progress) -> Self {
        self.progress = Some(progress);
        self
    }
    /// Sets `app`.
    pub fn app(mut self, app: App) -> Self {
        self.app = Some(app);
        self
    }
    /// Sets `id`. Without one, the report addresses the root record.
    pub fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }
    /// Sets `id` from an option, for code that may or may not have one.
    pub fn maybe_id(mut self, id: Option<Id>) -> Self {
        self.id = id;
        self
    }
    /// Sets `title`, a short label (≤ 192 UTF-8 bytes, no control characters).
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
    /// Sets `msg`, one line of text (≤ 2048 UTF-8 bytes, no control characters).
    pub fn msg(mut self, msg: impl Into<String>) -> Self {
        self.msg = Some(msg.into());
        self
    }

    /// Strict build. It fails if `msg` or `title` contains a control
    /// character or is over its limit, or if `kind` or `progress` is set on a
    /// state they don't apply to.
    pub fn try_build(self) -> Result<Report, BuildError> {
        if self.kind.is_some() && !self.state.allows_kind() {
            return Err(BuildError::KindNotAllowed(self.state));
        }
        if self.progress.is_some() && !self.state.allows_progress() {
            return Err(BuildError::ProgressNotAllowed(self.state));
        }
        for (key, text, max) in [
            ("title", &self.title, MAX_TITLE_DECODED),
            ("msg", &self.msg, MAX_MSG_DECODED),
        ] {
            if let Some(t) = text {
                if t.chars().any(char::is_control) {
                    return Err(BuildError::ControlChar { key });
                }
                if t.len() > max {
                    return Err(BuildError::TooLong {
                        key,
                        len: t.len(),
                        max,
                    });
                }
            }
        }
        Ok(self.into_report())
    }

    /// Lossy build that never fails. It strips control characters (newlines
    /// and tabs too), truncates `msg`/`title` on a char boundary to their
    /// limits, and drops `kind`/`progress` when the state doesn't allow them.
    pub fn build_lossy(mut self) -> Report {
        if !self.state.allows_kind() {
            self.kind = None;
        }
        if !self.state.allows_progress() {
            self.progress = None;
        }
        self.title = self.title.map(|t| sanitize(&t, MAX_TITLE_DECODED));
        self.msg = self.msg.map(|m| sanitize(&m, MAX_MSG_DECODED));
        self.into_report()
    }

    fn into_report(self) -> Report {
        Report {
            state: self.state,
            kind: self.kind,
            progress: self.progress,
            app: self.app,
            id: self.id,
            title: self.title,
            msg: self.msg,
        }
    }
}

/// Removes control characters and truncates to `max` bytes on a char boundary.
fn sanitize(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max));
    for c in text.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// Serde input for `Report`. Validated through [`ReportBuilder::try_build`].
#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
struct ReportFields {
    state: State,
    #[serde(default)]
    kind: Option<Kind>,
    #[serde(default)]
    progress: Option<Progress>,
    #[serde(default)]
    app: Option<App>,
    #[serde(default)]
    id: Option<Id>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    msg: Option<String>,
}

#[cfg(feature = "serde")]
impl TryFrom<ReportFields> for Report {
    type Error = BuildError;
    fn try_from(f: ReportFields) -> Result<Self, Self::Error> {
        ReportBuilder {
            state: f.state,
            kind: f.kind,
            progress: f.progress,
            app: f.app,
            id: f.id,
            title: f.title,
            msg: f.msg,
        }
        .try_build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_rejects_bad_combinations() {
        let r = Report::builder(State::Working).kind(Kind::Auth).try_build();
        assert_eq!(r, Err(BuildError::KindNotAllowed(State::Working)));
        let r = Report::builder(State::Done)
            .progress(Progress::new(5).unwrap())
            .try_build();
        assert_eq!(r, Err(BuildError::ProgressNotAllowed(State::Done)));
        let r = Report::builder(State::Done).msg("a\nb").try_build();
        assert_eq!(r, Err(BuildError::ControlChar { key: "msg" }));
        let r = Report::builder(State::Done).title("\u{85}").try_build();
        assert_eq!(r, Err(BuildError::ControlChar { key: "title" }));
    }

    #[test]
    fn strict_limits() {
        let ok = "a".repeat(MAX_MSG_DECODED);
        assert!(Report::builder(State::Done).msg(&ok).try_build().is_ok());
        let over = "a".repeat(MAX_MSG_DECODED + 1);
        assert!(matches!(
            Report::builder(State::Done).msg(over).try_build(),
            Err(BuildError::TooLong { key: "msg", .. })
        ));
        let ok = "a".repeat(MAX_TITLE_DECODED);
        assert!(Report::builder(State::Done).title(&ok).try_build().is_ok());
        let over = "a".repeat(MAX_TITLE_DECODED + 1);
        assert!(
            Report::builder(State::Done)
                .title(over)
                .try_build()
                .is_err()
        );
    }

    #[test]
    fn lossy_strips_and_truncates_on_char_boundary() {
        let r = Report::builder(State::Done)
            .kind(Kind::Auth)
            .progress(Progress::new(1).unwrap())
            .msg("line1\nline2\t\u{7f}\u{9b}!")
            // 191 ASCII bytes, then a 2-byte char that would cross 192.
            .title(format!("{}é", "a".repeat(191)))
            .build_lossy();
        assert_eq!(r.kind(), None);
        assert_eq!(r.progress(), None);
        assert_eq!(r.msg(), Some("line1line2!"));
        assert_eq!(r.title().unwrap().len(), 191);
    }

    #[test]
    fn encode_bel_and_clear() {
        let r = Report::clear(Some(Id::new("us-east").unwrap()));
        assert_eq!(
            r.encode(Terminator::Bel),
            b"\x1b]7501;state=clear:id=us-east\x07"
        );
        assert_eq!(
            Report::clear(None).encode(Terminator::St),
            b"\x1b]7501;state=clear\x1b\\"
        );
    }
}
