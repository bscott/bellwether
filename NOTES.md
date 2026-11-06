# Design notes

How bellwether reads the parts of the spec that leave room for choice, and why
the CLI behaves the way it does.

Spec: Program Status Protocol (OSC 7501), rev 0.4 (2026-10-10),
<https://www.superlogical.com/rex/docs/build/program-status>.

## Reading the spec

### Rules that are easy to miss

- The `states` list in a detection reply never includes `clear`.
- Programs must ignore reply pairs they don't understand.
- Terminals must support at least 64 records. 256 is the cap, and a terminal
  may choose a lower one.
- "Check every pair before touching any stored record": a report is applied
  whole or not at all.
- ST is exactly `ESC \` or BEL. The 8-bit C1 forms (0x9D OSC, 0x9C ST) are not
  mentioned.

### Open questions and the answers chosen

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

## The library

- **No I/O in the core.** Encoding, parsing, scanning, the record store and
  detection are pure, and depend only on `base64` and `thiserror` (plus
  `serde` behind a feature). Terminal access lives in the optional `tty`
  module, which needs a syscall layer and so pulls in `rustix`. rustix's
  wrappers are safe, so the whole crate is `#![forbid(unsafe_code)]`.
- **`App` and `Id` are typed in the builder.** A lossy id would quietly
  retarget a report, and the spec forbids falling back to the root record for
  a bad id. Messages and titles, by contrast, have a lossy builder
  (`build_lossy`) that strips control characters and truncates on character
  boundaries.
- **`Scanner` never alters the stream.** Anything that isn't a complete,
  in-limit OSC 7501 sequence comes out as bytes, so a pass-through wrapper can
  forward everything and only act on the sequences. It buffers at most 4097
  bytes, however long a sequence runs.
- **Encoding order** is state, kind, app, id, title, progress, msg, the order
  every spec example uses, so the tests compare encoded bytes against the spec
  text exactly. Base64 output is padded, as in the examples.
- **`Tty::probe` restores termios with a drop guard,** which covers errors,
  early returns and panics. Raw mode clears ISIG, so Ctrl-C arrives as a byte
  and doesn't kill the probe midway.

## The CLI

### `run`

- **Final state.** Exit 0 reports `done`. A nonzero exit, or death by a signal
  other than INT/TERM/HUP, reports `error` with the reason. SIGINT, SIGTERM
  and SIGHUP count as cancellation and report `idle`, whoever sent them.
- **Exit status.** bellwether exits with the command's status. If the command
  died by a signal, bellwether resets that signal to its default and re-raises
  it on itself, so the shell sees the same death. Its own failures follow
  shell conventions: 127 when the command isn't found, 126 when it can't be
  executed, 2 for invalid arguments (checked before the command starts).
- **Signals without handlers.** `run` blocks INT/QUIT/TERM/HUP in every thread,
  and one thread takes them with `sigwait`. The child gets the original mask
  back before `exec`: `std::process::Command` resets SIGPIPE's disposition but
  not the blocked mask, so without this the child would start with SIGINT and
  SIGTERM blocked.
- **Ctrl-C is not forwarded.** The terminal already delivers it to the whole
  foreground process group, so forwarding would deliver it twice. The cost: a
  SIGINT or SIGQUIT sent to bellwether's pid alone (`kill -INT <pid>`) is
  ignored, because it can't be told apart from the terminal's (macOS has no
  `sigwaitinfo` to see the sender). Signal the group, or use SIGTERM. SIGTERM
  and SIGHUP are forwarded.
- **pid reuse.** nix has no `waitid(WNOWAIT)` on macOS, so a SIGTERM arriving
  in the microseconds between `waitpid` returning and forwarding stopping
  could in theory reach a reused pid.

### Nested runs

`run` exports its record id in `BELLWETHER_PARENT_ID` (empty for the root).
A `run` that finds it reports on `parent/<--id>`, else `parent/<app>`, else
`parent/bellwether`; an empty parent means the root, so the record is just
`<app>`. An invalid value is ignored with a warning. If the joined id would
break the id limits (8 levels, 128 bytes), the nested run doesn't report at
all, rather than overwrite its parent's record. `emit` ignores the variable.

### Watch mode: `run --pty`

- **`--id` means the same thing in both modes:** the record bellwether reports
  on from the start.
- **When the command reports for itself.** If the command sends a report for
  the record bellwether is using (Claude Code does this), bellwether moves to
  the child record `bellwether` beneath it (`build/bellwether` with
  `--id build`), once. The command's report has replaced bellwether's, so
  nothing is left to clean up. Reports on other records don't trigger a move.
  Moving on *any* report would leave a stale `working` root record behind,
  because the spec has no way to clear the root alone.
- **Presets** are TOML tables with `commands`, `always`, `progress` and
  `blocked = [{ pattern, kind, msg }]`. Unknown keys, bad regexes, a progress
  regex without a capture group, and messages the spec wouldn't accept are
  rejected with exit 2 before the command runs. The built-ins are embedded TOML
  read by the same loader, and a user preset with a built-in's name replaces
  it. A preset is chosen by `--preset` or by command name, looking past
  wrappers and their options (`sudo -u bob restic`, `nice -n 5 restic` and
  `env -u FOO restic` all get `restic`).
- **The `sudo` preset applies to every command,** because sudo can prompt from
  deep inside a build script or Makefile. Its patterns are anchored to sudo's
  exact prompts (`[sudo] password for …:` on Linux, `Password:` on macOS).
- **Prompt detection.** Prompts don't end in a newline, so prompt patterns are
  matched against the unfinished line when output pauses, and must still match
  150 ms later. More output on that line cancels the match. `blocked` ends
  when the user presses Enter or the program finishes the line. Progress
  patterns are tried at every line end (`\n`, `\r`, cursor movement) and on
  each pause, so `\r`-redrawn progress bars work. Escape sequences, including
  charset designations such as `ESC ( B` from `tput sgr0`, are stripped before
  matching.
- **bellwether's reports never split the command's escape sequences.** A
  report waits until the output so far ends outside an escape sequence or
  UTF-8 character (at most 250 ms, then it is written anyway).
  Progress-only updates are limited to one per 200 ms.
- **SIGINT and SIGQUIT are forwarded under `--pty`.** The command is in its own
  session there, so a signal sent to bellwether's process group would never
  reach it. It goes to the PTY's foreground process group, as if typed. Typed
  Ctrl-C is a byte in raw mode and becomes a SIGINT inside the PTY. Both paths
  deliver exactly once.
- **Piped stdin.** When stdin isn't a terminal, the PTY's echo is off (nobody
  typed the input, so it shouldn't appear in the output), and end of input is
  passed on as Ctrl-D, twice if the input ended mid-line, because one Ctrl-D
  after a partial line only flushes that line.
- **Stdout closing** (`| head`) is treated as a terminal hangup: the command
  gets SIGHUP.
- **After the command exits,** output is copied until the PTY reports end of
  file, or has been quiet for 100 ms (1 s at most), in case a background
  process keeps it open. Keystrokes are no longer read, so they stay with the
  shell.

### `unsafe`

All of it is in `crates/bellwether-cli/src/sys.rs`: four calls, each with a
comment explaining why it is sound. The CLI crate denies `unsafe_code`, and
only those four functions opt out, so a new `unsafe` elsewhere needs a visible
`#[allow]`.

1. `pre_exec` restoring the signal mask in the child (see `run` above).
2. `pre_exec` running `setsid` and `TIOCSCTTY` under `--pty`, so the command
   has the PTY as its controlling terminal.
3. `signal(sig, SIG_DFL)` before re-raising the command's signal, because the
   Rust runtime sets SIGPIPE to `SIG_IGN`.
4. A do-nothing SIGWINCH handler, installed before the command starts (see
   macOS below). It never runs, because the signal stays blocked.

## Platform behaviour

- **macOS discards SIGWINCH** when its disposition is the default ("ignore"),
  even while it is blocked and a thread waits for it in `sigwait`. Linux keeps
  it pending. Hence the do-nothing handler.
- **macOS discards unread PTY output when the last descriptor for the
  secondary side closes.** A command that writes and exits before bellwether
  reads (`echo hi` on a slow machine) would lose all its output. bellwether
  keeps its own non-blocking descriptor for the secondary side, opened
  separately so the command's stdio stays blocking, reads everything once the
  command has exited, then closes it.
- **macOS sets `PENDIN`** in `c_lflag` when a terminal returns to canonical mode
  with input pending; termios checks ignore that bit.
- **Linux returns EIO, not EOF,** from the PTY master once nothing holds the
  secondary side. macOS `openpt` doesn't accept CLOEXEC, so it is set with
  `fcntl` on both.

## Known limitations

- **No job control under `--pty`.** The command leads its own session and its
  parent (bellwether) is in another, so its process group is orphaned, and the
  kernel discards SIGTSTP/SIGTTIN/SIGTTOU sent to it. Ctrl-Z doesn't suspend
  the command (vim's Ctrl-Z does nothing), as with `script(1)`. Supporting it
  would need bellwether to stop itself and resume the command, as a shell
  does.
- **Stopping bellwether itself** (`kill -STOP`) isn't handled. When it
  continues, the terminal is in whatever state the shell left it.
- **Terminal support varies.** Rex and the Herdr Mac app (via herdr-core) read
  OSC 7501. The `herdr` TUI (0.9.5) doesn't, and leaves the pane `unknown`.

## Testing

- Unit and property tests cover the library's invariants: no panics on any
  input, the scanner reproduces its input exactly, buffers stay bounded. Fuzz
  targets in `fuzz/` cover parsing and scanning (`cargo +nightly fuzz run`).
- The CLI tests drive bellwether through a PTY the test owns, playing the
  terminal: typing, resizing, sending signals and checking that termios is
  restored afterwards.
- The SIGINT single-delivery test counts signals with `perl`, because `sh`
  traps coalesce repeated signals (the shell's C handler only sets a flag).
- CI runs fmt, clippy (all features and none), the tests and rustdoc on Ubuntu
  and macOS, and builds the library on Rust 1.85 and the CLI on 1.88.

## Releases

- **How a release happens.** release-plz (`release-plz.toml`,
  `.github/workflows/release-plz.yml`) keeps a release PR open on `main`
  that bumps the shared version and adds a `CHANGELOG.md` entry from the
  commits since the last release. Merging it makes release-plz push the
  `vX.Y.Z` tag, and the tag starts cargo-dist. release-plz itself doesn't
  publish or create GitHub releases. It decides what is unreleased by
  comparing each crate with the copy on crates.io, so a commit that changes
  neither crate's files doesn't trigger a release.
- **Commit messages decide the version.** release-plz reads Conventional
  Commits: `fix:` is a patch, `feat:` a minor change (a patch while the
  version is 0.x), and `feat!:` or a `BREAKING CHANGE:` footer is breaking (a
  minor bump while 0.x). Other messages count as patches and land under
  "Other" in the changelog. For the library, cargo-semver-checks also raises
  the bump when the public API changes incompatibly. Check the version in
  each release PR before merging.
- **The release-plz token.** Its workflow uses `RELEASE_PLZ_TOKEN`, a
  fine-grained token for this repository with Contents and Pull requests
  read and write. GitHub's built-in token can't be used: PRs and tags it
  creates don't start other workflows, so CI wouldn't run on the release PR
  and the tag wouldn't start the release.
- **cargo-dist** (`dist-workspace.toml`; `.github/workflows/release.yml` is
  generated by `dist generate`, not edited by hand). A `v*` tag builds every
  target on native runners, publishes a GitHub release with a shell
  installer, and pushes the Homebrew formula `bellwether` to
  `bscott/homebrew-tap`.
- **Linux comes in two builds.** The gnu build is made on Ubuntu 22.04 and
  needs glibc 2.34 (it fails on Debian 11 with `GLIBC_2.33 not found`). The
  musl build is static and runs on any Linux, including Debian 11 and Alpine.
- **musl version.** Every Rust channel bundles musl 1.2.5 rather than the
  newer 1.2.6, but from Rust 1.95.0 on it is patched for CVE-2025-26519
  (iconv), CVE-2026-6042 and CVE-2026-40200. The last two aren't fixed in any
  musl release yet, so this copy is ahead of plain 1.2.6.
  `.github/build-setup.yml` stops a release build on Rust older than 1.95. The
  runners install Ubuntu's `musl-tools` (musl 1.2.2) only for `musl-gcc`: the
  link uses Rust's self-contained `crt1.o` and `libc.a`, and bellwether has no
  C code. The "sshhole" bug (musl `crypt()` returning `"*"` for long
  passwords, fixed in OpenSSH 10.6p2) is in sshd; bellwether never calls
  `crypt()`.
- **crates.io publishing** runs in CI as a cargo-dist custom publish job
  (`.github/workflows/publish-crates.yml`), after the GitHub release. It uses
  crates.io trusted publishing (OIDC), so no token is stored; each crate's
  crates.io settings list this repository with workflow `release.yml`
  (crates.io checks the top-level workflow, `workflow_ref`, not the called
  one). The job skips a version that is already published, so re-running a
  release is safe.
- **Minimum Rust:** 1.85 for the library, 1.88 for the CLI (let-chains).
