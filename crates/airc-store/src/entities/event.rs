//! `events` table — the canonical transcript event row.
//!
//! One row per persisted `TranscriptEvent`. The columns mirror
//! `TranscriptEvent`'s fields directly, except for the three
//! polymorphic optional payloads (`headers`, `body`, `attachment`,
//! `receipt`, `metadata`) which serialise to JSON blobs. SQLite
//! stores them as TEXT; Postgres can promote to JSONB later without
//! a schema change here.
//!
//! Primary key: `event_id` (a UUIDv4). Sort order for paging is
//! `(lamport ASC, event_id ASC)` — see the composite index in the
//! migration.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "events")]
pub struct Model {
    /// `event_id` as UUIDv4 — globally unique, primary key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub event_id: Uuid,
    /// Room (channel) this event landed in.
    pub room_id: Uuid,
    pub peer_id: Uuid,
    pub client_id: Uuid,
    /// `TranscriptKind` serialised as snake_case text. Stored as
    /// text rather than an int so a DB browse is self-documenting
    /// and so adding a new kind doesn't require an enum mapping
    /// table migration.
    pub kind: String,
    pub occurred_at_ms: i64,
    /// Lamport (sender's monotonic counter). Primary ordering key.
    pub lamport: i64,
    /// `MentionTarget` JSON.
    pub target: Json,
    /// `Headers` JSON object.
    pub headers: Json,
    /// `Option<Body>` JSON (null when no body).
    pub body: Option<Json>,
    /// `Option<AttachmentManifest>` JSON.
    pub attachment: Option<Json>,
    /// `Option<Receipt>` JSON.
    pub receipt: Option<Json>,
    /// Free-form consumer metadata.
    pub metadata: Json,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
