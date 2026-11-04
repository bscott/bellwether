# bellwether

A Rust library and CLI for **OSC 7501, the Program Status Protocol**. With it, a
program tells its terminal whether it is idle, working, blocked on the user,
done, or failed, and why.

Spec (rev 0.4): <https://www.superlogical.com/rex/docs/build/program-status>

> Status: the library and CLI are done. `run --pty` watch mode is next.

## CLI

```sh
cargo install --path crates/bellwether-cli
```

```sh
bellwether emit working --app deploy --msg "Deploying v2.4.1"
bellwether emit blocked --kind auth --msg "Password required"
bellwether emit clear --id us-east            # or just: bellwether emit clear

bellwether run --app restic -- restic backup ~/Documents
bellwether probe                              # exit 0 supported, 1 not, 3 no tty
printf '\e]7501;state=working:msg=SGk=\e\\' | bellwether decode
```

- Reports go to `/dev/tty`, so they work when stdout is piped. With no
  controlling terminal nothing is written and the exit status is 0. `--stdout`
  writes reports to stdout instead, for pipes and tests.
- `BELLWETHER=0` turns off all report output.
- `run` reports `working`, runs the command with the terminal's stdin, stdout
  and stderr, then reports `done` (exit 0), `error` (nonzero exit, or killed by
  a signal), or `idle` (SIGINT/SIGTERM/SIGHUP). It exits with the command's
  status, and re-raises the command's fatal signal on itself.
  - Ctrl-C reaches the command once, straight from the terminal: bellwether
    doesn't forward it.
  - SIGTERM and SIGHUP sent to bellwether are forwarded.
  - `run` never probes the terminal.

## Library

```toml
[dependencies]
bellwether = "0.1"
```

| Piece | What it does |
|---|---|
| `Report`, `ReportBuilder` | Typed reports. `try_build` (strict) or `build_lossy`. `encode(Terminator::St \| Bel)` |
| `parse(body)` | Receiver rules from the spec: malformed pairs skipped, unknown keys ignored, last value wins, whole report discarded on bad base64, control characters, invalid id or any limit breach |
| `Scanner` | Incremental finder for OSC 7501 sequences in a byte stream. Handles split chunks, passes every other byte through unchanged, buffers at most 4096 bytes |
| `RecordStore` | Receiver state: replace, subtree `clear`, `app` inheritance, LRU cap of 256, lifetime on process exit / OSC 133 A / RIS, OSC 9;4 mapping rule |
| `detect` | Query bytes, reply parsing, and a pure `Detector` for the DA1 race |
| `tty` (feature) | `/dev/tty` writes with a single `write(2)`, and a raw-mode probe that always restores termios |

```rust
use bellwether::{App, Report, State, Terminator};

let bytes = Report::builder(State::Working)
    .app(App::new("deploy")?)
    .msg("Deploying v2.4.1")
    .try_build()?
    .encode(Terminator::St);
```

Features: `serde` (Serialize/Deserialize for the value types) and `tty`
(pulls in `rustix`). The core depends only on `base64` and `thiserror`. The
crate has `#![forbid(unsafe_code)]`.

## Development

```sh
cargo test --workspace --all-features    # the SIGINT test needs perl
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo +nightly fuzz run parse      # or: scanner (needs cargo-fuzz)
```

See [NOTES.md](NOTES.md) for how the implementation reads the parts the spec
leaves open.

## Manual acceptance checks

These need a real terminal, so they run by hand.

- [ ] In a Rex tab, `bellwether run --app demo --msg "Napping" -- sleep 5`:
  - [ ] shows a spinner on the unfocused tab header, then a done marker;
  - [ ] is seen by a `rex do --all` Lua script with
        `rex.on("terminal.program_status_changed", …)`.
- [ ] In the Herdr app, the same command shows the pane as working, then done.
- [ ] `bellwether probe` reports supported in Rex and Herdr, and unsupported
      in a plain terminal, without hanging. Check that typing is normal
      afterwards (echo on, line editing works). With a terminal attached,
      `cargo test -p bellwether-cli -- --ignored probe` runs the same check
      automatically.
- [ ] `printf '\e]7501;state=working:msg=SGk=\e\\' | bellwether decode` prints
      one JSON report.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
