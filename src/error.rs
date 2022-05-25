use std::io::Error as IoError;

use tinyroute::errors::Error as TinyErr;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Io error")]
    Io(#[from] IoError),

    #[error("Missing environment variable: TINYLOG_SOCKET")]
    MissingEnvVar,

    #[error("TinyRoute error")]
    TinyRoute(#[from] TinyErr),

    #[error("Serialization error")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
