//! `pinna`: CLI and MCP server. Commands and tools share the same library functions.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use pinna::measure::RENDERS;
use pinna::project::WORKTREE;
use pinna::{mcp, measure, project, proposal};

#[derive(Parser)]
#[command(
    name = "pinna",
    version,
    about = "Agent-native DAW: project as text, measured renders, proposals as Git branches"
)]
struct Cli {
    /// Project folder containing pinna.ron.
    #[arg(long, global = true, default_value = ".")]
    project: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a project folder with pinna.ron, .gitignore and a git repository.
    Init {
        dir: PathBuf,
        /// Start from an example song.
        #[arg(long, value_parser = ["nachtzug"])]
        example: Option<String>,
    },
    /// Print the compact text view of the project.
    Digest {
        /// Section name, "9.1-17.1", "9-17" or "all".
        #[arg(long)]
        range: Option<String>,
        /// Only these tracks, comma separated ("master" for the master bus).
        #[arg(long, value_delimiter = ',')]
        tracks: Option<Vec<String>>,
        /// List every note.
        #[arg(long)]
        notes: bool,
    },
    /// Apply a JSON array of ops atomically (file or "-" for stdin).
    Edit { ops: String },
    /// Revert the last edit.
    Undo,
    /// Render a range of the project to a WAV file.
    Render {
        #[arg(long, default_value = "all")]
        range: String,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Render and measure loudness, masking, stereo and spectrum.
    Analyze {
        #[arg(long, default_value = "all")]
        range: String,
        /// Git revision or "worktree".
        #[arg(long, default_value = WORKTREE)]
        rev: String,
        /// Kick track id (default: "kick").
        #[arg(long)]
        kick: Option<String>,
        /// Bass track id (default: "bass").
        #[arg(long)]
        bass: Option<String>,
        /// Reference audio file inside the project folder.
        #[arg(long)]
        reference: Option<String>,
    },
    /// Compare two states loudness-matched and write both renders.
    Compare {
        #[arg(long, default_value = "HEAD")]
        a: String,
        #[arg(long, default_value = WORKTREE)]
        b: String,
        #[arg(long, default_value = "all")]
        range: String,
        #[arg(long)]
        kick: Option<String>,
        #[arg(long)]
        bass: Option<String>,
        /// Folder for the renders (default .pinna/renders in the project).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Commit the edits as a proposal branch with measured evidence.
    Propose {
        #[arg(long)]
        title: String,
        #[arg(long)]
        why: String,
        #[arg(long, default_value = "all")]
        range: String,
        #[arg(long)]
        kick: Option<String>,
        #[arg(long)]
        bass: Option<String>,
    },
    /// List open proposals with their evidence.
    Proposals,
    /// Merge a proposal into the current branch and delete it.
    Accept { name: String },
    /// Delete a proposal branch.
    Discard { name: String },
    /// Serve the MCP tools over stdio.
    Mcp,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = cli.project.as_path();
    let result = match cli.command {
        Command::Init { dir, example } => project::init(&dir, example.as_deref()),
        Command::Digest {
            range,
            tracks,
            notes,
        } => project::digest_text(root, range.as_deref(), tracks, notes),
        Command::Edit { ops } => read_ops(&ops).and_then(|ops| project::edit(root, &ops)),
        Command::Undo => project::undo(root),
        Command::Render { range, out } => measure::render_to(root, &range, &out),
        Command::Analyze {
            range,
            rev,
            kick,
            bass,
            reference,
        } => measure::analyze(
            root,
            &range,
            &rev,
            kick.as_deref(),
            bass.as_deref(),
            reference.as_deref(),
        ),
        Command::Compare {
            a,
            b,
            range,
            kick,
            bass,
            out,
        } => {
            let out = out.unwrap_or_else(|| root.join(RENDERS));
            measure::compare(root, &a, &b, &range, kick.as_deref(), bass.as_deref(), &out)
                .map(|c| c.text())
        }
        Command::Propose {
            title,
            why,
            range,
            kick,
            bass,
        } => proposal::propose(root, &title, &why, &range, kick.as_deref(), bass.as_deref()),
        Command::Proposals => proposal::list(root),
        Command::Accept { name } => proposal::accept(root, &name),
        Command::Discard { name } => proposal::discard(root, &name),
        Command::Mcp => return serve(root),
    };
    match result {
        Ok(text) => {
            print!("{text}");
            if !text.ends_with('\n') {
                println!();
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn serve(root: &Path) -> ExitCode {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    match runtime.block_on(mcp::serve(root.to_path_buf())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn read_ops(source: &str) -> Result<Vec<pinna_core::ops::Op>, String> {
    let text = if source == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| format!("stdin: {e}"))?;
        s
    } else {
        std::fs::read_to_string(source).map_err(|e| format!("{source}: {e}"))?
    };
    let values: Vec<serde_json::Value> = serde_json::from_str(&text)
        .map_err(|e| format!("nothing written: ops must be a JSON array of ops: {e}"))?;
    project::parse_ops(values)
}
