//! The CLI end to end, without rendering.

mod common;

use std::fs;
use std::path::Path;

use common::{EXAMPLE, fail, git, ok, pinna_with_stdin, read, song, texts};

const LOUDER: &str = r#"[{"op": "set", "path": "bass.gain_db", "value": 3}]"#;

fn edit(root: &Path, ops: &str) -> String {
    let out = pinna_with_stdin(root, &["edit", "-"], ops);
    let (stdout, stderr) = texts(&out);
    assert!(out.status.success(), "edit failed: {stderr}");
    stdout
}

#[test]
fn init_and_digest() {
    let (_tmp, song) = song();
    assert_eq!(read(&song), EXAMPLE);
    assert_eq!(
        fs::read_to_string(song.join(".gitignore")).unwrap(),
        ".pinna/\n"
    );
    assert_eq!(git(&song, &["log", "--format=%s"]), "Start Nachtzug\n");
    assert_eq!(git(&song, &["status", "--porcelain"]), "");

    let d = ok(
        &song,
        &["digest", "--range", "Refrain", "--tracks", "kick,bass"],
    );
    assert!(d.starts_with("Nachtzug · 124 BPM · 4/4 · 32 bars\n"), "{d}");
    assert!(d.contains("range: 9.1-17.1\n"), "{d}");
    assert!(
        d.contains("track bass saw SawBass(cutoff=500 sub=0.6)"),
        "{d}"
    );
    assert!(
        d.contains("clip refrain") && !d.contains("clip strophe"),
        "{d}"
    );
    assert!(!d.contains("track pad"), "{d}");

    let err = fail(&song, &["digest", "--tracks", "drums"]);
    assert!(
        err.contains("no track \"drums\"; tracks: kick, hats"),
        "{err}"
    );
    let err = fail(&song, &["init", song.to_str().unwrap()]);
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn invalid_file_is_reported_verbatim() {
    let (_tmp, song) = song();
    fs::write(
        song.join("pinna.ron"),
        read(&song).replace("tempo: 124.0", "tempo: 1000.0"),
    )
    .unwrap();
    let err = fail(&song, &["digest"]);
    assert!(
        err.contains("pinna.ron: invalid project:\n- tempo 1000 outside 20-400 BPM"),
        "{err}"
    );
}

#[test]
fn failing_edit_writes_nothing_and_names_the_op() {
    let (_tmp, song) = song();
    let ops = r#"[{"op": "set", "path": "bass.gain_db", "value": 3},
                  {"op": "set", "path": "bass.instrument.nope", "value": 1}]"#;
    let out = pinna_with_stdin(&song, &["edit", "-"], ops);
    let (_, err) = texts(&out);
    assert!(!out.status.success());
    assert!(err.contains("nothing written: op 1:"), "{err}");
    assert!(err.contains("params: cutoff"), "{err}");
    assert!(
        err.contains(r#"op 1 was: {"op":"set","path":"bass.instrument.nope","value":1.0}"#),
        "{err}"
    );

    let out = pinna_with_stdin(
        &song,
        &["edit", "-"],
        r#"[{"op": "set", "path": "bass.gain_db", "value": 3}, {"op": "explode"}]"#,
    );
    let (_, err) = texts(&out);
    assert!(
        err.contains("nothing written: op 1: unknown variant `explode`"),
        "{err}"
    );

    let out = pinna_with_stdin(
        &song,
        &["edit", "-"],
        r#"[{"op": "set", "path": "bass.gain_db", "value": 99}]"#,
    );
    let (_, err) = texts(&out);
    assert!(err.contains("nothing written: invalid project"), "{err}");

    assert_eq!(read(&song), EXAMPLE);
    assert!(!song.join(".pinna/undo").exists());
}

#[test]
fn edit_echo_comes_from_the_reloaded_file() {
    let (_tmp, song) = song();
    let ops = song.join("ops.json");
    fs::write(
        &ops,
        r#"[{"op": "add_fx", "track": "bass", "device": {"id": "hp", "kind": "HighPass", "params": {"freq": 40}}},
            {"op": "set", "path": "master.gain_db", "value": -3}]"#,
    )
    .unwrap();
    let out = ok(&song, &["edit", ops.to_str().unwrap()]);
    assert!(out.contains("fx: hp HighPass(freq=40)"), "{out}");
    assert!(out.contains("master gain -3 dB"), "{out}");
    let now = ok(&song, &["digest", "--tracks", "bass,master"]);
    assert_eq!(out, format!("ok, 2 ops\n{now}"));
    assert!(read(&song).contains("\"freq\": 40.0"));
}

#[test]
fn undo_restores_exactly() {
    let (_tmp, song) = song();
    // Hand-edited, valid but not canonical: undo must bring back these bytes.
    let original = EXAMPLE.replace("  tempo: 124.0,", "  tempo: 124.0, // hand edit");
    fs::write(song.join("pinna.ron"), &original).unwrap();
    edit(&song, LOUDER);
    edit(
        &song,
        r#"[{"op": "set", "path": "kick.gain_db", "value": -2}]"#,
    );

    let out = ok(&song, &["undo"]);
    assert!(out.starts_with("undone, 1 more undo steps\n"), "{out}");
    assert!(
        out.contains("track kick") && !out.contains("track bass"),
        "{out}"
    );
    assert!(read(&song).contains("gain_db: 3.0"));

    let out = ok(&song, &["undo"]);
    assert!(
        out.contains("track bass") && !out.contains("track kick"),
        "{out}"
    );
    assert_eq!(read(&song), original);

    let err = fail(&song, &["undo"]);
    assert!(err.contains("nothing to undo"), "{err}");
}

#[test]
fn undo_keeps_hand_edits_made_after_the_last_edit() {
    let (_tmp, song) = song();
    edit(&song, LOUDER);
    let hand = read(&song).replace("tempo: 124.0", "tempo: 126.0");
    fs::write(song.join("pinna.ron"), &hand).unwrap();
    let err = fail(&song, &["undo"]);
    assert!(err.contains("changed since the last pinna edit"), "{err}");
    assert_eq!(read(&song), hand);
    let err = fail(&song, &["undo"]);
    assert!(err.contains("nothing to undo"), "{err}");
}

#[cfg(unix)]
#[test]
fn writes_refuse_a_symlinked_pinna_dir() {
    let (_tmp, song) = song();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), song.join(".pinna")).unwrap();
    let out = pinna_with_stdin(&song, &["edit", "-"], LOUDER);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains(".pinna is a symlink"),
        "{err}"
    );
    assert_eq!(read(&song), EXAMPLE);
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn undo_journal_keeps_fifty() {
    let (_tmp, song) = song();
    for i in 0..53 {
        edit(
            &song,
            &format!(
                r#"[{{"op": "set", "path": "bass.gain_db", "value": {}}}]"#,
                -f64::from(i)
            ),
        );
    }
    let entries = fs::read_dir(song.join(".pinna/undo"))
        .unwrap()
        .filter(|e| {
            !e.as_ref()
                .unwrap()
                .path()
                .to_string_lossy()
                .ends_with(".after.ron")
        })
        .count();
    assert_eq!(entries, 50);
    assert_eq!(git(&song, &["status", "--porcelain"]), " M pinna.ron\n");
}

#[test]
fn propose_needs_changes_to_the_project_only() {
    let (_tmp, song) = song();
    let err = fail(&song, &["propose", "--title", "T", "--why", "W"]);
    assert!(err.contains("nothing to propose"), "{err}");

    edit(&song, LOUDER);
    fs::write(song.join("notes.txt"), "x").unwrap();
    let err = fail(&song, &["propose", "--title", "T", "--why", "W"]);
    assert!(
        err.contains("commit or remove these changes first: notes.txt"),
        "{err}"
    );
    assert!(git(&song, &["branch", "--list", "proposal/*"]).is_empty());
}

/// A proposal branch made by hand, as propose would leave it. Returns its hash.
fn proposal(song: &Path, name: &str) -> String {
    git(song, &["switch", "-q", "-c", &format!("proposal/{name}")]);
    edit(song, LOUDER);
    git(
        song,
        &[
            "commit",
            "-q",
            "-am",
            "Louder bass\n\nwhy\n\nPinna-Evidence (all 1.1-33.1, 61.9 s, A = x, B = proposal):\nkick punch dB: 4.0 -> 5.0 (+1.0)",
        ],
    );
    let hash = git(song, &["rev-parse", "HEAD"]);
    git(song, &["switch", "-q", "main"]);
    hash
}

#[test]
fn proposals_lists_evidence() {
    let (_tmp, song) = song();
    assert_eq!(ok(&song, &["proposals"]), "no open proposals\n");
    proposal(&song, "louder");
    let out = ok(&song, &["proposals"]);
    assert!(out.starts_with("proposal/louder "), "{out}");
    assert!(out.contains(" Louder bass\n  Pinna-Evidence (all"), "{out}");
    assert!(
        out.contains("\n  kick punch dB: 4.0 -> 5.0 (+1.0)\n"),
        "{out}"
    );
}

#[test]
fn accept_fast_forwards() {
    let (_tmp, song) = song();
    let hash = proposal(&song, "louder");
    let out = ok(&song, &["accept", "louder"]);
    assert!(
        out.starts_with("accepted proposal/louder (fast-forward)"),
        "{out}"
    );
    assert_eq!(git(&song, &["rev-parse", "HEAD"]), hash);
    assert!(git(&song, &["branch", "--list", "proposal/*"]).is_empty());
    assert!(read(&song).contains("gain_db: 3.0"));
}

#[test]
fn accept_merges_when_the_base_moved_on() {
    let (_tmp, song) = song();
    let hash = proposal(&song, "louder");
    fs::write(song.join("notes.txt"), "x").unwrap();
    git(&song, &["add", "notes.txt"]);
    git(&song, &["commit", "-q", "-m", "notes"]);
    let out = ok(&song, &["accept", "proposal/louder"]);
    assert!(
        out.starts_with("accepted proposal/louder (merge commit)"),
        "{out}"
    );
    assert_eq!(git(&song, &["rev-parse", "HEAD^2"]), hash);
    assert!(git(&song, &["branch", "--list", "proposal/*"]).is_empty());
}

#[test]
fn discard_deletes() {
    let (_tmp, song) = song();
    proposal(&song, "louder");
    let out = ok(&song, &["discard", "louder"]);
    assert!(out.starts_with("discarded proposal/louder (was "), "{out}");
    assert!(git(&song, &["branch", "--list", "proposal/*"]).is_empty());
    assert_eq!(read(&song), EXAMPLE);
    let err = fail(&song, &["discard", "louder"]);
    assert!(err.contains("no proposal \"louder\""), "{err}");
    let err = fail(&song, &["accept", "--", "-D"]);
    assert!(err.contains("must not be empty or start with '-'"), "{err}");
}

#[test]
fn revisions_starting_with_dash_are_rejected() {
    let (_tmp, song) = song();
    for args in [
        ["analyze", "--rev=--output=/tmp/pwned"],
        ["compare", "--a=-p"],
        ["compare", "--b=--all"],
    ] {
        let err = fail(&song, &args);
        assert!(
            err.contains("must not be empty or start with '-'"),
            "{args:?}: {err}"
        );
    }
}

#[test]
fn reference_must_stay_inside_the_project() {
    let (tmp, song) = song();
    fs::write(tmp.path().join("outside.wav"), b"x").unwrap();
    let err = fail(&song, &["analyze", "--reference", "../outside.wav"]);
    assert!(err.contains("must be relative to the project"), "{err}");
    let err = fail(
        &song,
        &[
            "analyze",
            "--reference",
            tmp.path().join("outside.wav").to_str().unwrap(),
        ],
    );
    assert!(err.contains("must be relative to the project"), "{err}");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(tmp.path().join("outside.wav"), song.join("ref.wav")).unwrap();
        let err = fail(&song, &["analyze", "--reference", "ref.wav"]);
        assert!(err.contains("leaves the project folder"), "{err}");
    }
}
