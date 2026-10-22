# Pinna

## Zweck & Links

- Agent-native DAW in Rust: project as text (`pinna.ron`), offline render with a measurement return channel, MCP server, proposals as Git branches with measured evidence. GitHub: `robinchoice/pinna`, public, MIT OR Apache-2.0.
- Stage 1 (now): engine MVP without GUI. Stage 2: egui UI after the approved mockup "Studio + Hör-Review" (arrangement view, ghosts where a proposal acts, proposal column with evidence, loudness-matched A/B, accept = merge).
- Patterns taken (not forked) from github.com/richer-richard/cochlea: RON score, agent digest, from/to zoom, determinism levels, path hardening.
- Related: `robinchoice/rust-dsp-anywhere` (invariant render tests).

## Checks

`scripts/check.sh`: `cargo fmt --check`, `clippy -D warnings`, `cargo test` for the whole workspace. CI runs the same script.

## Deploy

No deploy. A push to `main` runs `.github/workflows/ci.yml` and the secret scan `secrets.yml`; check with `gh run watch`.

## Fallen

- Agents cannot hear audio: verify DSP and metrics with offline renders and program-checkable invariants (no NaN, silence in gives silence out, no jumps at block boundaries, expected frequency content). Every guard needs a test that shows it can trip.
- `examples/*/pinna.ron` must stay canonical (what `pinna_core::io::to_text` writes); after hand edits run `PINNA_BLESS=1 cargo test -p pinna-core examples`.
- Git runs through the `git` CLI, not libgit2/gix: libgit2 ignores `commit.gpgsign` and hooks. Tests isolate Git from the user's config (`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`).
- In the Claude sandbox `~/.cargo` is read-only: `cargo add`/`fetch` need the sandbox disabled, builds with fetched deps work.
