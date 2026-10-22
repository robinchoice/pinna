//! The MCP server through a real client over stdio.

mod common;

use common::{EXAMPLE, call, mcp, read, song};
use serde_json::json;

#[tokio::test]
async fn six_tools_digest_edit_undo() {
    let (_tmp, song) = song();
    let client = mcp(&song).await;

    let tools = client.list_all_tools().await.unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort();
    assert_eq!(
        names,
        ["analyze", "compare", "digest", "edit", "propose", "undo"]
    );
    for t in &tools {
        let desc = t.description.as_deref().unwrap_or_default();
        assert!(desc.contains("Example: {"), "{} lacks an example", t.name);
    }
    // The Op schema reaches the agent: op names, device kinds and param defaults.
    let edit = tools.iter().find(|t| t.name == "edit").unwrap();
    let schema = serde_json::to_string(&edit.input_schema).unwrap();
    for needle in [
        "add_fx",
        "set_sidechain",
        "HighPass",
        "Compressor",
        "bar.beat",
    ] {
        assert!(schema.contains(needle), "schema lacks {needle}: {schema}");
    }
    let desc = edit.description.as_deref().unwrap();
    assert!(desc.contains("Compressor threshold_db=-20dB"), "{desc}");

    let (text, err) = call(
        &client,
        "digest",
        json!({"range": "Refrain", "tracks": ["bass"]}),
    )
    .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("range: 9.1-17.1") && text.contains("track bass"),
        "{text}"
    );
    let (text, err) = call(&client, "digest", json!({"range": "Bridge"})).await;
    assert!(err && text.contains("unknown range \"Bridge\""), "{text}");

    let ops = json!({"ops": [
        {"op": "add_fx", "track": "bass", "device": {"id": "duck", "kind": "Compressor", "sidechain": "kick", "params": {"ratio": 8}}},
        {"op": "set", "path": "bass.gain_db", "value": 3}
    ]});
    let (text, err) = call(&client, "edit", ops).await;
    assert!(!err, "{text}");
    assert!(text.starts_with("ok, 2 ops\n"), "{text}");
    assert!(
        text.contains("track bass saw SawBass(cutoff=500 sub=0.6) gain 3 dB"),
        "{text}"
    );
    assert!(
        text.contains("fx: duck Compressor(ratio=8) ←kick"),
        "{text}"
    );
    assert!(!text.contains("track kick"), "{text}");
    let edited = read(&song);

    let bad = json!({"ops": [{"op": "set", "path": "bass.gain_db", "value": 0}, {"op": "remove_fx", "track": "bass", "id": "nope"}]});
    let (text, err) = call(&client, "edit", bad).await;
    assert!(
        err && text.contains("nothing written: op 1: \"bass\" has no fx \"nope\""),
        "{text}"
    );
    let (text, err) = call(&client, "edit", json!({"ops": [{"op": "explode"}]})).await;
    assert!(
        err && text.contains("op 0: unknown variant `explode`"),
        "{text}"
    );
    assert_eq!(read(&song), edited);

    let (text, err) = call(&client, "undo", json!({})).await;
    assert!(!err && text.contains("track bass"), "{text}");
    assert_eq!(read(&song), EXAMPLE);
    let (text, err) = call(&client, "undo", json!({})).await;
    assert!(err && text.contains("nothing to undo"), "{text}");

    let (text, err) = call(&client, "analyze", json!({"rev": "-x"})).await;
    assert!(err && text.contains("start with '-'"), "{text}");
    let (text, err) = call(&client, "analyze", json!({"reference": "../x.wav"})).await;
    assert!(
        err && text.contains("must be relative to the project"),
        "{text}"
    );
    let (text, err) = call(&client, "propose", json!({"title": "T", "why": "W"})).await;
    assert!(err && text.contains("nothing to propose"), "{text}");

    client.cancel().await.unwrap();
}
