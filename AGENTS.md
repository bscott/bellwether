# AGENTS.md

Guidance for coding agents working in this repository. Read
[NOTES.md](NOTES.md) for the design and [API.md](API.md) for the library's
public surface before changing either.

## Layout

- `crates/bellwether`: the library (OSC 7501 parsing, `Scanner`,
  `RecordStore`, optional `tty` feature). `unsafe_code` is forbidden.
- `crates/bellwether-cli`: the `bellwether` binary. `unsafe` is denied
  everywhere except `src/sys.rs`, which holds the only four `unsafe` calls;
  keep it that way and give any new one a `// SAFETY:` comment.
- `crates/bellwether-cli/tests/cli.rs`: end-to-end tests, including PTY tests.
- `fuzz/`: cargo-fuzz targets `parse` and `scanner` (not a workspace member).

## Checks

CI runs these on Linux and macOS; run them before committing:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo test --workspace --all-features      # the SIGINT test needs perl
cargo test -p bellwether --no-default-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Minimum Rust: 1.85 for the library, 1.88 for the CLI
(`cargo +1.85 check -p bellwether --all-features`,
`cargo +1.88 check -p bellwether-cli`). Don't use newer language or std
features without raising `rust-version` deliberately.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org/). release-plz
reads them to pick the next version and write the changelog:

- `fix: …` and `feat: …` make a patch release while the version is 0.x.
- `feat!: …` or a `BREAKING CHANGE:` footer makes a minor release.
- `docs:`, `ci:`, `test:`, `refactor:`, `chore:` for everything else.

No AI attribution or co-author trailers in commit messages.

## Releases: what not to touch

- Don't bump versions or edit `CHANGELOG.md` entries by hand. The release PR
  opened by release-plz does both; merging it tags `vX.Y.Z`.
- Don't hand-edit `.github/workflows/release.yml`. cargo-dist generates it:
  change `dist-workspace.toml` and run `dist generate` (cargo-dist 0.34.0).
- crates.io publishing happens only from `release.yml` via trusted
  publishing (`.github/workflows/publish-crates.yml`). Never run
  `cargo publish` locally.
- Release builds need Rust 1.95 or later (`.github/build-setup.yml`) so the
  musl builds get a patched musl. Don't lower that floor.
- Don't push, tag or publish without the maintainer's go-ahead.

## Behaviour that is easy to break

These are covered by tests, but the reasons are in NOTES.md
("Platform behaviour"):

- On macOS, unread PTY output is discarded when the last secondary fd closes.
  `pty.rs` holds a non-blocking secondary fd until the child exits and the
  master is drained. Don't drop it earlier.
- SIGWINCH needs a do-nothing handler, installed before the child is spawned
  (`run.rs`). macOS discards a blocked SIGWINCH whose disposition is the
  default, so without it `sigwait` never sees resizes.
- When piped stdin ends without a trailing newline, `pty.rs` sends a second
  EOF (0x04) so the child sees end of input.
- Parsing follows the spec's rules exactly, including where it leaves things
  open; NOTES.md ("Reading the spec") records each reading. Change parsing
  only with a test that pins the new behaviour.
