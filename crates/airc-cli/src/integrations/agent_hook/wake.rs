//! An idle agent hears what is addressed to it (docs/AGENT-DELIVERY.md).
//!
//! Two parts, one owner each:
//! - **The wake** is one process per idle period. It holds this consumer's wake lock, carries the
//!   session's presence at the join's own cadence (presence has one owner: a hook call costs no
//!   message), and exits on the first event addressed to the consumer. A runtime whose background
//!   task wakes the session when it exits (Claude Code) is woken exactly when someone addresses it;
//!   nothing expires.
//! - **The stop gate** runs when the agent is about to go idle. It hands over what is addressed
//!   and undelivered, and asks once for a wake when none is armed.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use airc_lib::AddressedWake;
use fs2::FileExt;

use super::{compose, consumer_id, io, prepare, HookOptions, HookRuntime};
use crate::client_id::current_client_id;

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
    /// `client` is the session's identity as the hook knows it: the wake must read the same
    /// consumer the hooks and the stop gate deliver to, so the command binds it explicitly.
    pub(super) fn line(self, runtime: &HookRuntime, client: Option<&str>) -> Option<String> {
        let bind = client.map_or_else(String::new, |client| {
            format!(" --client {}", serde_json::json!(client))
        });
        match self {
            Self::Armed => None,
            Self::NotArmed => Some(format!(
                "AIRC wake is not armed for this session, so a message addressed to you waits until \
                 your next turn. Arm it: run `airc {} wake{bind}` as a background command. It exits \
                 on the next message addressed to you, which wakes you; arm it again after.",
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
/// `client` is the session the hook named (`--client`), else this process's own identity.
pub async fn run_wake(
    runtime: &HookRuntime,
    home: &Path,
    named: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = match named {
        Some(client) => Some(client),
        None => current_client_id()?,
    };
    let consumer = consumer_id(runtime, client.as_deref());
    // The order closes every gap (Codex on 4768de58): subscribed first, so nothing published from
    // here on is missed; armed (the lock) only once subscribed, so a stop hook that sees the wake
    // armed can trust it hears; then the backlog, so a message that arrived before the
    // subscription, after a stop hook looked, still wakes the session.
    let airc = crate::commands::attached_airc(home).await?;
    let mut addressed = airc.addressed_feed(client.as_deref()).await?;
    let lock = open_lock(home, &consumer)?;
    let end = match lock.try_lock_exclusive() {
        Err(error) if error.kind() == fs2::lock_contended_error().kind() => WakeEnd::AlreadyArmed,
        Err(error) => return Err(error.into()),
        Ok(()) => match airc
            .addressed_undelivered(&consumer, client.as_deref())
            .await?
        {
            Some(already) => WakeEnd::Woke(already),
            None => {
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
        },
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
        Wake::of(home, &consumer)?.line(runtime, client.as_deref())
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
