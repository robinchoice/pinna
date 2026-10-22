//! Helpers shared by the integration tests: the binary with Git isolated from the
//! user's config, a fresh copy of the example song, and an MCP client.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::LazyLock;

use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};

pub const BIN: &str = env!("CARGO_BIN_EXE_pinna");
pub const EXAMPLE: &str = include_str!("../../../../examples/nachtzug/pinna.ron");

/// A global git config with an identity only, so the user's signing setup and
/// hooks never touch test repos.
pub fn git_config() -> &'static Path {
    static FILE: LazyLock<tempfile::NamedTempFile> = LazyLock::new(|| {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            f.path(),
            "[user]\n\tname = Pinna Test\n\temail = test@example.org\n",
        )
        .unwrap();
        f
    });
    FILE.path()
}

fn isolate(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", git_config())
        .env("GIT_CONFIG_NOSYSTEM", "1")
}

pub fn pinna_with_stdin(root: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = isolate(Command::new(BIN).arg("--project").arg(root).args(args))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

pub fn pinna(root: &Path, args: &[&str]) -> Output {
    pinna_with_stdin(root, args, "")
}

/// Run pinna, expect success, return stdout.
pub fn ok(root: &Path, args: &[&str]) -> String {
    let out = pinna(root, args);
    let (stdout, stderr) = texts(&out);
    assert!(out.status.success(), "pinna {args:?} failed: {stderr}");
    stdout
}

/// Run pinna, expect failure, return stderr.
pub fn fail(root: &Path, args: &[&str]) -> String {
    let out = pinna(root, args);
    let (stdout, stderr) = texts(&out);
    assert!(!out.status.success(), "pinna {args:?} succeeded: {stdout}");
    stderr
}

pub fn texts(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

pub fn git(root: &Path, args: &[&str]) -> String {
    let out = isolate(Command::new("git").current_dir(root).args(args))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let (stdout, stderr) = texts(&out);
    assert!(out.status.success(), "git {args:?}: {stderr}");
    stdout
}

/// `pinna init --example nachtzug` in a temp dir: a git repo on main with one commit.
pub fn song() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let song = tmp.path().join("song");
    ok(
        tmp.path(),
        &["init", song.to_str().unwrap(), "--example", "nachtzug"],
    );
    (tmp, song)
}

pub fn read(root: &Path) -> String {
    std::fs::read_to_string(root.join("pinna.ron")).unwrap()
}

pub type Client = RunningService<RoleClient, ()>;

pub async fn mcp(root: &Path) -> Client {
    let transport = TokioChildProcess::new(tokio::process::Command::new(BIN).configure(|cmd| {
        cmd.arg("--project")
            .arg(root)
            .arg("mcp")
            .env("GIT_CONFIG_GLOBAL", git_config())
            .env("GIT_CONFIG_NOSYSTEM", "1");
    }))
    .unwrap();
    ().serve(transport).await.unwrap()
}

/// Call a tool, return its text and whether it is a tool error.
pub async fn call(client: &Client, name: &str, args: serde_json::Value) -> (String, bool) {
    let params = CallToolRequestParams::new(name.to_string())
        .with_arguments(args.as_object().unwrap().clone());
    let result = client.call_tool(params).await.unwrap();
    let text: Vec<&str> = result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .map(|t| t.text.as_str())
        .collect();
    (text.join("\n"), result.is_error == Some(true))
}
