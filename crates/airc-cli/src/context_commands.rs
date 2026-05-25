//! `airc context` — emit a budgeted JSON slice of room evidence
//! for managers, hooks, and RAG consumers.
//!
//! Thin pass-through over [`Airc::room_context`]. Output is one
//! line of JSON suitable for `jq`.

use std::path::Path;

use airc_lib::{Airc, ContextBudget};

pub async fn run_context(
    home: &Path,
    max_items: usize,
    max_age_ms: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let slice = airc
        .room_context(ContextBudget {
            max_items,
            max_age_ms,
        })
        .await?;
    let line = serde_json::to_string(&slice)
        .map_err(|error| format!("serialize context slice: {error}"))?;
    println!("{line}");
    Ok(())
}
