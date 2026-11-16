//! Backend-neutral DTO for a local identity row.

use airc_core::{identity::Identity, ClientId, PeerId};

pub use crate::entities::local_identity::DEFAULT_AGENT_NAME;

/// Public DTO mirroring a `local_identity` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLocalIdentity {
    pub peer_id: PeerId,
    pub client_id: ClientId,
    pub version: u32,
    pub created_at_ms: u64,
    pub identity: Identity,
    /// Discriminator for which agent this row describes (card
    /// 8384cc18 Sub-A). Existing legacy rows carry
    /// [`DEFAULT_AGENT_NAME`]; Sub-D's CLI surface creates distinct
    /// names.
    pub agent_name: String,
}

/// The same profile-selection contract for both EventStore implementations.
#[cfg(test)]
pub(crate) async fn assert_profile_update_selects_agent(store: &dyn crate::EventStore) {
    // Named agents can precede the default row; row id is not an agent selector.
    let mut expected = Vec::new();
    for (index, name) in ["alpha", DEFAULT_AGENT_NAME, "beta"]
        .into_iter()
        .enumerate()
    {
        let row = StoredLocalIdentity {
            peer_id: PeerId::from_u128(0xa0 + index as u128),
            client_id: ClientId::from_u128(0xc0 + index as u128),
            version: 1,
            created_at_ms: 42 + index as u64,
            identity: Identity::new(name),
            agent_name: name.into(),
        };
        store.insert_local_identity(row.clone()).await.unwrap();
        expected.push(row);
    }

    assert!(matches!(
        store
            .save_local_identity_card("missing", Identity::new("wrong"))
            .await,
        Err(crate::StoreError::NotFound("local_identity"))
    ));
    assert!(store
        .load_local_identity_by_agent_name("missing")
        .await
        .unwrap()
        .is_none());

    for index in 0..expected.len() {
        let mut updated = Identity::new(format!("{} display", expected[index].agent_name));
        updated.pronouns = "they".into();
        updated.role = "substrate".into();
        updated.status = "working".into();
        updated
            .integrations
            .insert("continuum".into(), "citizen".into());
        store
            .save_local_identity_card(&expected[index].agent_name, updated.clone())
            .await
            .unwrap();
        expected[index].identity = updated;
        for row in &expected {
            assert_eq!(
                store
                    .load_local_identity_by_agent_name(&row.agent_name)
                    .await
                    .unwrap()
                    .as_ref(),
                Some(row)
            );
        }
    }
    assert_eq!(
        store.load_local_identity().await.unwrap().as_ref(),
        Some(&expected[1])
    );
}
