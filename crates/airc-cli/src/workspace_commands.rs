//! `airc workspace ...` handlers.

use std::path::Path;

use uuid::Uuid;

use airc_lib::{
    AllocateWorkspace, BranchName, ClaimId, HeartbeatWorkspace, ReleaseWorkspace, RepoId,
    RequestWorkspace, WorkBoardProjection, WorkCardId, WorkspaceId,
};

pub async fn run_request(
    home: &Path,
    card_id: String,
    claim_id: String,
    repo: String,
    branch: String,
    base: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let workspace_id = airc
        .request_workspace(RequestWorkspace {
            card_id: parse_work_card_id(&card_id)?,
            claim_id: parse_claim_id(&claim_id)?,
            repo: RepoId::new(repo)?,
            branch: BranchName::new(branch)?,
            base: BranchName::new(base)?,
        })
        .await?;
    println!("workspace_id: {workspace_id}");
    Ok(())
}

pub async fn run_allocate(
    home: &Path,
    workspace_id: String,
    path: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    airc.allocate_workspace(AllocateWorkspace {
        workspace_id: parse_workspace_id(&workspace_id)?,
        path,
    })
    .await?;
    println!("workspace_allocated: workspace_id={workspace_id}");
    Ok(())
}

pub async fn run_heartbeat(
    home: &Path,
    workspace_id: String,
    disk_bytes: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    airc.heartbeat_workspace(HeartbeatWorkspace {
        workspace_id: parse_workspace_id(&workspace_id)?,
        disk_bytes,
    })
    .await?;
    println!("workspace_heartbeat: workspace_id={workspace_id}");
    Ok(())
}

pub async fn run_release(
    home: &Path,
    workspace_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    airc.release_workspace(ReleaseWorkspace {
        workspace_id: parse_workspace_id(&workspace_id)?,
    })
    .await?;
    println!("workspace_released: workspace_id={workspace_id}");
    Ok(())
}

pub async fn run_list(home: &Path, limit: usize) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let board = airc.work_board(limit).await?;
    print_workspaces(&board);
    Ok(())
}

fn print_workspaces(board: &WorkBoardProjection) {
    let snapshot = board.snapshot();
    if snapshot.workspaces.is_empty() {
        println!("(no workspaces)");
        return;
    }

    println!("workspaces: {}", snapshot.workspaces.len());
    for record in &snapshot.workspaces {
        let lease = &record.lease;
        let disk = lease
            .disk_bytes
            .map(|bytes| bytes.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{workspace_id}  {status:?}  card={card_id}  claim={claim_id}  repo={repo}  branch={branch}  base={base}  path={path}  disk_bytes={disk}",
            workspace_id = lease.workspace_id,
            status = lease.status,
            card_id = lease.card_id,
            claim_id = lease.claim_id,
            repo = lease.repo,
            branch = lease.branch,
            base = lease.base,
            path = lease.path,
        );
    }
}

fn parse_work_card_id(input: &str) -> Result<WorkCardId, Box<dyn std::error::Error>> {
    let uuid = Uuid::parse_str(input)
        .map_err(|error| format!("work card id {input:?} is not a valid UUID: {error}"))?;
    Ok(WorkCardId::from_uuid(uuid))
}

fn parse_claim_id(input: &str) -> Result<ClaimId, Box<dyn std::error::Error>> {
    let uuid = Uuid::parse_str(input)
        .map_err(|error| format!("claim id {input:?} is not a valid UUID: {error}"))?;
    Ok(ClaimId::from_uuid(uuid))
}

fn parse_workspace_id(input: &str) -> Result<WorkspaceId, Box<dyn std::error::Error>> {
    let uuid = Uuid::parse_str(input)
        .map_err(|error| format!("workspace id {input:?} is not a valid UUID: {error}"))?;
    Ok(WorkspaceId::from_uuid(uuid))
}
