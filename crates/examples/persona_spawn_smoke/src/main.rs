//! Spawned-persona binary (card 98bf179b — persona-peer 2/8).
//!
//! What a parent's spawn loop launches: reads `AIRC_HOME` +
//! `AIRC_PARENT_PEER_SPEC` (+ optional `AIRC_PERSONA_ROOM`,
//! `AIRC_PERSONA_ID`, `AIRC_PARENT_LAN_ADDR`) from the environment,
//! stands up the persona peer, prints its own peer spec on stdout so
//! the parent can enrol it, then serves `TurnRequested` →
//! `TurnEmitted` until the subscription closes.
//!
//! Run by hand against a listening parent:
//!
//! ```text
//! AIRC_HOME=/tmp/persona/.airc \
//! AIRC_PARENT_PEER_SPEC=<parent peer_spec> \
//! AIRC_PARENT_LAN_ADDR=127.0.0.1:<parent lan port> \
//! persona-spawn-smoke
//! ```

use std::error::Error;
use std::time::Duration;

use persona_spawn_smoke::persona_agent::{PersonaAgent, PersonaAgentConfig};

/// One serve_next_turn wait slice. The loop continues across idle
/// slices; only a closed subscription stream ends the process, so an
/// idle persona keeps serving (Ok(None) from a timeout just re-arms).
const TURN_WAIT: Duration = Duration::from_secs(30);

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error>> {
    let config = PersonaAgentConfig::from_env()?;
    let room = config.room.clone();
    let parent_lan_addr = config.parent_lan_addr;

    let mut agent = PersonaAgent::spawn(config).await?;

    // The parent enrols us from this line — the child side of the
    // spawn handshake (parents read the spawned process's stdout).
    println!("persona-peer-spec {}", agent.peer_spec());
    println!(
        "persona-ready persona_id={} room={} parent={}",
        agent.capabilities().persona_id,
        room,
        agent.parent_peer_id(),
    );

    if let Some(addr) = parent_lan_addr {
        agent.connect_parent_lan(addr).await?;
        println!("persona-route lan {addr}");
    }

    loop {
        match agent.serve_next_turn(TURN_WAIT).await? {
            Some(emitted) => println!(
                "persona-turn-emitted activity={} turn={} chars={}",
                emitted.activity_id,
                emitted.turn_id,
                emitted.text.len(),
            ),
            // Idle slice — keep serving. A closed subscription is a
            // loud `SubscriptionClosed` error from serve_next_turn,
            // so this loop never spins on a dead stream.
            None => continue,
        }
    }
}
