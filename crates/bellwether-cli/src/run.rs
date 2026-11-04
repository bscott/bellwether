//! `bellwether run`: report `working`, run the command, report the outcome.

use std::io::ErrorKind;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitCode, ExitStatus};
use std::time::{Duration, Instant};

use bellwether::{App, Id, Report, ReportBuilder, State};
use nix::sys::signal::Signal;

use crate::RunArgs;
use crate::output::Sink;
use crate::sys::{self, Forwarder, Hold};

/// Longest default `--msg` (the command line) in characters.
const DEFAULT_MSG_CHARS: usize = 100;

pub fn run(args: RunArgs) -> ExitCode {
    // Check user-supplied text up front: never run the command and then fail to
    // report on it.
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

    let program = &args.command[0];
    let app = args.app.clone().or_else(|| default_app(program));
    // A broken /dev/tty also turns reporting off.
    let sink = Sink::open(args.output.stdout).unwrap_or(Sink::Off);
    let report = |state: State, msg: String| {
        let r = base(state, app.clone(), args.id.clone())
            .msg(msg)
            .build_lossy();
        // Reporting is best-effort. A failed write must never affect the command
        // or its exit status.
        let _ = sink.write(&r);
    };

    // Block INT/QUIT/TERM/HUP before spawning any thread, so every thread
    // inherits the mask and only the forwarder's sigwait takes them.
    let hold = Hold::new();

    report(
        State::Working,
        args.msg
            .clone()
            .unwrap_or_else(|| default_msg(&args.command)),
    );
    let start = Instant::now();

    let mut cmd = Command::new(program);
    cmd.args(&args.command[1..]);
    hold.unblock_in_child(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bellwether: {program}: {e}");
            report(State::Error, format!("Failed to start {program}: {e}"));
            // Shell conventions: 127 not found, 126 found but not runnable.
            return ExitCode::from(if e.kind() == ErrorKind::NotFound {
                127
            } else {
                126
            });
        }
    };

    let forwarder = Forwarder::start(child.id());
    let status = loop {
        match child.wait() {
            Ok(s) => break s,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                eprintln!("bellwether: waiting for {program}: {e}");
                report(State::Error, format!("Lost track of {program}: {e}"));
                return ExitCode::FAILURE;
            }
        }
    };
    forwarder.child_reaped();
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
    report(state, msg);

    match outcome {
        Outcome::Exited(code) => ExitCode::from(code as u8),
        Outcome::Signaled(sig) => match Signal::try_from(sig) {
            Ok(s) => sys::die_by_signal(s),
            Err(_) => ExitCode::from((128 + sig).clamp(0, 255) as u8),
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

fn base(state: State, app: Option<App>, id: Option<Id>) -> ReportBuilder {
    let b = Report::builder(state).maybe_id(id);
    match app {
        Some(a) => b.app(a),
        None => b,
    }
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
