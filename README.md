# bellwether

A Rust library and CLI for **OSC 7501, the Program Status Protocol**. With it, a
program tells its terminal whether it is idle, working, blocked on the user,
done, or failed, and why.

Spec (rev 0.4): <https://www.superlogical.com/rex/docs/build/program-status>

Runs on macOS and Linux. The library needs Rust 1.85 or later, the CLI 1.88.
Linux releases come in two builds. The `gnu` build needs glibc 2.34 or later
(Ubuntu 22.04, Debian 12, RHEL 9 and newer). The `musl` build is a static
binary that runs on any Linux, including older glibc systems and Alpine.

Terminals that read OSC 7501 today include [Rex](https://www.superlogical.com/rex)
and the Herdr Mac app. Others ignore the sequences, so reporting is always
safe.

## Install

```sh
brew install bscott/tap/bellwether

# or the install script (macOS and Linux)
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/bscott/bellwether/releases/latest/download/bellwether-cli-installer.sh | sh

# or with cargo
cargo install bellwether-cli
```

## Use it

**Wrap any command.** Its tab shows `working` while it runs, then `done`, or
`error` with the exit code:

```sh
bellwether run -- make test
bellwether run --app restic --msg "Nightly backup" -- restic backup ~/Documents
bellwether run --title "Nightly backup" -- ./backup.sh   # label the record
```

**Report from a script.** Each `emit` writes one report:

```sh
bellwether emit working --app deploy --msg "Deploying v2.4.1" --progress 40
bellwether emit blocked --app deploy --kind question --msg "Promote to production?"
bellwether emit done --app deploy --msg "Deployed v2.4.1"
bellwether emit clear            # remove every record
```

**Watch the output** for progress and password prompts (see
[Watch mode](#watch-mode-run---pty)):

```sh
bellwether run --pty -- restic backup ~/Documents
```

**Check the terminal**, or see what a program sends:

```sh
bellwether probe                 # exit 0 supported, 1 not, 3 no terminal
some-program | bellwether decode # one JSON line per report
```

### Details

- Reports go to `/dev/tty`, so they work when stdout is piped. With no
  controlling terminal nothing is written and the exit status is 0. `--stdout`
  writes reports to stdout instead, for pipes and tests.
- `BELLWETHER=0` turns off all report output.
- `run` reports `working`, runs the command with the terminal's stdin, stdout
  and stderr, then reports `done` (exit 0), `error` (nonzero exit, or killed by
  a signal), or `idle` (SIGINT/SIGTERM/SIGHUP). It exits with the command's
  status, and re-raises the command's fatal signal on itself.
  - Ctrl-C reaches the command once, straight from the terminal: bellwether
    doesn't forward it. So a SIGINT or SIGQUIT sent to bellwether's pid alone
    (`kill -INT <pid>`) is ignored. Signal the process group, or use SIGTERM.
    (Under `--pty` they are forwarded, since the command has its own
    terminal.)
  - SIGTERM and SIGHUP sent to bellwether are forwarded.
  - `run` never probes the terminal.
- Nested runs report beneath their parent. `run` exports
  `BELLWETHER_PARENT_ID`, and a `bellwether run` that finds it reports on
  `parent/<app>` instead of the parent's record.

### Watch mode: `run --pty`

```sh
bellwether run --pty -- restic backup ~/Documents
bellwether run --pty --preset restic -- ./nightly-backup.sh
```

The command runs in a pseudoterminal. Keys and output pass through unchanged,
and window resizes follow. Along the way, bellwether reads the output:

- a preset's **progress** pattern sets `progress` on the `working` report;
- its **prompt** patterns report `blocked` (`kind=auth` or `question`) until
  you press Enter;
- if the command sends its own OSC 7501 report for the record bellwether is
  using (Claude Code, for example), bellwether moves to the child record
  `bellwether` so the two don't overwrite each other.

Built-in presets:

| Preset | Commands | Progress | Prompts |
|---|---|---|---|
| `restic` | `restic` | status line percent | repository password |
| `git` | `git` | receiving/writing objects, deltas, files | HTTPS username and password |
| `apt` | `apt`, `apt-get`, `aptitude` | downloads, dpkg's `Progress:` bar | `[Y/n]`, config-file `[default=N] ?` |
| `dnf` | `dnf`, `dnf5`, `yum` | | `[y/N]` |
| `rsync` | `rsync` | `--progress`, `--info=progress2` | |
| `cargo` | `cargo` | `Building [==>  ] 45/120` | |
| `sudo` | every command | | password |
| `ssh` | every command | | key passphrase, PIN, password, unknown host key |

`sudo` and `ssh` are always on, because their prompts turn up inside other
commands (`git push`, `rsync host:`, build scripts). Add your own in
`~/.config/bellwether/presets.toml` (`$XDG_CONFIG_HOME` is honoured). A preset
with a built-in's name replaces it.

```toml
[deployer]
commands = ["deploy", "deployer"]   # default: the preset's name
progress = 'step [0-9]+ \(([0-9]+)%\)'   # first capture group = percent
# or count steps: progress = '(?P<done>[0-9]+)/(?P<total>[0-9]+) steps'
blocked = [
  { pattern = '\[y/N\]\s*$', kind = "question" },
  { pattern = '(?i)passphrase[^:]*:\s*$', kind = "auth", msg = "SSH key passphrase" },
]

[ssh]
always = true                        # apply to every command
blocked = [{ pattern = '(?i)password:\s*$', kind = "auth" }]
```

## Library

```toml
[dependencies]
bellwether = { version = "0.1", features = ["tty"] }
```

[crates.io](https://crates.io/crates/bellwether) · [docs.rs](https://docs.rs/bellwether)

### Use case 1: report your program's own status

A deploy tool, a backup job or a test runner tells the terminal what it is
doing, so the user can see it from another tab. `Tty` writes to `/dev/tty`, so
it works when stdout is piped to a log, and does nothing without a terminal
(cron, CI).

```rust
use bellwether::tty::Tty;
use bellwether::{App, Kind, Progress, Report, State, Terminator};

let tty = Tty::open()?; // None: no controlling terminal
let send = |r: Report| match &tty {
    Some(t) => t.write_sequence(&r.encode(Terminator::St)),
    None => Ok(()),
};
let app = App::new("deploy")?;

send(Report::builder(State::Working).app(app.clone())
    .progress(Progress::new(40).unwrap()).msg("Uploading release").build_lossy())?;
send(Report::builder(State::Blocked).app(app.clone())
    .kind(Kind::Question).msg("Promote to production? [y/N]").build_lossy())?;
send(Report::builder(State::Done).app(app).msg("Deployed v2.4.1").build_lossy())?;
```

Full program: [`examples/report_status.rs`](crates/bellwether/examples/report_status.rs)
(`cargo run -p bellwether --features tty --example report_status`).

### Use case 2: track the status of programs you run

A CI log viewer, a test harness or a multiplexer reads a child's output.
`Scanner` picks the reports out of the byte stream and passes every other byte
through unchanged. `RecordStore` keeps each record's current status, with the
spec's rules for replacing, clearing and `app` inheritance.

```rust
use bellwether::{Change, Event, Message, RecordStore, Scanner};

let mut scanner = Scanner::new();
let mut store = RecordStore::new();

scanner.feed(&child_output, |event| match event {
    Event::Bytes(b) => forward(b), // everything else, untouched
    Event::Sequence { message: Message::Report(Ok(report)), .. } => {
        for change in store.apply(report) {
            if let Change::Set(id) = change {
                let rec = store.get(id.as_ref()).unwrap();
                println!("{:?} is {}", id, rec.state().as_str());
            }
        }
    }
    _ => {} // discarded reports, queries, replies
});
```

Full program: [`examples/track_status.rs`](crates/bellwether/examples/track_status.rs):

```sh
bellwether run --stdout -- make test | cargo run -p bellwether --example track_status
```

### What's in the crate

| Piece | What it does |
|---|---|
| `Report`, `ReportBuilder` | Typed reports. `try_build` (strict) or `build_lossy`. `encode(Terminator::St \| Bel)` |
| `parse(body)` | Receiver rules from the spec: malformed pairs skipped, unknown keys ignored, last value wins, whole report discarded on bad base64, control characters, invalid id or any limit breach |
| `Scanner` | Incremental finder for OSC 7501 sequences in a byte stream. Handles split chunks, passes every other byte through unchanged, buffers at most 4097 bytes (the 4096-byte limit plus one byte of look-ahead) |
| `RecordStore` | Receiver state: replace, subtree `clear`, `app` inheritance, LRU cap of 256, lifetime on process exit / OSC 133 A / RIS, OSC 9;4 mapping rule |
| `detect` | Query bytes, reply parsing, and a pure `Detector` for the DA1 race |
| `tty` (feature) | `/dev/tty` writes with a single `write(2)`, and a raw-mode probe that always restores termios |

Features: `serde` (Serialize/Deserialize for the value types) and `tty`
(pulls in `rustix`). The core depends only on `base64` and `thiserror`. The
crate has `#![forbid(unsafe_code)]`.

## Development

```sh
cargo test --workspace --all-features    # the SIGINT test needs perl
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo +nightly fuzz run parse      # or: scanner (needs cargo-fuzz)
```

Releases are automated: release-plz keeps a release PR open with the next
version and changelog, and merging it tags the release, which builds the
binaries and publishes everything. Write commits as
[Conventional Commits](https://www.conventionalcommits.org/) (`fix: …`,
`feat: …`, `feat!: …` for breaking changes); they decide the version and the
changelog. See [CHANGELOG.md](CHANGELOG.md).

See [NOTES.md](NOTES.md) for the design: how the implementation reads the
parts the spec leaves open, and why the CLI works the way it does.

## Manual acceptance checks

These need a real terminal, so they run by hand. Ticked items were checked on
2026-11-05 with Rex 0.0.1793645657 and Herdr.app (herdr-core 0.1.18) on macOS.

- In a Rex tab, `bellwether run --app demo --msg "Napping" -- sleep 5`:
  - [x] Rex reports `program_status_changed` events: `working` "Napping", then
        `done`, with bellwether as the owner;
  - [x] a `rex do --all` Lua script with
        `rex.on("terminal.program_status_changed", …)` sees each report;
  - [ ] the unfocused tab header shows a spinner, then a done marker (needs a
        look at the app).
- [x] In the Herdr app the same command shows the terminal as `working`, then
      `done`. Herdr gets this from herdr-core, which reads OSC 7501. The
      `herdr` TUI (0.9.5) does not read OSC 7501 and leaves the pane `unknown`.
- [x] `bellwether probe` reports supported, with all five states and three
      kinds, in Rex and in Herdr.
- [ ] `bellwether probe` reports unsupported in a plain terminal without
      hanging, and typing is normal afterwards (echo on, line editing works).
      With a terminal attached, `cargo test -p bellwether-cli -- --ignored
      probe` runs the same check automatically.
- [x] `printf '\e]7501;state=working:msg=SGk=\e\\' | bellwether decode` prints
      one JSON report.
- [x] Nested runs in Rex and Herdr: `outer` on the root record, `inner` on
      the child record `inner`, both ending `done`.
- Watch mode:
  - [x] A restic-style command in Rex and in Herdr: progress 10 → 55,
        `blocked` (auth) at the password prompt, `working` after Enter, then
        100 and `done`. (Herdr refuses to type into a terminal blocked on a
        password unless forced.)
  - [ ] `bellwether run --pty -- sudo -k true` shows `blocked` (auth) while
        sudo asks for the password, then `done` (needs a real password);
  - [x] `bellwether run --pty -- vim` in Rex follows resizes (33×57 in a
        split, 213×57 zoomed, back to 33×57) and Ctrl-C shows vim's own
        "Type :qa" message. Ctrl-Z doesn't suspend under `--pty`; see NOTES.md;
  - [ ] `bellwether run --pty -- claude` keeps Claude Code's own status, and
        bellwether's shows up on the child record `bellwether`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
