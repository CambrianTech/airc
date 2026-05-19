use std::fmt;

#[derive(Debug)]
pub enum GhGistError {
    Client(String),
    Json(serde_json::Error),
}

impl fmt::Display for GhGistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(error) => write!(f, "gh-gist transport client: {error}"),
            Self::Json(error) => write!(f, "gh-gist transport frame parse: {error}"),
        }
    }
}

impl std::error::Error for GhGistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Client(_) => None,
            Self::Json(error) => Some(error),
        }
    }
}

impl From<serde_json::Error> for GhGistError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
