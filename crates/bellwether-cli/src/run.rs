//! `bellwether run`: report `working`, run the command, report the outcome.

use std::ffi::OsString;
use std::io::{self, ErrorKind};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitCode, ExitStatus};
use std::time::{Duration, Instant};

use bellwether::{App, Id, Kind, Progress, Report, State};
use nix::sys::signal::Signal;

use crate::RunArgs;
use crate::output::Sink;
use crate::preset;
use crate::pty::{self, Shown};
use crate::sys::{self, Forwarder, Hold};
use crate::watch::Rules;

/// Longest default `--msg` (the command line) in characters.
const DEFAULT_MSG_CHARS: usize = 100;

/// Tells a nested `bellwether run` which record to report beneath. Empty means
/// the root record.
pub const PARENT_ENV: &str = "BELLWETHER_PARENT_ID";

/// The record id `run --pty` moves to, beneath its own, when the command
/// reports on the same record.
const FALLBACK_SEGMENT: &str = "bellwether";

/// Sends reports for one record. Reporting is best-effort: a failed write never
/// affects the command or its exit status.
pub struct Reporter {
    sink: Sink,
    app: Option<App>,
    /// The record reported on. `run --pty` changes it when moving.
    pub id: Option<Id>,
}

impl Reporter {
    pub fn send(&self, state: State, kind: Option<Kind>, progress: Option<u8>, msg: &str) {
        let mut b = Report::builder(state).maybe_id(self.id.clone());
        if let Some(a) = &self.app {
            b = b.app(a.clone());
        }
        if let Some(k) = kind {
            b = b.kind(k);
        }
        if let Some(p) = progress.and_then(Progress::new) {
            b = b.progress(p);
        }
        let _ = self.sink.write(&b.msg(msg).build_lossy());
    }
}

/// Why `run` couldn't produce an exit status for the command.
pub enum Failure {
    /// The command couldn't be started.
    Spawn(io::Error),
    /// The PTY couldn't be set up.
    Setup(io::Error),
    /// Lost track of the running command.
    Wait(io::Error),
}

pub fn run(args: RunArgs) -> ExitCode {
    // Check user-supplied text and presets up front: never run the command and
    // then fail to report on it.
    for (flag, text) in [
        ("--msg", &args.msg),
        ("--done-msg", &args.done_msg),
        ("--error-msg", &args.error_msg),
    ] {
        if let Some(t) = text
            && let Err(e) = Report::builder(State::Working).msg(t.clone()).try_build()
        {
            eprintln!("bellwether: {flag}: {e}");
            return ExitCode::from(2);
        }
    }
    let rules = if args.pty {
        match preset::load_all()
            .and_then(|all| preset::select(&all, args.preset.as_deref(), &args.command))
        {
            Ok(p) => Rules::from_presets(&p),
            Err(e) => {
                eprintln!("bellwether: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        Rules::default()
    };

    let program = &args.command[0];
    let app = args.app.clone().or_else(|| default_app(program));
    let ids = resolve_ids(args.id.clone(), app.as_ref(), std::env::var_os(PARENT_ENV));
    if let Some(w) = &ids.warning {
        eprintln!("bellwether: {w}");
    }
    // A broken /dev/tty also turns reporting off.
    let sink = match ids.report {
        true => Sink::open(args.output.stdout).unwrap_or(Sink::Off),
        false => Sink::Off,
    };
    let mut reporter = Reporter {
        sink,
        app,
        id: ids.record.clone(),
    };

    // Block the signals we take over before spawning any thread, so every
    // thread inherits the mask and only the forwarder's sigwait takes them.
    let hold = Hold::new(match args.pty {
        true => sys::with(&[Signal::SIGWINCH]),
        false => sys::handled(),
    });

    let working_msg = args
        .msg
        .clone()
        .unwrap_or_else(|| default_msg(&args.command));
    reporter.send(State::Working, None, None, &working_msg);
    let start = Instant::now();

    let mut cmd = Command::new(program);
    cmd.args(&args.command[1..]).env(PARENT_ENV, &ids.export);
    hold.unblock_in_child(&mut cmd);
    let result = if args.pty {
        let shown = Shown {
            state: State::Working,
            kind: None,
            progress: None,
            msg: working_msg,
        };
        let fallback = match &ids.record {
            Some(id) => id.child(FALLBACK_SEGMENT).ok(),
            None => Id::new(FALLBACK_SEGMENT).ok(),
        };
        pty::run(cmd, &mut reporter, rules, shown, fallback)
    } else {
        plain(cmd)
    };

    let status = match result {
        Ok(s) => s,
        Err(Failure::Spawn(e)) => {
            eprintln!("bellwether: {program}: {e}");
            reporter.send(
                State::Error,
                None,
                None,
                &format!("Failed to start {program}: {e}"),
            );
            // Shell conventions: 127 not found, 126 found but not runnable.
            return ExitCode::from(if e.kind() == ErrorKind::NotFound {
                127
            } else {
                126
            });
        }
        Err(Failure::Setup(e)) => {
            eprintln!("bellwether: cannot set up a pseudoterminal: {e}");
            reporter.send(
                State::Error,
                None,
                None,
                &format!("Failed to start {program}: no pseudoterminal: {e}"),
            );
            return ExitCode::FAILURE;
        }
        Err(Failure::Wait(e)) => {
            eprintln!("bellwether: waiting for {program}: {e}");
            reporter.send(
                State::Error,
                None,
                None,
                &format!("Lost track of {program}: {e}"),
            );
            return ExitCode::FAILURE;
        }
    };
    let elapsed = fmt_duration(start.elapsed());

    let outcome = Outcome::of(status);
    let (state, msg) = match outcome {
        Outcome::Exited(0) => (
            State::Done,
            args.done_msg
                .unwrap_or_else(|| format!("Finished in {elapsed}")),
        ),
        Outcome::Exited(code) => (
            State::Error,
            args.error_msg
                .unwrap_or_else(|| format!("Exited {code} after {elapsed}")),
        ),
        Outcome::Signaled(sig) if is_cancel(sig) => (
            State::Idle,
            format!("Interrupted by {} after {elapsed}", sig_name(sig)),
        ),
        Outcome::Signaled(sig) => (
            State::Error,
            args.error_msg
                .unwrap_or_else(|| format!("Killed by {} after {elapsed}", sig_name(sig))),
        ),
    };
    reporter.send(state, None, None, &msg);

    match outcome {
        Outcome::Exited(code) => ExitCode::from(code as u8),
        Outcome::Signaled(sig) => match Signal::try_from(sig) {
            Ok(s) => sys::die_by_signal(s),
            Err(_) => ExitCode::from((128 + sig).clamp(0, 255) as u8),
        },
    }
}

/// Plain `run`: the command shares our terminal and stdio.
fn plain(mut cmd: Command) -> Result<ExitStatus, Failure> {
    let mut child = cmd.spawn().map_err(Failure::Spawn)?;
    let forwarder = Forwarder::plain(child.id());
    let status = loop {
        match child.wait() {
            Ok(s) => break s,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(Failure::Wait(e)),
        }
    };
    forwarder.child_reaped();
    Ok(status)
}

/// Which record `run` reports on, and what it tells nested runs.
#[derive(Debug, PartialEq, Eq)]
struct Ids {
    /// Our record. `None` is the root record.
    record: Option<Id>,
    /// [`PARENT_ENV`] for the command: our record, or empty for the root.
    export: String,
    /// False if no valid record could be formed; reporting is then off rather
    /// than clobbering the parent's record.
    report: bool,
    warning: Option<String>,
}

/// Works out our record from `--id`, the app name and [`PARENT_ENV`].
///
/// * Not nested (`parent` unset): `--id`, or the root record. Same as before
///   nesting existed.
/// * Nested: `parent/<--id>`, or `parent/<app>`, or `parent/bellwether`. An
///   empty `parent` is the root record, so the result is just `<--id>` etc.
fn resolve_ids(id: Option<Id>, app: Option<&App>, parent: Option<OsString>) -> Ids {
    let mut warning = None;
    let parent = match parent.map(|p| p.into_string()) {
        None => None,
        Some(Ok(p)) if p.is_empty() => Some(None),
        Some(Ok(p)) => match Id::new(&p) {
            Ok(id) => Some(Some(id)),
            Err(e) => {
                warning = Some(format!("ignoring {PARENT_ENV}={p:?}: {e}"));
                None
            }
        },
        Some(Err(p)) => {
            warning = Some(format!("ignoring {PARENT_ENV}={p:?}: not UTF-8"));
            None
        }
    };
    let Some(parent) = parent else {
        let export = id
            .as_ref()
            .map(|i| i.as_str().to_owned())
            .unwrap_or_default();
        return Ids {
            record: id,
            export,
            report: true,
            warning,
        };
    };
    let own = id
        .as_ref()
        .map(Id::as_str)
        .or(app.map(App::as_str))
        .unwrap_or(FALLBACK_SEGMENT);
    let joined = match &parent {
        Some(p) => format!("{}/{own}", p.as_str()),
        None => own.to_owned(),
    };
    match Id::new(&joined) {
        Ok(record) => Ids {
            export: record.as_str().to_owned(),
            record: Some(record),
            report: true,
            warning,
        },
        Err(e) => Ids {
            record: parent.clone(),
            export: parent.map(|p| p.as_str().to_owned()).unwrap_or_default(),
            report: false,
            warning: Some(format!(
                "not reporting: nested record id {joined:?} is invalid: {e}"
            )),
        },
    }
}

enum Outcome {
    Exited(i32),
    Signaled(i32),
}

impl Outcome {
    fn of(status: ExitStatus) -> Outcome {
        match (status.code(), status.signal()) {
            (Some(code), _) => Outcome::Exited(code),
            (None, Some(sig)) => Outcome::Signaled(sig),
            (None, None) => Outcome::Exited(1), // stopped/continued: not reachable via wait()
        }
    }
}

/// Signals that count as cancellation, so the final report is `idle` (spec:
/// *States*): SIGINT, plus SIGTERM and SIGHUP, whether forwarded by us or sent
/// to the child directly. See NOTES.md.
fn is_cancel(sig: i32) -> bool {
    matches!(
        Signal::try_from(sig),
        Ok(Signal::SIGINT | Signal::SIGTERM | Signal::SIGHUP)
    )
}

fn sig_name(sig: i32) -> String {
    Signal::try_from(sig)
        .map(|s| s.as_str().to_owned())
        .unwrap_or_else(|_| format!("signal {sig}"))
}

/// The basename of `program`, cut down to the `app` character set.
fn default_app(program: &str) -> Option<App> {
    let base = program.rsplit('/').next().unwrap_or(program);
    let clean: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'))
        .take(bellwether::limits::MAX_APP)
        .collect();
    App::new(&clean).ok()
}

/// The command line, with arguments that need quoting single-quoted, cut to
/// [`DEFAULT_MSG_CHARS`].
fn default_msg(command: &[String]) -> String {
    let line = command
        .iter()
        .map(|a| {
            let plain = !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
            if plain {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if line.chars().count() > DEFAULT_MSG_CHARS {
        let cut: String = line.chars().take(DEFAULT_MSG_CHARS - 1).collect();
        format!("{cut}…")
    } else {
        line
    }
}

/// `850ms`, `4.2s`, `12s`, `3m05s`, `1h02m`.
fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if d < Duration::from_secs(1) {
        format!("{}ms", d.as_millis())
    } else if secs < 10 {
        format!("{:.1}s", d.as_secs_f64())
    } else if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Id {
        Id::new(s).unwrap()
    }

    #[test]
    fn ids_without_nesting() {
        let r = resolve_ids(None, Some(&App::new("make").unwrap()), None);
        assert_eq!((r.record, r.export.as_str(), r.report), (None, "", true));
        let r = resolve_ids(Some(id("build")), None, None);
        assert_eq!((r.record, r.export.as_str()), (Some(id("build")), "build"));
    }

    #[test]
    fn ids_when_nested() {
        let app = App::new("restic").unwrap();
        let env = |s: &str| Some(OsString::from(s));
        // Parent on the root record.
        let r = resolve_ids(None, Some(&app), env(""));
        assert_eq!(
            (r.record, r.export.as_str()),
            (Some(id("restic")), "restic")
        );
        // Parent on a record: the app name, else --id, else "bellwether".
        let r = resolve_ids(None, Some(&app), env("deploy"));
        assert_eq!(r.record, Some(id("deploy/restic")));
        let r = resolve_ids(Some(id("a/b")), Some(&app), env("deploy"));
        assert_eq!(r.record, Some(id("deploy/a/b")));
        let r = resolve_ids(None, None, env("deploy"));
        assert_eq!(r.record, Some(id("deploy/bellwether")));
        // Too deep: reporting off, and nested runs still stay under the parent.
        let deep = "a/b/c/d/e/f/g/h";
        let r = resolve_ids(None, Some(&app), env(deep));
        assert!(!r.report);
        assert_eq!(r.export, deep);
        assert!(r.warning.is_some());
        // Garbage in the variable is ignored with a warning.
        let r = resolve_ids(None, Some(&app), env("bad id!"));
        assert_eq!((r.record, r.report), (None, true));
        assert!(r.warning.unwrap().contains(PARENT_ENV));
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(Duration::from_millis(850)), "850ms");
        assert_eq!(fmt_duration(Duration::from_millis(4249)), "4.2s");
        assert_eq!(fmt_duration(Duration::from_secs(12)), "12s");
        assert_eq!(fmt_duration(Duration::from_secs(185)), "3m05s");
        assert_eq!(fmt_duration(Duration::from_secs(3720)), "1h02m");
    }

    #[test]
    fn app_from_program() {
        assert_eq!(default_app("/usr/bin/restic").unwrap().as_str(), "restic");
        assert_eq!(default_app("my tool!").unwrap().as_str(), "mytool");
        assert_eq!(default_app("日本").map(|a| a.as_str().to_owned()), None);
        assert_eq!(default_app(&"x".repeat(40)).unwrap().as_str().len(), 32);
    }

    #[test]
    fn msg_from_command() {
        let cmd = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(default_msg(&cmd(&["sleep", "5"])), "sleep 5");
        assert_eq!(
            default_msg(&cmd(&["sh", "-c", "echo 'hi' there"])),
            r#"sh -c 'echo '\''hi'\'' there'"#
        );
        let long = default_msg(&cmd(&["echo", &"a".repeat(300)]));
        assert_eq!(long.chars().count(), DEFAULT_MSG_CHARS);
        assert!(long.ends_with('…'));
    }
}
