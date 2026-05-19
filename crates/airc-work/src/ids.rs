//! Domain identifiers for work coordination.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            pub fn from_u128(value: u128) -> Self {
                Self(Uuid::from_u128(value))
            }

            pub fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

uuid_id!(WorkCardId);
uuid_id!(LaneId);
uuid_id!(ClaimId);
uuid_id!(WorkspaceId);

/// Repository key such as `CambrianTech/continuum`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoId(String);

impl RepoId {
    pub fn new(value: impl Into<String>) -> Result<Self, RepoIdError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(RepoIdError::Empty);
        }
        if trimmed.contains(char::is_whitespace) {
            return Err(RepoIdError::ContainsWhitespace);
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<&str> for RepoId {
    type Error = RepoIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RepoIdError {
    #[error("repo id cannot be empty")]
    Empty,
    #[error("repo id cannot contain whitespace")]
    ContainsWhitespace,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_ids_are_uuidv4_by_default() {
        assert_eq!(
            WorkCardId::new().as_uuid().get_version(),
            Some(uuid::Version::Random)
        );
        assert_eq!(
            LaneId::new().as_uuid().get_version(),
            Some(uuid::Version::Random)
        );
        assert_eq!(
            ClaimId::new().as_uuid().get_version(),
            Some(uuid::Version::Random)
        );
        assert_eq!(
            WorkspaceId::new().as_uuid().get_version(),
            Some(uuid::Version::Random)
        );
    }

    #[test]
    fn repo_id_rejects_empty_or_whitespace() {
        assert!(matches!(RepoId::new(""), Err(RepoIdError::Empty)));
        assert!(matches!(
            RepoId::new("CambrianTech / continuum"),
            Err(RepoIdError::ContainsWhitespace)
        ));
        assert_eq!(
            RepoId::new(" CambrianTech/continuum ").unwrap().as_str(),
            "CambrianTech/continuum"
        );
    }
}
