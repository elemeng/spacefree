use thiserror::Error;

#[derive(Error, Debug)]
pub enum DeleterError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("No valid paths provided")]
    NoValidPaths,

    #[error("User cancelled")]
    Cancelled,

    #[error("Task join error")]
    Join,

    #[error("Invalid glob: {0}")]
    Glob(String),
}
