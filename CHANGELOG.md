# Changelog

All notable changes to bellwether (the `bellwether` library and the
`bellwether-cli` binary, which share a version) are listed here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2](https://github.com/bscott/bellwether/compare/v0.1.1...v0.1.2) - 2026-11-11

### Added

- feat/presets title status ([#2](https://github.com/bscott/bellwether/pull/2))

### Other

- automate releases with release-plz

## [0.1.1](https://github.com/bscott/bellwether/compare/v0.1.0...v0.1.1) - 2026-11-06

### Added

- Static musl builds for Linux (aarch64 and x86_64), which run on systems older
  than the glibc build's 2.34 floor. Release builds require Rust 1.95 or later,
  whose bundled musl is patched for CVE-2025-26519, CVE-2026-6042 and
  CVE-2026-40200.
- Library examples: `report_status` and `track_status`.
- crates.io publishing from the release workflow, with trusted publishing.

### Changed

- Release archives are `.tar.gz`, so the install script works without `xz`.
- Each crate now ships the README and both license files; docs.rs documents
  all features.

## [0.1.0](https://github.com/bscott/bellwether/releases/tag/v0.1.0) - 2026-11-06

### Added

- `bellwether` library: typed OSC 7501 reports, the spec's parsing rules, a
  stream `Scanner`, a receiver-side `RecordStore`, feature detection, and an
  optional `tty` module.
- `bellwether` CLI: `emit`, `run` (plain and `--pty` watch mode with presets),
  `probe` and `decode`; nested runs report on child records.
- Release binaries for macOS and Linux, a shell installer and a Homebrew tap.
