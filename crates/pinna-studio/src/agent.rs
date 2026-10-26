//! Aufträge: an agent (Claude Code by default) with Pinna's MCP tools works in its own
//! Git worktree under `.pinna/agents/`, so the song you hear stays untouched, and ends
//! with a proposal branch. Its tool calls show what it is listening to.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use pinna::git::git;
use serde_json::Value;

/// The agent program; `PINNA_AGENT` overrides it. It gets Claude Code's flags.
const AGENT: &str = "claude";
const TOOLS: [&str; 6] = ["digest", "edit", "undo", "analyze", "compare", "propose"];

#[derive(Clone, Default)]
pub struct State {
    /// What it does right now, e.g. `analyze Refrain`.
    pub activity: String,
    /// Tracks its edits touched.
    pub tracks: Vec<String>,
    /// The last range it measured.
    pub range: Option<String>,
    pub proposed: bool,
    /// Final answer or error once it has ended.
    pub done: Option<Result<String, String>>,
}

pub struct Job {
    pub task: String,
    pub state: Arc<Mutex<State>>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Job {
    pub fn snapshot(&self) -> State {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn cancel(&self) {
        if let Some(c) = self.child.lock().ok().as_mut().and_then(|c| c.as_mut()) {
            let _ = c.kill();
        }
    }
}

/// Start an agent on `task`. `base` is the revision it starts from (a proposal
/// branch when revising one), `language` the language the musician writes in.
pub fn start(root: &Path, id: u64, task: &str, base: &str, language: &str) -> Result<Job, String> {
    let top = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?.trim());
    let prefix = git(root, &["rev-parse", "--show-prefix"])?
        .trim()
        .to_string();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let name = format!("{stamp}-{id}");
    let tree = root.join(".pinna/agents").join(&name);
    let branch = format!("pinna-agent/{name}");
    let tree_str = tree.to_string_lossy().into_owned();
    git(
        &top,
        &["worktree", "add", "-q", "-b", &branch, &tree_str, base],
    )?;
    let project = tree.join(&prefix);

    let pinna = sibling("pinna");
    let mcp = serde_json::json!({
        "mcpServers": {"pinna": {
            "command": pinna,
            "args": ["--project", project.to_string_lossy(), "mcp"],
        }}
    });
    let allowed: Vec<String> = TOOLS.iter().map(|t| format!("mcp__pinna__{t}")).collect();
    let prompt = prompt(task, language);
    let program = std::env::var("PINNA_AGENT").unwrap_or_else(|_| AGENT.into());
    let spawned = Command::new(&program)
        .current_dir(&project)
        .args(["-p", &prompt, "--output-format", "stream-json", "--verbose"])
        .args(["--strict-mcp-config", "--mcp-config", &mcp.to_string()])
        .args(["--tools", ""])
        .arg("--allowedTools")
        .args(&allowed)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            cleanup(&top, &tree_str, &branch);
            return Err(format!("{program}: {e}"));
        }
    };
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let state = Arc::new(Mutex::new(State {
        activity: "…".into(),
        ..State::default()
    }));
    let child = Arc::new(Mutex::new(Some(child)));
    let (st, ch) = (state.clone(), child.clone());
    thread::spawn(move || {
        let mut answer = None;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Ok(mut s) = st.lock() {
                follow(&v, &mut s, &mut answer);
            }
        }
        let mut err = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut err);
        let status = ch
            .lock()
            .ok()
            .and_then(|mut c| c.take())
            .map(|mut c| c.wait());
        cleanup(&top, &tree_str, &branch);
        let ok = matches!(status, Some(Ok(s)) if s.success());
        if let Ok(mut s) = st.lock() {
            s.done = Some(match answer {
                Some(Ok(a)) if ok => Ok(a),
                Some(Err(e)) => Err(e),
                _ if ok => Ok(String::new()),
                _ => Err(last_lines(&err)),
            });
        }
    });
    Ok(Job {
        task: task.to_string(),
        state,
        child,
    })
}

fn prompt(task: &str, language: &str) -> String {
    format!(
        "You work on a song in Pinna, a DAW whose project is the text file pinna.ron in \
         this folder. The musician asks: \"{task}\"\n\n\
         Work only through the pinna MCP tools: read the song with digest, measure with \
         analyze (choose the range where the change acts), change it with edit, check the \
         result with compare against HEAD, and finish with exactly one propose call. The \
         proposal title is one short line; why says what you heard in the numbers and \
         what you changed. Write title and why in {language}. Never accept or discard \
         proposals: the musician listens and decides."
    )
}

/// Update the state from one stream-json event.
fn follow(v: &Value, s: &mut State, answer: &mut Option<Result<String, String>>) {
    match v["type"].as_str() {
        Some("assistant") => {
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                if block["type"] != "tool_use" {
                    continue;
                }
                let name = block["name"].as_str().unwrap_or_default();
                let tool = name.strip_prefix("mcp__pinna__").unwrap_or(name);
                let input = &block["input"];
                let range = input["range"].as_str().map(str::to_string);
                if range.is_some() && matches!(tool, "analyze" | "compare" | "propose") {
                    s.range = range;
                }
                if tool == "edit" {
                    for op in input["ops"].as_array().into_iter().flatten() {
                        let track = op["track"]
                            .as_str()
                            .or_else(|| op["path"].as_str()?.split('.').next())
                            .filter(|t| *t != "tempo");
                        if let Some(t) = track
                            && !s.tracks.iter().any(|x| x == t)
                        {
                            s.tracks.push(t.to_string());
                        }
                    }
                }
                if tool == "propose" {
                    s.proposed = true;
                }
                s.activity = match &s.range {
                    Some(r) if matches!(tool, "analyze" | "compare") => format!("{tool} {r}"),
                    _ => tool.to_string(),
                };
            }
        }
        Some("result") => {
            let text = v["result"].as_str().unwrap_or_default().to_string();
            *answer = Some(if v["is_error"].as_bool().unwrap_or(false) {
                Err(text)
            } else {
                Ok(text)
            });
        }
        _ => {}
    }
}

fn cleanup(top: &Path, tree: &str, branch: &str) {
    let _ = git(top, &["worktree", "remove", "--force", tree]);
    let _ = git(top, &["branch", "-q", "-D", branch]);
}

/// A binary next to this one (as in the release archive), else from `PATH`.
fn sibling(name: &str) -> String {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(&file)))
        .filter(|p| p.exists())
        .map_or(file, |p| p.to_string_lossy().into_owned())
}

fn last_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(3)..].join("\n");
    if tail.is_empty() {
        "the agent ended without an answer".into()
    } else {
        tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_tool_calls_and_the_result() {
        let mut s = State::default();
        let mut answer = None;
        let events = [
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__pinna__analyze","input":{"range":"Refrain"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hm"},{"type":"tool_use","name":"mcp__pinna__edit","input":{"ops":[{"op":"set","path":"bass.gain_db","value":-3},{"op":"add_fx","track":"bass"},{"op":"set","path":"tempo","value":120}]}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__pinna__propose","input":{"title":"x","why":"y"}}]}}"#,
            r#"{"type":"result","is_error":false,"result":"Done."}"#,
        ];
        for e in events {
            follow(&serde_json::from_str(e).unwrap(), &mut s, &mut answer);
        }
        assert_eq!(s.range.as_deref(), Some("Refrain"));
        assert_eq!(s.tracks, ["bass"]);
        assert!(s.proposed);
        assert_eq!(s.activity, "propose");
        assert_eq!(answer, Some(Ok("Done.".into())));
    }
}
