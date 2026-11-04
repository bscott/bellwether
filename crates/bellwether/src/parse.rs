use base64::Engine as _;
use base64::alphabet::STANDARD;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use crate::error::ParseError;
use crate::limits::{
    MAX_APP, MAX_KEY, MAX_MSG_DECODED, MAX_MSG_ENCODED, MAX_SEQUENCE, MAX_TITLE_DECODED,
    MAX_TITLE_ENCODED,
};
use crate::report::Report;
use crate::types::{App, Id, Kind, Progress, State};
use crate::{PREFIX, pairs, trim_ws};

/// The longest body [`parse`] accepts: 4096 minus the shortest framing
/// (`ESC ] 7501 ;` plus a one-byte BEL terminator), which is 4088.
/// [`Scanner`](crate::Scanner) applies the exact 4096-byte limit to the whole
/// sequence, terminator included.
pub const MAX_BODY: usize = MAX_SEQUENCE - PREFIX.len() - 1;

/// Standard alphabet, padding optional (spec: *Syntax*).
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Parses one report body: the bytes after `ESC ] 7501 ;` and before ST.
///
/// This implements the receiver rules in spec sections *Syntax*, *Records and
/// ids*, *Keys* and *Limits*:
///
/// * Malformed pairs (no `=`, an empty or non-`[a-z]` key, a value byte outside
///   `[A-Za-z0-9_.,+/=-]`) are skipped. Whitespace around keys and values is
///   trimmed.
/// * Unknown keys are ignored. When a key repeats, the last value wins.
/// * `kind` outside `blocked`, `progress` outside `working`/`blocked`, and
///   values that don't match those keys or `app` are treated as absent.
/// * The whole report is discarded (`Err`) when `state` is missing or unknown,
///   `id` is invalid, `msg`/`title` base64 or UTF-8 is bad, decoded text has a
///   control character, or any limit is broken.
///
/// A body starting with `?` is a feature detection message and returns
/// [`ParseError::NotAReport`]. See [`detect`](crate::detect).
pub fn parse(body: &[u8]) -> Result<Report, ParseError> {
    if body.len() > MAX_BODY {
        return Err(ParseError::TooLong { len: body.len() });
    }
    if trim_ws(body).first() == Some(&b'?') {
        return Err(ParseError::NotAReport);
    }

    let mut state = None;
    let mut kind = None;
    let mut progress = None;
    let mut app = None;
    let mut id = None;
    let mut title = None;
    let mut msg = None;

    for (key, value) in pairs(body) {
        if key.len() > MAX_KEY {
            return Err(ParseError::KeyTooLong {
                key: String::from_utf8_lossy(key).into_owned(),
            });
        }
        let slot = match key {
            b"state" => &mut state,
            b"kind" => &mut kind,
            b"progress" => &mut progress,
            b"app" => &mut app,
            b"id" => &mut id,
            b"title" => &mut title,
            b"msg" => &mut msg,
            _ => continue,
        };
        *slot = Some(value);
    }

    // Value bytes are ASCII by construction (`pairs` checked the value set).
    let ascii = |v: &[u8]| {
        std::str::from_utf8(v)
            .expect("value set is ASCII")
            .to_owned()
    };

    let state: State = {
        let v = state.ok_or(ParseError::MissingState)?;
        ascii(v)
            .parse()
            .map_err(|_| ParseError::UnknownState(ascii(v)))?
    };

    let id = id
        .map(|v| Id::new(&ascii(v)).map_err(ParseError::InvalidId))
        .transpose()?;

    let app = match app {
        Some(v) if v.len() > MAX_APP => {
            return Err(ParseError::ValueTooLong {
                key: "app",
                encoded: true,
                len: v.len(),
                max: MAX_APP,
            });
        }
        Some(v) => App::new(&ascii(v)).ok(),
        None => None,
    };

    let title = title
        .map(|v| decode_text("title", v, MAX_TITLE_ENCODED, MAX_TITLE_DECODED))
        .transpose()?;
    let msg = msg
        .map(|v| decode_text("msg", v, MAX_MSG_ENCODED, MAX_MSG_DECODED))
        .transpose()?;

    let kind = kind
        .filter(|_| state.allows_kind())
        .and_then(|v| ascii(v).parse::<Kind>().ok());
    let progress = progress
        .filter(|_| state.allows_progress())
        .and_then(parse_progress);

    Ok(Report {
        state,
        kind,
        progress,
        app,
        id,
        title,
        msg,
    })
}

/// Integer 0–100 written as 1–3 ASCII digits. Anything else is treated as
/// absent.
fn parse_progress(v: &[u8]) -> Option<Progress> {
    if v.is_empty() || v.len() > 3 || !v.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n = v.iter().fold(0u16, |n, d| n * 10 + u16::from(d - b'0'));
    u8::try_from(n).ok().and_then(Progress::new)
}

fn decode_text(
    key: &'static str,
    v: &[u8],
    max_encoded: usize,
    max_decoded: usize,
) -> Result<String, ParseError> {
    if v.len() > max_encoded {
        return Err(ParseError::ValueTooLong {
            key,
            encoded: true,
            len: v.len(),
            max: max_encoded,
        });
    }
    let bytes = BASE64
        .decode(v)
        .map_err(|_| ParseError::InvalidBase64 { key })?;
    if bytes.len() > max_decoded {
        return Err(ParseError::ValueTooLong {
            key,
            encoded: false,
            len: bytes.len(),
            max: max_decoded,
        });
    }
    let text = String::from_utf8(bytes).map_err(|_| ParseError::InvalidBase64 { key })?;
    if text.chars().any(char::is_control) {
        return Err(ParseError::ControlChar { key });
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Report, ParseError> {
        parse(s.as_bytes())
    }

    #[test]
    fn minimal() {
        let r = p("state=idle").unwrap();
        assert_eq!(r.state(), State::Idle);
        assert_eq!(r.id(), None);
        assert_eq!(r.msg(), None);
    }

    #[test]
    fn missing_and_unknown_state_discard() {
        assert_eq!(p("msg=SGk="), Err(ParseError::MissingState));
        assert_eq!(p(""), Err(ParseError::MissingState));
        assert_eq!(
            p("state=sleeping"),
            Err(ParseError::UnknownState("sleeping".into()))
        );
        assert_eq!(
            p("state=Working"),
            Err(ParseError::UnknownState("Working".into()))
        );
        assert_eq!(p("state="), Err(ParseError::UnknownState("".into())));
    }

    #[test]
    fn malformed_pairs_are_skipped() {
        // no '=', empty key, uppercase key, bad value byte, then a good pair
        let r = p("junk:=x:Msg=SGk=:msg=S Gk=:app=a;b:state=done:app=ok").unwrap();
        assert_eq!(r.state(), State::Done);
        assert_eq!(r.app().unwrap().as_str(), "ok");
        assert_eq!(r.msg(), None);
        // A malformed state pair is skipped, so the state is missing.
        assert_eq!(p("state=wor king"), Err(ParseError::MissingState));
    }

    #[test]
    fn unknown_keys_ignored_and_last_wins() {
        let r = p("future=1:state=working:state=done:msg=YQ==:msg=Yg==").unwrap();
        assert_eq!(r.state(), State::Done);
        assert_eq!(r.msg(), Some("b"));
        // Only the winning value is validated.
        assert!(p("state=done:msg=S:msg=Yg==").is_ok());
        assert!(p("state=done:id=a//b:id=ok").is_ok());
        assert!(p("state=done:msg=@@@").is_ok()); // '@' is outside the value set: skipped
    }

    #[test]
    fn whitespace_trimmed() {
        let r = p(" state = blocked :\tkind=auth\t: app = x ").unwrap();
        assert_eq!(r.state(), State::Blocked);
        assert_eq!(r.kind(), Some(Kind::Auth));
        assert_eq!(r.app().unwrap().as_str(), "x");
    }

    #[test]
    fn kind_only_with_blocked() {
        assert_eq!(p("state=working:kind=auth").unwrap().kind(), None);
        assert_eq!(p("state=blocked:kind=nope").unwrap().kind(), None);
        assert_eq!(
            p("state=blocked:kind=question").unwrap().kind(),
            Some(Kind::Question)
        );
    }

    #[test]
    fn progress_rules() {
        let prog = |s: &str| p(s).unwrap().progress().map(Progress::get);
        assert_eq!(prog("state=working:progress=0"), Some(0));
        assert_eq!(prog("state=working:progress=100"), Some(100));
        assert_eq!(prog("state=blocked:progress=040"), Some(40));
        assert_eq!(prog("state=working:progress=101"), None);
        assert_eq!(prog("state=working:progress=-1"), None);
        assert_eq!(prog("state=working:progress=+5"), None);
        assert_eq!(prog("state=working:progress=5.5"), None);
        assert_eq!(prog("state=working:progress=1000"), None);
        assert_eq!(prog("state=working:progress="), None);
        assert_eq!(prog("state=done:progress=50"), None);
        assert_eq!(prog("state=idle:progress=50"), None);
    }

    #[test]
    fn app_outside_charset_is_absent() {
        assert_eq!(p("state=done:app=a,b").unwrap().app(), None);
        assert_eq!(p("state=done:app=a/b").unwrap().app(), None);
        assert_eq!(p("state=done:app=a=b").unwrap().app(), None);
        assert_eq!(p("state=done:app=").unwrap().app(), None);
    }

    #[test]
    fn base64_rules() {
        assert_eq!(p("state=done:msg=SGk").unwrap().msg(), Some("Hi")); // no padding
        assert_eq!(p("state=done:msg=SGk=").unwrap().msg(), Some("Hi"));
        assert_eq!(p("state=done:msg=").unwrap().msg(), Some(""));
        assert_eq!(
            p("state=done:msg=S"),
            Err(ParseError::InvalidBase64 { key: "msg" })
        );
        assert_eq!(
            p("state=done:title=SGk=SGk="),
            Err(ParseError::InvalidBase64 { key: "title" })
        );
        // 0xFF is not UTF-8
        assert_eq!(
            p("state=done:msg=/w=="),
            Err(ParseError::InvalidBase64 { key: "msg" })
        );
    }

    #[test]
    fn control_chars_discard() {
        // "a\nb", "\t", "\u{7f}", "\u{85}" (C1 NEL)
        for b64 in ["YQpi", "CQ==", "fw==", "woU="] {
            assert_eq!(
                p(&format!("state=done:msg={b64}")),
                Err(ParseError::ControlChar { key: "msg" }),
                "{b64}"
            );
        }
        assert_eq!(
            p("state=done:title=YQpi"),
            Err(ParseError::ControlChar { key: "title" })
        );
    }

    #[test]
    fn invalid_id_discards() {
        assert!(matches!(
            p("state=done:id=a//b"),
            Err(ParseError::InvalidId(_))
        ));
        assert!(matches!(p("state=done:id="), Err(ParseError::InvalidId(_))));
        assert!(matches!(
            p("state=done:id=a,b"),
            Err(ParseError::InvalidId(_))
        ));
    }

    #[test]
    fn query_is_not_a_report() {
        assert_eq!(p("?"), Err(ParseError::NotAReport));
        assert_eq!(p(" ? : states=idle"), Err(ParseError::NotAReport));
    }
}
