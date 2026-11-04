//! End-to-end tests of the `bellwether` binary. None of them need a tty:
//! reports go to stdout with `--stdout`.

use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;

const BIN: &str = env!("CARGO_BIN_EXE_bellwether");

fn cmd(args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args).env_remove("BELLWETHER");
    c
}

fn output(args: &[&str]) -> Output {
    cmd(args).output().unwrap()
}

/// Runs `bellwether decode` on `input` and returns its lines.
fn decode(input: &[u8]) -> Vec<String> {
    let mut child = cmd(&["decode"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A fresh scratch directory for one test.
fn scratch() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bellwether-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out waiting for {path:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Waits for `child` with a deadline, then returns its status and stdout.
fn finish(mut child: Child) -> (ExitStatus, Vec<u8>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("wrapper did not exit");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    (status, out)
}

// ── emit ──────────────────────────────────────────────────────────────

#[test]
fn emit_decode_round_trips_every_state() {
    let cases: &[(&[&str], &str)] = &[
        (&["idle"], r#"{"state":"idle"}"#),
        (
            &[
                "working",
                "--app",
                "cargo",
                "--msg",
                "Compiling",
                "--progress",
                "40",
            ],
            r#"{"state":"working","app":"cargo","progress":40,"msg":"Compiling"}"#,
        ),
        (
            &["done", "--title", "Build ✓", "--msg", "Built in 3s"],
            r#"{"state":"done","title":"Build ✓","msg":"Built in 3s"}"#,
        ),
        (
            &[
                "blocked",
                "--kind",
                "permission",
                "--id",
                "deploy/eu-west",
                "--progress",
                "0",
            ],
            r#"{"state":"blocked","kind":"permission","id":"deploy/eu-west","progress":0}"#,
        ),
        (
            &["blocked", "--kind", "question"],
            r#"{"state":"blocked","kind":"question"}"#,
        ),
        (
            &["blocked", "--kind", "auth"],
            r#"{"state":"blocked","kind":"auth"}"#,
        ),
        (
            &["error", "--msg", "rsync failed"],
            r#"{"state":"error","msg":"rsync failed"}"#,
        ),
        (
            &["clear", "--id", "us-east"],
            r#"{"state":"clear","id":"us-east"}"#,
        ),
        (&["clear"], r#"{"state":"clear"}"#),
    ];
    for (args, expected) in cases {
        let mut full = vec!["emit"];
        full.extend_from_slice(args);
        full.push("--stdout");
        let out = output(&full);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.starts_with(b"\x1b]7501;") && out.stdout.ends_with(b"\x1b\\"));
        assert_eq!(decode(&out.stdout), [*expected], "{args:?}");
    }
}

#[test]
fn emit_rejects_invalid_input_with_status_2() {
    let long_msg = "a".repeat(2049);
    let long_title = "a".repeat(193);
    let long_app = "a".repeat(33);
    let deep_id = ["a"; 9].join("/");
    let cases: Vec<Vec<&str>> = vec![
        vec!["sleeping"],
        vec!["working", "--kind", "auth"],
        vec!["blocked", "--kind", "nope"],
        vec!["done", "--progress", "50"],
        vec!["working", "--progress", "101"],
        vec!["done", "--msg", "two\nlines"],
        vec!["done", "--msg", &long_msg],
        vec!["done", "--title", "tab\there"],
        vec!["done", "--title", &long_title],
        vec!["done", "--app", &long_app],
        vec!["done", "--app", "has space"],
        vec!["done", "--id", "a//b"],
        vec!["done", "--id", &deep_id],
    ];
    for args in cases {
        let mut full = vec!["emit"];
        full.extend_from_slice(&args);
        full.push("--stdout");
        let out = output(&full);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?} must not emit anything");
        assert!(!out.stderr.is_empty(), "{args:?} should explain the error");
    }
}

#[test]
fn emit_accepts_values_exactly_at_the_limits() {
    let msg = "é".repeat(1024); // 2048 bytes
    let title = "a".repeat(192);
    let app = "a".repeat(32);
    let id = vec!["a".repeat(32); 3].join("/") + "/" + &"b".repeat(29); // 128 bytes
    let out = output(&[
        "emit",
        "working",
        "--msg",
        &msg,
        "--title",
        &title,
        "--app",
        &app,
        "--id",
        &id,
        "--progress",
        "100",
        "--stdout",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = decode(&out.stdout);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains(&msg) && lines[0].contains(&id));
}

#[test]
fn decode_spec_example() {
    assert_eq!(
        decode(b"\x1b]7501;state=working:msg=SGk=\x1b\\"),
        [r#"{"state":"working","msg":"Hi"}"#]
    );
}

#[test]
fn decode_reports_discarded_sequences_and_exits_0() {
    let lines = decode(b"text\x1b]7501;state=bogus\x07\x1b]7501;state=done:msg=YQpi\x07plain");
    assert_eq!(
        lines,
        [
            r#"{"error":"unknown state \"bogus\"","body":"state=bogus"}"#,
            r#"{"error":"msg: decoded text contains a control character","body":"state=done:msg=YQpi"}"#,
        ]
    );
    assert!(decode(b"no sequences at all").is_empty());
}

// ── run ───────────────────────────────────────────────────────────────

fn run_sh(script: &str, extra: &[&str]) -> (ExitStatus, Vec<String>) {
    let mut args = vec!["run", "--stdout"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--", "sh", "-c", script]);
    let out = output(&args);
    (out.status, decode(&out.stdout))
}

#[test]
fn run_propagates_exit_codes_and_reports_in_order() {
    for code in [0, 1, 3, 42, 255] {
        let (status, lines) = run_sh(&format!("exit {code}"), &[]);
        assert_eq!(status.code(), Some(code));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(
            lines[0],
            format!(r#"{{"state":"working","app":"sh","msg":"sh -c 'exit {code}'"}}"#)
        );
        if code == 0 {
            assert!(lines[1].starts_with(r#"{"state":"done","app":"sh","msg":"Finished in "#));
        } else {
            let prefix = format!(r#"{{"state":"error","app":"sh","msg":"Exited {code} after "#);
            assert!(lines[1].starts_with(&prefix), "{}", lines[1]);
        }
    }
}

#[test]
fn run_custom_flags() {
    let flags = [
        "--app",
        "demo",
        "--id",
        "job/1",
        "--msg",
        "Napping",
        "--done-msg",
        "Rested",
        "--error-msg",
        "Nightmare",
    ];
    let (status, lines) = run_sh("true", &flags);
    assert!(status.success());
    assert_eq!(
        lines,
        [
            r#"{"state":"working","app":"demo","id":"job/1","msg":"Napping"}"#,
            r#"{"state":"done","app":"demo","id":"job/1","msg":"Rested"}"#,
        ]
    );
    let (status, lines) = run_sh("false", &flags);
    assert_eq!(status.code(), Some(1));
    assert_eq!(
        lines[1],
        r#"{"state":"error","app":"demo","id":"job/1","msg":"Nightmare"}"#
    );
}

#[test]
fn run_rejects_invalid_messages_before_running() {
    let dir = scratch();
    let marker = dir.join("ran");
    let script = format!("touch {}", marker.display());
    let out = output(&[
        "run", "--stdout", "--msg", "a\nb", "--", "sh", "-c", &script,
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!marker.exists(), "the command must not run");
}

#[test]
fn run_missing_command_exits_127() {
    let out = output(&["run", "--stdout", "--", "/nonexistent/definitely-not-here"]);
    assert_eq!(out.status.code(), Some(127));
    let lines = decode(&out.stdout);
    assert_eq!(lines.len(), 2);
    assert!(lines[1].contains(r#""state":"error""#) && lines[1].contains("Failed to start"));
}

#[test]
fn run_child_killed_by_sigterm_reports_idle_and_wrapper_dies_by_sigterm() {
    let (status, lines) = run_sh("kill -TERM $$", &[]);
    assert_eq!(status.signal(), Some(Signal::SIGTERM as i32), "{status:?}");
    assert_eq!(status.code(), None, "a real signal death, not exit 128+N");
    assert_eq!(lines.len(), 2);
    assert!(
        lines[1].starts_with(r#"{"state":"idle","app":"sh","msg":"Interrupted by SIGTERM after "#),
        "{}",
        lines[1]
    );
}

#[test]
fn run_child_killed_by_other_signal_reports_error_naming_it() {
    let (status, lines) = run_sh("kill -KILL $$", &[]);
    assert_eq!(status.signal(), Some(Signal::SIGKILL as i32));
    assert!(
        lines[1].starts_with(r#"{"state":"error","app":"sh","msg":"Killed by SIGKILL after "#),
        "{}",
        lines[1]
    );
}

/// SIGINT sent to the whole process group (as the tty driver does for Ctrl-C)
/// must reach the child exactly once: the wrapper must not forward it.
#[test]
fn sigint_to_process_group_reaches_child_exactly_once() {
    let dir = scratch();
    let (count, ready, stop) = (dir.join("count"), dir.join("ready"), dir.join("stop"));
    // Counts SIGINTs. `sh` traps can't be used: the shell's C handler only
    // sets a flag, so two SIGINTs before the trap runs count as one. Perl's
    // deferred signals keep a per-signal pending count and run the handler once
    // for each delivery.
    let script = r#"
        my $n = 0;
        $SIG{INT} = sub {
            $n++;
            open my $f, '>', "$ENV{COUNT}.tmp" or die; print $f $n; close $f;
            rename "$ENV{COUNT}.tmp", $ENV{COUNT};
        };
        open my $r, '>', $ENV{READY} or die; close $r;
        select(undef, undef, undef, 0.01) until -e $ENV{STOP};
        exit 0;
    "#;
    let child = cmd(&["run", "--stdout", "--", "perl", "-e", script])
        .env("COUNT", &count)
        .env("READY", &ready)
        .env("STOP", &stop)
        .process_group(0)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let pgid = Pid::from_raw(child.id() as i32);
    wait_for_file(&ready);

    killpg(pgid, Signal::SIGINT).unwrap();
    wait_for_file(&count);
    // Leave time for a forwarded duplicate to arrive and be counted.
    thread::sleep(Duration::from_millis(500));
    std::fs::write(&stop, "").unwrap();

    let (status, out) = finish(child);
    assert_eq!(std::fs::read_to_string(&count).unwrap().trim(), "1");
    assert!(status.success(), "wrapper survived SIGINT: {status:?}");
    let lines = decode(&out);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].contains(r#""state":"done""#));
}

/// SIGTERM and SIGHUP sent to the wrapper's pid alone are forwarded to the
/// child.
#[test]
fn sigterm_and_sighup_to_wrapper_reach_child() {
    for sig in [Signal::SIGTERM, Signal::SIGHUP] {
        let dir = scratch();
        let pidfile = dir.join("pid");
        let child = cmd(&[
            "run",
            "--stdout",
            "--",
            "sh",
            "-c",
            r#"echo $$ > "$PIDFILE"; exec sleep 30"#,
        ])
        .env("PIDFILE", &pidfile)
        .process_group(0)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
        wait_for_file(&pidfile);
        // Wait until the pid file is written and `exec sleep` has happened.
        thread::sleep(Duration::from_millis(100));
        kill(Pid::from_raw(child.id() as i32), sig).unwrap();

        let (status, out) = finish(child);
        assert_eq!(status.signal(), Some(sig as i32), "{sig}: {status:?}");
        let lines = decode(&out);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[1].starts_with(&format!(
                r#"{{"state":"idle","app":"sh","msg":"Interrupted by {} after "#,
                sig.as_str()
            )),
            "{}",
            lines[1]
        );
    }
}

// ── output control ────────────────────────────────────────────────────

#[test]
fn bellwether_0_disables_all_output() {
    let out = output(&["emit", "working", "--msg", "hi", "--stdout"]);
    assert!(!out.stdout.is_empty(), "sanity check");
    for args in [
        &["emit", "working", "--msg", "hi", "--stdout"][..],
        &["run", "--stdout", "--", "true"][..],
    ] {
        let out = cmd(args).env("BELLWETHER", "0").output().unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn without_a_tty_emit_is_silent_and_probe_exits_3() {
    if bellwether::tty::Tty::open().unwrap().is_some() {
        eprintln!("skipped: this test process has a controlling tty");
        return;
    }
    let out = output(&["emit", "working", "--msg", "hi"]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    let out = output(&["probe", "--json"]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        r#"{"supported":false,"error":"no controlling terminal"}"#
    );
}

// ── probe (needs a real terminal) ─────────────────────────────────────

/// Run in a terminal with `cargo test -p bellwether-cli -- --ignored probe`.
/// This briefly puts the terminal in raw mode.
#[test]
#[ignore = "needs a controlling tty"]
fn probe_answers_without_hanging() {
    let start = Instant::now();
    let out = output(&["probe", "--timeout", "500ms", "--json"]);
    assert!(start.elapsed() < Duration::from_secs(3));
    let text = String::from_utf8_lossy(&out.stdout);
    match out.status.code() {
        Some(0) => assert!(text.starts_with(r#"{"supported":true,"states":["#)),
        Some(1) => assert_eq!(text.trim(), r#"{"supported":false}"#),
        other => panic!("unexpected {other:?}: {text}"),
    }
}
