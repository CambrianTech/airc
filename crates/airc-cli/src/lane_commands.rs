//! `airc-rs lane ...` handlers.

use std::path::Path;

use uuid::Uuid;

use airc_lib::{
    Airc, ChangeWorkLaneState, CreateWorkLane, LaneId, LaneState, RepoId, WorkBoardProjection,
};

use crate::lane_cli::CliLaneState;

pub async fn run_create(
    home: &Path,
    repo: String,
    title: String,
    state: CliLaneState,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let lane_id = airc
        .create_work_lane(CreateWorkLane {
            repo: RepoId::new(repo)?,
            title,
            state: state.into(),
        })
        .await?;
    println!("lane_id: {lane_id}");
    Ok(())
}

pub async fn run_state(
    home: &Path,
    lane_id: String,
    state: CliLaneState,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    airc.change_work_lane_state(ChangeWorkLaneState {
        lane_id: parse_lane_id(&lane_id)?,
        state: state.into(),
    })
    .await?;
    println!("lane_state: lane_id={lane_id} state={state:?}");
    Ok(())
}

pub async fn run_status(home: &Path, limit: usize) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let board = airc.work_board(limit).await?;
    print_status(&board);
    Ok(())
}

fn print_status(board: &WorkBoardProjection) {
    let snapshot = board.snapshot();
    if snapshot.lanes.is_empty() {
        println!("(no work lanes)");
        return;
    }

    println!("work lanes: {}", snapshot.lanes.len());
    for lane in &snapshot.lanes {
        println!(
            "{lane_id}  {state:?}  cards={cards}  repo={repo}  title={title}",
            lane_id = lane.lane_id,
            state = lane.state,
            cards = lane.card_ids.len(),
            repo = lane.repo,
            title = lane.title,
        );
    }
}

fn parse_lane_id(input: &str) -> Result<LaneId, Box<dyn std::error::Error>> {
    let uuid = Uuid::parse_str(input)
        .map_err(|error| format!("lane id {input:?} is not a valid UUID: {error}"))?;
    Ok(LaneId::from_uuid(uuid))
}

impl From<CliLaneState> for LaneState {
    fn from(value: CliLaneState) -> Self {
        match value {
            CliLaneState::Planned => Self::Planned,
            CliLaneState::Active => Self::Active,
            CliLaneState::Blocked => Self::Blocked,
            CliLaneState::Landing => Self::Landing,
            CliLaneState::Done => Self::Done,
        }
    }
}
