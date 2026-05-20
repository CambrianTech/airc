use std::sync::Arc;

use serde::{de::DeserializeOwned, Serialize};

use super::client::{GhCliClient, GistClient};
use super::error::GhGistError;

pub const GIST_INVITE_FILE: &str = "airc-invite.json";

/// Explicit gist-backed invite beacon store.
///
/// This is intentionally not a `Transport`: it writes one structured
/// bootstrap document and never appends runtime frames.
pub struct GhGistInviteStore<C = GhCliClient> {
    gist_id: String,
    client: Arc<C>,
}

impl GhGistInviteStore<GhCliClient> {
    pub fn new(gist_id: impl Into<String>) -> Self {
        Self::with_client(gist_id, GhCliClient::new())
    }
}

impl<C> GhGistInviteStore<C>
where
    C: GistClient + 'static,
{
    pub fn with_client(gist_id: impl Into<String>, client: C) -> Self {
        Self {
            gist_id: gist_id.into(),
            client: Arc::new(client),
        }
    }

    pub async fn publish<T>(&self, invite: &T) -> Result<(), GhGistError>
    where
        T: Serialize + Sync,
    {
        let content = serde_json::to_string_pretty(invite)?;
        self.client
            .put_file(&self.gist_id, GIST_INVITE_FILE, &content)
            .await
    }

    pub async fn read<T>(&self) -> Result<Option<T>, GhGistError>
    where
        T: DeserializeOwned,
    {
        let Some(content) = self
            .client
            .get_file(&self.gist_id, GIST_INVITE_FILE)
            .await?
        else {
            return Ok(None);
        };
        if content.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&content)
            .map(Some)
            .map_err(GhGistError::from)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    use super::*;

    #[derive(Default)]
    struct MemoryGistClient {
        files: Mutex<BTreeMap<String, String>>,
    }

    #[async_trait]
    impl GistClient for MemoryGistClient {
        async fn get_file(
            &self,
            _gist_id: &str,
            filename: &str,
        ) -> Result<Option<String>, GhGistError> {
            Ok(self.files.lock().unwrap().get(filename).cloned())
        }

        async fn put_file(
            &self,
            _gist_id: &str,
            filename: &str,
            content: &str,
        ) -> Result<(), GhGistError> {
            self.files
                .lock()
                .unwrap()
                .insert(filename.to_string(), content.to_string());
            Ok(())
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestInvite {
        peer: String,
        endpoints: Vec<String>,
    }

    #[tokio::test]
    async fn publish_writes_invite_file_not_messages_jsonl() {
        let client = MemoryGistClient::default();
        let store = GhGistInviteStore::with_client("gist-a", client);

        store
            .publish(&TestInvite {
                peer: "peer-a".to_string(),
                endpoints: vec!["lan:127.0.0.1:7373".to_string()],
            })
            .await
            .unwrap();

        let invite = store
            .client
            .get_file("gist-a", GIST_INVITE_FILE)
            .await
            .unwrap()
            .unwrap();
        let messages = store
            .client
            .get_file("gist-a", "messages.jsonl")
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&invite).unwrap();

        assert_eq!(parsed["peer"], "peer-a");
        assert_eq!(messages, None);
    }

    #[tokio::test]
    async fn read_round_trips_invite_payload() {
        let store = GhGistInviteStore::with_client("gist-a", MemoryGistClient::default());
        let invite = TestInvite {
            peer: "peer-a".to_string(),
            endpoints: vec!["relay:https://relay.example".to_string()],
        };

        store.publish(&invite).await.unwrap();

        assert_eq!(store.read::<TestInvite>().await.unwrap(), Some(invite));
    }
}
