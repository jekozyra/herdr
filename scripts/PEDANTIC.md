# Rust quality gate

`just lint` runs the existing warnings-as-errors Clippy check, then enables the
entire Clippy `pedantic` group and the `todo` and `unimplemented` restriction
lints. Clippy analyzes whole crates; `scripts/pedantic.py` compares its JSON
messages by file, lint, message, highlighted source, and occurrence count. It
ignores line-number shifts but rejects new diagnostics. Generated
`crates/ghostty-vt/src/bindings.rs` is excluded.

`pedantic-baseline.json` is the scan of upstream v0.9.5 at **2ecf19a9**, the
last release before the fork-specific Rust commits. Do **not** regenerate it
from fork HEAD: that would silently exempt the fork's code. The scanner ran
with the same pinned Rust 1.96.1 toolchain and `--all-targets --locked` flags.
`pedantic-fork-reviewed.json` records the findings still present after auditing
the fork's Rust additions; run `python3 scripts/pedantic.py --audit-fork` to see
*all* current fork-introduced findings, including those reviewed below. Do not
update that file just to pass CI. Fix new findings or document and review a
specific exception before updating its count.

The audit initially found 54 pedantic occurrences in the fork delta. We fixed
the actionable URI matching, approximate progress formatting, integer narrowing,
clone assignment, and test naming findings. The 21 remaining occurrences
(13 diagnostic identities, with repeated `--all-targets` compilation) are
reviewed exceptions:

- `src/client/mod.rs` and `src/client/shell/composition.rs`: old startup/client
  orchestration functions already far over the 100-line threshold. The fork
  changed their contents but splitting those flows is separate behavioral work.
  The `large_futures` warning is for one-shot client startup, not pane-scaled
  render or parsing; boxing that future just to change its measured size adds
  allocation without establishing a performance benefit.
- `src/client/loop_config.rs` and `src/client/shell/state.rs`: independent client
  input/presentation flags are kept in the existing configuration/state
  structures rather than replacing them with a new architecture for a lint.
- `src/client/shell/state.rs` and `src/config/io.rs`: existing long setup and
  configuration routines picked up small upload-related changes. Splitting
  them for the audit would touch unrelated behavior.
- `src/client/shell/file_upload.rs`: the new shell submodule follows the
  surrounding `use super::*` module pattern; replacing only this import would
  be inconsistent with its sibling submodules.
- `tests/client_mode.rs`: the long integration test follows a single
  upload-through-client flow. The `_master` field is a pre-existing test-helper
  name (the master must remain alive); its use, and the poison-lock recovery
  closure, follow neighboring tests.

`unwrap_used`, `expect_used`, and `panic` were also measured against the pre-fork
release but **not** enabled: the current all-targets run reports thousands of
legitimate test assertions and fixture setup operations, including 90 new
`unwrap_used` occurrences in fork Rust code. Production code still follows the
project's no-`unwrap()` convention. These lints would need separate production
and test policies to become useful blockers.

This gate runs on Linux in `just lint` and CI. macOS and Windows keep their
normal warnings-as-errors Clippy checks: platform-specific pedantic diagnostics
are not covered by the Linux release snapshot. Capturing and reviewing separate
platform baselines is needed before gating those targets. Diagnostic identity
depends on the pinned Rust toolchain; review a toolchain upgrade before updating
either snapshot.
