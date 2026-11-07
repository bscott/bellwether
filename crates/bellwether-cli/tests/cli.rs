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
    // Keep the developer's environment out: a surrounding `bellwether run`,
    // or their own presets file.
    c.args(args)
        .env_remove("BELLWETHER")
        .env_remove("BELLWETHER_PARENT_ID")
        .env(
            "XDG_CONFIG_HOME",
            std::env::temp_dir().join("bellwether-test-no-config"),
        );
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
fn run_title_goes_with_every_report() {
    let (status, lines) = run_sh(
        "true",
        &["--app", "demo", "--title", "Nightly", "--msg", "Napping"],
    );
    assert!(status.success());
    assert_eq!(
        lines[0],
        r#"{"state":"working","app":"demo","title":"Nightly","msg":"Napping"}"#
    );
    assert!(
        lines[1].starts_with(r#"{"state":"done","app":"demo","title":"Nightly","msg":"#),
        "{}",
        lines[1]
    );
    // Too long: rejected before the command runs.
    let long = "t".repeat(193);
    let out = output(&["run", "--stdout", "--title", &long, "--", "true"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
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
    sigint_exactly_once(&[]);
}

/// With `--pty` the command is in its own session, so a SIGINT sent to the
/// wrapper's process group doesn't reach it directly. The wrapper forwards it,
/// once.
#[test]
fn pty_sigint_to_process_group_reaches_child_exactly_once() {
    sigint_exactly_once(&["--pty"]);
}

fn sigint_exactly_once(extra: &[&str]) {
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
    let mut args = vec!["run", "--stdout"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--", "perl", "-e", script]);
    let child = cmd(&args)
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

// ── run --pty ─────────────────────────────────────────────────────────

/// A terminal for the wrapper to run in: the test holds the PTY's master side
/// and plays the user, so keystrokes, Ctrl-C, resizes and termios restore can
/// be checked without a real terminal.
struct FakeTerm {
    master: std::sync::Arc<std::os::fd::OwnedFd>,
    /// Kept open so termios can be read after the wrapper exits.
    secondary: std::os::fd::OwnedFd,
    out: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl FakeTerm {
    fn new(rows: u16, cols: u16) -> FakeTerm {
        use rustix::fs::{Mode, OFlags};
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let name = ptsname(&master, Vec::new()).unwrap();
        let secondary = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY,
            Mode::empty(),
        )
        .unwrap();
        let term = FakeTerm {
            master: std::sync::Arc::new(master),
            secondary,
            out: Default::default(),
        };
        term.resize(rows, cols);
        let (master, out) = (term.master.clone(), term.out.clone());
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = rustix::io::read(&*master, &mut buf) {
                if n == 0 {
                    break;
                }
                out.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        term
    }

    fn resize(&self, rows: u16, cols: u16) {
        let ws = rustix::termios::Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        rustix::termios::tcsetwinsize(&self.secondary, ws).unwrap();
    }

    /// Starts `bellwether <args>` with this terminal as stdin, stdout and stderr.
    fn spawn(&self, args: &[&str]) -> Child {
        let io = || Stdio::from(self.secondary.try_clone().unwrap());
        cmd(args)
            .stdin(io())
            .stdout(io())
            .stderr(io())
            .process_group(0)
            .spawn()
            .unwrap()
    }

    fn type_keys(&self, bytes: &[u8]) {
        rustix::io::write(&*self.master, bytes).unwrap();
    }

    fn output(&self) -> Vec<u8> {
        self.out.lock().unwrap().clone()
    }

    fn wait_for(&self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let out = self.output();
            if out.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {needle:?} in {:?}",
                String::from_utf8_lossy(&out)
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn termios(&self) -> rustix::termios::Termios {
        rustix::termios::tcgetattr(&self.secondary).unwrap()
    }
}

/// Waits for the wrapper, then for the reader thread to catch the last
/// output.
fn finish_in(term: &FakeTerm, mut child: Child) -> (ExitStatus, Vec<u8>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!(
                "wrapper did not exit; output: {:?}",
                String::from_utf8_lossy(&term.output())
            );
        }
        thread::sleep(Duration::from_millis(10));
    };
    // The wrapper's last write is in the PTY by now. Wait until the reader
    // thread has seen output stop growing, rather than a fixed time.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut seen = term.output().len();
    loop {
        thread::sleep(Duration::from_millis(100));
        let now = term.output().len();
        if now == seen || Instant::now() > deadline {
            break;
        }
        seen = now;
    }
    (status, term.output())
}

fn assert_termios_restored(before: &rustix::termios::Termios, after: &rustix::termios::Termios) {
    // macOS sets PENDIN on return to canonical mode with input pending.
    let pendin = rustix::termios::LocalModes::PENDIN;
    assert_eq!(before.local_modes - pendin, after.local_modes - pendin);
    assert_eq!(before.input_modes, after.input_modes);
    assert_eq!(before.output_modes, after.output_modes);
    assert_eq!(before.control_modes, after.control_modes);
}

fn write_script(dir: &Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_str().unwrap().to_owned()
}

/// Keys typed at the terminal reach the command through the PTY, the restic
/// preset turns its output into progress and a `blocked` prompt, and termios
/// is restored afterwards.
#[test]
fn pty_watches_progress_and_password_prompt() {
    let dir = scratch();
    let restic = write_script(
        &dir,
        "restic",
        r#"
printf '[0:01] 10.00%%  1 files\r'; sleep 0.3
printf '[0:02] 55.50%%  5 files\r'; sleep 0.3
printf '\nenter password for repository: '
stty -echo; read pw; stty echo; echo
echo "got $pw"
"#,
    );
    let term = FakeTerm::new(24, 80);
    let before = term.termios();
    let child = term.spawn(&[
        "run",
        "--pty",
        "--stdout",
        "--msg",
        "Backing up",
        "--",
        &restic,
    ]);
    term.wait_for("enter password for repository: ");
    // Typed keys go through raw: the command, not the wrapper, sees them.
    thread::sleep(Duration::from_millis(400));
    term.type_keys(b"hunter2\r");
    let (status, out) = finish_in(&term, child);
    assert!(status.success(), "{status:?}");
    assert!(String::from_utf8_lossy(&out).contains("got hunter2"));
    assert_termios_restored(&before, &term.termios());

    let lines = decode(&out);
    let w = r#"{"state":"working","app":"restic""#;
    assert_eq!(lines[0], format!(r#"{w},"msg":"Backing up"}}"#));
    assert_eq!(
        lines[1],
        format!(r#"{w},"progress":10,"msg":"Backing up"}}"#)
    );
    assert_eq!(
        lines[2],
        format!(r#"{w},"progress":55,"msg":"Backing up"}}"#)
    );
    assert_eq!(
        lines[3],
        r#"{"state":"blocked","kind":"auth","app":"restic","progress":55,"msg":"enter password for repository:"}"#
    );
    assert_eq!(
        lines[4],
        format!(r#"{w},"progress":55,"msg":"Backing up"}}"#)
    );
    assert!(
        lines[5].starts_with(r#"{"state":"done","app":"restic""#),
        "{lines:?}"
    );
    assert_eq!(lines.len(), 6, "{lines:?}");
}

/// Ctrl-C typed at the terminal is a byte in raw mode. It interrupts the
/// command through the PTY, and the wrapper reports idle and dies the same way.
#[test]
fn pty_typed_ctrl_c_interrupts_command() {
    let term = FakeTerm::new(24, 80);
    let before = term.termios();
    let child = term.spawn(&[
        "run",
        "--pty",
        "--stdout",
        "--",
        "sh",
        "-c",
        "echo ready; sleep 30",
    ]);
    term.wait_for("ready");
    term.type_keys(b"\x03");
    let (status, out) = finish_in(&term, child);
    assert_eq!(status.signal(), Some(Signal::SIGINT as i32), "{status:?}");
    assert_termios_restored(&before, &term.termios());
    let lines = decode(&out);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].contains(r#""state":"idle""#));
    assert!(lines[1].contains("Interrupted by SIGINT"));
}

/// SIGWINCH copies the terminal's new size to the PTY. SIGTERM to the wrapper
/// reaches the command.
#[test]
fn pty_resize_and_sigterm() {
    let term = FakeTerm::new(30, 100);
    let script = r#"
trap 'echo "size=$(stty size)"' WINCH
echo "size=$(stty size)"
i=0; while [ $i -lt 300 ]; do sleep 0.05; i=$((i+1)); done
"#;
    let child = term.spawn(&["run", "--pty", "--stdout", "--", "sh", "-c", script]);
    term.wait_for("size=30 100");
    term.resize(40, 120);
    kill(Pid::from_raw(child.id() as i32), Signal::SIGWINCH).unwrap();
    term.wait_for("size=40 120");
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    let (status, out) = finish_in(&term, child);
    assert_eq!(status.signal(), Some(Signal::SIGTERM as i32), "{status:?}");
    let lines = decode(&out);
    assert!(
        lines.last().unwrap().contains(r#""state":"idle""#),
        "{lines:?}"
    );
}

/// A 7501-aware command reporting on the root record pushes the wrapper's
/// record to `bellwether`, and both final reports survive.
#[test]
fn pty_moves_off_the_record_the_command_reports_on() {
    let script = format!(
        "{BIN} emit working --stdout --app kid --msg mine; sleep 0.3; \
         {BIN} emit done --stdout --app kid --msg kid-done"
    );
    let out = output(&[
        "run", "--pty", "--stdout", "--app", "wrap", "--msg", "m", "--", "sh", "-c", &script,
    ]);
    assert!(out.status.success());
    let lines = decode(&out.stdout);
    assert_eq!(lines[0], r#"{"state":"working","app":"wrap","msg":"m"}"#);
    assert_eq!(lines[1], r#"{"state":"working","app":"kid","msg":"mine"}"#);
    assert_eq!(
        lines[2],
        r#"{"state":"working","app":"wrap","id":"bellwether","msg":"m"}"#
    );
    assert_eq!(lines[3], r#"{"state":"done","app":"kid","msg":"kid-done"}"#);
    assert!(lines[4].starts_with(r#"{"state":"done","app":"wrap","id":"bellwether""#));
    assert_eq!(lines.len(), 5, "{lines:?}");

    // Reports on other records are left alone.
    let script = format!("{BIN} emit working --stdout --id elsewhere");
    let out = output(&[
        "run", "--pty", "--stdout", "--app", "wrap", "--", "sh", "-c", &script,
    ]);
    let lines = decode(&out.stdout);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains(r#""id":"bellwether""#)));
}

/// `BELLWETHER_PARENT_ID` makes nested runs report on child records.
#[test]
fn nested_runs_report_on_child_records() {
    let inner = format!("echo \"parent=[$BELLWETHER_PARENT_ID]\"; {BIN} run --stdout -- true");
    for mode in [&[][..], &["--pty"][..]] {
        let mut args = vec!["run", "--stdout", "--app", "outer"];
        args.extend_from_slice(mode);
        args.extend_from_slice(&["--", "sh", "-c", &inner]);
        let out = output(&args);
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("parent=[]"), "{text}");
        let lines = decode(&out.stdout);
        let ids: Vec<bool> = lines.iter().map(|l| l.contains(r#""id":"true""#)).collect();
        assert_eq!(ids, [false, true, true, false], "{mode:?} {lines:?}");
    }
    // Under a parent record, `--id` is relative to it.
    let out = cmd(&["run", "--stdout", "--id", "eu", "--", "true"])
        .env("BELLWETHER_PARENT_ID", "deploy")
        .output()
        .unwrap();
    let lines = decode(&out.stdout);
    assert!(
        lines.iter().all(|l| l.contains(r#""id":"deploy/eu""#)),
        "{lines:?}"
    );
}

/// Presets from the user's file: a question prompt answered from piped
/// input, and progress with a custom pattern.
#[test]
fn pty_user_presets() {
    let dir = scratch();
    std::fs::create_dir_all(dir.join("bellwether")).unwrap();
    std::fs::write(
        dir.join("bellwether/presets.toml"),
        r#"
[deployer]
progress = 'step [0-9]+ \(([0-9]+)%\)'
blocked = [{ pattern = '\[y/N\]\s*$', kind = "question", msg = "Confirm deploy?" }]
"#,
    )
    .unwrap();
    let deployer = write_script(
        &dir,
        "deployer",
        "echo 'step 1 (42%)'; printf 'Continue? [y/N] '; read a; echo \"answer=$a\"",
    );
    let pipeline = format!("(sleep 1; echo y) | {BIN} run --pty --stdout -- {deployer}");
    let out = Command::new("sh")
        .args(["-c", &pipeline])
        .env_remove("BELLWETHER_PARENT_ID")
        .env("XDG_CONFIG_HOME", &dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("answer=y"));
    let lines = decode(&out.stdout);
    let has = |s: &str| lines.iter().any(|l| l.contains(s));
    assert!(has(r#""progress":42"#), "{lines:?}");
    assert!(
        has(
            r#"{"state":"blocked","kind":"question","app":"deployer","progress":42,"msg":"Confirm deploy?"}"#
        ),
        "{lines:?}"
    );
    assert!(lines.last().unwrap().contains(r#""state":"done""#));
}

#[test]
fn pty_bad_presets_file_exits_2_without_running() {
    let dir = scratch();
    std::fs::create_dir_all(dir.join("bellwether")).unwrap();
    std::fs::write(dir.join("bellwether/presets.toml"), "[x]\nprogress = '('\n").unwrap();
    let marker = dir.join("ran");
    let out = cmd(&[
        "run",
        "--pty",
        "--stdout",
        "--",
        "touch",
        marker.to_str().unwrap(),
    ])
    .env("XDG_CONFIG_HOME", &dir)
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("presets.toml: [x].progress: invalid regex"),
        "{err}"
    );
    assert!(out.stdout.is_empty());
    assert!(!marker.exists());

    let out = output(&["run", "--pty", "--preset", "nope", "--", "true"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("available: restic, git, apt, dnf, rsync, cargo, sudo, ssh")
    );
    // --preset only makes sense with --pty.
    assert_eq!(
        output(&["run", "--preset", "restic", "--", "true"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn pty_exit_codes_and_quiet_mode() {
    let out = output(&[
        "run",
        "--pty",
        "--stdout",
        "--",
        "sh",
        "-c",
        "echo hi; exit 3",
    ]);
    assert_eq!(out.status.code(), Some(3));
    let lines = decode(&out.stdout);
    assert!(lines[1].contains(r#""state":"error""#), "{lines:?}");
    assert_eq!(
        output(&["run", "--pty", "--", "no-such-command-xyz"])
            .status
            .code(),
        Some(127)
    );

    // BELLWETHER=0: output still passes through, no reports.
    let out = cmd(&["run", "--pty", "--stdout", "--", "echo", "hello"])
        .env("BELLWETHER", "0")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, b"hello\r\n");
}

#[test]
fn pty_piped_input_without_trailing_newline_reaches_eof() {
    for input in [&b"abc"[..], b"abc\n", b"", b"a\nb"] {
        let mut child = cmd(&["run", "--pty", "--stdout", "--", "cat"])
            .env("BELLWETHER", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("cat never saw end of input for {input:?}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{input:?}");
        // The PTY turns \n into \r\n.
        let want = String::from_utf8_lossy(input).replace('\n', "\r\n");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{input:?}");
    }
}
