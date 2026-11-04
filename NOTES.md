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
