//! The target scenario with real renders, through MCP: the bass masks the kick in
//! the refrain; an agent ducks and cleans it up, checks the numbers and proposes.

mod common;

use common::{EXAMPLE, call, git, mcp, read, song};
use serde_json::json;

/// `(A, B)` from a compare row `"<metric>: A -> B (delta)"`.
fn ab(text: &str, metric: &str) -> (f64, f64) {
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{metric}: ")))
        .unwrap_or_else(|| panic!("no row {metric:?} in:\n{text}"));
    let (a, rest) = line.split_once(" -> ").unwrap();
    let b = rest.split_once(" (").map_or(rest, |(b, _)| b);
    (a.parse().unwrap(), b.parse().unwrap())
}

#[tokio::test]
async fn duck_the_bass_under_the_kick() {
    let (_tmp, song) = song();
    let client = mcp(&song).await;

    let (text, err) = call(&client, "digest", json!({})).await;
    assert!(!err && text.contains("track bass"), "{text}");

    let (before, err) = call(&client, "analyze", json!({"range": "Refrain"})).await;
    assert!(!err, "{before}");
    assert!(
        before.starts_with("range: Refrain 9.1-17.1, 15.5 s"),
        "{before}"
    );
    assert!(
        before.contains("loudness: ") && before.contains("third-octave dB: 20:"),
        "{before}"
    );
    let masking_before: f64 = before
        .split("masking kick/bass: index ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap()
        .parse()
        .unwrap();

    let ops = json!({"ops": [
        {"op": "add_fx", "track": "bass", "device": {"id": "hp", "kind": "HighPass", "params": {"freq": 40}}},
        {"op": "add_fx", "track": "bass", "device": {"id": "duck", "kind": "Compressor", "sidechain": "kick",
            "params": {"threshold_db": -36, "ratio": 10, "attack_ms": 1, "release_ms": 90}}},
        {"op": "set", "path": "bass.instrument.cutoff", "value": 900},
        {"op": "set", "path": "bass.gain_db", "value": 3}
    ]});
    let (text, err) = call(&client, "edit", ops).await;
    assert!(!err, "{text}");

    let (cmp, err) = call(&client, "compare", json!({"range": "Refrain"})).await;
    assert!(!err, "{cmp}");
    let (ma, mb) = ab(&cmp, "masking index kick/bass");
    assert!(
        (ma - masking_before).abs() < 1e-9,
        "analyze and compare disagree on A"
    );
    assert!(
        mb < ma - 0.05 && mb < ma * 0.8,
        "masking did not fall clearly:\n{cmp}"
    );
    let (la, lb) = ab(&cmp, "integrated LUFS");
    assert!(
        (lb - la).abs() < 1.0,
        "loudness moved by 1 LU or more:\n{cmp}"
    );
    let (pa, pb) = ab(&cmp, "kick punch dB");
    assert!(pb > pa, "kick punch did not rise:\n{cmp}");
    assert!(song.join(".pinna/renders/HEAD.wav").exists());
    assert!(song.join(".pinna/renders/worktree.wav").exists());

    let (text, err) = call(
        &client,
        "propose",
        json!({"title": "Duck the bass under the kick", "why": "The bass masks the kick in the refrain.", "range": "Refrain"}),
    )
    .await;
    assert!(!err, "{text}");
    let branch = "proposal/duck-the-bass-under-the-kick";
    assert!(text.starts_with(&format!("proposed {branch} @ ")), "{text}");
    let msg = git(&song, &["log", "-1", "--format=%B", branch]);
    assert!(msg.starts_with("Duck the bass under the kick\n\nThe bass masks the kick in the refrain.\n\nPinna-Evidence (Refrain 9.1-17.1, 15.5 s, A = "), "{msg}");
    let (ea, eb) = ab(&msg, "masking index kick/bass");
    assert_eq!((ea, eb), (ma, mb), "{msg}");
    assert_eq!(read(&song), EXAMPLE);
    assert_eq!(git(&song, &["branch", "--show-current"]), "main\n");

    client.cancel().await.unwrap();
}
