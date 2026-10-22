//! MCP server over stdio: six tools with plain-text results. Each call reads the
//! project fresh from disk; calls run one at a time.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pinna_core::Kind;
use pinna_core::ops::Op;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::measure::{self, RENDERS};
use crate::{project, proposal};

#[derive(Clone)]
pub struct Pinna {
    root: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DigestParams {
    /// Section name ("Refrain"), bars "9-17" (end exclusive), "9.1-17.1" or "all". Default: no range filter.
    range: Option<String>,
    /// Only these track ids; "master" selects the master bus. Default: all.
    tracks: Option<Vec<String>>,
    /// List every note instead of a summary per clip.
    #[serde(default)]
    notes: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct EditParams {
    /// Ops applied in order as one atomic batch.
    #[schemars(with = "Vec<Op>")]
    ops: Vec<serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AnalyzeParams {
    /// Section name, "9.1-17.1", "9-17" or "all" (default).
    range: Option<String>,
    /// Git revision like "HEAD" or "HEAD~1", or "worktree" (default, the file on disk).
    rev: Option<String>,
    /// Track id of the kick for masking. Default: track "kick" if it exists.
    kick: Option<String>,
    /// Track id of the bass for masking. Default: track "bass" if it exists.
    bass: Option<String>,
    /// Reference audio file, relative to the project folder, e.g. "audio/ref.wav".
    reference: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CompareParams {
    /// State A: git revision or "worktree". Default "HEAD".
    a: Option<String>,
    /// State B: git revision or "worktree". Default "worktree".
    b: Option<String>,
    /// Section name, "9.1-17.1", "9-17" or "all" (default). Resolved on A.
    range: Option<String>,
    /// Track id of the kick for masking. Default: track "kick" if it exists.
    kick: Option<String>,
    /// Track id of the bass for masking. Default: track "bass" if it exists.
    bass: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposeParams {
    /// One line, becomes the commit subject and the branch name proposal/<slug>.
    title: String,
    /// Why this change, in a few sentences, for the human who decides.
    why: String,
    /// Range the evidence is measured over. Default "all".
    range: Option<String>,
    /// Track id of the kick for masking. Default: track "kick" if it exists.
    kick: Option<String>,
    /// Track id of the bass for masking. Default: track "bass" if it exists.
    bass: Option<String>,
}

const DIGEST: &str = r#"Read the project as compact text: tempo, sections, tracks with instrument, clips, fx chain (only non-default params, "←kick" marks a sidechain) and automation. The ids shown are what edit ops use. Read it before editing. Example: {"range": "Refrain", "tracks": ["kick", "bass"]}"#;

const EDIT: &str = r#"Change pinna.ron with ops, applied in order as one atomic batch: all succeed and validate, or nothing is written and the error names the op. Returns "ok, N ops" and the touched tracks as re-read from disk. Positions are "bar.beat" strings ("9.1"), lengths beats ("4", "1/2"). Revert with undo. Example: {"ops": [{"op": "add_fx", "track": "bass", "device": {"id": "hp", "kind": "HighPass", "params": {"freq": 40}}}, {"op": "add_fx", "track": "bass", "device": {"id": "duck", "kind": "Compressor", "sidechain": "kick", "params": {"ratio": 8}}}, {"op": "set", "path": "bass.gain_db", "value": 3}]}"#;

const UNDO: &str = "Revert the last edit (journal of up to 50 steps). Returns the tracks that changed back. Example: {}";

const ANALYZE: &str = r#"Render a range offline and measure the mix: loudness (integrated LUFS, short-term max, true peak, LRA), kick/bass masking index (0 = bass never covers the kick, 0.5 = equal energy) with worst band and kick punch, stereo correlation/width/mono loss, 31 third-octave levels, and optionally the difference to a reference file. Example: {"range": "Refrain"}"#;

const COMPARE: &str = r#"Render two states over the same range, match B's integrated loudness to A, and list "metric: A -> B (delta)". Loudness rows are before matching, bands after. Writes both renders to .pinna/renders/ for a human to listen. Use it to check an edit before proposing. Example: {"range": "Refrain"} compares HEAD with the edited file."#;

const PROPOSE: &str = r#"Hand the current edits to the human: measures HEAD vs the edited file, commits pinna.ron (and audio/) on a new branch proposal/<slug> with the evidence in the message, then returns the worktree to HEAD. Needs a git repo whose only changes are pinna.ron and audio/. The human accepts or discards it. Example: {"title": "Duck the bass under the kick", "why": "The bass masks the kick in the refrain.", "range": "Refrain"}"#;

/// The edit description plus the param names and defaults of every device kind,
/// which the schema cannot express.
fn edit_description() -> String {
    let kinds: Vec<String> = Kind::ALL
        .iter()
        .map(|k| {
            let ps: Vec<String> = k
                .params()
                .iter()
                .map(|s| format!("{}={}{}", s.name, s.default, s.unit))
                .collect();
            format!("{k:?} {}", ps.join(" "))
        })
        .collect();
    format!("{EDIT}\nDevice params with defaults: {}", kinds.join("; "))
}

#[tool_router]
impl Pinna {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: Arc::new(root),
            lock: Arc::new(Mutex::new(())),
            tool_router: Self::tool_router(),
        }
    }

    /// Run a call off the async runtime, one at a time. A panic becomes a tool
    /// error instead of ending the server.
    async fn run(
        &self,
        f: impl FnOnce(&std::path::Path) -> Result<String, String> + Send + 'static,
    ) -> Result<String, String> {
        let (root, lock) = (self.root.clone(), self.lock.clone());
        tokio::task::spawn_blocking(move || {
            let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            f(&root)
        })
        .await
        .unwrap_or_else(|e| Err(format!("internal error: {e}")))
    }

    #[tool(description = DIGEST)]
    async fn digest(&self, Parameters(p): Parameters<DigestParams>) -> Result<String, String> {
        self.run(move |root| project::digest_text(root, p.range.as_deref(), p.tracks, p.notes))
            .await
    }

    #[tool(description = edit_description())]
    async fn edit(&self, Parameters(p): Parameters<EditParams>) -> Result<String, String> {
        self.run(move |root| project::edit(root, &project::parse_ops(p.ops)?))
            .await
    }

    #[tool(description = UNDO)]
    async fn undo(&self) -> Result<String, String> {
        self.run(project::undo).await
    }

    #[tool(description = ANALYZE)]
    async fn analyze(&self, Parameters(p): Parameters<AnalyzeParams>) -> Result<String, String> {
        self.run(move |root| {
            measure::analyze(
                root,
                p.range.as_deref().unwrap_or("all"),
                p.rev.as_deref().unwrap_or(project::WORKTREE),
                p.kick.as_deref(),
                p.bass.as_deref(),
                p.reference.as_deref(),
            )
        })
        .await
    }

    #[tool(description = COMPARE)]
    async fn compare(&self, Parameters(p): Parameters<CompareParams>) -> Result<String, String> {
        self.run(move |root| {
            measure::compare(
                root,
                p.a.as_deref().unwrap_or("HEAD"),
                p.b.as_deref().unwrap_or(project::WORKTREE),
                p.range.as_deref().unwrap_or("all"),
                p.kick.as_deref(),
                p.bass.as_deref(),
                &root.join(RENDERS),
            )
            .map(|c| c.text())
        })
        .await
    }

    #[tool(description = PROPOSE)]
    async fn propose(&self, Parameters(p): Parameters<ProposeParams>) -> Result<String, String> {
        self.run(move |root| {
            proposal::propose(
                root,
                &p.title,
                &p.why,
                p.range.as_deref().unwrap_or("all"),
                p.kick.as_deref(),
                p.bass.as_deref(),
            )
        })
        .await
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "pinna",
    instructions = "Pinna is a DAW whose project is the text file pinna.ron. You cannot hear audio, so measure instead: digest to read, edit to change, analyze or compare to measure, propose to hand a change to the human with its evidence. Ranges are section names (\"Refrain\"), \"9.1-17.1\" or \"all\"."
)]
impl ServerHandler for Pinna {}

pub async fn serve(root: PathBuf) -> anyhow::Result<()> {
    let service = Pinna::new(root).serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
