# Implementation notes

Spec: Program Status Protocol (OSC 7501), rev 0.4 (2026-10-10),
<https://www.superlogical.com/rex/docs/build/program-status>, fetched 2026-11-03.

## Brief vs. spec

I found no rule in the brief that contradicts the spec. These are things the
spec says that the brief left out:

- The states list in a detection reply excludes `clear`.
- Programs MUST ignore reply pairs they don't understand.
- Terminals MUST support at least 64 records. 256 is the cap, and a terminal
  MAY choose lower limits.
- "Check every pair before touching any stored record": a report is applied
  whole or not at all.
- ST is defined as exactly `ESC \` or BEL. The 8-bit C1 forms (0x9D OSC,
  0x9C ST) are not mentioned.

## Ambiguities and the choices made

| Question | Choice |
|---|---|
| `app` longer than 32 bytes: limit breach (discard the report) or outside the charset (treat as absent)? | **Discard.** The Limits section says a report that breaks a limit is discarded whole. Wrong characters or an empty value are treated as absent. |
| Order of the malformed-pair check and the key-length check | First, a pair whose key is not `[a-z]+` or whose value has a byte outside the set is skipped as malformed. Then a well-formed key longer than 16 bytes discards the report. |
| A repeated key where an earlier value breaks a limit | Last value wins first. Limits, base64 and control-character checks then apply only to the values that won. |
| Which whitespace "whitespace around keys and values" means | ASCII space and HT. |
| Accepted `progress` syntax | 1–3 ASCII digits with a value ≤ 100. Leading zeros are accepted. `+`, `-` and fractions are treated as absent. |
| Base64 strictness | Standard alphabet, padding optional, non-canonical trailing bits rejected. Invalid UTF-8 after decoding counts as a bad value, so the report is discarded. |
| Empty `msg=` / `title=` | Present, with empty text. |
| `parse()` gets only the body, so it can't tell which terminator was used | `parse` enforces body ≤ 4088 bytes (4096 minus the shortest framing, `ESC ] 7501 ;` + BEL). `Scanner` enforces the exact 4096-byte rule on the whole sequence. |
| OSC termination other than ST (CAN, SUB, or ESC followed by anything but `\`) | The sequence is aborted, as in the VT500 parser model, and its bytes are passed through unchanged. |
| 8-bit C1 OSC/ST | Not recognised. In UTF-8 streams 0x9C and 0x9D are continuation bytes. |
| Which reports end OSC 9;4 mapping | Any report that parses and is applied, including `clear`. Discarded reports and `?` queries don't count. |
| OSC 9;4 → root record mapping | 0 removes the root record only (not its children). 1 → `working` with progress. 2 → `error`. 3 → `working` with indeterminate progress. 4 (paused) → `working` with progress. |
| Dropping `idle` on process exit or prompt | Configurable. The default keeps `idle`. |

## Constraint deviations

- **`tty` feature dependency.** The brief limits library dependencies to
  `base64`, `thiserror` and `serde`, but also puts tty/termios code in the
  library behind `tty`. That module needs a syscall layer, so `tty` pulls in
  `rustix` (optional, off by default). The core still uses only `base64` and
  `thiserror`. rustix's wrappers are safe, so the crate has
  `#![forbid(unsafe_code)]` and no unsafe module.
- **Signal-safe termios restore.** `Tty::probe` restores termios with a drop
  guard, which covers errors, early returns and panics. Raw mode clears ISIG,
  so Ctrl-C arrives as a byte and does not kill the probe. A SIGTERM/SIGHUP
  from outside during the ≤200 ms probe is left to the CLI's handlers
  (Phase 2).
- **Fuzzing.** The `fuzz/` targets type-check on stable. Running them needs
  nightly and `cargo-fuzz`, which this machine doesn't have, so they haven't
  been run yet. The proptests cover the same invariants (no panics, input
  reproduced exactly, bounded buffer).

## Observed platform behaviour

- macOS sets `PENDIN` in `c_lflag` when a tty returns to canonical mode while
  input is pending. Termios restore tests ignore that bit.

## Phase 2 (CLI) decisions

- **SIGTERM/SIGHUP deaths always report `idle`.** The brief's rules say
  "dies from a SIGTERM/SIGHUP *we forwarded*: idle; any other signal: error",
  but its own test (`sh -c 'kill -TERM $$'`, where nothing was forwarded) expects
  `idle`. I followed the test: SIGINT, SIGTERM and SIGHUP all count as
  cancellation. Any other signal reports `error` naming it.
- **`unsafe` in the CLI.** `crates/bellwether-cli/src/sys.rs` holds four
  commented `unsafe` calls (two from this phase, listed here; two more from
  Phase 3, below). Everything else in the workspace is safe, and the library
  forbids `unsafe`. The CLI crate denies `unsafe_code` and only the four
  functions in `sys.rs` opt out, so a new `unsafe` elsewhere needs an
  explicit `#[allow]` that review would see.
  1. `pre_exec` to restore the signal mask in the child. `std::process::Command`
     does **not** reset the blocked mask before `exec`, only SIGPIPE's
     disposition. Without this, the child inherits SIGINT/SIGTERM blocked: Ctrl-C
     never reaches it and `kill -TERM` is held off. Found by a smoke test that
     printed the child's mask.
  2. `signal(sig, SIG_DFL)` before re-raising, because the Rust runtime sets
     SIGPIPE to `SIG_IGN`.
- **No signal handlers.** `run` blocks INT/QUIT/TERM/HUP in every thread, and
  one thread takes them with `sigwait`, dropping INT/QUIT and forwarding
  TERM/HUP. `probe` blocks the same signals while the tty is raw, so one that
  arrives is delivered only after termios is restored.
- **pid reuse window.** nix has no `waitid(WNOWAIT)` on macOS, so a SIGTERM
  that arrives in the microseconds between `waitpid` returning and forwarding
  stopping could, in theory, hit a reused pid.
- **Exit statuses for `run` itself.** 127 when the command isn't found, 126
  when it can't be executed (shell conventions), 2 for invalid `--msg` /
  `--done-msg` / `--error-msg` (checked before the command starts).
- **The SIGINT single-delivery test uses `perl`.** `sh` traps coalesce: the
  shell's C handler only sets a flag. A mutation check (forwarding SIGINT on
  purpose) passed with an `sh` counter and fails 5/5 with the perl counter.
  perl ships on macOS and on GitHub's Ubuntu runners.

## Phase 3 (`run --pty`) decisions

- **`--id` means the wrapper's record from the start, in both modes.** The
  brief says "move the wrapper's record to `--id` (default `bellwether`)" when
  the command reports. Using `--id` only as the move target would give the same
  flag two meanings. Instead the wrapper reports on `--id` (or the root) as in
  plain `run`, and moves to the child record `bellwether` beneath it:
  `bellwether` from the root, `build/bellwether` from `--id build`.
- **Moving happens only when the command reports on the wrapper's record.**
  The command's report has then replaced the wrapper's, so nothing needs
  cleaning up, and the wrapper continues on the child record. Reports on other
  records (a nested `bellwether run`, a program using its own ids) don't move
  anything. Moving on *any* report would leave a stale `working` root record
  behind, because the spec has no way to clear the root alone (`clear` without
  `id` clears everything). The move happens at most once.
- **`BELLWETHER_PARENT_ID`.** `run` (both modes) exports its record id, or an
  empty value for the root. A `run` that finds the variable reports on
  `parent/<--id>`, else `parent/<app>`, else `parent/bellwether`. An empty
  parent means the root, so the nested record is just `<app>`. An invalid value
  is ignored with a warning. If the joined id breaks the id limits (8 levels,
  128 bytes), the nested run doesn't report at all rather than overwrite its
  parent's record. `emit` ignores the variable, since the brief scopes it to
  `run`.
- **Presets.** TOML tables, one per preset, with `commands`, `always`,
  `progress` and `blocked = [{ pattern, kind, msg }]`. Unknown keys, bad
  regexes, a progress regex without a capture group, and messages that break
  the spec are rejected with exit 2 before the command runs. The built-ins are
  embedded TOML parsed by the same loader. A user preset replaces a built-in of
  the same name. The preset is picked by `--preset` or by command basename,
  looking past wrappers such as `sudo`, `env` and `nice` (`sudo restic backup`
  gets `restic`). `always` presets apply to every command.
- **The built-in `sudo` preset is `always = true`.** sudo can prompt from deep
  inside any command (a build script, a Makefile), so matching it by command
  name would miss most prompts. Its patterns are anchored to sudo's exact
  prompts (`[sudo] password for …:` on Linux, `Password:` on macOS).
- **Prompt detection.** Prompts don't end in a newline, so prompt patterns are
  matched against the unfinished line when output pauses, and must still match
  150 ms later. More output on that line cancels the match. `blocked` ends when
  the user presses Enter or the program finishes the line. Progress patterns are
  tried at every line end (`\n`, `\r`, cursor movement) and on each pause, so
  `\r`-redrawn progress bars work. Escape sequences are stripped before
  matching.
- **Our reports never split the command's escape sequences.** A report waits
  until the output so far ends outside an escape sequence or UTF-8 character
  (at most 250 ms, then it is written anyway). Progress-only updates are
  limited to one per 200 ms.
- **SIGINT/SIGQUIT are forwarded in `--pty` mode.** Plain `run` must not
  forward them, because the terminal already sent them to the child. Under
  `--pty` the command is in its own session, so a SIGINT sent to bellwether's
  process group never reaches it otherwise. It is sent to the PTY's foreground
  process group, as if typed. Typed Ctrl-C is a byte in raw mode and becomes a
  SIGINT in the PTY. Both paths deliver exactly once, and tests cover each.
- **A third `unsafe` call: `pre_exec` for `setsid` and `TIOCSCTTY`,** so the
  command runs in its own session with the PTY as its controlling terminal.
  Both are plain syscalls, safe between `fork` and `exec`.
- **A fourth `unsafe` call: a do-nothing SIGWINCH handler.** macOS discards a
  signal whose default action is "ignore" (SIGWINCH) when it arrives with the
  default disposition, even while it is blocked and a thread waits in
  `sigwait`. Linux keeps it pending. Found by the resize test, which timed out
  on macOS. The handler never runs, because the signal stays blocked in every
  thread.
- **Piped stdin.** When stdin isn't a terminal, the PTY's ECHO is turned off
  (nobody typed the input, so it shouldn't show up in the output), and end of
  input is passed on as Ctrl-D.
- **Stdout closing** (`| head`) is treated as a terminal hangup: the command
  gets SIGHUP.
- **After the command exits,** output is copied until the PTY reports end of
  file, or has been quiet for 100 ms (1 s at most), in case a background
  process keeps it open. Keystrokes are no longer read, so they stay with the
  shell.
- **SIGINT/SIGQUIT sent by pid in plain `run` are ignored.** They can't be told
  apart from the Ctrl-C the terminal already sent to the whole process group
  (macOS has no `sigwaitinfo` to see the sender). Use SIGTERM, or signal the
  group. Documented in `run --help`.
- **Not handled: job control under `--pty`.** The command leads its own session
  and its parent (bellwether) is in another one, so its process group is
  orphaned, and the kernel discards SIGTSTP/SIGTTIN/SIGTTOU sent to it. Ctrl-Z
  therefore doesn't suspend the command (vim's Ctrl-Z does nothing), as with
  `script(1)`. Supporting it would need bellwether to stop itself and resume
  the command, as a shell does. Also not handled: stopping bellwether itself
  (`kill -STOP` from outside). When it continues, the terminal is whatever the
  shell left it in.
- **Linux.** Run in VMPal containers (Debian, Rust 1.99) on aarch64 and on
  x86_64 under Rosetta: clippy clean, 109 passed and 1 ignored on both, and the
  PTY, SIGINT and nested tests passed 15 repeats in a row on aarch64. An
  end-to-end run under `script(1)` reported progress, blocked and done through
  `/dev/tty`. Handled Linux differences: EIO instead of EOF on the PTY master,
  and CLOEXEC for `openpt` set through `fcntl`.

## Phase 4 (release) decisions

- **CI** (`.github/workflows/ci.yml`) runs on Ubuntu and macOS: `fmt --check`,
  clippy `-D warnings` with all features and with none, the full test suite,
  the library's tests without features, and rustdoc `-D warnings`.
- **Minimum Rust was wrong.** The workspace said 1.85, but the CLI uses
  let-chains, stable from 1.88. The CLI now declares 1.88; the library stays
  at 1.85. A CI job checks both. It only builds, because proptest (a
  dev-dependency) needs 1.88.
- **Releases** use cargo-dist 0.34 (`dist-workspace.toml`,
  `.github/workflows/release.yml`, generated by `dist generate`, not edited by
  hand). Pushing a `v*` tag builds aarch64 and x86_64 for macOS and Linux
  (gnu) on native runners, makes a GitHub release with a shell installer, and
  pushes the formula to `bscott/homebrew-tap`.
- **Formula name** is `bellwether` (the binary), not `bellwether-cli` (the
  package). Archives keep the package name.
- **Not published to crates.io.** The brief didn't ask for it.
- **Needed before the first release:** the `bscott/homebrew-tap` repo, and a
  `HOMEBREW_TAP_TOKEN` secret on `bscott/bellwether` that can push to it.
