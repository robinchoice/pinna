//! Pinna's project model: the text format, validation, edit ops and the agent digest.

pub mod digest;
pub mod io;
pub mod model;
pub mod ops;
pub mod path;
pub mod range;
pub mod time;
pub mod validate;

pub use model::*;
pub use time::{Len, PPQ, Pos, Timing};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("invalid project:\n- {}", .0.join("\n- "))]
    Invalid(Vec<String>),
    #[error("op {index}: {msg}")]
    Op { index: usize, msg: String },
    #[error("{0}")]
    Io(String),
}
