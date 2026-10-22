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

Status: stage 1, engine MVP without GUI. A minimal UI follows (arrangement view plus
a proposal column with loudness-matched A/B).

## Use

```sh
cargo install --path crates/pinna
pinna init --example nachtzug song      # project folder with its own git repository
claude mcp add pinna -- pinna mcp --project "$PWD/song"
```

MCP tools: `digest`, `edit`, `undo`, `analyze`, `compare`, `propose`. Accepting stays
with the human: `pinna proposals`, then `pinna accept <slug>` or `pinna discard <slug>`.

## Layout

```
crates/pinna-core      project model, RON format, validation, edit ops, digest
crates/pinna-engine    built-in instruments and effects, deterministic offline render
crates/pinna-analysis  measurements on plain sample slices, usable without Pinna
crates/pinna           CLI and MCP server, Git proposals
examples/nachtzug      example song
```

## License

MIT OR Apache-2.0, at your option.
