//! Reading status from a program's terminal output (`run --pty`).
//!
//! [`Watcher`] follows the text of the line the cursor is on, with escape
//! sequences stripped, and matches it against the presets' patterns:
//!
//! * A progress pattern is tried at every line end (`\n`, `\r`, cursor
//!   movement) and whenever output pauses, so `\r`-redrawn progress bars work.
//! * Prompt (`blocked`) patterns are tried only on the unfinished line when
//!   output pauses, and must still match [`DEBOUNCE`] later. Prompts don't end
//!   in a newline, and a program that is waiting stops printing.
//! * The program counts as unblocked again when the user presses Enter or the
//!   program finishes the prompt line.
//!
//! The watcher is pure: no I/O, time passed in.

use std::time::{Duration, Instant};

use bellwether::Kind;
use regex::bytes::Regex;

use crate::preset::{Blocked, Preset};

/// How long a prompt must sit unchanged before it counts.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// Longest line kept. Prompts and status lines are much shorter; a longer line
/// keeps its tail.
const MAX_LINE: usize = 1024;

/// Longest prompt text used as a `msg`, in characters.
const MAX_PROMPT_CHARS: usize = 200;

/// The patterns from the selected presets, in priority order.
#[derive(Debug, Default)]
pub struct Rules {
    progress: Vec<Regex>,
    blocked: Vec<Blocked>,
}

impl Rules {
    pub fn from_presets(presets: &[Preset]) -> Rules {
        Rules {
            progress: presets.iter().filter_map(|p| p.progress.clone()).collect(),
            blocked: presets.iter().flat_map(|p| p.blocked.clone()).collect(),
        }
    }
}

/// What the output says the program is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Working,
    Blocked { kind: Kind, msg: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vt {
    Ground,
    Esc,
    /// After ESC and an intermediate byte, as in `ESC ( B` (charset) or
    /// `ESC # 8`, up to the final byte.
    EscInt,
    Csi,
    /// OSC, DCS, SOS, PM or APC string, up to ST or BEL.
    Str,
    StrEsc,
}

#[derive(Debug)]
struct Candidate {
    kind: Kind,
    msg: String,
    due: Instant,
}

pub struct Watcher {
    rules: Rules,
    line: Vec<u8>,
    vt: Vt,
    /// CSI parameter bytes.
    params: Vec<u8>,
    /// UTF-8 continuation bytes still expected.
    utf8_left: u8,
    progress: Option<u8>,
    blocked: Option<(Kind, String)>,
    candidate: Option<Candidate>,
}

impl Watcher {
    pub fn new(rules: Rules) -> Watcher {
        Watcher {
            rules,
            line: Vec::new(),
            vt: Vt::Ground,
            params: Vec::new(),
            utf8_left: 0,
            progress: None,
            blocked: None,
            candidate: None,
        }
    }

    /// Feeds program output, then [`pause`](Self::pause)s.
    #[cfg(test)]
    pub fn output(&mut self, bytes: &[u8], now: Instant) {
        self.feed(bytes);
        self.pause(now);
    }

    /// Feeds program output. Call [`pause`](Self::pause) once the available
    /// output has been fed.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
    }

    /// Feeds user input. Enter answers a prompt.
    pub fn input(&mut self, bytes: &[u8]) {
        if bytes.iter().any(|&b| b == b'\r' || b == b'\n') {
            self.blocked = None;
            self.candidate = None;
        }
    }

    /// Promotes a prompt that has sat unchanged for [`DEBOUNCE`].
    pub fn tick(&mut self, now: Instant) {
        if self.candidate.as_ref().is_some_and(|c| c.due <= now) {
            let c = self.candidate.take().expect("checked");
            self.blocked = Some((c.kind, c.msg));
        }
    }

    /// When [`tick`](Self::tick) next has something to do.
    pub fn deadline(&self) -> Option<Instant> {
        self.candidate.as_ref().map(|c| c.due)
    }

    pub fn status(&self) -> Status {
        match &self.blocked {
            Some((kind, msg)) => Status::Blocked {
                kind: *kind,
                msg: msg.clone(),
            },
            None => Status::Working,
        }
    }

    /// The last percentage seen.
    pub fn progress(&self) -> Option<u8> {
        self.progress
    }

    /// True when the output so far ends cleanly: not inside an escape sequence
    /// or a UTF-8 character. Only then is it safe to write our own sequence to
    /// the same terminal.
    pub fn at_boundary(&self) -> bool {
        self.vt == Vt::Ground && self.utf8_left == 0
    }

    fn byte(&mut self, b: u8) {
        match self.vt {
            Vt::Ground => self.ground(b),
            Vt::Esc => {
                self.vt = Vt::Ground;
                match b {
                    b'[' => {
                        self.params.clear();
                        self.vt = Vt::Csi;
                    }
                    b']' | b'P' | b'X' | b'^' | b'_' => self.vt = Vt::Str,
                    0x20..=0x2f => self.vt = Vt::EscInt,
                    0x1b => self.vt = Vt::Esc,
                    // RIS: full reset.
                    b'c' => self.end_line(),
                    _ => {}
                }
            }
            Vt::EscInt => match b {
                0x20..=0x2f => {}
                0x1b => self.vt = Vt::Esc,
                // The final byte, or CAN/SUB aborting the sequence.
                0x30..=0x7e | 0x18 | 0x1a => self.vt = Vt::Ground,
                _ => {}
            },
            Vt::Csi => match b {
                0x30..=0x3f => {
                    if self.params.len() < 32 {
                        self.params.push(b);
                    }
                }
                0x20..=0x2f => {}
                0x40..=0x7e => {
                    self.vt = Vt::Ground;
                    self.csi(b);
                }
                0x1b => self.vt = Vt::Esc,
                0x18 | 0x1a => self.vt = Vt::Ground,
                _ => {}
            },
            Vt::Str => match b {
                0x07 | 0x18 | 0x1a => self.vt = Vt::Ground,
                0x1b => self.vt = Vt::StrEsc,
                _ => {}
            },
            Vt::StrEsc => {
                if b == b'\\' {
                    self.vt = Vt::Ground;
                } else {
                    self.vt = Vt::Esc;
                    self.byte(b);
                }
            }
        }
    }

    fn ground(&mut self, b: u8) {
        if b < 0x80 {
            self.utf8_left = 0;
        } else if b & 0xc0 == 0x80 {
            self.utf8_left = self.utf8_left.saturating_sub(1);
        } else {
            self.utf8_left = match b {
                0xf0.. => 3,
                0xe0.. => 2,
                _ => 1,
            };
        }
        match b {
            0x1b => self.vt = Vt::Esc,
            b'\n' | b'\r' => self.end_line(),
            0x08 => {
                // Backspace: drop the last character, continuation bytes and all.
                while let Some(c) = self.line.pop() {
                    if c & 0xc0 != 0x80 {
                        break;
                    }
                }
            }
            b'\t' => self.push(b' '),
            0x00..=0x1f | 0x7f => {}
            _ => self.push(b),
        }
    }

    fn push(&mut self, b: u8) {
        if self.line.len() >= MAX_LINE {
            self.line.drain(..MAX_LINE / 4);
        }
        self.line.push(b);
    }

    fn csi(&mut self, final_byte: u8) {
        let p = self.params.as_slice();
        let moves_line = match final_byte {
            // Cursor up/down/next/previous line, position, erase display.
            b'A' | b'B' | b'E' | b'F' | b'H' | b'f' | b'd' | b'J' => true,
            // Erase line (1 or 2). Erase to end of line keeps the text before it.
            b'K' => p == b"1" || p == b"2",
            // Cursor to column 1: a `\r`.
            b'G' | b'`' => p.is_empty() || p == b"1",
            _ => false,
        };
        if moves_line {
            self.end_line();
        }
    }

    /// The cursor left the line: check it for progress, and treat a prompt on
    /// it as answered.
    fn end_line(&mut self) {
        self.check_progress();
        self.line.clear();
        self.blocked = None;
        self.candidate = None;
    }

    fn check_progress(&mut self) {
        for re in &self.rules.progress {
            if let Some(pct) = re
                .captures(&self.line)
                .and_then(|c| c.get(1))
                .and_then(|m| std::str::from_utf8(m.as_bytes()).ok())
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| (0.0..=100.0).contains(v))
            {
                self.progress = Some(pct.floor() as u8);
                return;
            }
        }
    }

    /// Output has stopped for now: check the unfinished line for progress and
    /// prompts. `now` starts the prompt debounce.
    pub fn pause(&mut self, now: Instant) {
        self.check_progress();
        if self.blocked.is_some() {
            return;
        }
        let found = self
            .rules
            .blocked
            .iter()
            .find(|r| r.pattern.is_match(&self.line));
        self.candidate = found.map(|rule| {
            let msg = rule.msg.clone().unwrap_or_else(|| prompt_text(&self.line));
            match self.candidate.take() {
                // Same prompt still showing: keep the original deadline.
                Some(c) if c.kind == rule.kind && c.msg == msg => c,
                _ => Candidate {
                    kind: rule.kind,
                    msg,
                    due: now + DEBOUNCE,
                },
            }
        });
    }
}

/// The prompt line as a report message: trimmed, at most
/// [`MAX_PROMPT_CHARS`], never empty. Control characters were already dropped.
fn prompt_text(line: &[u8]) -> String {
    let text = String::from_utf8_lossy(line);
    let text = text.trim();
    if text.is_empty() {
        return "Waiting for input".to_owned();
    }
    if text.chars().count() > MAX_PROMPT_CHARS {
        let cut: String = text.chars().take(MAX_PROMPT_CHARS - 1).collect();
        format!("{cut}…")
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::{BUILTIN, parse};

    fn watcher() -> Watcher {
        let presets = parse(BUILTIN, "built-in").unwrap();
        Watcher::new(Rules::from_presets(&presets))
    }

    fn settle(w: &mut Watcher, t: Instant) -> Instant {
        let later = t + DEBOUNCE;
        w.tick(later);
        later
    }

    #[test]
    fn progress_from_carriage_return_redraws() {
        let mut w = watcher();
        let t = Instant::now();
        w.output(b"[0:01] 12.50%  3 files\r[0:02] 47.9", t);
        assert_eq!(w.progress(), Some(12));
        w.output(b"0%  9 files\r", t);
        assert_eq!(w.progress(), Some(47));
        // Escape sequences in the line don't get in the way.
        w.output(b"\x1b[2K\x1b[1m[0:03]\x1b[0m 100.00%", t);
        assert_eq!(w.progress(), Some(100));
        // Out of range is ignored.
        w.output(b"\n[0:04] 250%\n", t);
        assert_eq!(w.progress(), Some(100));
    }

    #[test]
    fn prompt_needs_to_settle() {
        let mut w = watcher();
        let t = Instant::now();
        w.output(b"scanning...\nenter password for repository: ", t);
        assert_eq!(w.status(), Status::Working);
        assert_eq!(w.deadline(), Some(t + DEBOUNCE));
        w.tick(t + DEBOUNCE / 2);
        assert_eq!(w.status(), Status::Working);
        settle(&mut w, t);
        assert_eq!(
            w.status(),
            Status::Blocked {
                kind: Kind::Auth,
                msg: "enter password for repository:".into()
            }
        );
        // Enter answers it.
        w.input(b"hunter2\r");
        assert_eq!(w.status(), Status::Working);
        assert_eq!(w.deadline(), None);
    }

    #[test]
    fn prompt_split_across_chunks() {
        let mut w = watcher();
        let t = Instant::now();
        w.output(b"[sudo] pass", t);
        assert_eq!(w.deadline(), None);
        w.output(b"word for alice: ", t);
        settle(&mut w, t);
        assert!(matches!(
            w.status(),
            Status::Blocked {
                kind: Kind::Auth,
                ..
            }
        ));
        // The program finishing the line also unblocks.
        w.output(b"\n", t);
        assert_eq!(w.status(), Status::Working);
    }

    #[test]
    fn text_after_a_prompt_cancels_it() {
        let mut w = watcher();
        let t = Instant::now();
        w.output(b"Password:", t);
        assert!(w.deadline().is_some());
        w.output(b" was changed yesterday", t);
        assert_eq!(w.deadline(), None);
        settle(&mut w, t);
        assert_eq!(w.status(), Status::Working);
    }

    #[test]
    fn escape_with_intermediate_is_stripped() {
        // `tput sgr0` on Linux: designate US-ASCII, then reset attributes.
        let mut w = watcher();
        let t = Instant::now();
        w.output(b"\x1b(", t);
        assert!(!w.at_boundary());
        w.output(b"B\x1b[mPassword: ", t);
        assert!(w.at_boundary());
        settle(&mut w, t);
        assert_eq!(
            w.status(),
            Status::Blocked {
                kind: Kind::Auth,
                msg: "Password:".into()
            }
        );
    }

    #[test]
    fn boundary_tracking() {
        let mut w = watcher();
        let t = Instant::now();
        w.output(b"ok", t);
        assert!(w.at_boundary());
        w.output(b"\x1b[3", t);
        assert!(!w.at_boundary());
        w.output(b"1m", t);
        assert!(w.at_boundary());
        w.output("é".as_bytes().split_at(1).0, t);
        assert!(!w.at_boundary());
        w.output("é".as_bytes().split_at(1).1, t);
        assert!(w.at_boundary());
        w.output(b"\x1b]0;title", t);
        assert!(!w.at_boundary());
        w.output(b"\x1b\\", t);
        assert!(w.at_boundary());
    }

    #[test]
    fn line_editing() {
        let mut w = watcher();
        let t = Instant::now();
        // Backspace over a multi-byte character, and a title OSC mid-line.
        w.output("Passwö\x08ord\x1b]0;x\x07:".as_bytes(), t);
        settle(&mut w, t);
        assert!(matches!(w.status(), Status::Blocked { .. }));
    }

    #[test]
    fn prompt_text_is_trimmed_and_bounded() {
        assert_eq!(prompt_text(b"  Password:  "), "Password:");
        assert_eq!(prompt_text(b"   "), "Waiting for input");
        let long = prompt_text(&[b'x'; 500]);
        assert_eq!(long.chars().count(), MAX_PROMPT_CHARS);
    }
}
