# Pinna

## Zweck & Links

- Agent-native DAW in Rust: project as text (`pinna.ron`), offline render with a measurement return channel, MCP server, proposals as Git branches with measured evidence. GitHub: `robinchoice/pinna`, public, MIT OR Apache-2.0.
- Stage 1: engine, analysis, CLI/MCP, proposals as Git branches. Stage 2 (now): Pinna Studio (`crates/pinna-studio`, egui/eframe with glow, cpal output), built after the approved mockup "Studio + Hör-Review" in `~/dev/mockups/pinna/` (outside the repo): arrangement view, ghosts where a proposal acts, proposal column with evidence, loudness-matched A/B, accept = merge, task field that starts an agent with the Pinna MCP.
- Patterns taken (not forked) from github.com/richer-richard/cochlea: RON score, agent digest, from/to zoom, determinism levels, path hardening.
- Related: `robinchoice/rust-dsp-anywhere` (invariant render tests).

## Checks

`scripts/check.sh`: `cargo fmt --check`, `clippy -D warnings`, `cargo test` for the whole workspace. CI runs the same script on Linux, macOS and Windows.

UI review by screenshot: `cargo build -p pinna-studio --features screenshot`, then run it with `PINNA_SCREENSHOT=target/shots/x.png` (options in `crates/pinna-studio/src/screenshot.rs`). It needs a display (`DISPLAY=:0 XAUTHORITY=/run/user/1000/gdm/Xauthority`, outside the sandbox). With `PINNA_SCREENSHOT_PLAY` it plays through the real output (PipeWire ignores an ALSA null default), so avoid it when nobody wants to hear it. While the screen is off, frames come slowly and a run can take half a minute.

## Deploy

No deploy. A push to `main` runs `.github/workflows/ci.yml` and the secret scan `secrets.yml`; check with `gh run watch`. A tag `v*` runs `release.yml`: unsigned `pinna` and `pinna-studio` for linux-x86_64, macos-arm64, macos-x86_64 and windows-x86_64 as a GitHub release. macOS builds stay unsigned for now (Robin, 26.10.2026); signing/notarizing needs a paid Apple Developer account and a new decision.

## Fallen

- Agents cannot hear audio: verify DSP and metrics with offline renders and program-checkable invariants (no NaN, silence in gives silence out, no jumps at block boundaries, expected frequency content). Every guard needs a test that shows it can trip.
- `examples/*/pinna.ron` must stay canonical (what `pinna_core::io::to_text` writes); after hand edits run `PINNA_BLESS=1 cargo test -p pinna-core examples`.
- Git runs through the `git` CLI, not libgit2/gix: libgit2 ignores `commit.gpgsign` and hooks. Tests isolate Git from the user's config (`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`).
- In the Claude sandbox `~/.cargo` is read-only: `cargo add`/`fetch` need the sandbox disabled, builds with fetched deps work. Signed commits also need it (`~/.gnupg`).
- Live playback: `live::Output::fill` runs in the audio callback and must never lock, allocate or block; everything else happens on the render thread. Tests in `crates/pinna-engine/src/live/tests.rs` include a real-time feed, so the engine builds with `opt-level = 3` in every profile.
- Agents started from the studio work in Git worktrees under `.pinna/agents/` on a branch `pinna-agent/*`; both are removed when the agent ends. A crash can leave them behind: `git worktree prune` and delete the branch.
- Linux builds need the ALSA headers (`libasound2-dev`). `.gitattributes` forces LF so the canonical `examples/*/pinna.ron` stays byte-equal on Windows.
