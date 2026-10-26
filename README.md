# Pinna

An agent-native audio workstation in Rust. Agents can already drive DAWs, but they
cannot hear what they do. Pinna is built around that gap:

- **Project as text.** Tracks, clips, notes, effects and automation live in one
  canonical RON file (`pinna.ron`), diffable and versioned in Git.
- **Listening return channel.** Every change can be rendered offline and measured:
  loudness (LUFS, true peak), third-octave spectrum, kick/bass masking, stereo width
  and mono loss, comparison with a reference track, loudness-matched A/B of two states.
- **Proposals, not silent edits.** An agent's change becomes a Git branch with one
  commit that carries its measured before/after evidence. A human listens and accepts.
- **MCP server** (stdio) with few, token-lean tools, plus a CLI with the same functions.

**Pinna Studio** is the desktop app for the human side: the arrangement you know, a
column of proposals with their evidence, a ghost on the track where each one acts,
loudness-matched A/B, and accept or discard. A task typed into the studio starts an
agent (Claude Code by default, `PINNA_AGENT` overrides it) with the Pinna MCP tools in
its own Git worktree, so the song you hear stays untouched until you accept.

## Use

Release builds for Linux, macOS and Windows are on the
[releases page](https://github.com/robinchoice/pinna/releases): unpack and run
`pinna-studio`, which offers to open the example song. They are unsigned; on macOS
allow the first start under System Settings > Privacy & Security.

From source (Linux needs `libasound2-dev`):

```sh
cargo install --path crates/pinna
pinna init --example nachtzug song      # project folder with its own git repository
cargo run --release -p pinna-studio -- song
claude mcp add pinna -- pinna mcp --project "$PWD/song"   # or work with an agent directly
```

MCP tools: `digest`, `edit`, `undo`, `analyze`, `compare`, `propose`. Accepting stays
with the human: `pinna proposals`, then `pinna accept <slug>` or `pinna discard <slug>`.

## Layout

```
crates/pinna-core      project model, RON format, validation, edit ops, digest
crates/pinna-engine    built-in instruments and effects, deterministic render, offline and live
crates/pinna-analysis  measurements on plain sample slices, usable without Pinna
crates/pinna           CLI and MCP server, Git proposals
crates/pinna-studio    desktop app: arrangement, proposals, A/B, tasks for agents
examples/nachtzug      example song
```

## License

MIT OR Apache-2.0, at your option.
