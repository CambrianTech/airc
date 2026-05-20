use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::error::GhGistError;

#[async_trait]
pub trait GistClient: Send + Sync {
    async fn get_file(&self, gist_id: &str, filename: &str) -> Result<Option<String>, GhGistError>;

    async fn put_file(
        &self,
        gist_id: &str,
        filename: &str,
        content: &str,
    ) -> Result<(), GhGistError>;
}

#[derive(Debug, Clone)]
pub struct GhCliClient {
    gh_bin: PathBuf,
}

impl GhCliClient {
    pub fn new() -> Self {
        Self {
            gh_bin: PathBuf::from("gh"),
        }
    }

    pub fn with_bin(gh_bin: impl Into<PathBuf>) -> Self {
        Self {
            gh_bin: gh_bin.into(),
        }
    }
}

impl Default for GhCliClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GistClient for GhCliClient {
    async fn get_file(&self, gist_id: &str, filename: &str) -> Result<Option<String>, GhGistError> {
        let output = Command::new(&self.gh_bin)
            .args(["api", &format!("gists/{gist_id}")])
            .output()
            .await
            .map_err(|error| GhGistError::Client(error.to_string()))?;
        if !output.status.success() {
            return Err(GhGistError::Client(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        let gist: Value = serde_json::from_slice(&output.stdout)?;
        Ok(gist
            .get("files")
            .and_then(|files| files.get(filename))
            .and_then(|file| file.get("content"))
            .and_then(Value::as_str)
            .map(ToString::to_string))
    }

    async fn put_file(
        &self,
        gist_id: &str,
        filename: &str,
        content: &str,
    ) -> Result<(), GhGistError> {
        let body = serde_json::json!({
            "files": {
                filename: {
                    "content": content,
                }
            }
        });
        let mut child = Command::new(&self.gh_bin)
            .args([
                "api",
                "--method",
                "PATCH",
                &format!("gists/{gist_id}"),
                "--input",
                "-",
            ])
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| GhGistError::Client(error.to_string()))?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| GhGistError::Client("failed to open gh stdin".to_string()))?;
        stdin
            .write_all(body.to_string().as_bytes())
            .await
            .map_err(|error| GhGistError::Client(error.to_string()))?;
        drop(stdin);

        let output = child
            .wait_with_output()
            .await
            .map_err(|error| GhGistError::Client(error.to_string()))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(GhGistError::Client(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ))
        }
    }
}
