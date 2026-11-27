//! An idle agent hears what is addressed to it (docs/AGENT-DELIVERY.md).
//!
//! Three parts, none on a timer of its own:
//! - **Presence** is published at every hook call, so a session that only has hooks is present
//!   to its peers (an agent with no held `airc join` used to read as "no peer at all has been
//!   seen").
//! - **The wake** is one process per idle period. It holds this consumer's wake lock, keeps the
//!   session present, and exits on the first event addressed to the consumer. A runtime whose
//!   background task wakes the session when it exits (Claude Code) is then woken exactly when
//!   someone addresses it; nothing expires.
//! - **The stop gate** runs when the agent is about to go idle. It hands over what is addressed
//!   and undelivered, and asks once for a wake when none is armed.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use airc_lib::{AddressedWake, Airc, HeartbeatKind};
use fs2::FileExt;

use super::{compose, consumer_id, io, prepare, HookOptions, HookRuntime};
use crate::client_id::current_client_id;

/// Publish this session's presence: one live value per (peer, client), latest-wins, gone when it
/// lapses (airc#1341). A failure is reported and never fails the hook it runs in.
pub(super) async fn announce(airc: &Airc, runtime: &HookRuntime, client: Option<&str>) {
    let scope = std::env::current_dir()
        .ok()
        .map(|dir| dir.display().to_string());
    let build = (!crate::build_info::is_unknown()).then(|| crate::build_info::COMMIT_SHORT.into());
    if let Err(error) = airc
        .emit_agent_heartbeat_with_metadata(
            HeartbeatKind::Alive,
            runtime.presence,
            client.map(ToString::to_string),
            scope,
            build,
        )
        .await
    {
        eprintln!("airc {}: presence not published: {error}", runtime.verb);
    }
}

/// Whether a wake holds this consumer's lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Wake {
    Armed,
    NotArmed,
}

impl Wake {
    /// Read by trying the lock: one we can take is one no wake holds. Never left held.
    pub(super) fn of(home: &Path, consumer: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let file = open_lock(home, consumer)?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                file.unlock()?;
                Ok(Self::NotArmed)
            }
            Err(error) if error.kind() == fs2::lock_contended_error().kind() => Ok(Self::Armed),
            Err(error) => Err(error.into()),
        }
    }

    /// The line a turn shows when the consumer has no wake, naming the one command that arms it.
    pub(super) fn line(self, runtime: &HookRuntime) -> Option<String> {
        match self {
            Self::Armed => None,
            Self::NotArmed => Some(format!(
                "AIRC wake is not armed for this session, so a message addressed to you waits until \
                 your next turn. Arm it: run `airc {} wake` as a background command. It exits on the \
                 next message addressed to you, which wakes you; arm it again after.",
                runtime.verb
            )),
        }
    }
}

fn lock_path(home: &Path, consumer: &str) -> PathBuf {
    let name: String = consumer
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    home.join("wake").join(format!("{name}.lock"))
}

fn open_lock(home: &Path, consumer: &str) -> Result<File, Box<dyn std::error::Error>> {
    let path = lock_path(home, consumer);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?)
}

/// Why a wake ended. Every ending is a wake: the session reads its digest next.
#[derive(Debug, PartialEq, Eq)]
enum WakeEnd {
    Woke(AddressedWake),
    AlreadyArmed,
}

impl WakeEnd {
    fn line(&self) -> String {
        match self {
            Self::Woke(AddressedWake::Addressed { from, .. }) => {
                format!(
                    "airc wake: a message addressed to you arrived from {from}; read your digest"
                )
            }
            Self::Woke(AddressedWake::Lagged) => {
                "airc wake: the live feed lagged; read your digest for what was missed".into()
            }
            Self::Woke(AddressedWake::FeedClosed) => {
                "airc wake: the daemon's feed closed; run `airc join`".into()
            }
            Self::AlreadyArmed => "airc wake: already armed for this session".into(),
        }
    }
}

/// `airc <runtime>-hook wake`: wait for the first event addressed to this consumer, then exit.
pub async fn run_wake(
    runtime: &HookRuntime,
    home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = current_client_id()?;
    let consumer = consumer_id(runtime, client.as_deref());
    let lock = open_lock(home, &consumer)?;
    let end = match lock.try_lock_exclusive() {
        Err(error) if error.kind() == fs2::lock_contended_error().kind() => WakeEnd::AlreadyArmed,
        Err(error) => return Err(error.into()),
        Ok(()) => {
            let airc = crate::commands::attached_airc(home).await?;
            // Subscribed before anything else is awaited, so nothing addressed is missed.
            let mut addressed = airc.addressed_feed(client.as_deref()).await?;
            // While armed the session is present, as a joined session is (the join's own beat).
            let presence = airc
                .start_agent_heartbeat_with_metadata(
                    runtime.presence,
                    client.clone(),
                    std::env::current_dir()
                        .ok()
                        .map(|dir| dir.display().to_string()),
                    None,
                    airc_lib::agent_heartbeat::DEFAULT_HEARTBEAT_INTERVAL,
                )
                .await?;
            let woke = addressed.next().await;
            presence.stop().await;
            WakeEnd::Woke(woke)
        }
    };
    let mut output = std::io::stdout().lock();
    writeln!(output, "{}", end.line())?;
    output.flush()?;
    Ok(())
}

/// `airc <runtime>-hook stop`: the agent is about to go idle. Hand over what is addressed and
/// undelivered, so it is answered now rather than at the next prompt; with nothing addressed and no
/// wake armed, ask once for one. A second stop in the same idle (`stop_hook_active`) is never held.
pub async fn run_stop(
    runtime: &HookRuntime,
    home: &Path,
    options: &HookOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let input = io::drain_stdin(runtime)?;
    let airc = crate::commands::attached_airc(home).await?;
    let client = input.session.or(current_client_id()?);
    let consumer = consumer_id(runtime, client.as_deref());
    announce(&airc, runtime, client.as_deref()).await;
    let mut turn = prepare(&airc, &consumer, options).await?;
    let me = airc.addressee().await?;
    let addressed = turn
        .live_events()
        .iter()
        .any(|event| me.is_addressed(event));
    let reason = if addressed {
        let (context, live_end) =
            compose(runtime, &airc, &mut turn, client.as_deref(), options, None).await?;
        let held = context
            .map(|text| format!("Before you stop: messages addressed to you arrived.\n\n{text}"));
        if let Some(reason) = &held {
            io::print_stop_block(reason)?;
        }
        turn.acknowledge(live_end, held.is_some()).await?;
        return Ok(());
    } else if input.stop_hook_active {
        None
    } else {
        Wake::of(home, &consumer)?.line(runtime)
    };
    turn.acknowledge(0, false).await?;
    if let Some(reason) = reason {
        io::print_stop_block(&reason)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: a consumer reported as armed when nothing holds its wake (the agent
    // would idle deaf), or as unarmed while a wake waits (it would be told to arm a second).
    #[test]
    fn a_wake_is_armed_exactly_while_its_lock_is_held() {
        let home = tempfile::tempdir().unwrap();
        let consumer = "claude-hook:claude:session-1";
        assert_eq!(Wake::of(home.path(), consumer).unwrap(), Wake::NotArmed);
        let held = open_lock(home.path(), consumer).unwrap();
        held.try_lock_exclusive().unwrap();
        assert_eq!(Wake::of(home.path(), consumer).unwrap(), Wake::Armed);
        assert_eq!(
            Wake::of(home.path(), "claude-hook:claude:session-2").unwrap(),
            Wake::NotArmed,
            "each consumer has its own wake"
        );
        held.unlock().unwrap();
        assert_eq!(Wake::of(home.path(), consumer).unwrap(), Wake::NotArmed);
    }
}
