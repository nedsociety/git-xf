use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("git command failed in {repo}: {message}\nstderr: {stderr}")]
    Git {
        repo: String,
        message: String,
        stderr: String,
    },
    #[error("config error: {0}")]
    Config(String),
    #[error("transform '{name}' failed on {sha}: {stderr}")]
    Transform {
        name: String,
        sha: String,
        stderr: String,
    },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
