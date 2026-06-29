//! `airc state` handlers — a thin CLI over the airc-lib scoped-state
//! facade (`Airc::{get,set,list,delete}_scoped_state`). The facade owns
//! provenance + write-time stamping; this layer only resolves the scope
//! and renders. Routed through the daemon-attached handle (like `airc
//! identity set`) so the CLI exercises the same path a persona consumer
//! does.

use std::error::Error;
use std::path::Path;

use airc_core::ScopeRef;
use airc_lib::Airc;

use crate::commands::attached_airc;

/// `--in-room` → (this peer, the current room); otherwise this peer
/// alone. These are the two scopes a human edits by hand; the bare `Room`
/// (one peer's local room cache) is a library concern, not a CLI verb.
async fn resolve_scope(airc: &Airc, in_room: bool) -> Result<ScopeRef, Box<dyn Error>> {
    let peer = airc.peer_id();
    if in_room {
        let room = airc.current_room().await?;
        Ok(ScopeRef::UserInRoom(peer, room.channel))
    } else {
        Ok(ScopeRef::User(peer))
    }
}

pub async fn run_get(home: &Path, key: &str, in_room: bool) -> Result<(), Box<dyn Error>> {
    let airc = attached_airc(home).await?;
    let scope = resolve_scope(&airc, in_room).await?;
    if let Some(entry) = airc.get_scoped_state(scope, key).await? {
        println!("{}", entry.value_json);
    }
    Ok(())
}

pub async fn run_set(
    home: &Path,
    key: &str,
    value: &str,
    version: i64,
    in_room: bool,
) -> Result<(), Box<dyn Error>> {
    let airc = attached_airc(home).await?;
    let scope = resolve_scope(&airc, in_room).await?;
    airc.set_scoped_state(scope, key, value, version).await?;
    println!("set {key} = {value} (v{version})");
    Ok(())
}

pub async fn run_list(home: &Path, in_room: bool) -> Result<(), Box<dyn Error>> {
    let airc = attached_airc(home).await?;
    let scope = resolve_scope(&airc, in_room).await?;
    for entry in airc.list_scoped_state(scope).await? {
        println!("{}\t{}", entry.key, entry.value_json);
    }
    Ok(())
}

pub async fn run_delete(home: &Path, key: &str, in_room: bool) -> Result<(), Box<dyn Error>> {
    let airc = attached_airc(home).await?;
    let scope = resolve_scope(&airc, in_room).await?;
    airc.delete_scoped_state(scope, key).await?;
    println!("deleted {key}");
    Ok(())
}
